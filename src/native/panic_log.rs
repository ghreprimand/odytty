// SPDX-License-Identifier: GPL-3.0-only
//! Abort-on-panic hook with durable evidence (FREEZE-HARDEN item a).
//!
//! The v0.7.0 freeze postmortem concluded that a swallowed panic on a
//! render/update/worker thread would strand the winit event loop exactly as
//! observed: the loop keeps servicing compositor events mechanically while
//! the thread that did the real work is gone — a zombie window burning 0%
//! CPU. The hook installed here makes that impossible: ANY panic on ANY
//! thread writes the panic message, location, thread name, and a captured
//! backtrace to stderr, to the rotated runtime log (`odytty.log`), and to the
//! structured `panic.log`, then **aborts the process**. Dying visibly beats
//! running undead: the session hosts survive (detached processes), the
//! operator sees the window close, and the logs name the culprit.
//!
//! PRIVACY (hard release rule): panic records carry the panic message,
//! source location, thread name, and code addresses/symbol names — never PTY
//! bytes, grid text, or window titles. Panic messages are code-authored
//! assertion strings; do not interpolate terminal content into panics.

use std::backtrace::Backtrace;
use std::io::{self, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PANIC_LOG_FILE: &str = "panic.log";

pub(crate) fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        log_panic_info(info);
        // FREEZE-HARDEN (a): die visibly. A panicking render/update/worker
        // thread must never leave the event loop alive as a zombie window,
        // and a panicking main thread must not linger half-unwound. abort()
        // (not exit()) so a debugger/coredump still sees the crashed state.
        std::process::abort();
    }));
}

fn log_panic_info(info: &PanicHookInfo<'_>) {
    let message = panic_message(info);
    let location = info.location().map(|location| {
        format!(
            "{}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        )
    });
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("<unnamed>").to_owned();
    let backtrace = Backtrace::force_capture();
    let record = format_panic_record(
        &message,
        location.as_deref(),
        &thread_name,
        SystemTime::now(),
    );
    let human = format_human_report(&message, location.as_deref(), &thread_name, &backtrace);
    // Order matters only in that each write must be independent: any of the
    // three sinks may be unavailable (stderr at /dev/null, unwritable state
    // dir) and the others must still land. All errors are swallowed — the
    // hook must reach abort() no matter what.
    let _ = io::stderr().write_all(human.as_bytes());
    crate::logging::append_record_directly(&human);
    let dir = panic_log_dir();
    let _ = write_record(&dir, &format!("{record}{backtrace}\n"));
}

fn panic_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn panic_log_dir() -> PathBuf {
    crate::logging::state_log_dir()
}

fn format_panic_record(
    message: &str,
    location: Option<&str>,
    thread_name: &str,
    now: SystemTime,
) -> String {
    format!(
        "odytty_panic timestamp_unix_ms={} thread=\"{}\" panic_message=\"{}\" location=\"{}\"\n",
        unix_millis(now),
        escape_field(thread_name),
        escape_field(message),
        escape_field(location.unwrap_or("<unknown>")),
    )
}

/// The multi-line report written to stderr and `odytty.log`: readable in a
/// journal or terminal, with the backtrace verbatim.
fn format_human_report(
    message: &str,
    location: Option<&str>,
    thread_name: &str,
    backtrace: &Backtrace,
) -> String {
    format!(
        "odytty: PANIC (aborting): thread '{}' panicked at {}: {}\nbacktrace:\n{}\n",
        thread_name,
        location.unwrap_or("<unknown>"),
        message,
        backtrace,
    )
}

fn unix_millis(now: SystemTime) -> i128 {
    match now.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i128,
        Err(err) => -(err.duration().as_millis() as i128),
    }
}

fn escape_field(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn write_record(dir: &Path, record: &str) -> io::Result<PathBuf> {
    crate::state_dir::prepare_private_dir(dir)?;
    let path = dir.join(PANIC_LOG_FILE);
    let mut file = crate::state_dir::open_append_sensitive(&path)?;
    file.write_all(record.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn panic_hook_record_writes_parseable_line() {
        let temp = TempDir::new("odytty-panic-log-record");
        let record = format_panic_record(
            "synthetic panic\nquoted \"message\"",
            Some("src/native/mod.rs:108:5"),
            "main",
            UNIX_EPOCH + Duration::from_millis(42_123),
        );

        let path = write_record(temp.path(), &record).expect("write panic record");
        let content = fs::read_to_string(path).expect("read panic record");

        assert_eq!(content.lines().count(), 1);
        assert!(content.starts_with("odytty_panic timestamp_unix_ms=42123 "));
        assert!(content.contains("thread=\"main\""));
        assert!(content.contains("panic_message=\"synthetic panic\\nquoted \\\"message\\\"\""));
        assert!(content.contains("location=\"src/native/mod.rs:108:5\""));
    }

    #[test]
    fn write_record_appends_without_truncating() {
        let temp = TempDir::new("odytty-panic-log-append");
        let first = format_panic_record(
            "first",
            Some("src/native/mod.rs:1:2"),
            "main",
            UNIX_EPOCH + Duration::from_millis(1),
        );
        let second = format_panic_record(
            "second",
            Some("src/native/mod.rs:3:4"),
            "odytty-pty-pump",
            UNIX_EPOCH + Duration::from_millis(2),
        );

        let path = write_record(temp.path(), &first).expect("write first record");
        let second_path = write_record(temp.path(), &second).expect("write second record");
        let content = fs::read_to_string(path).expect("read appended records");

        assert_eq!(second_path, temp.path().join(PANIC_LOG_FILE));
        assert_eq!(content, format!("{first}{second}"));
    }

    #[cfg(unix)]
    #[test]
    fn panic_record_repairs_owner_private_modes_without_rewriting_records() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new("odytty-panic-log-modes");
        let state = temp.path().join("state");
        fs::create_dir(&state).expect("create state");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).expect("chmod state");
        let path = state.join(PANIC_LOG_FILE);
        fs::write(&path, "existing\n").expect("seed panic log");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod panic log");

        write_record(&state, "new\n").expect("append record");

        assert_eq!(
            fs::read_to_string(&path).expect("panic log contents"),
            "existing\nnew\n"
        );
        assert_eq!(
            fs::metadata(&state)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("panic metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// The human report puts every caller-controlled string through plain
    /// interpolation of panic metadata only — message, location, thread —
    /// plus the backtrace. This seam test pins the exact shape so a future
    /// edit cannot quietly start including extra state (privacy rule: no
    /// terminal content in any log sink).
    #[test]
    fn human_report_contains_only_panic_metadata_and_backtrace() {
        let backtrace = Backtrace::disabled();
        let report = format_human_report("boom", Some("src/x.rs:1:1"), "render-worker", &backtrace);
        let expected_prefix = "odytty: PANIC (aborting): thread 'render-worker' panicked at src/x.rs:1:1: boom\nbacktrace:\n";
        assert!(report.starts_with(expected_prefix), "got: {report}");
        assert!(report.ends_with('\n'));
    }

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
}
