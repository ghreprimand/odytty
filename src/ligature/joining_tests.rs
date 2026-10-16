// SPDX-License-Identifier: GPL-3.0-only
//! Joining runs of Syriac, N'Ko, Mandaic, Adlam, Hanifi Rohingya, and
//! Mongolian against HarfBuzz 14.5.1 references for the licensed Noto subsets
//! in `tests/fixtures/fonts/s5b/joining` (right to left) and `joining-ltr`
//! (Mongolian, left to right). Every reference row is one project-authored
//! run, shaped in its script's direction as production shapes it; each cell
//! must draw exactly the glyphs of its reference clusters, with marks at the
//! reference's offsets from their base, in logical order and in display
//! order.

use super::*;
use crate::core::Terminal;
use crate::selection::{CellPoint, SelectionRange, selected_text};

struct Fonts(FontHandle);

impl LigatureFonts for Fonts {
    fn ligature_font(&self, _style: FontStyle) -> &FontHandle {
        &self.0
    }
}

const REFERENCES: &str = include_str!("../../tests/fixtures/fonts/s5b/joining/reference.tsv");
const LTR_REFERENCES: &str =
    include_str!("../../tests/fixtures/fonts/s5b/joining-ltr/reference.tsv");

fn face(name: &str) -> FontHandle {
    let bytes: &[u8] = match name {
        "Syriac-subset.ttf" => {
            include_bytes!("../../tests/fixtures/fonts/s5b/joining/Syriac-subset.ttf")
        }
        "NKo-subset.ttf" => include_bytes!("../../tests/fixtures/fonts/s5b/joining/NKo-subset.ttf"),
        "Mandaic-subset.ttf" => {
            include_bytes!("../../tests/fixtures/fonts/s5b/joining/Mandaic-subset.ttf")
        }
        "Adlam-subset.ttf" => {
            include_bytes!("../../tests/fixtures/fonts/s5b/joining/Adlam-subset.ttf")
        }
        "HanifiRohingya-subset.ttf" => {
            include_bytes!("../../tests/fixtures/fonts/s5b/joining/HanifiRohingya-subset.ttf")
        }
        "Mongolian-subset.ttf" => {
            include_bytes!("../../tests/fixtures/fonts/s5b/joining-ltr/Mongolian-subset.ttf")
        }
        other => panic!("unknown fixture {other}"),
    };
    FontHandle::try_from_vec(bytes.to_vec()).expect("joining fixture parses")
}

/// One reference row: its text and, per cluster start byte, the glyph ids
/// of the cluster and each glyph's font-unit offset from the cluster's first
/// glyph in logical order.
struct Reference {
    note: String,
    text: String,
    clusters: std::collections::BTreeMap<usize, Vec<(u16, [i16; 2])>>,
}

fn references(font: &str) -> Vec<Reference> {
    let right_to_left = font != "Mongolian-subset.ttf";
    let table = if right_to_left {
        REFERENCES
    } else {
        LTR_REFERENCES
    };
    let mut out = Vec::new();
    for line in table.lines() {
        if line.starts_with('#') || line.starts_with("font\t") {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields[0] != font {
            continue;
        }
        let text: String = fields[1]
            .split_whitespace()
            .map(|cp| char::from_u32(u32::from_str_radix(&cp[2..], 16).unwrap()).unwrap())
            .collect();
        let numbers = |index: usize| -> Vec<i32> {
            fields[index]
                .split(',')
                .map(|value| value.parse().unwrap())
                .collect()
        };
        let (ids, clusters, dx, dy, advance) =
            (numbers(2), numbers(3), numbers(4), numbers(5), numbers(6));
        // Visual pen positions, then logical order.
        let mut pen = 0;
        let mut placed = Vec::new();
        for index in 0..ids.len() {
            placed.push((
                ids[index] as u16,
                clusters[index] as usize,
                pen + dx[index],
                dy[index],
            ));
            pen += advance[index];
        }
        if right_to_left {
            placed.reverse();
        }
        let mut clusters = std::collections::BTreeMap::<usize, Vec<_>>::new();
        let mut first = None;
        for (id, cluster, x, y) in placed {
            let offset = match first {
                Some((start, x0, y0)) if start == cluster => [(x - x0) as i16, (y - y0) as i16],
                _ => {
                    first = Some((cluster, x, y));
                    [0, 0]
                }
            };
            clusters.entry(cluster).or_default().push((id, offset));
        }
        out.push(Reference {
            note: fields[7].to_owned(),
            text,
            clusters,
        });
    }
    assert!(out.len() >= 6, "{font} has its reference rows");
    out
}

fn terminal(text: &str, cols: usize) -> Terminal {
    let mut terminal = Terminal::new(cols, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance(text.as_bytes());
    terminal
}

/// The glyphs each logical cell of row 0 draws: the overlay glyphs anchored
/// at it, or its own cmap glyph and those of its marks.
fn drawn(
    snapshot: &Snapshot,
    runs: &[LigatureRun],
    font: &FontHandle,
    cells: usize,
) -> Vec<Vec<(u16, [i16; 2])>> {
    (0..cells)
        .map(|column| {
            let run = runs.iter().find(|run| run.covers(0, column));
            match run {
                Some(run) => run
                    .glyphs
                    .iter()
                    .filter(|glyph| run.start + usize::from(glyph.key.anchor_cell) == column)
                    .map(|glyph| (glyph.key.glyph_id, glyph.key.mark_offset))
                    .collect(),
                None => {
                    let cell = &snapshot.cells[column];
                    std::iter::once(cell.ch)
                        .chain(cell.combining().iter().copied())
                        .map(|ch| (font.glyph_id(ch).0, [0, 0]))
                        .collect()
                }
            }
        })
        .collect()
}

fn sorted(mut glyphs: Vec<(u16, [i16; 2])>) -> Vec<(u16, [i16; 2])> {
    glyphs.sort_unstable();
    glyphs
}

/// Every reference row of `font` draws its reference glyphs per cell, with
/// bidi reordering off and on; at least one row is joined.
fn assert_runs_match_references(font_name: &str) {
    let font = face(font_name);
    let fonts = Fonts(font.clone());
    let mut joined = 0;
    for reference in references(font_name) {
        let terminal = terminal(&reference.text, 8);
        let snapshot = terminal.snapshot();
        // The byte each cell's grapheme starts at; a mark rides its letter.
        let mut starts = Vec::new();
        let mut byte = 0;
        for cell in &snapshot.cells {
            if byte >= reference.text.len() {
                break;
            }
            starts.push(byte);
            byte += cell.grapheme().len();
        }
        let cells = starts.len();
        assert_eq!(
            snapshot.cells[..cells]
                .iter()
                .map(|cell| cell.grapheme())
                .collect::<String>(),
            reference.text,
            "{} is one cell per letter",
            reference.note
        );
        let logical = LigatureShaper::new().build_runs(true, &snapshot, &fonts, &[]);
        let map = BidiDisplayMap::plan(&snapshot, &[false]);
        // Right-to-left runs reorder; horizontal Mongolian is left to right
        // and keeps the identity layout.
        assert_eq!(
            map.is_identity(),
            font_name == "Mongolian-subset.ttf",
            "{} layout",
            reference.note
        );
        let display = LigatureShaper::new().build_runs_bidi(&snapshot, &fonts, &[], &map);
        for (label, runs) in [("logical", &logical), ("display", &display)] {
            let got = drawn(&snapshot, runs, &font, cells);
            for (column, start) in starts.iter().enumerate() {
                // Every reference cluster that starts inside this cell, such
                // as a Mongolian vowel separator kept with its letter.
                let end = starts
                    .get(column + 1)
                    .copied()
                    .unwrap_or(reference.text.len());
                let expected: Vec<_> = reference
                    .clusters
                    .range(*start..end)
                    .flat_map(|(_, glyphs)| glyphs.iter().copied())
                    .collect();
                assert_eq!(
                    sorted(got[column].clone()),
                    sorted(expected),
                    "{font_name} {} {label} cell {column}",
                    reference.note
                );
            }
        }
        assert_eq!(
            logical, display,
            "{}: one shaping in either layout",
            reference.note
        );
        if !logical.is_empty() {
            joined += 1;
        }
        // Copy stays the logical text.
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 0 },
            end: CellPoint {
                row: 0,
                column: cells - 1,
            },
        };
        assert_eq!(selected_text(&snapshot, range), reference.text);
    }
    assert!(
        joined >= 5,
        "{font_name}: the joined rows draw through overlays"
    );
}

#[test]
fn syriac_run_takes_joining_forms() {
    assert_runs_match_references("Syriac-subset.ttf");
}

#[test]
fn nko_run_takes_joining_forms() {
    assert_runs_match_references("NKo-subset.ttf");
}

#[test]
fn mandaic_run_takes_joining_forms() {
    assert_runs_match_references("Mandaic-subset.ttf");
}

#[test]
fn adlam_run_takes_joining_forms() {
    assert_runs_match_references("Adlam-subset.ttf");
}

#[test]
fn hanifi_rohingya_run_takes_joining_forms() {
    assert_runs_match_references("HanifiRohingya-subset.ttf");
}

#[test]
fn horizontal_mongolian_run_takes_joining_forms() {
    assert_runs_match_references("Mongolian-subset.ttf");
}

#[test]
fn joining_tables_hold_each_scripts_letters_and_marks() {
    for (ch, script) in [
        ('\u{0710}', JoiningScript::Syriac),
        ('\u{0712}', JoiningScript::Syriac),
        ('\u{0860}', JoiningScript::Syriac),
        ('\u{07CA}', JoiningScript::Nko),
        ('\u{07FA}', JoiningScript::Nko),
        ('\u{0840}', JoiningScript::Mandaic),
        ('\u{0858}', JoiningScript::Mandaic),
        ('\u{1E900}', JoiningScript::Adlam),
        ('\u{1E943}', JoiningScript::Adlam),
        ('\u{10D00}', JoiningScript::HanifiRohingya),
        ('\u{1820}', JoiningScript::Mongolian),
        ('\u{180A}', JoiningScript::Mongolian),
    ] {
        assert_eq!(joining_script(ch), Some(script), "{:04X}", u32::from(ch));
    }
    // Punctuation, digits, the abbreviation mark, and other scripts.
    for ch in [
        'a',
        '\u{0700}',
        '\u{070F}',
        '\u{07C0}',
        '\u{085E}',
        '\u{05D0}',
        '\u{1E950}',
        '\u{10D30}',
        '\u{1810}',
        '\u{180E}',
    ] {
        assert_eq!(joining_script(ch), None, "{:04X}", u32::from(ch));
    }
    assert_eq!(joining_script('\u{0628}'), Some(JoiningScript::Arabic));
    for (script, mark) in [
        (JoiningScript::Syriac, '\u{0711}'),
        (JoiningScript::Syriac, '\u{0730}'),
        (JoiningScript::Syriac, '\u{074A}'),
        (JoiningScript::Nko, '\u{07EB}'),
        (JoiningScript::Nko, '\u{07FD}'),
        (JoiningScript::Mandaic, '\u{0859}'),
        (JoiningScript::Adlam, '\u{1E944}'),
        (JoiningScript::HanifiRohingya, '\u{10D24}'),
        (JoiningScript::Mongolian, '\u{180B}'),
        (JoiningScript::Mongolian, '\u{180E}'),
        (JoiningScript::Mongolian, '\u{1885}'),
        (JoiningScript::Arabic, '\u{064E}'),
    ] {
        assert!(is_joining_mark(script, mark), "{:04X}", u32::from(mark));
    }
    // A mark rides only its own script; Inherited marks ride none.
    assert!(!is_joining_mark(JoiningScript::Syriac, '\u{07EB}'));
    assert!(!is_joining_mark(JoiningScript::Nko, '\u{0730}'));
    assert!(!is_joining_mark(JoiningScript::Syriac, '\u{0308}'));
    assert!(!is_joining_mark(JoiningScript::Syriac, '\u{064E}'));
    // The vowel separator rides Mongolian letters only.
    assert!(!is_joining_mark(JoiningScript::Adlam, '\u{180E}'));
}

#[test]
fn joining_runs_never_mix_scripts_or_carry_foreign_marks() {
    let coverage = ColorRunCoverage::new(&[], 12, 1);
    let bounds = |text: &str| -> Vec<(usize, usize)> {
        let snapshot = terminal(text, 12).snapshot();
        compatible_run_bounds(&snapshot.cells[..12], 0, &coverage)
            .into_iter()
            .map(|(start, end, _)| (start, end))
            .collect()
    };
    // Syriac, N'Ko, Mandaic, then Arabic: four runs.
    assert_eq!(
        bounds("\u{0712}\u{0712}\u{07CA}\u{07CA}\u{0841}\u{0841}\u{0628}\u{0628}"),
        [(0, 2), (2, 4), (4, 6), (6, 8)]
    );
    // A Syriac mark rides its letter; an Inherited diaeresis or an N'Ko tone
    // mark on a Syriac letter keeps that cell on the monochrome path.
    assert_eq!(bounds("\u{0712}\u{0730}\u{0712}\u{0712}"), [(0, 3)]);
    assert_eq!(bounds("\u{0712}\u{0308}\u{0712}\u{0712}"), [(1, 3)]);
    assert_eq!(bounds("\u{0712}\u{07EB}\u{0712}\u{0712}"), [(1, 3)]);
}

#[test]
fn adlam_rohingya_and_mongolian_runs_stay_apart_and_keep_their_marks() {
    let coverage = ColorRunCoverage::new(&[], 12, 1);
    let bounds = |text: &str| -> Vec<(usize, usize)> {
        let snapshot = terminal(text, 12).snapshot();
        compatible_run_bounds(&snapshot.cells[..12], 0, &coverage)
            .into_iter()
            .map(|(start, end, _)| (start, end))
            .collect()
    };
    assert_eq!(
        bounds("\u{1E922}\u{1E923}\u{10D01}\u{10D02}\u{1820}\u{1821}"),
        [(0, 2), (2, 4), (4, 6)]
    );
    // The Mongolian vowel separator and a free variation selector stay with
    // their letter, which stays in its run.
    assert_eq!(
        bounds("\u{1820}\u{180E}\u{1821}\u{1828}\u{180B}\u{1820}"),
        [(0, 4)]
    );
    // Adlam nukta rides its letter; a Hanifi Rohingya sign on Adlam does not.
    assert_eq!(bounds("\u{1E925}\u{1E94A}\u{1E922}"), [(0, 2)]);
    assert_eq!(bounds("\u{1E925}\u{10D24}\u{1E922}\u{1E923}"), [(1, 3)]);
}

#[test]
fn script_shaping_off_draws_joining_scripts_per_cell() {
    let fonts = Fonts(face("Syriac-subset.ttf"));
    let snapshot = terminal("\u{0712}\u{0712}\u{0712} ab", 8).snapshot();
    let switches = |scripts| ShapingSwitches {
        ligatures: true,
        scripts,
    };
    let on = LigatureShaper::new().build_runs_with_switches(
        switches(true),
        &snapshot,
        &fonts,
        &[],
        LatinShapingFeatures::default(),
        None,
    );
    assert!(
        on.iter().any(|run| run.start == 0),
        "fixture is live: {on:?}"
    );
    let off = LigatureShaper::new().build_runs_with_switches(
        switches(false),
        &snapshot,
        &fonts,
        &[],
        LatinShapingFeatures::default(),
        None,
    );
    assert!(off.iter().all(|run| run.start >= 4), "{off:?}");
}

#[test]
fn an_unmapped_joining_letter_splits_its_run() {
    // The Syriac subset maps no Mandaic letter, so a Mandaic run on it keeps
    // the per-cell path; a Syriac letter the face lacks would split likewise.
    let fonts = Fonts(face("Syriac-subset.ttf"));
    let snapshot = terminal("\u{0841}\u{0842}\u{0843}", 6).snapshot();
    let runs = LigatureShaper::new().build_runs(true, &snapshot, &fonts, &[]);
    assert!(runs.is_empty(), "{runs:?}");
}
