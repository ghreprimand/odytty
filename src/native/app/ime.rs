// SPDX-License-Identifier: GPL-3.0-only
//! IME (input-method) composition. `winit` delivers four `Ime` events once IME
//! is allowed (see `App::resumed`):
//!
//! - **Enabled / Disabled** - composition session bracket; we clear any stale
//!   pre-edit on either edge so a cancelled composition leaves no ghost text.
//! - **Preedit(text, cursor)** - the in-progress composition. Stored in
//!   [`App::ime_preedit`] and rendered inline at the terminal cursor with an
//!   underline; never sent to the PTY.
//! - **Commit(text)** - the finalized string. Written to the active PTY exactly
//!   like typed `Character` input, and the pre-edit is cleared.
//!
//! Off-path contract: with no composition in progress `ime_preedit` is empty,
//! [`App::ime_overlay_signature`] is `Inert`, and
//! [`App::paint_ime_preedit_cells`] writes nothing - the default render path is
//! unchanged.

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Ime;

use crate::core::{Attrs, Cell, Snapshot, UnderlineStyle};

use super::*;

fn preedit_needs_cursor_area(text: &str) -> bool {
    !text.is_empty()
}

impl App {
    /// Route a `winit` IME event. Commits write to the PTY; pre-edits update the
    /// inline composition; enable/disable edges clear stale state.
    pub(in crate::native) fn handle_ime(&mut self, ime: Ime) {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                self.end_ime_composition();
                self.set_ime_preedit(String::new());
            }
            Ime::Preedit(text, _cursor) => {
                let has_preedit = preedit_needs_cursor_area(&text);
                if !text.is_empty() {
                    // Some platforms do not expose every composition keystroke
                    // through KeyboardInput. A meaningful pre-edit still counts
                    // as typing for the cursor's visible activity hold.
                    self.note_cursor_keyboard_activity(std::time::Instant::now());
                }
                if text.is_empty() {
                    self.end_ime_composition();
                } else {
                    // Composition text is shown on the active pane, so a new
                    // composition starts there and settles any earlier one.
                    self.ime_settled_owner = None;
                    if self.ime_session.is_none() {
                        self.ime_session = Some(self.sessions.active_id());
                    }
                }
                self.set_ime_preedit(text);
                // KDE/Wayland can answer a cursor-area update with another
                // empty Preedit. Reissuing the update for that empty edge
                // creates an unbounded event feedback loop. Candidate-window
                // placement matters only while composition text exists.
                if has_preedit {
                    self.update_ime_cursor_area();
                }
            }
            Ime::Commit(text) => {
                let active = self.sessions.active_id();
                let origin = self.ime_session.take();
                let settled = self.ime_settled_owner.take();
                let accepts_commit = match origin {
                    Some(owner) => owner == active,
                    // A composition that ended on another pane can still
                    // deliver its commit late; it never reaches this pane.
                    None => settled.is_none_or(|owner| owner == active),
                };
                if !text.is_empty() && accepts_commit {
                    self.note_cursor_keyboard_activity(std::time::Instant::now());
                }
                self.set_ime_preedit(String::new());
                if accepts_commit {
                    self.commit_ime_text(&text);
                }
            }
        }
    }

    /// End the current composition (an empty pre-edit or an enable/disable
    /// edge). An owner other than the active pane is kept as settled, so a
    /// commit delivered after the edge is still refused on this pane until a
    /// new composition starts here.
    fn end_ime_composition(&mut self) {
        if let Some(owner) = self.ime_session.take()
            && owner != self.sessions.active_id()
        {
            self.ime_settled_owner = Some(owner);
        }
    }

    /// C9: finalize an IME commit under the SAME overlay/search/modal gate the
    /// typed-`Character` path enforces in `handle_key_event`. Without this, a
    /// composed commit (CJK, dead-key accents) bypasses every field and leaks
    /// straight to the PTY behind a settings panel, picker, search box, rename
    /// field, or modal. The finalized text is routed to whichever surface owns
    /// the keyboard, matching the key path's precedence (overlay → search →
    /// modal); only when the terminal itself has focus does it reach the shell.
    ///
    /// Returns nothing; empty commits are a no-op. Text is fed one char at a
    /// time to the overlay/modal (their winit mapper only accepts single-char
    /// `Character`s) and as a whole string to search (its handler iterates).
    fn commit_ime_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // A one-character commit of the latched letter is the chord glyph
        // Windows delivers after a consumed Ctrl+Shift letter. That includes
        // a commit that followed a one-character pre-edit of the same letter.
        // A longer commit, or a commit of a different character, still writes.
        if self.swallow_chord_ime_commit(text) {
            return;
        }
        if self.overlay.is_open() {
            for ch in text.chars() {
                self.handle_overlay_key(
                    &WinitKey::Character(ch.to_string().into()),
                    KeyEventType::Press,
                );
            }
            return;
        }
        if self.search.is_open() {
            self.handle_search_key(WinitKey::Character(text.into()));
            return;
        }
        let modal = self.active_modal();
        if modal != ActiveModal::None {
            for ch in text.chars() {
                self.route_modal_key(modal, &WinitKey::Character(ch.to_string().into()));
            }
            return;
        }
        // Terminal owns the keyboard: committed text reaches the shell exactly
        // like typed input - snap to the live tail, then write the UTF-8 bytes.
        self.write_ime_text_to_pty(text);
    }

    fn set_ime_preedit(&mut self, text: String) {
        if self.ime_preedit == text {
            return;
        }
        self.ime_preedit = text;
        // Composition changes the painted cells at the cursor, so force a full
        // rebuild and repaint next frame.
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Write finalized IME text to the active PTY (the terminal-focused path of
    /// `commit_ime_text`). Committed text reaches the shell exactly like typed
    /// input: snap to the live tail, then write the UTF-8 bytes through the
    /// active PTY writer.
    fn write_ime_text_to_pty(&mut self, text: &str) {
        if text.is_empty() || !self.active_pane_accepts_input() {
            return;
        }
        self.return_to_live();
        self.broadcast_bytes(text.as_bytes());
        let delivered = self.writer.lock().is_ok_and(|mut writer| {
            writer
                .write_all(text.as_bytes())
                .and_then(|()| writer.flush())
                .is_ok()
        });
        if !delivered {
            self.raise_input_not_delivered_notice();
            return;
        }
        self.surface_input_loss();
    }

    /// Best-effort placement of the IME candidate window at the terminal cursor.
    /// Skipped when the GPU (and thus cell metrics) is not yet present.
    fn update_ime_cursor_area(&self) {
        let (Some(window), Some(gpu)) = (self.window.as_ref(), self.gpu.as_ref()) else {
            return;
        };
        let cell = gpu.cell();
        let [x, y] = self.ime_cursor_area_origin_px(cell, gpu.window_padding().as_f32());
        window.set_ime_cursor_area(
            PhysicalPosition::new(x, y),
            PhysicalSize::new(cell.width, cell.height),
        );
    }

    /// Window pixel position of the cursor cell the candidate window anchors
    /// at. In a split, zoomed, stacked or floating tab this is the focused
    /// pane's drawn grid origin (which already includes padding and tab
    /// chrome) plus the pane-local cursor cell. A single-pane tab uses the
    /// window padding plus the tab-chrome offset (band cells plus the
    /// chrome-facing padding gap), both 0 with no chrome shown.
    pub(super) fn ime_cursor_area_origin_px(&self, cell: CellSize, pad: f32) -> [f32; 2] {
        let cursor = crate::native::lock_recover(&self.terminal)
            .snapshot()
            .cursor;
        let column = self.ime_anchor_column(cursor);
        let origin = if let Some((_, origin, _)) = self.focused_pane_grid() {
            origin
        } else {
            let (chrome_dx, chrome_dy) = self.tab_chrome_offset_px(cell);
            [pad + chrome_dx as f32, pad + chrome_dy as f32]
        };
        [
            origin[0] + column as f32 * cell.width as f32,
            origin[1] + cursor.row as f32 * cell.height as f32,
        ]
    }

    /// The screen column the candidate window anchors at: the column the
    /// presented frame drew the cursor cell at. That is the logical column
    /// unless bidi reordering reordered the cursor's row.
    pub(super) fn ime_anchor_column(&self, cursor: crate::core::Position) -> usize {
        self.bidi_visual_column(CellPoint {
            row: cursor.row,
            column: cursor.column,
        })
    }

    /// Render-cache fragment: the live pre-edit string while composing (changes
    /// every keystroke ⇒ Full repaint), `Inert` otherwise.
    pub(super) fn ime_overlay_signature(&self) -> OverlayFragment {
        if self.ime_preedit.is_empty() {
            OverlayFragment::Inert
        } else {
            OverlayFragment::ImePreedit {
                text: self.ime_preedit.clone(),
            }
        }
    }

    /// Paint the pre-edit string inline starting at the cursor cell, underlined
    /// so it reads as provisional. Clamped to the cursor row; no-op when no
    /// composition is in progress.
    ///
    /// The text is laid out by a scratch one-row terminal with the pane's
    /// ambiguous-width setting, so the preview owns cells exactly as the
    /// committed text will: a decomposed accent or emoji sequence is one owner
    /// with its marks retained, a wide owner carries a real wide tail, and an
    /// owner that does not fit before the right edge is left out whole.
    pub(in crate::native) fn paint_ime_preedit_cells(
        &self,
        snapshot: &mut Snapshot,
        ambiguous_wide: bool,
    ) {
        if self.ime_preedit.is_empty() {
            return;
        }
        let columns = snapshot.dimensions.columns;
        let row = snapshot.cursor.row;
        let start = snapshot.cursor.column;
        if columns == 0 || row >= snapshot.dimensions.rows || start >= columns {
            return;
        }
        let mut attrs = Attrs::default();
        attrs.underline_style = UnderlineStyle::Straight;
        let projected = project_preedit_cells(&self.ime_preedit, columns - start, ambiguous_wide);
        if projected.is_empty() {
            return;
        }
        let base = row * columns;
        let end = start + projected.len();
        // Covering one half of a wide glyph already on screen would leave the
        // other half drawing across the pre-edit: blank the uncovered half.
        if snapshot.cells[base + start].wide_continuation && start > 0 {
            let lead = &mut snapshot.cells[base + start - 1];
            *lead = Cell::new(' ', lead.attrs);
        }
        if end < columns && snapshot.cells[base + end].wide_continuation {
            let tail = &mut snapshot.cells[base + end];
            *tail = Cell::new(' ', tail.attrs);
        }
        for (offset, mut cell) in projected.into_iter().enumerate() {
            cell.attrs = attrs;
            snapshot.cells[base + start + offset] = cell;
        }
    }
}

/// Upper bound on pre-edit scalars laid out per frame. A composition is a few
/// characters; the cap only keeps a pathological input-method string from
/// costing more than one row's worth of work.
const PREEDIT_MAX_SCALARS: usize = 4096;

/// Lay `text` out the way the terminal will once it is committed, in at most
/// `columns` cells, and return the cells of the whole owners that fit.
/// Control characters are dropped, so nothing in the text acts as a sequence.
fn project_preedit_cells(text: &str, columns: usize, ambiguous_wide: bool) -> Vec<Cell> {
    if columns == 0 {
        return Vec::new();
    }
    let printable: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(PREEDIT_MAX_SCALARS)
        .collect();
    // Two spare columns let the last owner that crosses `columns` land whole
    // on the scratch row, so the cut below is always at an owner boundary.
    let width = columns + 2;
    let mut scratch = crate::core::Terminal::new(width, 2);
    scratch.set_ambiguous_wide(ambiguous_wide);
    scratch.advance(printable.as_bytes());
    let snapshot = scratch.snapshot();
    let written = if snapshot.cursor.row == 0 {
        snapshot.cursor.column
    } else {
        width
    };
    let row = &snapshot.cells[..width];
    let mut end = 0;
    while end < written.min(width) {
        let cell = row[end];
        if cell.layout_padding {
            break;
        }
        let owner = if end + 1 < width && row[end + 1].wide_continuation {
            2
        } else {
            1
        };
        if end + owner > columns {
            break;
        }
        end += owner;
    }
    row[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Terminal;

    const ROWS: usize = 6;
    const COLS: usize = 40;

    #[derive(Clone, Default)]
    struct RecordingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("bytes").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A headless App whose PTY writes are recorded, so each test can assert
    /// exactly what reached the shell.
    fn build_app() -> (App, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        let d = Dimensions::new(COLS, ROWS);
        let recorder = RecordingWriter::default();
        let written = recorder.0.clone();
        let writer: crate::native::pty::PtyWriter =
            std::sync::Arc::new(std::sync::Mutex::new(Box::new(recorder)));
        let (mut app, _terminal) = crate::native::test_support::headless_app_with_writer(
            crate::native::options::NativeOptions::default(),
            d,
            Settings::default(),
            writer,
        );
        app.grid = d;
        (app, written)
    }

    fn pty_bytes(written: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> Vec<u8> {
        written.lock().expect("bytes").clone()
    }

    #[test]
    fn no_composition_is_inert_and_paints_nothing() {
        let (app, _written) = build_app();
        assert!(app.ime_preedit.is_empty());
        assert_eq!(app.ime_overlay_signature(), OverlayFragment::Inert);
        let mut snapshot = Terminal::new(COLS, ROWS).snapshot();
        let before = snapshot.cells.clone();
        app.paint_ime_preedit_cells(&mut snapshot, false);
        assert_eq!(snapshot.cells, before, "off path leaves the grid untouched");
    }

    #[test]
    fn preedit_stores_and_paints_at_cursor_then_commit_clears() {
        let (mut app, written) = build_app();
        app.handle_ime(Ime::Preedit("ab".to_owned(), Some((2, 2))));
        assert_eq!(app.ime_preedit, "ab");
        assert!(matches!(
            app.ime_overlay_signature(),
            OverlayFragment::ImePreedit { .. }
        ));

        let mut snapshot = Terminal::new(COLS, ROWS).snapshot();
        app.paint_ime_preedit_cells(&mut snapshot, false);
        assert_eq!(snapshot.cells[0].ch, 'a');
        assert_eq!(snapshot.cells[1].ch, 'b');
        assert_eq!(
            snapshot.cells[0].attrs.underline_style,
            UnderlineStyle::Straight,
            "pre-edit reads as provisional via underline"
        );

        assert!(
            pty_bytes(&written).is_empty(),
            "a pre-edit never reaches the PTY"
        );
        // Commit clears the pre-edit and the bytes go to the PTY.
        app.handle_ime(Ime::Commit("ab".to_owned()));
        assert!(app.ime_preedit.is_empty());
        assert_eq!(pty_bytes(&written), b"ab");
        assert_eq!(app.ime_overlay_signature(), OverlayFragment::Inert);
    }

    /// The pre-edit preview owns cells exactly as the committed text will:
    /// each case is painted at the cursor and compared, cell by cell (glyph,
    /// retained marks, wide tail), with a terminal that received the text.
    #[test]
    fn preedit_preview_matches_the_committed_layout() {
        let (mut app, _written) = build_app();
        for text in [
            "e\u{301}x",
            "\u{1f469}\u{200d}\u{1f4bb}x",
            "\u{915}\u{94d}\u{937}x",
            "\u{e01}\u{e33}x",
            "\u{2764}\u{fe0f}x",
            "\u{4e00}x",
        ] {
            app.handle_ime(Ime::Preedit(text.to_owned(), None));
            let mut snapshot = Terminal::new(COLS, ROWS).snapshot();
            app.paint_ime_preedit_cells(&mut snapshot, false);
            let mut committed = Terminal::new(COLS, ROWS);
            committed.advance(text.as_bytes());
            let committed = committed.snapshot();
            for column in 0..6 {
                let (painted, expected) = (snapshot.cells[column], committed.cells[column]);
                assert_eq!(painted.ch, expected.ch, "{text:?} column {column}");
                assert_eq!(painted.combining(), expected.combining(), "{text:?} marks");
                assert_eq!(
                    painted.wide_continuation, expected.wide_continuation,
                    "{text:?} wide tail at {column}"
                );
            }
        }
    }

    /// At the right edge a wide owner that does not fit is left out whole, and
    /// a pre-edit that starts on, or ends before, half of an existing wide
    /// glyph blanks the other half instead of leaving it drawing across.
    #[test]
    fn preedit_preview_keeps_owners_whole_at_edges() {
        let (mut app, _written) = build_app();
        app.handle_ime(Ime::Preedit("a\u{4e00}".to_owned(), None));
        let mut terminal = Terminal::new(COLS, ROWS);
        terminal.advance(format!("\x1b[1;{}H", COLS - 1).as_bytes());
        let mut snapshot = terminal.snapshot();
        app.paint_ime_preedit_cells(&mut snapshot, false);
        assert_eq!(snapshot.cells[COLS - 2].ch, 'a');
        assert_eq!(
            snapshot.cells[COLS - 1].ch,
            ' ',
            "the wide owner is left out"
        );
        assert!(!snapshot.cells[COLS - 1].wide_continuation);

        // Existing wide glyphs at columns 0-1 and 3-4; the cursor sits on the
        // first glyph's tail, and the two-cell pre-edit stops before the second.
        app.handle_ime(Ime::Preedit("ab".to_owned(), None));
        let mut terminal = Terminal::new(COLS, ROWS);
        terminal.advance("\u{4e00}-\u{4e8c}\x1b[1;2H".as_bytes());
        let mut snapshot = terminal.snapshot();
        assert!(snapshot.cells[1].wide_continuation);
        app.paint_ime_preedit_cells(&mut snapshot, false);
        assert_eq!(snapshot.cells[0].ch, ' ', "the uncovered lead is blanked");
        assert_eq!(snapshot.cells[1].ch, 'a');
        assert_eq!(snapshot.cells[2].ch, 'b');
        assert_eq!(snapshot.cells[3].ch, '\u{4e8c}', "an untouched owner stays");
        assert!(snapshot.cells[4].wide_continuation);

        // A one-cell pre-edit on the second glyph's lead blanks its tail.
        app.handle_ime(Ime::Preedit("a".to_owned(), None));
        let mut terminal = Terminal::new(COLS, ROWS);
        terminal.advance("\u{4e00}-\u{4e8c}\x1b[1;4H".as_bytes());
        let mut snapshot = terminal.snapshot();
        app.paint_ime_preedit_cells(&mut snapshot, false);
        assert_eq!(snapshot.cells[3].ch, 'a');
        assert_eq!(snapshot.cells[4].ch, ' ', "the uncovered tail is blanked");
        assert!(!snapshot.cells[4].wide_continuation);
    }

    #[test]
    fn disable_clears_stale_preedit() {
        let (mut app, written) = build_app();
        app.handle_ime(Ime::Preedit("x".to_owned(), None));
        assert_eq!(app.ime_preedit, "x");
        app.handle_ime(Ime::Disabled);
        assert!(
            app.ime_preedit.is_empty(),
            "a cancelled IME leaves no ghost"
        );
        assert!(
            pty_bytes(&written).is_empty(),
            "a cancelled pre-edit writes nothing"
        );
    }

    #[test]
    fn meaningful_ime_activity_rearms_cursor_visibility_without_empty_edges() {
        let (mut app, _written) = build_app();

        app.cursor_blink.park();
        app.handle_ime(Ime::Enabled);
        app.handle_ime(Ime::Preedit(String::new(), None));
        assert_eq!(
            app.cursor_blink.deadline(),
            None,
            "IME lifecycle edges and an empty pre-edit are not keyboard activity"
        );

        app.handle_ime(Ime::Preedit("x".to_owned(), None));
        assert!(
            app.cursor_blink.deadline().is_some(),
            "a nonempty pre-edit keeps a blinking cursor visible"
        );

        app.cursor_blink.park();
        app.handle_ime(Ime::Commit(String::new()));
        assert_eq!(
            app.cursor_blink.deadline(),
            None,
            "an empty commit cannot arm a wake"
        );
        app.handle_ime(Ime::Commit("x".to_owned()));
        assert!(
            app.cursor_blink.deadline().is_some(),
            "a committed composition re-arms the visible hold"
        );

        app.cursor_blink.park();
        app.ime_session = Some(SessionToken(99));
        app.handle_ime(Ime::Commit("late".to_owned()));
        assert_eq!(
            app.cursor_blink.deadline(),
            None,
            "a delayed commit from another pane cannot wake the active cursor"
        );
    }

    #[test]
    fn empty_preedit_does_not_require_candidate_window_positioning() {
        assert!(!preedit_needs_cursor_area(""));
        assert!(preedit_needs_cursor_area("x"));
    }

    #[test]
    fn ime_commit_routes_to_search_box_not_pty() {
        // C9: with the search box open, an IME commit must land in the search
        // field - not leak to the shell behind it. Before the fix the finalized
        // text went straight to the PTY and the query stayed empty.
        let (mut app, written) = build_app();
        app.open_search_for_test();
        assert!(app.search_open_for_test());
        assert_eq!(app.search_query_for_test(), "");

        app.handle_ime(Ime::Commit("hi".to_owned()));

        assert_eq!(
            app.search_query_for_test(),
            "hi",
            "IME commit must feed the open search field, not the PTY"
        );
        assert!(pty_bytes(&written).is_empty(), "nothing leaks to the shell");
    }

    #[test]
    fn ime_commit_routes_to_open_overlay_filter_not_pty() {
        // C9: with an overlay open (here the connection manager's type-to-filter
        // list), an IME commit must drive the overlay's filter - not leak to the
        // shell behind it. Committing "01" lands in the overlay's query box; its
        // "> 01" prompt line proves the commit reached the overlay. Before the
        // fix the finalized text went to the PTY and the query stayed empty.
        let (mut app, written) = build_app();
        app.open_connections_with_synthetic_hosts_for_test(3);
        assert!(app.overlay_open_for_test());

        app.handle_ime(Ime::Commit("01".to_owned()));

        let rows = app.render_overlay_rows_for_test(COLS, ROWS);
        // The type-to-filter prompt row (rendered "> <query>") is distinct from
        // any host row, so matching "> 01" isolates the query text from host
        // aliases that may also contain "01".
        assert!(
            rows.iter().any(|row| row.contains("> 01")),
            "IME commit must land in the overlay's type-to-filter query, not the \
             PTY behind it; rows: {rows:?}"
        );
        assert!(pty_bytes(&written).is_empty(), "nothing leaks to the shell");
    }
}
