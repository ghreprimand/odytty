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
pub const KEYBINDS_ENV: &str = "ODYTTY_KEYBINDS";
pub const PANE_PREFIX_ENV: &str = "ODYTTY_PANE_PREFIX";
pub const CURSOR_STYLE_ENV: &str = "ODYTTY_CURSOR_STYLE";
pub const CURSOR_BLINK_ENV: &str = "ODYTTY_CURSOR_BLINK";
pub const CURSOR_EASING_ENV: &str = "ODYTTY_CURSOR_EASING";
pub const CURSOR_MOTION_ENV: &str = "ODYTTY_CURSOR_MOTION";
pub const CURSOR_GLOW_ENV: &str = "ODYTTY_CURSOR_GLOW";
pub const CURSOR_TRAIL_ENV: &str = "ODYTTY_CURSOR_TRAIL";
pub const NEW_OUTPUT_FADE_ENV: &str = "ODYTTY_NEW_OUTPUT_FADE";
pub const WINDOW_BORDER_ENV: &str = "ODYTTY_WINDOW_BORDER";
pub const WINDOW_DECORATIONS_ENV: &str = "ODYTTY_WINDOW_DECORATIONS";
pub const OSC52_READ_ENV: &str = "ODYTTY_OSC52_READ";
pub const SYNTHETIC_STYLES_ENV: &str = "ODYTTY_SYNTHETIC_STYLES";
pub const GEOMETRIC_BOXDRAW_ENV: &str = "ODYTTY_GEOMETRIC_BOXDRAW";
pub const SYMBOL_FALLBACK_ENV: &str = "ODYTTY_SYMBOL_FALLBACK";
pub const SYMBOL_FONT_ENV: &str = "ODYTTY_SYMBOL_FONT";
pub const SYMBOL_MAP_ENV: &str = "ODYTTY_SYMBOL_MAP";
pub const THEMED_UI_ROLES_ENV: &str = "ODYTTY_THEMED_UI_ROLES";
pub const SCROLL_WHEEL_LINES_ENV: &str = "ODYTTY_SCROLL_WHEEL_LINES";
pub const SCROLLBACK_LINES_ENV: &str = "ODYTTY_SCROLLBACK_LINES";
pub const SCROLL_DRAG_SPEED_ENV: &str = "ODYTTY_SCROLL_DRAG_SPEED";
pub const SMOOTH_SCROLL_ENV: &str = "ODYTTY_SMOOTH_SCROLL";
pub const COPY_ON_SELECT_ENV: &str = "ODYTTY_COPY_ON_SELECT";
pub const SELECTION_DRAG_EXTEND_ENV: &str = "ODYTTY_SELECTION_DRAG_EXTEND";
pub const SCROLLBAR_DRAG_ENV: &str = "ODYTTY_SCROLLBAR_DRAG";
pub const WHEEL_ZOOM_ENV: &str = "ODYTTY_WHEEL_ZOOM";
pub const COMMAND_STATUS_GUTTER_ENV: &str = "ODYTTY_COMMAND_STATUS_GUTTER";
pub const SH_CLICK_ENV: &str = "ODYTTY_SH_CLICK";
pub const CVD_MODE_ENV: &str = "ODYTTY_CVD_MODE";
pub const CVD_STRENGTH_ENV: &str = "ODYTTY_CVD_STRENGTH";
pub const BELL_ENV: &str = "ODYTTY_BELL";
pub const NATIVE_AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";
pub const FOLLOW_OS_THEME_ENV: &str = "ODYTTY_FOLLOW_OS_THEME";
pub const OS_THEME_DARK_ENV: &str = "ODYTTY_OS_THEME_DARK";
pub const OS_THEME_LIGHT_ENV: &str = "ODYTTY_OS_THEME_LIGHT";
pub const CONFIRM_CLOSE_ENV: &str = "ODYTTY_CONFIRM_CLOSE";
pub const SSH_CONFIG_HOSTS_ENV: &str = "ODYTTY_SSH_CONFIG_HOSTS";
pub const SESSION_REPLAY_ENV: &str = "ODYTTY_SESSION_REPLAY";
pub const INTERACTIVE_PATHS_ENV: &str = "ODYTTY_INTERACTIVE_PATHS";
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
    KEYBINDS_ENV,
    PANE_PREFIX_ENV,
    CURSOR_STYLE_ENV,
    CURSOR_BLINK_ENV,
    CURSOR_EASING_ENV,
    CURSOR_MOTION_ENV,
    CURSOR_GLOW_ENV,
    CURSOR_TRAIL_ENV,
    NEW_OUTPUT_FADE_ENV,
    WINDOW_BORDER_ENV,
    WINDOW_DECORATIONS_ENV,
    OSC52_READ_ENV,
    SYNTHETIC_STYLES_ENV,
    GEOMETRIC_BOXDRAW_ENV,
    SYMBOL_FALLBACK_ENV,
    SYMBOL_FONT_ENV,
    SYMBOL_MAP_ENV,
    THEMED_UI_ROLES_ENV,
    SCROLL_WHEEL_LINES_ENV,
    SCROLLBACK_LINES_ENV,
    SCROLL_DRAG_SPEED_ENV,
    SMOOTH_SCROLL_ENV,
    COPY_ON_SELECT_ENV,
    SELECTION_DRAG_EXTEND_ENV,
    SCROLLBAR_DRAG_ENV,
    WHEEL_ZOOM_ENV,
    SH_CLICK_ENV,
    CVD_MODE_ENV,
    CVD_STRENGTH_ENV,
    BELL_ENV,
    FOLLOW_OS_THEME_ENV,
    OS_THEME_DARK_ENV,
    OS_THEME_LIGHT_ENV,
    CONFIRM_CLOSE_ENV,
    SSH_CONFIG_HOSTS_ENV,
    SESSION_REPLAY_ENV,
    INTERACTIVE_PATHS_ENV,
    INTERACTIVE_PATHS_EDITOR_ENV,
    NATIVE_AUTOCLOSE_ENV,
];

/// Default body-font size on a fresh install. Tuned for the bundled default
/// family (Victor Mono), which reads comfortably a notch smaller than the prior
/// JetBrains-era default.
pub const DEFAULT_FONT_SIZE_PX: f32 = 20.0;
pub const MIN_FONT_SIZE_PX: f32 = 6.0;
pub const MAX_FONT_SIZE_PX: f32 = 72.0;
pub const DEFAULT_THEME: Theme = Theme::ODYSSEY;
pub const DEFAULT_VISUAL: VisualEffect = VisualEffect::Ambient;
pub const DEFAULT_TEXT_GAMMA: f32 = 1.5;
pub const MIN_TEXT_GAMMA: f32 = 0.5;
pub const MAX_TEXT_GAMMA: f32 = 3.0;

/// Stem-darkening strength (`ODYTTY_STEM_DARKEN`): a coverage boost applied at
/// glyph raster time so light-on-dark body text holds weight at small sizes
/// (RV5). `0.0` disables it and is pixel-identical to the pre-feature renderer;
/// `1.0` is the strongest boost. Ships default-on at `0.5` for a visible
/// light-on-dark weight boost. Setting `0.0` is the opt-out and fully restores
/// the classic, pre-feature raster.
pub const DEFAULT_STEM_DARKEN: f32 = 0.5;
pub const MIN_STEM_DARKEN: f32 = 0.0;
pub const MAX_STEM_DARKEN: f32 = 1.0;

/// Minimum fg/bg contrast floor (`ODYTTY_MIN_CONTRAST`): a configurable WCAG
/// contrast ratio that every cell's foreground is lifted to meet, so no app can
/// render illegibly low-contrast text (RV1). The default is an assertive
/// readability floor; `1.0` disables the floor and is pixel-identical to the
/// pre-feature renderer. The lift moves only perceptual lightness, preserving hue.
pub const DEFAULT_MIN_CONTRAST: f32 = 16.0;
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

/// Window padding (`ODYTTY_WINDOW_PADDING`): logical pixels of inset on every
/// window edge before the terminal grid begins. `0.0` restores the historical
/// exact edge-to-edge layout; the non-zero default gives text breathing room.
pub const DEFAULT_WINDOW_PADDING_PX: f32 = 4.0;
pub const MIN_WINDOW_PADDING_PX: f32 = 0.0;
pub const MAX_WINDOW_PADDING_PX: f32 = 64.0;

/// Cell background opacity (`ODYTTY_CELL_BG_OPACITY`): the alpha multiplier on
/// every cell's resolved background colour. `1.0` (default) leaves cells fully
/// opaque so the cell-vertex output is byte-identical to before — the `image`
/// background treatment then shows only in the window padding. Values `< 1.0`
/// make cells translucent so a background image shows through behind text; the
/// RV1 floor stays safe at any opacity via the readability scrim.
pub const DEFAULT_CELL_BG_OPACITY: f32 = 1.0;
pub const MIN_CELL_BG_OPACITY: f32 = 0.0;
pub const MAX_CELL_BG_OPACITY: f32 = 1.0;

/// Background-image CPU box-blur radius (`ODYTTY_BACKGROUND_BLUR_RADIUS`),
/// applied once at load time. `0` (default) leaves the image sharp; the radius
/// is clamped to `MAX_BACKGROUND_BLUR_RADIUS` and skipped for oversized images.
pub const DEFAULT_BACKGROUND_BLUR_RADIUS: u32 = 0;
pub const MAX_BACKGROUND_BLUR_RADIUS: u32 = 256;

/// Bounds for the explicit background-image scrim override
/// (`ODYTTY_BACKGROUND_IMAGE_SCRIM`). When unset the scrim is auto-computed to
/// guarantee the RV1 floor; an explicit value is clamped to this range.
pub const MIN_BACKGROUND_IMAGE_SCRIM: f32 = 0.0;
pub const MAX_BACKGROUND_IMAGE_SCRIM: f32 = 1.0;

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
/// geometric box-drawing is off or when a font supplies the glyphs.
pub const DEFAULT_BOX_THICKNESS: f32 = 1.0;
pub const MIN_BOX_THICKNESS: f32 = 0.5;
pub const MAX_BOX_THICKNESS: f32 = 3.0;

/// Mouse-wheel scroll multiplier (`ODYTTY_SCROLL_WHEEL_LINES`, MOUSE-WHEEL-SPEED):
/// rows of local scrollback advanced per wheel notch. The default `3.0` is
/// byte-identical to the historical hardcoded `WHEEL_STEP_LINES`. Stored as `f32`
/// to ride the shared numeric-setting model (slider / keyboard step / range
/// label); the wheel path rounds it to a `usize >= 1`. Local viewport scroll
/// only — when TUI mouse reporting is active the wheel still reports unchanged,
/// and continuous (touchpad pixel) deltas are never multiplied.
pub const DEFAULT_SCROLL_WHEEL_LINES: f32 = 3.0;
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
/// on/off-switching. Off by default — while off the cursor renders its alpha at
/// a constant `1.0` and the blink off-phase hides the cursor outright, so the
/// render path is byte-identical to before. Purely presentational; never
/// affects cell semantics or the logical cursor position.
pub const DEFAULT_CURSOR_EASING: bool = false;

/// Cursor slide motion (`ODYTTY_CURSOR_MOTION`, VE4): when on, the cursor glides
/// a short sub-cell interpolation between adjacent steady-state positions
/// instead of teleporting. Off by default — while off the cursor sits at its
/// exact cell origin (zero offset) and the render path is byte-identical to
/// before. Discontinuities (first frame, resize/reflow, scrollback, large jump,
/// unfocused) always snap rather than slide. Purely presentational; the logical
/// cursor position is always the destination cell, so selection/clipboard and
/// TUI semantics are unaffected.
pub const DEFAULT_CURSOR_MOTION: bool = false;

/// Soft cursor glow (`ODYTTY_CURSOR_GLOW`, ID1): when on, three concentric
/// semi-transparent halo quads are drawn behind the cursor block in the theme
/// foreground color, giving the cursor a subtle bloom. Off by default — while
/// off no glow quads are emitted and the render path is byte-identical to
/// before. Purely presentational; never affects cell semantics or the logical
/// cursor position. The halo alpha is capped low enough that adjacent-cell text
/// contrast stays within the RV1 floor.
pub const DEFAULT_CURSOR_GLOW: bool = false;

/// Cursor motion trail (`ODYTTY_CURSOR_TRAIL`, VE4): when on, a short fading
/// after-image of decaying ghost quads trails the cursor along its slide path
/// while it glides between cells. Rides the existing cursor-slide animation
/// (`cursor_motion`): the trail only appears while a slide is in flight and
/// fully decays as the slide settles, so it never schedules a wake beyond the
/// slide's own animation window. Off by default — while off no trail quads are
/// emitted and the render path is byte-identical to before. The ghosts are
/// drawn behind the cursor block in the theme cursor color at low alpha, so
/// they never obscure cell content or affect the logical cursor position.
/// Visible only when `cursor_motion` is also on (the slide it trails).
pub const DEFAULT_CURSOR_TRAIL: bool = false;

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

/// Smooth (eased) scrollback animation (`ODYTTY_SMOOTH_SCROLL`, RV4): when on, a
/// scrollback movement glides into place over a short, bounded ease-out instead
/// of jumping instantly. Off by default — while off the viewport snaps to its new
/// row exactly as before and the render path is pixel-identical, scheduling zero
/// extra wakes. The scroll TARGET always updates immediately (no added input
/// latency); only the visual position eases toward it, and the animation is hard
/// capped so it always settles and never schedules a perpetual wake. Programmatic
/// jumps (search navigation, return-to-live, resize, scrollbar-thumb drag) and
/// active drag-autoscroll always snap rather than animate. Purely presentational;
/// never affects which rows are shown or any cell semantics.
pub const DEFAULT_SMOOTH_SCROLL: bool = false;

/// Duration of the RV4 smooth-scroll ease. A short, bounded budget so the
/// animation always settles quickly and never adds perceptible latency. Fixed
/// for now; a future tuning knob can expose it once a measured baseline exists.
pub const SMOOTH_SCROLL_DURATION: Duration = Duration::from_millis(80);

/// Frame cadence for the RV4 smooth-scroll animation (~60 fps). Each in-flight
/// scroll schedules at most a handful of wakes before settling.
pub const SMOOTH_SCROLL_FRAME: Duration = Duration::from_millis(16);

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
/// selection to the click. On by default (operator decision) — it only gives
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

/// Click-to-position-cursor (`ODYTTY_SH_CLICK`, SH-CLICK): when on, a plain left
/// click on the live shell prompt line moves the shell's input cursor to the
/// clicked column by emitting the matching number of Left/Right cursor keys —
/// the click slice of OSC 133 `click_events`, never a shell-input takeover.
/// Off by default, and doubly gated: even when on it acts only when a
/// cooperating shell has advertised `click_events=1` on its prompt, so a
/// non-integrated shell never triggers it. While off the pointer path is
/// byte-identical to today (no bytes emitted), and it never changes a pixel.
pub const DEFAULT_SH_CLICK: bool = false;

pub const DEFAULT_BLOOM: bool = true;
pub const DEFAULT_BLOOM_THRESHOLD: f32 = 0.75;
pub const BLOOM_THRESHOLD_MARGIN: f32 = 0.12;
pub const MIN_BLOOM_THRESHOLD: f32 = 0.70;
pub const MAX_BLOOM_THRESHOLD: f32 = 1.25;
pub const DEFAULT_BLOOM_INTENSITY: f32 = 0.8;
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
pub const DEFAULT_CRT_VIGNETTE_STRENGTH: f32 = 0.10;
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
