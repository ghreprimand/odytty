// SPDX-License-Identifier: GPL-3.0-only
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::{CONFIG_RELOAD_INTERVAL, ConfigValues, SETTING_ENV_KEYS, Settings, config_file_path};
fn env_snapshot() -> HashMap<&'static str, OsString> {
    SETTING_ENV_KEYS
        .iter()
        .filter_map(|&key| std::env::var_os(key).map(|value| (key, value)))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConfigFileFingerprint {
    pub(super) modified: SystemTime,
    pub(super) len: u64,
}

impl ConfigFileFingerprint {
    fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self {
                modified: metadata.modified()?,
                len: metadata.len(),
            })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigPollEvent {
    Unchanged,
    Changed,
    Deleted,
}

/// Bounded mtime+size polling for the resolved config path.
///
/// It is intentionally dependency-free and time-injected so the native event
/// loop can fold it into existing sleeps instead of adding a watcher thread.
#[derive(Debug, Clone)]
pub struct ConfigReloadPoller {
    pub(super) path: Option<PathBuf>,
    pub(super) interval: Duration,
    pub(super) next_poll: Instant,
    pub(super) last_seen: Option<ConfigFileFingerprint>,
}

impl ConfigReloadPoller {
    fn new(path: Option<PathBuf>, now: Instant) -> Self {
        let last_seen = path
            .as_deref()
            .and_then(|path| ConfigFileFingerprint::read(path).ok().flatten());
        Self {
            path,
            interval: CONFIG_RELOAD_INTERVAL,
            next_poll: now + CONFIG_RELOAD_INTERVAL,
            last_seen,
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.path.as_ref().map(|_| self.next_poll)
    }

    fn poll(&mut self, now: Instant) -> io::Result<ConfigPollEvent> {
        let path = self.path.clone();
        self.poll_with(now, || match path.as_deref() {
            Some(path) => ConfigFileFingerprint::read(path),
            None => Ok(None),
        })
    }

    pub(super) fn poll_with(
        &mut self,
        now: Instant,
        read_fingerprint: impl FnOnce() -> io::Result<Option<ConfigFileFingerprint>>,
    ) -> io::Result<ConfigPollEvent> {
        if self.path.is_none() || now < self.next_poll {
            return Ok(ConfigPollEvent::Unchanged);
        }
        self.next_poll = now + self.interval;

        let next_seen = read_fingerprint()?;
        let event = match (self.last_seen, next_seen) {
            (None, None) => ConfigPollEvent::Unchanged,
            (Some(_), None) => ConfigPollEvent::Deleted,
            (None, Some(_)) => ConfigPollEvent::Changed,
            (Some(previous), Some(next)) if previous == next => ConfigPollEvent::Unchanged,
            (Some(_), Some(_)) => ConfigPollEvent::Changed,
        };
        self.last_seen = next_seen;
        Ok(event)
    }
}

// `Reloaded` carries a full `Settings` by value; boxing it would ripple through
// every construction and match site for no runtime gain on this cold path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsReloadOutcome {
    Unchanged,
    Deleted,
    /// The config was re-read and produced usable settings. `warnings` carries
    /// any non-fatal parse notices (unknown/typo'd keys, out-of-range values that
    /// fell back) — surfaced but NOT a reason to discard the applied settings, so
    /// live reload matches the startup path (which applies partial configs and
    /// prints the same notices).
    Reloaded {
        settings: Settings,
        warnings: Vec<String>,
    },
    Unreadable {
        message: String,
    },
}

/// Runtime config reloader that preserves startup env precedence exactly.
#[derive(Debug, Clone)]
pub struct SettingsReloader {
    path: Option<PathBuf>,
    env_values: HashMap<&'static str, OsString>,
    poller: ConfigReloadPoller,
}

impl SettingsReloader {
    pub fn for_current_process(now: Instant) -> Self {
        let path = config_file_path();
        Self::new(path, env_snapshot(), now)
    }

    pub(super) fn new(
        path: Option<PathBuf>,
        env_values: HashMap<&'static str, OsString>,
        now: Instant,
    ) -> Self {
        Self {
            poller: ConfigReloadPoller::new(path.clone(), now),
            path,
            env_values,
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.poller.deadline()
    }

    pub fn config_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Test seam: point the reloader at a hermetic temp config file so a live
    /// persistence path (e.g. the F4-P4 rail seam drag) writes there instead of
    /// the operator's real `odytty.conf`.
    #[cfg(test)]
    pub fn set_config_path_for_test(&mut self, path: Option<PathBuf>) {
        self.path = path;
    }

    pub fn poll(&mut self, now: Instant) -> SettingsReloadOutcome {
        match self.poller.poll(now) {
            Ok(ConfigPollEvent::Unchanged) => SettingsReloadOutcome::Unchanged,
            Ok(ConfigPollEvent::Deleted) => SettingsReloadOutcome::Deleted,
            Ok(ConfigPollEvent::Changed) => self.load_changed_config(),
            Err(error) => SettingsReloadOutcome::Unreadable {
                message: format!("could not stat config file: {error}"),
            },
        }
    }

    fn load_changed_config(&self) -> SettingsReloadOutcome {
        let Some(path) = self.path.as_deref() else {
            return SettingsReloadOutcome::Unchanged;
        };

        let mut warnings = Vec::new();
        let mut suppressed = 0usize;
        // Bound the read: a huge/corrupt config file must not read gigabytes into
        // memory on the reload event thread. `read_capped` also rejects a
        // directory/FIFO and a too-large file (InvalidData); a deleted file still
        // surfaces as NotFound. Portable across Linux/macOS/Windows.
        let contents = match super::fs_read::read_capped(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return SettingsReloadOutcome::Deleted;
            }
            Err(error) => {
                return SettingsReloadOutcome::Unreadable {
                    message: format!("could not read config file {}: {error}", path.display()),
                };
            }
        };
        // Bound warning accumulation: a pathological file (every line unknown)
        // must not allocate one owned String per line and then synchronously log
        // millions of them, freezing the window.
        let settings = {
            let mut warn = super::fs_read::bounded_warn(&mut warnings, &mut suppressed);
            let config = ConfigValues::parse(&contents, |message| {
                warn(format!("{}: {message}", path.display()));
            });
            Settings::from_env_snapshot_and_config(&self.env_values, &config, &mut warn)
        };
        super::fs_read::note_suppressed(&mut warnings, suppressed);

        // Parsing is tolerant: unknown keys are skipped and out-of-range values
        // fall back, so a changed file always yields usable settings. Apply them
        // and surface any warnings non-fatally, mirroring startup — a single
        // typo'd or future key must not silently block a live edit until restart.
        SettingsReloadOutcome::Reloaded { settings, warnings }
    }
}

pub fn apply_reloadable_values(current: &mut Settings, mut reloaded: Settings) -> bool {
    reloaded.native_autoclose = current.native_autoclose;
    // Republish the synthetic-styles kill switch process-wide so the renderer's
    // atlas-build seam observes a live toggle on the next `apply_text_options`,
    // even when nothing else changed. Idempotent: re-storing the same value is
    // harmless, and the renderer only rebuilds when the value actually flips.
    super::set_synthetic_styles_enabled(reloaded.synthetic_styles);
    super::set_geometric_boxdraw_enabled(reloaded.geometric_boxdraw);
    super::set_symbol_fallback_enabled(reloaded.symbol_fallback);
    super::set_symbol_font_path(reloaded.symbol_font.clone());
    super::set_symbol_map(reloaded.symbol_map.clone());
    if *current == reloaded {
        return false;
    }
    *current = reloaded;
    true
}
