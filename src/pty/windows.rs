// SPDX-License-Identifier: GPL-3.0-only
//! Windows ConPTY backend for [`crate::pty`].
//!
//! Implements the same [`PtySession`] surface the Unix backend provides, but
//! over a Windows pseudoconsole (ConPTY) rather than a POSIX master/slave PTY.
//! The shape is fundamentally different:
//!
//! - There is **no fork/exec, no controlling terminal, and no process group**.
//!   A child is launched with `CreateProcessW`, attaching the pseudoconsole via
//!   `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` on a `STARTUPINFOEXW` attribute list.
//! - There is **no `(File, File)` master/slave pair**, so `open_pty_pair` has no
//!   Windows analogue and stays `#[cfg(unix)]` only. ConPTY creation lives
//!   inside [`PtySession::spawn_command`] instead.
//! - Host I/O flows over two anonymous pipes: the parent writes child input on
//!   one and reads child output on the other. The two ConPTY-owned pipe ends
//!   must be closed in the parent immediately after `CreatePseudoConsole`, or
//!   reads never observe EOF.
//! - There is no POSIX foreground process group, so [`PtySession::foreground_job`]
//!   always returns [`ForegroundJob::Unknown`] — the documented safe default the
//!   shared contract already treats as "safe to close".
//!
//! ConPTY is a VT translation layer: the child's Win32 console-API activity is
//! rendered to VT sequences on the output pipe, so output can differ from a raw
//! Unix PTY. Echo and line discipline are owned by the console host, not POSIX
//! termios. `kill` terminates only the root process (no process-group / job
//! teardown yet — a Job Object is the documented follow-up).

use core::ffi::c_void;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
// `AtomicUsize`/`Ordering` back the test-only `resize_calls` /
// `kernel_resizes` counters, so they are unused in a non-test Windows build —
// gate the imports to match (Standing rule C: a Windows-only file's lints are
// invisible to the Linux gate). P2-FIX removed the last non-test atomic (the
// old `hpcon_closed: AtomicBool` became the mutex-guarded flag in
// [`PconShared`]).
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED,
};
use windows::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows::Win32::System::Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTUPINFOEXW, STARTUPINFOW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use super::{CommandBuilder, ForegroundJob};
use crate::core::Dimensions;

/// `GetExitCodeProcess` reports a still-running process with the sentinel exit
/// code `STILL_ACTIVE` (259). Compared as a raw `u32` per the ConPTY reference,
/// avoiding the `NTSTATUS`-typed constant.
const STILL_ACTIVE_CODE: u32 = 259;

/// The exit code used when force-terminating a child via `TerminateProcess`.
const KILL_EXIT_CODE: u32 = 1;

pub struct PtySession {
    /// The pseudoconsole handle plus its close state, shared with the
    /// child-waiter thread. P2-FIX: `ResizePseudoConsole` and
    /// `ClosePseudoConsole` are serialized under the [`PconShared`] mutex so a
    /// UI-thread resize can never overlap the waiter's asynchronous close of
    /// the same HPCON (a use-after-free inside conhost). Also carries the
    /// original close-exactly-once guarantee across the three closers —
    /// natural child exit (waiter), explicit `kill`, and `Drop`.
    pcon: Arc<PconShared>,
    /// The child process handle. Owned: closed when the session drops.
    process: OwnedHandle,
    /// Parent end of the input pipe (host → child). Kept as an owned handle so
    /// `take_writer` can `DuplicateHandle` an independent writer from it,
    /// mirroring the Unix backend's `master.try_clone()`.
    input_write: OwnedHandle,
    /// Parent end of the output pipe (child → host). Source for the duplicated
    /// reader handed to `try_clone_reader`.
    output_read: OwnedHandle,
    /// Child-waiter thread: a single blocking `WaitForSingleObject(.., INFINITE)`
    /// on a *duplicated* process handle that wakes exactly once when the child
    /// exits on its own, then closes the pseudoconsole so the output reader hits
    /// EOF and the app tears the session down through its normal path. `Some`
    /// until joined in `Drop`. This is what makes a shell that exits by itself
    /// (e.g. the user types `exit`) close its tab without an explicit `kill`.
    waiter: Option<JoinHandle<()>>,
    /// One-shot slot for a startup-failure diagnostic. If the child exits
    /// abnormally *shortly* after spawn (a loader/DLL-init failure — see
    /// [`describe_immediate_exit`]), the child-waiter thread records a
    /// human-readable line here before closing the pseudoconsole. The PTY pump
    /// drains it on the resulting reader EOF and writes it into the pane exactly
    /// once, so a shell that dies during its own initialization surfaces a
    /// reason instead of a silently blank/vanishing session. `None` for the
    /// healthy path and for normal (clean or late) exits. See
    /// [`PtySession::pending_diagnostic_slot`].
    pending_diagnostic: Arc<Mutex<Option<String>>>,
    /// Test-only counter of kernel `resize` calls (`ResizePseudoConsole`). Lets
    /// a headless test assert the divider-drag coalescing fires ONE resize at
    /// drag-end instead of one per pointer-move. Not built outside tests.
    #[cfg(test)]
    resize_calls: AtomicUsize,
}

impl PtySession {
    pub fn spawn_default_shell(dimensions: Dimensions) -> Result<Self> {
        Self::spawn_default_shell_in(dimensions, None)
    }

    pub fn spawn_default_shell_in(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
    ) -> Result<Self> {
        Self::spawn_default_shell_in_with_settings(
            dimensions,
            working_directory,
            &crate::settings::Settings::default(),
        )
    }

    pub fn spawn_default_shell_in_with_settings(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
        settings: &crate::settings::Settings,
    ) -> Result<Self> {
        let mut command = CommandBuilder::new(default_shell().program);
        command.apply_terminal_env();
        if let Some(path) = working_directory {
            command.current_dir(path);
        }
        // Gate on the same `shell_integration` setting the Unix path honors.
        // The injector classifies the resolved program (pwsh/powershell) and
        // attaches `-NoExit -Command <snippet>`; cmd.exe is left untouched.
        if settings.shell_integration {
            crate::shell_integration::apply_spawn_integration(&mut command);
        }
        Self::spawn_command(dimensions, command)
    }

    pub fn spawn_shell_command(dimensions: Dimensions, command: &str) -> Result<Self> {
        // One-shot: run `command` in the default shell and exit. The flag form
        // differs by shell family (cmd `/C` vs PowerShell `-NoProfile -Command`),
        // so resolve the shell and select the arguments accordingly — the
        // Windows analogue of the Unix `$SHELL -lc` split, not a login shell.
        let shell = default_shell();
        let mut command_builder = CommandBuilder::new(shell.program);
        for arg in one_shot_args(shell.kind, command) {
            command_builder.arg(arg);
        }
        command_builder.apply_terminal_env();
        Self::spawn_command(dimensions, command_builder)
    }

    pub fn spawn_exec(
        dimensions: Dimensions,
        program: OsString,
        args: Vec<OsString>,
        working_directory: Option<PathBuf>,
    ) -> Result<Self> {
        let mut command = CommandBuilder::new(program);
        for arg in args {
            command.arg(arg);
        }
        command.apply_terminal_env();
        if let Some(path) = working_directory {
            command.current_dir(path);
        }
        Self::spawn_command(dimensions, command)
    }

    pub fn spawn_command(dimensions: Dimensions, command: CommandBuilder) -> Result<Self> {
        // SAFETY: the whole spawn sequence is a chain of Win32 calls whose
        // ordering and handle ownership rules are documented inline. Every raw
        // handle is wrapped in an `OwnedHandle`/RAII guard immediately after
        // creation so any early return frees it; the ConPTY-owned pipe ends are
        // dropped right after `CreatePseudoConsole`.
        unsafe {
            // 1. Two anonymous pipes: input (host→child) and output (child→host).
            let (input_read, input_write) = create_pipe().context("CreatePipe (input)")?;
            let (output_read, output_write) = create_pipe().context("CreatePipe (output)")?;

            // 2. Create the pseudoconsole. It consumes `input_read` (its stdin)
            //    and `output_write` (its stdout); it duplicates them internally,
            //    so the parent copies are dropped immediately after — REQUIRED,
            //    or the output reader never sees EOF.
            let size = coord(dimensions);
            let hpcon =
                CreatePseudoConsole(size, handle_of(&input_read), handle_of(&output_write), 0)
                    .context("CreatePseudoConsole")?;
            // Guard the pseudoconsole so any `?` early return below (attribute
            // setup or `CreateProcessW`) closes it instead of leaking it — Drop
            // for PtySession only runs once `Self` is constructed at the bottom.
            let hpcon_guard = HpconGuard(hpcon);
            drop(input_read);
            drop(output_write);

            // 3. Size and build the proc-thread attribute list (single attribute:
            //    the pseudoconsole). The first sizing call is expected to fail in
            //    Win32 terms while still writing the required byte count.
            let mut attr_size: usize = 0;
            let _ = InitializeProcThreadAttributeList(None, 1, None, &mut attr_size);
            let mut attr_buf = vec![0u8; attr_size];
            let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr().cast::<c_void>());
            InitializeProcThreadAttributeList(Some(attr_list), 1, None, &mut attr_size)
                .context("InitializeProcThreadAttributeList")?;
            let _attr_guard = AttrListGuard(attr_list);

            // CRITICAL: `lpValue` for PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE must be
            // the HPCON handle VALUE itself, NOT a pointer to the local `hpcon`
            // variable. Every canonical sample (Microsoft's EchoCon, the Console
            // docs, node-pty, wezterm) passes `hPC` directly with
            // `cbSize = sizeof(HPCON)`. Passing `&hpcon` instead hands the kernel
            // a stack address in place of the pseudoconsole handle; the child
            // then attaches to a garbage pseudoconsole and dies during console
            // wireup in DLL init with exit code 0xC0000142 (STATUS_DLL_INIT_FAILED)
            // — while our direct conhost child (created correctly by
            // CreatePseudoConsole) stays alive. `HPCON` is `HPCON(pub isize)` in
            // the `windows` crate, so `hpcon.0` is the handle value; reinterpret
            // it as the `lpValue` pointer exactly as the C samples pass `hPC`.
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(hpcon.0 as *const c_void),
                size_of::<HPCON>(),
                None,
                None,
            )
            .context("UpdateProcThreadAttribute")?;

            // 4. STARTUPINFOEXW carrying the attribute list. Built as a struct
            //    literal (not default-then-reassign) to satisfy the
            //    `field_reassign_with_default` lint.
            let startup = STARTUPINFOEXW {
                StartupInfo: STARTUPINFOW {
                    cb: size_of::<STARTUPINFOEXW>() as u32,
                    ..Default::default()
                },
                lpAttributeList: attr_list,
            };

            // 5. Command line (CreateProcessW may mutate it, so it must be a
            //    mutable, NUL-terminated UTF-16 buffer) and cwd.
            //
            //    Environment: we hand the child a PER-CHILD merged environment
            //    block (see [`build_env_block`]) and pass `lpEnvironment` pointing
            //    at it with `CREATE_UNICODE_ENVIRONMENT`. The block is THIS
            //    process's full environment as reported by `GetEnvironmentStringsW`
            //    — which (unlike Rust's `std::env::vars_os`) preserves the loader-
            //    critical hidden `=`-prefixed per-drive working-directory variables
            //    (`=C:=…`) and the leading `=::=::\` marker, whose absence makes a
            //    child fail DLL initialization with `0xC0000142`
            //    (STATUS_DLL_INIT_FAILED) — with OdyTTY's constant terminal-
            //    identification overrides (TERM/COLORTERM/TERM_PROGRAM[_VERSION])
            //    applied on top. Building a per-child block (rather than mutating
            //    our OWN process env via `SetEnvironmentVariableW` and inheriting)
            //    keeps every override scoped to the child it targets, with no
            //    global process-env mutation — the latent cross-session leak/race
            //    the moment any per-session env (per-profile TERM, SSH-in-a-tab)
            //    exists. `env_block` must outlive the `CreateProcessW` call below.
            let env_block = build_env_block(&command.env);
            let mut command_line = build_command_line(&command);
            let cwd_wide = command
                .current_dir
                .as_ref()
                .map(|p| to_wide_nul(p.as_os_str()));
            let cwd = match &cwd_wide {
                Some(w) => PCWSTR(w.as_ptr()),
                None => PCWSTR::null(),
            };

            let mut process_info = PROCESS_INFORMATION::default();
            // 6. Spawn. The pseudoconsole is injected via the attribute list, not
            //    handle inheritance, so `bInheritHandles` is false.
            //    `CREATE_UNICODE_ENVIRONMENT` marks `lpEnvironment` (our merged
            //    UTF-16 block) as wide; see the note above.
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                Some(env_block.as_ptr().cast::<c_void>()),
                cwd,
                (&startup as *const STARTUPINFOEXW).cast::<STARTUPINFOW>(),
                &mut process_info,
            )
            .context("CreateProcessW")?;

            // 7. Keep the process handle; the thread handle and attribute list
            //    are no longer needed. (`_attr_guard` deletes the list on drop.)
            let _ = CloseHandle(process_info.hThread);
            let process = OwnedHandle::from_raw_handle(process_info.hProcess.0 as RawHandle);

            // Spawn succeeded: transfer pseudoconsole ownership into `Self` so
            // the close paths (waiter / kill / Drop) are the only
            // `ClosePseudoConsole` callers. Forgetting the guard prevents a
            // double-close.
            std::mem::forget(hpcon_guard);

            // Start the child-waiter thread: it blocks on a duplicated process
            // handle and, when the child exits on its own, (a) records a
            // startup-failure diagnostic into `pending_diagnostic` if the exit
            // was abnormal AND immediate, then (b) closes the pseudoconsole so a
            // self-exiting shell tears its session down via the same reader-EOF
            // path a `kill` uses (the pump drains the diagnostic on that EOF). A
            // dup failure is non-fatal — the session still works; only natural-
            // exit auto-close + diagnostic is lost — so the waiter is `None` then.
            let pcon = Arc::new(PconShared::new(hpcon.0));
            let pending_diagnostic = Arc::new(Mutex::new(None));
            let waiter =
                spawn_child_waiter(&process, Arc::clone(&pcon), Arc::clone(&pending_diagnostic));

            Ok(Self {
                pcon,
                process,
                input_write,
                output_read,
                waiter,
                pending_diagnostic,
                #[cfg(test)]
                resize_calls: AtomicUsize::new(0),
            })
        }
    }

    /// No-op on Windows: `ResizePseudoConsole` carries only a `COORD`
    /// (columns × rows) — a ConPTY has no pixel-geometry field to populate.
    /// Present for cross-platform API parity so the native layer can call
    /// `pty.set_cell_metrics(..)` unconditionally (the Unix backend uses it to
    /// fill `ws_xpixel`/`ws_ypixel` on TIOCSWINSZ).
    pub fn set_cell_metrics(&self, _metrics: crate::core::CellMetrics) {}

    pub fn resize(&self, dimensions: Dimensions) -> Result<()> {
        #[cfg(test)]
        self.resize_calls.fetch_add(1, Ordering::Relaxed);
        // P2-FIX: serialized against `ClosePseudoConsole` inside `PconShared`;
        // a resize racing the child-waiter's asynchronous close is either
        // ordered before it (valid handle) or after it (clean no-op).
        self.pcon.resize(coord(dimensions))
    }

    /// Test-only: how many times [`resize`](Self::resize) has been called on
    /// this session. Drives the divider-drag coalescing assertion.
    #[cfg(test)]
    pub fn resize_call_count(&self) -> usize {
        self.resize_calls.load(Ordering::Relaxed)
    }

    /// Whether the shell on this backend authoritatively repaints with ABSOLUTE
    /// cursor positioning on every resize. True for ConPTY: `ResizePseudoConsole`
    /// makes conhost reflow its own screen buffer and re-emit an absolute `CUP`
    /// on every resize, independent of the foreground app. So OdyTTY must NOT
    /// translate the cursor itself — doing so fights conhost's repaint and
    /// flings the cursor rows away from where PSReadLine places it. The terminal
    /// defers cursor placement to the shell when this is true.
    pub fn shell_repaints_on_resize(&self) -> bool {
        true
    }

    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        let file = duplicate(&self.output_read).context("clone pty reader")?;
        Ok(Box::new(PtyReader { file }))
    }

    pub fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        let file = duplicate(&self.input_write).context("clone pty writer")?;
        Ok(Box::new(PtyWriter { file }))
    }

    /// ConPTY has no POSIX foreground process group, so this is always
    /// [`ForegroundJob::Unknown`] — the contract's safe "indeterminate" default
    /// (callers treat it as safe-to-close, never prompting).
    pub fn foreground_job(&self) -> ForegroundJob {
        ForegroundJob::Unknown
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let mut code: u32 = 0;
        // SAFETY: `self.process` is a live, owned process handle.
        unsafe {
            GetExitCodeProcess(self.process_handle(), &mut code).context("poll child")?;
        }
        if code == STILL_ACTIVE_CODE {
            Ok(None)
        } else {
            Ok(Some(ExitStatus::from_raw(code)))
        }
    }

    pub fn wait(&mut self) -> Result<ExitStatus> {
        // SAFETY: live owned process handle; `INFINITE` blocks until exit.
        let status = unsafe { WaitForSingleObject(self.process_handle(), INFINITE) };
        if status == WAIT_FAILED {
            return Err(io::Error::last_os_error()).context("wait for child");
        }
        let mut code: u32 = 0;
        unsafe {
            GetExitCodeProcess(self.process_handle(), &mut code).context("exit code")?;
        }
        Ok(ExitStatus::from_raw(code))
    }

    pub fn kill(&mut self) -> Result<()> {
        // Terminate the child if it is still running. Unlike POSIX `kill` (which
        // signals the process group, after which the master read returns
        // EIO→EOF once the slave closes), a ConPTY child's death does NOT close
        // the pseudoconsole's pipe ends. So after terminating we must also close
        // the pseudoconsole here — that is the only thing that releases ConPTY's
        // internal copy of the output pipe's write end and lets a blocked output
        // reader observe EOF. Without it the session-close path's
        // `pump_thread.join()` blocks forever on a reader that never completes
        // (and the child may already have exited, so this runs unconditionally,
        // not only when `TerminateProcess` fires).
        let outcome = (|| -> Result<()> {
            if self.try_wait()?.is_none() {
                // SAFETY: live owned process handle. Terminates only the root
                // process; child-tree teardown via a Job Object is a documented
                // follow-up.
                unsafe {
                    TerminateProcess(self.process_handle(), KILL_EXIT_CODE)
                        .context("kill child")?;
                }
            }
            Ok(())
        })();
        // Always close the pseudoconsole — even if the poll/terminate above
        // failed — so a blocked output reader still observes EOF and the close
        // path's `pump_thread.join()` cannot deadlock.
        self.close_pcon();
        outcome
    }

    /// Close the pseudoconsole exactly once, releasing ConPTY's internal copy of
    /// the output pipe's write end so any blocked output reader observes EOF.
    /// Idempotent across all three closers — the child-waiter thread, `kill`,
    /// and `Drop` — and serialized against `resize` (see [`PconShared`]).
    fn close_pcon(&mut self) {
        self.pcon.close_once();
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>> {
        let mut reader = self.try_clone_reader()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).context("read pty output")?;
        Ok(bytes)
    }

    /// A clone of this session's one-shot startup-failure diagnostic slot, for
    /// the PTY pump to drain on reader EOF.
    ///
    /// A shell child that dies during its own initialization (missing DLL, bad
    /// entrypoint, a `STATUS_DLL_INIT_FAILED` `0xC0000142`) makes `CreateProcessW`
    /// *succeed* and then exits a moment later, so the spawn returns `Ok` yet the
    /// pane would otherwise stay blank. The child-waiter thread detects that
    /// abnormal-and-immediate exit, records a human-readable line into this slot
    /// (and echoes it to stderr), then closes the pseudoconsole — which makes the
    /// pump's output reader observe EOF. The pump takes the line from here on
    /// that EOF and writes it into the pane exactly once, replacing the old
    /// synchronous 250 ms spawn-path wait with a zero-cost path that surfaces the
    /// reason through the normal `ShellExited` teardown instead of blocking
    /// startup. Returns `None` for the healthy path and for normal exits.
    pub fn pending_diagnostic_slot(&self) -> Option<Arc<Mutex<Option<String>>>> {
        Some(Arc::clone(&self.pending_diagnostic))
    }

    fn process_handle(&self) -> HANDLE {
        HANDLE(self.process.as_raw_handle())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // `kill` terminates the child if it is still running and closes the
        // pseudoconsole (idempotent via `PconShared`, so a prior `close()` →
        // `kill()` is not double-closed here). `wait` then reaps the child. The
        // owned pipe/process handles close when their `OwnedHandle` fields drop
        // after this body.
        let _ = self.kill();
        let _ = self.wait();
        self.close_pcon();
        // Join the child-waiter thread. `kill` has terminated the child (or it
        // already exited), so the waiter's blocking `WaitForSingleObject` has
        // returned and the thread is finishing — the join cannot hang, and it
        // guarantees the waiter never touches state after the session drops.
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
    }
}

/// The pseudoconsole handle and its close state, shared between the session
/// and the child-waiter thread.
///
/// P2-FIX (confirmed by P6-REPRO trace): the child-waiter closes the HPCON
/// asynchronously the instant the child self-exits, while a UI-thread resize
/// (window resize / divider drag) can be in flight on the still-alive
/// `PtySession` — `ResizePseudoConsole` racing `ClosePseudoConsole` on the
/// same HPCON is a use-after-free inside conhost (ConPTY does not document
/// thread-safety for concurrent Resize/Close). A flag-only check would be
/// TOCTOU — the waiter can close between the load and the resize call — so
/// the mutex spans the Win32 calls themselves: `resize` and `close_once` can
/// never overlap, and resize observes a definitively open-or-closed handle.
///
/// The `closed` flag under the lock also carries the original
/// close-exactly-once guarantee across the three closers (waiter, `kill`,
/// `Drop`); closing releases ConPTY's internal copy of the output pipe's
/// write end so a blocked output reader observes EOF.
struct PconShared {
    /// Raw `HPCON` value. Stored as `isize` (its newtype payload) so the
    /// struct is trivially `Send`/`Sync`; rewrapped as `HPCON` at call sites.
    hpcon: isize,
    /// `true` once [`ClosePseudoConsole`] has run. Guarded by the mutex whose
    /// critical sections span the `ResizePseudoConsole`/`ClosePseudoConsole`
    /// calls (see the type docs for why a bare atomic is not enough).
    closed: Mutex<bool>,
    /// Test-only counter of `ResizePseudoConsole` calls that actually reached
    /// the kernel (i.e. were NOT skipped by the closed fast-path). Lets the
    /// resize-after-close regression test assert the skip structurally.
    #[cfg(test)]
    kernel_resizes: AtomicUsize,
}

impl PconShared {
    fn new(hpcon: isize) -> Self {
        Self {
            hpcon,
            closed: Mutex::new(false),
            #[cfg(test)]
            kernel_resizes: AtomicUsize::new(0),
        }
    }

    /// Lock the close state, recovering from a poisoned mutex. The guarded
    /// data is a plain `bool` with no invariants a panic could break, and both
    /// critical sections must still run during teardown even if some other
    /// holder panicked, so poison recovery is safe and required.
    fn lock_closed(&self) -> std::sync::MutexGuard<'_, bool> {
        self.closed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Resize the pseudoconsole, or no-op `Ok(())` if it has been closed.
    /// Holding the lock across the Win32 call is the point: the waiter's
    /// `close_once` cannot free the handle mid-resize.
    fn resize(&self, size: COORD) -> Result<()> {
        let closed = self.lock_closed();
        if *closed {
            // The pseudoconsole is already torn down (the session is on its
            // way out through reader-EOF teardown); a late resize is a no-op,
            // not an error — the caller's pane is about to disappear.
            return Ok(());
        }
        #[cfg(test)]
        self.kernel_resizes.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `hpcon` came from a successful `CreatePseudoConsole`; the
        // held lock guarantees `ClosePseudoConsole` has not run and cannot run
        // concurrently.
        unsafe { ResizePseudoConsole(HPCON(self.hpcon), size).context("resize pty") }
    }

    /// Close the pseudoconsole exactly once across every closer (the
    /// child-waiter thread, `kill`, and `Drop`); serialized against `resize`.
    fn close_once(&self) {
        let mut closed = self.lock_closed();
        if !*closed {
            *closed = true;
            // SAFETY: `hpcon` came from a successful `CreatePseudoConsole` and
            // is closed exactly once thanks to the flag under the held lock;
            // no `ResizePseudoConsole` can be in flight while we hold it.
            unsafe {
                ClosePseudoConsole(HPCON(self.hpcon));
            }
        }
    }
}

/// How soon after spawn an abnormal child exit is treated as a *startup*
/// failure worth reporting (rather than a normal `exit 1` after real use). A
/// shell that dies during loader/DLL init does so within milliseconds; this is
/// a generous bound that still excludes any genuine interactive session.
const STARTUP_FAILURE_WINDOW: Duration = Duration::from_millis(500);

/// Spawn the child-waiter thread for a freshly created session.
///
/// It owns a *duplicated* process handle (so it never races `try_wait`/`wait`/
/// `Drop` on the session's own handle) and performs a single, blocking,
/// zero-CPU `WaitForSingleObject(.., INFINITE)` — NOT a poll/sleep loop — that
/// wakes exactly once when the child exits. On wake it:
///   1. reads the child's exit code and, if it exited *abnormally and within*
///      [`STARTUP_FAILURE_WINDOW`] of spawn, records a startup-failure line into
///      `diagnostic` and echoes it to stderr (this folds the former synchronous
///      250 ms `diagnose_immediate_exit` spawn-path wait into the wait that
///      already exists — no startup tax); then
///   2. closes the pseudoconsole (idempotent via the shared flag), which makes
///      the pump's blocked output reader observe EOF; the app then tears the
///      session down through its single existing `ShellExited` path, and the
///      pump writes any recorded diagnostic into the pane on that EOF.
///
/// The thread then exits and its duplicated handle closes. Returns `None` if the
/// handle could not be duplicated (non-fatal: the session still works, only
/// natural-exit auto-close + the diagnostic are lost).
fn spawn_child_waiter(
    process: &OwnedHandle,
    pcon: Arc<PconShared>,
    diagnostic: Arc<Mutex<Option<String>>>,
) -> Option<JoinHandle<()>> {
    let dup = duplicate_owned_handle(process).ok()?;
    std::thread::Builder::new()
        .name("odytty-conpty-waiter".to_string())
        .spawn(move || {
            let started = Instant::now();
            let handle = HANDLE(dup.as_raw_handle());
            // SAFETY: `dup` is a live owned process handle for the wait's whole
            // duration (it is moved into this closure and dropped only after).
            // `INFINITE` parks the thread at zero CPU until the child exits.
            let _ = unsafe { WaitForSingleObject(handle, INFINITE) };
            // The child has signalled exit; read its code and decide whether it
            // was an immediate startup failure worth surfacing.
            let elapsed = started.elapsed();
            let mut code: u32 = 0;
            // SAFETY: live owned process handle; the child has exited.
            if unsafe { GetExitCodeProcess(handle, &mut code) }.is_ok()
                && code != 0
                && code != STILL_ACTIVE_CODE
                && elapsed < STARTUP_FAILURE_WINDOW
            {
                let line = describe_immediate_exit(code);
                // stderr (routed to the launching console via AttachConsole) is
                // the durable channel; the pane copy is best-effort (the tab may
                // close before a frame draws).
                eprintln!("odytty:{}", line.replace(['\r', '\n'], " ").trim());
                if let Ok(mut slot) = diagnostic.lock() {
                    *slot = Some(line);
                }
            }
            pcon.close_once();
        })
        .ok()
}

/// Output-pipe reader that maps a closed ConPTY/child (surfaced as
/// `ERROR_BROKEN_PIPE` → [`io::ErrorKind::BrokenPipe`]) to a clean EOF, mirroring
/// the Unix backend's `EIO`→EOF mapping so `read_to_end` and the pump terminate
/// normally instead of erroring.
struct PtyReader {
    file: File,
}

impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
            result => result,
        }
    }
}

/// `ERROR_BROKEN_PIPE` (109): the pipe has been ended (read or write end closed).
const ERROR_BROKEN_PIPE: i32 = 109;
/// `ERROR_NO_DATA` (232): "The pipe is being closed" — returned to a writer when
/// the ConPTY/child read end has gone away.
const ERROR_NO_DATA: i32 = 232;

/// Input-pipe writer that normalizes a dead-pipe write — surfaced by Windows as
/// `ERROR_BROKEN_PIPE` (109, already `io::ErrorKind::BrokenPipe`) or the less
/// obvious `ERROR_NO_DATA` (232, "the pipe is being closed", which Rust does NOT
/// classify) — into a single canonical `io::ErrorKind::BrokenPipe` error.
///
/// This mirrors [`PtyReader`]'s `EIO`/broken-pipe → EOF mapping on the read
/// side, giving the writer honest, consistent error semantics: a write to a
/// dead ConPTY reports a canonical `BrokenPipe` rather than the platform-raw
/// `ERROR_NO_DATA` ("the pipe is being closed") that Rust would otherwise leave
/// unclassified. Session teardown on child exit is owned by the child-waiter
/// thread (it closes the pseudoconsole, the reader hits EOF, and the single
/// `ShellExited` fires) — not by this wrapper; the normalization simply ensures
/// any code that inspects a write error sees the correct `ErrorKind` instead of
/// a confusing OS-specific one.
struct PtyWriter {
    file: File,
}

impl PtyWriter {
    /// Map a raw Windows pipe-closed error to a canonical `BrokenPipe`, leaving
    /// every other error untouched.
    fn normalize(error: io::Error) -> io::Error {
        if error.kind() == io::ErrorKind::BrokenPipe {
            return error;
        }
        match error.raw_os_error() {
            Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA) => {
                io::Error::new(io::ErrorKind::BrokenPipe, "ConPTY input pipe closed")
            }
            _ => error,
        }
    }
}

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf).map_err(Self::normalize)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush().map_err(Self::normalize)
    }
}

/// RAII guard that closes a pseudoconsole on drop, so an early return between
/// `CreatePseudoConsole` and `Self` construction cannot leak the `HPCON` (which
/// has no `Drop`) plus its pipe buffers. On the success path the spawn forgets
/// this guard and hands ownership to `Drop for PtySession`, the single closer.
struct HpconGuard(HPCON);

impl Drop for HpconGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a successful `CreatePseudoConsole` and is
        // closed exactly once — only reached when the spawn errors out before
        // ownership transfers to `PtySession` (which `forget`s this guard).
        unsafe {
            ClosePseudoConsole(self.0);
        }
    }
}

/// RAII guard that deletes a proc-thread attribute list on drop, so an early
/// return between initialization and `CreateProcessW` cannot leak it.
struct AttrListGuard(LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for AttrListGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` was produced by a successful
        // `InitializeProcThreadAttributeList` and is deleted exactly once.
        unsafe {
            DeleteProcThreadAttributeList(self.0);
        }
    }
}

/// Which shell family the default resolved to. Selects the one-shot flag form
/// in [`one_shot_args`] (`cmd /C` vs PowerShell `-NoProfile -Command`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShellKind {
    PowerShell,
    Cmd,
}

/// The resolved default interactive shell: its executable and family.
struct ResolvedShell {
    program: OsString,
    kind: ShellKind,
}

/// Resolve the default interactive shell, preferring PowerShell so OdyTTY
/// matches Windows Terminal's command surface out of the box (so `ls`, `cat`,
/// and other modern commands work — `cmd.exe` only has `dir` and friends).
/// Precedence:
///   1. `pwsh.exe` (PowerShell 7) — found on `%PATH%`, else the well-known
///      `%ProgramFiles%\PowerShell\7\pwsh.exe`. Often absent.
///   2. `powershell.exe` (Windows PowerShell 5.1) — resolved by its fixed
///      absolute path under `%SystemRoot%\System32\WindowsPowerShell\v1.0\`,
///      which is present on every Windows install (do not trust `%PATH%` alone).
///   3. `%ComSpec%` / `cmd.exe` — last resort.
fn default_shell() -> ResolvedShell {
    if let Some(program) = resolve_pwsh() {
        return ResolvedShell {
            program,
            kind: ShellKind::PowerShell,
        };
    }
    if let Some(program) = resolve_windows_powershell() {
        return ResolvedShell {
            program,
            kind: ShellKind::PowerShell,
        };
    }
    ResolvedShell {
        program: std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe")),
        kind: ShellKind::Cmd,
    }
}

/// The one-shot argument vector for running `command` in `kind` and exiting.
/// cmd uses `/C <command>`; PowerShell uses `-NoProfile -Command <command>`
/// (`-NoProfile` skips user profile scripts for a deterministic, faster start;
/// `-Command` runs the string and exits). PowerShell's own parser handles the
/// trailing command string; OdyTTY's only Windows caller passes a controlled
/// command (the `--dump-command` default), so `-Command` is sufficient and
/// `-EncodedCommand` (base64 UTF-16LE, for hostile quoting) is not needed here.
fn one_shot_args(kind: ShellKind, command: &str) -> Vec<OsString> {
    match kind {
        ShellKind::Cmd => vec![OsString::from("/C"), OsString::from(command)],
        ShellKind::PowerShell => vec![
            OsString::from("-NoProfile"),
            OsString::from("-Command"),
            OsString::from(command),
        ],
    }
}

/// Locate `pwsh.exe` (PowerShell 7): `%PATH%` first, then the default install
/// location under `%ProgramFiles%`. Returns `None` when PowerShell 7 is absent.
fn resolve_pwsh() -> Option<OsString> {
    if let Some(found) = find_on_path("pwsh.exe") {
        return Some(found);
    }
    let program_files = std::env::var_os("ProgramFiles")?;
    let candidate = Path::new(&program_files)
        .join("PowerShell")
        .join("7")
        .join("pwsh.exe");
    candidate.is_file().then(|| candidate.into_os_string())
}

/// Locate Windows PowerShell 5.1 at its fixed System32 path (present on every
/// Windows install). Resolved by absolute path rather than `%PATH%` lookup so a
/// stripped `PATH` cannot hide it.
fn resolve_windows_powershell() -> Option<OsString> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
    let candidate = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    candidate.is_file().then(|| candidate.into_os_string())
}

/// Search `%PATH%` for `exe`, returning its full path if a file by that name
/// exists in one of the entries.
fn find_on_path(exe: &str) -> Option<OsString> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|candidate| candidate.is_file())
        .map(PathBuf::into_os_string)
}

/// Format a diagnostic line for an abnormal *immediate* child exit — a shell
/// that `CreateProcessW` created successfully but that then died during its own
/// startup (loader/DLL init), leaving an otherwise-blank pane. It surfaces ANY
/// such early failure as text; the `match` only decodes the few NT status codes
/// that name a concrete cause, falling back to the bare exit code otherwise.
/// CRLF-framed so it renders cleanly when written straight into the terminal
/// model.
fn describe_immediate_exit(code: u32) -> String {
    let detail = match code {
        0xC0000142 => " — STATUS_DLL_INIT_FAILED, a DLL failed to initialize",
        0xC0000135 => " — STATUS_DLL_NOT_FOUND, a required DLL is missing",
        0xC0000139 => " — STATUS_ENTRYPOINT_NOT_FOUND",
        _ => "",
    };
    format!(
        "\r\n  OdyTTY: the shell exited immediately at startup \
         (exit code 0x{code:08X}{detail}).\r\n  \
         The pseudoconsole could not start a usable shell.\r\n"
    )
}

/// Clamp a dimension into the signed 16-bit field `COORD` requires.
fn clamp_i16(value: usize) -> i16 {
    value.min(i16::MAX as usize) as i16
}

fn coord(dimensions: Dimensions) -> COORD {
    COORD {
        X: clamp_i16(dimensions.columns),
        Y: clamp_i16(dimensions.rows),
    }
}

fn handle_of(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

/// Create an anonymous pipe, returning `(read, write)` as owned handles so any
/// early return frees them.
///
/// SAFETY: caller is in an `unsafe` context; `CreatePipe` writes two valid
/// handles on success which are immediately wrapped for ownership.
unsafe fn create_pipe() -> Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe {
        CreatePipe(&mut read, &mut write, None, 0)?;
        Ok((
            OwnedHandle::from_raw_handle(read.0 as RawHandle),
            OwnedHandle::from_raw_handle(write.0 as RawHandle),
        ))
    }
}

/// Duplicate a pipe handle into an independent `File` (the Windows analogue of
/// the Unix `master.try_clone()`), so reader/writer handoff produces handles
/// that the pump owns and closes independently of the session.
fn duplicate(handle: &OwnedHandle) -> Result<File> {
    // SAFETY: `GetCurrentProcess` is a pseudo-handle; `handle` is live for the
    // duration of the call; the duplicated handle is wrapped for ownership.
    unsafe {
        let current = GetCurrentProcess();
        let mut target = HANDLE::default();
        DuplicateHandle(
            current,
            handle_of(handle),
            current,
            &mut target,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
        .context("DuplicateHandle")?;
        Ok(File::from(OwnedHandle::from_raw_handle(
            target.0 as RawHandle,
        )))
    }
}

/// Duplicate a handle into an independent [`OwnedHandle`] (used for the process
/// handle handed to the child-waiter thread, so the waiter's blocking wait and
/// the session's own `try_wait`/`wait`/`Drop` never race on a single handle's
/// close). Unlike [`duplicate`], this keeps it as a handle rather than wrapping
/// it in a `File`, which is the correct shape for a process handle.
fn duplicate_owned_handle(handle: &OwnedHandle) -> Result<OwnedHandle> {
    // SAFETY: `GetCurrentProcess` is a pseudo-handle; `handle` is live for the
    // duration of the call; the duplicated handle is wrapped for ownership.
    unsafe {
        let current = GetCurrentProcess();
        let mut target = HANDLE::default();
        DuplicateHandle(
            current,
            handle_of(handle),
            current,
            &mut target,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
        .context("DuplicateHandle (process)")?;
        Ok(OwnedHandle::from_raw_handle(target.0 as RawHandle))
    }
}

/// Encode an `OsStr` as a NUL-terminated UTF-16 buffer for the wide Win32 APIs.
fn to_wide_nul(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

/// Build the `CreateProcessW` command line from a [`CommandBuilder`]: the
/// program followed by each argument, each quoted per the MSVC
/// `CommandLineToArgvW` rules, space-separated, NUL-terminated. The program is
/// included as `argv[0]` because many Windows programs inspect the raw command
/// line.
fn build_command_line(command: &CommandBuilder) -> Vec<u16> {
    let mut line: Vec<u16> = Vec::new();
    append_arg(&mut line, &command.program);
    for arg in &command.args {
        line.push(b' ' as u16);
        append_arg(&mut line, arg);
    }
    line.push(0);
    line
}

/// Append a single argument to a command line, quoting per the standard MSVC
/// `CommandLineToArgvW` backslash/quote escaping rules. The argument is always
/// surrounded by double quotes so embedded spaces and tabs are preserved.
fn append_arg(line: &mut Vec<u16>, arg: &OsStr) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    line.push(QUOTE);
    let mut backslashes: usize = 0;
    for unit in arg.encode_wide() {
        if unit == BACKSLASH {
            backslashes += 1;
        } else {
            if unit == QUOTE {
                // Each backslash run preceding a literal quote must be doubled,
                // plus one extra backslash to escape the quote itself.
                line.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(unit);
    }
    // A trailing backslash run before the closing quote must be doubled so it is
    // not read as escaping that quote.
    line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
    line.push(QUOTE);
}

/// Build a per-child UTF-16 environment block: this process's full environment
/// with `overrides` applied on top, terminated by the trailing NUL
/// `CREATE_UNICODE_ENVIRONMENT` requires.
///
/// The base is taken from [`current_process_env`] (`GetEnvironmentStringsW`),
/// NOT `std::env::vars_os`, because the former preserves the loader-critical
/// hidden `=`-prefixed per-drive working-directory variables (`=C:=…`) and the
/// `=::=::\` marker that the latter silently drops — their absence makes a child
/// fail DLL initialization with `0xC0000142` (STATUS_DLL_INIT_FAILED). Any
/// existing variable whose name matches an override (case-insensitively, as
/// Windows env names are) is dropped so the override replaces it rather than
/// duplicating it; the hidden `=`-prefixed vars never match a normal override
/// name and are carried through verbatim.
///
/// Building a per-child block — rather than mutating this process's own
/// environment and inheriting — keeps every override scoped to the child it
/// targets, eliminating the global-mutation cross-session leak/race that would
/// appear the moment per-session env (per-profile TERM, SSH-in-a-tab) exists.
fn build_env_block(overrides: &[(OsString, OsString)]) -> Vec<u16> {
    let override_keys: Vec<Vec<u16>> = overrides
        .iter()
        .map(|(key, _)| key.encode_wide().collect())
        .collect();

    let mut block: Vec<u16> = Vec::new();
    for entry in current_process_env() {
        let key = env_entry_key(&entry);
        if override_keys
            .iter()
            .any(|candidate| utf16_eq_ignore_ascii_case(candidate, key))
        {
            continue;
        }
        block.extend_from_slice(&entry);
        block.push(0);
    }
    for (key, value) in overrides {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    // Terminating NUL for the block. OdyTTY always sets the four terminal vars,
    // so `block` is never empty and this yields the required `…\0\0` tail.
    block.push(0);
    block
}

/// Snapshot this process's environment as raw `KEY=VALUE` UTF-16 entries via
/// `GetEnvironmentStringsW`, including the hidden `=`-prefixed drive variables
/// that `std::env::vars_os` discards. The block is freed before returning.
fn current_process_env() -> Vec<Vec<u16>> {
    let mut entries = Vec::new();
    // SAFETY: `GetEnvironmentStringsW` returns a pointer to a double-NUL-
    // terminated block of NUL-terminated UTF-16 strings; we read it within
    // bounds (stopping at the empty string that marks the end) and free it via
    // `FreeEnvironmentStringsW` before returning.
    unsafe {
        let block = GetEnvironmentStringsW();
        if block.is_null() {
            return entries;
        }
        let mut cursor: *const u16 = block.0 as *const u16;
        loop {
            let mut len = 0usize;
            while *cursor.add(len) != 0 {
                len += 1;
            }
            if len == 0 {
                // Empty string → the terminating double NUL was reached.
                break;
            }
            entries.push(std::slice::from_raw_parts(cursor, len).to_vec());
            cursor = cursor.add(len + 1);
        }
        let _ = FreeEnvironmentStringsW(PCWSTR(block.0 as *const u16));
    }
    entries
}

/// The key portion of a raw `KEY=VALUE` environment entry. Windows exposes
/// hidden per-drive working-directory variables whose names START with `=`
/// (e.g. `=C:=C:\dir`), so the split is the first `=` at index ≥ 1, not 0.
fn env_entry_key(entry: &[u16]) -> &[u16] {
    const EQ: u16 = b'=' as u16;
    let start = usize::from(entry.first() == Some(&EQ));
    match entry[start..].iter().position(|&unit| unit == EQ) {
        Some(pos) => &entry[..start + pos],
        None => entry,
    }
}

/// ASCII-case-insensitive equality of two UTF-16 slices, for matching Windows
/// environment-variable names (which are case-insensitive).
fn utf16_eq_ignore_ascii_case(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(&x, &y)| ascii_lower_u16(x) == ascii_lower_u16(y))
}

/// Lowercase a UTF-16 code unit if it is an ASCII `A`–`Z`, else return it
/// unchanged (non-ASCII units never need folding for env-name matching).
fn ascii_lower_u16(unit: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&unit) {
        unit + 32
    } else {
        unit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(units: &[u16]) -> String {
        String::from_utf16_lossy(units)
    }

    #[test]
    fn clamp_i16_caps_at_signed_max() {
        assert_eq!(clamp_i16(0), 0);
        assert_eq!(clamp_i16(80), 80);
        assert_eq!(clamp_i16(usize::MAX), i16::MAX);
    }

    #[test]
    fn resize_after_pcon_close_is_a_clean_noop() {
        // P2-FIX regression: a resize arriving after the pseudoconsole has
        // been closed (the child-waiter does this asynchronously when the
        // shell self-exits) must be a clean Ok(()) no-op that never reaches
        // `ResizePseudoConsole` — pre-fix it called the kernel on a freed
        // HPCON (use-after-free inside conhost).
        let session = PtySession::spawn_default_shell(Dimensions {
            rows: 24,
            columns: 80,
        })
        .expect("spawn default shell");

        // A resize on the live handle reaches the kernel.
        session
            .resize(Dimensions {
                rows: 30,
                columns: 100,
            })
            .expect("live resize");
        assert_eq!(session.pcon.kernel_resizes.load(Ordering::Relaxed), 1);

        // Simulate the waiter's close (same code path: `PconShared::close_once`),
        // then resize again: Ok, and the kernel call count must NOT move.
        session.pcon.close_once();
        session
            .resize(Dimensions {
                rows: 24,
                columns: 80,
            })
            .expect("post-close resize must be a clean no-op");
        assert_eq!(
            session.pcon.kernel_resizes.load(Ordering::Relaxed),
            1,
            "post-close resize must not reach ResizePseudoConsole"
        );

        // Idempotence: a second close (kill/Drop will issue more) is a no-op.
        session.pcon.close_once();
    }

    #[test]
    fn resize_racing_child_self_exit_never_faults() {
        // P2-FIX race pressure: run a shell that exits immediately, so the
        // child-waiter thread closes the pseudoconsole asynchronously while
        // this thread hammers `resize`. Pre-fix this could order a
        // `ResizePseudoConsole` against a concurrently-freed HPCON (UAF in
        // conhost — intermittent crash); post-fix every resize is either
        // ordered before the close (valid handle) or after it (clean no-op),
        // so ALL calls must return Ok and the process must survive.
        let session = PtySession::spawn_shell_command(
            Dimensions {
                rows: 24,
                columns: 80,
            },
            "exit",
        )
        .expect("spawn one-shot shell");

        let deadline = Instant::now() + Duration::from_millis(750);
        let mut flip = false;
        while Instant::now() < deadline {
            flip = !flip;
            let dims = if flip {
                Dimensions {
                    rows: 30,
                    columns: 100,
                }
            } else {
                Dimensions {
                    rows: 24,
                    columns: 80,
                }
            };
            session
                .resize(dims)
                .expect("resize must never fail across the waiter's close");
            // Stop early once the waiter has closed the pcon and the no-op
            // path is proven (a few extra iterations keep pressure on it).
            if *session.pcon.lock_closed() {
                break;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn shell_repaints_absolutely_on_resize() {
        // ConPTY/conhost reflows its own buffer and re-emits an absolute CUP on
        // every ResizePseudoConsole, so the backend must report that the shell
        // owns cursor placement on resize — this is what makes the terminal
        // defer cursor translation to the shell on Windows.
        let session = PtySession::spawn_default_shell(Dimensions {
            rows: 24,
            columns: 80,
        })
        .expect("spawn default shell");
        assert!(
            session.shell_repaints_on_resize(),
            "ConPTY backend must report absolute resize repaint"
        );
    }

    #[test]
    fn default_shell_resolves_to_a_non_empty_program() {
        // On any normal Windows host the chain resolves to PowerShell (pwsh or
        // Windows PowerShell 5.1) or, failing both, cmd.exe — never empty.
        let shell = default_shell();
        assert!(!shell.program.is_empty());
    }

    #[test]
    fn one_shot_args_select_per_shell_flag_form() {
        // cmd.exe one-shot: `/C <command>`.
        assert_eq!(
            one_shot_args(ShellKind::Cmd, "echo hi"),
            vec![OsString::from("/C"), OsString::from("echo hi")],
        );
        // PowerShell one-shot: `-NoProfile -Command <command>` (NOT `/C`).
        assert_eq!(
            one_shot_args(ShellKind::PowerShell, "Get-ChildItem"),
            vec![
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("Get-ChildItem"),
            ],
        );
    }

    #[test]
    fn find_on_path_locates_a_known_system_binary() {
        // cmd.exe is on %PATH% (System32) on every Windows host, so the PATH
        // search must find it; a nonexistent name must return None.
        assert!(find_on_path("cmd.exe").is_some());
        assert!(find_on_path("definitely-not-a-real-binary-xyz.exe").is_none());
    }

    #[test]
    fn windows_powershell_5_1_resolves_by_absolute_path() {
        // Windows PowerShell 5.1 ships on every Windows install at its fixed
        // System32 location, so the absolute-path resolver must find it.
        assert!(resolve_windows_powershell().is_some());
    }

    #[test]
    fn append_arg_quotes_and_escapes() {
        let mut out = Vec::new();
        append_arg(&mut out, OsStr::new("simple"));
        assert_eq!(decode(&out), "\"simple\"");

        let mut spaced = Vec::new();
        append_arg(&mut spaced, OsStr::new("a b"));
        assert_eq!(decode(&spaced), "\"a b\"");

        // An embedded quote is escaped with a backslash.
        let mut quoted = Vec::new();
        append_arg(&mut quoted, OsStr::new("a\"b"));
        assert_eq!(decode(&quoted), "\"a\\\"b\"");

        // A trailing backslash run is doubled before the closing quote.
        let mut trailing = Vec::new();
        append_arg(&mut trailing, OsStr::new("path\\"));
        assert_eq!(decode(&trailing), "\"path\\\\\"");
    }

    #[test]
    fn build_command_line_joins_program_and_args() {
        let mut command = CommandBuilder::new("cmd.exe");
        command.arg("/C");
        command.arg("echo hi");
        let line = build_command_line(&command);
        // NUL-terminated; strip it for the comparison.
        assert_eq!(line.last(), Some(&0));
        let text = decode(&line[..line.len() - 1]);
        assert_eq!(text, "\"cmd.exe\" \"/C\" \"echo hi\"");
    }

    #[test]
    fn pty_writer_normalize_maps_pipe_closed_codes_to_broken_pipe() {
        // ERROR_BROKEN_PIPE (109): Rust already classifies this as `BrokenPipe`,
        // so it must pass through still classified as `BrokenPipe`.
        let broken = PtyWriter::normalize(io::Error::from_raw_os_error(ERROR_BROKEN_PIPE));
        assert_eq!(broken.kind(), io::ErrorKind::BrokenPipe);

        // ERROR_NO_DATA (232), "the pipe is being closed": Rust does NOT classify
        // it, so `normalize` is what must remap it to a canonical `BrokenPipe`.
        let no_data = PtyWriter::normalize(io::Error::from_raw_os_error(ERROR_NO_DATA));
        assert_eq!(no_data.kind(), io::ErrorKind::BrokenPipe);

        // An unrelated OS error is left untouched (kind and raw code preserved).
        let other = PtyWriter::normalize(io::Error::from_raw_os_error(2));
        assert_eq!(other.raw_os_error(), Some(2));
        assert_ne!(other.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn describe_immediate_exit_decodes_known_codes_and_falls_back() {
        let dll_init = describe_immediate_exit(0xC0000142);
        assert!(dll_init.contains("0xC0000142"));
        assert!(dll_init.contains("STATUS_DLL_INIT_FAILED"));

        let dll_missing = describe_immediate_exit(0xC0000135);
        assert!(dll_missing.contains("STATUS_DLL_NOT_FOUND"));

        let entrypoint = describe_immediate_exit(0xC0000139);
        assert!(entrypoint.contains("STATUS_ENTRYPOINT_NOT_FOUND"));

        // An unknown code is still surfaced, just without a decoded NT name.
        let unknown = describe_immediate_exit(0x0000_0001);
        assert!(unknown.contains("0x00000001"));
        assert!(!unknown.contains("STATUS_"));
    }

    #[test]
    fn env_entry_key_splits_on_first_equals_after_index_zero() {
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
        // Normal `KEY=VALUE`.
        assert_eq!(
            env_entry_key(&wide("PATH=C:\\bin")),
            wide("PATH").as_slice()
        );
        // Hidden per-drive var: the key is `=C:`, found at the SECOND `=`.
        assert_eq!(env_entry_key(&wide("=C:=C:\\dir")), wide("=C:").as_slice());
        // No `=` at all: the whole entry is the key.
        assert_eq!(env_entry_key(&wide("BARE")), wide("BARE").as_slice());
    }

    #[test]
    fn utf16_eq_ignore_ascii_case_matches_env_names() {
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
        assert!(utf16_eq_ignore_ascii_case(&wide("Path"), &wide("PATH")));
        assert!(utf16_eq_ignore_ascii_case(&wide("term"), &wide("TERM")));
        // Different lengths and different names must not match.
        assert!(!utf16_eq_ignore_ascii_case(
            &wide("TERM"),
            &wide("TERMINAL")
        ));
        assert!(!utf16_eq_ignore_ascii_case(&wide("FOO"), &wide("BAR")));
    }

    #[test]
    fn build_env_block_replaces_override_and_is_nul_terminated() {
        // The override must REPLACE any inherited `TERM`, appearing exactly once,
        // and the block must end with the terminating NUL.
        let overrides = vec![(OsString::from("TERM"), OsString::from("xterm-256color"))];
        let block = build_env_block(&overrides);
        assert_eq!(block.last(), Some(&0));

        let mut entries: Vec<String> = Vec::new();
        let mut current: Vec<u16> = Vec::new();
        for &unit in &block {
            if unit == 0 {
                if current.is_empty() {
                    break; // terminating double NUL
                }
                entries.push(String::from_utf16_lossy(&current));
                current.clear();
            } else {
                current.push(unit);
            }
        }
        let term_entries: Vec<&String> = entries
            .iter()
            .filter(|entry| entry.to_ascii_uppercase().starts_with("TERM="))
            .collect();
        assert_eq!(
            term_entries.len(),
            1,
            "TERM override must appear exactly once"
        );
        assert!(term_entries[0].eq_ignore_ascii_case("TERM=xterm-256color"));
    }
}
