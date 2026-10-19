// SPDX-License-Identifier: GPL-3.0-only
//! The render revision covers every input of the bidi display plan.
//!
//! The frame-to-frame plan cache reuses a plan while the terminal instance,
//! its render revision, the viewport offset, the dimensions, and the default
//! direction are unchanged. That is only sound if every terminal change that
//! can move a cell of the plan also advances the revision. These tests feed
//! seeded streams of the sequences that write cells, wrap flags, paragraph
//! properties, history, screens, and Sixel and Kitty images, and check after
//! every step that an unchanged revision means unchanged plan inputs.

use super::*;

/// What a plan of the viewport at `offset` reads from the terminal.
#[derive(Debug, PartialEq)]
struct PlanInputs {
    dimensions: Dimensions,
    cells: Vec<Cell>,
    rows: Vec<VisibleRow>,
    context: (Vec<VisibleRow>, bool),
    graphics: Vec<crate::graphics::placement::VisiblePlacement>,
}

fn inputs(terminal: &Terminal, offset: usize) -> PlanInputs {
    let snapshot = terminal.snapshot_with_scrollback(offset);
    PlanInputs {
        dimensions: snapshot.dimensions,
        cells: snapshot.cells,
        rows: terminal.visible_search_rows(offset),
        context: terminal.paragraph_context_rows(offset, 64),
        graphics: terminal.visible_graphics(offset),
    }
}

/// Small deterministic generator (xorshift64*), so a failure names its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// One small sixel image (two rows of a 12x20 cell), so placements enter and
/// leave the viewport.
const SIXEL: &str = "\x1bPq\"1;1;6;24#0;2;100;0;0#0~~~~~~-~~~~~~-~~~~~~-~~~~~~\x1b\\";

/// A one-pixel Kitty image placed at the cursor, and a delete of every
/// Kitty placement, so the Kitty placement path is covered as well.
const KITTY_PLACE: &str = "\x1b_Ga=T,f=24,s=1,v=1,c=2,r=2,q=2;AAAA\x1b\\";
const KITTY_DELETE: &str = "\x1b_Ga=d,q=2\x1b\\";

const TOKENS: &[&str] = &[
    "ab",
    "\u{05D0}\u{05D1}\u{05D2}",
    "\u{0627}\u{0644}",
    "123 ",
    " (x) ",
    "\u{4E2D}",
    "e\u{0301}",
    "\r\n",
    "\n",
    "\r",
    "\x08",
    "\t",
    "\x1b[2 k",
    "\x1b[1 k",
    "\x1b[0 k",
    "\x1b[8l",
    "\x1b[8h",
    "\x1b[K",
    "\x1b[1K",
    "\x1b[2K",
    "\x1b[J",
    "\x1b[1J",
    "\x1b[2J",
    "\x1b[3J",
    "\x1b[L",
    "\x1b[M",
    "\x1b[@",
    "\x1b[P",
    "\x1b[X",
    "\x1b[S",
    "\x1b[T",
    "\x1b[2;4r",
    "\x1b[r",
    "\x1b[H",
    "\x1b[3;5H",
    "\x1b[5G",
    "\x1b[A",
    "\x1b[B",
    "\x1bM",
    "\x1bD",
    "\x1bE",
    "\x1b7",
    "\x1b8",
    "\x1b[?7l",
    "\x1b[?7h",
    "\x1b[4h",
    "\x1b[4l",
    "\x1b[?1049h",
    "\x1b[?1049l",
    "\x1b[?47h",
    "\x1b[?47l",
    "\x1b[!p",
    "\x1bc",
    "\x1b#8",
    "\x1b[1;1;2;3$z",
    "\x1b[1;1;2;3;3;1;1$v",
    "\x1b[1\"q",
    "\x1b[1;1;2;3${",
    SIXEL,
    KITTY_PLACE,
    KITTY_DELETE,
];

#[test]
fn an_unchanged_revision_means_unchanged_plan_inputs() {
    for seed in 1..=96u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut terminal = Terminal::new(12, 5);
        let mut trace = String::new();
        for step in 0..400 {
            let offsets = [0, 3];
            let before: Vec<_> = offsets.iter().map(|&o| inputs(&terminal, o)).collect();
            let revision = terminal.render_revision();
            let token = TOKENS[rng.below(TOKENS.len())];
            trace.push_str(token);
            terminal.advance(token.as_bytes());
            if terminal.render_revision() != revision {
                continue;
            }
            for (offset, before) in offsets.iter().zip(&before) {
                assert!(
                    inputs(&terminal, *offset) == *before,
                    "seed {seed} step {step}: {token:?} changed the plan inputs at \
                     offset {offset} without a revision change; stream {trace:?}"
                );
            }
        }
    }
}

#[test]
fn a_paragraph_direction_change_at_a_paragraph_start_advances_the_revision() {
    let mut terminal = Terminal::new(12, 5);
    terminal.advance(b"ab\r");
    let revision = terminal.render_revision();
    terminal.advance(b"\x1b[2 k");
    assert_ne!(terminal.render_revision(), revision, "SCP rewrote the row");
    let revision = terminal.render_revision();
    terminal.advance(b"\x1b[8l");
    assert_ne!(terminal.render_revision(), revision, "BDSM rewrote the row");
}

#[test]
fn terminals_carry_distinct_instances() {
    let first = Terminal::new(4, 2);
    let second = Terminal::new(4, 2);
    assert_ne!(first.instance(), second.instance());
    assert_eq!(first.render_revision(), second.render_revision());
}

#[test]
fn the_image_tokens_place_and_delete_images() {
    for place in [SIXEL, KITTY_PLACE] {
        let mut terminal = Terminal::new(12, 5);
        terminal.advance(place.as_bytes());
        assert!(!terminal.visible_graphics(0).is_empty(), "{place:?} placed");
    }
    let mut terminal = Terminal::new(12, 5);
    terminal.advance(KITTY_PLACE.as_bytes());
    terminal.advance(KITTY_DELETE.as_bytes());
    assert!(
        terminal.visible_graphics(0).is_empty(),
        "the delete removed it"
    );
}

#[test]
fn lowering_the_history_limit_keeps_unchanged_revisions_honest() {
    // Long soft-wrapped paragraphs, the oldest opening with a right-to-left
    // letter, so trimming their heads changes what a scrolled-back viewport
    // and its paragraph context read.
    let mut terminal = Terminal::new(12, 4);
    terminal.set_scrollback_limit(200);
    let mut text = String::from("\u{05D0}");
    text.push_str(&"a".repeat(90));
    text.push_str("\r\n");
    for line in 0..30 {
        text.push_str(&format!("line {line} "));
        text.push_str(&"b".repeat(line % 4 * 13));
        text.push_str("\r\n");
    }
    terminal.advance(text.as_bytes());
    for limit in [20, 8, 3, 1] {
        let offsets = [0, 1, 4, 9, 20, 40, 200];
        let before: Vec<_> = offsets.iter().map(|&o| inputs(&terminal, o)).collect();
        let revision = terminal.render_revision();
        terminal.set_scrollback_limit(limit);
        if terminal.render_revision() == revision {
            for (&offset, old) in offsets.iter().zip(&before) {
                assert!(
                    inputs(&terminal, offset) == *old,
                    "limit {limit}: offset {offset} changed without a revision change"
                );
            }
        }
    }
}
