// SPDX-License-Identifier: GPL-3.0-only
//! Quoted `odytty.conf` values and a leading byte order mark.

use super::*;

fn parsed(line: &str) -> Option<String> {
    let key = config_key_to_env("external_palette_path").expect("known key");
    let config = ConfigValues::parse(line, |message| panic!("unexpected warning: {message}"));
    config
        .get(key)
        .map(|value| value.to_string_lossy().into_owned())
}

/// Only `\\` and `\"` are escapes inside quotes. Any other backslash is data,
/// so a hand-written Windows path reads back unchanged.
#[test]
fn a_quoted_windows_path_keeps_its_backslashes() {
    assert_eq!(
        parsed(r#"external_palette_path = "C:\Users\me\theme #1.conf""#).as_deref(),
        Some(r"C:\Users\me\theme #1.conf")
    );
    assert_eq!(
        parsed(r#"external_palette_path = "a\nb""#).as_deref(),
        Some(r"a\nb")
    );
    // A lone trailing backslash is kept as well.
    assert_eq!(
        parsed(r#"external_palette_path = "C:\dir\\""#).as_deref(),
        Some(r"C:\dir\")
    );
}

/// The escapes the writer produces still decode, so a saved value round-trips.
#[test]
fn the_writers_escapes_still_decode() {
    for original in [
        r"C:\Users\me\#back",
        r#"with "quotes" and \ slashes #"#,
        " leading space",
        r"trailing backslash \",
        r"\\server\share\x",
    ] {
        let quoted = config::quote_config_value(original);
        assert_eq!(
            parsed(&format!("external_palette_path = {quoted}")).as_deref(),
            Some(original),
            "{quoted}"
        );
    }
    assert_eq!(
        parsed(r#"external_palette_path = "a\\b\"c""#).as_deref(),
        Some(r#"a\b"c"#)
    );
}

/// Control: the byte order mark is removed once by `read_capped`, so the first
/// setting of a BOM file loads. This passed before this change too; it pins the
/// end-to-end behavior that the unit test in `fs_read` covers in isolation.
#[test]
fn the_first_setting_of_a_file_with_a_byte_order_mark_loads() {
    let dir = crate::test_dirs::ScratchDir::new("odytty-conf-bom-");
    let path = dir.path().join("odytty.conf");
    fs::write(
        &path,
        b"\xef\xbb\xbfexternal_palette_path = first\nfont_size = 13\n",
    )
    .expect("write config");
    let mut warnings = Vec::new();
    let config = ConfigValues::read(&path, |message| warnings.push(message)).expect("read config");
    assert!(warnings.is_empty(), "{warnings:?}");
    let key = config_key_to_env("external_palette_path").expect("known key");
    assert_eq!(config.get(key).and_then(|v| v.to_str()), Some("first"));
}
