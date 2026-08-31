// SPDX-License-Identifier: GPL-3.0-only
//! Opt-in external palette follower with content-hash polling and LKG retention.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::settings::fs_read;
use crate::theme::Theme;

use super::fingerprint::{ContentFingerprint, fingerprint_bytes};
use super::limits::{EXTERNAL_PALETTE_POLL_INTERVAL_MS, MAX_EXTERNAL_PALETTE_BYTES};
use super::parse::{
    ExternalPaletteError, ExternalPaletteProvider, NormalizedExternalPalette, parse_palette_bytes,
};

/// Test-observable count of palette file reads. Default launch must leave this
/// at zero; reads happen only after follow is explicitly enabled.
static PALETTE_READ_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn palette_read_count_for_test() -> usize {
    PALETTE_READ_COUNT.load(Ordering::Relaxed)
}

pub fn reset_palette_read_count_for_test() {
    PALETTE_READ_COUNT.store(0, Ordering::Relaxed);
}

/// Live status of the follower for settings/UI surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowStatus {
    Disabled,
    Watching,
    Applied,
    RetainedLastKnownGood { reason: String },
    Error { message: String },
}

impl FollowStatus {
    pub fn as_display(&self) -> String {
        match self {
            Self::Disabled => "off".to_owned(),
            Self::Watching => "watching".to_owned(),
            Self::Applied => "applied".to_owned(),
            Self::RetainedLastKnownGood { reason } => format!("retained ({reason})"),
            Self::Error { message } => format!("error: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowPollOutcome {
    Unchanged,
    Applied(Theme),
    Retained,
}

/// Opt-in watcher. Constructed empty; [`ExternalPaletteFollow::configure`]
/// arms it only when follow is enabled with an explicit path.
#[derive(Debug, Clone)]
pub struct ExternalPaletteFollow {
    enabled: bool,
    provider: ExternalPaletteProvider,
    path: Option<PathBuf>,
    interval: Duration,
    next_poll: Instant,
    last_fingerprint: Option<ContentFingerprint>,
    last_known_good: Option<NormalizedExternalPalette>,
    status: FollowStatus,
}

impl Default for ExternalPaletteFollow {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: ExternalPaletteProvider::OdyttyAnsi,
            path: None,
            interval: Duration::from_millis(EXTERNAL_PALETTE_POLL_INTERVAL_MS),
            next_poll: Instant::now(),
            last_fingerprint: None,
            last_known_good: None,
            status: FollowStatus::Disabled,
        }
    }
}

impl ExternalPaletteFollow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> &FollowStatus {
        &self.status
    }

    pub fn last_known_good_theme(&self) -> Option<Theme> {
        self.last_known_good
            .as_ref()
            .map(|palette| palette.to_theme())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.enabled.then_some(self.next_poll)
    }

    /// Arm or disarm from settings. Disabling clears the watcher but keeps
    /// last-known-good so a transient toggle off/on can reapply without a read
    /// storm only after the next explicit poll when re-enabled.
    pub fn configure(
        &mut self,
        enabled: bool,
        provider: ExternalPaletteProvider,
        path: Option<PathBuf>,
        now: Instant,
    ) {
        let path_changed = self.path != path || self.provider != provider;
        self.enabled = enabled;
        self.provider = provider;
        self.path = path;
        if !enabled {
            self.status = FollowStatus::Disabled;
            self.last_fingerprint = None;
            self.next_poll = now;
            return;
        }
        if self.path.is_none() {
            self.status = FollowStatus::Error {
                message: "external palette path is unset".to_owned(),
            };
            return;
        }
        self.status = FollowStatus::Watching;
        if path_changed {
            self.last_fingerprint = None;
            self.next_poll = now;
        }
    }

    /// Force an immediate read/apply attempt (settings enable / path change).
    pub fn refresh_now(&mut self, now: Instant) -> FollowPollOutcome {
        self.next_poll = now;
        self.poll(now)
    }

    pub fn poll(&mut self, now: Instant) -> FollowPollOutcome {
        if !self.enabled {
            return FollowPollOutcome::Unchanged;
        }
        if now < self.next_poll {
            return FollowPollOutcome::Unchanged;
        }
        self.next_poll = now + self.interval;
        let Some(path) = self.path.clone() else {
            self.status = FollowStatus::Error {
                message: "external palette path is unset".to_owned(),
            };
            return FollowPollOutcome::Retained;
        };
        match self.read_and_maybe_apply(&path) {
            Ok(Some(theme)) => {
                self.status = FollowStatus::Applied;
                FollowPollOutcome::Applied(theme)
            }
            Ok(None) => FollowPollOutcome::Unchanged,
            Err(error) => {
                if self.last_known_good.is_some() {
                    self.status = FollowStatus::RetainedLastKnownGood {
                        reason: error.to_string(),
                    };
                    FollowPollOutcome::Retained
                } else {
                    self.status = FollowStatus::Error {
                        message: error.to_string(),
                    };
                    FollowPollOutcome::Retained
                }
            }
        }
    }

    fn read_and_maybe_apply(&mut self, path: &Path) -> Result<Option<Theme>, ExternalPaletteError> {
        PALETTE_READ_COUNT.fetch_add(1, Ordering::Relaxed);
        let contents = match fs_read::read_capped_at(path, MAX_EXTERNAL_PALETTE_BYTES) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ExternalPaletteError::Incomplete(format!(
                    "palette file missing: {}",
                    path.display()
                )));
            }
            Err(error) => {
                return Err(ExternalPaletteError::Malformed(error.to_string()));
            }
        };
        let bytes = contents.as_bytes();
        let fingerprint = fingerprint_bytes(bytes);
        if self.last_fingerprint == Some(fingerprint) {
            return Ok(None);
        }
        let palette = parse_palette_bytes(self.provider, bytes)?;
        let theme = palette.to_theme();
        self.last_known_good = Some(palette);
        self.last_fingerprint = Some(fingerprint);
        Ok(Some(theme))
    }
}
