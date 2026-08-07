// SPDX-License-Identifier: GPL-3.0-only
//! Mouse and focus protocol encoding for the native app, plus the single PTY
//! write seam the pointer paths use.
//!
//! Owns the protocol and mode gates, SGR-pixel coordinate conversion, mouse and
//! focus report emission, and click-to-position travel encoding.

use super::*;

impl App {
    pub(super) fn mouse_protocol(&self) -> MouseProtocol {
        self.terminal
            .lock()
            .map(|terminal| terminal.mouse_protocol())
            .unwrap_or_default()
    }

    pub(super) fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_protocol().is_enabled()
    }

    /// Shift is the local-selection escape hatch while a TUI has enabled mouse
    /// reporting, matching the common xterm-family terminal convention.
    pub(super) fn should_report_mouse_to_pty(&self) -> bool {
        self.mouse_reporting_enabled() && !self.modifiers.shift
    }

    pub(super) fn write_pty_bytes(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    pub(super) fn send_mouse_report(
        &mut self,
        button: CoreMouseButton,
        kind: MouseEventKind,
    ) -> bool {
        let protocol = self.mouse_protocol();
        // SGR-pixel (1016) reports true 1-based physical pixel coordinates; every
        // other encoding (legacy/UTF-8/SGR/urxvt) reports cells. Only 1016 takes
        // the pixel seam — the cell path is untouched for all other modes.
        let bytes = if protocol.encoding == MouseEncoding::SgrPixel {
            self.encode_pixel_mouse_report(protocol, button, kind)
        } else {
            self.pointer_cell.and_then(|point| {
                encode_native_mouse_report(protocol, point, button, kind, self.modifiers)
            })
        };
        let Some(bytes) = bytes else {
            return false;
        };

        self.return_to_live();
        self.write_pty_bytes(&bytes);
        true
    }

    /// Encode an SGR-pixel (1016) mouse report from the cached physical pointer
    /// position. Returns `None` until a cursor position and GPU cell metrics are
    /// available, or when the active tracking gate drops the event (the core
    /// encoder applies the same gating as the cell path). The cached position is
    /// window-absolute, so it is first mapped grid-relative the same way the cell
    /// path maps it (subtract the tab-chrome offset on a single-pane tab, or the
    /// focused pane's rect origin in a multi-pane tab); [`pixel_coords_for_report`]
    /// then floors it to a 1-based pixel and clamps to the grid's pixel extent
    /// after removing any window padding.
    pub(super) fn encode_pixel_mouse_report(
        &self,
        protocol: MouseProtocol,
        button: CoreMouseButton,
        kind: MouseEventKind,
    ) -> Option<Vec<u8>> {
        let (x_px, y_px) = self.pointer_px?;
        let gpu = self.gpu.as_ref()?;
        let cell = gpu.cell();
        // Map the absolute pointer into grid-relative pixels, mirroring the cell
        // path: a top bar / left rail shifts the grid origin, and in a multi-pane
        // tab the focused pane's content rect is offset from the window origin.
        // Without this the SGR-pixel (1016) report would leak the chrome / pane
        // offset to the application (a left rail reporting X too large, a top bar
        // reporting Y too large).
        let (px, py) = if let Some((rect, _)) = self.focused_pane_inner_rect() {
            // PANE-PADDING: the focused pane's PADDED rect origin already folds in
            // the tab-chrome, window padding, AND the per-divider inset, so the
            // report maps to the same cell the glyph renders at (parity with
            // `pane_relative_cell`); no separate padding subtraction here.
            pixel_coords_for_report(
                x_px - f64::from(rect.x),
                y_px - f64::from(rect.y),
                cell,
                self.grid,
                WindowPadding::ZERO,
            )
        } else {
            // Single-pane: subtract the tab-chrome offset (both 0 on the plain
            // path, keeping it byte-identical there); padding is removed inside
            // `pixel_coords_for_report`, matching the cell path.
            let (chrome_dx, chrome_dy) = self.tab_chrome_offset_px(cell);
            pixel_coords_for_report(
                x_px - chrome_dx,
                y_px - chrome_dy,
                cell,
                self.grid,
                gpu.window_padding(),
            )
        };
        let mods = MouseModifiers {
            // Shift stays reserved for local selection while reporting is active,
            // matching the cell path's modifier policy.
            shift: false,
            alt: self.modifiers.alt,
            ctrl: self.modifiers.ctrl,
        };
        encode_mouse_event_pixel(protocol, button, kind, px, py, mods)
    }

    pub(super) fn send_mouse_motion_report(&mut self) {
        let protocol = self.mouse_protocol();
        let Some(button) = motion_report_button(protocol, self.report_button) else {
            return;
        };
        let _ = self.send_mouse_report(button, MouseEventKind::Motion);
    }

    pub(super) fn send_focus_report(&mut self, focused: bool) {
        let Some(bytes) = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| encode_native_focus_report(&terminal, focused))
        else {
            return;
        };

        self.write_pty_bytes(&bytes);
    }

    pub(super) fn handle_reported_mouse_input(
        &mut self,
        state: ElementState,
        button: CoreMouseButton,
    ) {
        match state {
            ElementState::Pressed => {
                self.report_button = Some(button);
                let _ = self.send_mouse_report(button, MouseEventKind::Press);
            }
            ElementState::Released => {
                let _ = self.send_mouse_report(button, MouseEventKind::Release);
                if self.report_button == Some(button) {
                    self.report_button = None;
                }
            }
        }
    }

    pub(super) fn handle_reported_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        // WHEEL-SENS (T-overlay decision, TUI arm): coalesce the burst so a
        // high-resolution scroll emits one wheel report per physical notch
        // rather than one per sub-notch event (which would fly a TUI pager). The
        // report protocol carries only a discrete up/down button — sign, not
        // magnitude — so we emit a single report per accumulated notch and
        // deliberately do NOT apply the user's `scroll_wheel_lines` multiplier
        // (the app owns its own line count). A clean `LineDelta(_, ±1.0)` still
        // yields exactly one report, byte-identical to before.
        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        let Some(notch) = self.wheel_accum.coalesce_scroll(delta, cell_height) else {
            return false;
        };
        let Some(button) = wheel_report_button(notch) else {
            return false;
        };
        self.send_mouse_report(button, MouseEventKind::Press)
    }
}

/// Convert a physical cursor pixel position to 1-based terminal pixel
/// coordinates for SGR-pixel (1016) mouse reporting, clamped to the grid's
/// pixel extent.
///
/// `x_px`/`y_px` are the raw `winit` `CursorMoved` coordinates, which are
/// already physical pixels; `CellSize` and `padding` are likewise
/// physical-pixel sized. The result first subtracts the window padding to get
/// grid-relative coordinates, then floors to an integer pixel and shifts to the
/// 1-based convention the protocol uses. A cursor left of or above the grid
/// clamps to pixel 1; a cursor at or past the right/bottom edge (e.g. while
/// dragging outside the window) clamps to the last in-grid pixel, mirroring how
/// [`selection::cell_at_physical_with_padding`] saturates the cell path.
/// SH-CLICK (F2): resolve a plain click against the core-derived
/// [`InputRegion`](crate::core::InputRegion) into a signed glyph delta —
/// how many Left (negative) or Right (positive) presses move the shell's
/// line-editor caret from the cursor to the clicked position. `None` means
/// no travel: the click was off the region, on the prompt side of the input
/// start, on an untrustworthy geometry, or on the cursor's own position.
///
/// Certainty ladder (F2 §3):
/// - `Unknown` (stale mark, or a hard-newline multi-logical-line buffer per
///   the signal's `nl=` offsets) → `None`. Left/Right DO cross hard newlines
///   in every editor, but the continuation-prompt geometry (PS2 / `>>` /
///   fish indent) is unmodeled, so an exact count is not computable — v1
///   no-ops rather than landing the caret on the wrong logical line.
/// - `Exact` (fresh private edit-region signal) → rune-precise travel over
///   the reconciled `row_spans` (wrap fillers excluded), including soft-wrap
///   multi-row travel.
/// - `RightEdgeUnknown` (bash / PowerShell / fish mid-edit) → grapheme-cell
///   heuristic over the region bounds, also multi-row across soft wraps.
///   Off-by-one is tolerable here because motion is NON-destructive and
///   every editor clamps the caret at the buffer ends: a mis-land is a
///   click-again, never a wrong edit (contrast select+Delete's charter).
///
/// Click mapping within the region's rows: a click left of a row's input
/// span start (the prompt) is a no-op (F2 G2 — the shipped code walked the
/// caret to buffer position 0 there); a click at or right of the span end
/// clamps to the end of that row's input (a decoration/autosuggestion click
/// moves the caret to the true input end, extra motion absorbed by the
/// shell's own clamp). Glyph counting skips wide-glyph continuation cells,
/// so one wide glyph is one press — the shipped raw-cell delta over-sent
/// arrows on CJK/emoji lines (F2-NF1).
///
/// `subcell_round_up` is the half-cell (nearest-boundary) target: when the
/// click fell in the right half of its cell the caret targets the NEXT column
/// boundary (after the glyph), else the current one (before it). The
/// prompt-side no-op still tests the floored `click.column`, so rounding up
/// never crosses the input start; `flat_at` clamps the target, so a right-half
/// click on the last glyph resolves to the append origin.
///
/// Pure and GPU-free; `click` and `cursor` are in visible-viewport
/// coordinates, the region is absolute (offset by `scrollback_len`).
pub(super) fn click_travel_delta(
    snapshot: &Snapshot,
    region: &crate::core::InputRegion,
    click: CellPoint,
    subcell_round_up: bool,
    cursor: Position,
    scrollback_len: usize,
    grid_rows: usize,
) -> Option<i32> {
    use super::selection_input::snapshot_row_cell_count;
    if region.certainty == crate::core::InputCertainty::Unknown {
        return None;
    }
    let base_visible = region.start_row.checked_sub(scrollback_len)?;
    let row_count = region.end_row - region.start_row + 1;
    if base_visible + row_count > grid_rows {
        return None;
    }
    // F2 G2: the click must land on the region's rows.
    if click.row < base_visible || click.row >= base_visible + row_count {
        return None;
    }
    let columns = snapshot.dimensions.columns;
    if columns == 0 {
        return None;
    }
    // Per-row input spans `(start_col, end_col_exclusive)`: authoritative under
    // Exact (core's reconciled rune walk); reconstructed from the region bounds
    // under RightEdgeUnknown (row 0 starts at the `B` mark, wrapped
    // continuation rows span the full width, the last row ends at the
    // heuristic edge).
    let spans: Vec<(usize, usize)> = if region.certainty == crate::core::InputCertainty::Exact
        && region.row_spans.len() == row_count
    {
        region.row_spans.clone()
    } else {
        (0..row_count)
            .map(|rel| {
                let start = if rel == 0 { region.start_col } else { 0 };
                let end = if rel == row_count - 1 {
                    region.end_col.min(columns)
                } else {
                    columns
                };
                (start, end)
            })
            .collect()
    };
    // Flattened glyph offset at each row's span start (soft wraps carry no
    // newline, so the spans concatenate into one logical horizontal axis —
    // same flatten as the R5 delete rung).
    let mut prefix = Vec::with_capacity(row_count);
    let mut total = 0usize;
    for (rel, &(start, end)) in spans.iter().enumerate() {
        prefix.push(total);
        if start < end {
            total += snapshot_row_cell_count(snapshot, base_visible + rel, start, end - 1);
        }
    }
    // Flattened glyph offset of a caret position on a region row: glyphs
    // between the span start and `col`, clamped right to the span end.
    let flat_at = |row_rel: usize, col: usize| -> usize {
        let (start, end) = spans[row_rel];
        let col = col.clamp(start, end);
        prefix[row_rel]
            + if col > start {
                snapshot_row_cell_count(snapshot, base_visible + row_rel, start, col - 1)
            } else {
                0
            }
    };
    let click_rel = click.row - base_visible;
    // Prompt-side click (left of the input start on its row): a proper no-op,
    // never a caret walk to buffer position 0. The guard tests the LITERAL cell
    // the pixel is over (the floored `click.column`), NOT the nearest-boundary
    // target below, so a right-half click on the last prompt cell stays a no-op
    // and never rounds up across the input start into a bogus travel.
    if click.column < spans[click_rel].0 {
        return None;
    }
    // HALF-CELL (nearest-boundary) caret targeting: a click that fell in the
    // right half of its cell (`subcell_round_up`) targets the NEXT column
    // boundary — the caret AFTER that glyph — while a left-half click targets
    // the current column (caret BEFORE it), matching universal text-editor
    // hit-testing. Flooring to the cell's left edge (still used when no sub-cell
    // fraction is available) lands the caret one cell left of a click that fell
    // a hair past a cell boundary. `flat_at` clamps the target to the span end,
    // so a right-half click on the last input glyph resolves to the append
    // origin, never past it; a wide glyph's continuation cell flattens to the
    // same offset as its lead, so a right-half click on a 2-cell glyph lands the
    // caret after the whole glyph.
    let target_col = if subcell_round_up {
        click.column + 1
    } else {
        click.column
    };
    let target = flat_at(click_rel, target_col);
    // The cursor sits on the region's rows whenever certainty != Unknown; a
    // disagreement here means the region and grid raced — degrade to no-op.
    let cursor_rel = cursor.row.checked_sub(base_visible)?;
    if cursor_rel >= row_count {
        return None;
    }
    let cursor_flat = flat_at(cursor_rel, cursor.column);
    let delta = i32::try_from(target).ok()? - i32::try_from(cursor_flat).ok()?;
    if delta == 0 { None } else { Some(delta) }
}

/// SH-CLICK: encode the cursor-positioning key burst for a click-to-position
/// travel delta — `|delta|` repetitions of Left (negative delta) or Right
/// (positive delta), each encoded through the live [`KeyModes`] so a shell in
/// DECCKM application-cursor mode receives the SS3 form (`\x1bOC`/`\x1bOD`), not
/// the CSI form (`\x1b[C`/`\x1b[D`). This is the load-bearing encoding trap:
/// hardcoded CSI arrows would move the cursor wrong (or not at all) in zsh/zle,
/// fish, and readline shells that run in application-cursor mode, so the bytes
/// MUST be identical to a real arrow keypress in every mode.
///
/// Pure and total: returns the exact bytes the PTY writer receives. A
/// zero delta cannot reach here ([`click_travel_delta`] returns `None` for a
/// same-position click), so `unsigned_abs` never overflows.
pub(super) fn click_position_bytes(delta: i32, modes: KeyModes) -> Vec<u8> {
    let (key, count) = if delta < 0 {
        (Key::Left, delta.unsigned_abs() as usize)
    } else {
        (Key::Right, delta as usize)
    };
    let arrow = input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Press);
    arrow.repeat(count)
}

pub(super) fn pixel_coords_for_report(
    x_px: f64,
    y_px: f64,
    cell: CellSize,
    dims: Dimensions,
    padding: WindowPadding,
) -> (usize, usize) {
    let max_px = (dims.columns as u32)
        .saturating_mul(cell.width.max(1))
        .max(1);
    let max_py = (dims.rows as u32).saturating_mul(cell.height.max(1)).max(1);
    let pad = f64::from(padding.physical_px());
    let px = ((x_px - pad).max(0.0) as u32).min(max_px - 1) as usize + 1;
    let py = ((y_px - pad).max(0.0) as u32).min(max_py - 1) as usize + 1;
    (px, py)
}

#[cfg(test)]
mod tests;
