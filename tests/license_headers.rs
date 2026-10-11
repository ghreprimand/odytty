// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::path::{Path, PathBuf};

const SPDX: &str = "// SPDX-License-Identifier: GPL-3.0-only";
const PY_SPDX: &str = "# SPDX-License-Identifier: GPL-3.0-only";

/// Scope of the check: every tracked Rust and WGSL source anywhere in the
/// repository (the library, tests, benches, the build script and its support
/// module, and the fuzz crates), and every Python script under `scripts/`,
/// `.github/scripts/` and `tests/` (header on line 1, or line 2 after a
/// shebang). The shipped installers and packaging manifests under `dist/`,
/// workflow YAML and TOML configuration are deliberately outside it.
#[test]
fn source_files_carry_gpl_spdx_header() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in [
        "src",
        "tests",
        "benches",
        "build_support",
        "fuzz",
        "scripts",
        ".github/scripts",
    ] {
        collect_source_files(&root.join(dir), &mut files);
    }
    files.push(root.join("build.rs"));
    files.sort();

    let missing = files
        .iter()
        .filter(|path| !has_header(path))
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "missing GPL SPDX header:\n{}",
        missing.join("\n")
    );
    // The walk must reach the files this test exists to cover.
    for expected in [
        "build.rs",
        "build_support/provenance.rs",
        "fuzz/parser_graphics/fuzz_targets/support.rs",
        "scripts/documentation-guard.py",
        ".github/scripts/derive-version-test.py",
    ] {
        assert!(
            files.iter().any(|path| path.ends_with(expected)),
            "the header walk does not reach {expected}"
        );
    }
}

fn has_header(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("py") => {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            let mut lines = text.lines();
            let first = lines.next();
            let second = lines.next();
            first == Some(PY_SPDX)
                || (first.is_some_and(|l| l.starts_with("#!")) && second == Some(PY_SPDX))
        }
        _ => first_line(path).as_deref() == Some(SPDX),
    }
}

fn collect_source_files(dir: &Path, files: &mut Vec<PathBuf>) {
    collect_matching_files(dir, files, |path| {
        matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("rs" | "wgsl" | "py")
        )
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
            // Build output and hidden directories hold no tracked source.
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name == "target" || name.starts_with('.') {
                continue;
            }
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
