// SPDX-License-Identifier: GPL-3.0-only
//! The bundled fonts are embedded in the binary, and the SIL Open Font License
//! and the other font licences require their notices to accompany every copy.
//! `dist/font-licenses.txt` names those files once; every package that ships
//! the binary must carry them. This suite keeps the list equal to the licence
//! files in `assets/fonts` and each package definition in step with the list.
//!
//! Platform behavior: pure file reads on every OS. The install helper is a
//! bash script, so the copy test runs on Unix only; the Windows zip step reads
//! the same list in PowerShell and is checked here as text.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(unix)]
#[path = "../src/test_dirs/allocation.rs"]
pub mod scratch_dirs;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn listed() -> Vec<String> {
    read("dist/font-licenses.txt")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// Every file under `assets/fonts` that is not a font program.
fn licence_files_on_disk() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read fonts dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(extension, "ttf" | "otf" | "woff" | "woff2") {
                let relative = path
                    .strip_prefix(root())
                    .expect("under the repository")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(relative);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&root().join("assets/fonts"), &mut out);
    out
}

#[test]
fn the_list_is_exactly_the_licence_files_in_the_font_directory() {
    let listed: BTreeSet<String> = listed().into_iter().collect();
    assert_eq!(
        listed,
        licence_files_on_disk(),
        "a licence file in assets/fonts is missing from dist/font-licenses.txt (or the reverse)"
    );
    for path in &listed {
        assert!(
            std::fs::metadata(root().join(path)).is_ok_and(|m| m.len() > 0),
            "{path} is empty"
        );
    }
}

#[test]
fn the_deb_and_rpm_ship_every_listed_file() {
    let manifest = read("Cargo.toml");
    let deb = manifest
        .split("[package.metadata.deb]")
        .nth(1)
        .and_then(|rest| rest.split("[package.metadata.generate-rpm]").next())
        .expect("deb section");
    let rpm = manifest
        .split("[package.metadata.generate-rpm]")
        .nth(1)
        .and_then(|rest| {
            rest.split("[package.metadata.generate-rpm.requires]")
                .next()
        })
        .expect("rpm section");
    for path in listed() {
        let relative = path.strip_prefix("assets/fonts/").expect("font path");
        assert!(
            deb.contains(&format!(
                "[\"{path}\", \"usr/share/doc/odytty/licenses/fonts/{relative}\", \"644\"]"
            )),
            "the deb does not ship {path}"
        );
        assert!(
            rpm.contains(&format!(
                "{{ source = \"{path}\", dest = \"/usr/share/licenses/odytty/fonts/{relative}\", mode = \"644\" }}"
            )),
            "the rpm does not ship {path}"
        );
    }
    assert!(deb.contains("[\"LICENSE\", \"usr/share/doc/odytty/LICENSE\", \"644\"]"));
}

#[test]
fn every_other_package_calls_the_install_helper_or_the_list() {
    for (file, needle) in [
        ("dist/aur/PKGBUILD", "bash dist/install-font-licenses.sh"),
        ("dist/macos/make-app.sh", "install-font-licenses.sh"),
        (
            "dist/appimage/build-appimage.sh",
            "install-font-licenses.sh",
        ),
        (
            ".github/workflows/release.yml",
            "bash dist/install-font-licenses.sh \"dist/build/${stage}/licenses/fonts\"",
        ),
        (
            ".github/workflows/release.yml",
            "Get-Content dist/font-licenses.txt",
        ),
        (
            ".github/workflows/dev-build.yml",
            "Get-Content dist/font-licenses.txt",
        ),
    ] {
        assert!(
            read(file).contains(needle),
            "{file} does not ship the font licences"
        );
    }
    // The AUR package and the macOS app also carry the notices themselves.
    assert!(read("dist/aur/PKGBUILD").contains("install -Dm644 NOTICE"));
    assert!(read("dist/macos/make-app.sh").contains("\"$ROOT/NOTICE\""));
}

#[cfg(unix)]
#[test]
fn the_install_helper_lays_the_files_out_by_font_set() {
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(scratch_dirs::fresh_temp_dir("odytty-font-licenses-"));
    let dest = scratch.0.join("out");
    let status = std::process::Command::new("bash")
        .arg(root().join("dist/install-font-licenses.sh"))
        .arg(&dest)
        .status()
        .expect("run the helper");
    assert!(status.success());
    for path in listed() {
        let relative = path.strip_prefix("assets/fonts/").expect("font path");
        let copied = dest.join(relative);
        assert_eq!(
            std::fs::read(&copied).expect("copied file"),
            std::fs::read(root().join(&path)).expect("source file"),
            "{relative}"
        );
    }
}
