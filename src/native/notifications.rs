// SPDX-License-Identifier: GPL-3.0-only
//! Transient notification/progress presentation state and platform adapters.
//!
//! No adapter discovery runs at startup. A delivery attempt is spawned only
//! after focus/policy/rate-limit checks, and desktop text is fixed
//! application-owned wording rather than untrusted terminal payload.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::core::{TerminalNotification, TerminalProgress};

pub(in crate::native) const IN_APP_NOTICE_TTL: Duration = Duration::from_secs(15);
pub(in crate::native) const PROGRESS_TTL: Duration = Duration::from_secs(10 * 60);
pub(in crate::native) const SILENCE_INTERVAL: Duration = Duration::from_secs(30);
const DEDUP_WINDOW: Duration = Duration::from_secs(5);
const RATE_WINDOW: Duration = Duration::from_secs(30);
const RATE_BURST: usize = 4;

#[derive(Debug, Clone)]
pub(in crate::native) struct InAppNotice {
    pub(in crate::native) text: String,
    pub(in crate::native) expires_at: Instant,
}

#[derive(Debug, Default)]
pub(in crate::native) struct NotificationLimiter {
    accepted: VecDeque<Instant>,
    last_fingerprint: Option<(String, Instant)>,
}

impl NotificationLimiter {
    pub(in crate::native) fn accept(&mut self, fingerprint: &str, now: Instant) -> bool {
        while self
            .accepted
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_WINDOW)
        {
            self.accepted.pop_front();
        }
        if self.last_fingerprint.as_ref().is_some_and(|(last, at)| {
            last == fingerprint && now.saturating_duration_since(*at) < DEDUP_WINDOW
        }) || self.accepted.len() >= RATE_BURST
        {
            return false;
        }
        self.accepted.push_back(now);
        self.last_fingerprint = Some((fingerprint.to_owned(), now));
        true
    }
}

#[derive(Debug, Default)]
pub(in crate::native) struct PaneAttention {
    pub(in crate::native) progress: Option<TerminalProgress>,
    progress_expires_at: Option<Instant>,
    pub(in crate::native) notice: Option<InAppNotice>,
    pub(in crate::native) unread: bool,
    pub(in crate::native) completed: bool,
    pub(in crate::native) failed: bool,
    limiter: NotificationLimiter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum PaneMonitorKind {
    Activity,
    Silence,
    Bell,
    ProcessFinish,
    CommandFailure,
}

impl PaneMonitorKind {
    pub(in crate::native) fn label(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Silence => "silence",
            Self::Bell => "bell",
            Self::ProcessFinish => "process finish",
            Self::CommandFailure => "command failure",
        }
    }

    fn notice(self) -> &'static str {
        match self {
            Self::Activity => "Pane activity detected.",
            Self::Silence => "Pane has been silent for 30 seconds.",
            Self::Bell => "Pane bell detected.",
            Self::ProcessFinish => "Pane process finished.",
            Self::CommandFailure => "Pane command failed.",
        }
    }
}

#[derive(Debug, Default)]
pub(in crate::native) struct PaneMonitors {
    activity_revision: Option<u64>,
    silence_revision: Option<u64>,
    silence_deadline: Option<Instant>,
    pub(in crate::native) bell: bool,
    pub(in crate::native) process_finish: bool,
    pub(in crate::native) command_failure: bool,
}

impl PaneMonitors {
    pub(in crate::native) fn arm(&mut self, kind: PaneMonitorKind, revision: u64, now: Instant) {
        match kind {
            PaneMonitorKind::Activity => self.activity_revision = Some(revision),
            PaneMonitorKind::Silence => {
                self.silence_revision = Some(revision);
                self.silence_deadline = Some(now + SILENCE_INTERVAL);
            }
            PaneMonitorKind::Bell => self.bell = true,
            PaneMonitorKind::ProcessFinish => self.process_finish = true,
            PaneMonitorKind::CommandFailure => self.command_failure = true,
        }
    }

    pub(in crate::native) fn clear(&mut self) -> bool {
        let changed = self.is_armed();
        *self = Self::default();
        changed
    }

    pub(in crate::native) fn is_armed(&self) -> bool {
        self.activity_revision.is_some()
            || self.silence_revision.is_some()
            || self.bell
            || self.process_finish
            || self.command_failure
    }

    pub(in crate::native) fn deadline(&self) -> Option<Instant> {
        self.silence_deadline
    }

    pub(in crate::native) fn observe(
        &mut self,
        revision: u64,
        now: Instant,
    ) -> Vec<PaneMonitorKind> {
        let mut fired = Vec::with_capacity(2);
        if self
            .activity_revision
            .is_some_and(|baseline| baseline != revision)
        {
            self.activity_revision = None;
            fired.push(PaneMonitorKind::Activity);
        }
        if let Some(previous) = self.silence_revision {
            if previous != revision {
                self.silence_revision = Some(revision);
                self.silence_deadline = Some(now + SILENCE_INTERVAL);
            } else if self
                .silence_deadline
                .is_some_and(|deadline| now >= deadline)
            {
                self.silence_revision = None;
                self.silence_deadline = None;
                fired.push(PaneMonitorKind::Silence);
            }
        }
        fired
    }
}

impl PaneAttention {
    pub(in crate::native) fn clear_all(&mut self) -> bool {
        let changed = self.has_badge() || self.notice.is_some();
        self.progress = None;
        self.progress_expires_at = None;
        self.notice = None;
        self.unread = false;
        self.completed = false;
        self.failed = false;
        changed
    }

    pub(in crate::native) fn note_notification(
        &mut self,
        notification: TerminalNotification,
        now: Instant,
    ) -> bool {
        let fingerprint = format!(
            "{}\u{1f}{}",
            notification.title.as_deref().unwrap_or_default(),
            notification.body
        );
        if !self.limiter.accept(&fingerprint, now) {
            return false;
        }
        self.notice = Some(InAppNotice {
            // Terminal-authored text is never copied into trusted application
            // chrome. The parsed payload remains only a bounded core event.
            text: "Terminal requested attention.".to_owned(),
            expires_at: now + IN_APP_NOTICE_TTL,
        });
        self.unread = true;
        true
    }

    pub(in crate::native) fn set_progress(
        &mut self,
        progress: Option<TerminalProgress>,
        now: Instant,
    ) -> bool {
        if self.progress == progress {
            return false;
        }
        self.progress = progress;
        self.progress_expires_at = progress.map(|_| now + PROGRESS_TTL);
        true
    }

    pub(in crate::native) fn note_completion(&mut self, exit: Option<i32>, now: Instant) {
        self.completed = true;
        self.failed = exit.is_some_and(|code| code != 0);
        self.unread = true;
        self.notice = Some(InAppNotice {
            text: if self.failed {
                "Command failed.".to_owned()
            } else {
                "Command finished.".to_owned()
            },
            expires_at: now + IN_APP_NOTICE_TTL,
        });
    }

    pub(in crate::native) fn note_monitor(&mut self, kind: PaneMonitorKind, now: Instant) {
        self.unread = true;
        self.failed = kind == PaneMonitorKind::CommandFailure;
        self.notice = Some(InAppNotice {
            text: kind.notice().to_owned(),
            expires_at: now + IN_APP_NOTICE_TTL,
        });
    }

    pub(in crate::native) fn clear_seen(&mut self) -> bool {
        let changed = self.unread || self.completed || self.failed;
        self.unread = false;
        self.completed = false;
        self.failed = false;
        changed
    }

    pub(in crate::native) fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| now >= notice.expires_at)
        {
            self.notice = None;
            changed = true;
        }
        if self
            .progress_expires_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.progress = None;
            self.progress_expires_at = None;
            changed = true;
        }
        changed
    }

    pub(in crate::native) fn has_badge(&self) -> bool {
        self.unread || self.completed || self.failed || self.progress.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum DesktopNotificationKind {
    TerminalRequest,
    CommandCompleted,
    CommandFailed,
    PaneMonitor,
}

impl DesktopNotificationKind {
    fn body(self) -> &'static str {
        match self {
            Self::TerminalRequest => "A background terminal requested attention.",
            Self::CommandCompleted => "The requested command finished.",
            Self::CommandFailed => "The requested command failed.",
            Self::PaneMonitor => "A pane monitor detected terminal activity.",
        }
    }
}

/// Start one on-demand platform delivery attempt. Failure is intentionally
/// silent: the in-app state remains authoritative and no success is inferred.
pub(in crate::native) fn deliver_desktop(kind: DesktopNotificationKind) {
    let body = kind.body();
    let _ = crate::spawn_util::spawn_named("notify-desktop", move || {
        let Some(spec) = platform_notification_spec(current_platform(), body) else {
            return;
        };
        // Windows: this is a console-subsystem child (`powershell.exe`) of the
        // GUI-subsystem binary. Without CREATE_NO_WINDOW the OS flashes a black
        // console over the terminal for the whole PowerShell startup, and the
        // rate limiter still permits four per 30s, so noisy PTY output could
        // drive repeated flashes. `apply_no_console_window` is the documented
        // fix (C13) and every other console spawn already routes through it;
        // this was the missing sixth site. No-op on non-Windows.
        let mut command = std::process::Command::new(spec.program);
        command.args(spec.args);
        crate::native::app::win_spawn::apply_no_console_window(&mut command);
        let _ = command.status();
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum NotificationPlatform {
    Linux,
    MacOs,
    Windows,
    Unsupported,
}

#[derive(Debug, PartialEq, Eq)]
struct PlatformNotificationSpec {
    program: &'static str,
    args: Vec<&'static str>,
}

fn current_platform() -> NotificationPlatform {
    #[cfg(target_os = "linux")]
    {
        return NotificationPlatform::Linux;
    }
    #[cfg(target_os = "macos")]
    {
        return NotificationPlatform::MacOs;
    }
    #[cfg(windows)]
    {
        return NotificationPlatform::Windows;
    }
    #[allow(unreachable_code)]
    NotificationPlatform::Unsupported
}

fn platform_notification_spec(
    platform: NotificationPlatform,
    body: &'static str,
) -> Option<PlatformNotificationSpec> {
    match platform {
        NotificationPlatform::Linux => Some(PlatformNotificationSpec {
            program: "notify-send",
            args: vec!["--app-name", "OdyTTY", "OdyTTY", body],
        }),
        NotificationPlatform::MacOs => Some(PlatformNotificationSpec {
            program: "osascript",
            args: vec![
                "-e",
                "on run argv\ndisplay notification (item 1 of argv) with title \"OdyTTY\"\nend run",
                "--",
                body,
            ],
        }),
        NotificationPlatform::Windows => Some(PlatformNotificationSpec {
            program: "powershell.exe",
            args: vec![
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                "$xml = New-Object Windows.Data.Xml.Dom.XmlDocument; $xml.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>OdyTTY</text><text>Terminal activity requires attention.</text></binding></visual></toast>'); [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('OdyTTY').Show([Windows.UI.Notifications.ToastNotification]::new($xml))",
            ],
        }),
        NotificationPlatform::Unsupported => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{NotificationSource, ProgressKind};

    #[test]
    fn pane_limit_deduplicates_and_expires_transient_state() {
        let now = Instant::now();
        let mut state = PaneAttention::default();
        let event = TerminalNotification {
            source: NotificationSource::Osc9,
            title: None,
            body: "finished".to_owned(),
        };
        assert!(state.note_notification(event.clone(), now));
        assert!(!state.note_notification(event, now + Duration::from_secs(1)));
        assert!(state.unread);
        assert_eq!(
            state.notice.as_ref().map(|notice| notice.text.as_str()),
            Some("Terminal requested attention.")
        );
        assert!(state.expire(now + IN_APP_NOTICE_TTL));
        assert!(state.notice.is_none());
    }

    #[test]
    fn progress_expires_and_seen_flags_do_not_clear_progress() {
        let now = Instant::now();
        let mut state = PaneAttention::default();
        assert!(state.set_progress(
            Some(TerminalProgress {
                kind: ProgressKind::Normal,
                value: Some(50),
            }),
            now
        ));
        state.unread = true;
        assert!(state.clear_seen());
        assert!(state.progress.is_some());
        assert!(state.expire(now + PROGRESS_TTL));
        assert!(state.progress.is_none());
    }

    #[test]
    fn activity_and_silence_monitors_are_one_shot() {
        let now = Instant::now();
        let mut monitors = PaneMonitors::default();
        monitors.arm(PaneMonitorKind::Activity, 10, now);
        monitors.arm(PaneMonitorKind::Silence, 10, now);
        assert!(monitors.observe(10, now).is_empty());
        assert_eq!(
            monitors.observe(11, now + Duration::from_secs(1)),
            vec![PaneMonitorKind::Activity]
        );
        assert_eq!(
            monitors.observe(11, now + Duration::from_secs(31)),
            vec![PaneMonitorKind::Silence]
        );
        assert!(!monitors.is_armed());
    }

    #[test]
    fn platform_adapters_are_on_demand_and_never_interpolate_terminal_text() {
        let body = DesktopNotificationKind::TerminalRequest.body();
        let linux = platform_notification_spec(NotificationPlatform::Linux, body).unwrap();
        assert_eq!(linux.program, "notify-send");
        assert_eq!(linux.args.last(), Some(&body));

        let macos = platform_notification_spec(NotificationPlatform::MacOs, body).unwrap();
        assert_eq!(macos.program, "osascript");
        assert_eq!(macos.args.last(), Some(&body));
        assert!(!macos.args[1].contains(body));

        let windows = platform_notification_spec(NotificationPlatform::Windows, body).unwrap();
        assert_eq!(windows.program, "powershell.exe");
        assert!(!windows.args.iter().any(|arg| arg.contains(body)));
        assert!(platform_notification_spec(NotificationPlatform::Unsupported, body).is_none());
    }

    #[test]
    fn limiter_enforces_burst_capacity() {
        let now = Instant::now();
        let mut limiter = NotificationLimiter::default();
        for index in 0..RATE_BURST {
            assert!(limiter.accept(&format!("event-{index}"), now));
        }
        assert!(!limiter.accept("overflow", now));
        assert!(limiter.accept("after-window", now + RATE_WINDOW));
    }
}
