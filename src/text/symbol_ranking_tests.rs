// SPDX-License-Identifier: GPL-3.0-only
//! Portable regular-style symbol selection and bounded fontconfig records.

use super::*;

struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("symbol-rank"))
    }
    fn face(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
            &path,
        )
        .expect("copy licensed bundled fixture face");
        path
    }
}
impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn host_nerd_selection_prefers_regular_over_bold_and_unnamed_variants() {
    let fixture = FixtureDir::new();
    fixture.face("SymbolsNerdFont-Bold.ttf");
    fixture.face("SymbolsNerdFont.ttf");
    let regular = fixture.face("SymbolsNerdFont-Regular.ttf");
    assert_eq!(
        resolve_symbol_font_path_in(std::slice::from_ref(&fixture.0)),
        Some(regular.clone())
    );
    let inventory = FontFileInventory::new(vec![fixture.0.clone()]);
    assert_eq!(
        load_host_symbol_font(&inventory).expect("load host face").0,
        regular
    );
}

#[test]
fn stronger_symbol_hint_still_wins_over_a_weaker_regular_face() {
    let fixture = FixtureDir::new();
    let dedicated = fixture.face("SymbolsNerdFont-Bold.ttf");
    fixture.face("OtherNerdFont-Regular.ttf");
    assert_eq!(
        resolve_symbol_font_path_in(std::slice::from_ref(&fixture.0)),
        Some(dedicated)
    );
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
#[test]
fn static_symbol_hint_prefers_regular_then_shortest_remaining_stem() {
    let fixture = FixtureDir::new();
    fixture.face("Marker-Bold.ttf");
    let plain = fixture.face("Marker.ttf");
    let regular = fixture.face("Marker-Regular.ttf");
    let inventory = FontFileInventory::new(vec![fixture.0.clone()]);
    let faces = hinted_fallback_faces(&inventory, &["marker"]);
    assert_eq!(faces[0].0, SymbolFontSource::Host(regular.clone()));
    std::fs::remove_file(regular).expect("remove regular fixture");
    let inventory = FontFileInventory::new(vec![fixture.0.clone()]);
    let faces = hinted_fallback_faces(&inventory, &["marker"]);
    assert_eq!(faces[0].0, SymbolFontSource::Host(plain));
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn malformed_utf8_drops_one_fontconfig_record_only() {
    let text = fontconfig_text(b"first.ttf\t0\nbad\xff.ttf\t0\nsecond.ttc\t3\n");
    let records: Vec<_> = text.lines().filter_map(parse_fc_record).collect();
    assert_eq!(
        records,
        vec![
            (PathBuf::from("first.ttf"), 0),
            (PathBuf::from("second.ttc"), 3)
        ]
    );
    assert!(
        !text.contains('\u{fffd}'),
        "do not invent a replacement pathname"
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn runtime_fontconfig_query_requests_sorted_regular_preferred_candidates() {
    let mut queried = Vec::new();
    let candidates = symbol_font_candidates_with('x', |program, args| {
        queried.push(program.to_owned());
        match program {
            "fc-match" => {
                assert!(args.contains(&"-s"));
                assert!(args.contains(&FC_RECORD_FORMAT_NL));
                assert!(args.last().unwrap().ends_with(":style=Regular"));
                Ok(Some("first.ttf\t0\nsecond.ttc\t3\n".to_owned()))
            }
            "fc-list" => Ok(Some(
                "first.ttf\t0\nsecond.ttc\t3\nthird.ttf\t0\n".to_owned(),
            )),
            _ => panic!("unexpected helper"),
        }
    })
    .expect("query candidates");
    assert_eq!(queried, ["fc-match", "fc-list"]);
    assert_eq!(
        candidates,
        vec![
            (PathBuf::from("first.ttf"), 0),
            (PathBuf::from("second.ttc"), 3),
            (PathBuf::from("third.ttf"), 0)
        ]
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn runtime_candidates_filter_coverage_before_the_attempt_cap() {
    let candidates = symbol_font_candidates_with('x', |program, _| {
        Ok(Some(match program {
            "fc-match" => {
                let mut text = String::new();
                for index in 0..MAX_SYMBOL_FONT_CANDIDATES {
                    text.push_str(&format!("uncovered{index}.ttf\t0\n"));
                }
                text.push_str("preferred.ttc\t2\n");
                text
            }
            "fc-list" => {
                let mut text = String::new();
                for index in 0..MAX_SYMBOL_FONT_CANDIDATES {
                    text.push_str(&format!("covering{index}.ttf\t0\n"));
                }
                text.push_str("preferred.ttc\t2\n");
                text
            }
            _ => panic!("unexpected helper"),
        }))
    })
    .expect("query candidates");
    assert_eq!(candidates.len(), MAX_SYMBOL_FONT_CANDIDATES);
    assert_eq!(candidates[0], (PathBuf::from("preferred.ttc"), 2));
    assert!(
        candidates
            .iter()
            .all(|(path, _)| !path.to_str().unwrap().starts_with("uncovered"))
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn runtime_candidates_have_no_faces_when_coverage_is_empty() {
    let candidates = symbol_font_candidates_with('x', |program, _| {
        Ok(Some(match program {
            "fc-match" => "uncovered.ttf\t0\n".to_owned(),
            "fc-list" => String::new(),
            _ => panic!("unexpected helper"),
        }))
    })
    .expect("query candidates");
    assert!(candidates.is_empty());
}
