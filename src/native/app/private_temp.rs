// SPDX-License-Identifier: GPL-3.0-only
//! Private, exclusive staging files for the image paste-through upload. Kept
//! apart from the upload worker so it is compiled and tested in test builds,
//! where the worker (which spawns `ssh`) is not.

/// Create `path` privately and exclusively, then write `bytes`; the file is
/// removed again when the write fails.
///
/// Unix: `O_CREAT|O_EXCL` with mode `0600` so the pasted image cannot land in a
/// world-readable file and a pre-planted symlink at the target path cannot be
/// followed (the exclusive create fails instead). Other platforms: the same
/// exclusive create, so an existing file is never replaced; the file inherits
/// the temp directory's ACL.
pub(super) fn write_private_temp(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    write_staged(file, path, bytes)
}

/// Write `bytes` to the just-created `out` at `path`, removing `path` when the
/// write fails so no partial temp is left behind.
fn write_staged(
    mut out: impl std::io::Write,
    path: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    let written = out.write_all(bytes).and_then(|()| out.flush());
    drop(out);
    if written.is_err() {
        let _ = std::fs::remove_file(path);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingWriter;

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_staging_write_removes_the_created_temp() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-image-stage-");
        let path = dir.join("staged.png");
        std::fs::write(&path, b"").expect("created by the exclusive open");
        assert!(write_staged(FailingWriter, &path, b"png").is_err());
        assert!(!path.exists(), "the partial temp is removed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn staging_never_replaces_an_existing_file() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-image-stage-");
        let path = dir.join("planted.png");
        std::fs::write(&path, b"planted").expect("planted file");
        assert!(write_private_temp(&path, b"png").is_err());
        assert_eq!(std::fs::read(&path).expect("still there"), b"planted");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn staging_writes_a_new_file() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-image-stage-");
        let path = dir.join("fresh.png");
        write_private_temp(&path, b"png").expect("staged");
        assert_eq!(std::fs::read(&path).expect("staged file"), b"png");
        let _ = std::fs::remove_dir_all(dir);
    }
}
