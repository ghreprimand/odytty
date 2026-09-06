// SPDX-License-Identifier: GPL-3.0-only
//! POSIX PTY backend for [`crate::pty`].
//!
//! Implements [`PtySession`] over a rustix-managed master/slave pair with
//! `setsid`/`TIOCSCTTY` controlling-terminal setup and process-group
//! signalling. Selected via `#[cfg(unix)]` from the parent module, which also
//! owns the platform-neutral [`CommandBuilder`]/[`ForegroundJob`] contract this
//! backend builds on.
use std::env;
use std::ffi::{CStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use rustix::fd::AsFd;
#[cfg(target_os = "linux")]
use rustix::pipe::{PipeFlags, pipe_with};
use rustix::process::RawPid;
#[cfg(target_os = "linux")]
use rustix::pty::ioctl_tiocgptpeer;
use rustix::pty::{OpenptFlags, grantpt, openpt, unlockpt};
use rustix::termios::{Winsize, tcgetpgrp, tcsetwinsize};

use super::{CommandBuilder, ForegroundJob};
use crate::core::{CellMetrics, Dimensions};
use crate::settings::Settings;

/// Classify the terminal foreground group of `fd` against `shell_pgid`.
///
/// Pure and read-only: a single `tcgetpgrp` (TIOCGPGRP) inspection. Any error
/// — including the no-foreground-group case, which rustix reports as
/// `OPNOTSUPP` — maps to [`ForegroundJob::Unknown`] so callers never prompt on
/// an indeterminate terminal.
fn classify_foreground<Fd: AsFd>(fd: Fd, shell_pgid: RawPid) -> ForegroundJob {
    match tcgetpgrp(fd) {
        Ok(foreground) if foreground.as_raw_pid() == shell_pgid => ForegroundJob::None,
        Ok(_) => ForegroundJob::Running,
        Err(_) => ForegroundJob::Unknown,
    }
}

pub struct PtySession {
    master: File,
    child: Child,
    /// Write end of a self-pipe that force-wakes every live [`PtyReader`]. A
    /// blocking master read cannot be interrupted once a `setsid`'d grandchild
    /// (e.g. an `ssh` ControlPersist master) keeps the slave open so the master
    /// never sees EOF; writing one byte here makes the reader's `poll` return so
    /// [`Self::force_reader_eof`] can release a wedged close. See that method.
    reader_wake_write: OwnedFd,
    /// Read end of the reader-wake self-pipe. Each [`Self::try_clone_reader`]
    /// hands the reader a `dup` of this so all readers share one wake signal.
    reader_wake_read: OwnedFd,
    /// Live cell pixel metrics packed as `(width_px << 32) | height_px`, used to
    /// fill `ws_xpixel`/`ws_ypixel` on every TIOCSWINSZ so pixel-aware programs
    /// (image protocols, some TUIs) see a real geometry instead of zero. Seeded
    /// with [`CellMetrics::DEFAULT`] at spawn (the native layer has no live
    /// metric until the first layout pass) and updated via
    /// [`Self::set_cell_metrics`] on every resize. `&self` mutation → atomic.
    cell_metrics: std::sync::atomic::AtomicU64,
    /// Test-only counter of kernel `resize` calls (TIOCSWINSZ). Lets a headless
    /// test assert the divider-drag coalescing fires ONE resize at drag-end
    /// instead of one per pointer-move. Not built outside tests.
    #[cfg(test)]
    resize_calls: std::sync::atomic::AtomicUsize,
}

/// Pack cell metrics into a single `u64` for atomic storage on [`PtySession`].
fn pack_cell_metrics(metrics: CellMetrics) -> u64 {
    ((metrics.width_px as u64) << 32) | metrics.height_px as u64
}

/// Inverse of [`pack_cell_metrics`]. Re-clamps through [`CellMetrics::new`] so a
/// stored value can never widen the `[1, 1024]` invariant.
fn unpack_cell_metrics(packed: u64) -> CellMetrics {
    CellMetrics::new((packed >> 32) as u32, (packed & 0xffff_ffff) as u32)
}

/// Resolve the interactive shell without assuming a desktop session exported
/// `SHELL`. Graphical launchers are allowed to omit it; on Fedora `/bin/sh` is
/// Bash, so the old fallback looked like Bash after exec while its `sh`
/// program name made the integration classifier skip the Bash rcfile.
fn default_shell_program() -> OsString {
    // Pass the NSS producer by name (uncalled) so `getpwuid_r` runs only when a
    // non-empty `SHELL` did not already win — the common desktop case performs
    // zero passwd/NSS lookups.
    resolve_default_shell(env::var_os("SHELL"), login_shell_from_passwd)
}

/// Resolve the shell program, consulting the login-shell producer lazily.
///
/// `login_shell` is a `FnOnce` invoked ONLY when `shell_env` is absent or empty,
/// so a valid `SHELL` short-circuits before any passwd/NSS lookup. Empty values
/// (env or login shell) are treated as absent and fall through to `/bin/sh`.
fn resolve_default_shell<F>(shell_env: Option<OsString>, login_shell: F) -> OsString
where
    F: FnOnce() -> Option<OsString>,
{
    shell_env
        .filter(|shell| !shell.is_empty())
        .or_else(|| login_shell().filter(|shell| !shell.is_empty()))
        .unwrap_or_else(|| OsString::from("/bin/sh"))
}

/// Read the effective user's login shell through NSS. `getpwuid_r` covers
/// local passwd files and directory-backed accounts without a subprocess or a
/// UTF-8 assumption. Failure remains non-fatal and falls through to `/bin/sh`.
fn login_shell_from_passwd() -> Option<OsString> {
    const DEFAULT_BUFFER: usize = 16 * 1024;
    const MAX_BUFFER: usize = 1024 * 1024;

    // SAFETY: `geteuid` has no pointer preconditions or mutable shared state.
    let uid = unsafe { libc::geteuid() };
    // POSIX permits -1 when no bound is known. Clamp an advertised size so a
    // hostile NSS configuration cannot force an excessive allocation.
    // SAFETY: `sysconf` is called with a valid POSIX selector.
    let suggested = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut capacity = usize::try_from(suggested)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_BUFFER)
        .clamp(1024, MAX_BUFFER);

    loop {
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; capacity];
        // SAFETY: every output pointer is valid for the duration of the call,
        // `buffer` owns `capacity` writable bytes, and `result` receives either
        // null or the initialized `record` pointer per getpwuid_r's contract.
        let code = unsafe {
            libc::getpwuid_r(
                uid,
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE && capacity < MAX_BUFFER {
            capacity = capacity.saturating_mul(2).min(MAX_BUFFER);
            continue;
        }
        if code != 0 || result.is_null() {
            return None;
        }

        // SAFETY: a zero return with non-null `result` initializes `record`;
        // `pw_shell` points into `buffer`, which remains alive until copied.
        let record = unsafe { record.assume_init() };
        if record.pw_shell.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r returns passwd string fields as NUL-terminated
        // strings inside its caller-provided buffer.
        let shell = unsafe { CStr::from_ptr(record.pw_shell) }.to_bytes();
        return (!shell.is_empty()).then(|| OsString::from_vec(shell.to_vec()));
    }
}

impl PtySession {
    pub fn spawn_default_shell(dimensions: Dimensions) -> Result<Self> {
        Self::spawn_default_shell_in(dimensions, None)
    }

    pub fn spawn_default_shell_in(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
    ) -> Result<Self> {
        Self::spawn_default_shell_in_with_shell_integration(
            dimensions,
            working_directory,
            false,
            false,
            false,
            &std::collections::BTreeMap::new(),
        )
    }

    pub fn spawn_default_shell_in_with_settings(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
        settings: &Settings,
    ) -> Result<Self> {
        Self::spawn_default_shell_in_with_settings_env(
            dimensions,
            working_directory,
            settings,
            &std::collections::BTreeMap::new(),
        )
    }

    /// Default-shell spawn that also applies a named profile's bounded `env`
    /// overrides. A profile that customizes only environment (no shell/command)
    /// takes this path, so its overrides must land in the child exactly as the
    /// explicit-shell arm applies them: after the standard interactive-shell
    /// env, so a profile value wins over any discovery advertisement.
    pub fn spawn_default_shell_in_with_settings_env(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
        settings: &Settings,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self> {
        Self::spawn_default_shell_in_with_shell_integration(
            dimensions,
            working_directory,
            settings.shell_integration,
            settings.buttons,
            settings.shell_key_enhancement,
            env,
        )
    }

    fn spawn_default_shell_in_with_shell_integration(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
        shell_integration: bool,
        buttons: bool,
        key_enhancement: bool,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self> {
        let shell = default_shell_program();
        let mut command = CommandBuilder::new(shell);
        command.apply_terminal_env();
        // Scrub any inherited ODYTTY_SHELL_INTEGRATION so a nested odytty (one
        // launched from an already-integrated session) does not disable
        // integration in the shell it spawns; the snippet re-exports it itself.
        command.apply_shell_integration_scrub();
        command.apply_buttons_discovery_env(buttons);
        command.apply_key_enhancement_discovery_env(key_enhancement);
        if shell_integration {
            crate::shell_integration::apply_spawn_integration(&mut command);
        }
        // Profile env overrides land last so a profile value wins over the
        // discovery advertisements above, matching the explicit-shell arm in
        // `build_local_command`.
        for (key, value) in env {
            command.env(key.clone(), value.clone());
        }
        if let Some(path) = working_directory {
            command.current_dir(path);
        }
        Self::spawn_command(dimensions, command)
    }

    pub fn spawn_shell_command(dimensions: Dimensions, command: &str) -> Result<Self> {
        let shell = default_shell_program();
        let mut command_builder = CommandBuilder::new(shell);
        command_builder.arg("-lc");
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
        let (master, slave) = open_pty_pair(dimensions)?;
        let slave_fd = slave.as_raw_fd();

        let mut command = command.into_command();
        command.stdin(Stdio::from(
            slave.try_clone().context("clone pty slave for stdin")?,
        ));
        command.stdout(Stdio::from(
            slave.try_clone().context("clone pty slave for stdout")?,
        ));
        command.stderr(Stdio::from(slave));

        // The child becomes a session leader and claims the slave side as its
        // controlling terminal before exec. rustix covers the PTY plumbing, but
        // TIOCSCTTY (present on both Linux and macOS) is not exposed as a
        // focused rustix helper, so it goes through libc directly.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                if libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                Ok(())
            });
        }

        // Self-pipe for forcing a wedged reader to EOF at close. CLOEXEC so the
        // shell child never inherits either end. Created BEFORE the spawn: a
        // pipe failure (fd exhaustion) after a successful spawn would return
        // early with a live child no one waits on — a zombie until the whole
        // process exits.
        let (reader_wake_read, reader_wake_write) = make_reader_wake_pipe()?;

        let child = command.spawn().context("spawn pty command")?;

        Ok(Self {
            master,
            child,
            reader_wake_write,
            reader_wake_read,
            cell_metrics: std::sync::atomic::AtomicU64::new(pack_cell_metrics(
                CellMetrics::DEFAULT,
            )),
            #[cfg(test)]
            resize_calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Record the live cell pixel metrics so the next TIOCSWINSZ can report a
    /// real `ws_xpixel`/`ws_ypixel`. The native layer calls this alongside
    /// `TerminalModel::set_cell_metrics` on every resize/font rebuild.
    pub fn set_cell_metrics(&self, metrics: CellMetrics) {
        self.cell_metrics.store(
            pack_cell_metrics(metrics),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub fn resize(&self, dimensions: Dimensions) -> Result<()> {
        #[cfg(test)]
        self.resize_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let metrics =
            unpack_cell_metrics(self.cell_metrics.load(std::sync::atomic::Ordering::Relaxed));
        tcsetwinsize(&self.master, winsize(dimensions, metrics)).context("resize pty")
    }

    /// Test-only: how many times [`resize`](Self::resize) has been called on
    /// this session. Drives the divider-drag coalescing assertion.
    #[cfg(test)]
    pub fn resize_call_count(&self) -> usize {
        self.resize_calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether the shell on this backend authoritatively repaints with ABSOLUTE
    /// cursor positioning on every resize. False for a POSIX PTY: resize raises
    /// `SIGWINCH`, which Linux/macOS shells service with a RELATIVE repaint the
    /// terminal's `preserve_cursor_physical_line` override is built to cooperate
    /// with — so OdyTTY keeps translating the cursor on resize as today.
    pub fn shell_repaints_on_resize(&self) -> bool {
        false
    }

    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(PtyReader {
            file: self.master.try_clone().context("clone pty reader")?,
            wake: self
                .reader_wake_read
                .try_clone()
                .context("clone pty reader wake")?,
        }))
    }

    /// Force every live reader on this PTY to observe EOF, unblocking a reader
    /// parked on the master because a `setsid`'d grandchild in a foreign process
    /// group still holds the slave open (so `kill` on the child's group cannot
    /// reach it and the master never reports EOF on its own). One byte in the
    /// self-pipe is level-triggered, so the reader's `poll` stays readable and it
    /// returns `Ok(0)`. Idempotent and best-effort: the session close path calls
    /// this after a bounded join deadline so neither the reaper nor the pump
    /// thread (and their fds) can leak on the ControlPersist case.
    pub fn force_reader_eof(&self) {
        let _ = rustix::io::write(&self.reader_wake_write, &[1]);
    }

    pub fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        Ok(Box::new(
            self.master.try_clone().context("clone pty writer")?,
        ))
    }

    /// Report whether a foreground job other than the shell is running on this
    /// PTY.
    ///
    /// Read-only: this performs a single `tcgetpgrp` inspection and never reaps,
    /// waits on, or mutates the child. The shell is its own session/group leader
    /// (it called `setsid` before exec), so its pgid equals its pid. A
    /// foreground group matching that pid means the shell is idle in the
    /// foreground ([`ForegroundJob::None`]); any other group is a running job
    /// ([`ForegroundJob::Running`]); any error is [`ForegroundJob::Unknown`].
    pub fn foreground_job(&self) -> ForegroundJob {
        classify_foreground(&self.master, self.child.id() as RawPid)
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child.try_wait().context("poll child")
    }

    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().context("wait for child")
    }

    pub fn kill(&mut self) -> Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }

        let pid = self.child.id();
        if pid > i32::MAX as u32 {
            bail!("child pid {pid} is too large for pid_t");
        }

        let process_group = -(pid as libc::pid_t);
        let result = unsafe { libc::kill(process_group, libc::SIGKILL) };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                self.child.kill().context("kill child")?;
            }
        }

        Ok(())
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>> {
        let mut reader = self.try_clone_reader()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).context("read pty output")?;
        Ok(bytes)
    }

    /// Unix has no ConPTY startup-failure diagnostic (a POSIX child that fails to
    /// exec reports the error synchronously at spawn), so there is no slot to
    /// drain — always `None`. Mirrors the Windows backend's contract method so
    /// the shared pump can take the slot uniformly across platforms.
    pub fn pending_diagnostic_slot(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<Option<String>>>> {
        None
    }

    #[cfg(test)]
    pub fn dimensions_for_test(&self) -> Result<Dimensions> {
        let mut winsize = Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe {
            libc::ioctl(
                self.master.as_raw_fd(),
                libc::TIOCGWINSZ as libc::c_ulong,
                &mut winsize,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error()).context("query pty size");
        }
        Ok(Dimensions::new(
            winsize.ws_col as usize,
            winsize.ws_row as usize,
        ))
    }

    /// Test-only: query the kernel `ws_xpixel`/`ws_ypixel` on the master via
    /// TIOCGWINSZ. Drives the C23 regression asserting TIOCSWINSZ now reports a
    /// real pixel geometry instead of zero.
    #[cfg(test)]
    pub fn pixel_dimensions_for_test(&self) -> Result<(u16, u16)> {
        let mut winsize = Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe {
            libc::ioctl(
                self.master.as_raw_fd(),
                libc::TIOCGWINSZ as libc::c_ulong,
                &mut winsize,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error()).context("query pty pixel size");
        }
        Ok((winsize.ws_xpixel, winsize.ws_ypixel))
    }
}

struct PtyReader {
    file: File,
    /// Read end of the session's reader-wake self-pipe. When it becomes readable
    /// (a close forced teardown via [`PtySession::force_reader_eof`]), the reader
    /// reports EOF instead of blocking on a master a grandchild keeps open.
    wake: OwnedFd,
}

impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            // Wait for either real PTY output or a forced-teardown wake. The
            // master fd stays blocking, but we only read after `poll` reports it
            // readable, so a healthy session is byte-identical to a direct read.
            let mut fds = [
                libc::pollfd {
                    fd: self.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.file.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: both fds are live for the duration of the call; `poll` only
            // reads/writes the two `pollfd` entries in `fds`.
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if rc < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if fds[0].revents != 0 {
                // Forced teardown: report EOF so the output pump exits promptly.
                return Ok(0);
            }
            if fds[1].revents != 0 {
                return match self.file.read(buf) {
                    // A closed slave surfaces as EIO on the master; treat it as a
                    // clean EOF, exactly as the pre-poll reader did.
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
                    result => result,
                };
            }
            // Spurious wakeup with nothing ready: poll again.
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.kill();
            let _ = self.child.wait();
        }
    }
}

pub(crate) fn open_pty_pair(dimensions: Dimensions) -> Result<(File, File)> {
    // `posix_openpt` only accepts a CLOEXEC flag on Linux; elsewhere the flag is
    // set on the master fd explicitly after opening (see below).
    #[cfg(target_os = "linux")]
    let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY;

    let master = openpt(flags).context("open pty master")?;
    grantpt(&master).context("grant pty")?;
    unlockpt(&master).context("unlock pty")?;

    // Ensure the master is close-on-exec so the spawned child never inherits it.
    // On Linux this came from `OpenptFlags::CLOEXEC`; off Linux set it directly.
    #[cfg(not(target_os = "linux"))]
    rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)
        .context("set cloexec on pty master")?;

    let slave = open_pty_slave(&master, flags)?;

    // At spawn the native layer has not run a layout pass yet, so the live cell
    // metric is unknown; seed the slave winsize with the headless-default metric
    // (8×16, matching `CellMetrics::DEFAULT`). The first real resize overwrites
    // ws_xpixel/ws_ypixel with the true geometry via `PtySession::set_cell_metrics`.
    tcsetwinsize(&slave, winsize(dimensions, CellMetrics::DEFAULT))
        .context("set pty window size")?;

    let master = unsafe { File::from_raw_fd(master.into_raw_fd()) };
    let slave = unsafe { File::from_raw_fd(slave.into_raw_fd()) };
    Ok((master, slave))
}

/// Create the close-time self-pipe used to force a wedged output reader to EOF.
///
/// Both ends must be close-on-exec so a spawned shell never inherits them. Linux
/// sets that atomically through `pipe2` (`PipeFlags::CLOEXEC`); macOS and the
/// BSDs lack `pipe2`, so a plain `pipe` is created and each end is marked
/// close-on-exec with `fcntl` immediately after (the same fallback the master fd
/// uses off Linux). The wake behavior is identical on every platform.
#[cfg(target_os = "linux")]
fn make_reader_wake_pipe() -> Result<(rustix::fd::OwnedFd, rustix::fd::OwnedFd)> {
    pipe_with(PipeFlags::CLOEXEC).context("create pty reader wake pipe")
}

#[cfg(all(unix, not(target_os = "linux")))]
fn make_reader_wake_pipe() -> Result<(rustix::fd::OwnedFd, rustix::fd::OwnedFd)> {
    let (read_end, write_end) = rustix::pipe::pipe().context("create pty reader wake pipe")?;
    rustix::io::fcntl_setfd(&read_end, rustix::io::FdFlags::CLOEXEC)
        .context("set cloexec on pty reader wake pipe read end")?;
    rustix::io::fcntl_setfd(&write_end, rustix::io::FdFlags::CLOEXEC)
        .context("set cloexec on pty reader wake pipe write end")?;
    Ok((read_end, write_end))
}

/// Open the slave (user) side of a freshly created, granted, and unlocked PTY
/// master.
///
/// Linux exposes the focused `TIOCGPTPEER` ioctl, which returns the slave fd
/// directly with no race on its name. macOS and the BSDs have no such ioctl, so
/// the slave is opened by name via the POSIX `ptsname` path instead. Both paths
/// honor the same `O_RDWR | O_NOCTTY | O_CLOEXEC` semantics as the master.
#[cfg(target_os = "linux")]
fn open_pty_slave<Fd: AsFd>(master: Fd, flags: OpenptFlags) -> Result<rustix::fd::OwnedFd> {
    ioctl_tiocgptpeer(master, flags).context("open pty slave (TIOCGPTPEER)")
}

#[cfg(all(unix, not(target_os = "linux")))]
fn open_pty_slave<Fd: AsFd>(master: Fd, _flags: OpenptFlags) -> Result<rustix::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags, open};
    use rustix::pty::ptsname;

    let name = ptsname(master, Vec::new()).context("ptsname")?;
    open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .context("open pty slave (ptsname)")
}

/// Build a `Winsize` from a grid geometry and its live cell metrics.
///
/// `ws_xpixel`/`ws_ypixel` carry `columns * width_px` and `rows * height_px` so
/// pixel-aware clients (Sixel/Kitty sizing, some TUIs) read a real surface
/// extent. The product is saturated to `u16::MAX` — the winsize pixel fields are
/// `u16`, and a pathological grid can exceed that even though real windows
/// (≤ 8K) never do. Rows/cols keep the pre-existing `u16` clamp.
fn winsize(dimensions: Dimensions, cell_metrics: CellMetrics) -> Winsize {
    let cols = dimensions.columns.min(u16::MAX as usize) as u16;
    let rows = dimensions.rows.min(u16::MAX as usize) as u16;
    let ws_xpixel = (cols as u32 * cell_metrics.width_px).min(u16::MAX as u32) as u16;
    let ws_ypixel = (rows as u32 * cell_metrics.height_px).min(u16::MAX as u32) as u16;
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel,
        ws_ypixel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    const TEST_DIMENSIONS: Dimensions = Dimensions {
        rows: 24,
        columns: 80,
    };

    #[test]
    fn default_shell_resolution_uses_login_shell_when_desktop_omits_shell() {
        assert_eq!(
            resolve_default_shell(None, || Some(OsString::from("/bin/bash"))),
            OsString::from("/bin/bash")
        );
        assert_eq!(
            resolve_default_shell(Some(OsString::new()), || Some(OsString::from(
                "/usr/bin/fish"
            ))),
            OsString::from("/usr/bin/fish")
        );
    }

    #[test]
    fn default_shell_resolution_preserves_env_and_has_safe_final_fallback() {
        assert_eq!(
            resolve_default_shell(Some(OsString::from("/usr/bin/zsh")), || Some(
                OsString::from("/bin/bash")
            )),
            OsString::from("/usr/bin/zsh")
        );
        assert_eq!(
            resolve_default_shell(None, || None),
            OsString::from("/bin/sh")
        );
    }

    /// The NSS/passwd producer must stay lazy: a valid non-empty `SHELL` wins
    /// without ever invoking `login_shell_from_passwd`. This is the whole point
    /// of the lazy-resolution change — the common desktop case performs zero
    /// passwd/NSS lookups.
    #[test]
    fn valid_shell_env_performs_zero_login_shell_lookups() {
        let calls = std::cell::Cell::new(0_u32);
        let resolved = resolve_default_shell(Some(OsString::from("/usr/bin/zsh")), || {
            calls.set(calls.get() + 1);
            Some(OsString::from("/bin/bash"))
        });
        assert_eq!(resolved, OsString::from("/usr/bin/zsh"));
        assert_eq!(
            calls.get(),
            0,
            "a valid SHELL must short-circuit before any NSS lookup"
        );
    }

    /// Every path that does NOT have a usable `SHELL` consults the producer
    /// exactly once — missing env, empty env, an empty login shell (treated as
    /// absent, falling through to `/bin/sh`), and a `None` producer result. Also
    /// pins non-UTF8 preservation: a login shell with invalid-UTF8 bytes rides
    /// through as an exact `OsString`.
    #[test]
    fn shellless_paths_consult_login_shell_exactly_once() {
        // Missing SHELL: one call, login shell wins.
        let calls = std::cell::Cell::new(0_u32);
        let resolved = resolve_default_shell(None, || {
            calls.set(calls.get() + 1);
            Some(OsString::from("/bin/bash"))
        });
        assert_eq!(resolved, OsString::from("/bin/bash"));
        assert_eq!(calls.get(), 1, "missing SHELL must consult NSS once");

        // Empty SHELL: treated as absent, one call, login shell wins.
        let calls = std::cell::Cell::new(0_u32);
        let resolved = resolve_default_shell(Some(OsString::new()), || {
            calls.set(calls.get() + 1);
            Some(OsString::from("/usr/bin/fish"))
        });
        assert_eq!(resolved, OsString::from("/usr/bin/fish"));
        assert_eq!(calls.get(), 1, "empty SHELL must consult NSS once");

        // Empty login shell: one call, filtered as absent, /bin/sh fallback.
        let calls = std::cell::Cell::new(0_u32);
        let resolved = resolve_default_shell(None, || {
            calls.set(calls.get() + 1);
            Some(OsString::new())
        });
        assert_eq!(resolved, OsString::from("/bin/sh"));
        assert_eq!(
            calls.get(),
            1,
            "an empty login shell is consulted once then falls back"
        );

        // Producer yields None (lookup failure): one call, /bin/sh fallback.
        let calls = std::cell::Cell::new(0_u32);
        let resolved = resolve_default_shell(None, || {
            calls.set(calls.get() + 1);
            None
        });
        assert_eq!(resolved, OsString::from("/bin/sh"));
        assert_eq!(calls.get(), 1, "a failed lookup is consulted once");

        // Non-UTF8 login shell bytes survive verbatim as an OsString.
        let non_utf8 = OsString::from_vec(vec![b'/', b'b', b'i', b'n', 0xff, b'x']);
        let expected = non_utf8.clone();
        let resolved = resolve_default_shell(None, move || Some(non_utf8));
        assert_eq!(
            resolved, expected,
            "non-UTF8 login shell bytes must ride through unchanged"
        );
    }

    #[test]
    fn force_reader_eof_unblocks_a_reader_the_slave_keeps_open() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;

        // A child that produces no output keeps its slave open, so the master
        // never reports EOF on its own and the pump reader blocks indefinitely —
        // the shape of the CLOSE-HANG-2 leak (a `setsid`'d grandchild holding the
        // slave). `force_reader_eof` must make the reader return EOF so a bounded
        // close join can complete instead of leaking the reader thread + fds.
        let session = PtySession::spawn_shell_command(TEST_DIMENSIONS, "exec sleep 30")
            .expect("spawn quiet shell");
        let mut reader = session.try_clone_reader().expect("clone reader");

        let done = Arc::new(AtomicBool::new(false));
        let done_thread = done.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,    // EOF (forced or natural)
                    Ok(_) => continue, // drain any startup bytes
                    Err(_) => break,
                }
            }
            done_thread.store(true, Ordering::SeqCst);
        });

        // The reader drains any initial bytes then blocks: the quiet child holds
        // the slave, so no EOF arrives on its own.
        sleep(Duration::from_millis(150));
        assert!(
            !done.load(Ordering::SeqCst),
            "reader must still be blocked while the slave is held open"
        );

        // Force EOF: the reader must return promptly.
        session.force_reader_eof();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "force_reader_eof did not unblock the reader within the deadline"
            );
            sleep(Duration::from_millis(20));
        }
        handle.join().expect("reader thread joins after forced EOF");
        // Dropping the session SIGKILLs the quiet child on the way out.
    }

    /// Look up the last value set for `key` in a builder's env list (later
    /// pushes win, matching the `into_command` apply order).
    fn env_value<'a>(cmd: &'a CommandBuilder, key: &str) -> Option<&'a OsString> {
        cmd.env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// `apply_terminal_env` advertises the standard TERM/COLORTERM capabilities
    /// (unchanged) plus the TERM_PROGRAM self-identification fastfetch reads.
    /// The program literal must be exactly "odytty" and the version must track
    /// the crate version at compile time.
    #[test]
    fn apply_terminal_env_sets_identification_and_capabilities() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.apply_terminal_env();

        assert_eq!(
            env_value(&cmd, "TERM").map(|v| v.as_os_str()),
            Some(OsString::from("xterm-256color").as_os_str()),
            "TERM capability must be unchanged"
        );
        assert_eq!(
            env_value(&cmd, "COLORTERM").map(|v| v.as_os_str()),
            Some(OsString::from("truecolor").as_os_str()),
            "COLORTERM capability must be unchanged"
        );
        assert_eq!(
            env_value(&cmd, "TERM_PROGRAM").map(|v| v.as_os_str()),
            Some(OsString::from("odytty").as_os_str()),
            "TERM_PROGRAM must be exactly \"odytty\" for fastfetch startsWith match"
        );
        assert_eq!(
            env_value(&cmd, "TERM_PROGRAM_VERSION").map(|v| v.as_os_str()),
            Some(OsString::from(env!("CARGO_PKG_VERSION")).as_os_str()),
            "TERM_PROGRAM_VERSION must track CARGO_PKG_VERSION"
        );
    }

    /// The POSIX PTY backend delegates resize repaint to the app's SIGWINCH
    /// handler (a RELATIVE repaint), so it must report that the shell does NOT
    /// authoritatively repaint with absolute positioning — keeping OdyTTY's
    /// cursor-translating reflow active on Linux/macOS (byte-identical behavior).
    #[test]
    fn shell_does_not_repaint_absolutely_on_resize() {
        let session =
            PtySession::spawn_default_shell(TEST_DIMENSIONS).expect("spawn default shell");
        assert!(
            !session.shell_repaints_on_resize(),
            "unix backend must defer resize repaint to SIGWINCH, not claim absolute repaint"
        );
    }

    /// A non-terminal fd (`/dev/null`) makes `tcgetpgrp` fail with `ENOTTY`,
    /// which must classify as `Unknown` — never `Running`, never a panic.
    #[test]
    fn errored_fd_classifies_as_unknown() {
        let not_a_tty = File::open("/dev/null").expect("open /dev/null");
        assert_eq!(
            classify_foreground(&not_a_tty, 12345),
            ForegroundJob::Unknown
        );
    }

    /// A pgid that does not match the terminal's foreground group classifies as
    /// `Running`; the matching pgid classifies as `None`. Exercised against a
    /// real PTY whose foreground group is the spawned shell's own group.
    #[test]
    fn shell_only_is_no_running_job() {
        let mut session =
            PtySession::spawn_default_shell(TEST_DIMENSIONS).expect("spawn default shell");

        // The child grabs the controlling terminal (TIOCSCTTY) shortly after
        // fork; before that the foreground group is indeterminate and the query
        // reports `Unknown`. Poll briefly for the shell to settle as foreground.
        let mut settled = ForegroundJob::Unknown;
        for _ in 0..100 {
            settled = session.foreground_job();
            if settled == ForegroundJob::None {
                break;
            }
            sleep(Duration::from_millis(10));
        }
        assert_eq!(
            settled,
            ForegroundJob::None,
            "an idle shell owns its own terminal foreground group"
        );

        // The query is read-only: it must not have reaped the child.
        assert!(
            matches!(session.try_wait(), Ok(None)),
            "foreground_job must not reap or wait on the child"
        );

        let _ = session.kill();
    }

    /// A foreground group different from the shell's pgid classifies as
    /// `Running`. Verified directly against the classifier with a real PTY's
    /// foreground group and a deliberately mismatched pgid.
    #[test]
    fn mismatched_pgid_classifies_as_running() {
        let mut session =
            PtySession::spawn_default_shell(TEST_DIMENSIONS).expect("spawn default shell");

        let mut ready = false;
        for _ in 0..100 {
            if session.foreground_job() == ForegroundJob::None {
                ready = true;
                break;
            }
            sleep(Duration::from_millis(10));
        }
        assert!(ready, "shell did not claim the terminal foreground in time");

        // Pass a pgid that cannot be the shell's: the live foreground group is
        // the shell, so a mismatched expectation reads as a running job.
        assert_eq!(
            classify_foreground(&session.master, -1),
            ForegroundJob::Running
        );

        let _ = session.kill();
    }

    /// C23: the `winsize` builder fills `ws_xpixel`/`ws_ypixel` from the grid
    /// geometry × cell metrics (previously hard-coded to zero). Pure builder
    /// check — no PTY needed.
    #[test]
    fn winsize_reports_pixel_geometry_from_cell_metrics() {
        let ws = winsize(Dimensions::new(80, 24), CellMetrics::new(10, 20));
        assert_eq!(ws.ws_col, 80);
        assert_eq!(ws.ws_row, 24);
        assert_eq!(ws.ws_xpixel, 800, "80 cols × 10px must report 800px wide");
        assert_eq!(ws.ws_ypixel, 480, "24 rows × 20px must report 480px tall");

        // The headless default metric (8×16) still yields a non-zero geometry,
        // so a freshly spawned PTY never advertises a zero pixel size.
        let ws_default = winsize(Dimensions::new(80, 24), CellMetrics::DEFAULT);
        assert_eq!(ws_default.ws_xpixel, 640);
        assert_eq!(ws_default.ws_ypixel, 384);
    }

    /// The pixel product is `u16`, so a pathological grid saturates instead of
    /// wrapping — a real (≤ 8K) window never reaches this, but the arithmetic
    /// must not truncate silently.
    #[test]
    fn winsize_saturates_pixel_geometry_at_u16_max() {
        let ws = winsize(Dimensions::new(60000, 60000), CellMetrics::new(1024, 1024));
        assert_eq!(ws.ws_xpixel, u16::MAX);
        assert_eq!(ws.ws_ypixel, u16::MAX);
    }

    /// C23 end-to-end: a spawned PTY reports a real pixel geometry over
    /// TIOCGWINSZ — the default metric at spawn, then the live metric fed via
    /// `set_cell_metrics` before a resize. Before the fix both were zero.
    #[test]
    fn tiocswinsz_carries_real_pixel_dims_after_resize() {
        let session =
            PtySession::spawn_default_shell(TEST_DIMENSIONS).expect("spawn default shell");

        // At spawn the slave winsize was seeded with the 8×16 default metric:
        // 80×24 grid → 640×384 px. Non-zero is the whole point of C23.
        let (spawn_x, spawn_y) = session
            .pixel_dimensions_for_test()
            .expect("query spawn pixel size");
        assert_eq!((spawn_x, spawn_y), (640, 384), "spawn must seed default px");

        // The native layer feeds the live metric, then resizes. TIOCSWINSZ must
        // now carry 100×30 grid × 12×24 px = 1200×720 px.
        session.set_cell_metrics(CellMetrics::new(12, 24));
        session
            .resize(Dimensions::new(100, 30))
            .expect("resize with live metric");
        let (x, y) = session
            .pixel_dimensions_for_test()
            .expect("query resized pixel size");
        assert_eq!(x, 1200, "100 cols × 12px must report 1200px wide");
        assert_eq!(y, 720, "30 rows × 24px must report 720px tall");
    }
}
