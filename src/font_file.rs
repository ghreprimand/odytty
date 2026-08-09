// SPDX-License-Identifier: GPL-3.0-only
//! Bounded reads for font files selected directly or found during discovery.
//!
//! Font parsing owns the returned bytes, so every caller needs a complete file.
//! This boundary rejects non-regular targets and stops one byte past a generous
//! ceiling, preventing a malformed filesystem entry from turning discovery into
//! an unbounded allocation. Symlinks that resolve to ordinary font files remain
//! supported because system font installations commonly use them.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

/// Upper bound for one font file. Installed text and emoji fonts are normally
/// far smaller; 256 MiB leaves headroom for large color-emoji collections
/// while bounding both discovery probes and explicit font-path loads.
pub(crate) const MAX_FONT_FILE_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn read_font_file(path: &Path) -> io::Result<Vec<u8>> {
    read_bounded(path, MAX_FONT_FILE_BYTES)
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    // Check before opening so directories, devices, sockets, and FIFOs are
    // rejected without attempting a potentially blocking read. metadata()
    // follows a symlink to its target, preserving existing installed-font and
    // explicit-path behavior when that target is a regular file.
    let path_metadata = std::fs::metadata(path)?;
    if !path_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "font path is not a regular file",
        ));
    }
    if path_metadata.len() > limit {
        return Err(over_limit(path_metadata.len(), limit));
    }

    let file = open_for_read(path)?;
    // Validate the opened object as well. This catches a path changed after the
    // pre-open check whenever the replacement can be opened without blocking.
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened font path is not a regular file",
        ));
    }
    if opened_metadata.len() > limit {
        return Err(over_limit(opened_metadata.len(), limit));
    }

    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(over_limit(bytes.len() as u64, limit));
    }
    Ok(bytes)
}

fn open_for_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

fn over_limit(found: u64, limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("font file is {found} bytes, over the {limit}-byte limit"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-font-read-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos()
        ));
        fs::create_dir(&path).expect("create temp directory");
        path
    }

    #[test]
    fn accepts_the_limit_and_rejects_limit_plus_one() {
        let dir = temp_dir("cap");
        let path = dir.join("font.bin");
        fs::write(&path, b"0123456789abcdef").expect("write at-limit file");
        assert_eq!(read_bounded(&path, 16).expect("read at limit").len(), 16);

        fs::write(&path, b"0123456789abcdefg").expect("write over-limit file");
        let error = read_bounded(&path, 16).expect_err("reject limit plus one");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("over the 16-byte limit"));
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    #[test]
    fn rejects_a_non_regular_path() {
        let dir = temp_dir("type");
        let error = read_bounded(&dir, 16).expect_err("reject directory");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(dir).expect("remove temp directory");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_symlinks_to_regular_font_files() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("symlink");
        let target = dir.join("target.bin");
        let link = dir.join("font.bin");
        fs::write(&target, b"font").expect("write target");
        symlink(&target, &link).expect("create font symlink");
        assert_eq!(read_bounded(&link, 16).expect("read linked font"), b"font");
        fs::remove_dir_all(dir).expect("remove temp directory");
    }
}
