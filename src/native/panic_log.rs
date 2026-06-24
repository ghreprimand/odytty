// SPDX-License-Identifier: GPL-3.0-only

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PANIC_LOG_FILE: &str = "panic.log";

pub(crate) fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log_panic_info(info);
        previous_hook(info);
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
    let record = format_panic_record(&message, location.as_deref(), SystemTime::now());
    let dir = panic_log_dir();
    let _ = write_record(&dir, &record);
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
    platform_panic_log_dir().unwrap_or_else(|| std::env::temp_dir().join("odytty"))
}

#[cfg(target_os = "macos")]
fn platform_panic_log_dir() -> Option<PathBuf> {
    env_path("HOME").map(|home| home.join("Library").join("Logs").join("odytty"))
}

#[cfg(not(target_os = "macos"))]
fn platform_panic_log_dir() -> Option<PathBuf> {
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

fn format_panic_record(message: &str, location: Option<&str>, now: SystemTime) -> String {
    format!(
        "odytty_panic timestamp_unix_ms={} panic_message=\"{}\" location=\"{}\"\n",
        unix_millis(now),
        escape_field(message),
        escape_field(location.unwrap_or("<unknown>")),
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
    fs::create_dir_all(dir)?;
    let path = dir.join(PANIC_LOG_FILE);
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(record.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn panic_hook_record_writes_parseable_line() {
        let temp = TempDir::new("odytty-panic-log-record");
        let record = format_panic_record(
            "synthetic panic\nquoted \"message\"",
            Some("src/native/mod.rs:108:5"),
            UNIX_EPOCH + Duration::from_millis(42_123),
        );

        let path = write_record(temp.path(), &record).expect("write panic record");
        let content = fs::read_to_string(path).expect("read panic record");

        assert_eq!(content.lines().count(), 1);
        assert!(content.starts_with("odytty_panic timestamp_unix_ms=42123 "));
        assert!(content.contains("panic_message=\"synthetic panic\\nquoted \\\"message\\\"\""));
        assert!(content.contains("location=\"src/native/mod.rs:108:5\""));
    }

    #[test]
    fn write_record_appends_without_truncating() {
        let temp = TempDir::new("odytty-panic-log-append");
        let first = format_panic_record(
            "first",
            Some("src/native/mod.rs:1:2"),
            UNIX_EPOCH + Duration::from_millis(1),
        );
        let second = format_panic_record(
            "second",
            Some("src/native/mod.rs:3:4"),
            UNIX_EPOCH + Duration::from_millis(2),
        );

        let path = write_record(temp.path(), &first).expect("write first record");
        let second_path = write_record(temp.path(), &second).expect("write second record");
        let content = fs::read_to_string(path).expect("read appended records");

        assert_eq!(second_path, temp.path().join(PANIC_LOG_FILE));
        assert_eq!(content, format!("{first}{second}"));
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
