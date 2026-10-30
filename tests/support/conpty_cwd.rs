// SPDX-License-Identifier: GPL-3.0-only
//! Compare the two shell directory views with the held, owned launch directory.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    volume: u64,
    id: [u8; 16],
}
fn selected(expected: Identity, process: Identity, provider: Identity) -> bool {
    expected == process && expected == provider
}
fn report(bytes: &[u8]) -> Result<[&str; 2], &'static str> {
    if bytes.len() > 8192 {
        return Err("cwd_report_over_capacity");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "cwd_report_encoding")?;
    let mut lines = text.lines();
    let process = lines.next().ok_or("cwd_report_missing_process")?;
    let provider = lines.next().ok_or("cwd_report_missing_provider")?;
    if lines.next().is_some()
        || [process, provider]
            .iter()
            .any(|p| p.is_empty() || p.contains('\0') || p.encode_utf16().count() > 1024)
    {
        return Err("cwd_report_invalid_paths");
    }
    Ok([process, provider])
}

// Windows path components are checked before opening the reported path.
fn safe_component(component: &str) -> bool {
    let component = component.trim_end_matches([' ', '.']);
    let stem = component
        .split(['.', ':'])
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    let upper = stem.to_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    for prefix in ["COM", "LPT"] {
        if upper.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        }) {
            return false;
        }
    }
    true
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Component, Path, PathBuf, Prefix};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo,
        GetFileInformationByHandleEx,
    };

    // Only fixture-reported local drive paths may be probed. In particular,
    // a malformed report never sends a filesystem request to a UNC share.
    fn local(path: &Path) -> bool {
        path.components().all(|component| match component {
            Component::Normal(name) => name.to_str().is_some_and(safe_component),
            Component::ParentDir => false,
            _ => true,
        }) && path.has_root()
            && matches!(path.components().next(), Some(Component::Prefix(p))
                if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    }
    pub struct Directory {
        _file: File,
        identity: Identity,
        path: PathBuf,
    }
    impl Directory {
        pub fn open(path: &Path) -> Result<Self, &'static str> {
            if !local(path) {
                return Err("cwd_nonlocal_or_relative");
            }
            let file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
                .open(path)
                .map_err(|_| "cwd_directory_open")?;
            if !file.metadata().map_err(|_| "cwd_metadata")?.is_dir() {
                return Err("cwd_not_directory");
            }
            let mut info = FILE_ID_INFO::default();
            // SAFETY: owned live directory handle, exact aligned output buffer.
            unsafe {
                GetFileInformationByHandleEx(
                    HANDLE(file.as_raw_handle()),
                    FileIdInfo,
                    (&raw mut info).cast(),
                    std::mem::size_of_val(&info) as u32,
                )
            }
            .map_err(|_| "cwd_identity")?;
            Ok(Self {
                _file: file,
                path: path.to_owned(),
                identity: Identity {
                    volume: info.VolumeSerialNumber,
                    id: info.FileId.Identifier,
                },
            })
        }
        pub fn verify(&self, path: &Path) -> Result<(), &'static str> {
            let mut bytes = Vec::new();
            File::open(path)
                .map_err(|_| "cwd_report_open")?
                .take(8193)
                .read_to_end(&mut bytes)
                .map_err(|_| "cwd_report_read")?;
            let paths = report(&bytes)?;
            let process = Self::open(Path::new(paths[0]))?;
            let provider = Self::open(Path::new(paths[1]))?;
            let process_match = self.identity == process.identity;
            let provider_match = self.identity == provider.identity;
            let process_spelling_match = self.path.as_os_str() == paths[0];
            let provider_spelling_match = self.path.as_os_str() == paths[1];
            // No directory names, volume numbers or file IDs leave the fixture.
            println!(
                "conpty_cwd_identity=observed;process_match={process_match};provider_match={provider_match};process_spelling_match={process_spelling_match};provider_spelling_match={provider_spelling_match}"
            );
            if selected(self.identity, process.identity, provider.identity) {
                Ok(())
            } else {
                Err("cwd_directory_identity_mismatch")
            }
        }
    }
    #[test]
    fn only_absolute_local_drive_reports_can_reach_a_probe() {
        for path in [r"C:\fixture", r"\\?\C:\fixture", "C:/fixture"] {
            assert!(local(Path::new(path)));
        }
        for path in [
            r"\\fixture.invalid\share",
            r"\\.\pipe\fixture",
            "relative",
            r"C:relative",
            r"\rooted",
            r"C:\CON",
            r"C:\fixture\nul.txt",
            r"C:\fixture\..\other",
        ] {
            assert!(!local(Path::new(path)));
        }
    }
}
#[cfg(windows)]
pub use native::Directory;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserved_device_components_are_rejected() {
        for component in [
            "CON", "con.txt", "NUL", "AUX", "PRN", "COM1", "LPT9", "COM¹", "LPT²", "CON ",
            "CON .txt", "CONIN$",
        ] {
            assert!(!safe_component(component), "{component}");
        }
        for component in ["console", "COM0", "COM10", "fixture.txt"] {
            assert!(safe_component(component), "{component}");
        }
    }
    #[test]
    fn both_directory_views_require_the_full_same_identity() {
        let expected = Identity {
            volume: 7,
            id: [8; 16],
        };
        assert!(selected(expected, expected, expected));
        let mut high_bits = expected;
        high_bits.id[15] ^= 1;
        let other_volume = Identity {
            volume: 9,
            ..expected
        };
        for other in [high_bits, other_volume] {
            assert!(!selected(expected, other, expected));
            assert!(!selected(expected, expected, other));
            assert!(!selected(expected, other, other));
        }
    }
    #[test]
    fn report_retains_unicode_and_different_path_spellings() {
        let bytes = "C:/fixture/cwd space α\r\nC:\\fixture\\cwd space α\r\n".as_bytes();
        let paths = report(bytes).unwrap();
        assert_ne!(paths[0], paths[1]);
        assert!(paths.iter().all(|p| p.ends_with("cwd space α")));
    }
    #[test]
    fn malformed_or_oversized_reports_fail_closed() {
        for bytes in [
            b"".as_slice(),
            b"one",
            b"one\n\n",
            b"one\ntwo\nthree",
            b"one\ntw\0o",
            &[255],
        ] {
            assert!(report(bytes).is_err());
        }
        assert!(report(&vec![b'x'; 8193]).is_err());
        let long = format!("{}\nsecond", "α".repeat(1025));
        assert!(report(long.as_bytes()).is_err());
        let astral = format!("{}\nsecond", "\u{1f600}".repeat(513));
        assert!(report(astral.as_bytes()).is_err());
    }
}
