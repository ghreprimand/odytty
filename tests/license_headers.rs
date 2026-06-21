// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::path::{Path, PathBuf};

const SPDX: &str = "// SPDX-License-Identifier: GPL-3.0-only";

#[test]
fn source_files_carry_gpl_spdx_header() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_source_files(&root.join("src"), &mut files);
    collect_rust_files(&root.join("tests"), &mut files);
    collect_rust_files(&root.join("benches"), &mut files);
    files.sort();

    let missing = files
        .iter()
        .filter(|path| first_line(path).as_deref() != Some(SPDX))
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "missing GPL SPDX header on line 1:\n{}",
        missing.join("\n")
    );
}

fn collect_source_files(dir: &Path, files: &mut Vec<PathBuf>) {
    collect_matching_files(dir, files, |path| {
        matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("rs" | "wgsl")
        )
    });
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    collect_matching_files(dir, files, |path| {
        path.extension().and_then(|ext| ext.to_str()) == Some("rs")
    });
}

fn collect_matching_files(dir: &Path, files: &mut Vec<PathBuf>, keep: fn(&Path) -> bool) {
    let mut entries = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .collect::<Vec<_>>();
    entries.sort();

    for path in entries {
        if path.is_dir() {
            collect_matching_files(&path, files, keep);
        } else if keep(&path) {
            files.push(path);
        }
    }
}

fn first_line(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
        .lines()
        .next()
        .map(str::to_owned)
}
