// SPDX-License-Identifier: GPL-3.0-only
//! Offline contracts run on every OS. Native execution is Windows-only.
use std::path::PathBuf;
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap()
}
#[test]
fn all_windows_builds_delegate_to_the_verified_fixed_runtime() {
    for workflow in ["ci.yml", "dev-build.yml", "release.yml"] {
        let text = read(&format!(".github/workflows/{workflow}"));
        assert!(
            text.contains("scripts/conpty-runtime/Prepare.ps1"),
            "{workflow}"
        );
        assert!(
            text.contains("scripts/conpty-runtime/Stage.ps1"),
            "{workflow}"
        );
        assert!(text.contains("dotnet-version: '10.0.100'"), "{workflow}");
        if workflow != "ci.yml" {
            assert!(text.contains("scripts/conpty-runtime/Package.ps1"));
        }
    }
    let ci = read(".github/workflows/ci.yml");
    for directory in ["'target/debug'", "'target/debug/deps'", "'target/release'"] {
        assert!(ci.contains(directory));
    }
    assert!(ci.contains("'conpty_graphics_transport'"));
    assert!(ci.contains("cargo test --locked --test $group"));
    let scoop = read("bucket/odytty.json");
    assert!(scoop.contains("\"bin\": \"odytty.exe\""));
    assert!(!scoop.contains("extract_dir"));
}
#[test]
fn inventory_is_complete_and_does_not_invent_binary_source_provenance() {
    let inventory = read("licenses/conpty/runtime-manifest.json");
    assert!(inventory.contains("\"compiled_source_commit\": null"));
    assert!(inventory.contains("\"publisher_sbom\": null"));
    let pin = read("scripts/conpty-runtime/pin.json");
    for hash in [
        "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a",
        "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586",
        "4313825a3ba634d113cc6ed3bc8ce735e3c19d360cecd985794787479469f91f",
    ] {
        assert!(pin.contains(hash));
        assert!(inventory.contains(hash));
        assert!(read("src/pty/windows/integrity.rs").contains(hash));
    }
    for (notice, size) in [
        ("LICENSE-MICROSOFT.txt", 1116),
        ("THIRD-PARTY-NOTICES.md", 23176),
    ] {
        assert!(inventory.contains(notice));
        assert_eq!(
            std::fs::metadata(root().join("licenses/conpty").join(notice))
                .unwrap()
                .len(),
            size
        );
    }
    assert!(read("NOTICE").contains("licenses/conpty/THIRD-PARTY-NOTICES.md"));
}

#[test]
fn default_provider_has_no_inbox_or_environment_override() {
    let source = read("src/pty/windows.rs");
    let begin = source.find("    pub fn spawn_command(").unwrap();
    let end = source[begin..].find("    fn spawn_with_provider(").unwrap() + begin;
    assert!(source[begin..end].contains("Provider::bundled()?"));
    let runtime = read("src/pty/windows/runtime.rs");
    assert!(!runtime.contains("Provider::System"));
    assert!(!runtime.contains("std::env::var"));
    assert!(runtime.contains("LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32"));
    assert!(!source.contains("spawn_qualified_conpty"));
    assert!(!read("build.rs").contains("conpty-runtime/Prepare"));
}

#[path = "support/conpty_cleanup.rs"]
mod conpty_cleanup;
#[path = "support/conpty_replies.rs"]
mod conpty_replies;

#[test]
fn host_fixture_selects_utf8_before_all_console_io() {
    let source = read("tests/conpty_graphics_transport.rs");
    let script = source
        .split("let script = r#\"")
        .nth(1)
        .expect("native host fixture")
        .split("\"#;")
        .next()
        .unwrap()
        .trim_start();
    assert!(
        script.starts_with(
            "[Console]::OutputEncoding=[Console]::InputEncoding=[Text.UTF8Encoding]::new($false);"
        ),
        "native fixture must set UTF8 before all console I/O"
    );
    assert!(script.contains("[Console]::ReadLine()"));
    assert!(script.contains("ENV_VALUE="));
}

#[path = "support/conpty_host.rs"]
mod conpty_host;

#[test]
fn host_identity_uses_private_job_and_file_handles_without_window_proxy() {
    #[cfg(windows)]
    let _native_check: fn(
        windows::Win32::Foundation::HANDLE,
        &std::path::Path,
    ) -> Result<(), String> = conpty_host::verify;
    let source = read("tests/conpty_graphics_transport.rs");
    assert!(!source.contains("GetConsoleWindow"));
    assert!(!source.contains("GetWindowThreadProcessId"));
    assert!(source.contains("conpty_host::verify(job"));
    let identity = read("tests/support/conpty_host.rs");
    for api in [
        "QueryInformationJobObject",
        "IsProcessInJob",
        "GetFileInformationByHandleEx",
    ] {
        assert!(identity.contains(api));
    }
    assert!(!identity.contains("CreateToolhelp32Snapshot"));
}
