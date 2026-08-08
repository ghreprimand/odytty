// SPDX-License-Identifier: GPL-3.0-only
//! Session, pane, tab, and workspace lifecycle: bounded teardown, close, shell
//! exit, whole-application shutdown, and the structural mutations that create,
//! move, and remove nodes.
//!
//! Every blocking wait or thread join here runs off the UI path, and
//! whole-application shutdown is bounded by a single deadline. Local sessions
//! terminate and reap; an attached session detaches on Unix. No Unix kill,
//! signal, socket, or detach assumption reaches the common paths - each one sits
//! behind its own source arm.

use super::model::{Session, SessionToken, Workspace, WorkspaceSet, default_workspace_name};
use super::transport::SessionSource;
use crate::native::layout::{EVEN_RATIO, PaneNode, SplitAxis};
use crate::pty::PtySession;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// The one-line in-pane notice painted when a remote link drops. Written into
/// the terminal model on its own line (leading/trailing CRLF) using standard SGR
/// so it renders in every theme's palette, and kept short enough for a narrow
/// pane. The prompt actions (Enter / Esc) are handled by the App's key path
/// while the session is awaiting reconnect.
/// CLOSE-HANG: the bounded wall-clock budget the whole-app shutdown teardown
/// waits for every session's child to be reaped and its pump thread joined
/// before it detaches the reaper and lets the process exit anyway. Healthy
/// sessions reap in well under this (the wait returns the instant the reaper
/// signals completion); the deadline only bites when a remote is wedged, so a
/// dead `ssh` link caps teardown here instead of freezing it. The OS reaps any
/// still-orphaned child once the process exits.
pub(in crate::native) const SHUTDOWN_REAP_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Grace before a single-session close forces its output reader to EOF. A
/// healthy session's reader EOFs the instant its slave closes (well inside
/// this), so the forced path only fires when a `setsid`'d grandchild keeps the
/// slave open.
const CLOSE_READER_JOIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Poll interval while a close reaper waits for its pump join to complete.
const CLOSE_JOIN_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Join a local session's output pump under a bounded deadline, forcing the PTY
/// reader to EOF if it is wedged.
///
/// The pump reader blocks on the PTY master, which reports EOF only once every
/// slave fd is closed. `kill` SIGKILLs the child's process group, but a
/// `setsid`'d grandchild in a foreign group (e.g. an `ssh` ControlMaster /
/// ControlPersist mux, or a disowned daemon) that inherited the slave is out of
/// reach, so the master never EOFs and a plain `join` — plus this reaper, the
/// pump thread, the writer thread, and the master fds — would block forever.
/// After [`CLOSE_READER_JOIN_GRACE`] the reader is forced to EOF through the
/// session's wake pipe, so the pump exits and the join completes; nothing leaks
/// even on the grandchild-holds-the-slave case.
fn bounded_pump_join(pump: JoinHandle<()>, pty: Arc<Mutex<PtySession>>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let done = Arc::new(AtomicBool::new(false));
    let done_signal = done.clone();
    let joiner = std::thread::Builder::new()
        .name("odytty-pump-join".to_owned())
        .spawn(move || {
            let _ = pump.join();
            done_signal.store(true, Ordering::Release);
        });
    let Ok(joiner) = joiner else {
        // Could not spawn the joiner (resource exhaustion): best-effort force the
        // reader to EOF and abandon rather than block the reaper.
        if let Ok(guard) = pty.lock() {
            guard.force_reader_eof();
        }
        return;
    };

    let deadline = Instant::now() + CLOSE_READER_JOIN_GRACE;
    while Instant::now() < deadline {
        if done.load(Ordering::Acquire) {
            let _ = joiner.join();
            return;
        }
        std::thread::sleep(CLOSE_JOIN_POLL);
    }
    // Still wedged after the grace: force the reader to EOF so the pump exits.
    if let Ok(guard) = pty.lock() {
        guard.force_reader_eof();
    }
    let _ = joiner.join();
}

/// The in-pane status rendered for a held local command. A missing numeric code
/// is truthful rather than guessed: Unix reports no code for signal death, and
/// another backend may be unable to classify the post-EOF status.
pub(super) fn held_exit_banner(code: Option<i32>) -> String {
    let status = match code {
        Some(code) => format!("status {code}"),
        None => "unknown status (the process may have exited from a signal)".to_owned(),
    };
    format!("\r\n\x1b[1;33m[Process exited with {status}. Press any key to close.]\x1b[0m\r\n")
}

impl Session {
    /// Fire a best-effort remote cleanup for any images this session uploaded
    /// during its life (F6-i7). Runs the `rm -f` over a detached `ssh` (reusing
    /// the live `ControlMaster` when available) and never waits on it, so tab
    /// close stays instant. Best-effort by nature: if the link already dropped
    /// the command cannot run and the remote's own `/tmp` reaper removes the
    /// file. Compiled to a no-op under `cfg(test)` so closing a synthetic remote
    /// tab never spawns a real `ssh`.
    fn fire_upload_cleanup(&self) {
        let Some(upload) = self.upload.as_ref() else {
            return;
        };
        let paths = std::mem::take(&mut *crate::native::lock_recover(&upload.uploaded_handle()));
        // Under `cfg(test)` the paths are simply drained (no real `ssh`); the
        // discard keeps the binding used without a trailing no-op return.
        #[cfg(test)]
        let _ = paths;
        #[cfg(not(test))]
        if !paths.is_empty()
            && let Some(command) = crate::ssh_connect::remote_cleanup_command(
                upload.destination(),
                upload.port(),
                upload.control_dir(),
                &paths,
            )
        {
            let (program, args) = command.into_program_args();
            let mut cleanup = std::process::Command::new(program);
            cleanup
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // Fourth console-child spawn site: suppress the Windows console-window
            // flash on tab close for a remote session that uploaded images. No-op
            // off Windows. Mirrors the opener/ssh-probe/ssh-upload sites.
            crate::native::app::win_spawn::apply_no_console_window(&mut cleanup);
            match cleanup.spawn() {
                Ok(child) => {
                    // Hand the child to the shared detached reaper: it blocks in
                    // `Child::wait` until the cleanup `ssh` exits (Dropping the
                    // `Child` never waits, so on Unix every cleanup otherwise
                    // left a ZOMBIE until the whole app exited, one per closed
                    // remote tab that uploaded images). The reaper is never
                    // joined here (close stays instant), never delays process
                    // exit, and degrades to drop-without-wait if the reaper
                    // thread cannot be created. On Windows there is no zombie
                    // concept, but the reaper is harmless and keeps one behavior
                    // on both platforms.
                    crate::spawn_util::spawn_child_reaper("odytty-upload-cleanup-reaper", child);
                }
                Err(error) => {
                    // Best-effort by design, but a failed spawn is at least
                    // visible now instead of silently discarded; the remote's
                    // own /tmp reaper still bounds the leak.
                    tracing::warn!("remote upload cleanup spawn failed: {error}");
                }
            }
        }
    }

    pub(super) fn close(mut self) -> bool {
        self.fire_upload_cleanup();
        let pump_thread = self.pump_thread.take();
        match self.source {
            SessionSource::Local { pty } => {
                // Kill promptly on the calling (main) thread — SIGKILL delivery
                // never waits for the child to actually die.
                if let Ok(mut guard) = pty.lock() {
                    let _ = guard.kill();
                }
                // Defer the blocking reap (`wait`) + reader join to a detached
                // thread. CLOSE-HANG-2: a ControlPersist mux master (or any
                // `setsid`'d grandchild in a foreign process group) can hold the
                // PTY slave open after the child is killed, so the reader never
                // sees EOF on its own and a plain join would block forever —
                // leaking this reaper, the pump thread, the writer thread, and
                // the master fds. `bounded_pump_join` bounds the join and, if the
                // reader is still wedged after the grace, forces it to EOF via the
                // session's wake pipe so the pump exits and everything is
                // released. The close itself returns to the event loop at once.
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut guard) = pty.lock() {
                            let _ = guard.wait();
                        }
                        if let Some(thread) = pump_thread {
                            bounded_pump_join(thread, pty);
                        }
                    });
            }
            // Closing an attached tab is a clean detach: the host keeps the PTY
            // + terminal model alive for later reattach by id. `detach()` is
            // best-effort network I/O and the reader join is unbounded, so run
            // both off the main thread too (`Drop` on the client is the backstop).
            #[cfg(unix)]
            SessionSource::Attached { client } => {
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut guard) = client.lock() {
                            let _ = guard.detach();
                        }
                        if let Some(thread) = pump_thread {
                            let _ = thread.join();
                        }
                    });
            }
            // A headless test session owns no child, PTY, or pump thread, so
            // close is an immediate no-op with no thread and no blocking wait.
            #[cfg(test)]
            SessionSource::Headless { .. } => {
                debug_assert!(pump_thread.is_none());
            }
        }
        true
    }

    pub(super) fn close_after_shell_exit(mut self) -> bool {
        self.fire_upload_cleanup();
        let pump_thread = self.pump_thread.take();
        match &self.source {
            SessionSource::Local { pty } => {
                let pty = pty.clone();
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut guard) = pty.lock() {
                            let _ = guard.try_wait();
                        }
                        // Even after the shell self-exits, a `setsid`'d
                        // grandchild can keep the slave open, so bound the pump
                        // join and force the reader to EOF if it is wedged rather
                        // than leak the pump/writer threads and the master fds.
                        if let Some(thread) = pump_thread {
                            bounded_pump_join(thread, pty);
                        }
                    });
            }
            // The host child already exited (or the link dropped). Detach is
            // best-effort network I/O and the reader is ending on its own, but a
            // wedged control socket could still stall an inline detach + join —
            // run both off the main thread, matching the Local arm.
            #[cfg(unix)]
            SessionSource::Attached { client } => {
                let client = client.clone();
                let _ = std::thread::Builder::new()
                    .name("odytty-session-close".to_owned())
                    .spawn(move || {
                        if let Ok(mut client) = client.lock() {
                            let _ = client.detach();
                        }
                        if let Some(thread) = pump_thread {
                            let _ = thread.join();
                        }
                    });
            }
            // A headless test session owns no child or pump thread; nothing to
            // wait on or join.
            #[cfg(test)]
            SessionSource::Headless { .. } => {
                debug_assert!(pump_thread.is_none());
            }
        }
        true
    }

    /// Whole-app shutdown teardown for one session. SIGKILL the child *now*
    /// (synchronous but non-blocking — signal delivery never waits) so no
    /// runaway shell or `ssh` client survives the process, then return the
    /// blocking reap (`wait`) + pump-thread join as a deferred closure the
    /// caller runs OFF the main thread under a single bounded deadline
    /// ([`WorkspaceSet::shutdown_all`]). This is the CLOSE-HANG fix: a wedged
    /// remote — an `ssh` client parked in a dead-socket syscall whose `wait()`
    /// blocks for the kernel's network timeout, or a reader thread that never
    /// sees EOF — must never delay window teardown. The process is exiting, so
    /// if the deferred reaper outlives the deadline the OS reaps the orphaned
    /// child and the detached thread dies with the process. [`Self::close`]
    /// (single-tab close in a long-lived process) uses the same off-main-thread
    /// deferral, but its reaper runs to completion — reaping the zombie and
    /// joining the reader — rather than leaning on process exit, so a live
    /// process leaks neither.
    pub(super) fn shutdown(mut self) -> Box<dyn FnOnce() + Send> {
        self.fire_upload_cleanup();
        let pump_thread = self.pump_thread.take();
        match self.source {
            SessionSource::Local { pty } => {
                // Kill promptly on the calling thread — SIGKILL is delivered
                // without waiting for the child to actually die.
                if let Ok(mut guard) = pty.lock() {
                    let _ = guard.kill();
                }
                Box::new(move || {
                    if let Ok(mut guard) = pty.lock() {
                        let _ = guard.wait();
                    }
                    if let Some(thread) = pump_thread {
                        let _ = thread.join();
                    }
                })
            }
            // A detach is best-effort network I/O; defer it too so a wedged
            // control socket cannot stall teardown.
            #[cfg(unix)]
            SessionSource::Attached { client } => Box::new(move || {
                if let Ok(mut guard) = client.lock() {
                    let _ = guard.detach();
                }
                if let Some(thread) = pump_thread {
                    let _ = thread.join();
                }
            }),
            // A headless test session has no child or pump thread; the deferred
            // reaper is a no-op.
            #[cfg(test)]
            SessionSource::Headless { .. } => {
                debug_assert!(pump_thread.is_none());
                Box::new(move || {
                    let _ = pump_thread;
                })
            }
        }
    }
}

/// Workspace-level operations (design doc §3.1, ODP-3/-10). These are the model
/// half of the workspace layer: create / switch / rename / close a workspace and
/// query the workspace list. The keyboard/palette layer (W3) wires these in; the
/// rail chrome (W2) reuses them. None of them run until a second workspace
/// exists, so single-workspace behavior is untouched.
impl WorkspaceSet {
    /// Remove the workspace at `ws_idx` (its last tab just closed - no empty
    /// workspaces, ODP-3) and clamp `active_ws` onto a surviving workspace,
    /// mirroring the tab-removal clamp. Returns `true` iff no workspaces remain,
    /// i.e. the last workspace closed and the caller should signal app exit.
    fn remove_workspace(&mut self, ws_idx: usize) -> bool {
        self.workspaces.remove(ws_idx);
        if self.workspaces.is_empty() {
            self.active_ws = 0;
            return true;
        }
        if self.active_ws == ws_idx {
            self.active_ws = ws_idx.min(self.workspaces.len() - 1);
        } else if self.active_ws > ws_idx {
            self.active_ws -= 1;
        }
        false
    }

    /// Whole-app shutdown (CLOSE-HANG): drain every session, SIGKILL each child
    /// promptly on the calling thread, then reap + join them OFF the calling
    /// thread under a single bounded `deadline`. Draining first means
    /// [`Self::is_empty`] holds the instant this returns. A wedged remote can no
    /// longer freeze teardown: the blocking `wait()`/join runs on a detached
    /// reaper thread, and the caller waits at most `deadline` before letting the
    /// process exit (the OS reaps any orphaned child). Healthy sessions reap
    /// well within the budget — the wait returns as soon as the reaper signals,
    /// not after the full deadline. Replaces the old serial per-session
    /// `pty.wait()` + `pump_thread.join()` on the main thread, which blocked
    /// indefinitely on a hung `ssh` client and produced the Super+Q
    /// not-responding stall.
    pub(in crate::native) fn shutdown_all(&mut self, deadline: std::time::Duration) {
        let drained = std::mem::take(&mut self.sessions);
        self.workspaces.clear();
        self.active_ws = 0;
        // `Session::shutdown` sends SIGKILL synchronously as the reaper closures
        // are built, so every child is signalled before we wait on anything.
        let reapers: Vec<Box<dyn FnOnce() + Send>> =
            drained.into_values().map(Session::shutdown).collect();
        if reapers.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("odytty-shutdown-reap".to_owned())
            .spawn(move || {
                for reap in reapers {
                    reap();
                }
                let _ = tx.send(());
            });
        // If even the reaper thread fails to spawn, don't block — the process is
        // exiting and the OS reaps. Otherwise wait up to the deadline for a
        // clean reap, then detach.
        if spawned.is_ok() {
            let _ = rx.recv_timeout(deadline);
        }
    }

    /// Focus the tab (and pane) that owns `token`, scanning ALL workspaces so a
    /// selection can deep-switch the active workspace + tab + focused pane in
    /// one step (attach dedup / summon deep-focus, ODP-10). With a single
    /// workspace this is byte-identical to the previous same-workspace switch.
    /// Returns true when the focus target actually moved.
    pub(in crate::native) fn switch(&mut self, token: SessionToken) -> bool {
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return false;
        };
        let already = self.active_ws == ws_idx
            && self.workspaces[ws_idx].active_tab == tab_idx
            && self.workspaces[ws_idx].tabs[tab_idx].focused == token;
        if already {
            return false;
        }
        self.active_ws = ws_idx;
        self.workspaces[ws_idx].active_tab = tab_idx;
        self.workspaces[ws_idx].tabs[tab_idx].focused = token;
        true
    }

    pub(in crate::native) fn next(&mut self) -> bool {
        let ws = self.active_workspace_mut();
        if ws.tabs.len() <= 1 {
            return false;
        }
        ws.active_tab = (ws.active_tab + 1) % ws.tabs.len();
        true
    }

    pub(in crate::native) fn prev(&mut self) -> bool {
        let ws = self.active_workspace_mut();
        if ws.tabs.len() <= 1 {
            return false;
        }
        ws.active_tab = if ws.active_tab == 0 {
            ws.tabs.len() - 1
        } else {
            ws.active_tab - 1
        };
        true
    }

    pub(in crate::native) fn close(&mut self, token: SessionToken) -> bool {
        self.close_with(token, Session::close)
    }

    pub(in crate::native) fn close_shell_exited(&mut self, token: SessionToken) -> bool {
        self.close_with(token, Session::close_after_shell_exit)
    }

    /// Keep a local pane alive after its PTY reader reaches EOF and paint the
    /// command's status into the terminal model. Status collection is the same
    /// post-EOF `try_wait()` poll used by reconnect classification, so it never
    /// blocks the event-loop thread. Returns `false` for attached/headless
    /// sources, which cannot be the launch-scoped local command covered by
    /// `--hold`.
    pub(in crate::native) fn hold_after_shell_exit(&mut self, token: SessionToken) -> bool {
        let is_local = self
            .sessions
            .get(&token)
            .is_some_and(|session| matches!(session.source, SessionSource::Local { .. }));
        if !is_local {
            return false;
        }

        let banner = held_exit_banner(self.capture_exit_code(token));
        if let Some(session) = self.sessions.get_mut(&token) {
            crate::native::lock_recover(&session.terminal).advance(banner.as_bytes());
            session.needs_rebuild = true;
            session.last_render_signature = None;
            true
        } else {
            false
        }
    }

    /// Close the **entire active tab** — reap every leaf session in its layout
    /// tree and remove the tab from the strip — regardless of how many panes it
    /// holds. This is the deliberate "Close Tab" semantics: closing a
    /// tab closes the tab you are in even when it holds multiple panes, and it
    /// must not behave like "Close Pane".
    ///
    /// Distinct from [`Self::close`] / `close_focused_pane`, which collapse a
    /// single leaf into its sibling and keep a multi-pane tab alive. For a
    /// single-pane tab this reaps the one session and removes the tab —
    /// byte-identical to the old `close(active_id())` path (the `None` branch of
    /// [`Self::close_with`]).
    ///
    /// Returns `true` iff no workspaces remain afterward, i.e. the last tab of
    /// the last workspace was closed and the caller should signal app exit. A
    /// workspace's last tab closing closes that workspace (ODP-3); only the last
    /// workspace closing exits. Exit keys on the last tab of the last
    /// **workspace**, never on the last pane.
    pub(in crate::native) fn close_active_tab(&mut self) -> bool {
        self.close_tab_at(self.active_workspace().active_tab)
    }

    /// Close the tab at strip index `tab_idx` — reap every leaf session in its
    /// layout tree and remove the tab — regardless of pane count. This is the
    /// index-aware reap shared by every "Close Tab" entry point: the menu /
    /// keyboard ([`Self::close_active_tab`], which delegates here with the
    /// active index) and the tab-strip `×` button, which can target a
    /// **non-active** tab.
    ///
    /// Fixes `active_tab` exactly like `close_with`'s `None` branch: when the
    /// closed tab was the active one (or to its left) the active index shifts so
    /// it still points at a live tab; closing a tab to the right of the active
    /// one leaves the active index unchanged. When the closed tab was the
    /// workspace's last, the workspace is removed too (ODP-3); returns `true`
    /// iff no workspaces remain (signal app exit).
    ///
    /// For a single-pane tab this reaps the one session and removes the tab —
    /// byte-identical to the old `close(token)` path the `×` button used.
    pub(in crate::native) fn close_tab_at(&mut self, tab_idx: usize) -> bool {
        // The tab strip shows the active workspace, so a strip index resolves
        // there. Collect every owned leaf token first (owned `Vec`, so the
        // immutable borrow of the workspace's tabs ends before the reap loop
        // mutates `self`).
        let ws_idx = if self.active_ws < self.workspaces.len() {
            self.active_ws
        } else {
            0
        };
        let tokens = match self
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        {
            Some(tab) => tab.layout.leaves(),
            None => return self.workspaces.is_empty(),
        };
        for token in tokens {
            if let Some(session) = self.sessions.remove(&token) {
                let _ = session.close();
            }
        }
        let ws = &mut self.workspaces[ws_idx];
        let was_active = ws.active_tab == tab_idx;
        ws.tabs.remove(tab_idx);
        if ws.tabs.is_empty() {
            // Last tab of the workspace closed -> close the workspace; the last
            // workspace closing signals app exit (ODP-3).
            return self.remove_workspace(ws_idx);
        }
        // Mirror `close_with`'s `None` branch: clamp the active tab index when
        // the active (or an earlier) tab was removed, leave it untouched when a
        // later tab was closed.
        if was_active {
            ws.active_tab = tab_idx.min(ws.tabs.len() - 1);
        } else if ws.active_tab > tab_idx {
            ws.active_tab -= 1;
        }
        false
    }

    fn close_with(
        &mut self,
        token: SessionToken,
        close_session: impl FnOnce(Session) -> bool,
    ) -> bool {
        // A closing pane's session may live in a background workspace (a
        // background shell exiting), so locate it across ALL workspaces, not the
        // active one alone.
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return self.sessions.is_empty();
        };
        // Reap the session itself.
        if let Some(session) = self.sessions.remove(&token) {
            let _ = close_session(session);
        }
        // Remove the pane leaf, collapsing its split parent into the sibling.
        // For a single-pane tab this yields `None`, i.e. the tab closes — the
        // byte-identical analogue of removing a session from the old Vec.
        let ws = &mut self.workspaces[ws_idx];
        match ws.tabs[tab_idx].layout.clone().close_leaf(token) {
            None => {
                let was_active = ws.active_tab == tab_idx;
                ws.tabs.remove(tab_idx);
                if ws.tabs.is_empty() {
                    // Last tab of the workspace closed -> close the workspace;
                    // the last workspace closing signals app exit (ODP-3).
                    return self.remove_workspace(ws_idx);
                }
                if was_active {
                    ws.active_tab = tab_idx.min(ws.tabs.len() - 1);
                } else if ws.active_tab > tab_idx {
                    ws.active_tab -= 1;
                }
                false
            }
            Some(layout) => {
                // The tab survives (a multi-pane tab lost one pane). Refocus a
                // surviving leaf if the closed pane held focus.
                if ws.tabs[tab_idx].focused == token
                    && let Some(first) = layout.leaves().first().copied()
                {
                    ws.tabs[tab_idx].focused = first;
                }
                ws.tabs[tab_idx].layout = layout;
                // Closing a pane changes the tree; un-zoom so the survivor(s)
                // render at their layout geometry. Closing the zoomed pane must
                // un-zoom, and closing a background pane while zoomed also
                // re-tiles, so clear unconditionally.
                ws.tabs[tab_idx].zoomed = false;
                false
            }
        }
    }

    /// Test-only driver for a split: arena-insert `session` then graft it into
    /// the active tab by splitting the focused leaf along `axis`. Mirrors the
    /// production [`Self::split_active`] minus the PTY spawn. `pub(in
    /// crate::native)` so App-level seams can seed a multi-pane tab headlessly
    /// (the production split needs an event-loop proxy to spawn a PTY).
    #[cfg(test)]
    pub(in crate::native) fn split_active_for_test(
        &mut self,
        axis: SplitAxis,
        session: Session,
    ) -> SessionToken {
        let token = self.push_arena_only(session);
        self.split_active_with(axis, token);
        token
    }

    /// Split the **focused pane of the active tab** along `axis`, spawning a new
    /// session at `grid` for the new pane and giving it focus (tmux semantics:
    /// the new pane becomes `second` and is focused). Returns the new session's
    /// token. A no-op-and-error if there is no active tab or spawn fails. The
    /// new pane shares the tab — no new tab-strip entry is added.
    pub(in crate::native) fn split_active(
        &mut self,
        axis: SplitAxis,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        if self.active_tab_ref().is_none() {
            return Err(std::io::Error::other("no active tab to split"));
        }
        let new_token = self.insert_spawned_session(grid)?;
        self.split_active_with(axis, new_token);
        Ok(new_token)
    }

    /// Pure tree-mutation half of [`Self::split_active`]: graft `new_token` into
    /// the active tab by splitting its currently focused leaf along `axis` at the
    /// even ratio, then focus the new pane. Assumes `new_token` already exists in
    /// the arena. Factored out so headless tests can exercise the layout-tree
    /// behaviour without spawning a real PTY.
    fn split_active_with(&mut self, axis: SplitAxis, new_token: SessionToken) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let focused = tab.focused;
        let layout = std::mem::replace(&mut tab.layout, PaneNode::leaf(new_token));
        tab.layout = layout.split_leaf(focused, axis, EVEN_RATIO, new_token);
        tab.focused = new_token;
        // Splitting changes the tree, so any prior zoom no longer applies
        // (tmux un-zooms on split).
        tab.zoomed = false;
    }

    /// Reset every split ratio in the active tab to even (tmux `Ctrl-b =`).
    pub(in crate::native) fn equalize_active(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            let layout = std::mem::replace(&mut tab.layout, PaneNode::leaf(tab.focused));
            tab.layout = layout.equalized();
            // Equalize re-tiles every pane, so a zoom (which hides all but one)
            // is cleared — the user asked to see the balanced layout.
            tab.zoomed = false;
        }
    }

    /// Toggle zoom (tmux `Ctrl-b z`) on the active tab. A no-op on a single-pane
    /// tab (zoom is meaningless), where it returns `false` so the caller skips
    /// the reflow/redraw. Returns `true` when the zoom state flipped. The layout
    /// tree is never mutated — only the `zoomed` flag — so un-zoom restores the
    /// prior geometry exactly.
    pub(in crate::native) fn toggle_active_zoom(&mut self) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        if tab.layout.is_single_pane() {
            return false;
        }
        tab.zoomed = !tab.zoomed;
        true
    }

    /// Cycle focus to the next pane of the active tab in tree order (tmux
    /// `Ctrl-b o`). No geometry needed. Returns true if focus moved.
    pub(in crate::native) fn focus_next_pane(&mut self) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        match tab.layout.next_leaf_after(tab.focused) {
            Some(next) if next != tab.focused => {
                tab.focused = next;
                true
            }
            _ => false,
        }
    }

    /// Set the focused pane of the active tab to `token` when it is a pane of
    /// that tab (directional focus / focus-follows-click land the resolved
    /// token here). Returns true if focus changed.
    pub(in crate::native) fn set_active_focus(&mut self, token: SessionToken) -> bool {
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        if tab.focused == token || !tab.layout.contains(token) {
            return false;
        }
        tab.focused = token;
        true
    }

    /// Bind (or, with `None`, unbind) the active workspace to a host alias
    /// (F6-W5). Idempotent; the binding is captured in the shape snapshot so it
    /// survives restore. Returns the previous binding.
    pub(in crate::native) fn set_active_workspace_default_profile(
        &mut self,
        profile: Option<String>,
    ) -> Option<String> {
        std::mem::replace(&mut self.active_workspace_mut().default_profile, profile)
    }

    /// Bind (or, with `None`, unbind) the workspace at rail index `idx`
    /// (RAIL-BIND). Same semantics as the active-workspace form, but targets a
    /// specific slot so the rail menu can bind a workspace without first
    /// switching to it. Returns the previous binding; an out-of-range index is a
    /// no-op returning `None`.
    pub(in crate::native) fn set_workspace_default_profile_at(
        &mut self,
        idx: usize,
        profile: Option<String>,
    ) -> Option<String> {
        let ws = self.workspaces.get_mut(idx)?;
        std::mem::replace(&mut ws.default_profile, profile)
    }

    /// Spawn a fresh shell in a brand-new workspace appended after the current
    /// list and switch focus to it. The new workspace owns exactly one
    /// single-pane tab (no empty workspaces, ODP-3). Mirrors [`Self::spawn`] one
    /// level up. Returns the new session's token.
    pub(in crate::native) fn new_workspace(
        &mut self,
        grid: crate::core::Dimensions,
    ) -> Result<SessionToken, std::io::Error> {
        self.new_workspace_in(grid, None)
    }

    /// Create a fresh workspace whose first shell spawns in `cwd` (the cwd-aware
    /// variant of [`Self::new_workspace`]). Threads the working directory into
    /// the new workspace's single-pane shell exactly as [`Self::spawn`] does for
    /// a new tab, so Duplicate Workspace opens where the active pane already is.
    /// A `None` cwd spawns in the default directory, byte-identical to
    /// `new_workspace`. Cross-platform: the cwd flows through the same
    /// `insert_spawned_session_in` path the cwd-aware tab spawn uses, so ConPTY
    /// honors it on Windows (drive-letter OSC 7 cwds are already normalized).
    pub(in crate::native) fn new_workspace_in(
        &mut self,
        grid: crate::core::Dimensions,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<SessionToken, std::io::Error> {
        let token = self.insert_spawned_session_in(grid, cwd)?;
        let name = default_workspace_name(self.workspaces.len());
        self.workspaces.push(Workspace::single(name, token));
        self.active_ws = self.workspaces.len() - 1;
        Ok(token)
    }

    /// Switch the active workspace to rail index `idx` (its active tab's focused
    /// pane becomes the `Deref` target). Returns true when the active workspace
    /// actually changed; out-of-range or same-index requests are no-ops.
    pub(in crate::native) fn switch_workspace(&mut self, idx: usize) -> bool {
        if idx == self.active_ws || idx >= self.workspaces.len() {
            return false;
        }
        self.active_ws = idx;
        true
    }

    /// Cycle the active workspace forward (rail order, wrapping). No-op with a
    /// single workspace. Returns true when the active workspace changed.
    pub(in crate::native) fn next_workspace(&mut self) -> bool {
        if self.workspaces.len() <= 1 {
            return false;
        }
        self.active_ws = (self.active_ws + 1) % self.workspaces.len();
        true
    }

    /// Cycle the active workspace backward (rail order, wrapping). No-op with a
    /// single workspace. Returns true when the active workspace changed.
    pub(in crate::native) fn prev_workspace(&mut self) -> bool {
        if self.workspaces.len() <= 1 {
            return false;
        }
        self.active_ws = if self.active_ws == 0 {
            self.workspaces.len() - 1
        } else {
            self.active_ws - 1
        };
        true
    }

    /// Move the workspace at rail index `idx` one slot toward the front (`up`)
    /// or the back of the rail (RAIL-REORDER). An adjacent swap: the ACTIVE
    /// workspace follows by identity, so reordering never changes which
    /// workspace is focused -- if the active slot is one of the two swapped, its
    /// index moves with it. No-op (returns `false`) when the move would run off
    /// either end (idx 0 up, last idx down) or `idx` is out of range; otherwise
    /// returns `true`. The workspace list is a plain `Vec<Workspace>`, so the
    /// swap carries each workspace's whole state (name, tabs, binding) with it,
    /// and the shape snapshot captures the new order for restore.
    pub(in crate::native) fn move_workspace(&mut self, idx: usize, up: bool) -> bool {
        if idx >= self.workspaces.len() {
            return false;
        }
        let target = if up {
            if idx == 0 {
                return false;
            }
            idx - 1
        } else {
            if idx + 1 >= self.workspaces.len() {
                return false;
            }
            idx + 1
        };
        self.workspaces.swap(idx, target);
        // Follow the active workspace by identity across the swap: only the two
        // swapped slots change index, so at most one of them is the active one.
        if self.active_ws == idx {
            self.active_ws = target;
        } else if self.active_ws == target {
            self.active_ws = idx;
        }
        true
    }

    /// Move a tab in the active workspace from strip index `from` to insertion
    /// index `to` (`0..=tab_count`). The active tab follows its `Tab` identity,
    /// so reordering never changes the focused session or pane. Returns `false`
    /// for invalid indices and no-op drops.
    pub(in crate::native) fn reorder_tab(&mut self, from: usize, to: usize) -> bool {
        let ws = self.active_workspace_mut();
        if from >= ws.tabs.len() || to > ws.tabs.len() {
            return false;
        }
        let dest = if to > from { to - 1 } else { to };
        if dest == from {
            return false;
        }
        let active_token = ws.tabs.get(ws.active_tab).map(|tab| tab.focused);
        let tab = ws.tabs.remove(from);
        ws.tabs.insert(dest, tab);
        if let Some(token) = active_token
            && let Some(active) = ws.tabs.iter().position(|tab| tab.focused == token)
        {
            ws.active_tab = active;
        }
        true
    }

    /// Rename the workspace at rail index `idx`. Out-of-range requests are
    /// no-ops. Used by the "Rename Workspace" action / palette entry (targeting
    /// the active index) and, later, the rail's in-place rename.
    pub(in crate::native) fn rename_workspace(&mut self, idx: usize, name: String) {
        if let Some(ws) = self.workspaces.get_mut(idx) {
            ws.name = name;
        }
    }

    /// Close the ENTIRE active workspace — reap every session of every tab and
    /// remove the workspace from the rail — regardless of tab/pane count. The
    /// "Close Workspace" action (ODP-3). Returns `true` iff no workspaces remain,
    /// i.e. the last workspace was closed and the caller should signal app exit
    /// (the App-level guard that avoids emptying the arena before teardown, as in
    /// `close_active_tab`, is wired when the keybinding/menu lands).
    pub(in crate::native) fn close_active_workspace(&mut self) -> bool {
        let ws_idx = if self.active_ws < self.workspaces.len() {
            self.active_ws
        } else {
            0
        };
        let tokens: Vec<SessionToken> = self
            .workspaces
            .get(ws_idx)
            .map(|ws| ws.tabs.iter().flat_map(|tab| tab.layout.leaves()).collect())
            .unwrap_or_default();
        for token in tokens {
            if let Some(session) = self.sessions.remove(&token) {
                let _ = session.close();
            }
        }
        self.remove_workspace(ws_idx)
    }

    /// Move the tab that owns `token` out of its workspace and append it to the
    /// workspace at `dest_idx` (ODP-7, "Move to workspace"). This is a `Tab`
    /// VALUE splice between two `Workspace.tabs` vecs — the sessions never leave
    /// the global arena, so pump-thread lookup by token is untouched. The
    /// destination's active tab is left as-is (v1: move without following), and
    /// the active workspace is unchanged unless the SOURCE workspace empties, in
    /// which case it is removed (no empty workspaces, ODP-3) and `active_ws` is
    /// clamped onto a survivor. Returns `true` when a tab actually moved, and
    /// separately whether the source workspace was closed by the move. No-op
    /// (`(false, false)`) when the token is unknown, `dest_idx` is out of range,
    /// or source == destination.
    pub(in crate::native) fn move_tab_to_workspace(
        &mut self,
        token: SessionToken,
        dest_idx: usize,
    ) -> (bool, bool) {
        let Some((src_idx, tab_idx)) = self.locate_token(token) else {
            return (false, false);
        };
        if src_idx == dest_idx || dest_idx >= self.workspaces.len() {
            return (false, false);
        }
        // Splice the tab value out of the source and append it to the
        // destination. Both indices are still valid here — no workspace is
        // removed until after the move.
        let tab = self.workspaces[src_idx].tabs.remove(tab_idx);
        self.workspaces[dest_idx].tabs.push(tab);
        // Source bookkeeping: an empty source workspace closes (ODP-3);
        // otherwise clamp its active-tab index exactly like a tab close.
        if self.workspaces[src_idx].tabs.is_empty() {
            let closed = self.remove_workspace(src_idx);
            // `remove_workspace` returns whether NO workspaces remain; here at
            // least the destination survives, so it is always `false`. The
            // source workspace was nonetheless closed.
            debug_assert!(!closed);
            return (true, true);
        }
        let src = &mut self.workspaces[src_idx];
        if src.active_tab == tab_idx {
            src.active_tab = tab_idx.min(src.tabs.len() - 1);
        } else if src.active_tab > tab_idx {
            src.active_tab -= 1;
        }
        (true, false)
    }

    /// The destination candidates for the "Move to Workspace" picker (W4-v2):
    /// every workspace EXCEPT the one that owns `token`, as `(original rail
    /// index, name)` pairs. The index is the [`Self::move_tab_to_workspace`]
    /// target. Empty when the token is unknown or it is the only workspace
    /// (nothing to move to), which suppresses the picker.
    pub(in crate::native) fn move_tab_destinations(
        &self,
        token: SessionToken,
    ) -> Vec<(usize, String)> {
        let Some((src_idx, _)) = self.locate_token(token) else {
            return Vec::new();
        };
        self.workspaces
            .iter()
            .enumerate()
            .filter(|(idx, _)| *idx != src_idx)
            .map(|(idx, ws)| (idx, ws.name.clone()))
            .collect()
    }

    /// Move the just-appended, currently-active tab so it sits immediately after
    /// the tab that owns `anchor` in the active workspace (ODP-5D "Connect to
    /// host ▸": the new remote tab reads as opening from the clicked tab). The
    /// moved tab is assumed to be the last strip entry — the state left by
    /// `connect_ssh_in_new_tab` + `switch`. A no-op when the anchor is missing,
    /// is itself the last tab, or the moved tab is already directly after it. On
    /// success `active_tab` follows the moved tab to its new index so it stays
    /// focused.
    pub(in crate::native) fn reposition_active_tab_after(&mut self, anchor: SessionToken) {
        let ws = self.active_workspace_mut();
        let last = ws.tabs.len().saturating_sub(1);
        let Some(anchor_idx) = ws.tabs.iter().position(|t| t.layout.contains(anchor)) else {
            return;
        };
        // Anchor is the last tab (or the moved tab itself), or the moved tab is
        // already the neighbour just after the anchor — nothing to reorder.
        if anchor_idx + 1 >= last {
            return;
        }
        let dest = anchor_idx + 1;
        let tab = ws.tabs.remove(last);
        ws.tabs.insert(dest, tab);
        ws.active_tab = dest;
    }

    /// Whether any pane of the tab that owns `token` has a running foreground
    /// job (ODP-5D replace gating). Scans that tab's leaf sessions across ALL
    /// workspaces so a background tab can be probed; an attached pane always
    /// reports not-running (its job lives on the remote host). `false` when the
    /// token resolves to no tab.
    pub(in crate::native) fn tab_foreground_job_running(&self, token: SessionToken) -> bool {
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return false;
        };
        self.workspaces[ws_idx].tabs[tab_idx]
            .layout
            .leaves()
            .into_iter()
            .any(|leaf| {
                self.sessions
                    .get(&leaf)
                    .is_some_and(Session::foreground_job_running)
            })
    }

    /// Whether a shell exit on `token` would close its ENTIRE workspace: the
    /// session is the sole pane of the sole tab of its workspace, so reaping it
    /// empties the workspace (SHELL-EXIT-CLOSES). Drives the App-mode exit
    /// setting, which escalates a workspace-closing shell exit into an app quit.
    /// `false` when the token is unknown, its tab has sibling panes, or its
    /// workspace has sibling tabs -- those exits close only the pane or tab.
    pub(in crate::native) fn shell_exit_closes_workspace(&self, token: SessionToken) -> bool {
        let Some((ws_idx, tab_idx)) = self.locate_token(token) else {
            return false;
        };
        let ws = &self.workspaces[ws_idx];
        ws.tabs.len() == 1 && ws.tabs[tab_idx].layout.leaves().len() == 1
    }

    /// Whether any session OTHER than `token` has a running foreground job
    /// (SHELL-EXIT-CLOSES). The App-mode exit quit reuses the window-close
    /// confirmation when this is true, so quitting on a shell exit cannot
    /// silently kill a live job in another workspace. The exiting session itself
    /// is excluded because it has already ended. Attached sessions report
    /// not-running (their job lives on the remote host), matching the
    /// confirm-close semantics elsewhere.
    pub(in crate::native) fn any_foreground_job_running_except(&self, token: SessionToken) -> bool {
        self.sessions
            .iter()
            .any(|(id, session)| *id != token && session.foreground_job_running())
    }
}
