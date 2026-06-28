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
use std::path::PathBuf;
use std::process::ExitStatus;

use anyhow::{Context, Result};

use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED,
};
use windows::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
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
    /// Raw `HPCON` value. Stored as `isize` (its newtype payload) so the whole
    /// struct is trivially `Send`/`Sync`; rewrapped as `HPCON` at call sites.
    hpcon: isize,
    /// Whether [`ClosePseudoConsole`] has already run for `hpcon`. Guards the
    /// single-close invariant now that both `kill` and `Drop` may close the
    /// pseudoconsole (see [`PtySession::close_pcon`]).
    hpcon_closed: bool,
    /// The child process handle. Owned: closed when the session drops.
    process: OwnedHandle,
    /// Parent end of the input pipe (host → child). Kept as an owned handle so
    /// `take_writer` can `DuplicateHandle` an independent writer from it,
    /// mirroring the Unix backend's `master.try_clone()`.
    input_write: OwnedHandle,
    /// Parent end of the output pipe (child → host). Source for the duplicated
    /// reader handed to `try_clone_reader`.
    output_read: OwnedHandle,
}

impl PtySession {
    pub fn spawn_default_shell(dimensions: Dimensions) -> Result<Self> {
        Self::spawn_default_shell_in(dimensions, None)
    }

    pub fn spawn_default_shell_in(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
    ) -> Result<Self> {
        let mut command = CommandBuilder::new(default_shell());
        command.apply_terminal_env();
        if let Some(path) = working_directory {
            command.current_dir(path);
        }
        Self::spawn_command(dimensions, command)
    }

    pub fn spawn_shell_command(dimensions: Dimensions, command: &str) -> Result<Self> {
        // ConPTY default shell is cmd.exe; there is no POSIX `$SHELL`/`-lc`.
        // `cmd /C <command>` runs a single command line and exits.
        let mut command_builder = CommandBuilder::new(default_shell());
        command_builder.arg("/C");
        command_builder.arg(command);
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

            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some((&hpcon as *const HPCON).cast::<c_void>()),
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
            //    mutable, NUL-terminated UTF-16 buffer), environment block, cwd.
            let mut command_line = build_command_line(&command);
            let env_block = build_env_block(&command.env);
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
            // `Drop for PtySession` is the single `ClosePseudoConsole` caller.
            // Forgetting the guard prevents a double-close.
            std::mem::forget(hpcon_guard);

            Ok(Self {
                hpcon: hpcon.0,
                hpcon_closed: false,
                process,
                input_write,
                output_read,
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
        Ok(Box::new(file))
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
    /// Idempotent: both [`PtySession::kill`] and [`Drop`] may call it, guarded by
    /// `hpcon_closed` to preserve the single-close invariant.
    fn close_pcon(&mut self) {
        if !self.hpcon_closed {
            self.hpcon_closed = true;
            // SAFETY: `self.hpcon` came from a successful `CreatePseudoConsole`
            // and is closed exactly once (guarded by `hpcon_closed`).
            unsafe {
                ClosePseudoConsole(HPCON(self.hpcon));
            }
        }
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>> {
        let mut reader = self.try_clone_reader()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).context("read pty output")?;
        Ok(bytes)
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
    }
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

/// `%ComSpec%` (the OS-configured command processor) or `cmd.exe` as a fallback.
fn default_shell() -> OsString {
    std::env::var_os("ComSpec").unwrap_or_else(|| OsString::from("cmd.exe"))
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

/// Build a `CREATE_UNICODE_ENVIRONMENT` block: the current process environment
/// merged with the builder's overrides, keys deduplicated case-insensitively
/// (Windows env semantics), sorted case-insensitively, encoded as
/// `KEY=VALUE\0…\0\0`.
fn build_env_block(overrides: &[(OsString, OsString)]) -> Vec<u16> {
    // (upper-cased key, original key, value); insertion order tracked for
    // case-insensitive replacement.
    let mut entries: Vec<(Vec<u16>, Vec<u16>, Vec<u16>)> = Vec::new();

    let mut upsert = |key: &OsStr, value: &OsStr| {
        let key_wide: Vec<u16> = key.encode_wide().collect();
        let key_upper: Vec<u16> = key_wide.iter().map(|&u| ascii_upper(u)).collect();
        let value_wide: Vec<u16> = value.encode_wide().collect();
        if let Some(slot) = entries.iter_mut().find(|(upper, ..)| *upper == key_upper) {
            slot.2 = value_wide;
        } else {
            entries.push((key_upper, key_wide, value_wide));
        }
    };

    for (key, value) in std::env::vars_os() {
        upsert(&key, &value);
    }
    for (key, value) in overrides {
        upsert(key, value);
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut block: Vec<u16> = Vec::new();
    for (_, key, value) in &entries {
        // Skip malformed entries with an empty key (an `=VALUE` line would be
        // rejected by the loader).
        if key.is_empty() {
            continue;
        }
        block.extend_from_slice(key);
        block.push(b'=' as u16);
        block.extend_from_slice(value);
        block.push(0);
    }
    block.push(0);
    block
}

/// ASCII-uppercase a single UTF-16 code unit, leaving non-ASCII units intact.
/// Sufficient for Windows environment-key case folding, which is ASCII-based.
fn ascii_upper(unit: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&unit) {
        unit - 32
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
    fn default_shell_is_non_empty() {
        // %ComSpec% is set in normal Windows environments; the fallback is
        // cmd.exe. Either way it is non-empty.
        assert!(!default_shell().is_empty());
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
    fn env_block_is_double_nul_terminated_and_sorted() {
        let overrides = vec![
            (OsString::from("ZZZ_ODYTTY_TEST"), OsString::from("1")),
            (OsString::from("AAA_ODYTTY_TEST"), OsString::from("2")),
        ];
        let block = build_env_block(&overrides);
        // Ends in the double-NUL block terminator.
        assert_eq!(block.last(), Some(&0));
        let text = decode(&block);
        assert!(text.contains("AAA_ODYTTY_TEST=2\0"));
        assert!(text.contains("ZZZ_ODYTTY_TEST=1\0"));
        // Case-insensitive sort places AAA before ZZZ.
        let aaa = text.find("AAA_ODYTTY_TEST").expect("AAA present");
        let zzz = text.find("ZZZ_ODYTTY_TEST").expect("ZZZ present");
        assert!(aaa < zzz, "entries must be sorted by key");
    }

    #[test]
    fn env_block_override_replaces_case_insensitively() {
        // A lower-case override of an existing PATH-like key replaces rather
        // than duplicating it.
        let overrides = vec![
            (OsString::from("ODYTTY_DUP_KEY"), OsString::from("first")),
            (OsString::from("odytty_dup_key"), OsString::from("second")),
        ];
        let block = build_env_block(&overrides);
        let text = decode(&block);
        let count = text.matches("DUP_KEY").count() + text.matches("dup_key").count();
        assert_eq!(count, 1, "case-insensitive keys must collapse to one entry");
        assert!(text.contains("second"));
        assert!(!text.contains("first"));
    }
}
