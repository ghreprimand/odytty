// SPDX-License-Identifier: GPL-3.0-only
//! Host symbol-face discovery falls through a candidate that fails to load
//! instead of giving up at the first match. Hermetic: every font root is a
//! temporary directory holding a broken file and a copy of a bundled face.

use std::path::{Path, PathBuf};

use super::*;

fn temp_root(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "odytty-discovery-siblings-{tag}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp font root");
    dir
}

fn loadable_face() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf")
}

#[test]
fn host_nerd_face_falls_through_an_unloadable_best_match() {
    // "Symbols Nerd Font" outranks a patched "* Nerd Font", but this one is
    // not a font; the patched face loads.
    let root = temp_root("nerd");
    std::fs::write(root.join("SymbolsNerdFont-Regular.ttf"), b"not a font").expect("write");
    let patched = root.join("HackNerdFont-Regular.ttf");
    std::fs::copy(loadable_face(), &patched).expect("copy loadable face");
    let dirs = std::slice::from_ref(&root);

    assert_eq!(
        resolve_symbol_font_path_in(dirs),
        Some(root.join("SymbolsNerdFont-Regular.ttf")),
        "the preferred file is still the stronger hint"
    );
    let (sources, fonts) = resolve_symbol_fonts_with_source(None, dirs);
    assert_eq!(sources.len(), fonts.len());
    assert!(
        sources.contains(&SymbolFontSource::Host(patched.clone())),
        "the loadable patched face joins the chain: {sources:?}"
    );
    assert!(resolve_symbol_font_in(dirs).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn linux_symbol_tail_hint_falls_through_an_unloadable_first_match() {
    // Name order puts the broken "-A" file first for both Noto Symbols hints.
    let root = temp_root("linux-tail");
    std::fs::write(root.join("NotoSansSymbols2-A.ttf"), b"not a font").expect("write");
    let regular = root.join("NotoSansSymbols2-Regular.ttf");
    std::fs::copy(loadable_face(), &regular).expect("copy loadable face");
    let faces = symbols::linux_symbol_fallback_faces(std::slice::from_ref(&root));
    let sources: Vec<_> = faces.iter().map(|(source, _)| source.clone()).collect();
    assert_eq!(sources, [SymbolFontSource::Host(regular)]);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn windows_symbol_tail_hint_falls_through_an_unloadable_first_match() {
    // Name order puts the broken "seguisym-a" file before "seguisym".
    let root = temp_root("windows-tail");
    std::fs::write(root.join("seguisym-a.ttf"), b"not a font").expect("write");
    let regular = root.join("seguisym.ttf");
    std::fs::copy(loadable_face(), &regular).expect("copy loadable face");
    let faces = symbols::windows_symbol_fallback_faces(std::slice::from_ref(&root));
    let sources: Vec<_> = faces.iter().map(|(source, _)| source.clone()).collect();
    assert_eq!(sources, [SymbolFontSource::Host(regular)]);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
#[test]
fn a_hint_whose_first_match_an_earlier_hint_loaded_adds_nothing() {
    // Unchanged de-duplication: one file matching two hints loads once.
    let root = temp_root("dedupe");
    #[cfg(windows)]
    let name = "seguisym.ttf";
    #[cfg(not(windows))]
    let name = "NotoSansSymbols2-Regular.ttf";
    let face = root.join(name);
    std::fs::copy(loadable_face(), &face).expect("copy loadable face");
    #[cfg(windows)]
    let faces = symbols::windows_symbol_fallback_faces(std::slice::from_ref(&root));
    #[cfg(not(windows))]
    let faces = symbols::linux_symbol_fallback_faces(std::slice::from_ref(&root));
    assert_eq!(faces.len(), 1);
    let _ = std::fs::remove_dir_all(root);
}
