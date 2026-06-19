// SPDX-License-Identifier: GPL-3.0-only
//! IME (input-method) composition. `winit` delivers four `Ime` events once IME
//! is allowed (see `App::resumed`):
//!
//! - **Enabled / Disabled** — composition session bracket; we clear any stale
//!   pre-edit on either edge so a cancelled composition leaves no ghost text.
//! - **Preedit(text, cursor)** — the in-progress composition. Stored in
//!   [`App::ime_preedit`] and rendered inline at the terminal cursor with an
//!   underline; never sent to the PTY.
//! - **Commit(text)** — the finalized string. Written to the active PTY exactly
//!   like typed `Character` input, and the pre-edit is cleared.
//!
//! Off-path contract: with no composition in progress `ime_preedit` is empty,
//! [`App::ime_overlay_signature`] is `Inert`, and
//! [`App::paint_ime_preedit_cells`] writes nothing — the default render path is
//! unchanged.

use unicode_width::UnicodeWidthChar;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Ime;

use crate::core::{Attrs, Cell, Snapshot, UnderlineStyle};

use super::*;

impl App {
    /// Route a `winit` IME event. Commits write to the PTY; pre-edits update the
    /// inline composition; enable/disable edges clear stale state.
    pub(in crate::native) fn handle_ime(&mut self, ime: Ime) {
        match ime {
            Ime::Enabled | Ime::Disabled => {
                self.set_ime_preedit(String::new());
            }
            Ime::Preedit(text, _cursor) => {
                self.set_ime_preedit(text);
                self.update_ime_cursor_area();
            }
            Ime::Commit(text) => {
                self.set_ime_preedit(String::new());
                self.commit_ime_text(&text);
            }
        }
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

    fn commit_ime_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Committed text reaches the shell exactly like typed input: snap to the
        // live tail, then write the UTF-8 bytes through the active PTY writer.
        self.return_to_live();
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(text.as_bytes());
            let _ = writer.flush();
        }
    }

    /// Best-effort placement of the IME candidate window at the terminal cursor.
    /// Skipped when the GPU (and thus cell metrics) is not yet present.
    fn update_ime_cursor_area(&self) {
        let (Some(window), Some(gpu)) = (self.window.as_ref(), self.gpu.as_ref()) else {
            return;
        };
        let cell = gpu.cell();
        let pad = gpu.window_padding().as_f32();
        let cursor = self
            .terminal
            .lock()
            .ok()
            .map(|terminal| terminal.snapshot().cursor)
            .unwrap_or_default();
        let x = pad + cursor.column as f32 * cell.width as f32;
        let y = pad + cursor.row as f32 * cell.height as f32;
        window.set_ime_cursor_area(
            PhysicalPosition::new(x, y),
            PhysicalSize::new(cell.width, cell.height),
        );
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
    pub(in crate::native) fn paint_ime_preedit_cells(&self, snapshot: &mut Snapshot) {
        if self.ime_preedit.is_empty() {
            return;
        }
        let columns = snapshot.dimensions.columns;
        let row = snapshot.cursor.row;
        if columns == 0 || row >= snapshot.dimensions.rows {
            return;
        }
        let mut attrs = Attrs::default();
        attrs.underline_style = UnderlineStyle::Straight;
        let mut x = snapshot.cursor.column;
        for ch in self.ime_preedit.chars() {
            if ch.is_control() {
                continue;
            }
            let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            if x + width > columns {
                break;
            }
            snapshot.cells[row * columns + x] = Cell::new(ch, attrs);
            if width == 2 && x + 1 < columns {
                snapshot.cells[row * columns + x + 1] = Cell::new(' ', attrs);
            }
            x += width;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Terminal;
    use std::sync::{Arc, Mutex};

    const ROWS: usize = 6;
    const COLS: usize = 40;

    fn build_app() -> Option<App> {
        let d = Dimensions::new(COLS, ROWS);
        let session = crate::pty::PtySession::spawn_shell_command(d, "sleep 1").ok()?;
        let writer: crate::native::pty::PtyWriter =
            Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(d.columns, d.rows)));
        let pty = Arc::new(Mutex::new(session));
        let mut app = App::new(
            crate::native::options::NativeOptions::default(),
            terminal,
            writer,
            pty,
            Settings::default(),
            crate::settings::SettingsReloader::for_current_process(Instant::now()),
        );
        app.grid = d;
        Some(app)
    }

    #[test]
    fn no_composition_is_inert_and_paints_nothing() {
        let Some(app) = build_app() else {
            return;
        };
        assert!(app.ime_preedit.is_empty());
        assert_eq!(app.ime_overlay_signature(), OverlayFragment::Inert);
        let mut snapshot = Terminal::new(COLS, ROWS).snapshot();
        let before = snapshot.cells.clone();
        app.paint_ime_preedit_cells(&mut snapshot);
        assert_eq!(snapshot.cells, before, "off path leaves the grid untouched");
    }

    #[test]
    fn preedit_stores_and_paints_at_cursor_then_commit_clears() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.handle_ime(Ime::Preedit("ab".to_owned(), Some((2, 2))));
        assert_eq!(app.ime_preedit, "ab");
        assert!(matches!(
            app.ime_overlay_signature(),
            OverlayFragment::ImePreedit { .. }
        ));

        let mut snapshot = Terminal::new(COLS, ROWS).snapshot();
        app.paint_ime_preedit_cells(&mut snapshot);
        assert_eq!(snapshot.cells[0].ch, 'a');
        assert_eq!(snapshot.cells[1].ch, 'b');
        assert_eq!(
            snapshot.cells[0].attrs.underline_style,
            UnderlineStyle::Straight,
            "pre-edit reads as provisional via underline"
        );

        // Commit clears the pre-edit (the bytes go to the PTY).
        app.handle_ime(Ime::Commit("ab".to_owned()));
        assert!(app.ime_preedit.is_empty());
        assert_eq!(app.ime_overlay_signature(), OverlayFragment::Inert);
    }

    #[test]
    fn disable_clears_stale_preedit() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.handle_ime(Ime::Preedit("x".to_owned(), None));
        assert_eq!(app.ime_preedit, "x");
        app.handle_ime(Ime::Disabled);
        assert!(
            app.ime_preedit.is_empty(),
            "a cancelled IME leaves no ghost"
        );
    }
}
