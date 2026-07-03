// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeSet;

use crate::core::{
    CursorStyle, Dimensions, KeyboardModes as CoreKeyboardModes, LinkId, Terminal,
    uri_has_openable_scheme,
};
use crate::graphics::{StoredImageId, VisiblePlacement};
use crate::grid::CursorRenderParams;
use crate::input::KeyModes;
use crate::input::Modifiers;
use crate::selection::AbsoluteSelectionRange;
use crate::text::CellSize;

use super::app::platform_opener::OpenerOs;
use super::image_layer::ImageUpload;
use super::overlay::OverlayRenderSignature;
use super::search_ui::SearchRenderSignature;

pub(super) fn key_modes_from_core(modes: CoreKeyboardModes) -> KeyModes {
    KeyModes {
        application_cursor: modes.application_cursor,
        application_keypad: modes.application_keypad,
        kitty_keyboard_flags: modes.kitty_keyboard_flags,
    }
}

pub(super) fn image_uploads_for_visible(
    terminal: &Terminal,
    visible: &[VisiblePlacement],
    cached: &BTreeSet<StoredImageId>,
) -> Vec<ImageUpload> {
    let mut requested = BTreeSet::new();
    visible
        .iter()
        .filter(|placement| {
            !cached.contains(&placement.image_id) && requested.insert(placement.image_id)
        })
        .filter_map(|placement| {
            terminal
                .graphics()
                .store()
                .get(placement.image_id)
                .map(ImageUpload::from)
        })
        .collect()
}

/// Whether the platform's "open" modifier is currently held: Ctrl on Linux,
/// Cmd (the super/logo key) on macOS.
///
/// macOS translates Ctrl+left-click into a SECONDARY (right) click at the OS
/// level, so winit reports it as a right button and it opens the context menu —
/// Ctrl can never reach the left/open path on a Mac. Cmd+left-click is NOT
/// translated (stays a left event) and is the macOS-native "open" convention,
/// so it is the open modifier there. Linux is unchanged (Ctrl).
///
/// The OS is taken as a value ([`OpenerOs`]) rather than read from `cfg!`
/// inline so BOTH arms are unit-testable on one CI host (the v0.4.0 lesson:
/// never let the macOS branch go unexercised). Production passes
/// [`OpenerOs::host`].
pub(super) fn open_modifier_held(mods: Modifiers, super_key: bool, os: OpenerOs) -> bool {
    match os {
        OpenerOs::Macos => super_key, // Cmd on macOS (Ctrl is taken by secondary-click)
        // Ctrl on Linux and Windows (Cmd/Super is not the open convention there).
        OpenerOs::Linux | OpenerOs::Windows => mods.ctrl,
    }
}

/// Whether a click/hover should trigger the open/armed-underline path: the
/// platform open modifier ([`open_modifier_held`]) is held, and either mouse
/// reporting is off or Shift overrides it (the TUI mouse-reporting escape
/// hatch). All three open behaviors — OSC 8 hyperlink open, interactive-path
/// open, and the armed-underline hover decoration — funnel through this one
/// predicate so the open gesture is consistent and platform-aware everywhere.
pub(super) fn hyperlink_action_allowed(
    mods: Modifiers,
    super_key: bool,
    mouse_reporting_enabled: bool,
    os: OpenerOs,
) -> bool {
    open_modifier_held(mods, super_key, os) && (!mouse_reporting_enabled || mods.shift)
}

pub(super) fn openable_hyperlink_uri(uri: &str) -> bool {
    uri_has_openable_scheme(uri)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeometryUpdate {
    Full,
    CursorOnly,
    Retained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderSignature {
    pub(super) content: RenderContentSignature,
    pub(super) cursor: CursorRenderSignature,
}

impl RenderSignature {
    pub(super) fn update_from(previous: Option<&Self>, next: &Self) -> GeometryUpdate {
        match previous {
            None => GeometryUpdate::Full,
            Some(previous) if previous == next => GeometryUpdate::Retained,
            Some(previous) if previous.content == next.content => GeometryUpdate::CursorOnly,
            Some(_) => GeometryUpdate::Full,
        }
    }
}

/// Per-contributor render-cache fragment for the overlay registry. `Inert` ⇒
/// the contributor adds nothing to the composite cache key, so an inactive
/// feature never perturbs the geometry-update decision. Each active feature
/// folds its compact, change-observable state into its own variant.
///
/// Foundation note: every variant except `Inert` is currently unconstructed in
/// production (the contributor `*_overlay_signature()` methods all return
/// `Inert`); the variants land here so the Wave N+1 feature packets fill in
/// only their own submodule body without re-editing this enum.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
// Foundation scaffolding: every variant except `Inert` is intentionally
// unconstructed in production until its Wave N+1 feature packet fills in the
// corresponding `*_overlay_signature()` body. Landing the variants now is the
// whole point of the dissolver — feature packets edit only their own submodule,
// never this enum.
#[allow(dead_code)]
pub(super) enum OverlayFragment {
    /// Contributes nothing to the cache key — the off/inactive state.
    Inert,
    /// HINTS label-overlay epoch (bumped when the visible label set changes).
    Hints { label_epoch: u64 },
    /// COPYMODE caret + optional selection anchor (cell coordinates).
    CopyMode {
        caret: (usize, usize),
        anchor: Option<(usize, usize)>,
    },
    /// VE4-v1 cursor-trail animation phase.
    CursorTrail { phase: u32 },
    /// ID1/VE4 cursor-glow animation phase (glow routed as overlay quads per
    /// D-IDVE-1, so it is a contributor slot like the trail).
    CursorGlow { phase: u32 },
    /// ID3/U5 background treatment (quantized scrim + treatment discriminant).
    Background { scrim_q: u16, treat: u8 },
    /// RENAME modal text + cursor. Changes on every keystroke so the painted
    /// input overlay repaints via a Full geometry update.
    Rename { text: String, cursor: usize },
    /// VE4 new-output fade: a monotonic epoch bumped once per rebuild while any
    /// row is mid-fade, so each animation frame reclassifies (the quad alphas
    /// change while the cell content does not). `Inert` once every row settles.
    NewRowFade { epoch: u64 },
    /// BELL visual flash: a monotonic epoch bumped once per rebuild while the
    /// flash is decaying, so each animation frame reclassifies. `Inert` once the
    /// flash settles and on the off / urgent-only path.
    BellFlash { epoch: u64 },
    /// Active IME pre-edit string. Changes on every composition keystroke so the
    /// inline pre-edit overlay repaints via a Full geometry update. `Inert` when
    /// no composition is in progress.
    ImePreedit { text: String },
    /// OPEN-NOTICE (P0-2) transient open-failure banner text. Changes when a new
    /// failure is raised so the banner repaints; `Inert` when no notice is in
    /// flight (the default / success path), keeping the cache decision
    /// unchanged.
    OpenNotice { text: String },
    /// UX-A (Phase 11) transient "Ctrl+click to open" discoverability hint.
    /// `Inert` when the hint is not shown (the default / feature-off path), so
    /// the cache decision is unchanged; `ClickHint { shown: true }` while the
    /// bottom-left hint is visible so it repaints when it raises and clears.
    ClickHint { shown: bool },
    /// UX-A (Phase 11) armed underline on the Ctrl+hovered interactive-path
    /// span. `Inert` unless `interactive_paths` is on, Ctrl is held, and a
    /// resolved path is hovered — so plain hover and feature-off are unchanged;
    /// the span coordinates change the key so moving the armed hover repaints.
    ArmedPath {
        row: usize,
        start: usize,
        end: usize,
    },
}

/// Folds the NEW overlay contributors' fragments into one hashable cache key.
/// The existing selection/search/overlay/hovered_hyperlink/prompt_marks_epoch
/// signature fields are left as-is (D-INFRA-1/D-INFRA-6) — only the open
/// frontier is generalized here. When every contributor is inactive every
/// fragment is `Inert`, so the composite is a frame-to-frame constant and the
/// geometry-update gate behaves exactly as before this field existed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct OverlayCompositeSignature {
    pub(super) hints: OverlayFragment,
    pub(super) copy_mode: OverlayFragment,
    pub(super) cursor_trail: OverlayFragment,
    pub(super) cursor_glow: OverlayFragment,
    pub(super) background: OverlayFragment,
    pub(super) new_row_fade: OverlayFragment,
    pub(super) rename: OverlayFragment,
    pub(super) bell_flash: OverlayFragment,
    pub(super) ime_preedit: OverlayFragment,
    pub(super) open_notice: OverlayFragment,
    /// UX-A (Phase 11) bottom-left click-to-open hint. `Inert` on the default /
    /// feature-off / not-shown path, so the composite stays constant there.
    pub(super) click_hint: OverlayFragment,
    /// UX-A (Phase 11) Ctrl+hover armed underline span. `Inert` unless armed, so
    /// toggling Ctrl while hovering a path reclassifies to a Full rebuild.
    pub(super) armed_path: OverlayFragment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderContentSignature {
    pub(super) terminal_revision: u64,
    pub(super) viewport_offset: usize,
    pub(super) scrollback_len: usize,
    /// RV4 smooth-scroll sub-row offset, as `f32::to_bits()`. Constant `0` on
    /// the off path / at rest (so the cache decision is unchanged), and changes
    /// every animating frame so a glide reclassifies the cache to a Full update
    /// and the GPU rebuilds the shifted vertices.
    pub(super) scroll_frac_bits: u32,
    pub(super) grid: Dimensions,
    pub(super) cell: CellSize,
    pub(super) selection: Option<SelectionSignature>,
    pub(super) search: SearchRenderSignature,
    pub(super) overlay: OverlayRenderSignature,
    pub(super) hovered_hyperlink: Option<LinkId>,
    pub(super) graphics: Vec<VisibleGraphicSignature>,
    pub(super) presentation_epoch: u64,
    /// SH2 status-gutter invalidation: a pure OSC 133 status transition can move
    /// prompt marks without bumping the terminal render revision, so the gutter
    /// would not repaint on a coalesced redraw. The native layer folds a
    /// monotonic prompt-marks epoch in here only while the status gutter is on;
    /// while it is off the epoch never advances, so the default render path
    /// stays byte-identical.
    pub(super) prompt_marks_epoch: u64,
    /// Overlay-registry composite cache key for the NEW painted contributors
    /// (hints / copy-mode / cursor-trail / background). All fragments are
    /// `Inert` while their features are off, so this field is a constant on the
    /// default path and the geometry-update decision is unchanged from before
    /// the overlay registry landed.
    pub(super) overlays: OverlayCompositeSignature,
    /// F4-P3 rail auto-hide overlay cache key. The revealed rail floats over the
    /// terminal without changing its content, so a pure reveal/hide (or a hover
    /// / tab-switch / auto-width change while revealed) would not perturb any of
    /// the fields above and the frame would be wrongly `Retained`. Folding the
    /// overlay's visibility + geometry + visual state in here makes those
    /// transitions reclassify to a Full rebuild. `default()` (not revealed) is a
    /// frame-to-frame constant, so the pinned / no-autohide path is unchanged.
    pub(super) rail_overlay: RailOverlaySignature,
}

/// Cache key for the F4-P3 revealed rail overlay. `default()` — not revealed —
/// is a constant, so the geometry-update gate is unchanged when autohide is off
/// or the rail is hidden.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RailOverlaySignature {
    /// Whether the floating rail is drawn this frame.
    pub(super) visible: bool,
    /// Overlay band width in cells (auto-width / manual resize changes it).
    pub(super) cols: usize,
    /// Band origin in physical px, as `f32::to_bits()` (surface/side changes).
    pub(super) origin_bits: [u32; 2],
    /// Hash of the rail's visual state (active index, tab count, hover, titles)
    /// so a switch / rename / hover while revealed rebuilds.
    pub(super) content_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CursorRenderSignature {
    pub(super) visible: bool,
    pub(super) style: CursorStyle,
    /// Quantized cursor-animation key (ID1 easing alpha + VE4 slide offset).
    ///
    /// The render cache only varies the cursor geometry on a `CursorOnly`
    /// reclassification, which fires when `content` is equal but `cursor`
    /// differs. The pre-existing `visible`/`style` fields cannot observe an
    /// alpha-only or sub-cell-offset change, so a live cursor animation would
    /// be invisible to the cache and freeze. Folding a quantized snapshot of
    /// [`CursorRenderParams`] in here makes the change observable: an animating
    /// frame perturbs this key → `CursorOnly` → the GPU re-threads the live
    /// params. When both features are off the params are the identity
    /// (`offset == [0, 0]`, `alpha == 1.0`) which quantizes to
    /// [`CursorAnimKey::IDENTITY`], so the key is a frame-to-frame constant and
    /// the classification stays `Retained` — the plain path is byte-identical.
    pub(super) anim: CursorAnimKey,
}

/// Change-observable, equality-stable quantization of [`CursorRenderParams`] for
/// the render-cache signature. The raw params carry `f32`s (no `Eq`); this key
/// buckets them into integers so it can live in the `Eq`/`Hash`-bearing
/// signature while still flagging a visible animation step.
///
/// Identity contract (the kill-shot): a fully opaque, zero-offset cursor MUST
/// quantize to [`Self::IDENTITY`] exactly. `offset == [0.0, 0.0]` rounds to
/// `(0, 0)` and `alpha == 1.0` rounds to the full alpha bucket, so the default
/// path produces a constant key and never spuriously reclassifies to
/// `CursorOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct CursorAnimKey {
    /// Sub-cell offset quantized to quarter-pixel buckets (VE4 slide).
    pub(super) offset_q: (i32, i32),
    /// Alpha quantized to `1/1024` buckets, clamped to `0..=1` (ID1 easing).
    pub(super) alpha_q: u16,
}

impl CursorAnimKey {
    /// Quarter-pixel offset buckets: fine enough that a slide reads as smooth,
    /// coarse enough that floating-point jitter at rest cannot leave the `(0,0)`
    /// identity bucket.
    const OFFSET_SCALE: f32 = 4.0;
    /// Alpha buckets across `0..=1`. `1.0` maps to exactly `ALPHA_SCALE`.
    const ALPHA_SCALE: f32 = 1024.0;
    /// The identity bucket: zero offset, full opacity. Equal to
    /// `CursorAnimKey::from_params(&CursorRenderParams::default())`. The
    /// default-identity gate asserts against this; it is referenced only from the
    /// test battery in production builds, so the `dead_code` allow documents the
    /// contract anchor without forcing a synthetic production use.
    #[allow(dead_code)]
    pub(super) const IDENTITY: Self = Self {
        offset_q: (0, 0),
        alpha_q: Self::ALPHA_SCALE as u16,
    };

    pub(super) fn from_params(params: &CursorRenderParams) -> Self {
        Self {
            offset_q: (
                (params.offset[0] * Self::OFFSET_SCALE).round() as i32,
                (params.offset[1] * Self::OFFSET_SCALE).round() as i32,
            ),
            alpha_q: (params.alpha.clamp(0.0, 1.0) * Self::ALPHA_SCALE).round() as u16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SelectionSignature {
    pub(super) start: (usize, usize),
    pub(super) end: (usize, usize),
    /// MOUSE-RECT: whether the live selection is a rectangular/column (block)
    /// run. The highlight geometry differs for the same two corner endpoints
    /// (a column band vs a wrapped run), so block-ness must take part in the
    /// render-cache signature — otherwise a cached frame could be reused with
    /// the wrong highlight shape. Folding it into the per-selection signature
    /// (rather than a top-level field) keeps it observable only while a
    /// selection exists, so a cleared-but-stale block flag never forces a
    /// spurious rebuild.
    pub(super) block: bool,
}

impl SelectionSignature {
    pub(super) fn from_range(range: AbsoluteSelectionRange, block: bool) -> Self {
        Self {
            start: (range.start.row, range.start.column),
            end: (range.end.row, range.end.column),
            block,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisibleGraphicSignature {
    pub(super) id: u64,
    pub(super) image_id: u64,
    pub(super) row: usize,
    pub(super) column: usize,
    pub(super) source: (u32, u32, u32, u32),
    pub(super) display_columns: usize,
    pub(super) display_rows: usize,
    pub(super) pixel_offset_x: i32,
    pub(super) pixel_offset_y: i32,
    pub(super) z_index: i32,
    pub(super) generation: u64,
}

impl From<&VisiblePlacement> for VisibleGraphicSignature {
    fn from(value: &VisiblePlacement) -> Self {
        Self {
            id: value.id.0,
            image_id: value.image_id.0,
            row: value.row,
            column: value.column,
            source: (
                value.source.x,
                value.source.y,
                value.source.width,
                value.source.height,
            ),
            display_columns: value.display_columns,
            display_rows: value.display_rows,
            pixel_offset_x: value.pixel_offset_x,
            pixel_offset_y: value.pixel_offset_y,
            z_index: value.z_index,
            generation: value.generation,
        }
    }
}

pub(super) fn visible_graphics_signature(
    visible: &[VisiblePlacement],
) -> Vec<VisibleGraphicSignature> {
    visible.iter().map(VisibleGraphicSignature::from).collect()
}
