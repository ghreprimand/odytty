// SPDX-License-Identifier: GPL-3.0-only
//! Font discovery bounds, scan order, and symlinked font files.

use std::fs;
use std::path::{Path, PathBuf};

use super::discovery::{FontScanLimits, collect_font_files, collect_font_files_bounded};

struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("odytty-font-scan-"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn touch(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture directory");
    }
    fs::write(path, b"x").expect("write fixture file");
}

fn names(files: &[PathBuf], root: &Path) -> Vec<String> {
    files
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .expect("under fixture root")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn examined_entries_are_bounded_even_when_none_are_fonts() {
    let tree = TempTree::new();
    for index in 0..60 {
        touch(&tree.path().join(format!("note-{index:02}.txt")));
    }
    touch(&tree.path().join("zz-late.ttf"));
    let limits = FontScanLimits {
        entries: 25,
        ..FontScanLimits::DEFAULT
    };
    let scan = collect_font_files_bounded(&[tree.path().to_owned()], limits);
    assert!(scan.truncated, "a scan that stops early reports it");
    assert!(
        scan.files.len() <= 1,
        "at most the 25 examined entries are considered"
    );

    let full = collect_font_files_bounded(&[tree.path().to_owned()], FontScanLimits::DEFAULT);
    assert!(!full.truncated);
    assert_eq!(names(&full.files, tree.path()), vec!["zz-late.ttf"]);
}

#[test]
fn directories_read_are_bounded() {
    let tree = TempTree::new();
    for index in 0..12 {
        fs::create_dir_all(tree.path().join(format!("d{index:02}"))).expect("mkdir");
    }
    touch(&tree.path().join("d11/late.otf"));
    let limits = FontScanLimits {
        dirs: 4,
        ..FontScanLimits::DEFAULT
    };
    let scan = collect_font_files_bounded(&[tree.path().to_owned()], limits);
    assert!(scan.truncated);
    assert!(scan.files.is_empty(), "d11 is past the directory bound");
}

#[test]
fn file_bound_reports_truncation_only_when_more_fonts_exist() {
    let tree = TempTree::new();
    for name in ["a.ttf", "b.ttf", "c.ttf"] {
        touch(&tree.path().join(name));
    }
    let exact = FontScanLimits {
        files: 3,
        ..FontScanLimits::DEFAULT
    };
    let scan = collect_font_files_bounded(&[tree.path().to_owned()], exact);
    assert_eq!(scan.files.len(), 3);
    assert!(!scan.truncated, "exactly the bound is not a truncation");

    let smaller = FontScanLimits {
        files: 2,
        ..FontScanLimits::DEFAULT
    };
    let scan = collect_font_files_bounded(&[tree.path().to_owned()], smaller);
    assert_eq!(names(&scan.files, tree.path()), vec!["a.ttf", "b.ttf"]);
    assert!(scan.truncated);
}

#[test]
fn scan_order_is_stable_and_by_name() {
    let tree = TempTree::new();
    for name in [
        "zeta.ttf",
        "alpha.otf",
        "sub/beta.ttc",
        "sub/aardvark.ttf",
        "mid.ttf",
        "sub/deeper/gamma.ttf",
    ] {
        touch(&tree.path().join(name));
    }
    let first = collect_font_files(&[tree.path().to_owned()]);
    let second = collect_font_files(&[tree.path().to_owned()]);
    assert_eq!(first, second);
    assert_eq!(
        names(&first, tree.path()),
        vec![
            "alpha.otf",
            "mid.ttf",
            "zeta.ttf",
            "sub/aardvark.ttf",
            "sub/beta.ttc",
            "sub/deeper/gamma.ttf",
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_to_a_regular_font_file_is_discovered() {
    use std::os::unix::fs::symlink;
    let tree = TempTree::new();
    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");
    assert!(target.is_file(), "bundled regular face present");
    let fonts = tree.path().join("fonts");
    fs::create_dir(&fonts).expect("mkdir");
    symlink(&target, fonts.join("JetBrainsMono-Regular.ttf")).expect("symlink font");

    let found = collect_font_files(std::slice::from_ref(&fonts));
    assert_eq!(names(&found, &fonts), vec!["JetBrainsMono-Regular.ttf"]);

    let resolved = super::try_resolve_font_family("JetBrains Mono", std::slice::from_ref(&fonts))
        .expect("a symlink-only family resolves");
    assert_eq!(resolved.regular, fonts.join("JetBrainsMono-Regular.ttf"));
}

#[cfg(unix)]
#[test]
fn directory_and_dangling_symlinks_are_not_followed() {
    use std::os::unix::fs::symlink;
    let tree = TempTree::new();
    let root = tree.path().join("root");
    let outside = tree.path().join("outside");
    touch(&outside.join("elsewhere.ttf"));
    fs::create_dir(&root).expect("mkdir");
    touch(&root.join("inside.ttf"));
    symlink(&outside, root.join("linked-dir")).expect("dir symlink");
    symlink(&root, root.join("loop")).expect("loop symlink");
    symlink(root.join("missing.ttf"), root.join("dangling.ttf")).expect("dangling symlink");
    symlink(&outside, root.join("dir-named.ttf")).expect("dir symlink with font name");

    let scan = collect_font_files_bounded(std::slice::from_ref(&root), FontScanLimits::DEFAULT);
    assert!(!scan.truncated);
    assert_eq!(names(&scan.files, &root), vec!["inside.ttf"]);
}
