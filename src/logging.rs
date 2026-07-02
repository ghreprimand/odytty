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

use std::fs::{self, File, OpenOptions};
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
/// (`~/Library/Logs/odytty` on macOS), falling back to a temp-dir `odytty`
/// folder when no home is resolvable.
pub(crate) fn state_log_dir() -> PathBuf {
    platform_state_dir().unwrap_or_else(|| std::env::temp_dir().join("odytty"))
}

#[cfg(target_os = "macos")]
fn platform_state_dir() -> Option<PathBuf> {
    env_path("HOME").map(|home| home.join("Library").join("Logs").join("odytty"))
}

#[cfg(not(target_os = "macos"))]
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
}

impl RotatingLog {
    fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            file: None,
            written: 0,
        }
    }

    fn write_all_swallowing(&mut self, buf: &[u8]) {
        let _ = self.write_all_inner(buf);
    }

    fn write_all_inner(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.file.is_some() && self.written.saturating_add(buf.len() as u64) > self.max_bytes {
            self.rotate();
        }
        if self.file.is_none() {
            self.open()?;
            // A pre-existing file from an earlier run may already be at the
            // cap; rotate before the first append of this run if so.
            if self.written.saturating_add(buf.len() as u64) > self.max_bytes {
                self.rotate();
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
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    fn rotate(&mut self) {
        self.file = None;
        let rotated = self
            .path
            .parent()
            .map(|parent| parent.join(ROTATED_LOG_FILE))
            .unwrap_or_else(|| PathBuf::from(ROTATED_LOG_FILE));
        let _ = fs::rename(&self.path, rotated);
        self.written = 0;
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
    }
}
