// SPDX-License-Identifier: GPL-3.0-only
//! Kitty graphics protocol file-based transports (t=f, t=t, t=s).
//!
//! SECURITY-CRITICAL: these transports read host filesystem state driven by
//! bytes arriving over the PTY (potentially from a remote SSH session).
//! Every path is validated before any I/O:
//!
//! ## Threat model
//!
//! 1. **PTY-directed host reads**: terminal output can request local file reads
//!    and learn success or failure through protocol status. Named transports
//!    are disabled by default; explicit opt-in retains the temp-root boundary.
//!
//! 2. **Symlink / TOCTOU attacks**: a symlink in `/tmp` could redirect to
//!    `/etc/shadow` or `~/.ssh/id_rsa`. The final component is never followed:
//!    Unix opens it with `O_NOFOLLOW`, and Windows opens the reparse point
//!    itself and rejects a handle carrying the reparse-point attribute. The
//!    directory is bound to the object that passed the temp-root check: Unix
//!    opens the directory, requires its canonical path to name that same open
//!    directory inside an allowed root, and opens the file relative to the
//!    directory handle. Windows requires the opened file handle's own final
//!    location to lie inside an allowed root. A directory swapped after the
//!    check therefore cannot redirect the read.
//!
//! 3. **Decode bombs**: a 1-byte file claiming to be a 100MP PNG.
//!    Mitigated by enforcing the ImageStore byte cap on the raw file read
//!    *before* any decode attempt (same cap as direct payloads).
//!
//! 4. **Shared-memory squatting**: a rogue process creates a named object to
//!    inject pixel data. Mitigated by opening read-only, reading within the
//!    size cap, then unlinking only after the read succeeds.
//!
//! 5. **Rebound names at deletion**: between the read and the deletion a name
//!    could be rebound to another object. Deletion targets only the object
//!    that was read: Unix compares the name's current object with the open
//!    descriptor and unlinks relative to the admitted directory, and Windows
//!    deletes through the open handle itself. A rebound name is retained and
//!    the transfer fails with `EPERM:object-changed`.
//!
//! ## Design choices stricter than Kitty proper
//!
//! - Kitty allows t=f from *any* path. OdyTTY restricts to temp dirs only.
//!   Rationale: the remote-exfiltration risk is real and under-documented;
//!   no legitimate application needs to transmit images from `~/.ssh/`.
//!   Programs that use t=f always write to temp dirs first anyway.
//!
//! - Kitty resolves symlinks. OdyTTY rejects final-component links at open.
//!   Rationale: TOCTOU window between stat and open is eliminated.
//!
//! - t=t requires the reference `tty-graphics-protocol` marker and deletes the
//!   file *before* decode, not after. If decode fails,
//!   the temp file is still gone, leaving no lingering data on the filesystem.
//!   This matches Kitty's documented "terminal should delete" semantics
//!   and is strictly safer.
//!   Rejected special files are never deleted.
//!
//! - t=s calls `shm_unlink` only after the bytes were read within the size
//!   cap and while the name still binds the object that was read. Image
//!   decoding happens later. Rejected objects retain their names.

#[cfg(unix)]
use std::ffi::CString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Maximum file/shm read size, enforced before decode.
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
    /// A temporary-file path lacks Kitty's required deletion marker.
    MissingTempMarker,
    /// Path is a final-component symlink or Windows reparse point.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    SymlinkRejected,
    /// Opened handle is not a regular file.
    NonRegularFile,
    /// File open / read failed.
    IoError(String),
    /// File exceeds the read size cap.
    TooLarge,
    /// Shared memory open / map failed.
    ShmError(String),
    /// The name was rebound to a different object between the read and the
    /// deletion, so nothing is deleted. Windows deletes through the open
    /// handle and never reports it.
    #[cfg_attr(not(unix), allow(dead_code))]
    ObjectChanged,
}

impl TransportError {
    pub(super) fn kitty_message(&self) -> &'static str {
        match self {
            TransportError::InvalidPath => "EBADF:invalid-path",
            TransportError::PathNotAllowed => "EPERM:path-not-allowed",
            TransportError::MissingTempMarker => "EPERM:missing-temp-marker",
            TransportError::SymlinkRejected => "EPERM:symlink-rejected",
            TransportError::NonRegularFile => "EPERM:non-regular-file",
            TransportError::IoError(_) => "EIO:read-failed",
            TransportError::TooLarge => "EFBIG:payload-too-large",
            TransportError::ShmError(_) => "EIO:shm-failed",
            TransportError::ObjectChanged => "EPERM:object-changed",
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

/// Whether a canonical directory lies inside one of the admitted temp roots.
fn inside_allowed_root(canonical_dir: &Path) -> bool {
    allowed_temp_dirs()
        .iter()
        .any(|prefix| canonical_dir.starts_with(prefix))
}

/// A transport path whose directory passed the temp-root check, bound to that
/// directory object rather than to a later resolution of its pathname.
///
/// Unix keeps the admitted directory open: the file is opened and later deleted
/// relative to that handle, and the handle must be the same directory as the
/// canonical path that passed containment. Windows opens the file by its
/// canonical path and then verifies the opened handle's own final location, and
/// deletes through that handle.
struct AdmittedPath {
    /// Canonical parent joined with the final component, used for the
    /// deletion-marker check.
    canonical: PathBuf,
    #[cfg(unix)]
    dir: std::fs::File,
    #[cfg(unix)]
    name: CString,
}

#[cfg(unix)]
fn admit_path(path: &Path) -> Result<AdmittedPath, TransportError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let parent = path.parent().ok_or(TransportError::InvalidPath)?;
    let file_name = path.file_name().ok_or(TransportError::InvalidPath)?;
    // Open the directory first, then canonicalize and require that the
    // canonical path still names the opened directory. A swap before the open
    // shows up in the canonical path; a swap after the open cannot affect the
    // handle every later operation uses.
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(parent)
        .map_err(|e| TransportError::IoError(format!("open parent: {e}")))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|e| TransportError::IoError(format!("canonicalize parent: {e}")))?;
    if !inside_allowed_root(&canonical_parent) {
        return Err(TransportError::PathNotAllowed);
    }
    let held = dir
        .metadata()
        .map_err(|e| TransportError::IoError(format!("parent metadata: {e}")))?;
    let named = std::fs::metadata(&canonical_parent)
        .map_err(|e| TransportError::IoError(format!("parent metadata: {e}")))?;
    if (held.dev(), held.ino()) != (named.dev(), named.ino()) {
        return Err(TransportError::PathNotAllowed);
    }
    let name = CString::new(file_name.as_bytes()).map_err(|_| TransportError::InvalidPath)?;
    Ok(AdmittedPath {
        canonical: canonical_parent.join(file_name),
        dir,
        name,
    })
}

#[cfg(windows)]
fn admit_path(path: &Path) -> Result<AdmittedPath, TransportError> {
    // The file itself may not exist yet for the canonicalize call, so
    // canonicalize the parent directory and verify containment. The opened
    // handle's own location is verified again in `open_admitted`.
    let parent = path.parent().ok_or(TransportError::InvalidPath)?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|e| TransportError::IoError(format!("canonicalize parent: {e}")))?;
    let file_name = path.file_name().ok_or(TransportError::InvalidPath)?;
    if !inside_allowed_root(&canonical_parent) {
        return Err(TransportError::PathNotAllowed);
    }
    Ok(AdmittedPath {
        canonical: canonical_parent.join(file_name),
    })
}

/// Open the admitted file without following a final-component link. Unix opens
/// relative to the admitted directory handle, nonblocking so a FIFO or device
/// cannot stall the PTY thread. Windows opens the reparse point itself and then
/// requires the opened handle's final location to lie inside an admitted root.
fn open_admitted(admitted: &AdmittedPath) -> Result<std::fs::File, TransportError> {
    #[cfg(test)]
    test_hooks::fire(test_hooks::Stage::AfterAdmission);

    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = unsafe {
            libc::openat(
                admitted.dir.as_raw_fd(),
                admitted.name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            // O_NOFOLLOW symlink rejection surfaces as ELOOP.
            if e.raw_os_error() == Some(libc::ELOOP) {
                return Err(TransportError::SymlinkRejected);
            }
            return Err(TransportError::IoError(format!("open: {e}")));
        }
        // SAFETY: `fd` is a freshly opened descriptor owned by nothing else.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&admitted.canonical)
            .map_err(|e| TransportError::IoError(format!("open: {e}")))?;
        windows_binding::verify_opened_location(&file)?;
        Ok(file)
    }
}

/// Delete the name of a file that was read, only while it still names the
/// object that was read. A name that no longer exists leaves nothing to delete.
/// A name rebound to another object is retained and reported as
/// [`TransportError::ObjectChanged`]. A failed unlink stays best-effort.
fn delete_admitted(admitted: &AdmittedPath, file: &std::fs::File) -> Result<(), TransportError> {
    #[cfg(test)]
    test_hooks::fire(test_hooks::Stage::BeforeDelete);

    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::MetadataExt;
        let read_object = file
            .metadata()
            .map_err(|e| TransportError::IoError(format!("metadata: {e}")))?;
        // `file` stays open across the check and the unlink, so its inode
        // number cannot be reused by another object meanwhile.
        let fd = unsafe {
            libc::openat(
                admitted.dir.as_raw_fd(),
                admitted.name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(TransportError::ObjectChanged);
        }
        // SAFETY: `fd` is a freshly opened descriptor owned by nothing else.
        let current = unsafe { std::fs::File::from_raw_fd(fd) };
        let bound = current
            .metadata()
            .map_err(|_| TransportError::ObjectChanged)?;
        if (bound.dev(), bound.ino()) != (read_object.dev(), read_object.ino()) {
            return Err(TransportError::ObjectChanged);
        }
        // POSIX has no unlink-by-descriptor. The remaining window between this
        // check and the unlink is confined to the admitted directory handle.
        unsafe {
            libc::unlinkat(admitted.dir.as_raw_fd(), admitted.name.as_ptr(), 0);
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let _ = admitted;
        windows_binding::delete_by_handle(file);
        Ok(())
    }
}

#[cfg(windows)]
mod windows_binding {
    use super::{TransportError, inside_allowed_root};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::PathBuf;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_FLAGS_AND_ATTRIBUTES, FILE_NAME_NORMALIZED,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo,
        GetFinalPathNameByHandleW, ReOpenFile, SetFileInformationByHandle,
    };

    /// Longest final path accepted from the kernel, in UTF-16 units.
    const MAX_FINAL_PATH: usize = 32_768;

    /// Require the opened handle's final location (after every directory
    /// junction or link on the way) to lie inside an admitted temp root.
    pub(super) fn verify_opened_location(file: &std::fs::File) -> Result<(), TransportError> {
        let handle = HANDLE(file.as_raw_handle());
        let mut buf = vec![0_u16; 512];
        loop {
            // FILE_NAME_NORMALIZED with the zero-valued VOLUME_NAME_DOS gives
            // the same `\\?\` form `std::fs::canonicalize` returns.
            let len = unsafe { GetFinalPathNameByHandleW(handle, &mut buf, FILE_NAME_NORMALIZED) }
                as usize;
            if len == 0 {
                return Err(TransportError::IoError(format!(
                    "final path: {}",
                    std::io::Error::last_os_error()
                )));
            }
            if len < buf.len() {
                buf.truncate(len);
                break;
            }
            if len > MAX_FINAL_PATH {
                return Err(TransportError::PathNotAllowed);
            }
            buf.resize(len + 1, 0);
        }
        let final_path = PathBuf::from(OsString::from_wide(&buf));
        let parent = final_path.parent().ok_or(TransportError::PathNotAllowed)?;
        if inside_allowed_root(parent) {
            Ok(())
        } else {
            Err(TransportError::PathNotAllowed)
        }
    }

    /// Mark the opened object itself for deletion. Reopening the same handle
    /// with delete access can fail when the writer did not share deletion;
    /// deletion stays best-effort in that case, as before.
    pub(super) fn delete_by_handle(file: &std::fs::File) {
        let Ok(handle) = (unsafe {
            ReOpenFile(
                HANDLE(file.as_raw_handle()),
                DELETE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAGS_AND_ATTRIBUTES(0),
            )
        }) else {
            return;
        };
        // SAFETY: ReOpenFile returned a new handle owned by nothing else.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        let _ = unsafe {
            SetFileInformationByHandle(
                HANDLE(handle.as_raw_handle()),
                FileDispositionInfo,
                (&raw const info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
    }
}

// ---------------------------------------------------------------------------
// t=f: regular file transport
// ---------------------------------------------------------------------------

/// Read image data from a regular file inside an admitted temp directory. The
/// final component is opened without following links, relative to the
/// admitted directory on Unix, and the opened handle is verified as regular
/// before any read, so FIFOs and devices cannot stall the PTY thread. On
/// Windows the open targets the reparse point itself, the handle's final
/// location must be inside an admitted root, and the handle attributes are
/// checked before the regular-file and size checks.
///
/// `max_read` is the maximum bytes to read (typically the store's decoded cap).
pub(super) fn read_file_transport(
    raw_path: &[u8],
    max_read: usize,
) -> Result<Vec<u8>, TransportError> {
    let path = path_from_bytes(raw_path)?;
    let admitted = admit_path(&path)?;
    let file = open_admitted(&admitted)?;
    read_opened_file(&file, max_read)
}

/// Read and then delete a temp file (t=t). The file is deleted *before*
/// returning the data, so even if later decode fails the temp file is gone.
/// A path rejected before a successful regular-file read is never deleted, and
/// deletion targets only the object that was read.
///
/// `max_read` is the maximum bytes to read.
pub(super) fn read_temp_transport(
    raw_path: &[u8],
    max_read: usize,
) -> Result<Vec<u8>, TransportError> {
    let path = path_from_bytes(raw_path)?;
    let admitted = admit_path(&path)?;
    if !admitted
        .canonical
        .to_string_lossy()
        .contains("tty-graphics-protocol")
    {
        return Err(TransportError::MissingTempMarker);
    }
    let file = open_admitted(&admitted)?;
    let data = read_opened_file(&file, max_read)?;
    delete_admitted(&admitted, &file)?;
    Ok(data)
}

// ---------------------------------------------------------------------------
// t=s: POSIX shared memory transport
// ---------------------------------------------------------------------------

/// Read image data from a POSIX shared memory segment. The segment is opened
/// read-only and unlinked only after its bytes were read within the size cap,
/// and only while the name still binds the object that was read. The name
/// must contain no path separators: it is passed directly to `shm_open`.
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

    // Copy through a fault-contained reader. Positional reads avoid mappings
    // on platforms that support them; macOS isolates its mmap-only shm access
    // in a child so a concurrent truncate cannot deliver SIGBUS to OdyTTY.
    let result = read_shm_fd(fd, max_read);

    let result = result.and_then(|data| {
        #[cfg(test)]
        test_hooks::fire(test_hooks::Stage::BeforeDelete);
        // Only content that was read earns the destructive protocol side
        // effect, and only while the name still binds the object that was read.
        if shm_name_binds_fd(&c_name, fd)? {
            unsafe {
                libc::shm_unlink(c_name.as_ptr());
            }
        }
        Ok(data)
    });

    // Close the fd regardless.
    unsafe {
        libc::close(fd);
    }

    result
}

/// Whether `name` still names the shared-memory object open as `fd`. `Ok(false)`
/// means the name no longer exists, so there is nothing to unlink. A name bound
/// to a different object, or one that cannot be reopened for comparison, is
/// [`TransportError::ObjectChanged`] and is retained.
///
/// The comparison covers device, object number, owner, mode and size. Linux
/// reports a device and object number for shared memory; where a platform
/// reports zero for both, the remaining fields still differ for any object
/// that is not an identical copy.
#[cfg(unix)]
fn shm_name_binds_fd(name: &CString, fd: i32) -> Result<bool, TransportError> {
    let other = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
    if other < 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(TransportError::ObjectChanged)
        };
    }
    let read_object = shm_identity(fd);
    let same = read_object.is_some() && read_object == shm_identity(other);
    unsafe {
        libc::close(other);
    }
    if same {
        Ok(true)
    } else {
        Err(TransportError::ObjectChanged)
    }
}

#[cfg(unix)]
fn shm_identity(fd: i32) -> Option<(u64, u64, u32, u32, u64)> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::MetadataExt;
    // SAFETY: the descriptor stays owned by the caller; ManuallyDrop keeps the
    // temporary File from closing it.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let metadata = file.metadata().ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.mode(),
        metadata.size(),
    ))
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

fn read_opened_file(file: &std::fs::File, max_read: usize) -> Result<Vec<u8>, TransportError> {
    #[cfg(windows)]
    use std::os::windows::fs::MetadataExt;

    #[cfg(windows)]
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

    let cap = max_read.min(MAX_TRANSPORT_READ_BYTES);

    // Check size before reading to avoid allocating for huge files.
    let metadata = file
        .metadata()
        .map_err(|e| TransportError::IoError(format!("metadata: {e}")))?;
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(TransportError::SymlinkRejected);
    }
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
    // Enforce the cap before mapping: never map more than the cap allows.
    if size > cap {
        return Err(TransportError::TooLarge);
    }
    Ok(size)
}

/// Refuse a segment whose size, re-read by `current`, no longer matches the
/// size its bytes were copied at. Both shared-memory readers end with this
/// check, so a segment resized during the copy is refused on every Unix.
#[cfg(unix)]
pub(super) fn ensure_size_unchanged(
    current: Result<usize, TransportError>,
    expected_size: usize,
) -> Result<(), TransportError> {
    if current? != expected_size {
        return Err(TransportError::ShmError(
            "shm segment changed size during read".into(),
        ));
    }
    Ok(())
}

/// Reap child `pid` through `wait` (a `waitpid` wrapper), retrying a wait a
/// signal interrupted, and return its raw status. `None` when the wait fails
/// for any other reason. Only the macOS reader forks; the helper is also built
/// for tests on every Unix so its retry is checked everywhere.
#[cfg(all(unix, any(target_os = "macos", test)))]
pub(super) fn reap_child(
    pid: libc::pid_t,
    mut wait: impl FnMut(libc::pid_t, &mut i32) -> std::io::Result<libc::pid_t>,
) -> Option<i32> {
    let mut status = 0_i32;
    loop {
        match wait(pid, &mut status) {
            Ok(reaped) if reaped == pid => return Some(status),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(_) | Err(_) => return None,
        }
    }
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

    ensure_size_unchanged(checked_shm_size(fd, cap), expected_size)?;
    Ok(buf)
}

/// The two calls of the macOS isolated copy whose outcome a test steers:
/// reaping the copy child, and re-reading the segment size.
#[cfg(target_os = "macos")]
pub(super) trait IsolatedCopyOps {
    /// `waitpid(pid, status, 0)`; an error carries `errno`.
    fn wait(&mut self, pid: libc::pid_t, status: &mut i32) -> std::io::Result<libc::pid_t>;
    /// The segment's current size, under `cap`.
    fn size(&mut self, fd: i32, cap: usize) -> Result<usize, TransportError>;
}

#[cfg(target_os = "macos")]
struct SystemCopyOps;

#[cfg(target_os = "macos")]
impl IsolatedCopyOps for SystemCopyOps {
    fn wait(&mut self, pid: libc::pid_t, status: &mut i32) -> std::io::Result<libc::pid_t> {
        // SAFETY: `status` is a valid out-pointer for the call's duration.
        let reaped = unsafe { libc::waitpid(pid, status, 0) };
        if reaped < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(reaped)
        }
    }

    fn size(&mut self, fd: i32, cap: usize) -> Result<usize, TransportError> {
        checked_shm_size(fd, cap)
    }
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
    read_shm_isolated(fd, expected_size, cap, &mut SystemCopyOps)
}

/// [`read_shm_fd_at_size`] on macOS with its wait and size calls routed
/// through `ops`. The child is reaped on every path after a successful fork,
/// and the size is re-read after the copy as on every other Unix.
#[cfg(target_os = "macos")]
pub(super) fn read_shm_isolated(
    fd: i32,
    expected_size: usize,
    cap: usize,
    ops: &mut impl IsolatedCopyOps,
) -> Result<Vec<u8>, TransportError> {
    if ops.size(fd, cap)? != expected_size {
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
    let status = reap_child(pid, |pid, status| ops.wait(pid, status));
    unsafe { libc::munmap(addr, expected_size) };
    if status != Some(0) || offset != expected_size {
        return Err(TransportError::ShmError(
            "shm segment changed or failed during isolated copy".into(),
        ));
    }
    ensure_size_unchanged(ops.size(fd, cap), expected_size)?;
    Ok(buf)
}

/// Test-only interleaving points. Tests use them to rebind names or swap
/// directories at the exact moments a concurrent process could, so the
/// object-binding guarantees are checked deterministically.
#[cfg(test)]
pub(super) mod test_hooks {
    use std::cell::RefCell;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::core) enum Stage {
        /// The directory passed the temp-root check; the file is not open yet.
        AfterAdmission,
        /// The bytes were read; deletion of the name has not happened yet.
        BeforeDelete,
    }

    type Hook = Box<dyn FnMut(Stage)>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Install a hook for this thread until the returned guard drops.
    pub(in crate::core) fn install(hook: impl FnMut(Stage) + 'static) -> Guard {
        HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
        Guard
    }

    pub(in crate::core) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|cell| *cell.borrow_mut() = None);
        }
    }

    pub(super) fn fire(stage: Stage) {
        let hook = HOOK.with(|cell| cell.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook(stage);
            HOOK.with(|cell| {
                let mut slot = cell.borrow_mut();
                if slot.is_none() {
                    *slot = Some(hook);
                }
            });
        }
    }
}
