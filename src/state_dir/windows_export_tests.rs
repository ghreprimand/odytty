// SPDX-License-Identifier: GPL-3.0-only
//! Export ownership and reparse behavior on a Windows filesystem.

use super::*;

#[test]
fn an_export_rejects_a_file_symlink_and_preserves_its_target() {
    let root = crate::test_dirs::ScratchDir::new("windows-export-link-");
    let original = root.path().join("original.txt");
    let link = root.path().join("export.txt");
    fs::write(&original, b"keep").expect("original");
    if let Err(error) = std::os::windows::fs::symlink_file(&original, &link) {
        // Local Windows runs can lack the symlink privilege. CI must execute
        // the assertion, rather than treating missing apparatus as a pass.
        assert!(
            std::env::var_os("GITHUB_ACTIONS").is_none(),
            "Windows CI must create the reparse fixture: {error}"
        );
        assert_eq!(error.raw_os_error(), Some(1314), "unexpected link error");
        eprintln!("VALIDATION_SKIP test=windows_export_file_symlink reason=symlink-privilege");
        return;
    }
    let error = write_atomic(&link, b"overwrite", WriteMode::Export)
        .expect_err("a final-component reparse point must be refused");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read(&original).expect("original contents"), b"keep");
    assert!(
        fs::symlink_metadata(&link)
            .expect("link metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_dir(root.path()).expect("directory").count(), 2);
    eprintln!("VALIDATION_EXECUTED test=windows_export_file_symlink");
}

#[test]
fn an_export_identity_swap_preserves_both_files_and_removes_its_temporary() {
    let root = crate::test_dirs::ScratchDir::new("windows-export-race-");
    let target = root.path().join("export.txt");
    let original = root.path().join("moved-original.txt");
    fs::write(&target, b"original").expect("original");
    let error =
        write_atomic_before_export_check(&target, b"must not publish", WriteMode::Export, |path| {
            fs::rename(path, &original).expect("move original object aside");
            fs::write(path, b"replacement").expect("install different object");
        })
        .expect_err("a different file identity must cancel the export");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        error.to_string(),
        "export destination changed before replacement"
    );
    assert_eq!(
        fs::read(&target).expect("replacement contents"),
        b"replacement"
    );
    assert_eq!(fs::read(&original).expect("original contents"), b"original");
    assert_eq!(fs::read_dir(root.path()).expect("directory").count(), 2);
}

#[test]
fn an_export_to_an_existing_windows_file_replaces_only_that_file() {
    let root = crate::test_dirs::ScratchDir::new("windows-export-replace-");
    let target = root.path().join("export.txt");
    let bystander = root.path().join("bystander.txt");
    fs::write(&target, b"old").expect("old file");
    fs::write(&bystander, b"keep").expect("bystander");
    write_atomic(&target, b"new", WriteMode::Export).expect("replace existing export");
    assert_eq!(fs::read(&target).expect("published bytes"), b"new");
    assert_eq!(fs::read(&bystander).expect("bystander bytes"), b"keep");
    assert_eq!(fs::read_dir(root.path()).expect("directory").count(), 2);
}
