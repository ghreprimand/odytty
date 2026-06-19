// SPDX-License-Identifier: GPL-3.0-only
use std::env;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use rustix::fd::AsFd;
use rustix::process::RawPid;
#[cfg(target_os = "linux")]
use rustix::pty::ioctl_tiocgptpeer;
use rustix::pty::{OpenptFlags, grantpt, openpt, unlockpt};
use rustix::termios::{Winsize, tcgetpgrp, tcsetwinsize};

use crate::core::Dimensions;

/// Whether a foreground job — a process group on the controlling terminal other
/// than the spawned shell itself — is currently running.
///
/// This is a *read-only* classification of the PTY master's foreground process
/// group versus the shell's own group. It never reaps, waits on, or otherwise
/// mutates the child; it only inspects kernel-owned terminal state.
///
/// `Unknown` is the deliberate safe default: callers (e.g. a close-confirmation
/// prompt) treat both `None` and `Unknown` as "safe to close, do not prompt",
/// so a dead PTY, an exited child, or a query error never blocks a close and
/// never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForegroundJob {
    /// The shell itself owns the terminal foreground — nothing would be lost on
    /// close.
    None,
    /// A process group other than the shell owns the terminal foreground — a job
    /// is running in the foreground.
    Running,
    /// The foreground could not be determined (PTY closed, child exited, no
    /// foreground group, or the query errored). Treated as "safe to close".
    Unknown,
}

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

#[derive(Debug, Clone)]
pub struct CommandBuilder {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    current_dir: Option<PathBuf>,
}

impl CommandBuilder {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            current_dir: None,
        }
    }

    pub fn arg(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn current_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.current_dir = Some(path.into());
        self
    }

    fn into_command(self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.args);
        if let Some(path) = self.current_dir {
            command.current_dir(path);
        }
        for (key, value) in self.env {
            command.env(key, value);
        }
        command
    }
}

pub struct PtySession {
    master: File,
    child: Child,
}

impl PtySession {
    pub fn spawn_default_shell(dimensions: Dimensions) -> Result<Self> {
        Self::spawn_default_shell_in(dimensions, None)
    }

    pub fn spawn_default_shell_in(
        dimensions: Dimensions,
        working_directory: Option<PathBuf>,
    ) -> Result<Self> {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let mut command = CommandBuilder::new(shell);
        command.env("TERM", "xterm-256color");
        if let Some(path) = working_directory {
            command.current_dir(path);
        }
        Self::spawn_command(dimensions, command)
    }

    pub fn spawn_shell_command(dimensions: Dimensions, command: &str) -> Result<Self> {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let mut command_builder = CommandBuilder::new(shell);
        command_builder.arg("-lc");
        command_builder.arg(command);
        command_builder.env("TERM", "xterm-256color");

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
        command.env("TERM", "xterm-256color");
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

        let child = command.spawn().context("spawn pty command")?;

        Ok(Self { master, child })
    }

    pub fn resize(&self, dimensions: Dimensions) -> Result<()> {
        tcsetwinsize(&self.master, winsize(dimensions)).context("resize pty")
    }

    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(PtyReader {
            file: self.master.try_clone().context("clone pty reader")?,
        }))
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
}

struct PtyReader {
    file: File,
}

impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.file.read(buf) {
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            result => result,
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

fn open_pty_pair(dimensions: Dimensions) -> Result<(File, File)> {
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

    tcsetwinsize(&slave, winsize(dimensions)).context("set pty window size")?;

    let master = unsafe { File::from_raw_fd(master.into_raw_fd()) };
    let slave = unsafe { File::from_raw_fd(slave.into_raw_fd()) };
    Ok((master, slave))
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

#[cfg(not(target_os = "linux"))]
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

fn winsize(dimensions: Dimensions) -> Winsize {
    Winsize {
        ws_row: dimensions.rows.min(u16::MAX as usize) as u16,
        ws_col: dimensions.columns.min(u16::MAX as usize) as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
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
}
