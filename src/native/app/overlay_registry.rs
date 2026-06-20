// SPDX-License-Identifier: GPL-3.0-only
//! Render-overlay registry + modal-input gate — the `app/mod.rs` chokepoint
//! dissolver (Wave-15 foundation).
//!
//! Two registries built on the same dissolving pattern: `app/mod.rs` holds one
//! *stable* call site, and the per-feature logic lives behind `pub(in
//! crate::native)` / `pub(super)` methods in disjoint submodules, each a no-op
//! while its feature is inactive.
//!
//! - **Frame-overlay registry** — a fixed-ordered manifest of `paint_*_cells`
//!   (snapshot cell mutators) and `paint_*_quads` (solid-quad emitters). The
//!   four existing paints (selection → search → overlay → hyperlink) and the two
//!   existing quad sources (scroll-indicator, status gutter) are relocated
//!   behind wrappers here; the new contributor slots (hints / copy-mode /
//!   cursor-trail / background) are no-ops until their feature packet fills in
//!   the body in its own submodule.
//! - **Modal-input gate** — [`ActiveModal`] plus [`App::active_modal`],
//!   [`App::route_modal_key`], and [`App::modal_captures_pointer`]. Today every
//!   modal predicate is `false`, so the gate routes nothing and the key/pointer
//!   paths are byte-identical to before it existed.
//!
//! Off-path contract (provable by construction): with no modal active and no
//! contributor active, every `paint_*` early-returns or mutates nothing, the
//! composite signature is constant-`Inert`, and `active_modal() == None` routes
//! nothing — so the frame bytes and the input routing are identical to HEAD.

use super::*;
use crate::core::{Attrs, Cell};
use unicode_width::UnicodeWidthChar;

/// ID1 v1 cursor-glow halo rings as `(extend_px, alpha)`, emitted outer-first so
/// the more opaque inner rings composite over the faint outer one. The pixel
/// extents reach at most a half-cell beyond the cursor and the alphas are capped
/// at `0.13`, which keeps adjacent-cell text within the RV1 floor safety
/// threshold (D-GLOW-2 / D-GLOW-6).
const CURSOR_GLOW_RINGS: [(f32, f32); 3] = [(8.0, 0.05), (4.0, 0.09), (1.0, 0.13)];

/// Read-only inputs every paint contributor needs (today threaded
/// individually). Built once per frame by [`App::overlay_ctx`].
#[derive(Debug, Clone, Copy)]
pub(in crate::native) struct OverlayCtx {
    pub(in crate::native) viewport_offset: usize,
    pub(in crate::native) scrollback_len: usize,
    pub(in crate::native) grid: Dimensions,
    pub(in crate::native) cell: CellSize,
    pub(in crate::native) window_padding: WindowPadding,
    /// Cursor cell position (viewport coordinates) for cursor-layer overlay
    /// contributors (ID1 glow, VE4 trail). Captured from the post-blink-resolve
    /// snapshot at the frame-composition call site.
    pub(in crate::native) cursor: Position,
    /// Whether the cursor is drawn this frame (already folds the blink
    /// off-phase). Cursor-layer overlays gate on this so the glow hides exactly
    /// when the cursor block does.
    pub(in crate::native) cursor_visible: bool,
    /// Frame composition instant, injected so per-frame overlay animations
    /// (VE4 new-output fade) advance from a single coherent clock.
    pub(in crate::native) now: Instant,
    /// Active theme background clear color in linear RGB (alpha `1.0`). VE4's
    /// fade quad is painted in this color so it is seamless against the frame
    /// clear; `[0.0; 4]` when the GPU is not yet present (the caller has
    /// already early-returned in that case, so this is just a total default).
    pub(in crate::native) clear_color: [f32; 4],
    /// Surface DPI scale factor (logical→physical px). Chrome thicknesses
    /// authored in logical pixels (ID4 window border) are multiplied by this so
    /// the frame is a consistent visual weight across displays. `1.0` when the
    /// GPU is not yet present (the caller has already early-returned then).
    pub(in crate::native) scale: f32,
}

/// The currently-active keyboard modal, if any. Extended as new modals land;
/// overlay/search are deliberately NOT modals here (D-INFRA-3) — they keep
/// their own `open()` semantics and the gate sits beneath them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum ActiveModal {
    None,
    CopyMode,
    HintsSelect,
    RenameTab,
}

impl App {
    /// Build the per-frame overlay context. `cell` is the already-resolved GPU
    /// cell size; the window padding is read from the live GPU state (which is
    /// always present at the frame-composition call site — the caller has
    /// already early-returned when it is absent), defaulting to zero so the
    /// builder is total.
    pub(in crate::native) fn overlay_ctx(
        &self,
        scrollback_len: usize,
        cell: CellSize,
        cursor: Position,
        cursor_visible: bool,
        now: Instant,
    ) -> OverlayCtx {
        OverlayCtx {
            viewport_offset: self.viewport.offset(),
            scrollback_len,
            grid: self.grid,
            cell,
            window_padding: self
                .gpu
                .as_ref()
                .map(GpuState::window_padding)
                .unwrap_or_default(),
            cursor,
            cursor_visible,
            now,
            clear_color: self
                .gpu
                .as_ref()
                .map(GpuState::clear_color_linear)
                .unwrap_or([0.0; 4]),
            scale: self.gpu.as_ref().map(GpuState::scale).unwrap_or(1.0),
        }
    }

    // --- cell-paints (order = paint precedence; a later slot overwrites an
    //     earlier one on a shared cell) ---------------------------------------

    /// Selection highlight (relocated `apply_selection_highlight`). Keeps the
    /// `if let Some(range)` guard internally so block-ness and the viewport
    /// arguments are unchanged from the inline form.
    pub(in crate::native) fn paint_selection_cells(
        &self,
        snapshot: &mut Snapshot,
        ctx: &OverlayCtx,
    ) {
        if let Some(range) = self.selection.range() {
            selection::apply_selection_highlight(
                snapshot,
                range,
                self.selection_block,
                ctx.viewport_offset,
                ctx.scrollback_len,
                ctx.grid,
                self.themed_selection_style(),
            );
        }
    }

    /// Search-match highlight (relocated `apply_search_ui`).
    pub(in crate::native) fn paint_search_cells(&self, snapshot: &mut Snapshot, ctx: &OverlayCtx) {
        apply_search_ui(
            snapshot,
            &self.search,
            ctx.viewport_offset,
            ctx.scrollback_len,
            ctx.grid,
            self.themed_search_style(),
        );
    }

    /// Settings/theme overlay paint (relocated `apply_overlay`).
    pub(in crate::native) fn paint_overlay_cells(
        &mut self,
        snapshot: &mut Snapshot,
        _ctx: &OverlayCtx,
    ) {
        apply_overlay(snapshot, &mut self.overlay);
    }

    /// Hovered links are signalled by the pointer shape (hand) only — OdyTTY does
    /// not draw a hover underline. A render-time underline keyed on the hovered
    /// `LinkId` smeared across unrelated cells: `hovered_hyperlink` is not
    /// recomputed while live output streams under a stationary pointer, and
    /// `HyperlinkTable::clear()` resets the id counter, so a stale/reused id
    /// matched freshly drawn content every frame. The cell paint is intentionally
    /// a no-op; the slot is kept in the manifest for the renderer.
    pub(in crate::native) fn paint_hyperlink_cells(
        &self,
        _snapshot: &mut Snapshot,
        _ctx: &OverlayCtx,
    ) {
    }

    // --- quads (scroll-indicator first, then status gutter, then the new
    //     no-op slots) --------------------------------------------------------

    /// Scroll-position indicator thumb (relocated
    /// `scroll_indicator_quad_with_padding`). Hidden at the live tail
    /// (`viewport_offset == 0`) exactly as before.
    pub(in crate::native) fn paint_scroll_indicator_quads(
        &self,
        ctx: &OverlayCtx,
        out: &mut Vec<SolidQuad>,
    ) {
        let color = self.scroll_indicator_color();
        if let Some(quad) = scroll_indicator_quad_with_padding(
            ctx.viewport_offset,
            ctx.scrollback_len,
            ctx.grid,
            ctx.cell,
            color,
            ctx.window_padding,
        ) {
            out.push(quad);
        }
    }

    /// SH2 per-command success/fail gutter (relocated inline
    /// `command_status_gutter_overlays` call). The method is gated on its
    /// setting and returns no quads while off, so this slot is empty on the
    /// default path.
    pub(in crate::native) fn paint_gutter_quads(&self, ctx: &OverlayCtx, out: &mut Vec<SolidQuad>) {
        out.extend(self.command_status_gutter_overlays(
            ctx.scrollback_len,
            ctx.cell,
            ctx.window_padding,
        ));
    }

    // VE4 cursor-trail quads: `paint_cursor_trail_quads` lives in `cursor_trail.rs`
    // alongside `cursor_trail_overlay_signature`, so the whole trail feature is
    // one submodule.

    /// ID1 v1 soft cursor glow — three concentric semi-transparent halo quads
    /// in the theme foreground color, emitted as cursor-layer overlays so the
    /// GPU draws them BEHIND the cursor block (D-GLOW-3 reorder). Off by default
    /// (`cursor_glow`); while off this pushes nothing, so the default render
    /// path is byte-identical. The rings extend at most a half-cell beyond the
    /// cursor cell at low alpha (0.05/0.09/0.13), kept under the RV1 floor's
    /// safety threshold for adjacent-cell text (D-GLOW-6).
    pub(in crate::native) fn paint_cursor_glow_quads(
        &self,
        ctx: &OverlayCtx,
        out: &mut Vec<SolidQuad>,
    ) {
        if !self.settings.cursor_glow || !ctx.cursor_visible {
            return;
        }
        let cols = ctx.grid.columns;
        let rows = ctx.grid.rows;
        if cols == 0 || rows == 0 {
            return;
        }
        // Defensive clamp mirroring `push_cursor`: a stale snapshot could carry
        // a cursor past the grid.
        let col = ctx.cursor.column.min(cols - 1) as f32;
        let row = ctx.cursor.row.min(rows - 1) as f32;
        let pad = ctx.window_padding.as_f32();
        let cell_w = ctx.cell.width as f32;
        let cell_h = ctx.cell.height as f32;
        let x0 = pad + col * cell_w;
        let y0 = pad + row * cell_h;
        let x1 = x0 + cell_w;
        let y1 = y0 + cell_h;
        // Glow color = theme foreground in linear RGB (matches the scroll
        // indicator's color basis); per-ring alpha set below. D-GLOW-5.
        let (r, g, b) = self.effective_theme.foreground;
        let base = text::foreground_linear(Color::Rgb(r, g, b));
        // Outer ring first (largest, faintest) so the more opaque inner rings
        // composite on top of it; all three precede the cursor block.
        for (extend, alpha) in CURSOR_GLOW_RINGS {
            let mut color = base;
            color[3] = alpha;
            out.push(SolidQuad {
                rect: [x0 - extend, y0 - extend, x1 + extend, y1 + extend],
                color,
            });
        }
    }

    /// ID3/U5 background-treatment quads — no-op slot (foundation). Filled by
    /// the background-treatment feature packet.
    pub(in crate::native) fn paint_background_quads(
        &self,
        _ctx: &OverlayCtx,
        _out: &mut Vec<SolidQuad>,
    ) {
    }

    // VE4 cursor-trail cache fragment: `cursor_trail_overlay_signature` lives in
    // `cursor_trail.rs` alongside the trail's quad emitter.

    /// ID1 cursor-glow cache fragment. `Inert` while off, so the composite key
    /// is a frame-to-frame constant and the default render path never
    /// reclassifies. When on it returns a constant `CursorGlow { phase: 0 }`:
    /// the off→on toggle flips `Inert` ↔ `CursorGlow`, forcing a rebuild so the
    /// glow appears/disappears without a stale cache, while cursor *moves* and
    /// blink toggles already reclassify through the terminal-revision and the
    /// `CursorRenderSignature.visible` fields respectively (D-GLOW-7 — the glow
    /// quads are repainted from the live `ctx.cursor` every frame, so no extra
    /// per-position signature field is needed).
    pub(in crate::native) fn cursor_glow_overlay_signature(&self) -> OverlayFragment {
        if !self.settings.cursor_glow {
            return OverlayFragment::Inert;
        }
        OverlayFragment::CursorGlow { phase: 0 }
    }

    // Note: `background_overlay_signature()` (ID3/U5) lives in `background_ui.rs`
    // alongside `background_treatment_params()`, so the whole treatment feature
    // is one submodule.

    // --- modal-input gate ----------------------------------------------------

    /// The active keyboard modal. Reads per-feature predicates that live in the
    /// feature submodules; all return `false` today, so this is always `None`
    /// and the gate is dead code on the live path. Precedence is fixed
    /// (D-INFRA-4): copy-mode before hints (mutually exclusive in practice).
    pub(in crate::native) fn active_modal(&self) -> ActiveModal {
        if self.copy_mode_active() {
            return ActiveModal::CopyMode;
        }
        if self.hints_selecting() {
            return ActiveModal::HintsSelect;
        }
        if self.rename_state.is_some() {
            return ActiveModal::RenameTab;
        }
        ActiveModal::None
    }

    /// Route a key to the active modal's handler — each in its own submodule.
    /// The `None` arm is unreachable from the key ladder (the caller only routes
    /// when `active_modal() != None`) but is handled for totality.
    pub(in crate::native) fn route_modal_key(&mut self, modal: ActiveModal, key: &WinitKey) {
        match modal {
            ActiveModal::CopyMode => self.copy_mode_key(key),
            ActiveModal::HintsSelect => self.hints_key(key),
            ActiveModal::RenameTab => self.rename_key(key),
            ActiveModal::None => {}
        }
    }

    /// Whether the active modal owns the mouse.
    pub(in crate::native) fn modal_captures_pointer(&self) -> bool {
        matches!(
            self.active_modal(),
            ActiveModal::CopyMode | ActiveModal::RenameTab
        )
    }

    pub(in crate::native) fn rename_overlay_signature(&self) -> OverlayFragment {
        match &self.rename_state {
            Some(state) => OverlayFragment::Rename {
                text: state.text.clone(),
                cursor: state.cursor,
            },
            None => OverlayFragment::Inert,
        }
    }

    pub(super) fn enter_rename_tab(&mut self, target: SessionToken) {
        let Some(title) = self
            .sessions
            .iter()
            .find(|session| session.id == target)
            .map(|session| session.effective_tab_title().to_owned())
        else {
            return;
        };
        let cursor = title.chars().count();
        self.rename_state = Some(RenameState {
            target,
            text: title,
            cursor,
        });
        self.request_selection_redraw();
    }

    pub(super) fn rename_key(&mut self, key: &WinitKey) {
        let Some(state) = self.rename_state.as_mut() else {
            return;
        };
        match key {
            WinitKey::Named(NamedKey::Escape) => {
                self.rename_state = None;
            }
            WinitKey::Named(NamedKey::Enter) => {
                let target = state.target;
                let text = state.text.trim().to_owned();
                let override_name = (!text.is_empty()).then_some(text);
                if let Some(session) = self.sessions.get_mut(target) {
                    session.set_title_override(override_name);
                }
                self.rename_state = None;
            }
            WinitKey::Named(NamedKey::Backspace) => {
                if state.cursor > 0 {
                    let remove_at = rename_byte_index(&state.text, state.cursor - 1);
                    state.text.remove(remove_at);
                    state.cursor -= 1;
                }
            }
            WinitKey::Named(NamedKey::ArrowLeft) => {
                state.cursor = state.cursor.saturating_sub(1);
            }
            WinitKey::Named(NamedKey::ArrowRight) => {
                state.cursor = (state.cursor + 1).min(state.text.chars().count());
            }
            WinitKey::Named(NamedKey::Home) => {
                state.cursor = 0;
            }
            WinitKey::Named(NamedKey::End) => {
                state.cursor = state.text.chars().count();
            }
            WinitKey::Character(text) if !self.modifiers.ctrl && !self.modifiers.alt => {
                for ch in text.chars().filter(|ch| !ch.is_control()) {
                    let insert_at = rename_byte_index(&state.text, state.cursor);
                    state.text.insert(insert_at, ch);
                    state.cursor += 1;
                }
            }
            _ => {}
        }
        self.request_selection_redraw();
    }

    pub(in crate::native) fn paint_rename_tab_cells(&self, snapshot: &mut Snapshot) {
        let Some(state) = self.rename_state.as_ref() else {
            return;
        };
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        if columns < 8 || rows < 3 {
            return;
        }
        let width = columns.clamp(8, 48);
        let height = 3;
        let left = (columns - width) / 2;
        let top = (rows - height) / 2;
        let panel = rename_panel_attrs();
        let border = rename_border_attrs();
        rename_fill_rect(snapshot, left, top, width, height, panel);
        rename_draw_border(snapshot, left, top, width, height, border);

        let body_left = left + 2;
        let body_width = width.saturating_sub(4);
        if body_width == 0 {
            return;
        }
        let prompt = "Tab name: ";
        let prompt_width = prompt.chars().count().min(body_width);
        rename_write_text(snapshot, top + 1, body_left, prompt_width, prompt, panel);
        let input_left = body_left + prompt_width;
        let input_width = body_width.saturating_sub(prompt_width);
        if input_width > 0 {
            rename_write_input(snapshot, top + 1, input_left, input_width, state, panel);
        }
    }

    // --- cursor render-params aggregator (Wave-15b foundation) ---------------

    /// Fold the per-feature cursor render parameters into one [`CursorRenderParams`].
    /// Each field is filled by exactly one Phase-4 feature's contributor stub —
    /// `cursor_motion_offset()` (VE4-slide, `cursor_frame.rs`) and
    /// `cursor_blink_alpha()` (ID1-easing, `cursor.rs`). Both stubs return the
    /// identity today (`[0.0, 0.0]` / `1.0`), so this returns
    /// `CursorRenderParams::default()` and the cursor renders byte-identically.
    ///
    /// This aggregator dissolves the `push_cursor` collision: ID1 and VE4 each
    /// own one field, so neither edits the other's file in Wave 16.
    pub(in crate::native) fn cursor_render_params(&self) -> CursorRenderParams {
        CursorRenderParams {
            offset: self.cursor_motion_offset(),
            alpha: self.cursor_blink_alpha(),
        }
    }

    /// Fold every cursor-animation wake source into the soonest deadline, or
    /// `None` when nothing is animating. Mirrors the render-params aggregator:
    /// `cursor_blink_fade_deadline()` (ID1-easing) and `cursor_motion_deadline()`
    /// (VE4-slide) each return `None` today, so the min is `None` and the
    /// control-flow collector schedules exactly as before — zero extra wakes on
    /// an at-rest terminal.
    ///
    /// This aggregator dissolves the `update_control_flow_deadline` collision:
    /// the collector folds in one stable `self.animation_deadline()` entry, and
    /// each feature adds its wake source behind its own stub in its own file.
    pub(in crate::native) fn animation_deadline(&self) -> Option<Instant> {
        [
            self.cursor_blink_fade_deadline(),
            self.cursor_motion_deadline(),
            self.new_row_fade_deadline(),
            // RV4 smooth scroll — 4th contributor; `None` on the off path.
            self.scroll_anim_deadline(),
            // BELL visual flash — `None` on the off / urgent-only path.
            self.bell_flash_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

fn rename_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

fn rename_panel_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Default;
    attrs.background = Color::Default;
    attrs.set_inverse(true);
    attrs
}

fn rename_border_attrs() -> Attrs {
    let mut attrs = rename_panel_attrs();
    attrs.foreground = Color::Indexed(14);
    attrs
}

fn rename_cursor_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(0);
    attrs.background = Color::Indexed(11);
    attrs
}

fn rename_fill_rect(
    snapshot: &mut Snapshot,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    attrs: Attrs,
) {
    for row in top..(top + height).min(snapshot.dimensions.rows) {
        let offset = row * snapshot.dimensions.columns;
        for column in left..(left + width).min(snapshot.dimensions.columns) {
            snapshot.cells[offset + column] = Cell::new(' ', attrs);
        }
    }
}

fn rename_draw_border(
    snapshot: &mut Snapshot,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    attrs: Attrs,
) {
    if width < 2 || height < 2 {
        return;
    }
    let right = left + width - 1;
    let bottom = top + height - 1;
    rename_write_cell(snapshot, top, left, '+', attrs);
    rename_write_cell(snapshot, top, right, '+', attrs);
    rename_write_cell(snapshot, bottom, left, '+', attrs);
    rename_write_cell(snapshot, bottom, right, '+', attrs);
    for column in left + 1..right {
        rename_write_cell(snapshot, top, column, '-', attrs);
        rename_write_cell(snapshot, bottom, column, '-', attrs);
    }
    for row in top + 1..bottom {
        rename_write_cell(snapshot, row, left, '|', attrs);
        rename_write_cell(snapshot, row, right, '|', attrs);
    }
}

fn rename_write_text(
    snapshot: &mut Snapshot,
    row: usize,
    column: usize,
    max_width: usize,
    text: &str,
    attrs: Attrs,
) {
    if row >= snapshot.dimensions.rows || column >= snapshot.dimensions.columns || max_width == 0 {
        return;
    }
    let mut x = column;
    let right = (column + max_width).min(snapshot.dimensions.columns);
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if width > 2 || x + width > right {
            break;
        }
        rename_write_cell(snapshot, row, x, ch, attrs);
        if width == 2 {
            rename_write_cell(snapshot, row, x + 1, ' ', attrs);
        }
        x += width;
    }
}

fn rename_write_input(
    snapshot: &mut Snapshot,
    row: usize,
    column: usize,
    width: usize,
    state: &RenameState,
    attrs: Attrs,
) {
    let cursor = state.cursor.min(state.text.chars().count());
    let chars: Vec<char> = state.text.chars().collect();
    let start = if chars.len() > width {
        cursor.saturating_sub(width.saturating_sub(1))
    } else {
        0
    };
    let visible_cursor = cursor.saturating_sub(start).min(width.saturating_sub(1));
    for col in 0..width {
        let cell_attrs = if col == visible_cursor {
            rename_cursor_attrs()
        } else {
            attrs
        };
        let ch = chars.get(start + col).copied().unwrap_or(' ');
        rename_write_cell(snapshot, row, column + col, ch, cell_attrs);
    }
}

fn rename_write_cell(snapshot: &mut Snapshot, row: usize, column: usize, ch: char, attrs: Attrs) {
    if row >= snapshot.dimensions.rows || column >= snapshot.dimensions.columns {
        return;
    }
    snapshot.cells[row * snapshot.dimensions.columns + column] = Cell::new(ch, attrs);
}
