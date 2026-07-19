// SPDX-License-Identifier: GPL-3.0-only
//! F4-P1 unified tab **panel** + **seam** geometry — the background-segment
//! quads that back the "Phosphor Flat v2" tab chrome (ODP-1 / ODP-2).
//!
//! [`super::tab_chrome`] owns the *colors* (panel tint, wash alpha, seam color);
//! this module owns the *geometry*: it turns a resolved [`PanelQuadSpec`] into
//! the pixel-space [`SolidQuad`]s the integration layer splices into the
//! background segment of the two GPU update paths (immediately after the NF11
//! wallpaper edge-wash block, so the panel re-tints the padding strips and veils
//! the fills, and the seam sits over the panel — both still under every glyph).
//!
//! Two coupled quads, in this order:
//! 1. **panel wash** — one translucent quad over the whole panel rect, at the
//!    [`super::tab_chrome::panel_wash_alpha`] that tops the band's cell fill
//!    up to the strength-driven coverage target. Emitted only when `p > 0`;
//!    when the cells already compose at or above the target, the
//!    [`super::tab_chrome::panel_tint`] cell layer is the whole panel and no
//!    wash is needed.
//! 2. **seam** — one hairline flush inside the panel's content-facing edge,
//!    `max(1, round(scale))` px, at [`super::tab_chrome::SEAM_ALPHA`]. Emitted
//!    only when a seam color is supplied (caller gates on the seam knob AND a
//!    live panel).
//!
//! Pure and GPU-device-free: unit-tested without a window. Handles all three
//! axes (`Top`, `Left`, `Right`) so right-side tab placement rides this unchanged.

use super::panes::{ChromeGap, TabReserve};
use super::*;
use crate::theme::Srgb;

/// Which edge of the window the panel occupies. `Top` is the horizontal bar;
/// `Left`/`Right` are the vertical rails. The seam always sits on the panel's
/// content-facing edge (bottom for `Top`, right for `Left`, left for `Right`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PanelAxis {
    Top,
    Left,
    Right,
}

/// Fully-resolved inputs for one panel's background quads. The caller resolves
/// all colors through [`super::tab_chrome`] and all geometry through the live
/// window metrics, so this module stays pure geometry + sRGB→linear conversion.
#[derive(Debug, Clone, Copy)]
pub(super) struct PanelQuadSpec {
    pub(super) axis: PanelAxis,
    /// Surface size in physical px `[w, h]`.
    pub(super) surface: [f32; 2],
    /// Window padding in physical px `[x, y]`.
    pub(super) pad: [f32; 2],
    /// Cell size in physical px `[w, h]`.
    pub(super) cell: [f32; 2],
    /// Rail band width in cells (`Left`/`Right`) or bar height in rows (`Top`).
    pub(super) band_cells: usize,
    /// Optional horizontal clip `[left, right]` for the top panel. `None`
    /// preserves the legacy full-window top span. Ignored for side rails.
    pub(super) top_span: Option<[f32; 2]>,
    /// For `Right` only: content cells occupied to the LEFT of the rail band.
    /// The seam sits at the
    /// rail's grid-aligned content edge (`pad_x + lead_cells·cell_w`) so the
    /// wash/seam line up exactly with where the rail glyphs render (the rail is
    /// grid-embedded, left-aligned from the window padding, so a surface-derived
    /// `surface_w − pad − band·cell` edge would float the sub-cell horizontal
    /// remainder off the true band edge). Ignored for `Top`/`Left`, where the
    /// band is flush to the padding and the surface- and grid-derived edges
    /// coincide.
    pub(super) lead_cells: usize,
    /// CHROME-GAP: extra pixels between the lead (content) cells and a RIGHT
    /// rail band — the chrome-facing padding gap that shifts the band away from
    /// the content columns. `0.0` for `Top`/`Left` bands and at zero padding,
    /// keeping those seams byte-identical.
    pub(super) lead_gap_px: f32,
    /// Surface scale factor (for the seam thickness: `max(1, round(scale))` px).
    pub(super) scale_factor: f32,
    /// The panel-tint color the wash quad paints (same color as the cell tint,
    /// so the surface reads as one continuous color regardless of opacity).
    pub(super) panel_color: Srgb,
    /// Wash alpha `p`. `≤ 0` → no wash quad (the tint cell layer is the panel).
    pub(super) wash_alpha: f32,
    /// Seam color, or `None` to omit the seam (knob off, or panel inactive).
    pub(super) seam: Option<Srgb>,
    /// Seam quad alpha.
    pub(super) seam_alpha: f32,
}

fn linear_rgba(c: Srgb, alpha: f32) -> [f32; 4] {
    [
        text::srgb_to_linear(c.0),
        text::srgb_to_linear(c.1),
        text::srgb_to_linear(c.2),
        alpha.clamp(0.0, 1.0),
    ]
}

/// Resolve the top panel's horizontal span when a pinned workspace rail is
/// present. The top seam joins the rail's content-facing seam at the shared
/// panel junction.
///
/// CHROME-GAP frame-continuity rule: content never touches chrome, but chrome
/// always touches chrome. The bar's TABS (and the content below) sit a padding
/// gap away from the rail band, yet the bar band's BACKGROUND extends across
/// that gap strip to abut the rail band edge, so the two chrome bands read as
/// one continuous frame instead of floating apart at the corner. Past a pinned
/// LEFT rail the span therefore starts at the rail's content-facing band edge
/// (`padding + rail_cols·cell_w`, no gap inset); past a RIGHT rail it ends a
/// gap PAST the content edge, at the rail band's near edge. At zero gap both
/// reproduce the historical junction exactly.
pub(super) fn joined_top_span(
    surface_width: f32,
    padding: f32,
    cell_width: f32,
    content_cols: usize,
    reserve: TabReserve,
    gap: ChromeGap,
) -> Option<[f32; 2]> {
    if reserve.left_cols > 0 {
        Some([
            padding + reserve.left_cols as f32 * cell_width,
            surface_width,
        ])
    } else if reserve.right_cols > 0 {
        Some([
            0.0,
            padding + (content_cols + reserve.gap_cols) as f32 * cell_width + gap.right,
        ])
    } else {
        None
    }
}

/// Whether `x` lies on the top panel's drawn horizontal span. A missing clip is
/// the full surface width. Shared by the App hit-test and pure geometry tests.
pub(super) fn top_span_contains_x(span: Option<[f32; 2]>, surface_width: f32, x: f32) -> bool {
    let [left, right] = span.unwrap_or([0.0, surface_width]);
    x >= left && x < right
}

/// The panel's content-facing seam coordinate along the panel's growth axis:
/// for a rail this is the x of the rail↔content boundary; for the top bar the y
/// of the bar↔content boundary.
fn seam_coord(spec: &PanelQuadSpec) -> f32 {
    let band_px = |cell_dim: f32| spec.pad_axis() + spec.band_cells as f32 * cell_dim;
    match spec.axis {
        PanelAxis::Top => band_px(spec.cell[1]),
        PanelAxis::Left => band_px(spec.cell[0]),
        // Grid-aligned to the rail's actual (left-aligned, grid-embedded) band
        // edge rather than surface-derived, so the seam sits exactly on the
        // rail↔content boundary regardless of the sub-cell horizontal remainder.
        // CHROME-GAP: past the content columns the chrome-facing padding gap
        // shifts the band (and thus its content-facing seam) further right;
        // `lead_gap_px == 0.0` reproduces the historical seam exactly.
        PanelAxis::Right => spec.pad[0] + spec.lead_cells as f32 * spec.cell[0] + spec.lead_gap_px,
    }
}

/// The full surface-space panel rectangle for one band.
pub(super) fn panel_rect(spec: &PanelQuadSpec) -> Option<[f32; 4]> {
    let surface_w = spec.surface[0];
    let surface_h = spec.surface[1];
    if surface_w <= 0.0 || surface_h <= 0.0 || spec.band_cells == 0 {
        return None;
    }
    let seam = seam_coord(spec);
    let rect = match spec.axis {
        PanelAxis::Top => {
            let bottom = seam.clamp(0.0, surface_h);
            let [left, right] = spec.top_span.unwrap_or([0.0, surface_w]);
            let left = left.clamp(0.0, surface_w);
            let right = right.clamp(left, surface_w);
            [left, 0.0, right, bottom]
        }
        PanelAxis::Left => [0.0, 0.0, seam.clamp(0.0, surface_w), surface_h],
        PanelAxis::Right => [seam.clamp(0.0, surface_w), 0.0, surface_w, surface_h],
    };
    (rect[2] > rect[0] && rect[3] > rect[1]).then_some(rect)
}

/// Split `rect` around its overlap with `cut`, returning non-overlapping strips.
pub(crate) fn rect_without(rect: [f32; 4], cut: [f32; 4]) -> Vec<[f32; 4]> {
    let overlap = [
        rect[0].max(cut[0]),
        rect[1].max(cut[1]),
        rect[2].min(cut[2]),
        rect[3].min(cut[3]),
    ];
    if overlap[2] <= overlap[0] || overlap[3] <= overlap[1] {
        return vec![rect];
    }
    let candidates = [
        [rect[0], rect[1], rect[2], overlap[1]],
        [rect[0], overlap[3], rect[2], rect[3]],
        [rect[0], overlap[1], overlap[0], overlap[3]],
        [overlap[2], overlap[1], rect[2], overlap[3]],
    ];
    candidates
        .into_iter()
        .filter(|r| r[2] > r[0] && r[3] > r[1])
        .collect()
}

/// Fill only the panel pixels not covered by chrome cells. This closes outer
/// padding and sub-cell remainder strips without double-compositing beneath the
/// cell backgrounds.
pub(super) fn panel_base_gap_quads(
    spec: &PanelQuadSpec,
    cell_coverage: [f32; 4],
    alpha: f32,
) -> Vec<SolidQuad> {
    let Some(rect) = panel_rect(spec) else {
        return Vec::new();
    };
    base_gap_quads(rect, cell_coverage, spec.panel_color, alpha)
}

pub(super) fn base_gap_quads(
    panel_rect: [f32; 4],
    cell_coverage: [f32; 4],
    panel_color: Srgb,
    alpha: f32,
) -> Vec<SolidQuad> {
    if alpha <= 0.0 {
        return Vec::new();
    }
    let color = linear_rgba(panel_color, alpha);
    rect_without(panel_rect, cell_coverage)
        .into_iter()
        .map(|rect| SolidQuad { rect, color })
        .collect()
}

impl PanelQuadSpec {
    /// The window-padding component on the panel's growth axis (x for rails, y
    /// for the top bar).
    fn pad_axis(&self) -> f32 {
        match self.axis {
            PanelAxis::Top => self.pad[1],
            PanelAxis::Left | PanelAxis::Right => self.pad[0],
        }
    }
}

/// Build the background-segment quads (panel wash, then seam) for one panel.
/// Returns an empty vec when nothing is drawn (no wash and no seam), keeping the
/// no-panel / opaque-cells / seam-off frames byte-identical.
pub(super) fn panel_quads(spec: &PanelQuadSpec) -> Vec<SolidQuad> {
    let surface_w = spec.surface[0];
    let surface_h = spec.surface[1];
    if surface_w <= 0.0 || surface_h <= 0.0 || spec.band_cells == 0 {
        return Vec::new();
    }
    let seam_x_or_y = seam_coord(spec);
    let seam_w = spec.scale_factor.round().max(1.0);

    // Panel rect + seam rect per axis.
    let Some(panel_rect) = panel_rect(spec) else {
        return Vec::new();
    };
    let seam_rect: [f32; 4] = match spec.axis {
        PanelAxis::Top => {
            let bottom = seam_x_or_y.clamp(0.0, surface_h);
            let [left, right] = spec.top_span.unwrap_or([0.0, surface_w]);
            let left = left.clamp(0.0, surface_w);
            let right = right.clamp(left, surface_w);
            [left, (bottom - seam_w).max(0.0), right, bottom]
        }
        PanelAxis::Left => {
            let seam_x = seam_x_or_y.clamp(0.0, surface_w);
            [(seam_x - seam_w).max(0.0), 0.0, seam_x, surface_h]
        }
        PanelAxis::Right => {
            let seam_x = seam_x_or_y.clamp(0.0, surface_w);
            [seam_x, 0.0, (seam_x + seam_w).min(surface_w), surface_h]
        }
    };

    let mut quads = Vec::with_capacity(2);
    let push = |quads: &mut Vec<SolidQuad>, rect: [f32; 4], color: [f32; 4]| {
        if rect[2] > rect[0] && rect[3] > rect[1] && color[3] > 0.0 {
            quads.push(SolidQuad { rect, color });
        }
    };

    if spec.wash_alpha > 0.0 {
        push(
            &mut quads,
            panel_rect,
            linear_rgba(spec.panel_color, spec.wash_alpha),
        );
    }
    if let Some(seam) = spec.seam {
        push(&mut quads, seam_rect, linear_rgba(seam, spec.seam_alpha));
    }
    quads
}

/// Build the F4-P3 rail **auto-hide overlay** wash + seam, returned as two
/// separate quads so the integration layer can layer them around the floating
/// strip: the **wash draws UNDER** the strip cells (muting the live content the
/// revealed rail floats over — CHROME-ALPHA: at the same shared panel-wash
/// alpha as the pinned bands, so the band's translucency does not depend on
/// the autohide state), and the **seam draws OVER** the strip (the
/// content-facing edge line).
///
/// Unlike [`panel_quads`], the overlay band is NOT grid-embedded — the caller
/// supplies the resolved content-facing `seam_x` directly (a left band hugs the
/// left padding; a right band hugs the right window edge), so this takes no
/// `lead_cells` / `band_cells`. `axis` selects which side the band occupies
/// (`Top` is not an auto-hide axis and yields nothing). Returns
/// `(wash, seam)` — either may be `None` (zero alpha / no seam color / degenerate
/// surface).
#[allow(clippy::too_many_arguments)]
pub(super) fn overlay_band_quads(
    axis: PanelAxis,
    seam_x: f32,
    surface_w: f32,
    surface_h: f32,
    seam_w: f32,
    panel_color: Srgb,
    wash_alpha: f32,
    seam: Option<Srgb>,
    seam_alpha: f32,
) -> (Option<SolidQuad>, Option<SolidQuad>) {
    if surface_w <= 0.0 || surface_h <= 0.0 {
        return (None, None);
    }
    let seam_w = seam_w.max(1.0);
    let seam_x = seam_x.clamp(0.0, surface_w);
    let (wash_rect, seam_rect): ([f32; 4], [f32; 4]) = match axis {
        PanelAxis::Left => (
            [0.0, 0.0, seam_x, surface_h],
            [(seam_x - seam_w).max(0.0), 0.0, seam_x, surface_h],
        ),
        PanelAxis::Right => (
            [seam_x, 0.0, surface_w, surface_h],
            [seam_x, 0.0, (seam_x + seam_w).min(surface_w), surface_h],
        ),
        // The top bar has no auto-hide overlay (it keeps `always_show_tab_bar`).
        PanelAxis::Top => return (None, None),
    };
    let make = |rect: [f32; 4], color: [f32; 4]| {
        (rect[2] > rect[0] && rect[3] > rect[1] && color[3] > 0.0)
            .then_some(SolidQuad { rect, color })
    };
    let wash = make(wash_rect, linear_rgba(panel_color, wash_alpha));
    let seam = seam.and_then(|s| make(seam_rect, linear_rgba(s, seam_alpha)));
    (wash, seam)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: Srgb = (0x20, 0x20, 0x24);
    const SEAM: Srgb = (0x66, 0x66, 0x66);

    fn base(axis: PanelAxis) -> PanelQuadSpec {
        PanelQuadSpec {
            axis,
            surface: [800.0, 600.0],
            pad: [4.0, 4.0],
            cell: [8.0, 16.0],
            band_cells: if matches!(axis, PanelAxis::Top) {
                1
            } else {
                16
            },
            top_span: None,
            // Right-rail lead: content columns to the left of the band. In
            // this fixture the surface is exactly `pad + (lead + band)·cell + pad`
            // (800 = 4 + (83 + 16)·8 + 4), so the grid-aligned right seam lands at
            // the same 668 px a surface-derived edge would — no remainder.
            lead_cells: 83,
            lead_gap_px: 0.0,
            scale_factor: 1.0,
            panel_color: PANEL,
            wash_alpha: 0.10,
            seam: Some(SEAM),
            seam_alpha: 0.45,
        }
    }

    #[test]
    fn left_rail_panel_spans_to_the_seam_and_seam_is_flush_inside() {
        let spec = base(PanelAxis::Left);
        let quads = panel_quads(&spec);
        assert_eq!(quads.len(), 2, "wash + seam");
        let seam_x = 4.0 + 16.0 * 8.0; // pad_x + rail_cols*cell_w = 132
        // Panel rect: [0,0, seam_x, surface_h].
        assert_eq!(quads[0].rect, [0.0, 0.0, seam_x, 600.0]);
        // Seam rect: 1px flush inside the panel's right (content-facing) edge.
        assert_eq!(quads[1].rect, [seam_x - 1.0, 0.0, seam_x, 600.0]);
    }

    #[test]
    fn right_rail_panel_and_seam_mirror_to_the_far_side() {
        let spec = base(PanelAxis::Right);
        let quads = panel_quads(&spec);
        assert_eq!(quads.len(), 2);
        // Grid-aligned content edge = pad_x + lead_cells·cell_w = 4 + 83·8 = 668
        // (equals surface_w − pad_x − rail·cell here because the fixture has no
        // sub-cell remainder).
        let seam_x = 4.0 + 83.0 * 8.0;
        assert_eq!(
            quads[0].rect,
            [seam_x, 0.0, 800.0, 600.0],
            "panel spans from the rail's content edge to the right window edge"
        );
        // Seam on the panel's LEFT (content-facing) edge.
        assert_eq!(quads[1].rect, [seam_x, 0.0, seam_x + 1.0, 600.0]);
    }

    #[test]
    fn right_rail_seam_is_grid_aligned_not_surface_derived() {
        // Fails-before-guard for the P-RIGHT grid-alignment fix: when the surface
        // carries a sub-cell horizontal remainder (the rail is grid-embedded and
        // left-aligned from the padding, so it does NOT sit flush to
        // `surface_w − pad`), the seam must follow the true grid band edge, not
        // the surface-derived `surface_w − pad − band·cell`.
        let mut spec = base(PanelAxis::Right);
        // 5px remainder: surface 805 = pad 4 + (83 + 16)·8 + pad 4 + 5.
        spec.surface = [805.0, 600.0];
        let quads = panel_quads(&spec);
        let grid_seam = 4.0 + 83.0 * 8.0; // 668 — the rail's real content edge
        let surface_seam = 805.0 - 4.0 - 16.0 * 8.0; // 673 — the wrong, drifted edge
        assert_ne!(grid_seam, surface_seam, "the two derivations differ here");
        // Panel spans from the grid seam to the far window edge (covering the
        // rail glyphs plus the sub-cell remainder + right padding).
        assert_eq!(quads[0].rect, [grid_seam, 0.0, 805.0, 600.0]);
        assert_eq!(quads[1].rect, [grid_seam, 0.0, grid_seam + 1.0, 600.0]);
    }

    #[test]
    fn top_bar_panel_spans_full_width_and_seam_is_at_the_bottom() {
        let spec = base(PanelAxis::Top);
        let quads = panel_quads(&spec);
        assert_eq!(quads.len(), 2);
        let bottom = 4.0 + 1.0 * 16.0; // pad_y + bar_rows*cell_h = 20
        assert_eq!(quads[0].rect, [0.0, 0.0, 800.0, bottom]);
        assert_eq!(quads[1].rect, [0.0, bottom - 1.0, 800.0, bottom]);
    }

    #[test]
    fn top_bar_with_left_rail_starts_at_the_content_edge() {
        let mut spec = base(PanelAxis::Top);
        let content_left = 4.0 + 16.0 * 8.0;
        spec.top_span = Some([content_left, 800.0]);
        let quads = panel_quads(&spec);
        let bottom = 4.0 + 16.0;
        assert_eq!(quads[0].rect, [content_left, 0.0, 800.0, bottom]);
        assert_eq!(quads[1].rect, [content_left, bottom - 1.0, 800.0, bottom]);
    }

    #[test]
    fn joined_left_span_abuts_the_rail_band_while_content_keeps_the_gap() {
        let reserve = TabReserve {
            top_rows: 1,
            left_cols: 16,
            right_cols: 0,
            gap_cols: 0,
        };
        let padding = WindowPadding::from_logical(4.0, 1.0);
        let gap = reserve.chrome_gap(padding);
        let span = joined_top_span(800.0, 4.0, 8.0, 80, reserve, gap).expect("left rail");
        let content = pane_content_rect(
            800,
            600,
            CellSize {
                width: 8,
                height: 16,
                baseline: 0,
            },
            padding,
            reserve,
        );

        // CHROME-GAP frame continuity: the bar BAND starts flush at the rail
        // band's content-facing edge (132 = pad + band) so the two chrome bands
        // touch, while the bar's tab columns (and the content below) sit one
        // padding gap further right, pixel-aligned with each other.
        assert_eq!(gap.left, 4.0, "left gap equals the window padding");
        assert_eq!(span, [132.0, 800.0], "band background abuts the rail band");
        assert_eq!(reserve.left_reserved_cols(), 16, "rail band reserved");
        assert_eq!(content.x, 136.0, "content begins a gap past the rail");
        assert_eq!(
            content.x,
            span[0] + gap.left,
            "tabs and content sit one gap inside the band edge"
        );
    }

    #[test]
    fn joined_right_span_extends_to_the_rail_band_edge() {
        let reserve = TabReserve {
            top_rows: 1,
            left_cols: 0,
            right_cols: 16,
            gap_cols: 0,
        };
        let padding = WindowPadding::from_logical(4.0, 1.0);
        let gap = reserve.chrome_gap(padding);
        // Exact grid width: padding + content + gap + rail + padding.
        let surface_width = 4 + 80 * 8 + 4 + 16 * 8 + 4;
        let span =
            joined_top_span(surface_width as f32, 4.0, 8.0, 80, reserve, gap).expect("right rail");
        let content = pane_content_rect(
            surface_width,
            600,
            CellSize {
                width: 8,
                height: 16,
                baseline: 0,
            },
            padding,
            reserve,
        );
        let content_right = content.x + content.w;

        // CHROME-GAP frame continuity: the bar's tab columns still end flush
        // with the content's right edge (they share columns), but the band
        // BACKGROUND extends one gap further right to abut the rail band, so
        // the two chrome bands touch instead of floating apart at the corner.
        assert_eq!(gap.right, 4.0, "right gap = padding");
        assert_eq!(reserve.right_reserved_cols(), 16, "rail band reserved");
        assert_eq!(content_right, 644.0, "content ends at the shared edge");
        assert_eq!(
            span,
            [0.0, 648.0],
            "band background reaches the rail band edge"
        );
        assert_eq!(
            span[1],
            content_right + gap.right,
            "band edge is one gap past the shared content edge"
        );
    }

    #[test]
    fn joined_top_span_hit_owns_the_gap_strip_up_to_the_rail_band() {
        let padding = WindowPadding::from_logical(4.0, 1.0);
        let left = TabReserve {
            top_rows: 1,
            left_cols: 16,
            right_cols: 0,
            gap_cols: 0,
        };
        let left_span = joined_top_span(800.0, 4.0, 8.0, 80, left, left.chrome_gap(padding));
        assert!(
            !top_span_contains_x(left_span, 800.0, 131.0),
            "rail band pixels belong to the rail, not the bar"
        );
        assert!(
            top_span_contains_x(left_span, 800.0, 132.0),
            "the bar band (and its gap strip) begins at the rail band edge"
        );
        assert!(
            top_span_contains_x(left_span, 800.0, 135.0),
            "the junction gap strip is bar band"
        );

        let right = TabReserve {
            top_rows: 1,
            left_cols: 0,
            right_cols: 16,
            gap_cols: 0,
        };
        let surface_width = (4 + 80 * 8 + 4 + 16 * 8 + 4) as f32;
        let right_span = joined_top_span(
            surface_width,
            4.0,
            8.0,
            80,
            right,
            right.chrome_gap(padding),
        );
        assert!(
            top_span_contains_x(right_span, surface_width, 647.0),
            "the junction gap strip is bar band up to the rail edge"
        );
        assert!(
            !top_span_contains_x(right_span, surface_width, 648.0),
            "the rail band starts where the bar band ends"
        );
    }

    #[test]
    fn top_band_background_reaches_the_rail_edge_with_nonzero_padding() {
        // Frame-continuity contract: with a pinned left rail and nonzero window
        // padding, the top band's painted background (wash + seam + base gaps)
        // must extend leftward across the gap strip to the rail band edge, so
        // the chrome bands stay joined while the tabs keep their gap inset.
        let reserve = TabReserve {
            top_rows: 1,
            left_cols: 16,
            right_cols: 0,
            gap_cols: 0,
        };
        let padding = WindowPadding::from_logical(4.0, 1.0);
        let mut spec = base(PanelAxis::Top);
        spec.top_span = joined_top_span(800.0, 4.0, 8.0, 80, reserve, reserve.chrome_gap(padding));
        let rail_edge = 4.0 + 16.0 * 8.0; // pad + rail_cols·cell_w = 132
        let bottom = 4.0 + 16.0; // pad + bar_rows·cell_h = 20

        let quads = panel_quads(&spec);
        assert_eq!(quads.len(), 2, "wash + seam");
        assert_eq!(
            quads[0].rect,
            [rail_edge, 0.0, 800.0, bottom],
            "band wash abuts the rail band edge"
        );
        assert_eq!(
            quads[1].rect,
            [rail_edge, bottom - 1.0, 800.0, bottom],
            "bottom seam runs to the rail band edge, joining the rail seam"
        );

        // The base-gap fill covers the junction gap strip: the bar's cell rect
        // starts a gap PAST the rail edge (tabs stay content-aligned), so the
        // strip between rail edge and bar cells is painted band background.
        let bar_cells_left = rail_edge + 4.0; // one gap inside the band edge
        let gaps = panel_base_gap_quads(&spec, [bar_cells_left, 4.0, 796.0, bottom], 0.8);
        assert!(
            gaps.iter()
                .any(|quad| quad.rect[0] == rail_edge && quad.rect[2] >= bar_cells_left),
            "a base-gap quad covers the junction strip from the rail edge: {gaps:?}"
        );
    }

    #[test]
    fn wash_omitted_when_alpha_zero_but_seam_still_drawn() {
        // Opaque cells (p = 0): the tint cell layer is the panel, so no wash
        // quad — but the seam still separates the panel from content.
        let mut spec = base(PanelAxis::Left);
        spec.wash_alpha = 0.0;
        let quads = panel_quads(&spec);
        assert_eq!(quads.len(), 1, "only the seam");
        let seam_x = 4.0 + 16.0 * 8.0;
        assert_eq!(quads[0].rect, [seam_x - 1.0, 0.0, seam_x, 600.0]);
    }

    #[test]
    fn seam_omitted_when_none() {
        let mut spec = base(PanelAxis::Left);
        spec.seam = None;
        let quads = panel_quads(&spec);
        assert_eq!(quads.len(), 1, "only the wash");
        assert!(quads[0].color[3] > 0.0);
    }

    #[test]
    fn nothing_emitted_when_panel_off_and_no_seam() {
        // strength 0 → wash_alpha 0 AND caller passes seam None → byte-identical.
        let mut spec = base(PanelAxis::Left);
        spec.wash_alpha = 0.0;
        spec.seam = None;
        assert!(panel_quads(&spec).is_empty());
    }

    #[test]
    fn seam_width_is_pixel_snapped_across_scale_factors() {
        let seam_x = 4.0 + 16.0 * 8.0;
        for (scale, width) in [(1.0, 1.0), (1.25, 1.0), (1.5, 2.0), (1.75, 2.0), (2.0, 2.0)] {
            let mut spec = base(PanelAxis::Left);
            spec.scale_factor = scale;
            let quads = panel_quads(&spec);
            assert_eq!(quads[1].rect, [seam_x - width, 0.0, seam_x, 600.0]);
        }
    }

    #[test]
    fn base_gap_quads_cover_only_padding_and_remainders() {
        let mut spec = base(PanelAxis::Right);
        spec.surface = [805.0, 603.0];
        let coverage = [668.0, 4.0, 796.0, 596.0];
        let gaps = panel_base_gap_quads(&spec, coverage, 0.8);
        assert_eq!(
            gaps.iter().map(|quad| quad.rect).collect::<Vec<_>>(),
            vec![
                [668.0, 0.0, 805.0, 4.0],
                [668.0, 596.0, 805.0, 603.0],
                [796.0, 4.0, 805.0, 596.0],
            ]
        );
        assert!(gaps.iter().all(|quad| (quad.color[3] - 0.8).abs() < 1e-6));
    }

    #[test]
    fn zero_band_or_surface_emits_nothing() {
        let mut spec = base(PanelAxis::Left);
        spec.band_cells = 0;
        assert!(panel_quads(&spec).is_empty());
        let mut spec = base(PanelAxis::Left);
        spec.surface = [0.0, 600.0];
        assert!(panel_quads(&spec).is_empty());
    }

    // ---- F4-P3 auto-hide overlay band quads -------------------------------

    #[test]
    fn overlay_left_band_washes_to_the_seam_and_seams_inside() {
        let seam_x = 132.0; // pad + cols·cell for the caller-resolved band
        let (wash, seam) = overlay_band_quads(
            PanelAxis::Left,
            seam_x,
            800.0,
            600.0,
            1.0,
            PANEL,
            0.85,
            Some(SEAM),
            0.45,
        );
        // Wash spans the window edge → seam (occludes content the band floats
        // over, including the outer padding), at the reveal alpha.
        let wash = wash.expect("wash present");
        assert_eq!(wash.rect, [0.0, 0.0, seam_x, 600.0]);
        assert!((wash.color[3] - 0.85).abs() < 1e-6, "reveal wash alpha");
        // Seam is 1px flush inside the content-facing (right) edge.
        assert_eq!(
            seam.expect("seam present").rect,
            [seam_x - 1.0, 0.0, seam_x, 600.0]
        );
    }

    #[test]
    fn overlay_right_band_hugs_the_window_edge() {
        // A right overlay hugs the right window edge; seam_x is the band's LEFT
        // (content-facing) edge, resolved by the caller (not grid-embedded).
        let seam_x = 668.0;
        let (wash, seam) = overlay_band_quads(
            PanelAxis::Right,
            seam_x,
            800.0,
            600.0,
            1.0,
            PANEL,
            0.9,
            Some(SEAM),
            0.45,
        );
        assert_eq!(wash.expect("wash").rect, [seam_x, 0.0, 800.0, 600.0]);
        assert_eq!(seam.expect("seam").rect, [seam_x, 0.0, seam_x + 1.0, 600.0]);
    }

    #[test]
    fn overlay_seam_thickens_at_hidpi_and_omits_without_color() {
        let (_, seam) = overlay_band_quads(
            PanelAxis::Left,
            132.0,
            800.0,
            600.0,
            2.0,
            PANEL,
            0.85,
            Some(SEAM),
            0.45,
        );
        assert_eq!(
            seam.expect("seam").rect,
            [132.0 - 2.0, 0.0, 132.0, 600.0],
            "2px at 2x"
        );
        let (wash, seam) = overlay_band_quads(
            PanelAxis::Left,
            132.0,
            800.0,
            600.0,
            1.0,
            PANEL,
            0.85,
            None,
            0.45,
        );
        assert!(
            wash.is_some() && seam.is_none(),
            "seam omitted when no color"
        );
    }

    #[test]
    fn overlay_top_axis_and_degenerate_surface_emit_nothing() {
        let (w, s) = overlay_band_quads(
            PanelAxis::Top,
            20.0,
            800.0,
            600.0,
            1.0,
            PANEL,
            0.85,
            Some(SEAM),
            0.45,
        );
        assert!(
            w.is_none() && s.is_none(),
            "top bar has no auto-hide overlay"
        );
        let (w, s) = overlay_band_quads(
            PanelAxis::Left,
            132.0,
            0.0,
            600.0,
            1.0,
            PANEL,
            0.85,
            Some(SEAM),
            0.45,
        );
        assert!(
            w.is_none() && s.is_none(),
            "degenerate surface emits nothing"
        );
    }

    /// CHROME-ALPHA regression: for the same wash alpha, the pinned band
    /// builder and the auto-hide overlay builder emit washes at EXACTLY that
    /// alpha — neither path floors, scales, or otherwise diverges, so a chrome
    /// band composes the same effective translucency in both autohide states.
    /// (The old reveal path floored its wash at 0.85, which made toggling
    /// auto-hide visibly change the band's opacity under a translucent
    /// window.) Zero alpha suppresses the wash on both paths identically.
    #[test]
    fn pinned_and_overlay_band_washes_carry_the_same_alpha() {
        for alpha in [0.05_f32, 0.2, 0.6, 0.9] {
            let mut spec = base(PanelAxis::Left);
            spec.wash_alpha = alpha;
            spec.seam = None;
            let pinned = panel_quads(&spec);
            assert_eq!(pinned.len(), 1, "pinned band: wash only");
            let (overlay, _) = overlay_band_quads(
                PanelAxis::Left,
                132.0,
                800.0,
                600.0,
                1.0,
                PANEL,
                alpha,
                None,
                0.45,
            );
            let overlay = overlay.expect("overlay wash present");
            assert!(
                (pinned[0].color[3] - alpha).abs() < 1e-6
                    && (overlay.color[3] - alpha).abs() < 1e-6,
                "both washes carry alpha {alpha} exactly: pinned {} overlay {}",
                pinned[0].color[3],
                overlay.color[3]
            );
        }
        // Zero alpha: no wash from either path.
        let mut spec = base(PanelAxis::Left);
        spec.wash_alpha = 0.0;
        spec.seam = None;
        assert!(panel_quads(&spec).is_empty());
        let (overlay, _) = overlay_band_quads(
            PanelAxis::Left,
            132.0,
            800.0,
            600.0,
            1.0,
            PANEL,
            0.0,
            None,
            0.45,
        );
        assert!(overlay.is_none(), "zero-alpha overlay wash suppressed");
    }
}
