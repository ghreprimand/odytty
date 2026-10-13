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

    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command
            .arg(self.stage.join("install.sh"))
            .env("PREFIX", &self.prefix);
        for key in [
            "HOME",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "USERPROFILE",
        ] {
            command.env(key, &self.home);
        }
        command
    }

    fn run(&self, args: &[&str]) {
        let output = self
            .command()
            .args(args)
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

/// A custom prefix can contain other licence files. Uninstall removes only the
/// files the tarball installed and the directories that are then empty, never
/// a file or directory it did not put there.
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

/// Map a scratch prefix to the system-prefix identity at the canonicalization
/// boundary. This keeps a failing-before run entirely inside scratch storage,
/// while the real realpath still resolves dot components and symlinks.
#[cfg(target_os = "linux")]
fn assert_system_prefix_is_refused(args: &[&str], alias: bool) {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = TarballFixture::new();
    let paths = [
        "bin/odytty",
        "share/applications/io.unfinished_works.odytty.desktop",
        "share/metainfo/io.unfinished_works.odytty.metainfo.xml",
        "share/icons/hicolor/48x48/apps/io.unfinished_works.odytty.png",
        "share/licenses/odytty/LICENSE",
        "share/licenses/odytty/NOTICE",
        "share/licenses/odytty/THIRD-PARTY-LICENSES",
    ];
    let mut planted: Vec<PathBuf> = paths.iter().map(|path| fixture.prefix.join(path)).collect();
    for path in listed() {
        planted.push(
            fixture
                .licences()
                .join("fonts")
                .join(path.strip_prefix("assets/fonts/").expect("font path")),
        );
    }
    for path in &planted {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("package directory");
        std::fs::write(path, b"package-owned fixture\n").expect("package file");
    }
    let realpath = std::process::Command::new("sh")
        .args(["-c", "command -v realpath"])
        .output()
        .expect("resolve realpath");
    assert!(realpath.status.success());
    let tools = fixture.scratch.join("tools");
    std::fs::create_dir(&tools).expect("tools directory");
    let shim = tools.join("realpath");
    std::fs::write(&shim, "#!/bin/sh\nresolved=$(\"$FIXTURE_REALPATH\" \"$@\") || exit 1\nif [ \"$resolved\" = \"$FIXTURE_SYSTEM_PREFIX\" ]; then printf '/usr\\n'; else printf '%s\\n' \"$resolved\"; fi\n")
        .expect("canonicalization seam");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
        .expect("executable seam");
    let prefix = if alias {
        let link = fixture.scratch.join("prefix-alias");
        symlink(&fixture.prefix, &link).expect("prefix alias");
        link.join(".")
    } else {
        fixture.prefix.join(".")
    };
    let mut search = vec![tools];
    search.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    ));
    let output = fixture
        .command()
        .args(args)
        .env("PREFIX", prefix)
        .env(
            "PATH",
            std::env::join_paths(search).expect("tool search path"),
        )
        .env(
            "FIXTURE_REALPATH",
            String::from_utf8(realpath.stdout)
                .expect("realpath command")
                .trim(),
        )
        .env("FIXTURE_SYSTEM_PREFIX", &fixture.prefix)
        .output()
        .expect("run installer");
    // Check the planted files before the status, so the negative proof records
    // the overwritten binary or removed package file as the concrete failure.
    for path in &planted {
        assert_eq!(
            std::fs::read(path).ok().as_deref(),
            Some(b"package-owned fixture\n".as_slice()),
            "package-owned file must survive"
        );
    }
    assert!(!output.status.success(), "system prefix must be refused");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("reserved for package-managed installations")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_tarball_install_refuses_a_package_managed_prefix() {
    assert_system_prefix_is_refused(&[], false);
    assert_system_prefix_is_refused(&[], true);
}

#[cfg(target_os = "linux")]
#[test]
fn the_tarball_uninstall_refuses_a_package_managed_prefix() {
    assert_system_prefix_is_refused(&["--uninstall"], false);
    assert_system_prefix_is_refused(&["--uninstall"], true);
}

/// Fake merged-/usr tree: real symlinks stay in scratch storage, while the
/// canonicalization seam presents the protected subtree as /usr.
#[cfg(target_os = "linux")]
fn assert_destination_alias_is_refused(args: &[&str], directory: &str) {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = TarballFixture::new();
    let usr = fixture.scratch.join("usr");
    let icon = fixture
        .stage
        .join("share/icons/hicolor/48x48/apps/io.unfinished_works.odytty.png");
    std::fs::create_dir_all(icon.parent().expect("icon parent")).expect("icon source directory");
    std::fs::write(icon, b"synthetic icon fixture\n").expect("icon source");
    let package_dir = usr.join(directory);
    std::fs::create_dir_all(&package_dir).expect("package directory");
    let destination = fixture.prefix.join(directory);
    std::fs::create_dir_all(destination.parent().expect("parent")).expect("prefix parent");
    symlink(&package_dir, &destination).expect("merged directory alias");
    let package = usr.join("bin/odytty");
    std::fs::create_dir_all(package.parent().expect("binary directory")).expect("package bin");
    std::fs::write(&package, b"package-owned binary\n").expect("package binary");
    let local_binary = fixture.prefix.join("bin/odytty");
    if directory != "bin" {
        std::fs::create_dir_all(local_binary.parent().expect("local bin"))
            .expect("local directory");
        std::fs::write(&local_binary, b"local binary fixture\n").expect("local binary");
    }
    let tools = fixture.scratch.join("tools");
    std::fs::create_dir(&tools).expect("tool directory");
    let realpath = std::process::Command::new("sh")
        .args(["-c", "command -v realpath"])
        .output()
        .expect("resolve realpath");
    assert!(realpath.status.success());
    let shim = tools.join("realpath");
    std::fs::write(&shim, "#!/bin/sh\nresolved=$(\"$FIXTURE_REALPATH\" \"$@\") || exit 1\ncase \"$resolved\" in \"$FIXTURE_USR\"|\"$FIXTURE_USR\"/*) printf '/usr%s\\n' \"${resolved#\"$FIXTURE_USR\"}\";; \"$FIXTURE_ROOT\") printf '/\\n';; *) printf '%s\\n' \"$resolved\";; esac\n")
        .expect("realpath seam");
    std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o755))
        .expect("executable seam");
    let mut search = vec![tools];
    search.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    ));
    let output = fixture
        .command()
        .args(args)
        .env("PATH", std::env::join_paths(search).expect("tool path"))
        .env(
            "FIXTURE_REALPATH",
            String::from_utf8(realpath.stdout).expect("realpath").trim(),
        )
        .env("FIXTURE_USR", &usr)
        .env("FIXTURE_ROOT", &fixture.prefix)
        .output()
        .expect("run installer");
    assert_eq!(
        std::fs::read(&package).ok().as_deref(),
        Some(b"package-owned binary\n".as_slice()),
        "merged-/usr binary must survive"
    );
    assert!(
        !output.status.success(),
        "protected destination alias must be refused"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("reserved for package-managed installations")
    );
    if directory != "bin" {
        assert_eq!(
            std::fs::read(local_binary).ok().as_deref(),
            Some(b"local binary fixture\n".as_slice()),
            "refusal must precede any binary modification"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_tarball_install_refuses_merged_usr_and_data_directory_aliases() {
    let font_set = format!(
        "share/licenses/odytty/fonts/{}",
        Path::new(&listed()[0])
            .strip_prefix("assets/fonts")
            .expect("font path")
            .parent()
            .expect("font set")
            .display()
    );
    for directory in [
        "bin",
        "share",
        "share/applications",
        "share/metainfo",
        "share/icons/hicolor",
        "share/licenses/odytty/fonts",
        "share/icons/hicolor/48x48/apps",
        &font_set,
    ] {
        assert_destination_alias_is_refused(&[], directory);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_tarball_uninstall_refuses_merged_usr_and_data_directory_aliases() {
    let font_set = format!(
        "share/licenses/odytty/fonts/{}",
        Path::new(&listed()[0])
            .strip_prefix("assets/fonts")
            .expect("font path")
            .parent()
            .expect("font set")
            .display()
    );
    for directory in [
        "bin",
        "share",
        "share/applications",
        "share/metainfo",
        "share/icons/hicolor",
        "share/licenses/odytty/fonts",
        "share/icons/hicolor/48x48/apps",
        &font_set,
    ] {
        assert_destination_alias_is_refused(&["--uninstall"], directory);
    }
}
