// SPDX-License-Identifier: GPL-3.0-only
//! Chrome visibility, placement, widget rendering, and panel painting for the
//! native app window.

use super::*;

#[derive(Default)]
pub(super) struct TabPanelFrameQuads {
    pub(super) base_gaps: Vec<SolidQuad>,
    pub(super) overlays: Vec<SolidQuad>,
}

impl TabPanelFrameQuads {
    pub(super) fn is_empty(&self) -> bool {
        self.base_gaps.is_empty() && self.overlays.is_empty()
    }
}

impl App {
    pub(super) fn update_window_title(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(title) = ({
            // P0-3: OSC-title event path — poison-recover, never abort.
            let mut terminal = crate::native::lock_recover(&self.terminal);
            changed_window_title(&mut terminal, &self.options.title)
        }) else {
            return;
        };

        window.set_title(&title);
    }

    pub(super) fn active_window_title(&self) -> String {
        self.terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.title().map(ToOwned::to_owned))
            .unwrap_or_else(|| self.options.title.clone())
    }

    pub(super) fn sync_active_window_title(&self) {
        if let Some(window) = self.window.as_ref() {
            window.set_title(&self.active_window_title());
        }
    }

    pub(super) fn should_show_tab_bar(&self) -> bool {
        // ≥2 tabs always shows the bar; otherwise honor the opt-in
        // `always_show_tab_bar` setting, and show a lone tab when it carries a
        // custom name so a named single "workflow" tab is visible (F4 ODP-7 /
        // F4-NF1).
        self.sessions.tab_count() >= 2
            || self.settings.always_show_tab_bar
            || self.sessions.lone_tab_has_title_override()
    }

    /// Whether the workspace rail is shown this frame (design doc ODP-2). The
    /// rail lists workspaces, independent of the top tab bar: `Auto` (default)
    /// shows it only once a second workspace exists, so a single-workspace
    /// session keeps the top-only tab bar with zero chrome change; `Always` and
    /// the explicit `Left`/`Right` sides pin it even with one workspace.
    pub(super) fn should_show_workspace_rail(&self) -> bool {
        self.settings.workspace_rail.always_visible() || self.sessions.workspace_count() >= 2
    }

    /// Which side the workspace rail occupies. An explicit `Left`/`Right` in
    /// `workspace_rail` wins; otherwise the side is inherited from
    /// `tab_bar_placement` (migration: a former vertical-tab user keeps their
    /// side), defaulting to the left for the `Top` placement.
    pub(super) fn workspace_rail_side(&self) -> RailSide {
        let placement = self
            .settings
            .workspace_rail
            .forced_side()
            .unwrap_or_else(|| self.effective_placement());
        match placement {
            TabBarPlacement::Right => RailSide::Right,
            TabBarPlacement::Left | TabBarPlacement::Top => RailSide::Left,
        }
    }

    /// Whether ANY tab chrome is shown this frame (top tab bar or workspace
    /// rail). The master gate the reserve / panel / hit-test / pointer paths key
    /// on so the rail is reachable even when the active workspace has a single
    /// unnamed tab (no top bar). `false` is the byte-identical no-chrome frame.
    /// TRANSPARENCY: the window background alpha to render this frame. Full
    /// opacity (`1.0`) unless the `window_transparency` setting is on and the
    /// swapchain can actually composite alpha (`capable`); otherwise the
    /// configured `window_opacity` percent as a `0..=1` fraction. An open overlay
    /// panel keeps the window translucent (MENU-OPACITY): only the panel's own
    /// cell span is held opaque so it stays readable, while the content behind it
    /// keeps showing the desktop through.
    pub(super) fn effective_window_bg_alpha(&self, capable: bool) -> f32 {
        window_bg_alpha_for(
            self.settings.window_transparency,
            capable,
            self.settings.window_opacity,
        )
    }

    /// TRANSPARENCY (MENU-OPACITY / PROMPT-OPACITY): the opaque cell span for a
    /// modal surface merged into the SINGLE-PANE snapshot, in the built
    /// (tab-chrome-decorated) snapshot's coordinates. Two surfaces paint into the
    /// terminal grid centred: an overlay panel at [`overlay_rect`]'s rect, and
    /// the rename/prompt band ([`Self::rename_band_content_rect`]) on its own
    /// path. The rename band paints AFTER (on top of) any overlay, so it takes
    /// precedence when a rename is active; otherwise the open overlay panel's
    /// rect is used. The tab-bar/rail decoration then shifts the content (and
    /// with it the surface) down by the reserved rows and right by the reserved
    /// columns, so the span the renderer sees is that rect plus the reservation.
    /// Holding these cells opaque keeps the surface readable while the terminal
    /// cells around it scale with the window opacity. `None` when neither a
    /// rename nor an overlay is open; the caller also passes `None` on the opaque
    /// window path so that path stays byte-identical.
    pub(super) fn single_pane_overlay_opaque_region(&self) -> Option<crate::grid::CellRegion> {
        let (left, top, width, height) = self.rename_band_content_rect().or_else(|| {
            overlay_rect(&self.overlay, self.grid.columns, self.grid.rows)
                .map(|rect| (rect.left, rect.top, rect.width, rect.height))
        })?;
        let reserve = self.tab_reserve();
        Some(crate::grid::CellRegion {
            left: left + reserve.left_reserved_cols(),
            top: top + reserve.top_rows,
            width,
            height,
        })
    }

    pub(super) fn any_chrome_shown(&self) -> bool {
        self.should_show_tab_bar() || self.should_show_workspace_rail()
    }

    /// The theme-role colors the tab bar paints with (F4). Reads
    /// `effective_theme` so every color is CVD-adapted like the rest of the
    /// chrome; nothing is hardcoded.
    pub(super) fn tab_bar_colors(&self) -> tab_bar::TabBarColors {
        tab_bar::TabBarColors {
            foreground: self.effective_theme.foreground,
            background: self.effective_theme.background,
            inactive: self.effective_theme.inactive,
            active_bg: self.effective_theme.selection,
        }
    }

    /// The placement actually honored by the render path this frame. All three
    /// placements now render (F4-P2 landed the right rail), so
    /// [`TabBarPlacement::effective`] is an identity; the indirection is kept as
    /// the single seam the render/reserve paths read.
    pub(super) fn effective_placement(&self) -> TabBarPlacement {
        self.settings.tab_bar_placement.effective()
    }

    pub(super) fn render_top_bar_widget(
        &self,
        columns: usize,
        y_offset_px: f32,
        cell: CellSize,
        padding: WindowPadding,
    ) -> tab_bar::TabBarOutput {
        let preview = self
            .top_tab_drag
            .filter(|drag| drag.armed)
            .map(|drag| PreviewSource::new(&self.sessions, drag.origin_idx, drag.drop_idx));
        let source: &dyn TabBarSource = preview
            .as_ref()
            .map_or(&self.sessions, |preview| preview as &dyn TabBarSource);
        let pressed = self.top_tab_drag.and_then(|drag| {
            if drag.armed {
                preview.as_ref().and_then(PreviewSource::gap_idx)
            } else {
                Some(drag.origin_idx)
            }
        });
        let mut output = self.tab_bar.render_with_pressed(
            source,
            pressed,
            columns,
            y_offset_px,
            cell,
            padding,
            self.tab_bar_colors(),
            self.tab_panel_strength(),
        );
        let colors = self.tab_bar_colors();
        let panel = tab_chrome::panel_tint(colors, self.tab_panel_strength());
        let accent = chrome_accent_color(tab_chrome::active_fill(colors, panel));
        if let Some(drag) = self.top_tab_drag.filter(|drag| drag.armed)
            && let Some(geometry) = self.top_strip_geom(cell)
        {
            output
                .quads
                .push(geometry.insertion_indicator(drag.drop_idx, drag.origin_idx, accent));
        }
        output
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_rail_widget(
        &self,
        cols: usize,
        rows: usize,
        origin: [f32; 2],
        cell: CellSize,
        side: RailSide,
    ) -> tab_rail::TabRailOutput {
        let rail_source = self.sessions.rail_source();
        let preview = self
            .rail_ws_drag
            .filter(|drag| drag.armed)
            .map(|drag| PreviewSource::new(&rail_source, drag.origin_idx, drag.drop_idx));
        let source: &dyn TabBarSource = preview
            .as_ref()
            .map_or(&rail_source, |preview| preview as &dyn TabBarSource);
        let pressed = self.rail_ws_drag.and_then(|drag| {
            if drag.armed {
                preview.as_ref().and_then(PreviewSource::gap_idx)
            } else {
                Some(drag.origin_idx)
            }
        });
        let mut output = self.tab_rail.render_with_pressed(
            source,
            pressed,
            cols,
            rows,
            origin,
            cell,
            side,
            self.tab_bar_colors(),
            self.rail_geom(),
            self.tab_panel_strength(),
            self.effective_theme.cursor,
            self.settings.tab_rail_autohide,
        );
        let colors = self.tab_bar_colors();
        let panel = tab_chrome::panel_tint(colors, self.tab_panel_strength());
        let accent = chrome_accent_color(tab_chrome::active_fill(colors, panel));
        if let Some(drag) = self.rail_ws_drag.filter(|drag| drag.armed) {
            let committed_geometry =
                ChromeSlotGeom::rail(&rail_source, cols, rows, origin, cell, self.rail_geom());
            output.quads.push(committed_geometry.insertion_indicator(
                drag.drop_idx,
                drag.origin_idx,
                accent,
            ));
        }
        output
    }

    /// The live unified-panel strength (F4-P1 `tab_panel_strength`), passed to
    /// both tab-chrome widgets for the resting-cell tint and used to build the
    /// panel wash/seam background quads.
    pub(super) fn tab_panel_strength(&self) -> f32 {
        self.settings.tab_panel_strength
    }

    /// Horizontal span of the drawn top-panel seam. Hit-testing consumes this
    /// same resolved span so every visible segment owns the row-resize target,
    /// including the pinned or revealed rail junction. CHROME-GAP: this is the
    /// band BACKGROUND extent, which abuts a pinned rail band (chrome always
    /// touches chrome) — the tabs themselves stay gap-inset with the content
    /// columns, so the gap strip at the junction is painted band and owns the
    /// seam row-resize, while tab hits are untouched.
    pub(super) fn top_panel_span(
        &self,
        cell: CellSize,
        surface_width: f32,
        padding: WindowPadding,
    ) -> Option<[f32; 2]> {
        let reserve = self.tab_reserve();
        let mut span = tab_panel::joined_top_span(
            surface_width,
            padding.as_f32(),
            cell.width as f32,
            self.tab_bar_grid_cols(),
            reserve,
            reserve.chrome_gap(padding),
        );
        if span.is_none()
            && self.rail_autohide_active()
            && self.rail_overlay_visible()
            && self.rail_autohide_side() == Some(RailSide::Left)
        {
            span = Some([
                padding.as_f32() + self.rail_overlay_cols() as f32 * cell.width as f32,
                surface_width,
            ]);
        } else if self.rail_autohide_active()
            && self.rail_overlay_visible()
            && self.rail_autohide_side() == Some(RailSide::Right)
        {
            span = Some([0.0, self.rail_overlay_origin_px(cell, RailSide::Right)[0]]);
        }
        span
    }

    /// CHROME-ALPHA: the one shared paint decision for every chrome-panel
    /// surface this frame — the panel surface color, the panel-wash alpha, and
    /// the optional seam color. The pinned top-bar band, the pinned rail band,
    /// and the auto-hide rail overlay ALL take their wash from here, so the
    /// chrome bands compose to the same effective translucency regardless of
    /// the autohide state or placement. Any future divergence must edit this
    /// chokepoint, not a caller.
    pub(super) fn chrome_panel_paint(
        &self,
    ) -> (crate::theme::Srgb, f32, Option<crate::theme::Srgb>) {
        let strength = self.tab_panel_strength();
        let colors = self.tab_bar_colors();
        let panel_color = tab_chrome::panel_tint(colors, strength);
        // The wash tops the band's own cell fill up to the strength-driven
        // target, so it needs the alpha those cells actually compose this
        // frame (window translucency × wallpaper softening — the same value
        // the content build uses).
        let capable = self
            .gpu
            .as_ref()
            .is_some_and(crate::native::gpu::GpuState::transparency_capable);
        // COLORED-BG-FLOOR EXEMPT: chrome wash math — the band's effective
        // opacity is owned by `tab_panel_strength`, and its cells composite at
        // the plain content alpha (chrome strips/rows are floor-exempt), so the
        // top-up target must reference the same plain product.
        let band_cell_alpha = crate::native::gpu::content_build_opacity(
            self.effective_window_bg_alpha(capable),
            self.settings.cell_bg_opacity,
        );
        let wash_alpha = tab_chrome::panel_wash_alpha(strength, band_cell_alpha);
        let seam = (self.settings.tab_seam && strength > 0.0)
            .then(|| tab_chrome::seam_color(colors, panel_color));
        (panel_color, wash_alpha, seam)
    }

    /// Build the F4-P1 unified-panel background quads (ODP-1 wash + ODP-2 seam)
    /// for the current frame, in surface pixels. Empty when the bar is hidden,
    /// the GPU is not up yet, or the band is degenerate; the caller splices these
    /// into the GPU background segment (after the NF11 edge wash). The panel wash
    /// is emitted only when the shared [`Self::chrome_panel_paint`] alpha is
    /// positive; the seam only when the seam knob is on AND the panel is live
    /// (`strength > 0`).
    pub(super) fn tab_panel_bg_quads(&self, cell: CellSize) -> TabPanelFrameQuads {
        let Some(gpu) = self.gpu.as_ref() else {
            return TabPanelFrameQuads::default();
        };
        let show_top = self.should_show_tab_bar();
        // Auto-hide floats the rail (its wash/seam ride the overlay quads), so it
        // contributes no pinned panel band here.
        let show_rail = self.should_show_workspace_rail() && !self.rail_autohide_active();
        if !show_top && !show_rail {
            return TabPanelFrameQuads::default();
        }
        let (surface_w, surface_h) = gpu.surface_size();
        let padding = gpu.window_padding();
        let (panel_color, wash_alpha, seam) = self.chrome_panel_paint();
        let top_span = if show_top {
            self.top_panel_span(cell, surface_w as f32, padding)
        } else {
            None
        };
        // Emit a panel band per shown chrome element (design doc §7: the top bar
        // and the workspace rail are independent bands that can coexist). Each
        // band is grid-aligned to the SAME basis the reserve/decorate/hit-test
        // paths use, so wash, seam, glyphs, and click targets agree to the pixel.
        let mut bands: Vec<(tab_panel::PanelAxis, usize, usize)> = Vec::new();
        if show_top {
            bands.push((tab_panel::PanelAxis::Top, self.tab_bar_rows(), 0));
        }
        if show_rail {
            match self.workspace_rail_side() {
                RailSide::Left => bands.push((tab_panel::PanelAxis::Left, self.rail_cols(), 0)),
                // For a right rail the seam sits at the rail's grid-aligned
                // content edge (the content columns left of the band).
                RailSide::Right => bands.push((
                    tab_panel::PanelAxis::Right,
                    self.rail_cols(),
                    self.grid.columns,
                )),
            }
        }
        // COLORED-BG-FLOOR EXEMPT: chrome panel band quads (see
        // `chrome_panel_paint`).
        let base_alpha = crate::native::gpu::content_build_opacity(
            self.effective_window_bg_alpha(gpu.transparency_capable()),
            self.settings.cell_bg_opacity,
        );
        let mut quads = TabPanelFrameQuads::default();
        for (axis, band_cells, lead_cells) in bands {
            if band_cells == 0 {
                continue;
            }
            let spec = tab_panel::PanelQuadSpec {
                axis,
                surface: [surface_w as f32, surface_h as f32],
                pad: [padding.as_f32(), padding.as_f32()],
                cell: [cell.width as f32, cell.height as f32],
                band_cells,
                top_span: if matches!(axis, tab_panel::PanelAxis::Top) {
                    top_span
                } else {
                    None
                },
                lead_cells,
                // CHROME-GAP: only a RIGHT rail band sits past a content-facing
                // gap on its lead side; every other band keeps a zero lead gap.
                lead_gap_px: if matches!(axis, tab_panel::PanelAxis::Right) {
                    self.tab_reserve().chrome_gap(padding).right
                } else {
                    0.0
                },
                scale_factor: gpu.scale(),
                panel_color,
                wash_alpha,
                seam,
                seam_alpha: tab_chrome::SEAM_ALPHA,
            };
            let origin = match axis {
                tab_panel::PanelAxis::Top => self.top_bar_origin_px(cell),
                tab_panel::PanelAxis::Left | tab_panel::PanelAxis::Right => {
                    self.rail_origin_px(cell)
                }
            };
            let dimensions = match axis {
                tab_panel::PanelAxis::Top => [
                    self.tab_bar_grid_cols() as f32 * cell.width as f32,
                    self.tab_bar_rows() as f32 * cell.height as f32,
                ],
                tab_panel::PanelAxis::Left | tab_panel::PanelAxis::Right => [
                    self.rail_cols() as f32 * cell.width as f32,
                    self.tab_rail_grid_rows() as f32 * cell.height as f32,
                ],
            };
            quads.base_gaps.extend(tab_panel::panel_base_gap_quads(
                &spec,
                [
                    origin[0],
                    origin[1],
                    origin[0] + dimensions[0],
                    origin[1] + dimensions[1],
                ],
                base_alpha,
            ));
            quads.overlays.extend(tab_panel::panel_quads(&spec));
        }
        quads
    }
}
