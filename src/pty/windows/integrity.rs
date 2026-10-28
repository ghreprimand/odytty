// SPDX-License-Identifier: GPL-3.0-only
//! Offline identity and lifetime binding of the three shipped native files.
use super::*;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use windows::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
    BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};

pub(super) const DIRECTORY: &str = "runtime/conpty/1.25.260930003";
const MEMBERS: [(&str, u64, &str); 3] = [
    (
        "conpty.dll",
        119608,
        "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a",
    ),
    (
        "x64/OpenConsole.exe",
        1089376,
        "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586",
    ),
    (
        "arm64/OpenConsole.exe",
        1149752,
        "4313825a3ba634d113cc6ed3bc8ce735e3c19d360cecd985794787479469f91f",
    ),
];

pub(super) struct BoundRuntime {
    pub dll: PathBuf,
    // Denying write/delete sharing pins files and every runtime directory until
    // process exit. Install/upgrade always replaces the complete closed package.
    pub handles: Vec<File>,
}

fn held_path(file: &File) -> Result<PathBuf> {
    let mut wide = [0u16; 1024];
    // SAFETY: file owns a live handle; fixed buffer and returned length checked.
    let count = unsafe {
        GetFinalPathNameByHandleW(HANDLE(file.as_raw_handle()), &mut wide, VOLUME_NAME_DOS)
    } as usize;
    anyhow::ensure!(
        count > 0 && count < wide.len(),
        "runtime handle path unavailable"
    );
    Ok(PathBuf::from(OsString::from_wide(&wide[..count])))
}
fn open_bound(path: &Path, directory: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
            && metadata.is_dir() == directory,
        "runtime member kind or reparse point"
    );
    anyhow::ensure!(
        held_path(&file)? == path.canonicalize()?,
        "runtime handle path mismatch"
    );
    Ok(file)
}
fn exact_entries(path: &Path, expected: &[&str]) -> Result<()> {
    let mut actual = Vec::new();
    for entry in std::fs::read_dir(path)? {
        anyhow::ensure!(actual.len() < expected.len(), "unexpected runtime entry");
        actual.push(entry?.file_name());
    }
    actual.sort();
    let mut expected: Vec<_> = expected.iter().map(OsString::from).collect();
    expected.sort();
    anyhow::ensure!(
        actual == expected,
        "incomplete or unexpected runtime entries"
    );
    Ok(())
}

pub(super) fn sha256(bytes: &[u8]) -> Result<String> {
    struct Algorithm(BCRYPT_ALG_HANDLE);
    impl Drop for Algorithm {
        fn drop(&mut self) {
            // SAFETY: exactly one owner of a successful algorithm-open result.
            let _ = unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
        }
    }
    let mut handle = BCRYPT_ALG_HANDLE::default();
    // SAFETY: live output slot, built-in algorithm name, no secret or provider.
    unsafe {
        BCryptOpenAlgorithmProvider(
            &mut handle,
            BCRYPT_SHA256_ALGORITHM,
            None,
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
        .ok()?;
    }
    let algorithm = Algorithm(handle);
    let mut digest = [0; 32];
    // SAFETY: slices describe their complete allocated buffers.
    unsafe {
        BCryptHash(algorithm.0, None, bytes, &mut digest).ok()?;
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) fn layout(base: &Path) -> Result<()> {
    exact_entries(base, &["conpty.dll", "x64", "arm64"])?;
    for arch in ["x64", "arm64"] {
        exact_entries(&base.join(arch), &["OpenConsole.exe"])?;
    }
    Ok(())
}

pub(super) fn bind(parent: &Path) -> Result<BoundRuntime> {
    let parent = parent.canonicalize()?;
    let mut handles = Vec::new();
    let mut base = parent.clone();
    for component in ["runtime", "conpty", "1.25.260930003"] {
        base.push(component);
        handles.push(open_bound(&base, true)?);
    }
    for arch in ["x64", "arm64"] {
        handles.push(open_bound(&base.join(arch), true)?);
    }
    layout(&base)?;
    let dll = base.join("conpty.dll");
    // Keep every path within the fixed observation buffers used by this
    // loader. The publisher formats its own host command on the heap.
    for (member, size, hash) in MEMBERS {
        let path = base.join(member);
        anyhow::ensure!(
            path.as_os_str().encode_wide().count() < 1024,
            "runtime path too long"
        );
        let mut file = open_bound(&path, false)?;
        anyhow::ensure!(
            file.metadata()?.len() == size,
            "runtime member size mismatch"
        );
        let mut bytes = Vec::new();
        (&mut file).take(size + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 == size && sha256(&bytes)? == hash,
            "runtime member hash mismatch"
        );
        handles.push(file);
    }
    anyhow::ensure!(base == parent.join(DIRECTORY), "runtime path mismatch");
    Ok(BoundRuntime { dll, handles })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> crate::test_dirs::ScratchDir {
        let scratch = crate::test_dirs::ScratchDir::new("conpty-integrity-");
        let source = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(DIRECTORY);
        let target = scratch.path().join(DIRECTORY);
        for arch in ["x64", "arm64"] {
            std::fs::create_dir_all(target.join(arch)).unwrap();
        }
        for (member, _, _) in MEMBERS {
            std::fs::copy(source.join(member), target.join(member)).unwrap();
        }
        scratch
    }
    #[test]
    fn offline_hash_matches_standard_known_answer() {
        assert_eq!(
            sha256(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
    #[test]
    fn verified_files_and_directories_deny_mutation_until_released() {
        let scratch = fixture();
        let bound = bind(scratch.path()).unwrap();
        assert!(OpenOptions::new().write(true).open(&bound.dll).is_err());
        assert!(std::fs::remove_file(&bound.dll).is_err());
        assert!(
            std::fs::rename(scratch.path().join("runtime"), scratch.path().join("moved")).is_err()
        );
        let host = scratch.path().join(DIRECTORY).join("x64/OpenConsole.exe");
        assert!(OpenOptions::new().write(true).open(&host).is_err());
        assert!(std::fs::remove_file(&host).is_err());
        drop(bound);
        std::fs::remove_file(host).unwrap();
    }
    #[test]
    fn already_writable_native_file_cannot_be_bound() {
        let scratch = fixture();
        let host = scratch.path().join(DIRECTORY).join("x64/OpenConsole.exe");
        let writer = OpenOptions::new().write(true).open(&host).unwrap();
        assert!(bind(scratch.path()).is_err());
        drop(writer);
        assert!(bind(scratch.path()).is_ok());
    }
    #[test]
    fn missing_changed_shadow_and_unexpected_members_fail_closed() {
        for member in ["conpty.dll", "x64/OpenConsole.exe", "arm64/OpenConsole.exe"] {
            let scratch = fixture();
            let path = scratch.path().join(DIRECTORY).join(member);
            std::fs::remove_file(&path).unwrap();
            assert!(bind(scratch.path()).is_err());
            std::fs::write(&path, b"wrong size").unwrap();
            assert!(bind(scratch.path()).is_err());
        }
        for extra in ["OpenConsole.exe", "unexpected.dll", "x64/extra.dll"] {
            let scratch = fixture();
            std::fs::write(scratch.path().join(DIRECTORY).join(extra), b"shadow").unwrap();
            assert!(bind(scratch.path()).is_err());
        }
        let scratch = fixture();
        let path = scratch.path().join(DIRECTORY).join("conpty.dll");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[64] ^= 1;
        std::fs::write(path, bytes).unwrap();
        assert!(bind(scratch.path()).is_err());
    }
}
