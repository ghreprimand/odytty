// SPDX-License-Identifier: GPL-3.0-only
//! The bundled fonts are embedded in the binary, and the SIL Open Font License
//! and the other font licences require their notices to accompany every copy.
//! `dist/font-licenses.txt` names those files once; every package that ships
//! the binary must carry them. This suite keeps the list equal to the licence
//! files in `assets/fonts` and each package definition in step with the list.
//!
//! Platform behavior: pure file reads on every OS. The install helper is a
//! bash script, so the copy test runs on Unix only; the tarball installer test
//! runs on Linux only, where the tarball ships; the Windows zip step reads the
//! same list in PowerShell and is checked here as text.

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

/// An extracted tarball laid out the way the release job assembles it, with a
/// stand-in binary, and a scratch prefix and home to install into. Linux only:
/// the tarball and its `install.sh` ship only for Linux, and the script relies
/// on GNU `install -D` creating parent directories, which BSD `install` on
/// macOS does not do.
#[cfg(target_os = "linux")]
struct TarballFixture {
    scratch: PathBuf,
    stage: PathBuf,
    prefix: PathBuf,
    home: PathBuf,
}

#[cfg(target_os = "linux")]
impl Drop for TarballFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

#[cfg(target_os = "linux")]
impl TarballFixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let scratch = scratch_dirs::fresh_temp_dir("odytty-tarball-install-");
        let stage = scratch.join("odytty-linux-x86_64");
        std::fs::create_dir_all(&stage).expect("stage");
        std::fs::copy(
            root().join("dist/linux/tarball-install.sh"),
            stage.join("install.sh"),
        )
        .expect("installer");
        std::fs::write(stage.join("odytty"), "#!/bin/sh\n").expect("stand-in binary");
        std::fs::set_permissions(stage.join("odytty"), std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        for notice in ["LICENSE", "NOTICE", "THIRD-PARTY-LICENSES"] {
            std::fs::copy(root().join(notice), stage.join(notice)).expect("notice");
        }
        let status = std::process::Command::new("bash")
            .arg(root().join("dist/install-font-licenses.sh"))
            .arg(stage.join("licenses/fonts"))
            .status()
            .expect("run the helper");
        assert!(status.success());
        Self {
            prefix: scratch.join("prefix"),
            home: scratch.join("home"),
            scratch,
            stage,
        }
    }

    fn licences(&self) -> PathBuf {
        self.prefix.join("share/licenses/odytty")
    }

    fn run(&self, args: &[&str]) {
        let output = std::process::Command::new("sh")
            .arg(self.stage.join("install.sh"))
            .args(args)
            .env("PREFIX", &self.prefix)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .output()
            .expect("run the tarball installer");
        assert!(output.status.success(), "{output:?}");
    }
}

/// The tarball's own installer (which the one-line installer's tarball branch
/// runs) copies LICENSE, NOTICE, THIRD-PARTY-LICENSES and the font licences to
/// `PREFIX/share/licenses/odytty`, and `--uninstall` removes them.
#[cfg(target_os = "linux")]
#[test]
fn the_tarball_installer_installs_and_removes_the_licence_texts() {
    let fixture = TarballFixture::new();
    fixture.run(&[]);
    let licences = fixture.licences();
    for notice in ["LICENSE", "NOTICE", "THIRD-PARTY-LICENSES"] {
        assert_eq!(
            std::fs::read(licences.join(notice)).expect("installed notice"),
            std::fs::read(root().join(notice)).expect("source notice"),
            "{notice}"
        );
    }
    for path in listed() {
        let relative = path.strip_prefix("assets/fonts/").expect("font path");
        assert_eq!(
            std::fs::read(licences.join("fonts").join(relative)).expect("installed licence"),
            std::fs::read(root().join(&path)).expect("source licence"),
            "{relative}"
        );
    }
    assert!(fixture.prefix.join("bin/odytty").is_file());

    fixture.run(&["--uninstall"]);
    assert!(!licences.exists(), "uninstall removes the licence texts");
    assert!(!fixture.prefix.join("bin/odytty").exists());
}

/// With `PREFIX=/usr` the licence directory can be shared with a deb, rpm or
/// AUR package. Uninstall removes only the files the tarball installed and the
/// directories that are then empty, never a file or directory it did not put
/// there.
#[cfg(target_os = "linux")]
#[test]
fn the_tarball_uninstall_keeps_licence_files_it_did_not_install() {
    let fixture = TarballFixture::new();
    let licences = fixture.licences();
    let foreign_file = licences.join("COPYING.packaged");
    let foreign_font_file = licences.join("fonts").join("PackagedFont-OFL.txt");
    let foreign_font_dir_file = licences
        .join("fonts")
        .join("packaged-set")
        .join("LICENSE.txt");
    for foreign in [&foreign_file, &foreign_font_file, &foreign_font_dir_file] {
        std::fs::create_dir_all(foreign.parent().expect("parent")).expect("foreign dir");
        std::fs::write(foreign, "owned by another package\n").expect("foreign file");
    }

    fixture.run(&[]);
    fixture.run(&["--uninstall"]);

    for foreign in [&foreign_file, &foreign_font_file, &foreign_font_dir_file] {
        assert_eq!(
            std::fs::read_to_string(foreign).expect("a foreign file survives uninstall"),
            "owned by another package\n",
            "{foreign:?}"
        );
    }
    for notice in ["LICENSE", "NOTICE", "THIRD-PARTY-LICENSES"] {
        assert!(!licences.join(notice).exists(), "{notice} is removed");
    }
    for path in listed() {
        let relative = path.strip_prefix("assets/fonts/").expect("font path");
        assert!(
            !licences.join("fonts").join(relative).exists(),
            "{relative} is removed"
        );
    }
    assert!(!fixture.prefix.join("bin/odytty").exists());
}
