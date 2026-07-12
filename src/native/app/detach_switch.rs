// SPDX-License-Identifier: GPL-3.0-only
//! "Detach & switch": convert the focused pane into a fresh managed session
//! (Packet 2 / UX-E).
//!
//! HONEST FRAMING: this is a SPAWN, not a live-process migration. The shell
//! running in the focused pane is the window's own child (this process owns its
//! pty fds and reaps it), so it cannot be losslessly handed to a survivable
//! session-host. Instead we spawn a FRESH managed shell in the SAME working
//! directory — the same `spawn_host_on_demand` path `odytty new` uses — attach
//! to it in a new tab, and switch. This framing is a deliberate design decision;
//! the dialog copy says so.
//!
//! UNIT = the focused pane (one session). A host serves exactly one shell, so a
//! multi-pane tab cannot "become a session"; only the focused pane is affected,
//! sibling panes stay. The user picks the original pane's fate in a 3-way dialog
//! (Swap = close this pane after the managed session is live; Keep both = leave
//! it; Cancel).
//!
//! FAILURE GUARD: the original pane is NEVER closed before the new managed
//! session is confirmed live. The order is always spawn → attach → (swap only)
//! close-original. A spawn or attach failure surfaces a transient
//! [`super::open_notice::OpenNotice`] and leaves the original pane untouched.

#[cfg(unix)]
use std::path::PathBuf;

// The managed-session spawn path uses the Unix-only session-host; on Windows the
// Detach & switch action is stubbed to a transient "not supported" notice.
#[cfg(unix)]
use crate::session_host::{
    HostCommand, HostConfig, SessionMetadata, now_unix_ms, spawn_host_on_demand,
    write_session_metadata,
};

use super::App;

impl App {
    /// Open the Detach & switch choice dialog over the focused pane (Packet 2).
    /// Reads the focused pane's cwd (the App owns the terminal lock; the overlay
    /// cannot) and hands it to the dialog. An unknown cwd is carried as an empty
    /// string → the spawn falls back to the default directory.
    pub(in crate::native) fn open_detach_switch_choice(&mut self) {
        let cwd = self.focused_pane_cwd().unwrap_or_default();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_detach_switch_choice(cwd);
        self.request_selection_redraw();
    }

    /// The focused pane's current working directory (from OSC 7 tracking), or
    /// `None` when unknown. Reads the active session's terminal — the same lock
    /// the interactive-paths hover resolution uses. Shared across the crate as
    /// the single OSC 7 cwd read helper: the detach/switch dialog seeds it, and
    /// the F1 cwd-inheritance path (new tab / new window / Duplicate Tab) threads
    /// it into the spawn so a new shell starts where the active pane is. Windows:
    /// OSC 7 drive-letter cwds are already normalized upstream
    /// (`strip_leading_drive_slash`), so this returns a valid path there too.
    pub(in crate::native) fn focused_pane_cwd(&self) -> Option<String> {
        self.terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.current_working_directory().map(str::to_owned))
    }

    /// The focused pane's OSC 7 cwd, VALIDATED for seeding a spawn (audit D-1).
    /// The tracked cwd is attacker-influenceable (any process' output can emit
    /// OSC 7) and the Windows PowerShell integration can manufacture
    /// non-filesystem paths (UNC `//srv/share`, PSDrive `/HKLM:/...`); handing
    /// such a path to New Tab / Duplicate / New Window makes `CreateProcessW`
    /// receive a bogus `lpCurrentDirectory` (the spawn fails and a new window
    /// dies with stdio nulled), or silently starts a shell in the wrong dir on
    /// Unix. Mirror the restore path's discipline
    /// (`persistence::validate_interactive_cwd`): an existing dir is used as-is,
    /// a bogus one falls back to home, an unknown cwd stays `None` (default dir,
    /// unchanged). Windows: `%USERPROFILE%` is the home fallback.
    pub(in crate::native) fn validated_spawn_cwd(&self) -> Option<std::path::PathBuf> {
        let captured = self.focused_pane_cwd();
        let home = crate::native::persistence::restore_home_dir();
        crate::native::persistence::validate_interactive_cwd(captured.as_deref(), home.as_deref())
    }

    /// "Swap": spawn a managed session in `cwd`, attach + focus it, then close
    /// the original focused pane. Emitted by the dialog's `[S]` choice.
    /// Unix-only orchestration; on Windows it raises a "not supported" notice.
    pub(in crate::native) fn detach_switch_swap(&mut self, cwd: String) {
        #[cfg(unix)]
        self.detach_switch(cwd, true);
        #[cfg(not(unix))]
        {
            let _ = cwd;
            self.raise_open_notice("Detach & switch is not supported on Windows yet.".to_owned());
        }
    }

    /// "Keep both": spawn a managed session in `cwd`, attach + focus it, and
    /// leave the original pane untouched. Emitted by the dialog's `[K]` choice.
    /// Unix-only orchestration; on Windows it raises a "not supported" notice.
    pub(in crate::native) fn detach_switch_keep_both(&mut self, cwd: String) {
        #[cfg(unix)]
        self.detach_switch(cwd, false);
        #[cfg(not(unix))]
        {
            let _ = cwd;
            self.raise_open_notice("Detach & switch is not supported on Windows yet.".to_owned());
        }
    }

    /// Production entry: spawn through the real `spawn_host_on_demand` and run
    /// the swap/keep orchestration. `cwd` is empty when unknown (spawn in the
    /// default directory). Mapped to the testable [`Self::detach_switch_with_spawner`].
    #[cfg(unix)]
    fn detach_switch(&mut self, cwd: String, swap: bool) {
        let working_directory = (!cwd.is_empty()).then(|| PathBuf::from(cwd));
        self.detach_switch_with_spawner(working_directory, None, swap, |config| {
            spawn_host_on_demand(config)
                .map(|_| ())
                .map_err(|error| std::io::Error::other(error.to_string()))
        });
    }

    /// Spawn-seam orchestration (Packet 2). Captures the original focused token
    /// BEFORE spawning, spawns the managed session via `spawner`, attaches +
    /// focuses it, and — for `swap` only — closes the original focused pane via
    /// the existing close path (single-pane tab → close tab; multi-pane → close
    /// just that pane). Any spawn/attach failure surfaces a transient notice and
    /// leaves the original pane untouched. `spawner` is a seam so tests can force
    /// a failure without spawning a real host process.
    #[cfg(unix)]
    fn detach_switch_with_spawner(
        &mut self,
        working_directory: Option<PathBuf>,
        runtime_base: Option<PathBuf>,
        swap: bool,
        spawner: impl FnOnce(&HostConfig) -> std::io::Result<()>,
    ) {
        // Capture the replace target BEFORE spawning. Opening the dialog did not
        // change the active session, but be explicit (same ordering as Phase 14
        // Replace).
        let original = self.sessions.active_id();

        let session_id =
            match self.spawn_managed_session(working_directory, runtime_base.clone(), spawner) {
                Ok(id) => id,
                Err(error) => {
                    // Spawn failed → original pane untouched, nothing attached.
                    self.raise_open_notice(format!("Couldn't detach & switch — {error}"));
                    return;
                }
            };

        // Attach + focus the new managed session. NEVER close the original
        // before this succeeds: an attach failure leaves the original pane live.
        if self
            .attach_session_in_new_tab(runtime_base.as_deref(), &session_id)
            .is_err()
        {
            self.raise_open_notice(
                "Couldn't detach & switch — the new session did not attach.".to_owned(),
            );
            return;
        }

        if swap {
            // Close the ORIGINAL focused pane through the existing close path.
            // The just-attached managed tab guarantees the WorkspaceSet is non-empty,
            // so this can never be the last session (the `true` exit branch is
            // defensive and unreachable here).
            if self.sessions.close(original) {
                self.pending_exit = true;
            } else if self.sessions.active_is_single_pane() {
                // Collapsing a multi-pane original back to one pane returns the
                // tab to the plain single-pane input path; clear any pending
                // multiplexer prefix so stale state cannot swallow a key (mirrors
                // `close_focused_pane`).
                self.prefix_engine.cancel();
            }
        }
        self.on_active_session_changed();
    }

    /// Build the managed [`HostConfig`] (default shell in `working_directory`,
    /// mirroring `odytty new`), write its sidecar metadata so it shows a name in
    /// the manager, and spawn it via `spawner`. Returns the new session id on a
    /// successful spawn. Metadata write is best-effort (the row falls back to the
    /// id); only a spawn failure is fatal.
    #[cfg(unix)]
    fn spawn_managed_session(
        &mut self,
        working_directory: Option<PathBuf>,
        runtime_base: Option<PathBuf>,
        spawner: impl FnOnce(&HostConfig) -> std::io::Result<()>,
    ) -> std::io::Result<String> {
        let session_id = format!("s-{}-{}", std::process::id(), now_unix_ms());
        let mut config = HostConfig::new(session_id.clone());
        config.runtime_base = runtime_base;
        config.command = HostCommand::DefaultShell { working_directory };
        if let Ok(paths) = config.runtime_paths() {
            let metadata = SessionMetadata {
                id: session_id.clone(),
                name: session_id.clone(),
                created_unix_ms: now_unix_ms(),
                pane_count: 1,
            };
            let _ = write_session_metadata(&paths.dir, &metadata);
        }
        spawner(&config)?;
        Ok(session_id)
    }

    /// Test seam: drive the full Detach & switch orchestration with a spawner
    /// that always fails, so the failure guard (notice raised + original pane
    /// untouched, nothing attached) is exercisable without a real host process.
    #[cfg(all(test, unix))]
    pub(in crate::native) fn detach_switch_spawn_failure_for_test(&mut self, swap: bool) {
        self.detach_switch_with_spawner(
            Some(PathBuf::from("/home/user/proj")),
            None,
            swap,
            |_config| Err(std::io::Error::other("forced spawn failure (test)")),
        );
    }
}
