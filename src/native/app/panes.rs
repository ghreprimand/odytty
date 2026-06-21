// SPDX-License-Identifier: GPL-3.0-only
//! Multi-pane render dispatch (design doc §3.2, Phase 1c-3).
//!
//! When the active tab holds more than one pane, the redraw path branches here
//! instead of the single-pane `update_from_snapshot*` machinery. Each visible
//! pane of the active tab is snapshotted from its own terminal at its own
//! scrollback viewport and drawn into its laid-out sub-rect; the focused pane
//! draws a live cursor, the others none. The 1px themed dividers fill the gaps
//! between panes. When two or more *tabs* exist the tab strip is drawn as an
//! extra one-row pane along the top.
//!
//! Hard boundary: the **single-pane** path never reaches this module — the
//! caller only calls [`App::rebuild_multipane`] when `active_is_single_pane()`
//! is false — so the byte-identical fast path is wholly untouched. This path is
//! deliberately the only new behaviour.
//!
//! Scope note (v1, 1c-3): per-pane interactive overlays (selection / search /
//! hints) are not yet composited for non-focused panes; each pane renders its
//! base terminal content plus, for the focused pane, the cursor. Per-pane
//! overlay parity and the inactive-pane dim are later checkboxes on the Phase 1
//! plan.

use super::*;
use crate::native::gpu::PaneRender;
use crate::native::layout::{PaneRect, divider_rects};

/// Width of the divider gap between panes, in physical pixels. A crisp hairline
/// matching the §8 pixel-smoke invariants.
pub(super) const PANE_DIVIDER_PX: f32 = 1.0;

/// How far on each side of a 1px divider a press still grabs it, in physical
/// pixels — the hairline divider would be near-impossible to hit otherwise.
pub(super) const DIVIDER_GRAB_PX: f32 = 6.0;

/// The per-pane focus-dim value for the multi-pane render path: the focused
/// pane is never dimmed (`0.0`), the inactive panes recede by `inactive_dim`.
/// When `inactive_dim` is `0.0` (the default, and the value the plain renderer
/// profile forces) this returns `0.0` for every pane, so the assignment is an
/// exact identity and the multi-pane frame is byte-identical to before the knob
/// existed.
pub(super) fn pane_focus_dim(is_focused: bool, inactive_dim: f32) -> f32 {
    if is_focused { 0.0 } else { inactive_dim }
}

/// The pixel rectangle available to panes: the surface minus window padding on
/// all sides, and minus the tab-bar strip along the top when it is shown. For a
/// single-pane tab this rect's cell dimensions equal `self.grid`, so the
/// single-pane resize/render geometry is unchanged (it never calls this).
pub(super) fn pane_content_rect(
    width_px: u32,
    height_px: u32,
    cell: CellSize,
    padding: WindowPadding,
    show_tab_bar: bool,
) -> PaneRect {
    let pad = padding.as_f32();
    let tab_h = if show_tab_bar {
        cell.height as f32 * TAB_BAR_ROWS as f32
    } else {
        0.0
    };
    let w = (width_px as f32 - 2.0 * pad).max(0.0);
    let h = (height_px as f32 - 2.0 * pad - tab_h).max(0.0);
    PaneRect::new(pad, pad + tab_h, w, h)
}

impl App {
    /// The pane content rect + cell metrics for the active **multi-pane** tab's
    /// pointer math, or `None` when the active tab is single-pane (the
    /// byte-identical path) or there is no GPU yet. Pointer coordinates
    /// (`pointer_px`) share this absolute physical-pixel basis.
    pub(super) fn multipane_geometry(&self) -> Option<(PaneRect, CellSize)> {
        if self.sessions.active_is_single_pane() {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        let cell = gpu.cell();
        let (w, h) = gpu.surface_size();
        let content =
            pane_content_rect(w, h, cell, gpu.window_padding(), self.should_show_tab_bar());
        Some((content, cell))
    }

    /// Apply an in-progress divider drag to the current pointer position,
    /// re-deriving the grabbed split's ratio and reflowing the affected panes
    /// (per-pane terminal `resize` + PTY `TIOCSWINSZ` via
    /// [`TabSet::resize_all_panes`]) before requesting a repaint. No-op unless a
    /// divider is grabbed and the active tab is multi-pane. The full-window grid
    /// is unchanged by a divider drag, so this reflows pane sub-rects directly
    /// rather than through the window-resize debouncer (which keys on the
    /// whole-window grid and would early-return).
    pub(super) fn drag_divider_to_pointer(&mut self) {
        let Some(target) = self.divider_drag else {
            return;
        };
        let Some((content, cell)) = self.multipane_geometry() else {
            return;
        };
        let Some((x, y)) = self.pointer_px else {
            return;
        };
        if self
            .sessions
            .drag_active_divider(content, PANE_DIVIDER_PX, target, x as f32, y as f32)
            .is_some()
        {
            self.sessions
                .resize_all_panes(content, cell.width, cell.height, PANE_DIVIDER_PX);
            self.sessions.active_mut().needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Rebuild the GPU geometry for a **multi-pane** active tab. Mirrors the
    /// single-pane rebuild's GPU hand-off but assembles one [`PaneRender`] per
    /// visible pane and calls [`GpuState::update_from_panes`].
    pub(super) fn rebuild_multipane(&mut self) {
        let Some((cell, padding, surface)) = self
            .gpu
            .as_ref()
            .map(|gpu| (gpu.cell(), gpu.window_padding(), gpu.surface_size()))
        else {
            return;
        };
        let (surface_w, surface_h) = surface;
        let show_tab_bar = self.should_show_tab_bar();
        let content = pane_content_rect(surface_w, surface_h, cell, padding, show_tab_bar);
        let treatment = self.background_treatment_params();
        let focused = self.sessions.active_id();

        // Per-pane owned snapshots (PaneRender borrows them, so they must
        // outlive the render call). Each pane is snapshotted from its own
        // terminal at its own scrollback offset.
        let rects = self.sessions.active_pane_rects(content, PANE_DIVIDER_PX);
        let mut panes_owned: Vec<(Snapshot, [f32; 2], bool, crate::core::CursorStyle)> =
            Vec::with_capacity(rects.len());
        // The focused pane's overlay inputs, captured while its terminal is
        // locked: (index into `panes_owned`, viewport offset, scrollback len).
        // Used after the loop to paint that pane's own selection + search
        // highlights with a pane-scoped ctx (1c-3c).
        let mut focused_overlay: Option<(usize, usize, usize)> = None;
        for (token, rect) in &rects {
            let Some(session) = self.sessions.get(*token) else {
                continue;
            };
            let offset = session.viewport.offset();
            let Ok(terminal) = session.terminal.lock() else {
                continue;
            };
            let snapshot = terminal.snapshot_with_scrollback(offset);
            let cursor_style = terminal.cursor_style();
            let is_focused = *token == focused;
            if is_focused {
                focused_overlay = Some((
                    panes_owned.len(),
                    offset,
                    terminal.screen().scrollback_len(),
                ));
            }
            drop(terminal);
            panes_owned.push((snapshot, [rect.x, rect.y], is_focused, cursor_style));
        }

        // Paint the focused pane's selection + search highlights onto its own
        // snapshot, keyed to that pane's grid / scrollback / viewport (not the
        // whole-window overlay_ctx). `self.selection` / `self.search` Deref to
        // the focused pane, so only the geometry inputs are pane-specific.
        if let Some((idx, viewport_offset, scrollback_len)) = focused_overlay
            && let Some((snapshot, _, _, _)) = panes_owned.get_mut(idx)
        {
            let pane_grid = snapshot.dimensions;
            self.paint_focused_pane_overlays(
                snapshot,
                pane_grid,
                viewport_offset,
                scrollback_len,
                cell,
            );
        }

        // The tab strip (only when ≥2 tabs) is drawn as a one-row pane along the
        // very top, above the content rect.
        let tab_strip = if show_tab_bar {
            self.tab_bar_strip(cell, padding, surface_w)
        } else {
            None
        };

        // Themed 1px dividers in the gaps between panes. None while zoomed: the
        // focused pane is full-bleed and the layout tree underneath is hidden,
        // so no divider should overdraw it.
        let divider_quads = if self.sessions.active_is_zoomed() {
            Vec::new()
        } else {
            self.sessions
                .active_layout()
                .map(|layout| {
                    let (r, g, b) = self.effective_theme.border;
                    let mut color = text::foreground_linear(Color::Rgb(r, g, b));
                    color[3] = 1.0;
                    divider_rects(layout, content, PANE_DIVIDER_PX)
                        .into_iter()
                        .map(|d| SolidQuad {
                            rect: [d.x, d.y, d.x + d.w, d.y + d.h],
                            color,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        // Assemble the borrow-bound `PaneRender` list.
        let pad = padding.as_f32();
        let mut panes: Vec<PaneRender> = Vec::with_capacity(panes_owned.len() + 1);
        if let Some((snapshot, _)) = tab_strip.as_ref() {
            panes.push(PaneRender {
                snapshot,
                origin: [pad, pad],
                focused: false,
                cursor_style: crate::core::CursorStyle::default(),
                focus_dim: 0.0,
                overlays: &[],
                treatment,
            });
        }
        // Inactive-pane dimming: the focused pane is never dimmed (`0.0`), the
        // others recede by the configured amount. `0.0` (the default, and the
        // forced value on the plain renderer profile) is an exact identity, so
        // every pane renders undimmed and the multi-pane frame stays
        // byte-identical to before this knob existed.
        let inactive_dim = self.settings.effective_inactive_pane_dim();
        for (snapshot, origin, is_focused, cursor_style) in &panes_owned {
            panes.push(PaneRender {
                snapshot,
                origin: *origin,
                focused: *is_focused,
                cursor_style: *cursor_style,
                focus_dim: pane_focus_dim(*is_focused, inactive_dim),
                overlays: &[],
                treatment,
            });
        }

        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_scroll_frac_offset(0.0);
            gpu.update_from_panes(&panes, &divider_quads);
        }
        // Multi-pane v1 does not participate in the single-pane render-signature
        // cache; it rebuilds whenever a visible pane requests a redraw. Reset
        // the cache so the first frame after returning to a single-pane tab does
        // a full rebuild rather than trusting a stale signature.
        self.last_render_signature = None;
    }

    /// Build a one-row snapshot carrying the tab strip glyphs, for the
    /// multi-pane path to draw as the topmost pane. Returns the snapshot plus
    /// the tab bar's solid quads (empty in the current cell-background-based
    /// integration, but threaded through for parity).
    fn tab_bar_strip(
        &self,
        cell: CellSize,
        padding: WindowPadding,
        surface_w: u32,
    ) -> Option<(Snapshot, Vec<SolidQuad>)> {
        let pad = padding.physical_px();
        let columns =
            (surface_w.saturating_sub(pad.saturating_mul(2)) / cell.width.max(1)) as usize;
        if columns == 0 {
            return None;
        }
        let mut snapshot = Snapshot {
            dimensions: Dimensions::new(columns, TAB_BAR_ROWS as usize),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); columns * TAB_BAR_ROWS as usize],
        };
        let output = self.tab_bar.render(
            &self.sessions,
            columns,
            padding.as_f32(),
            cell,
            padding,
            self.effective_theme.foreground,
            self.effective_theme.background,
            self.effective_theme.selection,
        );
        for glyph in output.glyphs {
            if glyph.col < columns {
                snapshot.cells[glyph.col] = crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        Some((snapshot, output.quads))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::CellSize;
    use crate::native::app::TAB_BAR_ROWS;
    use crate::native::viewport::grid_dimensions_for_with_padding;

    fn cell() -> CellSize {
        CellSize {
            width: 10,
            height: 20,
            baseline: 0,
        }
    }

    #[test]
    fn single_pane_content_rect_matches_the_legacy_grid_math() {
        // The pane content rect's cell dimensions must equal the dims the
        // single-pane resize path produces, so a lone-leaf tab stays
        // byte-identical (no tab bar case).
        let cell = cell();
        let padding = WindowPadding::from_logical(8.0, 1.0);
        let (w, h) = (1280u32, 800u32);
        let rect = pane_content_rect(w, h, cell, padding, false);
        let (cols, rows) = crate::native::layout::grid_dims_for_rect(rect, cell.width, cell.height);
        let legacy = grid_dimensions_for_with_padding(w, h, cell, padding);
        assert_eq!((cols, rows), (legacy.columns, legacy.rows));
    }

    #[test]
    fn tab_bar_shrinks_the_content_rect_by_exactly_the_strip() {
        let cell = cell();
        let padding = WindowPadding::from_logical(8.0, 1.0);
        let (w, h) = (1280u32, 800u32);
        let without = pane_content_rect(w, h, cell, padding, false);
        let with = pane_content_rect(w, h, cell, padding, true);
        // Same width and x; the strip eats `TAB_BAR_ROWS` cell-heights off the
        // top, shifting y down and reducing height by the same amount.
        assert!((without.w - with.w).abs() < f32::EPSILON);
        assert!((without.x - with.x).abs() < f32::EPSILON);
        let strip = cell.height as f32 * TAB_BAR_ROWS as f32;
        assert!((with.y - (without.y + strip)).abs() < f32::EPSILON);
        assert!((with.h - (without.h - strip)).abs() < f32::EPSILON);
    }

    #[test]
    fn pane_focus_dim_focused_is_always_identity() {
        // The focused pane is never dimmed regardless of the configured amount.
        assert_eq!(pane_focus_dim(true, 0.0), 0.0);
        assert_eq!(pane_focus_dim(true, 0.3), 0.0);
        assert_eq!(pane_focus_dim(true, 1.0), 0.0);
    }

    #[test]
    fn pane_focus_dim_inactive_uses_configured_amount() {
        // Non-focused panes recede by exactly the configured value.
        assert_eq!(pane_focus_dim(false, 0.25), 0.25);
        assert_eq!(pane_focus_dim(false, 1.0), 1.0);
    }

    #[test]
    fn pane_focus_dim_off_is_byte_identical_for_every_pane() {
        // The default-off path: with `inactive_dim == 0.0` both the focused and
        // inactive panes get `0.0`, identical to the pre-feature hardcoded
        // value, so the multi-pane frame is byte-identical. The grid layer
        // already proves `focus_dim == 0.0` is an exact no-op.
        assert_eq!(pane_focus_dim(true, 0.0), 0.0);
        assert_eq!(pane_focus_dim(false, 0.0), 0.0);
    }
}
