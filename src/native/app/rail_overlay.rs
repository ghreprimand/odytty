// SPDX-License-Identifier: GPL-3.0-only
//! Workspace-rail reveal, auto-hide, and overlay assembly for the native app.
//!
//! Owns reveal trigger and keep-alive geometry, the auto-hide state machine
//! driven by pointer samples, and the per-frame revealed-rail overlay build and
//! signature.

use super::*;

/// Owned inputs for the F4-P3 revealed rail overlay, built once per frame by
/// [`App::build_rail_overlay`]. Holds the strip snapshot by value (the GPU call
/// borrows it) plus the pre-resolved origin, label offset, and wash/seam quads;
/// the render path
/// lends a [`gpu::RailOverlay`] from it at the update call.
pub(super) struct RailOverlayData {
    pub(super) snapshot: Snapshot,
    pub(super) origin: [f32; 2],
    pub(super) rail_glyph_dy_rows: f32,
    pub(super) widget_quads: Vec<SolidQuad>,
    pub(super) base_gaps: Vec<SolidQuad>,
    pub(super) wash: Option<SolidQuad>,
    pub(super) seam: Option<SolidQuad>,
}

impl App {
    /// RAIL-AUTOHIDE-CTL: flip `tab_rail_autohide` from the rail's bottom-edge
    /// toggle control. Routes the change through the full reload seam (as an
    /// [`SettingsApplySource::ExternalChrome`] mutation) so the panel row, the
    /// rail visibility reconciliation, and the grid reflow all stay coherent,
    /// then writes it back to `odytty.conf` so it survives a restart. No new
    /// settings key: this is the existing `tab_rail_autohide` gate, reachable
    /// from the rail itself.
    pub(super) fn toggle_tab_rail_autohide(&mut self) {
        let mut next = self.settings.clone();
        next.tab_rail_autohide = !next.tab_rail_autohide;
        // ExternalChrome, not OverlayEdit: the toggle originates from the rail
        // affordance, not the panel, so the open settings panel must rebase its
        // Layout row onto the new value (a fresh clean baseline) rather than
        // keeping its own stale edit copy. Routing through the full reload seam
        // still runs the rail visibility reconciliation and grid reflow.
        self.apply_settings_through_reload_seam(next, SettingsApplySource::ExternalChrome);
        self.persist_tab_rail_autohide();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Write the live `tab_rail_autohide` value through to `odytty.conf` so the
    /// rail-toggle choice survives a restart. The in-memory setting is already
    /// applied; a missing config path or write error is logged, not fatal.
    pub(super) fn persist_tab_rail_autohide(&mut self) {
        let value = if self.settings.tab_rail_autohide {
            "on"
        } else {
            "off"
        }
        .to_owned();
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        let changes = [SettingEdit {
            key: "tab_rail_autohide",
            env: TAB_RAIL_AUTOHIDE_ENV,
            value,
        }];
        if let Err(error) = write_settings_changes_to_path(path, &changes) {
            tracing::warn!(error = %error, "could not persist tab rail autohide");
        }
    }

    // -----------------------------------------------------------------------
    // F4-P3 rail auto-hide (ODP-4)
    // -----------------------------------------------------------------------

    /// Whether rail auto-hide is active this frame: the knob is on, the tab
    /// chrome is shown, AND the placement is a side rail (the top bar keeps
    /// `always_show_tab_bar` semantics — a hidden top bar is already expressible
    /// by turning that off). When active, `tab_reserve` returns `NONE` and the
    /// rail is a floating overlay.
    pub(super) fn rail_autohide_active(&self) -> bool {
        // The workspace rail is always on a side, so auto-hide applies whenever
        // the rail is shown and the knob is on. The top tab bar is never
        // auto-hidden (it keeps `always_show_tab_bar` semantics).
        self.should_show_workspace_rail() && self.settings.tab_rail_autohide
    }

    /// The side an auto-hidden rail occupies (independent of `tab_reserve`, which
    /// is `NONE` under autohide). `None` when autohide is inactive.
    pub(super) fn rail_autohide_side(&self) -> Option<RailSide> {
        self.rail_autohide_active()
            .then(|| self.workspace_rail_side())
    }

    /// The side whose width seam is interactive this frame. A pinned rail uses
    /// its reserved side. An auto-hidden rail has no reservation, so its seam
    /// exists only while the floating overlay is revealed.
    pub(super) fn effective_rail_seam_side(&self) -> Option<RailSide> {
        if self.rail_autohide_active() {
            if self.rail_overlay_visible() {
                self.rail_autohide_side()
            } else {
                None
            }
        } else {
            self.rail_side()
        }
    }

    /// The width (cells) of the auto-hidden rail overlay band — the same width
    /// the rail would resolve to if pinned (`Manual` clamp or `Auto` from the
    /// longest title), computed independently of the (now zero) reservation.
    pub(super) fn rail_overlay_cols(&self) -> usize {
        self.settings.rail_width_cols(self.rail_auto_want_cols())
    }

    /// Physical-pixel top-left of the revealed rail overlay band. A left rail
    /// hugs the left padding (`[pad, pad]`); a right rail hugs the right window
    /// edge (`surface_w − pad − band_w`). Unlike the pinned right rail (which is
    /// grid-embedded after the full-width content), the overlay floats at the
    /// window edge — content underneath is already full-width. Surface + padding
    /// come from [`Self::resolved_surface`] so the drawn band, its seam, and the
    /// reveal-zone geometry all read the SAME basis (and are test-injectable).
    pub(super) fn rail_overlay_origin_px(&self, cell: CellSize, side: RailSide) -> [f32; 2] {
        let (surface_w, pad) = self.reveal_surface_metrics();
        let (surface_w, pad) = (surface_w as f32, pad as f32);
        let band_w = self.rail_overlay_cols() as f32 * cell.width as f32;
        let x = match side {
            RailSide::Left => pad,
            RailSide::Right => (surface_w - pad - band_w).max(pad),
        };
        [x, pad]
    }

    /// The revealed overlay band's content-facing seam x (physical px): the
    /// right edge of a left band, the left edge of a right band.
    pub(super) fn rail_overlay_seam_x(&self, cell: CellSize, side: RailSide) -> f32 {
        let origin_x = self.rail_overlay_origin_px(cell, side)[0];
        let band_w = self.rail_overlay_cols() as f32 * cell.width as f32;
        match side {
            RailSide::Left => origin_x + band_w,
            RailSide::Right => origin_x,
        }
    }

    /// `(surface_w, window_pad)` in **physical** px for the reveal-zone geometry,
    /// via [`Self::resolved_surface`] — the same basis the drawn rail band uses,
    /// and test-injectable through `set_test_surface_for_test` so the reveal
    /// wiring can be exercised at a real scale + padding headlessly. `(0, 0)`
    /// before the GPU / a test surface exists.
    pub(super) fn reveal_surface_metrics(&self) -> (f64, f64) {
        match self.resolved_surface() {
            Some((w, _h, padding)) => (w as f64, padding.as_f32() as f64),
            None => (0.0, 0.0),
        }
    }

    /// The reveal trigger-zone reach (physical px) inward from the rail's window
    /// edge: the window padding plus the scaled `tab_rail_reveal_px`. Both terms
    /// are physical: the padding is stored physical, and `tab_rail_reveal_px` is
    /// logical so it is scaled by [`Self::effective_scale`] — winit reports the
    /// pointer in physical px, so the whole comparison stays in one space.
    pub(super) fn reveal_reach_px(&self) -> f64 {
        let reveal_px = self.settings.tab_rail_reveal_px as f64 * self.effective_scale() as f64;
        let (_surface_w, pad) = self.reveal_surface_metrics();
        pad + reveal_px
    }

    /// Whether a raw pointer x is inside the reveal **trigger** zone — an
    /// **interior** band measured from the rail's window edge inward by the
    /// window padding PLUS `tab_rail_reveal_px` (see [`reveal_edge_contains`]).
    pub(super) fn pointer_in_reveal_edge(&self, px_x: f64, side: RailSide) -> bool {
        let (surface_w, _pad) = self.reveal_surface_metrics();
        reveal_edge_contains(side, px_x, self.reveal_reach_px(), surface_w)
    }

    /// Whether a raw pointer x is inside the reveal **keep-alive** region — the
    /// UNION of the trigger zone and the drawn overlay band, so the rail holds
    /// while the pointer is anywhere over either (see [`reveal_band_contains`]).
    pub(super) fn pointer_in_reveal_band(&self, px_x: f64, cell: CellSize, side: RailSide) -> bool {
        let seam_x = self.rail_overlay_seam_x(cell, side) as f64;
        let (surface_w, _pad) = self.reveal_surface_metrics();
        reveal_band_contains(side, px_x, seam_x, self.reveal_reach_px(), surface_w)
    }

    /// Reveal the auto-hidden rail for a flash after a keyboard tab action
    /// (ODP-4 SHOULD). Inert (and cheap) unless autohide is active; requests a
    /// redraw and schedules the flash-expiry wake when it takes effect.
    pub(super) fn flash_rail_autohide(&mut self) {
        if !self.rail_autohide_active() {
            return;
        }
        self.rail_autohide.flash(Instant::now());
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Feed the live pointer to the auto-hide machine and repaint on a
    /// visibility change. Called from the pointer-move path while autohide is
    /// active; also called with `in_edge = in_band = false` when the pointer
    /// leaves the window so a rail revealed at the edge can hide. `now` is the
    /// event time (`Instant::now()` in production; injected in tests so the
    /// reveal → hold → hide sequence is deterministic through the real contact
    /// geometry).
    pub(super) fn update_rail_autohide_pointer(&mut self, px_x: f64, cell: CellSize, now: Instant) {
        let Some(side) = self.rail_autohide_side() else {
            return;
        };
        // Popup-tracking rule (ODP-4): hold the rail up while an overlay (its
        // right-click context menu) is open — the hide timer is suspended until
        // the menu closes. RAIL-PIN: also suspend while a rail-anchored menu or a
        // workspace rename prompt is up so the grace never elapses under it.
        self.rail_autohide
            .set_suspend(self.overlay.is_open() || self.rail_pinned_open());
        // Motion-aware trigger: fold in the previous sample so the segment
        // prev→curr is tested against the edge zone (a fast approach jumps over a
        // static point zone). Record this sample as the next prev before feeding.
        let prev_px_x = self.last_rail_pointer_px;
        self.last_rail_pointer_px = Some(px_x);
        let (in_edge, in_band) = self.reveal_pointer_contact(px_x, prev_px_x, cell, side);
        let changed = self.rail_autohide.on_pointer(in_edge, in_band, now);
        if changed {
            // A visibility flip must rebuild the frame, not merely re-present it:
            // the rail overlay is only assembled inside the `should_rebuild_frame`
            // gate (`build_rail_overlay`), and that gate reads `needs_rebuild`.
            // Requesting a redraw without marking the frame dirty lets the
            // RedrawRequested skip the rebuild and re-present the previous
            // (rail-less) frame — the reveal then only paints when some unrelated
            // event happens to set `needs_rebuild`, which over a quiescent
            // terminal is "not until the pointer crosses off the window edge".
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// The `(in_edge, in_band)` reveal contact the machine is fed for a raw
    /// pointer x, given the previous sample `prev_px_x` (`None` on the first
    /// sample after entry). Yields to an active scrollbar-thumb drag (ODP-5): a
    /// drag near the edge — the right rail's scrollbar sits just inside the seam —
    /// must not trigger or hold a reveal, so a drag-in-progress reports no
    /// contact.
    ///
    /// `in_edge` is **motion-aware**: the current point in the trigger zone OR
    /// the segment from the previous sample crossing it (see
    /// [`reveal_edge_segment_crosses`]) — natural mouse motion delivers samples
    /// 30–200 px apart, so a fast approach can jump clean over a static point
    /// zone. `in_band` stays a **point** test: the keep-alive / hide-grace logic
    /// needs "is the pointer *now* over the band", not "did the path ever touch
    /// it" (a motion-aware band would never let go once the pointer left). The
    /// active seam grab band extends this point contact slightly into content so
    /// the revealed rail cannot hide beneath its resize cursor.
    pub(super) fn reveal_pointer_contact(
        &self,
        px_x: f64,
        prev_px_x: Option<f64>,
        cell: CellSize,
        side: RailSide,
    ) -> (bool, bool) {
        if self.pointer_drag.scrollbar_grab().is_some() {
            return (false, false);
        }
        let in_edge = self.pointer_in_reveal_edge(px_x, side)
            || prev_px_x.is_some_and(|prev| {
                let (surface_w, _pad) = self.reveal_surface_metrics();
                reveal_edge_segment_crosses(side, prev, px_x, self.reveal_reach_px(), surface_w)
            });
        let in_band = self.pointer_in_reveal_band(px_x, cell, side)
            || self.pointer_over_rail_seam(px_x, cell);
        (in_edge, in_band)
    }

    /// Whether the auto-hidden rail overlay is drawn (and hit-tested) this
    /// frame: autohide active AND the state machine currently visible AND no
    /// window overlay is open.
    ///
    /// The last clause is the fix for the "can't right-click Settings" report:
    /// the revealed rail strip is composited *topmost* — over the panes AND over
    /// the `overlay_top` window overlay (context menu / Settings / palette). If
    /// the rail were drawn while a menu is open it would paint over the menu,
    /// hiding items the pointer is trying to click. An open window overlay owns
    /// the screen, so the floating rail steps aside until it closes (the
    /// reveal-machine phase is held via `set_suspend`, so the rail reappears
    /// afterward if the pointer is still near the edge). Hit-testing already
    /// short-circuits to the overlay while it is open, so suppressing the draw
    /// keeps render and hit-test consistent.
    pub(super) fn rail_overlay_visible(&self) -> bool {
        if !self.rail_autohide_active() {
            return false;
        }
        // RAIL-PIN: a context menu opened FROM the rail, or a workspace rename
        // prompt, is anchored to the rail — the rail must stay revealed under it
        // rather than stepping aside like an unrelated window overlay does.
        // `set_suspend` alone was insufficient: the draw was gated on
        // `!overlay.is_open()`, so the rail vanished the moment its own context
        // menu opened; and the rename prompt (not an `overlay`) released the
        // suspend, so the grace elapsed and hid the rail mid-rename.
        if self.rail_pinned_open() {
            return true;
        }
        !self.overlay.is_open() && self.rail_autohide.is_visible(Instant::now())
    }

    /// Whether an open menu/prompt is anchored to the workspace rail and so
    /// should keep the auto-hide rail revealed (RAIL-PIN): a rail context menu
    /// (workspace slot or empty rail), a workspace rename prompt, or a
    /// "Save as Layout" name prompt (which a rail slot can spawn). Tab renames
    /// and non-rail overlays are excluded — only surfaces that target the rail
    /// pin it open.
    pub(super) fn rail_pinned_open(&self) -> bool {
        // RAIL-DRAG: in-flight workspace reorder and seam-resize drags are
        // rail-anchored gestures — hold the floating overlay open for their
        // whole lifetime so the drop target or resize edge never vanishes.
        self.rail_ws_drag.is_some()
            || self.rail_seam_drag
            || self.overlay.is_workspace_rail_context_menu()
            || matches!(
                self.rename_state.as_ref().map(|state| state.target),
                Some(
                    RenameTarget::Workspace(_)
                        | RenameTarget::SaveLayout(_)
                        | RenameTarget::SaveAllLayout
                )
            )
    }

    /// Hover the revealed rail overlay from the live pointer, using the overlay
    /// band geometry (window-edge origin, overlay width) rather than the pinned
    /// reservation (which is `NONE` under autohide). Clears any stale top-bar
    /// hover and keeps the default cursor over the band.
    pub(super) fn update_rail_overlay_hover(
        &mut self,
        x_px: f64,
        y_px: f64,
        cell: CellSize,
        side: RailSide,
    ) {
        let _ = side;
        let hit = self.rail_geom_px(cell).map_or(TabHit::None, |geometry| {
            geometry.hit(PxPoint::new(x_px, y_px))
        });
        let hover = (hit != TabHit::None).then_some(hit);
        let mut redraw = false;
        if self.tab_rail.hover != hover {
            self.tab_rail.set_hover(hover);
            redraw = true;
        }
        if self.tab_bar.hover.is_some() {
            self.tab_bar.set_hover(None);
            redraw = true;
        }
        self.apply_cursor_icon(CursorIcon::Default);
        if redraw {
            // Rail hover highlight lives in the overlay signature, which is only
            // recomputed inside the `should_rebuild_frame` gate — mark the frame
            // dirty so the hover repaints over an otherwise-idle terminal.
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Build the F4-P3 revealed rail overlay for this frame: the `rail_cols ×
    /// rows` strip snapshot (rail glyphs + baked panel tint), its window-edge
    /// origin, and the occluding wash + content-facing seam quads. `None` unless
    /// the overlay is currently revealed. The owned snapshot must outlive the GPU
    /// call, so the caller holds this and lends a [`gpu::RailOverlay`] from it.
    pub(super) fn build_rail_overlay(&self, cell: CellSize) -> Option<RailOverlayData> {
        let side = self.rail_autohide_side()?;
        if !self.rail_overlay_visible() {
            return None;
        }
        let cols = self.rail_overlay_cols();
        let rows = self.tab_rail_grid_rows();
        if cols == 0 || rows == 0 {
            return None;
        }
        let origin = self.rail_overlay_origin_px(cell, side);
        let output = self.render_rail_widget(cols, rows, origin, cell, side);
        let mut snapshot = Snapshot {
            dimensions: Dimensions::new(cols, rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); cols * rows],
        };
        for glyph in output.glyphs {
            if glyph.row < rows && glyph.col < cols {
                snapshot.cells[glyph.row * cols + glyph.col] =
                    crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        let (wash, seam) = self.build_rail_overlay_quads(cell, side);
        let mut base_gaps = Vec::new();
        if let Some(gpu) = self.gpu.as_ref() {
            let (surface_w, surface_h) = gpu.surface_size();
            let seam_x = match side {
                RailSide::Left => origin[0] + cols as f32 * cell.width as f32,
                RailSide::Right => origin[0],
            };
            let panel_rect = match side {
                RailSide::Left => [0.0, 0.0, seam_x, surface_h as f32],
                RailSide::Right => [seam_x, 0.0, surface_w as f32, surface_h as f32],
            };
            // COLORED-BG-FLOOR EXEMPT: floating-rail base-gap chrome quads (see
            // `chrome_panel_paint`).
            let alpha = crate::native::gpu::content_build_opacity(
                self.effective_window_bg_alpha(gpu.transparency_capable()),
                self.settings.cell_bg_opacity,
            );
            base_gaps = tab_panel::base_gap_quads(
                panel_rect,
                [
                    origin[0],
                    origin[1],
                    origin[0] + cols as f32 * cell.width as f32,
                    origin[1] + rows as f32 * cell.height as f32,
                ],
                tab_chrome::panel_tint(self.tab_bar_colors(), self.tab_panel_strength()),
                alpha,
            );
        }
        let slot_rows = self.rail_geom().slot_rows;
        Some(RailOverlayData {
            snapshot,
            origin,
            rail_glyph_dy_rows: crate::grid::rail_label_descender_safe_dy_rows(
                slot_rows,
                slot_rows.saturating_sub(1) / 2,
                cell.height,
            ),
            widget_quads: output.quads,
            base_gaps,
            wash,
            seam,
        })
    }

    /// The revealed rail overlay's wash and its content-facing seam, in surface
    /// pixels. CHROME-ALPHA: the wash takes the SAME panel-wash alpha as the
    /// pinned bands (via [`Self::chrome_panel_paint`]) — the floating rail is
    /// the same chrome surface, so toggling auto-hide can no longer change the
    /// band's effective translucency. The old dedicated near-opaque reveal
    /// floor (`max(p, 0.85)`) made the revealed band ignore the window's
    /// translucency entirely, which read as a jarring opacity jump against the
    /// tab bar and the pinned rail once window transparency shipped on by
    /// default. The geometry hugs the window edge (not grid-embedded), so it
    /// goes through [`tab_panel::overlay_band_quads`] with the resolved seam x.
    pub(super) fn build_rail_overlay_quads(
        &self,
        cell: CellSize,
        side: RailSide,
    ) -> (Option<SolidQuad>, Option<SolidQuad>) {
        let Some(gpu) = self.gpu.as_ref() else {
            return (None, None);
        };
        let (surface_w, surface_h) = gpu.surface_size();
        let (panel_color, wash_alpha, seam) = self.chrome_panel_paint();
        let seam_x = self.rail_overlay_seam_x(cell, side);
        let axis = match side {
            RailSide::Left => tab_panel::PanelAxis::Left,
            RailSide::Right => tab_panel::PanelAxis::Right,
        };
        tab_panel::overlay_band_quads(
            axis,
            seam_x,
            surface_w as f32,
            surface_h as f32,
            gpu.scale().round().max(1.0),
            panel_color,
            wash_alpha,
            seam,
            tab_chrome::SEAM_ALPHA,
        )
    }

    /// The single-pane render-cache key for the revealed rail overlay (F4-P3).
    /// `default()` (not revealed) is a frame-to-frame constant, so the pinned /
    /// no-autohide path keeps its byte-identical cache behavior; when revealed,
    /// the visibility + geometry + a hash of the rail's visual state (active
    /// index, tab count, hover, titles) make a reveal / hide / switch / rename /
    /// hover / auto-width change reclassify to a Full rebuild.
    pub(super) fn rail_overlay_render_signature(&self, cell: CellSize) -> RailOverlaySignature {
        use std::hash::{Hash, Hasher};
        let Some(side) = self.rail_autohide_side() else {
            return RailOverlaySignature::default();
        };
        if !self.rail_overlay_visible() {
            return RailOverlaySignature::default();
        }
        let cols = self.rail_overlay_cols();
        let origin = self.rail_overlay_origin_px(cell, side);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use tab_bar::TabBarSource;
        // The rail lists WORKSPACES, so its visual state hashes the workspace
        // list (active index, count, names), not the active tab titles.
        let source = self.sessions.rail_source();
        source.active_tab().hash(&mut hasher);
        source.tab_count().hash(&mut hasher);
        for idx in 0..source.tab_count() {
            source.tab_title(idx).hash(&mut hasher);
        }
        // Hover state changes the highlighted slot, so a hover move while
        // revealed must repaint.
        format!("{:?}", self.tab_rail.hover).hash(&mut hasher);
        RailOverlaySignature {
            visible: true,
            cols,
            origin_bits: [origin[0].to_bits(), origin[1].to_bits()],
            content_hash: hasher.finish(),
        }
    }
}

/// F4-P3 reveal-zone regression: whether a raw physical pointer x is inside the
/// auto-hide reveal **trigger** zone — an interior band from the rail's window
/// edge inward by `reach` (= window padding + the scaled `tab_rail_reveal_px`).
/// Left: `x ≤ reach`; right: `x ≥ surface_w − reach`.
///
/// Padding-aware by construction: a zone that stopped at the bare surface edge
/// (`[0, reveal_px]`) sat *behind* the window's empty padding margin, so it was
/// only reachable by shoving the pointer into the extreme corner — the reported
/// "only reveals when the pointer leaves the window". Including the padding in
/// `reach` extends the zone through the margin and `reveal_px` into visible
/// content, reachable well before the pointer leaves. Pure so the geometry is
/// unit-tested with real padding without a GPU/window.
pub(super) fn reveal_edge_contains(side: RailSide, px_x: f64, reach: f64, surface_w: f64) -> bool {
    match side {
        RailSide::Left => px_x <= reach,
        RailSide::Right => px_x >= surface_w - reach,
    }
}

/// F4-P3 motion-aware trigger: whether the pointer *segment* from `prev_px_x` to
/// `curr_px_x` intersects the reveal trigger band on the rail side. The left band
/// is `[0, reach]`; the right band is `[surface_w − reach, surface_w]`.
///
/// A live pointer trace showed the reveal armed reliably only when the pointer
/// overshot OFF the window edge (where the compositor clamps and delivers a run
/// of in-zone samples): at real cursor speed consecutive samples jump 30–200 px
/// and hop clean over a static point zone, so aiming *at* the edge frequently
/// registered nothing. Testing the whole segment arms a deliberate approach
/// regardless of speed — a move from `px_x = 60` to `px_x = −5` has neither
/// endpoint in `[0, reach]` yet its path crosses the band. The current point is
/// folded in by the caller as the first-sample fallback (no `prev`). Pure so the
/// geometry is unit-tested without a GPU/window.
pub(super) fn reveal_edge_segment_crosses(
    side: RailSide,
    prev_px_x: f64,
    curr_px_x: f64,
    reach: f64,
    surface_w: f64,
) -> bool {
    let (lo, hi) = (prev_px_x.min(curr_px_x), prev_px_x.max(curr_px_x));
    match side {
        // Left band [0, reach]: the segment reaches into it (`lo ≤ reach`)
        // without lying entirely off the window to the left (`hi ≥ 0`).
        RailSide::Left => lo <= reach && hi >= 0.0,
        // Right band [surface_w − reach, surface_w]: the segment reaches into it
        // (`hi ≥ surface_w − reach`) without lying entirely off to the right.
        RailSide::Right => hi >= surface_w - reach && lo <= surface_w,
    }
}

/// F4-P3: whether a raw physical pointer x is inside the reveal **keep-alive**
/// region — the UNION of the trigger zone ([`reveal_edge_contains`]) and the
/// drawn overlay band (window edge → content-facing `seam_x`). Hide grace
/// begins only on leaving this union. Unioning the two explicitly (rather than
/// assuming the band always contains the trigger zone) keeps the keep-alive
/// correct even if a future width makes the band narrower than the padding-aware
/// trigger. Left band: `x < seam_x`; right band: `x > seam_x`.
pub(super) fn reveal_band_contains(
    side: RailSide,
    px_x: f64,
    seam_x: f64,
    reach: f64,
    surface_w: f64,
) -> bool {
    if reveal_edge_contains(side, px_x, reach, surface_w) {
        return true;
    }
    match side {
        RailSide::Left => px_x < seam_x,
        RailSide::Right => px_x > seam_x,
    }
}
