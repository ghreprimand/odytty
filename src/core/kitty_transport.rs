// SPDX-License-Identifier: GPL-3.0-only
//! Kitty graphics protocol file-based transports (t=f, t=t, t=s).
//!
//! SECURITY-CRITICAL: these transports read host filesystem state driven by
//! bytes arriving over the PTY (potentially from a remote SSH session).
//! Every path is validated before any I/O:
//!
//! ## Threat model
//!
//! 1. **Remote exfiltration via SSH**: a malicious remote host can send APC
//!    sequences that instruct the local terminal to read a local file and echo
//!    pixel data back. Mitigated by restricting t=f/t=t paths to canonical
//!    temp directories only (`/tmp`, `/dev/shm`, resolved `$TMPDIR`).
//!
//! 2. **Symlink / TOCTOU attacks**: a symlink in `/tmp` could redirect to
//!    `/etc/shadow` or `~/.ssh/id_rsa`. Mitigated by opening with
//!    `O_NOFOLLOW` so symlinks are rejected at the kernel level, and by
//!    canonicalizing the *directory* component to verify it resolves inside
//!    an allowed prefix before opening the file.
//!
//! 3. **Decode bombs**: a 1-byte file claiming to be a 100MP PNG.
//!    Mitigated by enforcing the ImageStore byte cap on the raw file read
//!    *before* any decode attempt (same cap as direct payloads).
//!
//! 4. **Shared-memory squatting**: a rogue process creates a `/dev/shm`
//!    object with a known name to inject pixel data. Mitigated by opening
//!    with `O_RDONLY` + immediate `shm_unlink` so the object lifetime is
//!    minimal, and the same size cap applies.
//!
//! ## Design choices stricter than Kitty proper
//!
//! - Kitty allows t=f from *any* path. OdyTTY restricts to temp dirs only.
//!   Rationale: the remote-exfiltration risk is real and under-documented;
//!   no legitimate application needs to transmit images from `~/.ssh/`.
//!   Programs that use t=f always write to temp dirs first anyway.
//!
//! - Kitty resolves symlinks. OdyTTY rejects them (`O_NOFOLLOW`).
//!   Rationale: TOCTOU window between stat and open is eliminated.
//!
//! - t=t deletes the file *before* decode, not after. If decode fails,
//!   the temp file is still gone — no lingering data on the filesystem.
//!   This matches Kitty's documented "terminal should delete" semantics
//!   and is strictly safer.
//!   Rejected special files are never deleted.
//!
//! - t=s calls `shm_unlink` immediately after opening, before reading.
//!   The shared memory segment ceases to be addressable by name as soon
//!   as possible.

#[cfg(unix)]
use std::ffi::CString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Maximum file/shm read size — enforced before decode.
/// Kept intentionally lower than the image store decoded cap since the
/// file is base64 or raw payload *before* pixel expansion.
const MAX_TRANSPORT_READ_BYTES: usize = 96 * 1024 * 1024;

/// Errors specific to file-based transports. These are converted to
/// Kitty error responses at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TransportError {
    /// Path is empty or contains null bytes.
    InvalidPath,
    /// Path is outside the allowed temp directory set.
    PathNotAllowed,
    /// Path is a symlink (O_NOFOLLOW enforcement).
    #[cfg_attr(not(unix), allow(dead_code))]
    SymlinkRejected,
    /// Opened handle is not a regular file.
    NonRegularFile,
    /// File open / read failed.
    IoError(String),
    /// File exceeds the read size cap.
    TooLarge,
    /// Shared memory open / map failed.
    ShmError(String),
}

impl TransportError {
    pub(super) fn kitty_message(&self) -> &'static str {
        match self {
            TransportError::InvalidPath => "EBADF:invalid-path",
            TransportError::PathNotAllowed => "EPERM:path-not-allowed",
            TransportError::SymlinkRejected => "EPERM:symlink-rejected",
            TransportError::NonRegularFile => "EPERM:non-regular-file",
            TransportError::IoError(_) => "EIO:read-failed",
            TransportError::TooLarge => "EFBIG:payload-too-large",
            TransportError::ShmError(_) => "EIO:shm-failed",
        }
    }
}

// ---------------------------------------------------------------------------
// Allowed directory set
// ---------------------------------------------------------------------------

/// Returns the set of canonical directory prefixes that file transports
/// may read from. Each entry is a canonicalized absolute path.
fn allowed_temp_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // Always include /tmp and /dev/shm.
    for base in &["/tmp", "/dev/shm"] {
        if let Ok(canonical) = std::fs::canonicalize(base) {
            dirs.push(canonical);
        }
    }

    // Include $TMPDIR if set and canonicalizable.
    if let Ok(tmpdir) = std::env::var("TMPDIR")
        && let Ok(canonical) = std::fs::canonicalize(&tmpdir)
    {
        // Avoid duplicates.
        if !dirs.contains(&canonical) {
            dirs.push(canonical);
        }
    }

    #[cfg(windows)]
    if let Ok(canonical) = std::fs::canonicalize(std::env::temp_dir())
        && !dirs.contains(&canonical)
    {
        dirs.push(canonical);
    }

    dirs
}

/// Validate that `path` lives inside one of the allowed temp directories.
/// Returns the validated canonical path on success.
fn validate_path(path: &Path) -> Result<PathBuf, TransportError> {
    // The file itself may not exist yet for the canonicalize call, so
    // canonicalize the parent directory and verify containment.
    let parent = path.parent().ok_or(TransportError::InvalidPath)?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|e| TransportError::IoError(format!("canonicalize parent: {e}")))?;
    let file_name = path.file_name().ok_or(TransportError::InvalidPath)?;

    let canonical = canonical_parent.join(file_name);

    let allowed = allowed_temp_dirs();
    if allowed.is_empty() {
        return Err(TransportError::PathNotAllowed);
    }
    let in_allowed = allowed
        .iter()
        .any(|prefix| canonical_parent.starts_with(prefix));
    if !in_allowed {
        return Err(TransportError::PathNotAllowed);
    }

    Ok(canonical)
}

// ---------------------------------------------------------------------------
// t=f: regular file transport
// ---------------------------------------------------------------------------

/// Read image data from a regular file. The file is opened with `O_NOFOLLOW`
/// and must reside inside an allowed temp directory. On Unix the open is also
/// nonblocking, then the opened handle is verified as regular before any read,
/// so FIFOs and devices cannot stall the PTY thread. Windows has no POSIX FIFO
/// surface and retains the existing read-only regular-file open behavior.
///
/// `max_read` is the maximum bytes to read (typically the store's decoded cap).
pub(super) fn read_file_transport(
    raw_path: &[u8],
    max_read: usize,
) -> Result<Vec<u8>, TransportError> {
    let path = path_from_bytes(raw_path)?;
    let validated = validate_path(&path)?;
    read_regular_file(&validated, max_read)
}

/// Read and then delete a temp file (t=t). The file is deleted *before*
/// returning the data — even if later decode fails, the temp file is gone.
/// A path rejected before a successful regular-file read is never deleted.
///
/// `max_read` is the maximum bytes to read.
pub(super) fn read_temp_transport(
    raw_path: &[u8],
    max_read: usize,
) -> Result<Vec<u8>, TransportError> {
    let path = path_from_bytes(raw_path)?;
    let validated = validate_path(&path)?;
    let data = read_regular_file(&validated, max_read)?;

    // Delete unconditionally — best-effort; failure is not fatal.
    let _ = std::fs::remove_file(&validated);

    Ok(data)
}

// ---------------------------------------------------------------------------
// t=s: POSIX shared memory transport
// ---------------------------------------------------------------------------

/// Read image data from a POSIX shared memory segment. The segment is opened
/// read-only and immediately unlinked before the data is read. The name
/// must contain no path separators — it is passed directly to `shm_open`.
///
/// `max_read` is the maximum bytes to read.
#[cfg(unix)]
pub(super) fn read_shm_transport(
    raw_name: &[u8],
    max_read: usize,
) -> Result<Vec<u8>, TransportError> {
    let name_str = std::str::from_utf8(raw_name).map_err(|_| TransportError::InvalidPath)?;

    // POSIX shared memory names must start with '/' and contain no
    // further slashes. Validate strictly.
    if name_str.is_empty() {
        return Err(TransportError::InvalidPath);
    }

    // Build the canonical shm name with leading /.
    let canonical_name = if let Some(stripped) = name_str.strip_prefix('/') {
        if name_str.len() < 2 || stripped.contains('/') {
            return Err(TransportError::InvalidPath);
        }
        name_str.to_string()
    } else {
        if name_str.contains('/') {
            return Err(TransportError::InvalidPath);
        }
        format!("/{name_str}")
    };

    let c_name = CString::new(canonical_name).map_err(|_| TransportError::InvalidPath)?;

    // Open read-only.
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        return Err(TransportError::ShmError(format!("shm_open: {err}")));
    }

    // Immediately unlink — the segment stays readable via our fd but becomes
    // unreachable by name. Minimizes the window for squatting attacks.
    unsafe {
        libc::shm_unlink(c_name.as_ptr());
    }

    // Copy through a fault-contained reader. Positional reads avoid mappings
    // on platforms that support them; macOS isolates its mmap-only shm access
    // in a child so a concurrent truncate cannot deliver SIGBUS to OdyTTY.
    let result = read_shm_fd(fd, max_read);

    // Close the fd regardless.
    unsafe {
        libc::close(fd);
    }

    result
}

/// POSIX shared-memory transport (t=s) is unavailable on non-Unix platforms:
/// `shm_open`/`mmap` have no portable analogue. Returns a transport error so
/// the call site emits the standard Kitty failure response; the caller only
/// reads [`TransportError::kitty_message`], never matches the variant.
#[cfg(not(unix))]
pub(super) fn read_shm_transport(
    _raw_name: &[u8],
    _max_read: usize,
) -> Result<Vec<u8>, TransportError> {
    Err(TransportError::ShmError(
        "shm transport unsupported on this platform".into(),
    ))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn path_from_bytes(raw: &[u8]) -> Result<PathBuf, TransportError> {
    if raw.is_empty() {
        return Err(TransportError::InvalidPath);
    }
    // Kitty sends the path as base64-decoded bytes. For file paths
    // we require UTF-8 (no exotic OsStr encodings from a remote).
    let s = std::str::from_utf8(raw).map_err(|_| TransportError::InvalidPath)?;
    if s.is_empty() || s.contains('\0') {
        return Err(TransportError::InvalidPath);
    }
    Ok(PathBuf::from(s))
}

fn read_regular_file(path: &Path, max_read: usize) -> Result<Vec<u8>, TransportError> {
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let cap = max_read.min(MAX_TRANSPORT_READ_BYTES);

    // Open with O_NOFOLLOW — kernel rejects symlinks. O_NONBLOCK makes the
    // admission check safe for FIFOs and devices; only regular files proceed.
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);

    let file = opts.open(path).map_err(|e| {
        // O_NOFOLLOW symlink rejection surfaces as ELOOP on Unix; the flag and
        // this classification are both Unix-only. Off Unix the open simply
        // succeeds or fails as a plain I/O error.
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::ELOOP) {
            return TransportError::SymlinkRejected;
        }
        TransportError::IoError(format!("open: {e}"))
    })?;

    // Check size before reading to avoid allocating for huge files.
    let metadata = file
        .metadata()
        .map_err(|e| TransportError::IoError(format!("metadata: {e}")))?;
    if !metadata.is_file() {
        return Err(TransportError::NonRegularFile);
    }
    let len = metadata.len() as usize;
    if len > cap {
        return Err(TransportError::TooLarge);
    }

    // Read with cap+1 to detect growth between stat and read.
    let mut buf = Vec::with_capacity(len.min(cap));
    let read = file
        .take((cap as u64) + 1)
        .read_to_end(&mut buf)
        .map_err(|e| TransportError::IoError(format!("read: {e}")))?;
    if read > cap {
        return Err(TransportError::TooLarge);
    }

    Ok(buf)
}

#[cfg(unix)]
fn read_shm_fd(fd: i32, max_read: usize) -> Result<Vec<u8>, TransportError> {
    let cap = max_read.min(MAX_TRANSPORT_READ_BYTES);
    let size = checked_shm_size(fd, cap)?;
    read_shm_fd_at_size(fd, size, cap)
}

#[cfg(unix)]
pub(super) fn checked_shm_size(fd: i32, cap: usize) -> Result<usize, TransportError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        return Err(TransportError::ShmError(format!("fstat: {err}")));
    }
    let raw_size = unsafe { stat.assume_init() }.st_size;
    if raw_size <= 0 {
        return Err(TransportError::ShmError("empty shm segment".into()));
    }
    let size = raw_size as usize;
    // Enforce the cap before mapping — never map more than the cap allows.
    if size > cap {
        return Err(TransportError::TooLarge);
    }
    Ok(size)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn read_shm_fd_at_size(
    fd: i32,
    expected_size: usize,
    cap: usize,
) -> Result<Vec<u8>, TransportError> {
    // A second size check catches a shrink after the admission check. The
    // positional read itself remains fault-tolerant if truncation races later:
    // it returns EOF/error rather than touching an invalid mapped page.
    if checked_shm_size(fd, cap)? != expected_size {
        return Err(TransportError::ShmError(
            "shm segment changed size before read".into(),
        ));
    }

    let mut buf = vec![0_u8; expected_size];
    let mut offset = 0;
    while offset < expected_size {
        let read = unsafe {
            libc::pread(
                fd,
                buf[offset..].as_mut_ptr().cast(),
                expected_size - offset,
                offset as libc::off_t,
            )
        };
        if read > 0 {
            offset += read as usize;
            continue;
        }
        if read < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        let detail = if read == 0 {
            "shm segment shrank during read".into()
        } else {
            format!("pread: {}", std::io::Error::last_os_error())
        };
        return Err(TransportError::ShmError(detail));
    }

    if checked_shm_size(fd, cap)? != expected_size {
        return Err(TransportError::ShmError(
            "shm segment changed size during read".into(),
        ));
    }
    Ok(buf)
}

/// macOS POSIX shm descriptors are mmap-only. The mapping and copy run in a
/// short-lived child process, using only async-signal-safe libc operations
/// after `fork`. A concurrent truncate may SIGBUS that child, but the parent
/// observes an incomplete pipe copy/non-zero wait status and returns an error.
#[cfg(target_os = "macos")]
pub(super) fn read_shm_fd_at_size(
    fd: i32,
    expected_size: usize,
    cap: usize,
) -> Result<Vec<u8>, TransportError> {
    if checked_shm_size(fd, cap)? != expected_size {
        return Err(TransportError::ShmError(
            "shm segment changed size before read".into(),
        ));
    }

    // Establish the mapping in the parent without touching its pages. Only the
    // child dereferences it; the parent can always munmap safely after reaping.
    let addr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            expected_size,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        return Err(TransportError::ShmError(format!(
            "mmap: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut pipe_fds = [0_i32; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } < 0 {
        unsafe { libc::munmap(addr, expected_size) };
        return Err(TransportError::ShmError(format!(
            "pipe: {}",
            std::io::Error::last_os_error()
        )));
    }
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        unsafe {
            libc::close(pipe_fds[0]);
            libc::close(pipe_fds[1]);
            libc::munmap(addr, expected_size);
        }
        return Err(TransportError::ShmError(format!(
            "fork: {}",
            std::io::Error::last_os_error()
        )));
    }
    if pid == 0 {
        unsafe {
            libc::close(pipe_fds[0]);
            let mut written = 0_usize;
            while written < expected_size {
                let count = libc::write(
                    pipe_fds[1],
                    (addr as *const u8).add(written).cast(),
                    expected_size - written,
                );
                if count > 0 {
                    written += count as usize;
                } else if count < 0 && *libc::__error() == libc::EINTR {
                    continue;
                } else {
                    libc::_exit(3);
                }
            }
            libc::munmap(addr, expected_size);
            libc::close(pipe_fds[1]);
            libc::_exit(0);
        }
    }

    unsafe { libc::close(pipe_fds[1]) };
    let mut buf = vec![0_u8; expected_size];
    let mut offset = 0_usize;
    while offset < expected_size {
        let read = unsafe {
            libc::read(
                pipe_fds[0],
                buf[offset..].as_mut_ptr().cast(),
                expected_size - offset,
            )
        };
        if read > 0 {
            offset += read as usize;
        } else if read < 0
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {
            continue;
        } else {
            break;
        }
    }
    unsafe { libc::close(pipe_fds[0]) };
    let mut status = 0_i32;
    let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
    unsafe { libc::munmap(addr, expected_size) };
    if waited != pid || status != 0 || offset != expected_size {
        return Err(TransportError::ShmError(
            "shm segment changed or failed during isolated copy".into(),
        ));
    }
    Ok(buf)
}
