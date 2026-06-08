use std::env;
use std::io::{Read, Write};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::core::Dimensions;

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
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
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: dimensions.rows as u16,
                cols: dimensions.columns as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("open pty")?;

        let child = pair
            .slave
            .spawn_command(command)
            .context("spawn pty command")?;

        Ok(Self {
            master: pair.master,
            child,
        })
    }

    pub fn resize(&self, dimensions: Dimensions) -> Result<()> {
        self.master
            .resize(PtySize {
                rows: dimensions.rows as u16,
                cols: dimensions.columns as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize pty")
    }

    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master.try_clone_reader().context("clone pty reader")
    }

    pub fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        self.master.take_writer().context("take pty writer")
    }

    pub fn try_wait(&mut self) -> Result<Option<portable_pty::ExitStatus>> {
        self.child.try_wait().context("poll child")
    }

    pub fn wait(&mut self) -> Result<portable_pty::ExitStatus> {
        self.child.wait().context("wait for child")
    }

    pub fn kill(&mut self) -> Result<()> {
        self.child.kill().context("kill child")
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>> {
        let mut reader = self.try_clone_reader()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).context("read pty output")?;
        Ok(bytes)
    }
}
