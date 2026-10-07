// SPDX-License-Identifier: GPL-3.0-only
//! Discovery root identity, depth reporting, and injected Linux data roots.

use super::*;

struct TempTree(PathBuf);
impl TempTree {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("font-roots"))
    }
    fn font(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).expect("create fixture directories");
        std::fs::write(&path, b"project-authored discovery marker").expect("write fixture marker");
        path
    }
}
impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn depth_truncation_is_reported_without_skipping_eligible_siblings() {
    let tree = TempTree::new();
    tree.font("a/deep/missing.ttf");
    let visible = tree.font("z/visible.ttf");
    let scan = collect_font_files_bounded(
        std::slice::from_ref(&tree.0),
        FontScanLimits {
            depth: 1,
            ..FontScanLimits::DEFAULT
        },
    );
    assert!(scan.truncated, "depth refusal reports truncation");
    assert_eq!(
        scan.files,
        vec![visible],
        "shallower siblings still scanned"
    );
}

#[test]
fn overlapping_roots_do_not_duplicate_files_or_exhaust_the_file_limit() {
    let tree = TempTree::new();
    let a = tree.font("sub/a.ttf");
    let b = tree.font("sub/b.otf");
    let scan = collect_font_files_bounded(
        &[tree.0.clone(), tree.0.join("sub")],
        FontScanLimits {
            files: 2,
            ..FontScanLimits::DEFAULT
        },
    );
    assert_eq!(scan.files, vec![a, b]);
    assert!(!scan.truncated, "duplicates do not consume file admission");
}

#[cfg(unix)]
#[test]
fn aliased_roots_and_file_symlinks_are_admitted_once() {
    use std::os::unix::fs::symlink;
    let tree = TempTree::new();
    let root = tree.0.join("fonts");
    let target = tree.font("fonts/a.ttf");
    symlink(&root, tree.0.join("alias")).expect("alias root");
    symlink(&target, root.join("z.ttf")).expect("alias font");
    let scan = collect_font_files_bounded(
        &[root, tree.0.join("alias")],
        FontScanLimits {
            files: 1,
            ..FontScanLimits::DEFAULT
        },
    );
    assert_eq!(scan.files, vec![tree.0.join("alias/a.ttf")]);
    assert!(!scan.truncated);
}

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn linux_data_roots_honor_relocated_home_and_configured_system_order() {
    let tree = TempTree::new();
    let home = tree.0.join("home");
    let data = tree.0.join("data");
    let high = tree.0.join("high");
    let low = tree.0.join("low");
    let dirs = std::env::join_paths([&high, &low]).expect("data root list");
    let roots = linux_font_roots(Some(&home), Some(data.as_os_str()), Some(&dirs));
    assert!(roots.contains(&data.join("fonts")));
    assert!(!roots.contains(&home.join(".local/share/fonts")));
    let low_index = roots
        .iter()
        .position(|path| *path == low.join("fonts"))
        .unwrap();
    let high_index = roots
        .iter()
        .position(|path| *path == high.join("fonts"))
        .unwrap();
    assert!(
        low_index < high_index,
        "configured first root has higher scan priority"
    );
    assert_eq!(roots.last(), Some(&home.join(".fonts")));
}

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn linux_data_roots_ignore_relative_values_and_keep_standard_defaults() {
    let tree = TempTree::new();
    let relative = std::ffi::OsStr::new("relative");
    let roots = linux_font_roots(Some(&tree.0), Some(relative), Some(relative));
    assert!(roots.contains(&tree.0.join(".local/share/fonts")));
    assert!(roots.contains(&PathBuf::from("/usr/local/share/fonts")));
    assert!(roots.contains(&PathBuf::from("/usr/share/fonts")));
    assert!(!roots.iter().any(|path| path.starts_with("relative")));
}
