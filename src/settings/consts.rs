// SPDX-License-Identifier: GPL-3.0-only
use std::time::Duration;

use crate::theme::{Theme, VisualEffect};

pub const THEME_ENV: &str = "ODYTTY_THEME";
pub const VISUAL_ENV: &str = "ODYTTY_VISUAL";
pub const FONT_ENV: &str = "ODYTTY_FONT";
pub const FONT_FAMILY_ENV: &str = "ODYTTY_FONT_FAMILY";
pub const FONT_WEIGHT_ENV: &str = "ODYTTY_FONT_WEIGHT";
pub const FONT_SIZE_ENV: &str = "ODYTTY_FONT_SIZE";
pub const TEXT_GAMMA_ENV: &str = "ODYTTY_TEXT_GAMMA";
pub const STEM_DARKEN_ENV: &str = "ODYTTY_STEM_DARKEN";
pub const MIN_CONTRAST_ENV: &str = "ODYTTY_MIN_CONTRAST";
pub const FOCUS_DIM_ENV: &str = "ODYTTY_FOCUS_DIM";
pub const INACTIVE_PANE_DIM_ENV: &str = "ODYTTY_INACTIVE_PANE_DIM";
pub const RENDER_QUALITY_ENV: &str = "ODYTTY_RENDER_QUALITY";
pub const BACKGROUND_TREATMENT_ENV: &str = "ODYTTY_BACKGROUND_TREATMENT";
pub const BACKGROUND_IMAGE_ENV: &str = "ODYTTY_BACKGROUND_IMAGE";
pub const BACKGROUND_BLUR_RADIUS_ENV: &str = "ODYTTY_BACKGROUND_BLUR_RADIUS";
pub const BACKGROUND_IMAGE_SCRIM_ENV: &str = "ODYTTY_BACKGROUND_IMAGE_SCRIM";
pub const CELL_BG_OPACITY_ENV: &str = "ODYTTY_CELL_BG_OPACITY";
pub const SELECTION_OPACITY_ENV: &str = "ODYTTY_SELECTION_OPACITY";
pub const WINDOW_PADDING_ENV: &str = "ODYTTY_WINDOW_PADDING";
pub const BLOOM_ENV: &str = "ODYTTY_BLOOM";
pub const BLOOM_THRESHOLD_ENV: &str = "ODYTTY_BLOOM_THRESHOLD";
pub const BLOOM_INTENSITY_ENV: &str = "ODYTTY_BLOOM_INTENSITY";
pub const BLOOM_RADIUS_ENV: &str = "ODYTTY_BLOOM_RADIUS";
pub const RETRO_ENV: &str = "ODYTTY_RETRO";
pub const CRT_ENV: &str = "ODYTTY_CRT";
pub const CRT_SCANLINE_INTENSITY_ENV: &str = "ODYTTY_CRT_SCANLINE_INTENSITY";
pub const CRT_SCANLINE_PERIOD_ENV: &str = "ODYTTY_CRT_SCANLINE_PERIOD";
pub const CRT_VIGNETTE_STRENGTH_ENV: &str = "ODYTTY_CRT_VIGNETTE_STRENGTH";
pub const CRT_CURVATURE_ENV: &str = "ODYTTY_CRT_CURVATURE";
pub const SUBPIXEL_ENV: &str = "ODYTTY_SUBPIXEL";
pub const LINE_HEIGHT_ENV: &str = "ODYTTY_LINE_HEIGHT";
pub const BOX_THICKNESS_ENV: &str = "ODYTTY_BOX_THICKNESS";
pub const LIGATURES_ENV: &str = "ODYTTY_LIGATURES";
pub const KITTY_NAMED_TRANSPORTS_ENV: &str = "ODYTTY_KITTY_NAMED_TRANSPORTS";
pub const KEYBINDS_ENV: &str = "ODYTTY_KEYBINDS";
pub const PANE_PREFIX_ENV: &str = "ODYTTY_PANE_PREFIX";
pub const CURSOR_STYLE_ENV: &str = "ODYTTY_CURSOR_STYLE";
pub const CURSOR_BLINK_ENV: &str = "ODYTTY_CURSOR_BLINK";
pub const CURSOR_EASING_ENV: &str = "ODYTTY_CURSOR_EASING";
pub const CURSOR_MOTION_ENV: &str = "ODYTTY_CURSOR_MOTION";
pub const CURSOR_GLOW_ENV: &str = "ODYTTY_CURSOR_GLOW";
pub const CURSOR_GLOW_INTENSITY_ENV: &str = "ODYTTY_CURSOR_GLOW_INTENSITY";
pub const CURSOR_TRAIL_ENV: &str = "ODYTTY_CURSOR_TRAIL";
pub const CURSOR_TRAIL_STRENGTH_ENV: &str = "ODYTTY_CURSOR_TRAIL_STRENGTH";
pub const REDUCED_MOTION_ENV: &str = "ODYTTY_REDUCED_MOTION";
pub const NEW_OUTPUT_FADE_ENV: &str = "ODYTTY_NEW_OUTPUT_FADE";
pub const WINDOW_BORDER_ENV: &str = "ODYTTY_WINDOW_BORDER";
pub const WINDOW_DECORATIONS_ENV: &str = "ODYTTY_WINDOW_DECORATIONS";
pub const WINDOW_TRANSPARENCY_ENV: &str = "ODYTTY_WINDOW_TRANSPARENCY";
pub const WINDOW_OPACITY_ENV: &str = "ODYTTY_WINDOW_OPACITY";
pub const OSC52_READ_ENV: &str = "ODYTTY_OSC52_READ";
pub const OSC52_WRITE_ENV: &str = "ODYTTY_OSC52_WRITE";
pub const SYNTHETIC_STYLES_ENV: &str = "ODYTTY_SYNTHETIC_STYLES";
pub const GEOMETRIC_BOXDRAW_ENV: &str = "ODYTTY_GEOMETRIC_BOXDRAW";
pub const SYMBOL_FALLBACK_ENV: &str = "ODYTTY_SYMBOL_FALLBACK";
pub const SYMBOL_FONT_ENV: &str = "ODYTTY_SYMBOL_FONT";
pub const SYMBOL_MAP_ENV: &str = "ODYTTY_SYMBOL_MAP";
pub const THEMED_UI_ROLES_ENV: &str = "ODYTTY_THEMED_UI_ROLES";
pub const SCROLL_WHEEL_LINES_ENV: &str = "ODYTTY_SCROLL_WHEEL_LINES";
pub const SCROLLBACK_LINES_ENV: &str = "ODYTTY_SCROLLBACK_LINES";
pub const SCROLL_DRAG_SPEED_ENV: &str = "ODYTTY_SCROLL_DRAG_SPEED";
pub const PIXEL_SCROLL_ENV: &str = "ODYTTY_PIXEL_SCROLL";
pub const SCROLL_PIXEL_SPEED_ENV: &str = "ODYTTY_SCROLL_PIXEL_SPEED";
pub const SCROLL_GLIDE_ENV: &str = "ODYTTY_SCROLL_GLIDE";
pub const COPY_ON_SELECT_ENV: &str = "ODYTTY_COPY_ON_SELECT";
pub const SMART_CTRL_C_ENV: &str = "ODYTTY_SMART_CTRL_C";
pub const SELECTION_DRAG_EXTEND_ENV: &str = "ODYTTY_SELECTION_DRAG_EXTEND";
pub const SCROLLBAR_DRAG_ENV: &str = "ODYTTY_SCROLLBAR_DRAG";
pub const WHEEL_ZOOM_ENV: &str = "ODYTTY_WHEEL_ZOOM";
pub const COMMAND_STATUS_GUTTER_ENV: &str = "ODYTTY_COMMAND_STATUS_GUTTER";
pub const ALWAYS_SHOW_TAB_BAR_ENV: &str = "ODYTTY_ALWAYS_SHOW_TAB_BAR";
pub const TAB_BAR_PLACEMENT_ENV: &str = "ODYTTY_TAB_BAR_PLACEMENT";
pub const TAB_BAR_HEIGHT_ENV: &str = "ODYTTY_TAB_BAR_HEIGHT";
pub const WORKSPACE_RAIL_ENV: &str = "ODYTTY_WORKSPACE_RAIL";
pub const TAB_RAIL_WIDTH_ENV: &str = "ODYTTY_TAB_RAIL_WIDTH";
pub const TAB_RAIL_MAX_WIDTH_ENV: &str = "ODYTTY_TAB_RAIL_MAX_WIDTH";
pub const TAB_RAIL_GAP_ENV: &str = "ODYTTY_TAB_RAIL_GAP";
pub const TAB_RAIL_SLOT_ROWS_ENV: &str = "ODYTTY_TAB_RAIL_SLOT_ROWS";
pub const TAB_PANEL_STRENGTH_ENV: &str = "ODYTTY_TAB_PANEL_STRENGTH";
pub const TAB_SEAM_ENV: &str = "ODYTTY_TAB_SEAM";
pub const TAB_RAIL_AUTOHIDE_ENV: &str = "ODYTTY_TAB_RAIL_AUTOHIDE";
pub const TAB_RAIL_REVEAL_PX_ENV: &str = "ODYTTY_TAB_RAIL_REVEAL_PX";
// The vertical rail shows workspaces (tabs are top-only), so these
// `WORKSPACE_RAIL_*` names are the preferred family for its geometry. Each is a
// pure alias onto the same Settings field as its legacy `TAB_RAIL_*` twin, which
// stays fully accepted. The master toggle `ODYTTY_WORKSPACE_RAIL` already exists.
pub const WORKSPACE_RAIL_WIDTH_ENV: &str = "ODYTTY_WORKSPACE_RAIL_WIDTH";
pub const WORKSPACE_RAIL_MAX_WIDTH_ENV: &str = "ODYTTY_WORKSPACE_RAIL_MAX_WIDTH";
pub const WORKSPACE_RAIL_GAP_ENV: &str = "ODYTTY_WORKSPACE_RAIL_GAP";
pub const WORKSPACE_RAIL_SLOT_ROWS_ENV: &str = "ODYTTY_WORKSPACE_RAIL_SLOT_ROWS";
pub const WORKSPACE_RAIL_AUTOHIDE_ENV: &str = "ODYTTY_WORKSPACE_RAIL_AUTOHIDE";
pub const WORKSPACE_RAIL_REVEAL_PX_ENV: &str = "ODYTTY_WORKSPACE_RAIL_REVEAL_PX";
// Canonical rail-side name (the vertical rail shows workspaces). Aliases the
// legacy `ODYTTY_TAB_BAR_PLACEMENT` side selector; accepts left|right and wins
// over the legacy key when both are set.
pub const WORKSPACE_RAIL_SIDE_ENV: &str = "ODYTTY_WORKSPACE_RAIL_SIDE";
pub const SH_CLICK_ENV: &str = "ODYTTY_SH_CLICK";
pub const BUTTONS_ENV: &str = "ODYTTY_BUTTONS";
pub const BUTTONS_ITERM_COMPAT_ENV: &str = "ODYTTY_BUTTONS_ITERM_COMPAT";
pub const BUTTONS_STICKY_ENV: &str = "ODYTTY_BUTTONS_STICKY";
pub const SHELL_INTEGRATION_ENV: &str = "ODYTTY_SHELL_INTEGRATION";
pub const SHELL_KEY_ENHANCEMENT_ENV: &str = "ODYTTY_SHELL_KEY_ENHANCEMENT";
pub const RESTORE_WORKSPACES_ENV: &str = "ODYTTY_RESTORE_WORKSPACES";
pub const CVD_MODE_ENV: &str = "ODYTTY_CVD_MODE";
pub const CVD_STRENGTH_ENV: &str = "ODYTTY_CVD_STRENGTH";
pub const BELL_ENV: &str = "ODYTTY_BELL";
pub const NATIVE_AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";
pub const FOLLOW_OS_THEME_ENV: &str = "ODYTTY_FOLLOW_OS_THEME";
pub const OS_THEME_DARK_ENV: &str = "ODYTTY_OS_THEME_DARK";
pub const OS_THEME_LIGHT_ENV: &str = "ODYTTY_OS_THEME_LIGHT";
pub const CONFIRM_CLOSE_ENV: &str = "ODYTTY_CONFIRM_CLOSE";
pub const SHELL_EXIT_CLOSES_ENV: &str = "ODYTTY_SHELL_EXIT_CLOSES";
pub const SSH_CONFIG_HOSTS_ENV: &str = "ODYTTY_SSH_CONFIG_HOSTS";
pub const REMOTE_INTEGRATION_ENV: &str = "ODYTTY_REMOTE_INTEGRATION";
pub const REMOTE_REUSE_ENV: &str = "ODYTTY_REMOTE_REUSE";
pub const REMOTE_TMUX_ENV: &str = "ODYTTY_REMOTE_TMUX";
pub const REMOTE_PERSIST_ENV: &str = "ODYTTY_REMOTE_PERSIST";
pub const REMOTE_IMAGE_PASTE_ENV: &str = "ODYTTY_REMOTE_IMAGE_PASTE";
pub const SESSION_REPLAY_ENV: &str = "ODYTTY_SESSION_REPLAY";
pub const INTERACTIVE_URLS_ENV: &str = "ODYTTY_INTERACTIVE_URLS";
pub const INTERACTIVE_PATHS_ENV: &str = "ODYTTY_INTERACTIVE_PATHS";
pub const INTERACTIVE_PATHS_BAREWORDS_ENV: &str = "ODYTTY_INTERACTIVE_PATHS_BAREWORDS";
pub const INTERACTIVE_PATHS_CLICK_HINT_ENV: &str = "ODYTTY_INTERACTIVE_PATHS_CLICK_HINT";
pub const INTERACTIVE_PATHS_IMAGE_INLINE_ENV: &str = "ODYTTY_INTERACTIVE_PATHS_IMAGE_INLINE";
pub const INTERACTIVE_PATHS_EDITOR_ENV: &str = "ODYTTY_INTERACTIVE_PATHS_EDITOR";
pub const CONFIG_FILE_NAME: &str = "odytty.conf";
pub const CONFIG_DIR_NAME: &str = "odytty";
/// Subdirectory of the config dir where user theme files (`*.theme`) live.
pub const THEME_DIR_NAME: &str = "themes";
pub const CONFIG_RELOAD_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) const SETTING_ENV_KEYS: &[&str] = &[
    THEME_ENV,
    VISUAL_ENV,
    FONT_ENV,
    FONT_FAMILY_ENV,
    FONT_WEIGHT_ENV,
    FONT_SIZE_ENV,
    TEXT_GAMMA_ENV,
    STEM_DARKEN_ENV,
    MIN_CONTRAST_ENV,
    FOCUS_DIM_ENV,
    INACTIVE_PANE_DIM_ENV,
    RENDER_QUALITY_ENV,
    BACKGROUND_TREATMENT_ENV,
    BACKGROUND_IMAGE_ENV,
    BACKGROUND_BLUR_RADIUS_ENV,
    BACKGROUND_IMAGE_SCRIM_ENV,
    CELL_BG_OPACITY_ENV,
    SELECTION_OPACITY_ENV,
    WINDOW_PADDING_ENV,
    BLOOM_ENV,
    BLOOM_THRESHOLD_ENV,
    BLOOM_INTENSITY_ENV,
    BLOOM_RADIUS_ENV,
    RETRO_ENV,
    CRT_ENV,
    CRT_SCANLINE_INTENSITY_ENV,
    CRT_SCANLINE_PERIOD_ENV,
    CRT_VIGNETTE_STRENGTH_ENV,
    CRT_CURVATURE_ENV,
    SUBPIXEL_ENV,
    LINE_HEIGHT_ENV,
    BOX_THICKNESS_ENV,
    LIGATURES_ENV,
    KITTY_NAMED_TRANSPORTS_ENV,
    KEYBINDS_ENV,
    PANE_PREFIX_ENV,
    CURSOR_STYLE_ENV,
    CURSOR_BLINK_ENV,
    CURSOR_EASING_ENV,
    CURSOR_MOTION_ENV,
    CURSOR_GLOW_ENV,
    CURSOR_GLOW_INTENSITY_ENV,
    CURSOR_TRAIL_ENV,
    CURSOR_TRAIL_STRENGTH_ENV,
    REDUCED_MOTION_ENV,
    NEW_OUTPUT_FADE_ENV,
    WINDOW_BORDER_ENV,
    WINDOW_DECORATIONS_ENV,
    WINDOW_TRANSPARENCY_ENV,
    WINDOW_OPACITY_ENV,
    OSC52_READ_ENV,
    OSC52_WRITE_ENV,
    SYNTHETIC_STYLES_ENV,
    GEOMETRIC_BOXDRAW_ENV,
    SYMBOL_FALLBACK_ENV,
    SYMBOL_FONT_ENV,
    SYMBOL_MAP_ENV,
    THEMED_UI_ROLES_ENV,
    SCROLL_WHEEL_LINES_ENV,
    SCROLLBACK_LINES_ENV,
    SCROLL_DRAG_SPEED_ENV,
    PIXEL_SCROLL_ENV,
    SCROLL_PIXEL_SPEED_ENV,
    SCROLL_GLIDE_ENV,
    COPY_ON_SELECT_ENV,
    SMART_CTRL_C_ENV,
    SELECTION_DRAG_EXTEND_ENV,
    SCROLLBAR_DRAG_ENV,
    WHEEL_ZOOM_ENV,
    COMMAND_STATUS_GUTTER_ENV,
    ALWAYS_SHOW_TAB_BAR_ENV,
    TAB_BAR_PLACEMENT_ENV,
    TAB_BAR_HEIGHT_ENV,
    WORKSPACE_RAIL_ENV,
    TAB_RAIL_WIDTH_ENV,
    TAB_RAIL_MAX_WIDTH_ENV,
    TAB_RAIL_GAP_ENV,
    TAB_RAIL_SLOT_ROWS_ENV,
    TAB_PANEL_STRENGTH_ENV,
    TAB_SEAM_ENV,
    TAB_RAIL_AUTOHIDE_ENV,
    TAB_RAIL_REVEAL_PX_ENV,
    WORKSPACE_RAIL_WIDTH_ENV,
    WORKSPACE_RAIL_MAX_WIDTH_ENV,
    WORKSPACE_RAIL_GAP_ENV,
    WORKSPACE_RAIL_SLOT_ROWS_ENV,
    WORKSPACE_RAIL_AUTOHIDE_ENV,
    WORKSPACE_RAIL_REVEAL_PX_ENV,
    WORKSPACE_RAIL_SIDE_ENV,
    SH_CLICK_ENV,
    BUTTONS_ENV,
    BUTTONS_ITERM_COMPAT_ENV,
    BUTTONS_STICKY_ENV,
    SHELL_INTEGRATION_ENV,
    SHELL_KEY_ENHANCEMENT_ENV,
    RESTORE_WORKSPACES_ENV,
    CVD_MODE_ENV,
    CVD_STRENGTH_ENV,
    BELL_ENV,
    FOLLOW_OS_THEME_ENV,
    OS_THEME_DARK_ENV,
    OS_THEME_LIGHT_ENV,
    CONFIRM_CLOSE_ENV,
    SHELL_EXIT_CLOSES_ENV,
    SSH_CONFIG_HOSTS_ENV,
    REMOTE_INTEGRATION_ENV,
    REMOTE_REUSE_ENV,
    REMOTE_TMUX_ENV,
    REMOTE_PERSIST_ENV,
    REMOTE_IMAGE_PASTE_ENV,
    SESSION_REPLAY_ENV,
    INTERACTIVE_URLS_ENV,
    INTERACTIVE_PATHS_ENV,
    INTERACTIVE_PATHS_BAREWORDS_ENV,
    INTERACTIVE_PATHS_CLICK_HINT_ENV,
    INTERACTIVE_PATHS_IMAGE_INLINE_ENV,
    INTERACTIVE_PATHS_EDITOR_ENV,
    NATIVE_AUTOCLOSE_ENV,
];

/// Default body-font size on a fresh install. Tuned for the bundled default
/// family (Victor Mono), which reads comfortably a notch smaller than the prior
/// JetBrains-era default.
pub const DEFAULT_FONT_SIZE_PX: f32 = 20.0;
pub const MIN_FONT_SIZE_PX: f32 = 6.0;
pub const MAX_FONT_SIZE_PX: f32 = 72.0;
pub const DEFAULT_THEME: Theme = Theme::ODYSSEY_DEFAULT;
pub const DEFAULT_VISUAL: VisualEffect = VisualEffect::Ambient;
pub const DEFAULT_TEXT_GAMMA: f32 = 1.2;
pub const MIN_TEXT_GAMMA: f32 = 0.5;
pub const MAX_TEXT_GAMMA: f32 = 3.0;

/// Stem-darkening strength (`ODYTTY_STEM_DARKEN`): a coverage boost applied at
/// glyph raster time so light-on-dark body text holds weight at small sizes
/// (RV5). `0.0` disables it and is pixel-identical to the pre-feature renderer;
/// `1.0` is the strongest boost. Ships default-on at `0.5` for a visible
/// light-on-dark weight boost. Setting `0.0` is the opt-out and fully restores
/// the classic, pre-feature raster.
pub const DEFAULT_STEM_DARKEN: f32 = 0.7;
pub const MIN_STEM_DARKEN: f32 = 0.0;
pub const MAX_STEM_DARKEN: f32 = 1.0;

/// Minimum fg/bg contrast floor (`ODYTTY_MIN_CONTRAST`): a configurable WCAG
/// contrast ratio that every cell's foreground is lifted to meet, so no app can
/// render illegibly low-contrast text (RV1). The default is an assertive
/// readability floor; `1.0` disables the floor and is pixel-identical to the
/// pre-feature renderer. The lift moves only perceptual lightness, preserving hue.
pub const DEFAULT_MIN_CONTRAST: f32 = 17.0;
pub const MIN_MIN_CONTRAST: f32 = 1.0;
pub const MAX_MIN_CONTRAST: f32 = 21.0;

/// Focus dimming amount (`ODYTTY_FOCUS_DIM`): how much the whole grid (both text
/// and background) recedes perceptually while the window is unfocused (ID2). The
/// dim runs at color-resolution time before the RV1 minimum-contrast floor, so
/// legibility is preserved by construction. `0.0` disables it and is
/// pixel-identical to the pre-feature renderer; higher values dim further. The
/// focused window is never dimmed regardless of this value, so focused frames
/// stay byte-identical to today.
pub const DEFAULT_FOCUS_DIM: f32 = 0.0;
pub const MIN_FOCUS_DIM: f32 = 0.0;
pub const MAX_FOCUS_DIM: f32 = 1.0;

/// Inactive-pane dimming (`ODYTTY_INACTIVE_PANE_DIM`): a subtle dim applied to
/// the non-focused panes of a multi-pane tab so the focused pane stands out, in
/// OKLab so hue is preserved. `0.0` (the default) disables it and is
/// byte-identical to the pre-feature multi-pane renderer — every pane renders
/// undimmed exactly as today. Higher values dim the inactive panes further. The
/// focused pane is never dimmed, and single-pane tabs are never affected.
pub const DEFAULT_INACTIVE_PANE_DIM: f32 = 0.0;
pub const MIN_INACTIVE_PANE_DIM: f32 = 0.0;
pub const MAX_INACTIVE_PANE_DIM: f32 = 1.0;

/// Opt-in OpenSSH config host-name import (`ODYTTY_SSH_CONFIG_HOSTS`). Off by
/// default, so OdyTTY never touches `~/.ssh/config` unless explicitly enabled.
/// When on, only host names/display fields are read through the bounded
/// name-only parser; key material and credentials are never surfaced.
pub const DEFAULT_SSH_CONFIG_HOSTS: bool = false;

/// Opt-in per-session output recording for the replay overlay
/// (`ODYTTY_SESSION_REPLAY`). Off by default, so the PTY pump records nothing
/// and the plain path is byte-identical. When on, each session keeps a bounded
/// in-memory ring of recent screen frames; recording is local-only (never
/// written to disk or sent anywhere).
pub const DEFAULT_SESSION_REPLAY: bool = false;

/// Opt-in interactive filesystem paths (`ODYTTY_INTERACTIVE_PATHS`). Off by
/// default, so the pointer path never scans terminal text for paths and the
/// plain hover path is byte-identical. When on, hovering a path-looking span
/// that resolves to a real entry shows the pointer (hand) cursor; detection is
/// local-only — nothing is logged, persisted, or sent anywhere, and the single
/// `stat` happens only on a hovered candidate. Hover detection runs on the
/// focused pane only (v1 bound, shared with OSC 8 hyperlink hover).
pub const DEFAULT_INTERACTIVE_PATHS: bool = false;

/// Interactive bare URLs (`ODYTTY_INTERACTIVE_URLS`). On by default (matches the
/// common terminal expectation that printed URLs are clickable out of the
/// box). When on, hovering a bare `http(s)://…` (or other
/// allowlisted-scheme) URL that an application printed without an OSC 8 escape
/// shows the pointer (hand) cursor and a Ctrl+hover armed underline, and
/// Ctrl+click opens it through the same argv-only, scheme-allowlisted dispatch
/// as OSC 8 hyperlinks — never auto-opened, never shell-interpolated. Off makes
/// the bare-URL hover scan never run, so the hover path is byte-identical to a
/// build without the feature. Independent of `interactive_paths`: URL opening
/// and filesystem-path detection toggle separately. Explicit OSC 8 hyperlinks
/// always win a tie so a cell is never double-decorated.
pub const DEFAULT_INTERACTIVE_URLS: bool = true;

/// Bare filename detection for interactive paths
/// (`ODYTTY_INTERACTIVE_PATHS_BAREWORDS`). On by default once interactive paths
/// are enabled, so common `ls` output like `carpet1.jpg` can be clicked. The
/// global `interactive_paths` gate is still off by default and resolution stays
/// stat-gated against the pane cwd, so non-existent words remain inert.
pub const DEFAULT_INTERACTIVE_PATHS_BAREWORDS: bool = true;

/// Click-to-open discoverability hint for interactive paths
/// (`ODYTTY_INTERACTIVE_PATHS_CLICK_HINT`). On by default behind the global
/// `interactive_paths` gate. When on, two plain mis-clicks on a resolved path
/// within a short window raise a transient bottom-left "Ctrl+click to open"
/// hint. Off silences only the hint — the hand cursor, Ctrl+hover underline, and
/// Ctrl+click open all still work. The global `interactive_paths` gate is still
/// off by default, so nothing shows until paths are enabled.
pub const DEFAULT_INTERACTIVE_PATHS_CLICK_HINT: bool = true;

/// Inline image opening for interactive paths
/// (`ODYTTY_INTERACTIVE_PATHS_IMAGE_INLINE`). On by default behind the global
/// `interactive_paths` gate. When on, Ctrl+clicking a resolved image path opens
/// the in-OdyTTY viewer; when off, images use the external opener like other
/// paths. The right-click "Open in OdyTTY" action remains available either way.
pub const DEFAULT_INTERACTIVE_PATHS_IMAGE_INLINE: bool = true;

/// Window padding (`ODYTTY_WINDOW_PADDING`): logical pixels of inset on every
/// window edge before the terminal grid begins. `0.0` restores the historical
/// exact edge-to-edge layout; the non-zero default gives text breathing room.
pub const DEFAULT_WINDOW_PADDING_PX: f32 = 4.0;
pub const MIN_WINDOW_PADDING_PX: f32 = 0.0;
pub const MAX_WINDOW_PADDING_PX: f32 = 64.0;

/// Cell background opacity (`ODYTTY_CELL_BG_OPACITY`): the alpha multiplier on
/// every cell's resolved background colour. `0.8` (default, v0.6.0) makes cells
/// slightly translucent so the bundled background shows through; `1.0` leaves
/// cells fully opaque so the cell-vertex output is byte-identical to before,
/// with the `image` treatment then showing only in the window padding. Values
/// `< 1.0` make cells translucent so a background image shows through behind
/// text; the RV1 floor stays safe at any opacity via the readability scrim.
pub const DEFAULT_CELL_BG_OPACITY: f32 = 0.8;
pub const MIN_CELL_BG_OPACITY: f32 = 0.0;
pub const MAX_CELL_BG_OPACITY: f32 = 1.0;

/// Selection opacity (`ODYTTY_SELECTION_OPACITY`): the alpha strength of the
/// text-selection highlight fill, independent of window opacity, theme colours,
/// and the min-contrast floor. The default `0.6` is a translucent tint: the
/// selection reads as a highlight rather than a solid block, yet the
/// punch-through surface-alpha lerp keeps it clearly visible over a transparent
/// or busy backdrop (it is never weaker than the surrounding content). Lower
/// values thin the tint further; `1.0` restores a fully opaque selection,
/// byte-identical to the historical inverse highlight at any window opacity.
/// The RV1 min-contrast floor still holds foreground legibility over the
/// effective composited fill at every setting; the mechanism clamps to `[0,1]`.
pub const DEFAULT_SELECTION_OPACITY: f32 = 0.6;
pub const MIN_SELECTION_OPACITY: f32 = 0.0;
pub const MAX_SELECTION_OPACITY: f32 = 1.0;

/// Background-image CPU box-blur radius (`ODYTTY_BACKGROUND_BLUR_RADIUS`),
/// applied once at load time. `0` (default) leaves the image sharp; the radius
/// is clamped to `MAX_BACKGROUND_BLUR_RADIUS` and skipped for oversized images.
pub const DEFAULT_BACKGROUND_BLUR_RADIUS: u32 = 0;
pub const MAX_BACKGROUND_BLUR_RADIUS: u32 = 256;

/// Sentinel stored in `Settings::background_image` to mean "the original
/// background bundled into the binary" rather than a real on-disk file (v0.6.0).
/// Deliberately an obviously-not-a-path token (angle brackets) so it can never
/// collide with a real wallpaper path. The GPU loader recognizes it and decodes
/// the compiled-in [`crate::native::gpu::default_background::DEFAULT_BACKGROUND_WEBP`]
/// bytes from memory; the settings layer round-trips it through parse, writeback,
/// and `--show-config` so users never see the raw marker.
pub const BUNDLED_BACKGROUND_SENTINEL: &str = "<odytty-default-background>";

/// The user-facing `background_image` config token that opts in to the bundled
/// default, e.g. `background_image = default`. Written by writeback and shown by
/// `--show-config`, and accepted (case-insensitively) by the parser.
pub const BUNDLED_BACKGROUND_TOKEN: &str = "default";

/// Whether a `background_image` value points at the bundled default rather than
/// a real file. Compares against [`BUNDLED_BACKGROUND_SENTINEL`].
pub fn is_bundled_background(path: &std::path::Path) -> bool {
    path.as_os_str() == std::ffi::OsStr::new(BUNDLED_BACKGROUND_SENTINEL)
}

/// The `Settings::background_image` value representing the bundled default.
pub fn bundled_background_path() -> std::path::PathBuf {
    std::path::PathBuf::from(BUNDLED_BACKGROUND_SENTINEL)
}

/// Bounds for the explicit background-image scrim override
/// (`ODYTTY_BACKGROUND_IMAGE_SCRIM`). When unset the scrim is auto-computed to
/// guarantee the RV1 floor; an explicit value is clamped to this range.
pub const MIN_BACKGROUND_IMAGE_SCRIM: f32 = 0.0;
pub const MAX_BACKGROUND_IMAGE_SCRIM: f32 = 1.0;

/// Default background-image scrim (v0.6.0). The shipped identity pairs the
/// bundled default background with a fixed mid scrim so text stays readable out
/// of the box; set `background_image_scrim = auto` to restore the auto-computed
/// floor-safe scrim, or any value in [`MIN_BACKGROUND_IMAGE_SCRIM`]..=[`MAX_BACKGROUND_IMAGE_SCRIM`].
pub const DEFAULT_BACKGROUND_IMAGE_SCRIM: f32 = 0.5;

/// Line-height multiplier (`ODYTTY_LINE_HEIGHT`, LINEHEIGHT): extra vertical
/// leading baked into each glyph cell, expressed as a multiple of the natural
/// font cell height. `1.0` (default) adds zero leading and is byte-identical to
/// the pre-feature atlas: cell height, baseline and glyph coverage are all
/// unchanged. Values above `1.0` grow the cell box and shift the baseline down
/// by the top half of the added rows, so glyphs keep their exact shape and only
/// gain breathing room above and below. The leading is clamped so it can never
/// exceed one extra cell height.
pub const DEFAULT_LINE_HEIGHT: f32 = 1.0;
pub const MIN_LINE_HEIGHT: f32 = 1.0;
pub const MAX_LINE_HEIGHT: f32 = 2.0;

/// Box-drawing stroke-thickness multiplier (`ODYTTY_BOX_THICKNESS`, BOXTHICK):
/// scales the geometric box-drawing / Powerline line weight relative to the
/// DPI-derived default. `1.0` (default) reproduces the historical stroke widths
/// byte-identically (multiplying the light weight by `1.0` is exact in `f32`);
/// values below `1.0` draw thinner rules and above `1.0` draw heavier rules.
/// Only affects the renderer's own geometric box-drawing path — inert when
/// geometric box-drawing is off or when a symbol-map override wins.
pub const DEFAULT_BOX_THICKNESS: f32 = 1.0;
pub const MIN_BOX_THICKNESS: f32 = 0.5;
pub const MAX_BOX_THICKNESS: f32 = 3.0;

/// Mouse-wheel scroll multiplier (`ODYTTY_SCROLL_WHEEL_LINES`, MOUSE-WHEEL-SPEED):
/// rows advanced per wheel notch. The default `6.0` rows per notch is chosen
/// for interactive feel. Applies to BOTH local scrollback and alternate-scroll
/// (DECSET 1007) arrow emulation, so classic pagers (`less`, `man`, `git log`)
/// scroll at the same rows-per-notch as the local viewport. `WHEEL_STEP_LINES`
/// (3) remains the fixed base only for the overlay free-scroll and the
/// notch-reporting / pixel-conversion paths. Stored as `f32` to ride the shared
/// numeric-setting model (slider / keyboard step / range label); the wheel path
/// rounds it to a `usize >= 1`. Full mouse-reporting TUIs still own the wheel —
/// their report carries direction, not magnitude — and continuous (touchpad
/// pixel) deltas are never multiplied.
pub const DEFAULT_SCROLL_WHEEL_LINES: f32 = 6.0;
pub const MIN_SCROLL_WHEEL_LINES: f32 = 1.0;
pub const MAX_SCROLL_WHEEL_LINES: f32 = 10.0;

/// Scrollback retention cap (`ODYTTY_SCROLLBACK_LINES`): the maximum number of
/// logical (hard-terminated) lines kept in history before the oldest are
/// evicted. Bounds steady-state memory so a process streaming unbounded output
/// cannot grow OdyTTY until the OS OOM-kills it. The default matches the common
/// terminal default; `0` means unbounded (no cap — use with care). Stored as
/// `f32` to ride the shared numeric-setting model; the core rounds it to a
/// `usize`. Live-reloadable: lowering it trims existing history immediately.
pub const DEFAULT_SCROLLBACK_LINES: f32 = 10_000.0;
pub const MIN_SCROLLBACK_LINES: f32 = 0.0;
pub const MAX_SCROLLBACK_LINES: f32 = 1_000_000.0;

/// Upper bound on the rows the drag-edge autoscroll advances per ~80 ms tick
/// when the velocity ramp is active (`ODYTTY_SCROLL_DRAG_SPEED=ramp`,
/// MOUSE-AUTOSCROLL-VEL). The ramp grows one extra row per cell-height the
/// pointer is dragged past the edge band and is clamped to this cap so it can
/// never scroll uncontrollably fast. The `legacy` mode pins the step to exactly
/// one row per tick, which is byte-identical to the pre-feature behavior.
pub const MAX_AUTOSCROLL_ROWS: usize = 8;

/// Copy-on-select (`ODYTTY_COPY_ON_SELECT`, MOUSE-COPYSELECT): when on, finishing
/// a local selection also writes the CLIPBOARD (in addition to the PRIMARY
/// selection it always writes). Off by default — PRIMARY and middle-click paste
/// already work regardless, so the off path is byte-identical to before.
pub const DEFAULT_COPY_ON_SELECT: bool = false;

/// Cursor blink-fade easing (`ODYTTY_CURSOR_EASING`, ID1): when on, the cursor
/// eases its opacity in and out across the blink toggle instead of hard
/// on/off-switching. On by default; while disabled or under reduced motion, the
/// cursor renders its alpha at a constant `1.0` and the blink off-phase hides
/// the cursor outright. Purely presentational; never affects cell semantics or
/// the logical cursor position.
pub const DEFAULT_CURSOR_EASING: bool = true;

/// Cursor slide motion (`ODYTTY_CURSOR_MOTION`, VE4): when on, the cursor glides
/// a short sub-cell interpolation between adjacent steady-state positions
/// instead of teleporting. On by default; while at rest or explicitly disabled,
/// the cursor sits at its exact cell origin with zero animation wake. The
/// reduced-motion master setting also forces this static path. Discontinuities
/// (first frame, resize/reflow, scrollback, large jump, unfocused) always snap
/// rather than slide. Purely presentational; the logical cursor position is
/// always the destination cell, so selection/clipboard and TUI semantics are
/// unaffected.
pub const DEFAULT_CURSOR_MOTION: bool = true;

/// Soft cursor glow (`ODYTTY_CURSOR_GLOW`, ID1): when on, one shape-aware
/// analytic aura is drawn behind the cursor glyph, matching Block, Bar, or
/// Underline geometry in the resolved cursor color. On by default; while off
/// no aura geometry is emitted. Purely presentational; never affects cell
/// semantics or the logical cursor position. The aura alpha is capped low
/// enough that adjacent-cell text contrast stays within the RV1 floor.
pub const DEFAULT_CURSOR_GLOW: bool = true;

/// Cursor glow strength (`ODYTTY_CURSOR_GLOW_INTENSITY`, ID1): a user-facing
/// normalized `0.0..=1.0` scale for the aura peak alpha, independent of the
/// whole-scene HDR `bloom_intensity`. `0.0` emits no aura even while
/// `cursor_glow` is on; the default reproduces the calibrated restrained peak
/// (Block `0.08`, Bar/Underline `0.10`, translucent-background lift cap `0.02`);
/// `1.0` doubles the peak while staying bounded so nearby text remains readable
/// and translucent backgrounds never receive an excessive alpha lift. The
/// mapping is `multiplier = intensity / DEFAULT_CURSOR_GLOW_INTENSITY`, so the
/// default value maps to the historical fixed peaks exactly.
pub const DEFAULT_CURSOR_GLOW_INTENSITY: f32 = 0.5;
pub const MIN_CURSOR_GLOW_INTENSITY: f32 = 0.0;
pub const MAX_CURSOR_GLOW_INTENSITY: f32 = 1.0;

/// Cursor motion trail (`ODYTTY_CURSOR_TRAIL`, VE4): when on, a short fading
/// after-image of decaying ghost quads trails the cursor along its slide path
/// while it glides between cells. Rides the existing cursor-slide animation
/// (`cursor_motion`): the trail only appears while a slide is in flight and
/// fully decays as the slide settles, so it never schedules a wake beyond the
/// slide's own animation window. On by default; while off no trail quads are
/// emitted and the render path is byte-identical to before. The ghosts are
/// drawn behind the cursor block in the theme cursor color at low alpha, so
/// they never obscure cell content or affect the logical cursor position.
/// Visible only when `cursor_motion` is also on (the slide it trails).
pub const DEFAULT_CURSOR_TRAIL: bool = true;

/// Reduced motion (`ODYTTY_REDUCED_MOTION`): master accessibility gate for the
/// cursor slide, trail, glow, blink fade, and new-output fade. On forces those
/// effects to their static or instant behavior without changing their stored
/// individual settings. The explicit setting has the same behavior on every
/// supported platform; OS preference discovery remains future work.
pub const DEFAULT_REDUCED_MOTION: bool = false;

/// New-output fade-in (`ODYTTY_NEW_OUTPUT_FADE`, VE4): when on, rows of freshly
/// arrived output at the live tail fade in over a short ease-out ramp instead of
/// appearing instantly. Implemented as a background-color overlay quad that
/// decays from opaque to transparent over each new row, so the underlying cell
/// content is always rendered at full opacity and never drops below the RV1
/// readability floor mid-fade — the quad only *obscures then reveals*. Off by
/// default; while off no fade quads are emitted, no extra wakes are scheduled,
/// and the render path is byte-identical to before. Only fades at the live tail
/// (`viewport_offset == 0`); scrolling back or resizing snaps instantly. The
/// row carrying the cursor is never obscured. Purely presentational.
pub const DEFAULT_NEW_OUTPUT_FADE: bool = false;

/// Programming ligatures (`ODYTTY_LIGATURES`): ASCII contextual `calt`
/// shaping. On by default; `off` performs no shaping and preserves the scalar
/// atlas/geometry output exactly.
pub const DEFAULT_LIGATURES: bool = true;

/// Themed window border (`ODYTTY_WINDOW_BORDER`, ID4): when on, a thin border in
/// the theme `border` role color is drawn around the grid, framing the terminal
/// content. The border is painted as overlay quads within the existing window
/// padding band, so it never eats cell area; its thickness is specified in
/// logical pixels and scaled by the surface DPI factor, and the frame tracks the
/// content rect on resize. Off by default — while off no border quads are
/// emitted and the render path is byte-identical to before. Purely
/// presentational; never affects cell semantics.
pub const DEFAULT_WINDOW_BORDER: bool = false;

/// Show window decorations (`ODYTTY_WINDOW_DECORATIONS`, WIN-DECOR): when on, the
/// window keeps its title bar and borders; when off, OdyTTY requests a borderless
/// surface. On by default — `true` reproduces the historical window-attribute
/// chain (`WindowAttributes::default()` already sets `decorations = true`), so the
/// default startup is pixel-identical. The toggle applies both at window creation
/// and live on a settings change. Effect depends on the environment: Wayland
/// compositors remove the title bar reliably (client-side decorations), while on
/// X11 the request is sent to the window manager as a hint and is honored on a
/// best-effort basis — never a hard guarantee.
pub const DEFAULT_WINDOW_DECORATIONS: bool = true;
/// Window transparency master toggle (TRANSPARENCY). On by default: the default
/// presentation draws the terminal background at `DEFAULT_WINDOW_OPACITY`, so the
/// desktop shows through a little while text, the cursor, and every overlay stay
/// fully opaque. The compositor request is unconditional, so a display server
/// without alpha compositing simply presents opaque; a fully-opaque look is one
/// setting away (`window_transparency = off`, or `window_opacity = 100`, which is
/// byte-identical to the opaque path).
pub const DEFAULT_WINDOW_TRANSPARENCY: bool = true;
/// Default window opacity as a percentage of full opacity, used when
/// `window_transparency` is on. 80% keeps text firmly readable while letting
/// the desktop show through the terminal background a little more generously
/// than a near-opaque value would.
pub const DEFAULT_WINDOW_OPACITY: f32 = 80.0;
/// Minimum window opacity percent: a deliberate deep-transparency floor (20%)
/// for maximum desktop bleed-through. Text, cursor, and selection stay legible
/// regardless — only the background scales toward this value, and the
/// readability scrim keeps glyphs readable independent of it.
pub const MIN_WINDOW_OPACITY: f32 = 20.0;
/// Maximum window opacity percent (fully opaque).
pub const MAX_WINDOW_OPACITY: f32 = 100.0;

/// Continuous pixel-precise scrollback (`ODYTTY_PIXEL_SCROLL`): when on,
/// high-resolution wheels and touchpads that emit pixel deltas scroll the
/// viewport by a continuous sub-row amount tracking physical travel, instead of
/// quantizing to whole notches. On by default. It only affects pixel-precise
/// input — classic detented wheels (line deltas) keep the notch path unchanged
/// whether this is on or off, and at rest the render path is byte-identical.
pub const DEFAULT_PIXEL_SCROLL: bool = true;

/// Animated scrollback glide between discrete wheel notches
/// (`ODYTTY_SCROLL_GLIDE`): when on, a wheel notch moves the
/// integer viewport offset instantly (as always) but the RENDERED viewport
/// eases toward it over a few frames — a forward-chase follower that only ever
/// moves in the scroll direction, so continuous notches cannot sawtooth.
/// Discrete wheels emit whole notches with no sub-step data, so this is the
/// only source of smoothness for them; high-resolution / touchpad devices use
/// `pixel_scroll` instead. On by default (tuned interactive feel); at rest the
/// render path is byte-identical.
pub const DEFAULT_SCROLL_GLIDE: bool = true;

/// Sensitivity multiplier for the continuous pixel-scroll lane
/// (`ODYTTY_SCROLL_PIXEL_SPEED`). `1.0` (default) tracks finger travel exactly
/// (one cell-height of travel = one row); higher scrolls faster than the finger,
/// lower slower. Stored as `f32` to ride the shared numeric-setting model.
/// Applies only to pixel-precise input; detented wheels use `scroll_wheel_lines`.
pub const DEFAULT_SCROLL_PIXEL_SPEED: f32 = 1.0;
pub const MIN_SCROLL_PIXEL_SPEED: f32 = 0.25;
pub const MAX_SCROLL_PIXEL_SPEED: f32 = 4.0;

/// Follow the OS dark/light appearance preference (`ODYTTY_FOLLOW_OS_THEME`,
/// OS-THEME): when on, OdyTTY switches between the `os_theme_dark` and
/// `os_theme_light` themes based on the desktop's color-scheme signal (delivered
/// live by the compositor on Wayland; seeded from `ODYTTY_APPEARANCE` on X11
/// where no live signal exists). Off by default — while off the OS signal is
/// ignored entirely and the authored `theme` drives presentation exactly as
/// before. When on but a direction's theme name is unset (or unknown), that
/// direction keeps the authored theme rather than guessing.
pub const DEFAULT_FOLLOW_OS_THEME: bool = false;

/// The sentinel theme value that follows the OS dark/light appearance
/// (OS-THEME alias). When `ODYTTY_THEME` is set to this value, OdyTTY enables
/// [`Settings::follow_os_theme`] and maps the OS dark signal to the default
/// dark Odyssey theme and the OS light signal to the default light Odyssey
/// theme. The individual `os_theme_dark`/`os_theme_light` settings remain
/// available for custom overrides. This is a config-layer alias only — it never
/// extracts the desktop palette or accent colors.
pub const SYSTEM_THEME_NAME: &str = "system";

/// Default theme name applied to an OS dark signal when `theme = system` is
/// active and the user has not set an explicit `os_theme_dark` override.
pub const DEFAULT_OS_THEME_DARK: &str = "odyssey";

/// Default theme name applied to an OS light signal when `theme = system` is
/// active and the user has not set an explicit `os_theme_light` override.
pub const DEFAULT_OS_THEME_LIGHT: &str = "odyssey-light";

/// Confirm before closing while a foreground job is running (`ODYTTY_CONFIRM_CLOSE`,
/// CLOSE-CONFIRM): when on, a close request (window close button / WM close)
/// while a program is actively running in the terminal opens a confirmation
/// dialog instead of exiting immediately. On by default — the dialog only ever
/// appears when data would actually be lost: an idle shell (no foreground job)
/// always closes silently, exactly as before, and any query error or dead PTY
/// also takes the silent-close path. Off restores unconditional close-on-request.
pub const DEFAULT_CONFIRM_CLOSE: bool = true;

/// Drag-to-extend selection (`ODYTTY_SELECTION_DRAG_EXTEND`, MOUSE-EXTEND): when
/// on, a double-click-then-drag grows the selection by whole words, a
/// triple-click-then-drag by whole lines, and Shift+click extends the current
/// selection to the click. On by default; it only gives
/// meaning to gestures that did nothing before. Off restores the historical
/// behavior where a double/triple-click finalizes and the follow-on drag does
/// not extend. Local selection only; never affects TUI mouse reporting.
pub const DEFAULT_SELECTION_DRAG_EXTEND: bool = true;

/// Draggable scroll thumb (`ODYTTY_SCROLLBAR_DRAG`, MOUSE-SCROLLBAR): when on, a
/// left press on the right-edge scroll indicator grabs it as a thumb and the
/// drag scrubs through scrollback. On by default — the thumb only renders while
/// scrolled back into history, so the grab is inert at the live tail and the
/// off path (and the live-tail path) leave press routing byte-identical. Local
/// only; never affects TUI mouse reporting (a press off the thumb still reports
/// as before).
pub const DEFAULT_SCROLLBAR_DRAG: bool = true;

/// Ctrl+wheel font-size zoom (`ODYTTY_WHEEL_ZOOM`, MOUSE-WHEEL): when on,
/// Ctrl+wheel up grows the font and Ctrl+wheel down shrinks it, within
/// [`MIN_FONT_SIZE_PX`]..[`MAX_FONT_SIZE_PX`]. On by default — it only fires on
/// the explicit Ctrl+wheel gesture while mouse reporting is off, so a plain
/// wheel (and the wheel inside a TUI mouse-reporting app) is byte-identical.
/// Off restores Ctrl+wheel to plain scrollback movement.
pub const DEFAULT_WHEEL_ZOOM: bool = true;

/// Per-command success/fail gutter (`ODYTTY_COMMAND_STATUS_GUTTER`, SH2): when
/// on, a thin coloured bar at the left edge of each finished command's prompt
/// row reads green for an explicit `exit 0` and red for a non-zero exit, sourced
/// from the OSC 133 command blocks and coloured from the active ANSI palette.
/// Off by default — while off the gutter draws nothing and the render path is
/// pixel-identical to today. With shell integration absent no command marks
/// exist, so the gutter is empty regardless of the setting.
pub const DEFAULT_COMMAND_STATUS_GUTTER: bool = false;

/// Always show the tab bar (`ODYTTY_ALWAYS_SHOW_TAB_BAR`, F4 ODP-7): when on,
/// the tab strip renders even with a single tab. Off by default — with one
/// unnamed tab the bar stays hidden and the render path is byte-identical to
/// today. Independent of this setting, a lone tab that carries a custom name
/// (`title_override`) shows the bar regardless, so a named single "workflow"
/// tab is never invisible (F4-NF1).
pub const DEFAULT_ALWAYS_SHOW_TAB_BAR: bool = false;

/// Top tab-bar height in text rows (`ODYTTY_TAB_BAR_HEIGHT`). The classic bar is
/// one text row (`Auto` / the default); a taller bar adds chrome breathing room
/// around that one row of labels, which are centered vertically in the band.
/// Parsed as `auto | <rows>` ([`super::TabBarHeight`]) exactly like the rail
/// width, so the draggable bottom seam and a numeric config both pin a `Manual`
/// height clamped to `[MIN_TAB_BAR_ROWS, MAX_TAB_BAR_ROWS]`; double-clicking the
/// seam resets to `Auto` (one row). `MIN` is the operator's hard floor of one
/// text row; `MAX` caps the chrome so the bar cannot swallow the content.
pub const DEFAULT_TAB_BAR_ROWS: f32 = 1.0;
pub const MIN_TAB_BAR_ROWS: f32 = 1.0;
pub const MAX_TAB_BAR_ROWS: f32 = 5.0;

/// Vertical tab-rail band width in cells (`ODYTTY_TAB_RAIL_WIDTH`, F4-P1/P4).
/// The rail widget's VISUAL width (the rail↔content wallpaper gap is reserved
/// separately). Hot-reloadable so the operator can tune it live. Only affects
/// the `left`/`right` rail placements; the top bar ignores it.
///
/// F4-P4 turned this into an `auto | <cols>` mode ([`super::TabRailWidth`]):
/// `auto` (the new default) sizes the rail to the longest tab title (clamped to
/// `[MIN_TAB_RAIL_WIDTH, tab_rail_max_width]`); a plain integer pins a manual
/// width (clamped to `[MIN_TAB_RAIL_WIDTH, MAX_TAB_RAIL_WIDTH]`), which the seam
/// drag / double-click-reset also write. Old numeric configs parse as `Manual`,
/// so an existing `tab_rail_width = 20` keeps its exact behavior. `MIN`/`MAX`
/// remain the absolute widget bounds the manual value (and the seam drag) clamp
/// to.
pub const DEFAULT_TAB_RAIL_WIDTH: f32 = 16.0;
pub const MIN_TAB_RAIL_WIDTH: f32 = 8.0;
pub const MAX_TAB_RAIL_WIDTH: f32 = 32.0;

/// Upper clamp for the `auto` rail width (`ODYTTY_TAB_RAIL_MAX_WIDTH`, F4-P4):
/// the widest the rail will auto-grow to fit long tab titles before switching
/// to single-line ellipsis truncation. Only consulted in `auto` mode; a manual
/// width can still be dragged up to `MAX_TAB_RAIL_WIDTH`. Bounded by the same
/// absolute widget floor/ceiling so the auto cap can never fall below the min
/// usable width or exceed the widget maximum. Hot-reloadable, rail-only. Stored
/// as `f32` for the shared numeric-setting model; rounded to a `usize` where
/// used.
pub const DEFAULT_TAB_RAIL_MAX_WIDTH: f32 = 24.0;
pub const MIN_TAB_RAIL_MAX_WIDTH: f32 = MIN_TAB_RAIL_WIDTH;
pub const MAX_TAB_RAIL_MAX_WIDTH: f32 = MAX_TAB_RAIL_WIDTH;

/// Rows of band-fill gap between adjacent rail slots (`ODYTTY_TAB_RAIL_GAP`,
/// F4-P1). The top margin before the first slot follows it. Rail-only,
/// hot-reloadable. Stored as `f32`; rounded to a `usize` in `[MIN, MAX]`.
pub const DEFAULT_TAB_RAIL_GAP: f32 = 1.0;
pub const MIN_TAB_RAIL_GAP: f32 = 0.0;
pub const MAX_TAB_RAIL_GAP: f32 = 3.0;

/// Rows each rail slot occupies (`ODYTTY_TAB_RAIL_SLOT_ROWS`, F4-P1): `1` = a
/// compact single-row list (labels truncate, never wrap), `2` = the padded /
/// wrapping default. Rail-only, hot-reloadable. Stored as `f32`; rounded and
/// clamped to `{1, 2}`.
pub const DEFAULT_TAB_RAIL_SLOT_ROWS: f32 = 2.0;
pub const MIN_TAB_RAIL_SLOT_ROWS: f32 = 1.0;
pub const MAX_TAB_RAIL_SLOT_ROWS: f32 = 2.0;

/// Tab-panel strength (`ODYTTY_TAB_PANEL_STRENGTH`, F4-P1): scales the unified
/// translucent panel behind the rail/bar. `0.0` = panel fully off (the pre-panel
/// bare-labels look); `1.0` (default) = the strongest, most opaque panel
/// surface. Drives both the panel-tint lift (cell backgrounds) and the
/// panel-wash quad alpha `p = strength × (1 − cell_bg_opacity)`. Both axes,
/// hot-reloadable.
pub const DEFAULT_TAB_PANEL_STRENGTH: f32 = 1.0;
pub const MIN_TAB_PANEL_STRENGTH: f32 = 0.0;
pub const MAX_TAB_PANEL_STRENGTH: f32 = 1.0;

/// Tab-panel seam line (`ODYTTY_TAB_SEAM`, F4-P1): when on, one hairline
/// separates the panel from the content on both axes, derived from the inactive
/// TEXT role at α0.45 and luma-capped so it can never bloom. On by default; off
/// removes the line only (the panel stays). Both axes, hot-reloadable.
pub const DEFAULT_TAB_SEAM: bool = true;

/// Rail auto-hide (`ODYTTY_TAB_RAIL_AUTOHIDE`, F4-P1/P3): the reveal/hide
/// behavior is provided by rail autohide. Off by default,
/// rail-only.
pub const DEFAULT_TAB_RAIL_AUTOHIDE: bool = false;

/// Rail auto-hide reveal-zone width in **logical** px (`ODYTTY_TAB_RAIL_REVEAL_PX`,
/// F4-P3): how close to the rail's window edge the pointer must come to summon
/// an auto-hidden rail. Logical (scaled by the display scale factor at the
/// comparison site) so the zone is a consistent physical size across displays —
/// a physical-px zone shrinks under fractional/HiDPI scaling and the rail
/// became unreachably thin. Default raised to 16: at 8 the trigger band was too
/// thin to catch a normal-speed approach (a fast pointer's samples skip over it
/// unless it clamps at a screen edge), so the rail felt like it needed the
/// pointer shoved hard into the corner. 16 logical px is a comfortable target
/// while still well short of incidental content-area contact.
pub const DEFAULT_TAB_RAIL_REVEAL_PX: f32 = 16.0;
pub const MIN_TAB_RAIL_REVEAL_PX: f32 = 1.0;
pub const MAX_TAB_RAIL_REVEAL_PX: f32 = 32.0;

/// Click-to-position-cursor (`ODYTTY_SH_CLICK`, SH-CLICK/F2): when on, a plain
/// left click on the live shell input moves the shell's cursor to the clicked
/// position by emitting the matching number of Left/Right cursor keys —
/// the click slice of OSC 133 `click_events`, never a shell-input takeover.
/// ON by default since F2: it stays inert unless a
/// cooperating shell has advertised `click_events=1` on its prompt, so a
/// non-integrated shell never triggers it — the blast radius of the default is
/// exactly the shells whose integration opted in. While off (or without an
/// advertising shell) the pointer path emits no bytes and changes no pixel.
pub const DEFAULT_SH_CLICK: bool = true;

/// Button protocol master gate (`ODYTTY_BUTTONS`, docs/buttons.md): when on,
/// programs can define clickable buttons in their output (OSC
/// `133;P;odytty-button` label runs and iTerm2 `1337;Button=` point buttons)
/// and clicking a live button reports its integer code back to the program as
/// `CSI ? 1337 ; code ~`. Off by default: the sequences are parsed and
/// discarded, nothing is stored or rendered, and clicks never emit a report —
/// the off path is byte-identical to plain output. The gate is enforced at the
/// parser and again at the pointer, so turning it off also deadens buttons
/// already on screen. When on, `ODYTTY_BUTTONS=1` is injected into new
/// terminal sessions' environment so emitters can discover support.
///
/// On by default: click reports are composed by the terminal from the parsed
/// integer code, so a program can never inject report bytes; clicks are
/// suppressed while a mouse-reporting application owns the pointer; and the
/// risk class matches OSC 8 hyperlinks, which already ship enabled.
pub const DEFAULT_BUTTONS: bool = true;

/// iTerm2-compatible button spelling (`ODYTTY_BUTTONS_ITERM_COMPAT`): accept
/// `OSC 1337 ; Button=type=custom ; code=N` point buttons in addition to the
/// native spelling. Sub-gate of `ODYTTY_BUTTONS`; inert while the master gate
/// is off. On by default so the common iTerm2 spelling is recognized wherever
/// the master gate is on.
pub const DEFAULT_BUTTONS_ITERM_COMPAT: bool = true;

/// Sticky button lifetime (`ODYTTY_BUTTONS_STICKY`): honor `scope=sticky`
/// definitions, which outlive prompt boundaries and keep reporting from
/// scrollback until invalidated or scrolled off. When off, sticky requests
/// downgrade to the block lifetime (dead at the next prompt). Sub-gate of
/// `ODYTTY_BUTTONS`; inert while the master gate is off. Off by default: a
/// button surviving a scroll-away is the one surprising variant, so sticky
/// stays opt-in even with the master gate on.
pub const DEFAULT_BUTTONS_STICKY: bool = false;

/// Automatic OSC 133 shell integration (`ODYTTY_SHELL_INTEGRATION`): when on,
/// default local shell launches receive OdyTTY's prompt-mark hooks at spawn so
/// features that need prompt/input boundaries (selection-delete, prompt jumps,
/// click-to-position support from cooperating shells) can work without editing
/// the user's rc files. On by default: the integration is opt-out and only ever
/// adds prompt-mark hooks to OdyTTY's own default-shell launches at spawn; it
/// never edits the user's rc files and leaves shells started by other means
/// untouched.
pub const DEFAULT_SHELL_INTEGRATION: bool = true;

/// Prompt-scoped key enhancement for bash/zsh (`ODYTTY_SHELL_KEY_ENHANCEMENT`):
/// when on, integrated bash/zsh shells push Kitty keyboard flag 0x1
/// (disambiguate only, so Ctrl+C keeps generating SIGINT) while the prompt owns
/// the line and pop it before each command runs. This makes modified keys like
/// Ctrl+Enter, Shift+Enter, and Ctrl+Backspace reachable as distinct CSI-u
/// sequences that users can bind through inputrc/bindkey, with zero effect on
/// the programs the shell launches. Sub-feature of shell integration: the
/// push/pop only ships when `shell_integration` is also on, and OdyTTY injects
/// `ODYTTY_KEY_ENHANCE=1` into the child environment so the snippet can discover
/// support (mirroring buttons discovery). fish manages the keyboard protocol
/// itself and PowerShell key bindings use the PSReadLine/Console API, so neither
/// is affected. Off by default.
pub const DEFAULT_SHELL_KEY_ENHANCEMENT: bool = false;

/// Remote OSC 133 shell integration for SSH tabs (`ODYTTY_REMOTE_INTEGRATION`):
/// when on, an SSH tab injects OdyTTY's bash prompt-mark bootstrap on the remote
/// so a remote bash session gains the same prompt/input boundaries as a local
/// one (nothing is persisted on the remote; the rcfile self-deletes). On by
/// default because the injection degrades to a plain ssh session on any failure
/// or for non-bash remote shells, so the safe fallback makes default-on safe. A
/// per-host `Integration off` in `hosts.conf` opts a single host out; turning
/// this off globally makes every SSH tab byte-identical to a plain ssh launch.
pub const DEFAULT_REMOTE_INTEGRATION: bool = true;

/// ControlMaster connection reuse for integrated SSH tabs (`ODYTTY_REMOTE_REUSE`):
/// when on, an integrated SSH tab adds `-o ControlMaster=auto -o ControlPersist`
/// with an OdyTTY-owned `ControlPath`, so the first tab to a host establishes a
/// shared master and later tabs multiplex over it with no fresh handshake. On by
/// default; the failure mode (master gone) degrades to a normal fresh connect. A
/// per-host `Reuse off` in `hosts.conf` opts a single host out. OpenSSH for
/// Windows has no socket multiplexing, so reuse is a silent no-op on a Windows
/// client (the control options are never emitted).
pub const DEFAULT_REMOTE_REUSE: bool = true;

/// tmux persistence for integrated SSH tabs (`ODYTTY_REMOTE_TMUX`): when on, an
/// integrated SSH tab's bootstrap `exec`s `tmux new-session -A -s odytty` so a
/// dropped-and-reconnected link reattaches the same remote session with its
/// state intact. Opt-in (default off); the remote shell degrades to plain bash
/// when the remote has no `tmux`, so enabling it never yields a broken session.
/// A per-host `Tmux on`/`Tmux off` in `hosts.conf` overrides the default for a
/// single host. Only meaningful with remote integration enabled.
pub const DEFAULT_REMOTE_TMUX: bool = false;

/// Ceiling on the PNG-encoded size of a clipboard image uploaded through
/// image paste-through (F6-i7). A pasted image whose encoded size exceeds this
/// is refused with a one-line notice rather than uploaded, so a pathological
/// clipboard can never ship tens of megabytes over the link or fill the remote
/// `/tmp`. Fixed at 10 MiB for now; the confirm prompt shows the encoded size so
/// an over-cap paste is obvious.
pub const REMOTE_IMAGE_PASTE_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Restore the previous workspace/tab/pane SHAPE at launch
/// (`ODYTTY_RESTORE_WORKSPACES`): when on, a bare `odytty` launch reopens the
/// last saved layout — workspace names, tab titles/order, and each pane's
/// split tree at its captured cwd — landing a fresh interactive shell in each
/// pane. Never restores grid content, scrollback, or commands. Off by default;
/// any CLI argument suppresses restore for that launch. The shape autosave that
/// feeds this runs regardless of the setting, so a snapshot is ready the moment
/// it is turned on.
pub const DEFAULT_RESTORE_WORKSPACES: bool = false;

pub const DEFAULT_BLOOM: bool = true;
pub const DEFAULT_BLOOM_THRESHOLD: f32 = 0.7;
pub const BLOOM_THRESHOLD_MARGIN: f32 = 0.12;
pub const MIN_BLOOM_THRESHOLD: f32 = 0.70;
pub const MAX_BLOOM_THRESHOLD: f32 = 1.25;
pub const DEFAULT_BLOOM_INTENSITY: f32 = 0.7;
pub const MIN_BLOOM_INTENSITY: f32 = 0.0;
pub const MAX_BLOOM_INTENSITY: f32 = 1.0;
pub const DEFAULT_BLOOM_RADIUS: f32 = 8.0;
pub const MIN_BLOOM_RADIUS: f32 = 0.5;
pub const MAX_BLOOM_RADIUS: f32 = 8.0;
pub const DEFAULT_RETRO: bool = false;
pub const RETRO_BLOOM_THRESHOLD: f32 = 0.70;
pub const RETRO_BLOOM_INTENSITY: f32 = 1.0;
pub const RETRO_BLOOM_RADIUS: f32 = 8.0;
pub const RETRO_CRT_SCANLINE_INTENSITY: f32 = 0.35;
pub const RETRO_CRT_VIGNETTE_STRENGTH: f32 = 0.35;
pub const RETRO_CRT_CURVATURE: f32 = 0.025;

pub fn default_bloom_threshold_for_theme(theme: Theme) -> f32 {
    (crate::theme::relative_luminance(theme.foreground) as f32 + BLOOM_THRESHOLD_MARGIN)
        .clamp(MIN_BLOOM_THRESHOLD, MAX_BLOOM_THRESHOLD)
}

pub const DEFAULT_CRT: bool = true;
pub const DEFAULT_CRT_SCANLINE_INTENSITY: f32 = 0.17;
pub const MIN_CRT_SCANLINE_INTENSITY: f32 = 0.0;
pub const MAX_CRT_SCANLINE_INTENSITY: f32 = 0.35;
pub const DEFAULT_CRT_SCANLINE_PERIOD: f32 = 7.0;
pub const MIN_CRT_SCANLINE_PERIOD: f32 = 2.0;
pub const MAX_CRT_SCANLINE_PERIOD: f32 = 12.0;
pub const DEFAULT_CRT_VIGNETTE_STRENGTH: f32 = 0.45;
pub const MIN_CRT_VIGNETTE_STRENGTH: f32 = 0.0;
pub const MAX_CRT_VIGNETTE_STRENGTH: f32 = 0.45;
pub const DEFAULT_CRT_CURVATURE: f32 = 0.0;
pub const MIN_CRT_CURVATURE: f32 = 0.0;
pub const MAX_CRT_CURVATURE: f32 = 0.12;

/// Colour-vision-deficiency adaptation strength (`ODYTTY_CVD_STRENGTH`, U4): how
/// strongly the palette is daltonised toward separability for the selected
/// [`crate::settings::CvdMode`]. `1.0` (default) is the full correction; `0.0`
/// is an exact passthrough. Inert while the mode is `off` — the off mode is the
/// primary pixel-identical guarantee, and `0.0` strength is a second net. The
/// adaptation is palette-scope (the 16 ANSI colours plus the cursor/selection/
/// search roles), re-floored to stay readable; app truecolour is not remapped.
pub const DEFAULT_CVD_STRENGTH: f32 = 1.0;
pub const MIN_CVD_STRENGTH: f32 = 0.0;
pub const MAX_CVD_STRENGTH: f32 = 1.0;
