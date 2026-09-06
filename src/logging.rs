// SPDX-License-Identifier: GPL-3.0-only
//! Default runtime logging (FREEZE-HARDEN item c).
//!
//! The v0.7.0 freeze postmortem found odytty launched with stderr redirected
//! to `/dev/null`, so every diagnostic the process emitted before the stall
//! was discarded. This module removes the dependency on launcher stderr
//! redirection: `init()` installs a `tracing` subscriber that tees every
//! record to stderr AND to a size-capped, rotated log file at
//! `$XDG_STATE_HOME/odytty/odytty.log` (falling back per platform exactly
//! like the panic log). The default level is WARN so the steady-state file
//! stays tiny; `RUST_LOG=<level>` raises or lowers it.
//!
//! PRIVACY (hard release rule): nothing routed through this module may ever
//! contain terminal content — no PTY bytes, no grid text, no window titles
//! (titles can embed paths and commands). Callers log *state*, *counters*,
//! and *OS error strings* only. See the seam tests here and in
//! `native::watchdog` / `native::panic_log`.
//!
//! The file writer is deliberately lazy and infallible: the log directory is
//! only created on the first actual write (so `odytty --version` never
//! touches the state dir), and every I/O error is swallowed — logging must
//! never take the terminal down.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;

/// Rotation cap for `odytty.log`. When an incoming record would push the file
/// past this size it is renamed to `odytty.log.1` (replacing any previous
/// rotation) and a fresh file is started, so on-disk usage is bounded by
/// roughly twice this value.
const LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;

const LOG_FILE: &str = "odytty.log";
const ROTATED_LOG_FILE: &str = "odytty.log.1";

/// Install the process-wide default subscriber: WARN+ (overridable via a bare
/// `RUST_LOG=<level>`), no ANSI, teed to stderr and the rotated state-dir log
/// file. Idempotent in effect: a second call fails to set the global default
/// and is silently ignored (tests may have installed their own subscriber).
pub fn init() {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(env_level())
        .with_ansi(false)
        .with_writer(TeeMakeWriter::for_state_dir())
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Resolve the default level from `RUST_LOG` when it is a bare level token
/// (`error|warn|info|debug|trace`, any case); anything else — unset, empty,
/// or a directive list this build cannot parse — falls back to WARN. The
/// fallback direction is deliberate: a typo must not silence errors.
fn env_level() -> tracing::Level {
    let Ok(raw) = std::env::var("RUST_LOG") else {
        return tracing::Level::WARN;
    };
    raw.trim().parse().unwrap_or(tracing::Level::WARN)
}

/// The odytty state directory used for `odytty.log` (and the panic log):
/// `$XDG_STATE_HOME/odytty`, falling back to `~/.local/state/odytty`
/// (`~/Library/Logs/odytty` on macOS, `%LOCALAPPDATA%\odytty` on Windows),
/// falling back to an owner-scoped temp leaf on Unix (or the existing inherited
/// ACL temp folder on Windows) when nothing else is resolvable.
pub(crate) fn state_log_dir() -> PathBuf {
    platform_state_dir().unwrap_or_else(temp_state_dir)
}

/// Prepare the final OdyTTY state leaf before a sensitive runtime writer uses
/// it.  Resolution stays side-effect free; this is intentionally called only
/// by the lazy writer paths so CLI version/help remain disk-silent.
pub(crate) fn prepare_state_log_dir() -> io::Result<PathBuf> {
    let dir = state_log_dir();
    crate::state_dir::prepare_private_dir(&dir)?;
    Ok(dir)
}

#[cfg(unix)]
fn temp_state_dir() -> PathBuf {
    // A system temp root is shared.  The fallback leaf is therefore scoped to
    // the effective UID and then validated as a real owner-private directory
    // by `prepare_state_log_dir` before any data is written.
    let uid = unsafe { libc::geteuid() };
    unix_temp_state_dir(&std::env::temp_dir(), uid)
}

#[cfg(unix)]
fn unix_temp_state_dir(temp_root: &std::path::Path, uid: u32) -> PathBuf {
    temp_root.join(format!("odytty-{uid}"))
}

#[cfg(not(unix))]
fn temp_state_dir() -> PathBuf {
    // Windows keeps the existing inherited-ACL behavior.  The Unix UID/mode
    // hardening deliberately does not construct or replace Windows ACLs.
    std::env::temp_dir().join("odytty")
}

#[cfg(target_os = "macos")]
fn platform_state_dir() -> Option<PathBuf> {
    macos_state_dir(env_path("HOME"))
}

#[cfg(target_os = "macos")]
fn macos_state_dir(home: Option<PathBuf>) -> Option<PathBuf> {
    home.map(|home| home.join("Library").join("Logs").join("odytty"))
}

/// Windows: `%LOCALAPPDATA%\odytty` — a persistent per-user location. Without
/// this arm, `XDG_STATE_HOME`/`HOME` are typically unset on Windows, so the
/// state dir fell through to `std::env::temp_dir()` (`%TEMP%`), which Windows
/// periodically cleans — breaking the FREEZE-HARDEN "send me your odytty.log"
/// support flow. Precedent for `LOCALAPPDATA`: `src/text/discovery.rs`
/// (`std::env::var_os("LOCALAPPDATA")`). Kept split from the resolution so the mapping is unit-tested
/// on the `windows-latest` CI leg without mutating process env.
#[cfg(windows)]
fn platform_state_dir() -> Option<PathBuf> {
    windows_state_dir(env_path("LOCALAPPDATA"))
}

#[cfg(windows)]
fn windows_state_dir(local_appdata: Option<PathBuf>) -> Option<PathBuf> {
    local_appdata.map(|local| local.join("odytty"))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_state_dir() -> Option<PathBuf> {
    if let Some(state_home) = env_path("XDG_STATE_HOME") {
        Some(state_home.join("odytty"))
    } else {
        env_path("HOME").map(|home| home.join(".local").join("state").join("odytty"))
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Append `record` directly to the shared rotated log file, bypassing the
/// `tracing` machinery. The panic hook uses this: it must not re-enter the
/// subscriber (the panic may have originated inside it) and must work even
/// when `init()` was never called. Errors are swallowed.
pub(crate) fn append_record_directly(record: &str) {
    shared_rotating_log()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .write_all_swallowing(record.as_bytes());
}

/// The single process-wide rotating file handle. Shared between the
/// subscriber's tee writer and the panic hook's direct append so rotation
/// accounting stays coherent.
fn shared_rotating_log() -> &'static Arc<Mutex<RotatingLog>> {
    static SHARED: OnceLock<Arc<Mutex<RotatingLog>>> = OnceLock::new();
    SHARED.get_or_init(|| {
        Arc::new(Mutex::new(RotatingLog::new(
            state_log_dir().join(LOG_FILE),
            LOG_MAX_BYTES,
        )))
    })
}

/// Size-capped appender: lazily opens (and lazily creates the parent
/// directory of) its path on first write; rotates `path` -> `path.1` when a
/// write would exceed the cap. All errors are swallowed by the callers —
/// logging is strictly best-effort.
struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    file: Option<File>,
    written: u64,
    disk_disabled: bool,
}

impl RotatingLog {
    fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            file: None,
            written: 0,
            disk_disabled: false,
        }
    }

    fn write_all_swallowing(&mut self, buf: &[u8]) {
        if self.disk_disabled {
            return;
        }
        if self.write_all_inner(buf).is_err() {
            self.disable_disk_sink();
        }
    }

    fn write_all_inner(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.file.is_some() && self.written.saturating_add(buf.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        if self.file.is_none() {
            self.open()?;
            // A pre-existing file from an earlier run may already be at the
            // cap; rotate before the first append of this run if so.
            if self.written.saturating_add(buf.len() as u64) > self.max_bytes {
                self.rotate()?;
                if self.file.is_none() {
                    self.open()?;
                }
            }
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        file.write_all(buf)?;
        self.written = self.written.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn open(&mut self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            crate::state_dir::prepare_private_dir(parent)?;
        }
        let file = crate::state_dir::open_append_sensitive(&self.path)?;
        let rotated = self.rotated_path();
        crate::state_dir::repair_existing_sensitive(&rotated)?;
        self.written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        if let Some(parent) = self.path.parent() {
            crate::state_dir::prepare_private_dir(parent)?;
        }
        // Validate both known files through no-follow handles before the
        // replacement.  `rename` replaces a name rather than following a final
        // symlink, and a pre-existing broad same-owner rotation is repaired.
        crate::state_dir::repair_existing_sensitive(&self.path)?;
        let rotated = self.rotated_path();
        crate::state_dir::repair_existing_sensitive(&rotated)?;
        fs::rename(&self.path, &rotated)?;
        crate::state_dir::repair_existing_sensitive(&rotated)?;
        self.written = 0;
        Ok(())
    }

    fn rotated_path(&self) -> PathBuf {
        self.path
            .parent()
            .map(|parent| parent.join(ROTATED_LOG_FILE))
            .unwrap_or_else(|| PathBuf::from(ROTATED_LOG_FILE))
    }

    fn disable_disk_sink(&mut self) {
        self.file = None;
        self.disk_disabled = true;
        // The state sink may be hostile or unavailable.  Keep the notice fixed:
        // do not print a filesystem path or an OS error into terminal-visible
        // diagnostics.
        let _ = io::stderr().write_all(b"odytty: secure state log disabled\n");
    }
}

/// `MakeWriter` handing out tee writers over the shared rotating log.
struct TeeMakeWriter {
    log: Arc<Mutex<RotatingLog>>,
}

impl TeeMakeWriter {
    fn for_state_dir() -> Self {
        Self {
            log: Arc::clone(shared_rotating_log()),
        }
    }
}

impl<'a> MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            log: Arc::clone(&self.log),
        }
    }
}

/// Writes every buffer to stderr (best-effort) and to the rotating file
/// (best-effort), reporting success regardless so `tracing`'s fmt layer never
/// sees an error from us.
struct TeeWriter {
    log: Arc<Mutex<RotatingLog>>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_all_swallowing(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        if let Ok(mut log) = self.log.lock()
            && let Some(file) = log.file.as_mut()
        {
            let _ = file.flush();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock before epoch")
                    .as_nanos()
            ));
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
    fn state_log_dir_is_namespaced_to_odytty() {
        // Whatever the platform resolution or fallback, the state dir must end
        // in an `odytty` component so odytty.log / panic.log never scatter into
        // a shared location. Always-on cross-platform contract.
        let dir = state_log_dir();
        let name = dir.file_name().and_then(|name| name.to_str());
        #[cfg(unix)]
        let expected_temp_name = format!("odytty-{}", unsafe { libc::geteuid() });
        #[cfg(unix)]
        assert!(
            name == Some("odytty") || name == Some(expected_temp_name.as_str()),
            "state log dir must be namespaced under odytty: {dir:?}"
        );
        #[cfg(not(unix))]
        assert_eq!(
            name,
            Some("odytty"),
            "state log dir must be namespaced under odytty: {dir:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_temp_state_dir_is_scoped_to_the_effective_uid() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_state_dir();
        let uid = unsafe { libc::geteuid() };
        let expected = format!("odytty-{uid}");
        assert_eq!(
            dir.file_name().and_then(|name| name.to_str()),
            Some(expected.as_str())
        );

        let temp = TempDir::new("odytty-log-private-dir");
        let leaf = temp.path().join("state");
        crate::state_dir::prepare_private_dir(&leaf).expect("prepare private leaf");
        assert_eq!(
            fs::metadata(leaf)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_temp_fallback_rejects_an_ambiguous_existing_leaf() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new("odytty-temp-fallback");
        let uid = unsafe { libc::geteuid() };
        let fallback = unix_temp_state_dir(root.path(), uid);
        let target = root.path().join("target");
        fs::create_dir(&target).expect("create target");
        symlink(&target, &fallback).expect("seed ambiguous fallback");

        assert!(
            crate::state_dir::prepare_private_dir(&fallback).is_err(),
            "a pre-existing symlink fallback must fail closed"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_dir_uses_local_appdata_with_temp_fallback() {
        // NF13: on Windows the log dir is %LOCALAPPDATA%\odytty (persistent),
        // not %TEMP% (which Windows cleans). Pure mapping test — no env
        // mutation; runs on the authoritative windows-latest CI leg.
        let local = PathBuf::from(r"C:\Users\example\AppData\Local");
        assert_eq!(
            windows_state_dir(Some(local.clone())),
            Some(local.join("odytty"))
        );
        // Unset LOCALAPPDATA => None, so state_log_dir uses the temp-dir
        // fallback rather than panicking.
        assert_eq!(windows_state_dir(None), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_state_dir_retains_the_library_logs_mapping() {
        let home = PathBuf::from("/Users/example");
        assert_eq!(
            macos_state_dir(Some(home.clone())),
            Some(home.join("Library").join("Logs").join("odytty"))
        );
        assert_eq!(macos_state_dir(None), None);
    }

    #[test]
    fn rotating_log_is_lazy_until_first_write() {
        let temp = TempDir::new("odytty-log-lazy");
        let dir = temp.path().join("nested");
        let mut log = RotatingLog::new(dir.join(LOG_FILE), 1024);
        assert!(!dir.exists(), "constructing the log must not touch disk");

        log.write_all_swallowing(b"first\n");
        assert_eq!(
            fs::read_to_string(dir.join(LOG_FILE)).expect("log file"),
            "first\n"
        );
    }

    #[test]
    fn rotation_caps_the_file_and_keeps_one_predecessor() {
        let temp = TempDir::new("odytty-log-rotate");
        let path = temp.path().join(LOG_FILE);
        let mut log = RotatingLog::new(path.clone(), 16);

        log.write_all_swallowing(b"0123456789\n"); // 11 bytes
        log.write_all_swallowing(b"abcdefghij\n"); // would exceed 16 => rotate
        assert_eq!(
            fs::read_to_string(&path).expect("current log"),
            "abcdefghij\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join(ROTATED_LOG_FILE)).expect("rotated log"),
            "0123456789\n"
        );

        log.write_all_swallowing(b"klmnopqrst\n"); // second rotation replaces .1
        assert_eq!(
            fs::read_to_string(&path).expect("current log"),
            "klmnopqrst\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join(ROTATED_LOG_FILE)).expect("rotated log"),
            "abcdefghij\n"
        );
    }

    #[test]
    fn preexisting_oversized_file_rotates_before_first_append() {
        let temp = TempDir::new("odytty-log-preexisting");
        let path = temp.path().join(LOG_FILE);
        fs::write(&path, b"old-run-contents-over-cap\n").expect("seed oversized log");
        let mut log = RotatingLog::new(path.clone(), 16);

        log.write_all_swallowing(b"fresh\n");
        assert_eq!(fs::read_to_string(&path).expect("current log"), "fresh\n");
        assert_eq!(
            fs::read_to_string(temp.path().join(ROTATED_LOG_FILE)).expect("rotated log"),
            "old-run-contents-over-cap\n"
        );
    }

    #[test]
    fn unwritable_destination_is_swallowed() {
        // Point at a path whose parent is a FILE, so create_dir_all fails.
        let temp = TempDir::new("odytty-log-unwritable");
        let blocker = temp.path().join("blocker");
        fs::write(&blocker, b"x").expect("blocker file");
        let mut log = RotatingLog::new(blocker.join(LOG_FILE), 1024);
        log.write_all_swallowing(b"dropped\n"); // must not panic
        assert!(log.file.is_none());
        assert!(
            log.disk_disabled,
            "a failed secure open disables the disk sink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rotation_repairs_current_and_rotated_log_modes_without_changing_data() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new("odytty-log-modes");
        let state = temp.path().join("state");
        fs::create_dir(&state).expect("create state");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).expect("chmod state");
        let current = state.join(LOG_FILE);
        let rotated = state.join(ROTATED_LOG_FILE);
        fs::write(&current, b"old\n").expect("seed current");
        fs::write(&rotated, b"previous\n").expect("seed rotated");
        fs::set_permissions(&current, fs::Permissions::from_mode(0o644)).expect("chmod current");
        fs::set_permissions(&rotated, fs::Permissions::from_mode(0o644)).expect("chmod rotated");

        let mut log = RotatingLog::new(current.clone(), 4);
        log.write_all_swallowing(b"new\n");

        assert_eq!(
            fs::read_to_string(&current).expect("current contents"),
            "new\n"
        );
        assert_eq!(
            fs::read_to_string(&rotated).expect("rotated contents"),
            "old\n"
        );
        assert_eq!(
            fs::metadata(&state)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for path in [&current, &rotated] {
            assert_eq!(
                fs::metadata(path)
                    .expect("log metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{} must be owner-only",
                path.display()
            );
        }
    }
}
