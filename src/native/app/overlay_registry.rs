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

    /// Paint the **focused pane's** selection + search highlights onto its own
    /// snapshot, in the multi-pane render path (1c-3c). Unlike [`overlay_ctx`],
    /// which keys to the whole-window `self.grid`, this builds a pane-scoped
    /// [`OverlayCtx`] from the focused pane's own `grid` dimensions, scrollback
    /// length, and viewport offset — so `apply_selection_highlight` /
    /// `apply_search_ui` map highlights onto the correct cells inside the
    /// smaller pane grid. `self.selection` / `self.search` already `Deref` to
    /// the focused pane, so only these geometry inputs differ.
    ///
    /// Multi-pane v1 cut: only selection + search are painted here (the two
    /// per-pane overlays the render-box requires); the cursor-layer overlays
    /// (glow/trail), hints, copy-mode, and non-focused-pane overlays are not.
    /// The fields those paints would read (`cursor`, `now`, `clear_color`) are
    /// set to inert defaults because selection/search never read them.
    ///
    /// [`overlay_ctx`]: Self::overlay_ctx
    pub(in crate::native) fn paint_focused_pane_overlays(
        &self,
        snapshot: &mut Snapshot,
        pane_grid: Dimensions,
        viewport_offset: usize,
        scrollback_len: usize,
        cell: CellSize,
    ) {
        let ctx = OverlayCtx {
            viewport_offset,
            scrollback_len,
            grid: pane_grid,
            cell,
            window_padding: self
                .gpu
                .as_ref()
                .map(GpuState::window_padding)
                .unwrap_or_default(),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            now: Instant::now(),
            clear_color: [0.0; 4],
            scale: self.gpu.as_ref().map(GpuState::scale).unwrap_or(1.0),
        };
        self.paint_selection_cells(snapshot, &ctx);
        self.paint_search_cells(snapshot, &ctx);
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
        if self.sessions.position_of_token(target).is_none() {
            return;
        }
        let title = self.sessions.effective_tab_title(target);
        self.begin_rename(RenameTarget::Tab(target), title);
    }

    /// Open the rename overlay on the workspace at rail index `idx`, seeded with
    /// its current label. The "Rename Workspace" action / palette entry target
    /// the active workspace; the rail's in-place rename (W2) reuses this. A
    /// stale index is a no-op.
    pub(super) fn enter_rename_workspace(&mut self, idx: usize) {
        let Some(name) = self.sessions.workspace_name(idx) else {
            return;
        };
        let seed = name.to_owned();
        self.begin_rename(RenameTarget::Workspace(idx), seed);
    }

    /// Open the "Save as Layout" name prompt for the workspace at rail index
    /// `idx` (LAYOUT-SURFACE), seeded with that workspace's current name as a
    /// sensible default. Reuses the rename modal; Enter commits via
    /// [`Self::commit_rename`] to the WP3 `save_workspace_as_layout` path. A
    /// stale index is a no-op.
    pub(super) fn enter_save_layout_prompt(&mut self, idx: usize) {
        let Some(name) = self.sessions.workspace_name(idx) else {
            return;
        };
        let seed = name.to_owned();
        self.begin_rename(RenameTarget::SaveLayout(idx), seed);
    }

    /// Open the "Save as Layout" name prompt for the WHOLE application
    /// (SAVE-ALL-LAYOUT), seeded with the active workspace's name as a default.
    /// Reuses the rename modal; Enter commits via [`Self::commit_rename`] to the
    /// [`Self::save_all_workspaces_as_layout`] path, capturing every workspace.
    pub(super) fn enter_save_all_layout_prompt(&mut self) {
        let idx = self.sessions.active_workspace_index();
        let seed = self
            .sessions
            .workspace_name(idx)
            .map(str::to_owned)
            .unwrap_or_default();
        self.begin_rename(RenameTarget::SaveAllLayout, seed);
    }

    /// Reopen the "Layout name:" prompt for a different name after an overwrite
    /// collision (OVERWRITE-WARN "Rename" arm), seeded with the colliding `seed`
    /// so the user can edit it. `kind` restores the same save target — the whole
    /// app or the workspace at a rail index — that hit the collision.
    pub(super) fn reopen_layout_name_prompt(&mut self, kind: LayoutSaveKind, seed: String) {
        let target = match kind {
            LayoutSaveKind::WholeApp => RenameTarget::SaveAllLayout,
            LayoutSaveKind::Workspace(idx) => RenameTarget::SaveLayout(idx),
        };
        self.begin_rename(target, seed);
    }

    /// Shared setup for the rename overlay: seed the field with `text`, park the
    /// caret at the end, and clear any stale pointer streak from a previous
    /// field so a leftover drag/double-click can't leak in (F4-RENAME-MOUSE).
    fn begin_rename(&mut self, target: RenameTarget, text: String) {
        let cursor = text.chars().count();
        self.rename_state = Some(RenameState {
            target,
            text,
            cursor,
            anchor: None,
        });
        self.rename_dragging = false;
        self.rename_clicks = ClickTracker::default();
        self.request_selection_redraw();
    }

    pub(super) fn rename_key(&mut self, key: &WinitKey) {
        // Enter commits and may need full `&mut self` (a layout save reaches
        // beyond the disjoint `sessions` field the tab/workspace renames touch),
        // so it takes ownership of the prompt state up front rather than
        // borrowing it across the commit.
        if matches!(key, WinitKey::Named(NamedKey::Enter)) {
            self.commit_rename();
            return;
        }
        let Some(state) = self.rename_state.as_mut() else {
            return;
        };
        match key {
            WinitKey::Named(NamedKey::Escape) => {
                self.rename_state = None;
                self.rename_dragging = false;
            }
            WinitKey::Named(NamedKey::Backspace) => {
                // F4-RENAME-MOUSE: a live selection is replaced (deleted) by
                // Backspace; only with no selection does it delete the char
                // before the caret.
                if !rename_delete_selection(state) && state.cursor > 0 {
                    let remove_at = rename_byte_index(&state.text, state.cursor - 1);
                    state.text.remove(remove_at);
                    state.cursor -= 1;
                }
            }
            WinitKey::Named(NamedKey::Delete) => {
                // F4-RENAME-MOUSE: Delete replaces a live selection, else it
                // deletes the char at (forward of) the caret.
                if !rename_delete_selection(state) {
                    let count = state.text.chars().count();
                    if state.cursor < count {
                        let remove_at = rename_byte_index(&state.text, state.cursor);
                        state.text.remove(remove_at);
                    }
                }
            }
            WinitKey::Named(NamedKey::ArrowLeft) => {
                // Collapse a selection to its left edge; otherwise step left.
                state.cursor = match rename_selection_range(state) {
                    Some((lo, _)) => lo,
                    None => state.cursor.saturating_sub(1),
                };
                state.anchor = None;
            }
            WinitKey::Named(NamedKey::ArrowRight) => {
                // Collapse a selection to its right edge; otherwise step right.
                state.cursor = match rename_selection_range(state) {
                    Some((_, hi)) => hi,
                    None => (state.cursor + 1).min(state.text.chars().count()),
                };
                state.anchor = None;
            }
            WinitKey::Named(NamedKey::Home) => {
                state.cursor = 0;
                state.anchor = None;
            }
            WinitKey::Named(NamedKey::End) => {
                state.cursor = state.text.chars().count();
                state.anchor = None;
            }
            WinitKey::Character(text) if !self.modifiers.ctrl && !self.modifiers.alt => {
                // Typing over a selection replaces it first.
                rename_delete_selection(state);
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

    /// Commit the in-progress rename / layout-save prompt (Enter). Takes the
    /// prompt state by value so the commit has full `&mut self` — a layout save
    /// reaches beyond the disjoint `sessions` field the tab/workspace renames
    /// touch. A no-op when no prompt is open.
    fn commit_rename(&mut self) {
        let Some(state) = self.rename_state.take() else {
            return;
        };
        let text = state.text.trim().to_owned();
        match state.target {
            RenameTarget::Tab(token) => {
                let override_name = (!text.is_empty()).then_some(text);
                self.sessions.set_title_override(token, override_name);
            }
            RenameTarget::Workspace(idx) => {
                // A workspace always keeps a name: an empty field leaves the
                // existing label unchanged (unlike a tab, which clears back to
                // its live title).
                if !text.is_empty() {
                    self.sessions.rename_workspace(idx, text);
                }
            }
            RenameTarget::SaveLayout(idx) => {
                // LAYOUT-SURFACE: an empty name cancels the save (never writes an
                // unnamed layout file); otherwise capture the workspace at `idx`
                // under the typed name via the shared WP3 save path.
                if !text.is_empty() {
                    self.save_workspace_as_layout(idx, Some(&text));
                }
            }
            RenameTarget::SaveAllLayout => {
                // SAVE-ALL-LAYOUT: an empty name cancels; otherwise capture every
                // workspace as one named layout under the typed name.
                if !text.is_empty() {
                    self.save_all_workspaces_as_layout(Some(&text));
                }
            }
        }
        self.rename_dragging = false;
        self.request_selection_redraw();
    }

    /// F4-RENAME-MOUSE: handle a left mouse button on the tab-rename field.
    ///
    /// The rename modal is painted into the single-pane content snapshot
    /// (`paint_rename_tab_cells`), so the render basis is exactly
    /// `self.grid` + `self.pointer_cell`. A press that lands on the input line
    /// places the caret at the clicked character (a second click within the
    /// double-click window selects the word under it); the drag that a plain
    /// press arms is extended by `rename_drag_extend` on pointer motion. A
    /// release ends the drag and collapses an empty selection.
    ///
    /// Non-left buttons and presses off the input line are ignored (the caret
    /// stays put); the modal already owns the pointer, so nothing leaks to the
    /// grid beneath.
    pub(super) fn handle_rename_pointer_button(
        &mut self,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        if button != WinitMouseButton::Left {
            return;
        }
        if state == ElementState::Released {
            self.rename_dragging = false;
            if let Some(rename) = self.rename_state.as_mut()
                && rename.anchor == Some(rename.cursor)
            {
                // An armed drag that never moved leaves an empty selection.
                rename.anchor = None;
            }
            self.request_selection_redraw();
            return;
        }
        let Some(point) = self.pointer_cell else {
            return;
        };
        let (columns, rows) = (self.grid.columns, self.grid.rows);
        let Some(rename) = self.rename_state.as_ref() else {
            return;
        };
        let char_count = rename.text.chars().count();
        let prompt = rename_prompt(rename.target);
        let Some(idx) = rename_input_hit(
            columns,
            rows,
            prompt,
            char_count,
            rename.cursor,
            point.row,
            point.column,
        ) else {
            return;
        };
        let clicks = self.rename_clicks.register_click(point, Instant::now());
        let Some(rename) = self.rename_state.as_mut() else {
            return;
        };
        if clicks >= 2 {
            // Double- (or triple-) click selects the whole word under the caret.
            let (lo, hi) = rename_word_bounds(&rename.text, idx);
            rename.anchor = Some(lo);
            rename.cursor = hi;
            self.rename_dragging = false;
        } else {
            // A single click places the caret and arms a drag from there.
            rename.cursor = idx;
            rename.anchor = Some(idx);
            self.rename_dragging = true;
        }
        self.request_selection_redraw();
    }

    /// F4-RENAME-MOUSE: extend the field selection to the current pointer cell
    /// while a rename drag is live. The drag anchor (set on press) is kept; the
    /// caret follows the pointer, clamped onto the input line so dragging off
    /// the row still tracks horizontally.
    pub(super) fn rename_drag_extend(&mut self) {
        let Some(point) = self.pointer_cell else {
            return;
        };
        let (columns, rows) = (self.grid.columns, self.grid.rows);
        let Some(rename) = self.rename_state.as_ref() else {
            return;
        };
        let char_count = rename.text.chars().count();
        let prompt = rename_prompt(rename.target);
        // Clamp the drag onto the input row so vertical straying still tracks
        // the horizontal position (a text drag conventionally follows X).
        let Some(row) = rename_input_row(columns, rows, prompt) else {
            return;
        };
        if let Some(idx) = rename_input_hit(
            columns,
            rows,
            prompt,
            char_count,
            rename.cursor,
            row,
            point.column,
        ) && let Some(rename) = self.rename_state.as_mut()
        {
            rename.cursor = idx;
            self.request_selection_redraw();
        }
    }

    pub(in crate::native) fn paint_rename_tab_cells(&self, snapshot: &mut Snapshot) {
        let Some(state) = self.rename_state.as_ref() else {
            return;
        };
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        let Some((left, top, width, height)) = rename_band_box(columns, rows) else {
            return;
        };
        let panel = rename_panel_attrs();
        let border = rename_border_attrs();
        rename_fill_rect(snapshot, left, top, width, height, panel);
        rename_draw_border(snapshot, left, top, width, height, border);

        let body_left = left + 2;
        let body_width = width.saturating_sub(4);
        if body_width == 0 {
            return;
        }
        let prompt = rename_prompt(state.target);
        let prompt_width = prompt.chars().count().min(body_width);
        rename_write_text(snapshot, top + 1, body_left, prompt_width, prompt, panel);
        // Derive the input trio from the shared layout so render and the mouse
        // hit-test can never drift (F4-RENAME-MOUSE). `rename_layout` replicates
        // the box math above exactly; a unit test pins that agreement.
        if let Some(layout) = rename_layout(columns, rows, prompt) {
            rename_write_input(
                snapshot,
                layout.input_row,
                layout.input_left,
                layout.input_width,
                state,
                panel,
            );
        }
    }

    /// TRANSPARENCY (PROMPT-OPACITY): the rename/prompt band's outer cell box in
    /// content-grid coordinates (`left, top, width, height`), or `None` when no
    /// rename is active or the grid is too small for the modal. Shares
    /// [`rename_band_box`] with the painter so the cells the opaque-region span
    /// holds opaque match exactly the cells `paint_rename_tab_cells` fills. The
    /// band paints on its own path (not `overlay_rect`), so without marking this
    /// span opaque it renders translucent under a translucent window.
    pub(super) fn rename_band_content_rect(&self) -> Option<(usize, usize, usize, usize)> {
        self.rename_state.as_ref()?;
        rename_band_box(self.grid.columns, self.grid.rows)
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

    /// Fold every overlay/cursor animation wake source into the soonest
    /// deadline, or `None` when nothing is animating — cursor blink-fade + slide,
    /// smooth-scroll glide, bell flash, new-row fade, and the open-notice /
    /// click-hint auto-expiry. Each contributor returns `None` at rest, so the
    /// min is `None` and an at-rest terminal schedules zero extra wakes.
    ///
    /// Consumed on BOTH sides of the event loop: `next_wake_deadline` sources it
    /// (single-pane gated — that render path is the only consumer that advances
    /// these timers; NF21-1/7 owns the multipane path) so a wake is scheduled,
    /// and the about-to-wait maintenance pass treats "woken while this is `Some`"
    /// as "request a frame". The two must move together: three of the frame-paced
    /// getters return `Instant::now() + FRAME` while in flight, so a `now >=
    /// deadline` consumer would never fire mid-animation — the maintenance
    /// predicate is `is_some()`, not the equality, precisely so the frame-paced
    /// contributors drive a repaint every frame instead of a silent spin.
    ///
    /// (History: this entry was in the collector until the multi-session refactor
    /// replaced it with a cursor-only fan-out; the five non-cursor contributors
    /// were stranded with a consumer but no wake source until NF21-2 restored it.)
    pub(in crate::native) fn animation_deadline(&self) -> Option<Instant> {
        [
            self.cursor_blink_fade_deadline(),
            self.cursor_motion_deadline(),
            self.new_row_fade_deadline(),
            // BELL visual flash — `None` on the off / urgent-only path.
            self.bell_flash_deadline(),
            // OPEN-NOTICE (P0-2) auto-expiry — `None` when no notice is in flight.
            self.open_notice_deadline(),
            // UX-A (Phase 11) click-hint auto-expiry — `None` when no hint shown.
            self.click_hint_deadline(),
            // SCROLL-GLIDE follower — `None` at rest / on the off path.
            self.scroll_glide_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

/// The prompt label printed before the editable input in the rename modal,
/// chosen by what the rename targets so a workspace rename does not mislabel
/// itself as a tab rename. Both the painter and [`rename_layout`] resolve the
/// prompt from the same target so the input's start column is computed
/// identically on the render and hit-test paths (a longer prompt shifts the
/// input right by exactly its width).
fn rename_prompt(target: RenameTarget) -> &'static str {
    match target {
        RenameTarget::Tab(_) => "Tab name: ",
        RenameTarget::Workspace(_) => "Workspace name: ",
        RenameTarget::SaveLayout(_) | RenameTarget::SaveAllLayout => "Layout name: ",
    }
}

/// The rename/prompt modal's outer cell box (`left, top, width, height`) in
/// content-grid coordinates, or `None` when the grid is too small for the modal.
/// The single source of the box math the painter ([`App::paint_rename_tab_cells`])
/// and the input-geometry helper ([`rename_layout`]) both build on, so the
/// rendered band, the mouse hit-test, and the opaque-region span
/// (PROMPT-OPACITY) can never drift.
fn rename_band_box(columns: usize, rows: usize) -> Option<(usize, usize, usize, usize)> {
    if columns < 8 || rows < 3 {
        return None;
    }
    let width = columns.clamp(8, 48);
    let height = 3usize;
    let left = (columns - width) / 2;
    let top = (rows - height) / 2;
    Some((left, top, width, height))
}

/// Geometry of the tab-rename modal's editable input, derived purely from the
/// content-grid dimensions. Render (`paint_rename_tab_cells` /
/// `rename_write_input`) and the mouse hit-test both go through this so a click
/// maps to exactly the character the painter drew under the pointer.
struct RenameLayout {
    /// Leftmost box column (inclusive).
    box_left: usize,
    /// Rightmost box column (inclusive).
    box_right: usize,
    /// Grid row the editable input sits on.
    input_row: usize,
    /// First grid column of the editable input (just after the prompt).
    input_left: usize,
    /// Width, in cells, of the editable input (1 cell renders 1 character).
    input_width: usize,
}

/// Replicate the box math in [`App::paint_rename_tab_cells`] exactly, returning
/// the editable-input geometry — or `None` when the grid is too small for the
/// modal (the painter bails on the same conditions). `prompt` is the label the
/// painter prints before the input, so its width (which differs between tab and
/// workspace renames) shifts the input start identically here. A unit test pins
/// that this stays byte-aligned with the painter.
fn rename_layout(columns: usize, rows: usize, prompt: &str) -> Option<RenameLayout> {
    let (left, top, width, _height) = rename_band_box(columns, rows)?;
    let body_left = left + 2;
    let body_width = width.saturating_sub(4);
    if body_width == 0 {
        return None;
    }
    let prompt_width = prompt.chars().count().min(body_width);
    let input_left = body_left + prompt_width;
    let input_width = body_width.saturating_sub(prompt_width);
    if input_width == 0 {
        return None;
    }
    Some(RenameLayout {
        box_left: left,
        box_right: left + width - 1,
        input_row: top + 1,
        input_left,
        input_width,
    })
}

/// The grid row the rename input occupies, or `None` if the modal cannot fit.
fn rename_input_row(columns: usize, rows: usize, prompt: &str) -> Option<usize> {
    rename_layout(columns, rows, prompt).map(|layout| layout.input_row)
}

/// The first visible character index given the current caret and field width —
/// the horizontal scroll offset shared by the painter and the hit-test so both
/// agree on which characters are on screen.
fn rename_visible_start(char_count: usize, cursor: usize, width: usize) -> usize {
    if char_count > width {
        cursor.saturating_sub(width.saturating_sub(1))
    } else {
        0
    }
}

/// Map a clicked content-grid cell to a caret character index in the rename
/// input, or `None` when the click is not on the input line / outside the box.
/// `cursor` is the pre-click caret (it decides the visible scroll window).
fn rename_input_hit(
    columns: usize,
    rows: usize,
    prompt: &str,
    char_count: usize,
    cursor: usize,
    row: usize,
    col: usize,
) -> Option<usize> {
    let layout = rename_layout(columns, rows, prompt)?;
    if row != layout.input_row || col < layout.box_left || col > layout.box_right {
        return None;
    }
    let start = rename_visible_start(char_count, cursor.min(char_count), layout.input_width);
    let rel = col.saturating_sub(layout.input_left);
    Some((start + rel).min(char_count))
}

/// Whitespace-delimited word bounds `[lo, hi)` (character indices) around the
/// character at `idx`. A click on whitespace selects the whitespace run; an
/// out-of-range `idx` clamps to the last character. Empty text yields `(0, 0)`.
fn rename_word_bounds(text: &str, idx: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let i = idx.min(chars.len() - 1);
    let ws = chars[i].is_whitespace();
    let mut lo = i;
    while lo > 0 && chars[lo - 1].is_whitespace() == ws {
        lo -= 1;
    }
    let mut hi = i + 1;
    while hi < chars.len() && chars[hi].is_whitespace() == ws {
        hi += 1;
    }
    (lo, hi)
}

/// The active selection span `[lo, hi)` (character indices) in the rename field,
/// or `None` when nothing is selected (no anchor, or anchor collapsed onto the
/// caret).
fn rename_selection_range(state: &RenameState) -> Option<(usize, usize)> {
    let anchor = state.anchor?;
    let cursor = state.cursor;
    (anchor != cursor).then(|| (anchor.min(cursor), anchor.max(cursor)))
}

/// If the rename field has a live selection, delete it (setting the caret to the
/// span start) and return `true`; otherwise return `false`. Either way the
/// anchor is cleared, so a subsequent edit starts from a collapsed caret.
fn rename_delete_selection(state: &mut RenameState) -> bool {
    let removed = if let Some((lo, hi)) = rename_selection_range(state) {
        let lo_b = rename_byte_index(&state.text, lo);
        let hi_b = rename_byte_index(&state.text, hi);
        state.text.replace_range(lo_b..hi_b, "");
        state.cursor = lo;
        true
    } else {
        false
    };
    state.anchor = None;
    removed
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

/// F4-RENAME-MOUSE: styling for selected characters in the rename field. The
/// modal paints into the indexed-palette cell snapshot (like the cursor and
/// border above), where the theme's `selection` role isn't addressable, so this
/// uses the conventional blue-highlight palette pair — bright text on a blue
/// field — the closest faithful analog in this cell-modal path.
fn rename_selection_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(15);
    attrs.background = Color::Indexed(4);
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
    let char_count = state.text.chars().count();
    let cursor = state.cursor.min(char_count);
    let chars: Vec<char> = state.text.chars().collect();
    let start = rename_visible_start(char_count, cursor, width);
    let visible_cursor = cursor.saturating_sub(start).min(width.saturating_sub(1));
    // F4-RENAME-MOUSE: highlight the selected span [lo, hi) in selection colors.
    let selection = rename_selection_range(state);
    let sel_attrs = rename_selection_attrs();
    for col in 0..width {
        let char_index = start + col;
        let selected = selection.is_some_and(|(lo, hi)| char_index >= lo && char_index < hi);
        // The caret cell wins over selection styling so the focus edge stays
        // visible; then selected cells; then the plain panel fill.
        let cell_attrs = if col == visible_cursor {
            rename_cursor_attrs()
        } else if selected {
            sel_attrs
        } else {
            attrs
        };
        let ch = chars.get(char_index).copied().unwrap_or(' ');
        rename_write_cell(snapshot, row, column + col, ch, cell_attrs);
    }
}

fn rename_write_cell(snapshot: &mut Snapshot, row: usize, column: usize, ch: char, attrs: Attrs) {
    if row >= snapshot.dimensions.rows || column >= snapshot.dimensions.columns {
        return;
    }
    snapshot.cells[row * snapshot.dimensions.columns + column] = Cell::new(ch, attrs);
}

#[cfg(test)]
mod rename_mouse_tests {
    //! F4-RENAME-MOUSE: pure hit-test / caret-mapping / selection-edit seam
    //! tests. These need no `App` or PTY — they exercise the free functions the
    //! render and pointer paths both go through, so a click maps to exactly the
    //! character the painter drew.
    use super::*;

    /// The tab-rename prompt, resolved from a tab target for the layout tests
    /// below (they exercise the tab modal geometry).
    const TAB_PROMPT: &str = "Tab name: ";

    /// A `RenameState` with no live session — fine for the edit/selection
    /// helpers, which never touch the target token.
    fn state(text: &str, cursor: usize) -> RenameState {
        RenameState {
            target: RenameTarget::Tab(SessionToken(1)),
            text: text.to_owned(),
            cursor,
            anchor: None,
        }
    }

    #[test]
    fn prompt_label_matches_the_rename_target() {
        // RENAME-LABEL: a tab rename says "Tab name: "; a workspace rename says
        // "Workspace name: " — the shared modal must not mislabel a workspace as
        // a tab.
        assert_eq!(
            rename_prompt(RenameTarget::Tab(SessionToken(1))),
            "Tab name: "
        );
        assert_eq!(
            rename_prompt(RenameTarget::Workspace(0)),
            "Workspace name: "
        );
        // LAYOUT-SURFACE: the Save-as-Layout prompt reuses the modal with its own
        // label so a layout save is never mislabeled as a rename.
        assert_eq!(rename_prompt(RenameTarget::SaveLayout(0)), "Layout name: ");
        // SAVE-ALL-LAYOUT: the whole-app save prompt shares the "Layout name: "
        // label.
        assert_eq!(rename_prompt(RenameTarget::SaveAllLayout), "Layout name: ");
    }

    #[test]
    fn layout_input_start_follows_the_prompt_width() {
        // The longer workspace prompt shifts the editable input right by exactly
        // the extra prompt columns, and the input is that much narrower — so the
        // width math derives from the actual prompt, never a hard-coded literal.
        let columns = 80;
        let rows = 24;
        let tab = rename_layout(columns, rows, "Tab name: ").expect("tab layout fits");
        let ws = rename_layout(columns, rows, "Workspace name: ").expect("ws layout fits");
        let delta = "Workspace name: ".chars().count() - "Tab name: ".chars().count();
        assert_eq!(ws.input_left, tab.input_left + delta);
        assert_eq!(ws.input_width, tab.input_width - delta);
        // The box itself is unchanged — only the prompt/input split moves.
        assert_eq!(ws.box_left, tab.box_left);
        assert_eq!(ws.box_right, tab.box_right);
    }

    #[test]
    fn layout_matches_the_painter_box_math() {
        // The painter centers an 8..=48-wide, 3-tall box and reserves a 2-cell
        // border + the "Tab name: " prompt. Reproduce that here so a drift in
        // `rename_layout` (used by the hit-test) is caught.
        let columns = 80;
        let rows = 24;
        let width = columns.clamp(8, 48); // 48
        let left = (columns - width) / 2; // 16
        let top = (rows - 3) / 2; // 10
        let body_left = left + 2; // 18
        let prompt_width = TAB_PROMPT.chars().count(); // 10
        let layout = rename_layout(columns, rows, TAB_PROMPT).expect("layout fits");
        assert_eq!(layout.box_left, left);
        assert_eq!(layout.box_right, left + width - 1);
        assert_eq!(layout.input_row, top + 1);
        assert_eq!(layout.input_left, body_left + prompt_width);
        assert_eq!(layout.input_width, width - 4 - prompt_width);
    }

    #[test]
    fn band_box_is_the_single_source_for_the_painter_and_hit_test() {
        // PROMPT-OPACITY: `rename_band_box` is the one place the centered
        // 8..=48-wide, 3-tall box math lives; the painter, the input hit-test
        // (`rename_layout`), and the opaque-region span all build on it, so the
        // opaque cells can never drift from the painted band.
        let columns = 80;
        let rows = 24;
        let (left, top, width, height) =
            rename_band_box(columns, rows).expect("band fits an 80x24 grid");
        assert_eq!(width, columns.clamp(8, 48));
        assert_eq!(height, 3);
        assert_eq!(left, (columns - width) / 2);
        assert_eq!(top, (rows - height) / 2);
        // The input hit-test derives its box origin from the same helper.
        let layout = rename_layout(columns, rows, TAB_PROMPT).expect("layout fits");
        assert_eq!(layout.box_left, left);
        assert_eq!(layout.box_right, left + width - 1);
        // Too-small grids collapse identically: no band, no opaque span.
        assert!(rename_band_box(7, 24).is_none(), "too few columns");
        assert!(rename_band_box(80, 2).is_none(), "too few rows");
    }

    #[test]
    fn layout_is_none_when_the_grid_is_too_small() {
        assert!(
            rename_layout(7, 24, TAB_PROMPT).is_none(),
            "too few columns"
        );
        assert!(rename_layout(80, 2, TAB_PROMPT).is_none(), "too few rows");
    }

    #[test]
    fn hit_maps_a_click_to_the_character_under_it() {
        let columns = 80;
        let rows = 24;
        let layout = rename_layout(columns, rows, TAB_PROMPT).unwrap();
        // Short text (no scroll): char index == click column - input_left.
        let char_count = 5; // "hello"
        let cursor = 5;
        for col_off in 0..layout.input_width.min(char_count + 1) {
            let hit = rename_input_hit(
                columns,
                rows,
                TAB_PROMPT,
                char_count,
                cursor,
                layout.input_row,
                layout.input_left + col_off,
            );
            assert_eq!(hit, Some(col_off), "click {col_off} maps to char {col_off}");
        }
    }

    #[test]
    fn hit_clamps_past_end_and_before_start() {
        let columns = 80;
        let rows = 24;
        let layout = rename_layout(columns, rows, TAB_PROMPT).unwrap();
        let char_count = 3;
        // Far to the right of the 3-char text but still inside the box → end.
        let far = rename_input_hit(
            columns,
            rows,
            TAB_PROMPT,
            char_count,
            char_count,
            layout.input_row,
            layout.box_right,
        );
        assert_eq!(far, Some(char_count), "clamps to text length");
        // On the prompt (left of the input field) → start of the visible window.
        let onprompt = rename_input_hit(
            columns,
            rows,
            TAB_PROMPT,
            char_count,
            char_count,
            layout.input_row,
            layout.box_left + 1,
        );
        assert_eq!(onprompt, Some(0), "left of input clamps to visible start");
    }

    #[test]
    fn hit_is_none_off_the_input_row_or_box() {
        let columns = 80;
        let rows = 24;
        let layout = rename_layout(columns, rows, TAB_PROMPT).unwrap();
        assert!(
            rename_input_hit(
                columns,
                rows,
                TAB_PROMPT,
                3,
                3,
                layout.input_row + 1,
                layout.input_left
            )
            .is_none(),
            "different row misses"
        );
        assert!(
            rename_input_hit(
                columns,
                rows,
                TAB_PROMPT,
                3,
                3,
                layout.input_row,
                layout.box_right + 1
            )
            .is_none(),
            "past the right border misses"
        );
    }

    #[test]
    fn hit_accounts_for_horizontal_scroll() {
        // A field narrower than the text scrolls so the caret stays visible; the
        // hit-test must use the same `start` window or clicks land on the wrong
        // character. Force a narrow input via a small grid.
        let columns = 20;
        let rows = 5;
        let layout = rename_layout(columns, rows, TAB_PROMPT).expect("narrow layout fits");
        let width = layout.input_width;
        let char_count = width + 10; // longer than the field → scrolled
        let cursor = char_count; // caret at end → window shows the tail
        let start = rename_visible_start(char_count, cursor, width);
        assert!(start > 0, "text is scrolled");
        // Clicking the first visible cell selects the first visible character.
        let hit = rename_input_hit(
            columns,
            rows,
            TAB_PROMPT,
            char_count,
            cursor,
            layout.input_row,
            layout.input_left,
        );
        assert_eq!(hit, Some(start));
    }

    #[test]
    fn word_bounds_select_whitespace_delimited_words() {
        let text = "hello world foo";
        assert_eq!(rename_word_bounds(text, 0), (0, 5), "first word");
        assert_eq!(rename_word_bounds(text, 3), (0, 5), "mid first word");
        assert_eq!(rename_word_bounds(text, 6), (6, 11), "second word");
        // Index on the space between words selects the whitespace run.
        assert_eq!(rename_word_bounds(text, 5), (5, 6), "single space run");
        // Out-of-range clamps to the last character's word.
        assert_eq!(
            rename_word_bounds(text, 99),
            (12, 15),
            "clamped to last word"
        );
        assert_eq!(rename_word_bounds("", 0), (0, 0), "empty text");
    }

    #[test]
    fn selection_range_reports_ordered_span() {
        let mut s = state("hello", 4);
        assert_eq!(rename_selection_range(&s), None, "no anchor → no selection");
        s.anchor = Some(4);
        assert_eq!(rename_selection_range(&s), None, "collapsed anchor → none");
        s.anchor = Some(1);
        assert_eq!(rename_selection_range(&s), Some((1, 4)), "ordered lo..hi");
        s.cursor = 0;
        assert_eq!(
            rename_selection_range(&s),
            Some((0, 1)),
            "anchor behind caret"
        );
    }

    #[test]
    fn delete_selection_removes_the_span_and_clears_anchor() {
        let mut s = state("hello world", 5);
        s.anchor = Some(0); // select "hello"
        assert!(rename_delete_selection(&mut s), "removed a span");
        assert_eq!(s.text, " world");
        assert_eq!(s.cursor, 0, "caret to span start");
        assert_eq!(s.anchor, None, "anchor cleared");
        // No selection → returns false, still clears any stale anchor.
        let mut s2 = state("abc", 1);
        s2.anchor = Some(1); // collapsed
        assert!(!rename_delete_selection(&mut s2));
        assert_eq!(s2.text, "abc");
        assert_eq!(s2.anchor, None);
    }

    #[test]
    fn delete_selection_handles_multibyte_text() {
        // "café» " — non-ASCII so byte and char indices diverge; the delete must
        // map through char→byte or it would slice mid-codepoint / mispositioned.
        let mut s = state("café☺x", 4); // chars: c a f é ☺ x
        s.anchor = Some(3); // select "é☺" (chars 3..4? actually 3..cursor)
        s.cursor = 5; // select chars 3..5 = "é☺"
        assert!(rename_delete_selection(&mut s));
        assert_eq!(s.text, "cafx");
        assert_eq!(s.cursor, 3);
    }
}
