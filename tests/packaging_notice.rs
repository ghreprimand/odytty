// SPDX-License-Identifier: GPL-3.0-only
//! NOTICE ships with every package. The binary embeds tables generated from
//! Unicode Character Database and Unicode emoji data files, and the Unicode
//! License requires its copyright and permission notice to accompany them.
//! This suite keeps NOTICE carrying the licence text the generated tables cite
//! and naming the Unicode version they were generated from.
//!
//! Platform behavior: pure file reads on every OS.

use std::path::PathBuf;

fn read(relative: &str) -> String {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every generated table that cites the Unicode License.
const GENERATED_TABLES: &[&str] = &[
    "src/atlas/marks.rs",
    "src/core/bidi/classes.rs",
    "src/core/bidi/brackets.rs",
    "src/core/bidi/mirrored.rs",
    "src/core/bidi/mirroring.rs",
    "src/core/emoji_width/data.rs",
    "src/core/emoji_width/zwj_first.rs",
    "src/core/emoji_width/zwj_second.rs",
    "src/core/grapheme/data.rs",
    "src/core/indic/data.rs",
    "src/core/indic/linked.rs",
];

const LICENSE_FIXTURES: &[&str] = &[
    "tests/fixtures/unicode-bidi/LICENSE-UNICODE.txt",
    "tests/fixtures/unicode-emoji/LICENSE-UNICODE.txt",
    "tests/fixtures/unicode-grapheme/LICENSE-UNICODE.txt",
    "tests/fixtures/unicode-indic/LICENSE-UNICODE.txt",
    "tests/fixtures/unicode-legacy-computing/LICENSE-UNICODE.txt",
    "tests/fixtures/unicode-marks/LICENSE-UNICODE.txt",
];

#[test]
fn notice_carries_the_unicode_license_the_tables_cite() {
    let notice = words(&read("NOTICE"));
    for fixture in LICENSE_FIXTURES {
        let license = words(&read(fixture));
        assert!(!license.is_empty(), "{fixture} is empty");
        assert!(
            notice.contains(&license),
            "NOTICE does not reproduce {fixture} in full"
        );
    }
}

/// The Unicode release every generated table was built from, and the way
/// NOTICE names it. Moving the tables to a new release changes both.
const UNICODE_RELEASE: &str = "17.0";
const NOTICE_VERSION_PHRASE: &str = "version 17.0.0";

#[test]
fn notice_names_the_unicode_version_of_every_generated_table() {
    let notice = read("NOTICE");
    assert!(
        notice.contains(NOTICE_VERSION_PHRASE),
        "NOTICE does not say `{NOTICE_VERSION_PHRASE}`"
    );
    for table in GENERATED_TABLES {
        let source = read(table);
        assert!(
            source.contains(&format!("{UNICODE_RELEASE}.")) || source.contains("-17.0.0"),
            "{table} is not generated from Unicode {UNICODE_RELEASE}; update NOTICE and this suite"
        );
    }
}

#[test]
fn every_generated_table_is_listed() {
    // A new generated table that cites the Unicode copyright must join the
    // list above, so its version is checked against NOTICE too.
    fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.contains("Copyright Unicode, Inc.") {
                    let relative = path
                        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .expect("under the manifest dir");
                    found.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    found.sort();
    let mut listed: Vec<String> = GENERATED_TABLES.iter().map(|s| (*s).to_owned()).collect();
    listed.sort();
    assert_eq!(found, listed);
}
