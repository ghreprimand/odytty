// SPDX-License-Identifier: GPL-3.0-only
//! Owner-private persistent-state filesystem helpers.
//!
//! Unix state contains local paths, saved layouts, and diagnostic records.  The
//! helpers in this module enforce the narrow boundary shared by those writers:
//! only the final OdyTTY-owned directory is normalized, and only known regular
//! files opened without following a final symlink are repaired.  Parents and
//! unknown children are deliberately left alone.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

// Unix owner-only mode bits. The Windows path uses inherited ACLs and never
// references these, so they are scoped to the platforms that apply them.
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Create or repair one application-owned state leaf.  This never changes a
/// parent directory and never walks children.
pub(crate) fn prepare_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::prepare_private_dir(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Validate an existing state leaf without creating it.
pub(crate) fn validate_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::validate_private_dir(path)
    }
    #[cfg(not(unix))]
    {
        let metadata = fs::metadata(path)?;
        if metadata.is_dir() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "persistent state path is not a directory",
            ))
        }
    }
}

/// Open an existing known-sensitive regular file for reading and repair its
/// owner-only mode where Unix mode bits apply.
pub(crate) fn open_existing_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::open_existing_sensitive(path, false, false, false)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().read(true).open(path)
    }
}

/// Open a known-sensitive regular file for append, creating it owner-private
/// on Unix.  Existing objects are validated through the open handle first.
pub(crate) fn open_append_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::open_existing_sensitive(path, true, false, true)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().create(true).append(true).open(path)
    }
}

/// Open a known-sensitive regular file for read/write, creating it
/// owner-private on Unix.  The caller retains the handle for locking.
pub(crate) fn open_read_write_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::open_existing_sensitive(path, false, true, true)
    }
    #[cfg(not(unix))]
    {
        // The caller keeps this handle for advisory locking; the persistent
        // lock content must survive, so truncation is explicitly disabled.
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
    }
}

/// Create one unique sibling file without replacing an existing object.  This
/// is used by atomic persistence writes, so a colliding temporary name can
/// never be opened or removed as though it belonged to this process.
pub(crate) fn create_new_sensitive(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::create_new_sensitive(path)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().create_new(true).write(true).open(path)
    }
}

/// Validate and repair a known sensitive file if it already exists.  Absence is
/// not an error; callers that need creation use one of the open helpers above.
pub(crate) fn repair_existing_sensitive(path: &Path) -> io::Result<()> {
    match open_existing_sensitive(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExportTargetIdentity {
    volume: u64,
    file: u64,
}

fn export_target_identity(path: &Path) -> io::Result<Option<ExportTargetIdentity>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match unix::open_existing_export_target(path) {
            Ok(file) => {
                let metadata = file.metadata()?;
                Ok(Some(ExportTargetIdentity {
                    volume: metadata.dev(),
                    file: metadata.ino(),
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    #[cfg(windows)]
    {
        windows_export::target_identity(path)
    }
}

fn create_new_export(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        unix::create_new_sensitive(path)
    }
    #[cfg(windows)]
    {
        windows_export::create_private(path)
    }
}

fn replace_export_target(tmp: &Path, path: &Path, replacing: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = replacing;
        fs::rename(tmp, path)
    }
    #[cfg(windows)]
    {
        windows_export::replace(tmp, path, replacing)
    }
}

/// Write policy for [`write_atomic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteMode {
    /// Owner-private state (session snapshots, layouts). The parent leaf is
    /// prepared owner-private, the temp is created exclusively at `0600`
    /// (`O_NOFOLLOW`), and the target is validated/repaired to an owner-owned
    /// regular file before and after the rename.
    Sensitive,
    /// Ordinary config a user may inspect or share (`odytty.conf`,
    /// `hosts.conf`). The temp is still created exclusively so a pre-planted
    /// name cannot be followed, but no owner/symlink enforcement is applied to
    /// the target; a stricter existing target mode is preserved (else `0644`
    /// for a new file), rather than being forced wider.
    Config,
    /// Explicit user-selected export. The parent must already exist and is
    /// never created or chmodded. The final file and exclusive sibling temp are
    /// owner-private and the final component is never followed.
    Export,
}

/// Clamp an existing config-file mode to the 0644 baseline unless it is already
/// no more permissive than 0644.
///
/// A mode whose permission bits are a subset of 0644 (for example 0640 or 0600,
/// which only tighten access) is preserved verbatim. Any mode that grants a bit
/// 0644 does not -- a wider group/other read-or-write bit, or ANY execute bit --
/// is replaced with 0644, so a terminal-driven writeback never keeps or widens a
/// permissive or executable config. Execute bits always force the clamp because
/// a config file has no reason to be executable.
#[cfg(unix)]
fn clamp_config_mode(mode: u32) -> u32 {
    if mode & !0o644 == 0 { mode } else { 0o644 }
}

/// Atomically write `bytes` to `path` under `mode`.
///
/// Creates the parent directory, writes a uniquely-named exclusive sibling temp,
/// `sync_all`s it, renames it over the target, then fsyncs the parent directory
/// so the rename is durable across a power loss. A crash mid-write can only leave
/// the temp behind (removed on the error path), never a half-written or
/// renamed-but-empty target. The rename is atomic and replaces an existing target
/// on both Unix and Windows.
///
/// Windows: ordinary config and state files retain their existing inherited-ACL
/// behavior, while export creates an owner-restricted sibling and moves that
/// security descriptor with the file. The Unix mode-preservation step is a
/// no-op there; parent-directory fsync remains best-effort because opening a
/// directory handle for `File::sync_all` is unsupported.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: WriteMode) -> io::Result<()> {
    use std::io::Write as _;

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    match mode {
        WriteMode::Sensitive => {
            if let Some(parent) = parent {
                prepare_private_dir(parent)?;
            }
        }
        WriteMode::Config => {
            if let Some(parent) = parent {
                fs::create_dir_all(parent)?;
            }
        }
        WriteMode::Export => {
            let Some(parent) = parent else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "export destination has no parent directory",
                ));
            };
            if !fs::metadata(parent)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "export destination parent is not a directory",
                ));
            }
        }
    }

    let export_identity = if matches!(mode, WriteMode::Export) {
        export_target_identity(path)?
    } else {
        None
    };

    // Preserve an existing config mode only when it is no more permissive than
    // the 0644 baseline; read it before the write, while the target still
    // exists. A stricter owner/group-limited mode (0640, 0600) is kept, but any
    // mode that adds a bit 0644 lacks -- a wider group/other write bit or ANY
    // execute bit -- is clamped back to 0644 rather than preserved, so a
    // pre-existing 0666/0755/0777 config cannot keep a terminal writeback
    // permissive. A new file (no existing target) falls back to 0644, matching
    // the previous config-writeback behavior.
    #[cfg(unix)]
    let preserved_config_mode: Option<u32> = if matches!(mode, WriteMode::Config) {
        use std::os::unix::fs::PermissionsExt as _;
        Some(
            fs::metadata(path)
                .map(|meta| clamp_config_mode(meta.permissions().mode() & 0o777))
                .unwrap_or(0o644),
        )
    } else {
        None
    };

    let (tmp, mut file) = create_temp_sibling(path, mode)?;
    let write_result = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        // Flush the temp's data BEFORE the rename so a crash right after the
        // rename cannot expose a renamed-but-empty file.
        file.sync_all()?;
        #[cfg(unix)]
        if let Some(target_mode) = preserved_config_mode {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(target_mode))?;
        }
        // Close the data handle before replacement. Windows refuses a rename
        // while the custom export handle is still open without delete sharing;
        // the completed sync above makes closing here safe on every platform.
        drop(file);
        if matches!(mode, WriteMode::Sensitive) {
            // An existing target must be a known owner-owned regular file before
            // replacement; absence is valid for a first write.
            repair_existing_sensitive(path)?;
        }
        if matches!(mode, WriteMode::Export) {
            let current = export_target_identity(path)?;
            if current != export_identity {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "export destination changed before replacement",
                ));
            }
            replace_export_target(&tmp, path, export_identity.is_some())?;
        } else {
            fs::rename(&tmp, path)?;
        }
        if matches!(mode, WriteMode::Sensitive) {
            repair_existing_sensitive(path)?;
        }
        // fsync the parent directory so the rename itself survives a crash.
        if let Some(parent) = parent
            && let Ok(dir) = File::open(parent)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// Reserve a uniquely-named sibling temp next to `path` and return it with an
/// open write handle. The name mixes pid, a nanosecond timestamp, a
/// process-lifetime counter, and an attempt index; combined with exclusive
/// creation, a colliding or pre-planted name is retried rather than followed.
fn create_temp_sibling(path: &Path, mode: WriteMode) -> io::Result<(std::path::PathBuf, File)> {
    static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("odytty.tmp");
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    for attempt in 0..128_u32 {
        let tmp_name = format!(
            ".{base}.{}.{nanos}.{counter}.{attempt}.tmp",
            std::process::id()
        );
        let tmp = parent.join(&tmp_name);
        let created = match mode {
            WriteMode::Sensitive => create_new_sensitive(&tmp),
            WriteMode::Export => create_new_export(&tmp),
            WriteMode::Config => OpenOptions::new().create_new(true).write(true).open(&tmp),
        };
        match created {
            Ok(file) => return Ok((tmp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a unique temporary file",
    ))
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    fn current_uid() -> u32 {
        // SAFETY: `geteuid` has no arguments, no memory preconditions, and no
        // failure mode.  It is queried only to enforce ownership of this
        // process's own persistent state leaf and files.
        unsafe { libc::geteuid() }
    }

    fn owned_by_current_user(uid: u32) -> bool {
        uid == current_uid()
    }

    fn invalid(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, message)
    }

    fn open_dir_no_follow(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true);
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        options.open(path)
    }

    fn validate_private_dir_handle(dir: &File) -> io::Result<()> {
        let metadata = dir.metadata()?;
        if !metadata.file_type().is_dir() {
            return Err(invalid("persistent state path is not a directory"));
        }
        if !owned_by_current_user(metadata.uid()) {
            return Err(invalid("persistent state path is not owned by this user"));
        }
        if metadata.mode() & 0o777 != PRIVATE_DIR_MODE {
            // On macOS this is an fchmod of the opened descriptor: BSD mode
            // bits are tightened without stripping any extended ACL entries.
            dir.set_permissions(fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
        }
        Ok(())
    }

    pub(super) fn prepare_private_dir(path: &Path) -> io::Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            // Parent creation is needed for a fresh XDG/HOME tree, but no parent
            // is chmodded or otherwise normalized by this helper.
            fs::create_dir_all(parent)?;
        }

        let mut builder = fs::DirBuilder::new();
        builder.mode(PRIVATE_DIR_MODE);
        match builder.create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }

        let dir = open_dir_no_follow(path)?;
        validate_private_dir_handle(&dir)
    }

    pub(super) fn validate_private_dir(path: &Path) -> io::Result<()> {
        let dir = open_dir_no_follow(path)?;
        validate_private_dir_handle(&dir)
    }

    fn validate_sensitive_file_handle(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(invalid("persistent state path is not a regular file"));
        }
        if !owned_by_current_user(metadata.uid()) {
            return Err(invalid("persistent state file is not owned by this user"));
        }
        if metadata.mode() & 0o777 != PRIVATE_FILE_MODE {
            // File::set_permissions is descriptor-based on Unix.  This repairs
            // the opened object rather than a pathname that might have changed;
            // macOS ACL entries are preserved rather than replaced.
            file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        }
        Ok(())
    }

    pub(super) fn open_existing_sensitive(
        path: &Path,
        append: bool,
        write: bool,
        create: bool,
    ) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(!append || !write);
        options.append(append).write(write).create(create);
        options.mode(PRIVATE_FILE_MODE);
        // `O_NONBLOCK` makes validation fail promptly for a FIFO or device
        // planted at a sensitive-file path. Regular files ignore the flag,
        // while `O_NOFOLLOW` continues to reject a final-component symlink.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(path)?;
        validate_sensitive_file_handle(&file)?;
        Ok(file)
    }

    pub(super) fn create_new_sensitive(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true).mode(PRIVATE_FILE_MODE);
        options.custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path)?;
        validate_sensitive_file_handle(&file)?;
        Ok(file)
    }

    /// Open a user-selected export target without following the final component
    /// or changing its permissions. The private sibling temp replaces it only
    /// after identity is rechecked, so a failed export leaves the original file
    /// byte-for-byte and mode-for-mode unchanged.
    pub(super) fn open_existing_export_target(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(invalid("export destination is not a regular file"));
        }
        if !owned_by_current_user(metadata.uid()) {
            return Err(invalid("export destination is not owned by this user"));
        }
        Ok(file)
    }

    #[cfg(test)]
    pub(super) fn owner_policy_accepts_only_the_effective_uid(uid: u32) -> bool {
        owned_by_current_user(uid)
    }
}

#[cfg(windows)]
mod windows_export {
    use super::*;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, MOVEFILE_REPLACE_EXISTING,
        MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn windows_error(error: windows::core::Error) -> io::Error {
        let code = error.code().0 as u32;
        if code & 0xffff_0000 == 0x8007_0000 {
            io::Error::from_raw_os_error((code & 0xffff) as i32)
        } else {
            io::Error::other(error.to_string())
        }
    }

    fn open_no_follow(path: &Path) -> io::Result<File> {
        let path = wide(path);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                windows::Win32::Storage::FileSystem::OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(windows_error)?;
        let file = unsafe { File::from_raw_handle(handle.0) };
        validate_regular(&file)?;
        Ok(file)
    }

    fn validate_regular(file: &File) -> io::Result<()> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe {
            GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &raw mut info)
                .map_err(windows_error)?;
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "export destination is a reparse point",
            ));
        }
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "export destination is not a regular file",
            ));
        }
        Ok(())
    }

    pub(super) fn target_identity(path: &Path) -> io::Result<Option<ExportTargetIdentity>> {
        let file = match open_no_follow(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe {
            GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &raw mut info)
                .map_err(windows_error)?;
        }
        Ok(Some(ExportTargetIdentity {
            volume: u64::from(info.dwVolumeSerialNumber),
            file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        }))
    }

    pub(super) fn create_private(path: &Path) -> io::Result<File> {
        let descriptor_text: Vec<u16> = "D:P(A;;FA;;;OW)\0".encode_utf16().collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(descriptor_text.as_ptr()),
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(windows_error)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let path = wide(path);
        let result = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ,
                Some(&raw const attributes),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        }
        let handle = result.map_err(windows_error)?;
        let file = unsafe { File::from_raw_handle(handle.0) };
        validate_regular(&file)?;
        Ok(file)
    }

    pub(super) fn replace(tmp: &Path, path: &Path, replacing: bool) -> io::Result<()> {
        let tmp = wide(tmp);
        let path = wide(path);
        let flags = if replacing {
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
        } else {
            MOVEFILE_WRITE_THROUGH
        };
        unsafe { MoveFileExW(PCWSTR(tmp.as_ptr()), PCWSTR(path.as_ptr()), flags) }
            .map_err(windows_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-state-dir-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos()
        ));
        fs::create_dir(&path).expect("create temp root");
        path
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn private_leaf_and_known_file_repair_same_owner_modes() {
        let root = temp_dir("repair");
        let leaf = root.join("state");
        fs::create_dir(&leaf).expect("create broad leaf");
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o755)).expect("chmod leaf");
        let file = leaf.join("state.json");
        fs::write(&file, "unchanged").expect("seed file");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod file");

        prepare_private_dir(&leaf).expect("repair leaf");
        let opened = open_existing_sensitive(&file).expect("repair file");
        drop(opened);

        assert_eq!(mode(&leaf), PRIVATE_DIR_MODE);
        assert_eq!(mode(&file), PRIVATE_FILE_MODE);
        assert_eq!(fs::read_to_string(&file).expect("read file"), "unchanged");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn private_creations_ignore_a_permissive_umask_in_a_subprocess() {
        const CHILD_ENV: &str = "ODYTTY_STATE_DIR_UMASK_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            // SAFETY: this branch runs in a dedicated test subprocess, so no
            // parallel in-process test can observe the temporary umask.
            unsafe { libc::umask(0) };
            let root = temp_dir("umask-child");
            let leaf = root.join("state");
            prepare_private_dir(&leaf).expect("create private state leaf");
            let file = leaf.join("state.json");
            drop(create_new_sensitive(&file).expect("create private state file"));
            assert_eq!(mode(&leaf), PRIVATE_DIR_MODE);
            assert_eq!(mode(&file), PRIVATE_FILE_MODE);
            let _ = fs::remove_dir_all(root);
            return;
        }

        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("--exact")
            .arg("state_dir::tests::private_creations_ignore_a_permissive_umask_in_a_subprocess")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .status()
            .expect("run umask child");
        assert!(status.success(), "umask child failed with {status}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaf_and_file_are_rejected_without_following() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("symlink");
        let target = root.join("target");
        fs::create_dir(&target).expect("target dir");
        let leaf_link = root.join("state");
        symlink(&target, &leaf_link).expect("leaf link");
        assert!(prepare_private_dir(&leaf_link).is_err());

        let leaf = root.join("real-state");
        prepare_private_dir(&leaf).expect("prepare leaf");
        let target_file = root.join("target-file");
        fs::write(&target_file, "keep").expect("target file");
        let file_link = leaf.join("state.json");
        symlink(&target_file, &file_link).expect("file link");
        assert!(open_existing_sensitive(&file_link).is_err());
        assert_eq!(
            fs::read_to_string(&target_file).expect("target contents"),
            "keep"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn non_directory_state_leaf_is_rejected_without_replacement() {
        let root = temp_dir("non-directory-leaf");
        let leaf = root.join("state");
        fs::write(&leaf, "keep").expect("seed wrong leaf type");

        assert!(prepare_private_dir(&leaf).is_err());
        assert_eq!(fs::read_to_string(&leaf).expect("leaf contents"), "keep");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_known_file_is_rejected_without_repairing_it() {
        let root = temp_dir("non-regular");
        let leaf = root.join("state");
        prepare_private_dir(&leaf).expect("prepare leaf");
        let directory_at_file_path = leaf.join("workspaces.json");
        fs::create_dir(&directory_at_file_path).expect("create wrong type");

        assert!(open_existing_sensitive(&directory_at_file_path).is_err());
        assert!(
            directory_at_file_path.is_dir(),
            "wrong type stays untouched"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn owner_policy_rejects_a_different_uid_without_a_privileged_fixture() {
        let uid = unsafe { libc::geteuid() };
        assert!(unix::owner_policy_accepts_only_the_effective_uid(uid));
        assert!(!unix::owner_policy_accepts_only_the_effective_uid(
            uid.wrapping_add(1)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_config_preserves_a_stricter_existing_mode() {
        // A config file the user tightened to 0600 must not be widened by a
        // writeback (the previous config writer forced 0644).
        let root = temp_dir("cfg-preserve");
        let path = root.join("odytty.conf");
        fs::write(&path, "old = 1\n").expect("seed config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod 600");

        write_atomic(&path, b"new = 2\n", WriteMode::Config).expect("write config");

        assert_eq!(mode(&path), 0o600, "stricter existing mode must survive");
        assert_eq!(fs::read_to_string(&path).expect("read"), "new = 2\n");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_config_clamps_permissive_modes_but_keeps_stricter_ones() {
        // A config writeback preserves only a mode no more permissive than 0644:
        // stricter owner/group-limited modes survive, but any wider read/write
        // bit or ANY execute bit is clamped back to the 0644 baseline so a
        // terminal-driven write never keeps a permissive or executable config.
        let root = temp_dir("cfg-clamp");
        // (seeded mode, expected mode after writeback)
        let cases = [
            (0o666, 0o644), // group+other write -> clamp
            (0o777, 0o644), // world rwx -> clamp
            (0o755, 0o644), // execute bits -> clamp
            (0o640, 0o640), // stricter (no other read) -> preserve
            (0o600, 0o600), // owner-only -> preserve
        ];
        for (seed, expected) in cases {
            let path = root.join(format!("odytty-{seed:o}.conf"));
            fs::write(&path, "old = 1\n").expect("seed config");
            fs::set_permissions(&path, fs::Permissions::from_mode(seed)).expect("chmod seed");

            write_atomic(&path, b"new = 2\n", WriteMode::Config).expect("write config");

            assert_eq!(
                mode(&path),
                expected,
                "seed {seed:o} must land at {expected:o} after writeback",
            );
            assert_eq!(fs::read_to_string(&path).expect("read"), "new = 2\n");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_config_new_file_defaults_to_0644() {
        // A fresh config file (no existing target) lands at 0644, matching the
        // previous new-file behavior, regardless of the caller's umask.
        let root = temp_dir("cfg-new");
        let path = root.join("odytty.conf");

        write_atomic(&path, b"a = 1\n", WriteMode::Config).expect("write config");

        assert_eq!(mode(&path), 0o644);
        assert_eq!(fs::read_to_string(&path).expect("read"), "a = 1\n");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_sensitive_creates_owner_private_target() {
        // Sensitive writes land 0600 owner-private (the snapshot/layout policy).
        let root = temp_dir("sensitive");
        let leaf = root.join("state");
        prepare_private_dir(&leaf).expect("prepare leaf");
        let path = leaf.join("snapshot.json");

        write_atomic(&path, b"{}", WriteMode::Sensitive).expect("write sensitive");

        assert_eq!(mode(&path), PRIVATE_FILE_MODE);
        assert_eq!(fs::read_to_string(&path).expect("read"), "{}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_leaves_no_temp_and_survives_a_pre_planted_temp_name() {
        // The write must complete atomically (no temp residue) even when a file
        // already occupies a plausible temp name; exclusive creation retries.
        let root = temp_dir("temp-collision");
        let path = root.join("odytty.conf");
        // Pre-plant a decoy temp sibling so the first attempt could collide.
        let decoy = root.join(format!(".odytty.conf.{}.0.0.0.tmp", std::process::id()));
        fs::write(&decoy, "decoy").expect("seed decoy");

        write_atomic(&path, b"real\n", WriteMode::Config).expect("write config");

        assert_eq!(fs::read_to_string(&path).expect("read"), "real\n");
        // No .tmp residue from our write remains beyond the decoy we planted.
        let residue: Vec<_> = fs::read_dir(&root)
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                name.ends_with(".tmp") && name != decoy.file_name().unwrap().to_str().unwrap()
            })
            .collect();
        assert!(residue.is_empty(), "unexpected temp residue: {residue:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn write_atomic_config_and_sensitive_write_on_windows() {
        // Both policies write and rename atomically on Windows (mode bits N/A).
        let root = temp_dir("win-write");
        let cfg = root.join("odytty.conf");
        write_atomic(&cfg, b"a = 1\n", WriteMode::Config).expect("config write");
        assert_eq!(fs::read_to_string(&cfg).expect("read cfg"), "a = 1\n");

        let leaf = root.join("state");
        prepare_private_dir(&leaf).expect("prepare leaf");
        let snap = leaf.join("snapshot.json");
        write_atomic(&snap, b"{}", WriteMode::Sensitive).expect("sensitive write");
        assert_eq!(fs::read_to_string(&snap).expect("read snap"), "{}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_ordinary_inherited_acl_operations() {
        let root = temp_dir("windows");
        let leaf = root.join("state");
        prepare_private_dir(&leaf).expect("create state leaf");
        let file = leaf.join("state.json");
        drop(open_read_write_sensitive(&file).expect("create state file"));
        assert!(file.exists());
        let _ = fs::remove_dir_all(root);
    }
}
