// SPDX-License-Identifier: GPL-3.0-only
//! Per-user Unix-domain socket placement and lifecycle guards.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const SOCKET_DIR_NAME: &str = "odytty";
const SOCKET_PREFIX: &str = "session-";
const SOCKET_SUFFIX: &str = ".sock";
const RUNTIME_DIR_MODE: u32 = 0o700;
const LOCK_FILE_MODE: u32 = 0o600;

/// Upper bound on a bindable `AF_UNIX` socket path, in bytes. `sun_path` is a
/// fixed-size field in `sockaddr_un` and the path must fit with a trailing NUL:
/// macOS sizes it at 104 bytes, Linux at 108. Exceeding it makes `bind()` fail
/// with an opaque error, so we check up front and report the real cause. On
/// Linux the runtime base (`XDG_RUNTIME_DIR`, e.g. `/run/user/<uid>`) is short and
/// never approaches this; the guard is inert there, so the Linux path stays
/// byte-identical.
#[cfg(target_os = "macos")]
const MAX_SOCKET_PATH_LEN: usize = 104;
#[cfg(not(target_os = "macos"))]
const MAX_SOCKET_PATH_LEN: usize = 108;

/// Reject a socket path that would overflow `sun_path` before `bind()`/`connect()`
/// turns it into an opaque failure. The path plus a NUL terminator must fit, so
/// the usable budget is `MAX_SOCKET_PATH_LEN - 1`.
fn check_socket_path_len(path: &Path) -> Result<()> {
    let len = path.as_os_str().as_bytes().len();
    if len >= MAX_SOCKET_PATH_LEN {
        bail!(
            "session-host socket path is {len} bytes, which does not fit the \
             platform AF_UNIX sun_path limit of {MAX_SOCKET_PATH_LEN} (path + NUL): {}",
            path.display()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
}

pub fn runtime_base_from_env() -> Result<PathBuf> {
    // An explicitly-set `XDG_RUNTIME_DIR` always wins, on every platform. This
    // keeps Linux byte-identical (its standard `/run/user/<uid>` is owner-private
    // and local) and lets any environment pin the location deterministically.
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(dir));
    }

    // macOS has no `XDG_RUNTIME_DIR`. Fall back to the per-user Darwin temp dir
    // (`std::env::temp_dir()` resolves `confstr(_CS_DARWIN_USER_TEMP_DIR)`, e.g.
    // `/var/folders/.../T/`), which is per-user and on a local filesystem. We do
    // not trust the parent's mode: `prepare_runtime_dir` creates the `odytty`
    // subdir 0700 and `validate_runtime_dir` enforces it is owner-private, so the
    // socket directory upholds the same local-only, owner-private charter as
    // Linux -- no network, nothing leaves the machine.
    // Each cfg arm is the function's tail expression on its platform, so neither
    // uses `return` (clippy::needless_return fires on the tail position, and it
    // is only lint-checked on the platform whose arm is compiled in).
    #[cfg(target_os = "macos")]
    {
        Ok(env::temp_dir())
    }
    #[cfg(not(target_os = "macos"))]
    {
        bail!("XDG_RUNTIME_DIR is required for session-host sockets")
    }
}

pub fn runtime_dir_path(runtime_base: &Path) -> PathBuf {
    runtime_base.join(SOCKET_DIR_NAME)
}

fn resolved_runtime_base(runtime_base: Option<&Path>) -> Result<Option<PathBuf>> {
    match runtime_base {
        Some(path) => Ok(Some(path.to_owned())),
        None => match runtime_base_from_env() {
            Ok(base) => Ok(Some(base)),
            Err(_) => Ok(None),
        },
    }
}

/// Return the existing session-host runtime directory, using the same
/// `runtime_base_from_env` resolution as the write path. This keeps macOS
/// discovery pointed at the Darwin temp fallback where `runtime_paths(None, ..)`
/// creates sockets instead of only looking at `XDG_RUNTIME_DIR`.
pub fn existing_runtime_dir(runtime_base: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(base) = resolved_runtime_base(runtime_base)? else {
        return Ok(None);
    };
    let dir = runtime_dir_path(&base);
    if !dir.exists() {
        return Ok(None);
    }
    validate_runtime_dir(&dir)?;
    Ok(Some(dir))
}

pub fn runtime_paths(runtime_base: Option<&Path>, session_id: &str) -> Result<RuntimePaths> {
    let base = match resolved_runtime_base(runtime_base)? {
        Some(base) => base,
        None => runtime_base_from_env()?,
    };
    let dir = prepare_runtime_dir(&base)?;
    let socket = session_socket_path(&dir, session_id)?;
    let lock = socket.with_extension("sock.lock");
    Ok(RuntimePaths { dir, socket, lock })
}

pub fn prepare_runtime_dir(runtime_base: &Path) -> Result<PathBuf> {
    let dir = runtime_dir_path(runtime_base);
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
    let socket = runtime_dir.join(format!("{SOCKET_PREFIX}{safe}{SOCKET_SUFFIX}"));
    check_socket_path_len(&socket)?;
    Ok(socket)
}

pub fn session_metadata_path(runtime_dir: &Path, session_id: &str) -> Result<PathBuf> {
    validate_runtime_dir(runtime_dir)?;
    let safe = safe_session_id(session_id)?;
    Ok(runtime_dir.join(format!("{SOCKET_PREFIX}{safe}.meta")))
}

pub fn session_id_from_socket_name(name: &str) -> Option<&str> {
    name.strip_prefix(SOCKET_PREFIX)?
        .strip_suffix(SOCKET_SUFFIX)
}

pub fn bind_listener(socket_path: &Path, lock_path: &Path) -> Result<(UnixListener, StartupLock)> {
    validate_socket_parent(socket_path)?;
    // Both the socket and the (longer) lock path must fit `sun_path`; fail with
    // the real cause instead of an opaque bind error on long macOS temp paths.
    check_socket_path_len(socket_path)?;
    check_socket_path_len(lock_path)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("{prefix}-{}-{seq}", std::process::id()));
            fs::create_dir(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn resolved_runtime_base_uses_explicit_base_without_env() {
        let temp = TempDir::new("shrb");

        assert_eq!(
            resolved_runtime_base(Some(temp.path())).expect("resolve base"),
            Some(temp.path().to_owned())
        );
    }

    #[test]
    fn existing_runtime_dir_agrees_with_runtime_paths_for_explicit_base() {
        let temp = TempDir::new("shrw");
        let paths = runtime_paths(Some(temp.path()), "rw").expect("runtime paths");

        assert_eq!(
            existing_runtime_dir(Some(temp.path())).expect("existing runtime dir"),
            Some(paths.dir)
        );
    }
}
