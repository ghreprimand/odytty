use std::env;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};

use crate::core::Dimensions;

#[derive(Debug, Clone)]
pub struct CommandBuilder {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl CommandBuilder {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
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

    fn into_command(self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.args);
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
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let mut command = CommandBuilder::new(shell);
        command.env("TERM", "xterm-256color");
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
        // Linux TIOCSCTTY is not exposed as a focused rustix helper.
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
            bail!("child pid {pid} is too large for Linux pid_t");
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
    let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
    let master = openpt(flags).context("open /dev/ptmx")?;
    grantpt(&master).context("grant pty")?;
    unlockpt(&master).context("unlock pty")?;
    let slave = ioctl_tiocgptpeer(&master, flags).context("open pty slave")?;

    tcsetwinsize(&slave, winsize(dimensions)).context("set pty window size")?;

    let master = unsafe { File::from_raw_fd(master.into_raw_fd()) };
    let slave = unsafe { File::from_raw_fd(slave.into_raw_fd()) };
    Ok((master, slave))
}

fn winsize(dimensions: Dimensions) -> Winsize {
    Winsize {
        ws_row: dimensions.rows.min(u16::MAX as usize) as u16,
        ws_col: dimensions.columns.min(u16::MAX as usize) as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}
