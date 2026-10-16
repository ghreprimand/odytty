// SPDX-License-Identifier: GPL-3.0-only
//! Fixed internal regression rows. This module changes no terminal behavior.
use super::*;
use odytty::complex_shaping::ComplexShaper;
use odytty::grid::{BidiDisplayMap, BidiParagraphContext, BidiParagraphs, max_bidi_context_rows};

const PANES: usize = 4;
const FRAME_OPS: usize = 200;

pub(super) fn enabled() -> bool {
    std::env::var_os("ODYTTY_PERF_PROTOCOL").is_some()
}

pub(super) fn record(name: &str, elapsed: Duration, ops: usize) {
    if enabled() {
        println!("SAMPLE\t{name}\t{}\t{ops}\tns", elapsed.as_nanos());
    }
}

fn measure(name: &str, ops: usize, mut f: impl FnMut()) {
    f();
    let elapsed = time_once(|| {
        for _ in 0..ops {
            f();
        }
    });
    record(name, elapsed, ops);
}

fn geometry(
    vertices: &mut Vec<odytty::grid::Vertex>,
    snapshot: &odytty::core::Snapshot,
    atlas: &GlyphAtlas,
    runs: &[odytty::ligature::LigatureRun],
    map: Option<&BidiDisplayMap>,
) {
    odytty::grid::build_cell_vertices_with_ligatures_and_selection_into(
        vertices,
        snapshot,
        atlas,
        &[],
        runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        1.0,
        1.0,
        None,
        ChromePin::NONE,
        1.0,
        map,
    );
    black_box(&vertices);
}

/// The live frame's plan, as the native frame builds it: soft-wrap flags,
/// each paragraph's stored properties, the context above the viewport, and
/// the default left-to-right direction, with image rows reset.
fn plan_frame(
    terminal: &Terminal,
    snapshot: &odytty::core::Snapshot,
    offset: usize,
) -> BidiDisplayMap {
    let (wrapped, properties): (Vec<bool>, Vec<_>) = terminal
        .visible_search_rows(offset)
        .iter()
        .map(|row| (row.wrapped, row.paragraph_bidi))
        .unzip();
    let (rows, overflow) = terminal.paragraph_context_rows(offset, max_bidi_context_rows(COLS));
    let mut map = BidiDisplayMap::plan_with_paragraphs(
        snapshot,
        &wrapped,
        &BidiParagraphContext {
            properties: rows
                .first()
                .map(|row| row.paragraph_bidi)
                .unwrap_or_default(),
            rows: rows.into_iter().map(|row| row.cells).collect(),
            overflow,
        },
        BidiParagraphs {
            properties: &properties,
            default_direction: Default::default(),
        },
    );
    // Rows an image covers keep the identity layout, as in the live frame.
    if !map.is_identity() {
        map.reset_image_rows(snapshot, &terminal.visible_graphics(offset));
    }
    map
}

fn bidi_rows() {
    let font = FontHandle::try_from_vec(
        include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec(),
    )
    .expect("project-authored bidi face");
    let atlas = GlyphAtlas::build(&font, 28.0);
    for (corpus, pattern) in [
        (
            "mixed",
            "abc (\u{5d0}\u{5d1}\u{5d2}) 123 \u{627}\u{628}\u{644} xy ",
        ),
        ("ascii", "abc (abc) 123 abc xy "),
    ] {
        // 120 logical paragraphs, each spanning at least eight visual rows.
        // Offset 1000 opens a paragraph above the visible viewport.
        let paragraph = pattern.repeat(40) + "\r\n";
        for (position, offset) in [("live", 0), ("history", 1000)] {
            for on in [false, true] {
                let mut terminals: Vec<_> = (0..PANES)
                    .map(|_| {
                        let mut terminal = Terminal::new(COLS, ROWS);
                        terminal.advance(paragraph.repeat(120).as_bytes());
                        terminal
                    })
                    .collect();
                let switch = if on { "on" } else { "off" };
                let name = format!("bidi {corpus} {position} {switch} four-pane frame");
                let mut vertices = Vec::new();
                measure(&name, FRAME_OPS, || {
                    for terminal in &terminals {
                        let snapshot = terminal.snapshot_with_scrollback(offset);
                        let map = on.then(|| plan_frame(terminal, &snapshot, offset));
                        geometry(&mut vertices, &snapshot, &atlas, &[], map.as_ref());
                    }
                });
                // Live-output frames feed one full line per pane before replanning.
                if offset == 0 {
                    let name = format!("bidi {corpus} output {switch} four-pane frame");
                    let line = pattern.repeat(4) + "\r\n";
                    measure(&name, FRAME_OPS, || {
                        for terminal in &mut terminals {
                            terminal.advance(line.as_bytes());
                            let snapshot = terminal.snapshot();
                            let map = on.then(|| plan_frame(terminal, &snapshot, 0));
                            geometry(&mut vertices, &snapshot, &atlas, &[], map.as_ref());
                        }
                    });
                }
            }
        }
    }
}

fn script_rows() {
    for (name, text, bytes) in [
        (
            "Devanagari",
            "\u{915}\u{94d}\u{937} \u{915}\u{93f} ",
            include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Devanagari-subset.ttf")
                .as_slice(),
        ),
        (
            "Bengali",
            "\u{995}\u{9cd}\u{9b7} \u{995}\u{9bf} ",
            include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Bengali-subset.ttf")
                .as_slice(),
        ),
        (
            "Javanese",
            "\u{a98f}\u{a9c0}\u{a98f} \u{a98f}\u{a9ba} ",
            include_bytes!("../../tests/fixtures/fonts/s5b/g6/Javanese-subset.ttf").as_slice(),
        ),
        (
            "Balinese",
            "\u{1b13}\u{1b44}\u{1b13} \u{1b13}\u{1b35} ",
            include_bytes!("../../scripts/bench-protocol/fonts/Balinese-subset.ttf").as_slice(),
        ),
        (
            "Syriac",
            "\u{710}\u{730}\u{712}\u{715} ",
            include_bytes!("../../scripts/bench-protocol/fonts/Syriac-subset.ttf").as_slice(),
        ),
    ] {
        let font = FontHandle::try_from_vec(bytes.to_vec()).expect("licensed script face");
        for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
            assert_ne!(font.glyph_id(ch).0, 0, "{name}: missing {ch:?}");
        }
        let payload = (text.repeat(20) + "\r\n").repeat(200);
        let mut terminal = Terminal::new(COLS, ROWS);
        terminal.advance(payload.as_bytes());
        let snapshot = terminal.snapshot();
        let fonts = BenchLigatureFont(&font);
        let mut atlas = GlyphAtlas::build(&font, 28.0);
        let mut scripts = ComplexShaper::new();
        let mut joining = LigatureShaper::new();
        let mut vertices = Vec::new();
        for on in [false, true] {
            let switch = if on { "on" } else { "off" };
            let runs = scripts.build_runs(on, &snapshot, &fonts, &mut atlas, &[]);
            println!("COVERAGE\t{name}\t{switch}\t{}", runs.len());
            measure(
                &format!("script {name} cached {switch} frame"),
                FRAME_OPS,
                || {
                    let mut runs = scripts.build_runs(on, &snapshot, &fonts, &mut atlas, &[]);
                    // Include the joining dispatcher alongside owner shaping.
                    runs.extend(joining.build_runs(on, &snapshot, &fonts, &[]));
                    geometry(&mut vertices, &snapshot, &atlas, &runs, None);
                },
            );
        }
        measure(&format!("script {name} cold frame"), 20, || {
            scripts.clear();
            joining.clear();
            let mut runs = scripts.build_runs(true, &snapshot, &fonts, &mut atlas, &[]);
            runs.extend(joining.build_runs(true, &snapshot, &fonts, &[]));
            geometry(&mut vertices, &snapshot, &atlas, &runs, None);
        });
        measure(&format!("script {name} feed"), 10, || {
            let mut terminal = Terminal::new(COLS, ROWS);
            terminal.advance(black_box(payload.as_bytes()));
            black_box(terminal);
        });
    }
}

pub(super) fn run() {
    measure("model constructor 80x24", 1000, || {
        black_box(Terminal::new(COLS, ROWS));
    });
    let terminal = feed_all(&gen_plain(5000));
    black_box(terminal.snapshot_with_scrollback(1000));
    let bytes = terminal.scrollback_bytes();
    println!("SAMPLE\tmemory history ring\t{}\t1\tbytes", bytes.ring);
    println!(
        "SAMPLE\tmemory history projection\t{}\t1\tbytes",
        bytes.projection
    );
    if let Some(rss) = odytty::memory_report::read_process_memory().resident_bytes {
        println!("SAMPLE\tmemory process resident\t{rss}\t1\tbytes");
    }
    bidi_rows();
    script_rows();
}
