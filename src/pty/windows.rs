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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};

use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows::Win32::System::Environment::SetEnvironmentVariableW;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
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
    /// Raw `HPCON` value. Stored as `isize` (its newtype payload) so the whole
    /// struct is trivially `Send`/`Sync`; rewrapped as `HPCON` at call sites.
    hpcon: isize,
    /// Whether [`ClosePseudoConsole`] has already run for `hpcon`. Shared with
    /// the child-waiter thread (and read by `kill`/`Drop`) so the pseudoconsole
    /// is closed exactly once no matter which path reaches it first — natural
    /// child exit (waiter), explicit `kill`, or `Drop`. See [`close_pcon_once`].
    hpcon_closed: Arc<AtomicBool>,
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
}

impl PtySession {
    pub fn spawn_default_shell(dimensions: Dimensions) -> Result<Self> {
        Self::spawn_default_shell_in(dimensions, None)
    }

    pub fn spawn_default_shell_in(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
    ) -> Result<Self> {
        let mut command = CommandBuilder::new(default_shell().program);
        command.apply_terminal_env();
        if let Some(path) = working_directory {
            command.current_dir(path);
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
            //    Environment: we pass `lpEnvironment = NULL` (no
            //    `CREATE_UNICODE_ENVIRONMENT`) so the child INHERITS this
            //    process's environment block, exactly like Microsoft's canonical
            //    ConPTY sample (EchoCon) and Windows Terminal. A hand-built block
            //    via Rust's `std::env::vars_os()` silently drops the loader-
            //    critical hidden drive variables (the `=C:` per-drive cwd vars and
            //    the leading `=::=::\` marker — their keys are empty / contain
            //    `=`, so `vars_os()` filters them out), and a child whose env is
            //    missing those fails DLL initialization with `0xC0000142`
            //    (STATUS_DLL_INIT_FAILED) — the exact on-device symptom. Letting
            //    Windows hand the child our inherited block avoids the whole class.
            //
            //    The only overrides OdyTTY sets are the constant terminal-
            //    identification vars (TERM/COLORTERM/TERM_PROGRAM[_VERSION]); we
            //    publish them onto THIS process before the spawn so the inherited
            //    block carries them. They are process-lifetime constants, so this
            //    is idempotent and safe (no per-session env exists in the tree).
            apply_env_overrides_to_self(&command.env);
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
            //    handle inheritance, so `bInheritHandles` is false. `lpEnvironment`
            //    is NULL → inherit this process's environment (see note above).
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
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
            // handle and closes the pseudoconsole when the child exits on its
            // own, so a self-exiting shell tears its session down via the same
            // reader-EOF path a `kill` uses. A dup failure is non-fatal — the
            // session still works; only natural-exit auto-close is lost — so the
            // waiter is `None` in that case.
            let hpcon_closed = Arc::new(AtomicBool::new(false));
            let waiter = spawn_child_waiter(&process, hpcon.0, Arc::clone(&hpcon_closed));

            Ok(Self {
                hpcon: hpcon.0,
                hpcon_closed,
                process,
                input_write,
                output_read,
                waiter,
            })
        }
    }

    pub fn resize(&self, dimensions: Dimensions) -> Result<()> {
        // SAFETY: `self.hpcon` is a live pseudoconsole owned by this session.
        unsafe { ResizePseudoConsole(HPCON(self.hpcon), coord(dimensions)).context("resize pty") }
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
    /// and `Drop` — via the shared `hpcon_closed` flag (see [`close_pcon_once`]).
    fn close_pcon(&mut self) {
        close_pcon_once(self.hpcon, &self.hpcon_closed);
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>> {
        let mut reader = self.try_clone_reader()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).context("read pty output")?;
        Ok(bytes)
    }

    /// Bring-up diagnostic: detect a shell child that died during its own
    /// initialization (e.g. a missing DLL, or a `STATUS_DLL_INIT_FAILED`
    /// `0xC0000142` console/ConPTY conflict the child inherits).
    ///
    /// Such a child makes `CreateProcessW` *succeed* — the process is created —
    /// and then exit a moment later during loader/init, so the spawn returns
    /// `Ok` yet the pane stays blank with no error. This waits up to `timeout`
    /// for the child and returns a human-readable line **iff** it has already
    /// exited with an abnormal (non-zero, non-`STILL_ACTIVE`) code, so the
    /// failure can be surfaced as text instead of a silent empty session.
    ///
    /// Returns `None` for a still-running child (the healthy path) or a clean
    /// exit, so a working shell pays at most `timeout` once at startup. It does
    /// **not** diagnose a pseudoconsole whose helper process is wedged without
    /// the child exiting (the reader simply blocks); that case is addressed by
    /// releasing the host's own console before ConPTY creation.
    pub fn diagnose_immediate_exit(&self, timeout: std::time::Duration) -> Option<String> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `self.process` is a live, owned process handle.
        let wait = unsafe { WaitForSingleObject(self.process_handle(), ms) };
        if wait != WAIT_OBJECT_0 {
            // Timed out (child still running → healthy) or the wait failed.
            return None;
        }
        let mut code: u32 = 0;
        // SAFETY: live owned process handle; the child has signaled exit.
        if unsafe { GetExitCodeProcess(self.process_handle(), &mut code) }.is_err() {
            return None;
        }
        if code == 0 || code == STILL_ACTIVE_CODE {
            // A clean or `cmd /C`-style fast exit is not a failure to report.
            return None;
        }
        Some(describe_immediate_exit(code))
    }

    fn process_handle(&self) -> HANDLE {
        HANDLE(self.process.as_raw_handle())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // `kill` terminates the child if it is still running and closes the
        // pseudoconsole (idempotent via `hpcon_closed`, so a prior `close()` →
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

/// Close the pseudoconsole exactly once across every closer (the child-waiter
/// thread, `kill`, and `Drop`), guarded by the shared `closed` flag. Closing it
/// releases ConPTY's internal copy of the output pipe's write end, so any
/// blocked output reader observes EOF and the session tears down cleanly.
fn close_pcon_once(hpcon: isize, closed: &AtomicBool) {
    // `swap` returns the prior value: only the first caller (prior == false)
    // performs the close; the rest are no-ops, preserving single-close.
    if !closed.swap(true, Ordering::AcqRel) {
        // SAFETY: `hpcon` came from a successful `CreatePseudoConsole` and is
        // closed exactly once thanks to the atomic guard above.
        unsafe {
            ClosePseudoConsole(HPCON(hpcon));
        }
    }
}

/// Spawn the child-waiter thread for a freshly created session.
///
/// It owns a *duplicated* process handle (so it never races `try_wait`/`wait`/
/// `Drop` on the session's own handle) and performs a single, blocking,
/// zero-CPU `WaitForSingleObject(.., INFINITE)` — NOT a poll/sleep loop — that
/// wakes exactly once when the child exits. On wake it closes the pseudoconsole
/// (idempotent via the shared flag), which makes the pump's blocked output
/// reader observe EOF; the app then tears the session down through its single
/// existing `ShellExited` path. The thread then exits and its duplicated handle
/// closes. Returns `None` if the handle could not be duplicated (non-fatal: the
/// session still works, only natural-exit auto-close is lost).
fn spawn_child_waiter(
    process: &OwnedHandle,
    hpcon: isize,
    closed: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    let dup = duplicate_owned_handle(process).ok()?;
    std::thread::Builder::new()
        .name("odytty-conpty-waiter".to_string())
        .spawn(move || {
            // SAFETY: `dup` is a live owned process handle for the wait's whole
            // duration (it is moved into this closure and dropped only after).
            // `INFINITE` parks the thread at zero CPU until the child exits.
            let _ = unsafe { WaitForSingleObject(HANDLE(dup.as_raw_handle()), INFINITE) };
            close_pcon_once(hpcon, &closed);
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

/// Format a terminal-pane diagnostic line for an abnormal immediate child exit,
/// decoding the few NT status codes that point at a concrete cause. CRLF-framed
/// so it renders cleanly when written straight into the terminal model.
fn describe_immediate_exit(code: u32) -> String {
    let detail = match code {
        0xC0000142 => " — STATUS_DLL_INIT_FAILED, a console/ConPTY initialization conflict",
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

/// Publish the builder's environment overrides onto THIS process via
/// `SetEnvironmentVariableW`, so a child spawned with `lpEnvironment = NULL`
/// inherits them on top of the full (loader-correct) Windows environment block.
///
/// This replaces the previous hand-built `CREATE_UNICODE_ENVIRONMENT` block,
/// which dropped the hidden `=`-prefixed drive variables and produced
/// `0xC0000142` (STATUS_DLL_INIT_FAILED) in the child. The only overrides
/// OdyTTY sets are the constant terminal-identification vars, so mutating our
/// own environment is idempotent and side-effect-free in practice.
fn apply_env_overrides_to_self(overrides: &[(OsString, OsString)]) {
    for (key, value) in overrides {
        let key_wide = to_wide_nul(key);
        let value_wide = to_wide_nul(value);
        // SAFETY: both buffers are NUL-terminated UTF-16; a failed set is
        // non-fatal (the child simply does not see that var), so the result is
        // intentionally ignored.
        unsafe {
            let _ = SetEnvironmentVariableW(PCWSTR(key_wide.as_ptr()), PCWSTR(value_wide.as_ptr()));
        }
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
}
