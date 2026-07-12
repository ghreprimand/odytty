// SPDX-License-Identifier: GPL-3.0-only
//! Bounded, portable reads for operator-supplied config and theme files.
//!
//! Config and theme files are untrusted-in-shape text: a corrupt, truncated, or
//! hostile file must never make the terminal read gigabytes into memory, and on
//! the settings-reload event thread a huge file that produces one warning per
//! line would allocate millions of warning strings and freeze the window while
//! it logs them. Every read of a user config/theme file goes through
//! [`read_capped`], and warning accumulation is bounded by [`MAX_WARNINGS`].
//!
//! Platform behavior is identical on Linux, macOS, and Windows: the cap is a
//! byte count over the file contents and the regular-file check uses portable
//! `std::fs` metadata, so nothing here assumes Unix metadata types or path
//! syntax.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Generous ceiling for a single user config or theme file. A real
/// `odytty.conf` or `.theme` is a few KiB; 1 MiB sits far above any legitimate
/// hand-authored file while bounding the memory a corrupt or hostile file can
/// force the terminal to allocate.
pub(super) const MAX_CONFIG_BYTES: u64 = 1 << 20;

/// Maximum number of parse/apply warnings retained from one config load. Beyond
/// this a single "N further warnings suppressed" record stands in for the rest,
/// so a pathological file cannot allocate and log an unbounded warning stream.
pub(super) const MAX_WARNINGS: usize = 100;

/// Read a user config/theme file, capped at [`MAX_CONFIG_BYTES`].
///
/// Behavior, identical on all platforms:
/// - A missing file returns [`io::ErrorKind::NotFound`] (callers distinguish
///   "deleted"/"absent" from a real read error by this kind).
/// - A non-regular path (directory, FIFO, device, socket) returns
///   [`io::ErrorKind::InvalidData`] rather than blocking or reading a device.
/// - Content longer than the ceiling returns [`io::ErrorKind::InvalidData`]
///   without loading the whole file (the read stops at `MAX_CONFIG_BYTES + 1`).
/// - Non-UTF-8 content returns [`io::ErrorKind::InvalidData`] (config/theme
///   files are UTF-8 text).
pub(super) fn read_capped(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    // Reject non-regular files portably: `FileType::is_file` is defined on every
    // platform, so a directory or FIFO fails the same way on Windows and Unix
    // instead of blocking in a device read or succeeding on a directory handle.
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "config path is not a regular file",
        ));
    }

    // Read at most one byte past the ceiling so an over-limit file is detected
    // without materializing its full contents.
    let mut buf = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("config file exceeds the {MAX_CONFIG_BYTES}-byte limit; not loading"),
        ));
    }

    String::from_utf8(buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "config file is not valid UTF-8"))
}

/// Accumulate warnings into `sink` up to [`MAX_WARNINGS`], counting the rest in
/// `suppressed`. Returns a closure suitable for the `impl FnMut(String)` warn
/// hooks, so a single config load with a pathological number of bad lines cannot
/// grow the warning buffer without bound.
pub(super) fn bounded_warn<'a>(
    sink: &'a mut Vec<String>,
    suppressed: &'a mut usize,
) -> impl FnMut(String) + 'a {
    move |message: String| {
        if sink.len() < MAX_WARNINGS {
            sink.push(message);
        } else {
            *suppressed += 1;
        }
    }
}

/// Append the "N further warnings suppressed" record when any were dropped.
pub(super) fn note_suppressed(sink: &mut Vec<String>, suppressed: usize) {
    if suppressed > 0 {
        sink.push(format!(
            "{suppressed} further configuration warnings suppressed"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "odytty-fsread-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir
    }

    #[test]
    fn reads_a_file_at_and_below_the_limit() {
        let path = temp_path("under");
        let body = vec![b'a'; (MAX_CONFIG_BYTES - 1) as usize];
        File::create(&path).unwrap().write_all(&body).unwrap();
        let read = read_capped(&path).unwrap();
        assert_eq!(read.len() as u64, MAX_CONFIG_BYTES - 1);
        let _ = std::fs::remove_file(&path);

        let path = temp_path("at");
        let body = vec![b'b'; MAX_CONFIG_BYTES as usize];
        File::create(&path).unwrap().write_all(&body).unwrap();
        let read = read_capped(&path).unwrap();
        assert_eq!(read.len() as u64, MAX_CONFIG_BYTES);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_a_file_one_byte_over_the_limit() {
        let path = temp_path("over");
        let body = vec![b'c'; (MAX_CONFIG_BYTES + 1) as usize];
        File::create(&path).unwrap().write_all(&body).unwrap();
        let err = read_capped(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_reports_not_found() {
        let path = temp_path("absent");
        let err = read_capped(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rejects_a_non_regular_path() {
        // A directory is the portable non-regular file every platform provides;
        // this arm runs on the windows-latest leg as well as Unix.
        let path = temp_path("dir");
        std::fs::create_dir(&path).unwrap();
        let err = read_capped(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir(&path);
    }

    #[test]
    fn warnings_are_capped_with_a_suppressed_note() {
        let mut sink = Vec::new();
        let mut suppressed = 0usize;
        {
            let mut warn = bounded_warn(&mut sink, &mut suppressed);
            for i in 0..(MAX_WARNINGS + 250) {
                warn(format!("warning {i}"));
            }
        }
        assert_eq!(sink.len(), MAX_WARNINGS);
        assert_eq!(suppressed, 250);
        note_suppressed(&mut sink, suppressed);
        assert_eq!(sink.len(), MAX_WARNINGS + 1);
        assert!(
            sink.last()
                .unwrap()
                .contains("further configuration warnings suppressed")
        );
    }
}
