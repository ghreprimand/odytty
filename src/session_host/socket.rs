// SPDX-License-Identifier: GPL-3.0-only
//! Per-user Unix-domain socket placement and lifecycle guards.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const SOCKET_DIR_NAME: &str = "odytty";
const SOCKET_PREFIX: &str = "session-";
const SOCKET_SUFFIX: &str = ".sock";
const RUNTIME_DIR_MODE: u32 = 0o700;
const LOCK_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
}

pub fn runtime_base_from_env() -> Result<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is required for session-host sockets"))
}

pub fn runtime_paths(runtime_base: Option<&Path>, session_id: &str) -> Result<RuntimePaths> {
    let base = match runtime_base {
        Some(path) => path.to_owned(),
        None => runtime_base_from_env()?,
    };
    let dir = prepare_runtime_dir(&base)?;
    let socket = session_socket_path(&dir, session_id)?;
    let lock = socket.with_extension("sock.lock");
    Ok(RuntimePaths { dir, socket, lock })
}

pub fn prepare_runtime_dir(runtime_base: &Path) -> Result<PathBuf> {
    let dir = runtime_base.join(SOCKET_DIR_NAME);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(RUNTIME_DIR_MODE))
        .with_context(|| format!("chmod 0700 {}", dir.display()))?;
    validate_runtime_dir(&dir)?;
    Ok(dir)
}

pub fn validate_runtime_dir(dir: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(dir)
        .with_context(|| format!("stat session-host runtime dir {}", dir.display()))?;
    if !metadata.file_type().is_dir() {
        bail!(
            "session-host runtime path is not a directory: {}",
            dir.display()
        );
    }
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid {
        bail!(
            "session-host runtime dir {} is owned by uid {}, expected {}",
            dir.display(),
            metadata.uid(),
            uid
        );
    }
    let mode = metadata.mode() & 0o777;
    if mode != RUNTIME_DIR_MODE {
        bail!(
            "session-host runtime dir {} has mode {:03o}, expected 700",
            dir.display(),
            mode
        );
    }
    Ok(())
}

pub fn validate_socket_parent(socket_path: &Path) -> Result<()> {
    let parent = socket_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("socket path has no parent: {}", socket_path.display()))?;
    validate_runtime_dir(parent)
}

pub fn session_socket_path(runtime_dir: &Path, session_id: &str) -> Result<PathBuf> {
    validate_runtime_dir(runtime_dir)?;
    let safe = safe_session_id(session_id)?;
    Ok(runtime_dir.join(format!("{SOCKET_PREFIX}{safe}{SOCKET_SUFFIX}")))
}

pub fn bind_listener(socket_path: &Path, lock_path: &Path) -> Result<(UnixListener, StartupLock)> {
    validate_socket_parent(socket_path)?;
    let lock = StartupLock::acquire(lock_path)?;
    cleanup_stale_socket(socket_path)?;
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind {}", socket_path.display()))?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("set nonblocking {}", socket_path.display()))?;
    Ok((listener, lock))
}

pub fn cleanup_stale_socket(socket_path: &Path) -> Result<()> {
    match UnixStream::connect(socket_path) {
        Ok(_) => bail!(
            "session-host socket already has a live peer: {}",
            socket_path.display()
        ),
        Err(error) if is_stale_socket_error(&error) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("probe session-host socket {}", socket_path.display()));
        }
    }

    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("stat stale socket {}", socket_path.display()));
        }
    };
    if !metadata.file_type().is_socket() {
        bail!(
            "refusing to remove non-socket stale path {}",
            socket_path.display()
        );
    }
    fs::remove_file(socket_path)
        .with_context(|| format!("remove stale socket {}", socket_path.display()))
}

fn is_stale_socket_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ECONNREFUSED | libc::ECONNRESET | libc::ENOENT)
    )
}

#[derive(Debug)]
pub struct StartupLock {
    file: File,
}

impl StartupLock {
    pub fn acquire(lock_path: &Path) -> Result<Self> {
        if let Some(parent) = lock_path.parent() {
            validate_runtime_dir(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .with_context(|| format!("open startup lock {}", lock_path.display()))?;
        fs::set_permissions(lock_path, fs::Permissions::from_mode(LOCK_FILE_MODE))
            .with_context(|| format!("chmod startup lock {}", lock_path.display()))?;

        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == -1 {
            let error = io::Error::last_os_error();
            bail!(
                "session-host startup lock {} is held or unavailable: {}",
                lock_path.display(),
                error
            );
        }
        Ok(Self { file })
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn safe_session_id(session_id: &str) -> Result<&str> {
    if session_id.is_empty() || session_id.len() > 128 {
        bail!("session id must be 1..=128 bytes");
    }
    if !session_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("session id contains unsupported path characters");
    }
    if session_id == "." || session_id == ".." {
        bail!("session id cannot be . or ..");
    }
    Ok(session_id)
}
