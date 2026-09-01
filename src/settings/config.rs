// SPDX-License-Identifier: GPL-3.0-only
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::Path;

use super::{
    ALWAYS_SHOW_TAB_BAR_ENV, BACKGROUND_BLUR_RADIUS_ENV, BACKGROUND_IMAGE_ENV,
    BACKGROUND_IMAGE_SCRIM_ENV, BACKGROUND_TREATMENT_ENV, BELL_ENV, BLOOM_ENV, BLOOM_INTENSITY_ENV,
    BLOOM_RADIUS_ENV, BLOOM_THRESHOLD_ENV, BUTTONS_ENV, BUTTONS_ITERM_COMPAT_ENV,
    BUTTONS_STICKY_ENV, CELL_BG_OPACITY_ENV, COLORED_BG_OPACITY_ENV, COMMAND_STATUS_GUTTER_ENV,
    CONFIRM_CLOSE_ENV, COPY_ON_SELECT_ENV, CRT_CURVATURE_ENV, CRT_ENV, CRT_SCANLINE_INTENSITY_ENV,
    CRT_SCANLINE_PERIOD_ENV, CRT_VIGNETTE_STRENGTH_ENV, CURSOR_BLINK_ENV, CURSOR_EASING_ENV,
    CURSOR_GLOW_ENV, CURSOR_GLOW_INTENSITY_ENV, CURSOR_MOTION_ENV, CURSOR_STYLE_ENV,
    CURSOR_TRAIL_ENV, CURSOR_TRAIL_STRENGTH_ENV, CVD_MODE_ENV, CVD_STRENGTH_ENV,
    EXTERNAL_PALETTE_PATH_ENV, EXTERNAL_PALETTE_PROVIDER_ENV, FOCUS_DIM_ENV,
    FOLLOW_EXTERNAL_PALETTE_ENV, FOLLOW_OS_THEME_ENV, FONT_ENV, FONT_FAMILY_ENV, FONT_SIZE_ENV,
    FONT_WEIGHT_ENV, GEOMETRIC_BOXDRAW_ENV, INACTIVE_PANE_DIM_ENV, KEYBINDS_ENV, MIN_CONTRAST_ENV,
    NATIVE_AUTOCLOSE_ENV, NEW_OUTPUT_FADE_ENV, NEW_OUTPUT_FADE_MS_ENV, NOTIFICATIONS_ENV,
    OS_THEME_DARK_ENV, OS_THEME_LIGHT_ENV, OSC52_READ_ENV, OSC52_WRITE_ENV, PANE_PREFIX_ENV,
    PIXEL_SCROLL_ENV, PROFILE_AUTO_SWITCH_ENV, REDUCED_MOTION_ENV, RENDER_QUALITY_ENV,
    RESTORE_WORKSPACES_ENV, RETRO_ENV, SCROLL_DRAG_SPEED_ENV, SCROLL_GLIDE_ENV,
    SCROLL_PIXEL_SPEED_ENV, SCROLL_WHEEL_LINES_ENV, SCROLLBACK_LINES_ENV, SCROLLBAR_DRAG_ENV,
    SELECTION_DRAG_EXTEND_ENV, SELECTION_OPACITY_ENV, SH_CLICK_ENV, SHELL_EXIT_CLOSES_ENV,
    SHELL_INTEGRATION_ENV, SHELL_KEY_ENHANCEMENT_ENV, SMART_CTRL_C_ENV, STEM_DARKEN_ENV,
    SUBPIXEL_ENV, SYMBOL_FALLBACK_ENV, SYMBOL_FONT_ENV, SYMBOL_MAP_ENV, SYNTHETIC_STYLES_ENV,
    TAB_BAR_HEIGHT_ENV, TAB_BAR_PLACEMENT_ENV, TAB_PANEL_STRENGTH_ENV, TAB_RAIL_AUTOHIDE_ENV,
    TAB_RAIL_GAP_ENV, TAB_RAIL_MAX_WIDTH_ENV, TAB_RAIL_REVEAL_PX_ENV, TAB_RAIL_SLOT_ROWS_ENV,
    TAB_RAIL_WIDTH_ENV, TAB_SEAM_ENV, TEXT_BRIGHTNESS_ENV, TEXT_GAMMA_ENV, THEME_ENV,
    THEMED_UI_ROLES_ENV, VISUAL_ENV, WHEEL_ZOOM_ENV, WINDOW_BORDER_ENV, WINDOW_DECORATIONS_ENV,
    WINDOW_OPACITY_ENV, WINDOW_PADDING_ENV, WINDOW_TRANSPARENCY_ENV, WORKSPACE_RAIL_AUTOHIDE_ENV,
    WORKSPACE_RAIL_ENV, WORKSPACE_RAIL_GAP_ENV, WORKSPACE_RAIL_MAX_WIDTH_ENV,
    WORKSPACE_RAIL_REVEAL_PX_ENV, WORKSPACE_RAIL_SIDE_ENV, WORKSPACE_RAIL_SLOT_ROWS_ENV,
    WORKSPACE_RAIL_WIDTH_ENV, normalize_name,
};
use super::{
    BOX_THICKNESS_ENV, INTERACTIVE_PATHS_BAREWORDS_ENV, INTERACTIVE_PATHS_CLICK_HINT_ENV,
    INTERACTIVE_PATHS_EDITOR_ENV, INTERACTIVE_PATHS_ENV, INTERACTIVE_PATHS_IMAGE_INLINE_ENV,
    INTERACTIVE_URLS_ENV, KITTY_NAMED_TRANSPORTS_ENV, LIGATURE_SS01_ENV, LIGATURE_SS02_ENV,
    LIGATURES_ENV, LINE_HEIGHT_ENV, REMOTE_IMAGE_PASTE_ENV, REMOTE_INTEGRATION_ENV,
    REMOTE_PERSIST_ENV, REMOTE_REUSE_ENV, REMOTE_TMUX_ENV, SESSION_REPLAY_ENV,
    SSH_CONFIG_HOSTS_ENV, WARN_ON_RISKY_PASTE_ENV,
};
#[derive(Debug, Clone, Default)]
pub(crate) struct ConfigValues {
    values: HashMap<&'static str, OsString>,
}

impl ConfigValues {
    pub(super) fn read(path: &Path, mut warn: impl FnMut(String)) -> io::Result<Self> {
        let contents = super::fs_read::read_capped(path)?;
        Ok(Self::parse(&contents, |message| {
            warn(format!("{}: {message}", path.display()));
        }))
    }

    pub(crate) fn parse(contents: &str, mut warn: impl FnMut(String)) -> Self {
        let mut values = HashMap::new();
        for (line_index, line) in contents.lines().enumerate() {
            let line_number = line_index + 1;
            let trimmed = strip_inline_comment(line).trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((key_raw, value_raw)) = trimmed.split_once('=') else {
                warn(format!(
                    "line {line_number}: expected key = value; skipping"
                ));
                continue;
            };
            let key = key_raw.trim();
            if key.is_empty() {
                warn(format!("line {line_number}: empty key; skipping"));
                continue;
            }
            let Some(env_key) = config_key_to_env(key) else {
                warn(format!("line {line_number}: unknown key {key:?}; skipping"));
                continue;
            };
            values.insert(env_key, OsString::from(unquote_config_value(value_raw)));
        }
        Self { values }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&OsString> {
        self.values.get(key)
    }
}

/// Split a config line at its inline `#` comment, honoring double-quoted values
/// so a `#` inside quotes is data, not a comment. Returns the slice before the
/// comment (quotes intact). A line with no unquoted `#` is returned whole, so an
/// ordinary line is byte-identical to the old `split_once('#')`.
pub(super) fn strip_inline_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..idx],
            _ => {}
        }
    }
    line
}

/// Read side of [`quote_config_value`]: a value wrapped in double quotes has the
/// quotes removed and `\\`/`\"` unescaped; any other value is returned trimmed.
/// An unquoted value is byte-identical to the old `value.trim()`.
pub(super) fn unquote_config_value(value: &str) -> String {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                out.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        trimmed.to_owned()
    }
}

/// Quote a config value for writing when writing it raw would break the
/// line-oriented format on read-back: it contains a `#` (comment start) or a
/// `"`, or it has leading/trailing whitespace (trimmed away on read) or is
/// empty. Newlines are stripped outright -- a value may never span lines. A
/// value needing none of this is returned verbatim, so ordinary settings write
/// exactly as before and round-trip byte-identically.
pub(super) fn quote_config_value(value: &str) -> String {
    let sanitized: String = value.chars().filter(|&c| c != '\n' && c != '\r').collect();
    let needs_quote = sanitized.is_empty()
        || sanitized.contains('#')
        || sanitized.contains('"')
        || sanitized.trim() != sanitized;
    if !needs_quote {
        return sanitized;
    }
    let mut out = String::with_capacity(sanitized.len() + 2);
    out.push('"');
    for ch in sanitized.chars() {
        if ch == '\\' || ch == '"' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

pub(super) fn config_key_to_env(key: &str) -> Option<&'static str> {
    match normalize_name(key).as_str() {
        "theme" => Some(THEME_ENV),
        "followostheme" | "followsystemtheme" | "ostheme" | "autotheme" => {
            Some(FOLLOW_OS_THEME_ENV)
        }
        "followexternalpalette" | "externalpalette" | "followpalette" => {
            Some(FOLLOW_EXTERNAL_PALETTE_ENV)
        }
        "externalpaletteprovider" | "paletteprovider" => Some(EXTERNAL_PALETTE_PROVIDER_ENV),
        "externalpalettepath" | "palettepath" => Some(EXTERNAL_PALETTE_PATH_ENV),
        "osthemedark" | "darktheme" | "themedark" => Some(OS_THEME_DARK_ENV),
        "osthemelight" | "lighttheme" | "themelight" => Some(OS_THEME_LIGHT_ENV),
        "visual" => Some(VISUAL_ENV),
        "bloom" => Some(BLOOM_ENV),
        "bloomthreshold" => Some(BLOOM_THRESHOLD_ENV),
        "bloomintensity" => Some(BLOOM_INTENSITY_ENV),
        "bloomradius" => Some(BLOOM_RADIUS_ENV),
        "retro" | "retropreset" | "phosphor" => Some(RETRO_ENV),
        "crt" => Some(CRT_ENV),
        "crtscanlineintensity" | "crtscanlines" => Some(CRT_SCANLINE_INTENSITY_ENV),
        "crtscanlineperiod" | "crtscanlinedensity" => Some(CRT_SCANLINE_PERIOD_ENV),
        "crtvignettestrength" | "crtvignette" => Some(CRT_VIGNETTE_STRENGTH_ENV),
        "crtcurvature" | "curvature" => Some(CRT_CURVATURE_ENV),
        "font" => Some(FONT_ENV),
        "fontfamily" => Some(FONT_FAMILY_ENV),
        "fontweight" | "weight" | "fontweightvariant" => Some(FONT_WEIGHT_ENV),
        "fontsize" => Some(FONT_SIZE_ENV),
        "textgamma" => Some(TEXT_GAMMA_ENV),
        "stemdarken" => Some(STEM_DARKEN_ENV),
        "mincontrast" => Some(MIN_CONTRAST_ENV),
        "focusdim" | "unfocuseddim" => Some(FOCUS_DIM_ENV),
        "inactivepanedim" | "panedim" | "inactivedim" => Some(INACTIVE_PANE_DIM_ENV),
        "renderquality" | "quality" | "rendermode" => Some(RENDER_QUALITY_ENV),
        "backgroundtreatment" | "background" | "bgtreatment" | "bg" => {
            Some(BACKGROUND_TREATMENT_ENV)
        }
        "backgroundimage" | "bgimage" | "backgroundimagepath" | "wallpaper" => {
            Some(BACKGROUND_IMAGE_ENV)
        }
        "backgroundblurradius" | "backgroundblur" | "bgblur" | "blurradius" => {
            Some(BACKGROUND_BLUR_RADIUS_ENV)
        }
        "backgroundimagescrim" | "bgscrim" | "imagescrim" | "scrim" => {
            Some(BACKGROUND_IMAGE_SCRIM_ENV)
        }
        "cellbgopacity" | "cellbackgroundopacity" | "cellopacity" | "bgopacity" => {
            Some(CELL_BG_OPACITY_ENV)
        }
        "coloredbgopacity" | "coloredbackgroundopacity" | "coloredopacity" | "keepbgcolors" => {
            Some(COLORED_BG_OPACITY_ENV)
        }
        "selectionopacity" | "selectionalpha" | "selopacity" | "highlightopacity" => {
            Some(SELECTION_OPACITY_ENV)
        }
        "textbrightness" | "textlift" | "brightness" => Some(TEXT_BRIGHTNESS_ENV),
        "windowpadding" | "padding" | "windowpaddingpx" => Some(WINDOW_PADDING_ENV),
        "geometricboxdraw" | "boxdraw" => Some(GEOMETRIC_BOXDRAW_ENV),
        "symbolfallback" | "symbols" | "nerdfont" => Some(SYMBOL_FALLBACK_ENV),
        "symbolfont" | "nerdfontpath" | "symbolfontpath" => Some(SYMBOL_FONT_ENV),
        "symbolmap" | "symbolmaps" | "codepointmap" => Some(SYMBOL_MAP_ENV),
        "themeduiroles" | "themedroles" | "uiroles" => Some(THEMED_UI_ROLES_ENV),
        "subpixel" => Some(SUBPIXEL_ENV),
        "lineheight" | "lineleading" | "cellleading" => Some(LINE_HEIGHT_ENV),
        "boxthickness" | "boxweight" | "boxstroke" => Some(BOX_THICKNESS_ENV),
        "keybinds" | "keybindings" => Some(KEYBINDS_ENV),
        "paneprefix" | "prefix" | "multiplexerprefix" => Some(PANE_PREFIX_ENV),
        "cursorstyle" => Some(CURSOR_STYLE_ENV),
        "cursorblink" => Some(CURSOR_BLINK_ENV),
        "cursoreasing" | "cursorfade" | "cursorblinkfade" => Some(CURSOR_EASING_ENV),
        "cursorglow" | "cursorhalo" | "cursorbloom" => Some(CURSOR_GLOW_ENV),
        "cursorglowintensity" | "cursorglowstrength" | "cursorglowamount" => {
            Some(CURSOR_GLOW_INTENSITY_ENV)
        }
        "cursortrail" | "cursortrails" | "cursorghost" | "cursorafterimage" => {
            Some(CURSOR_TRAIL_ENV)
        }
        "cursortrailstrength" | "trailstrength" | "cursorstreakstrength" => {
            Some(CURSOR_TRAIL_STRENGTH_ENV)
        }
        "reducedmotion" | "reducemotion" | "nomotion" => Some(REDUCED_MOTION_ENV),
        "newoutputfade" | "outputfade" | "fadein" | "newlinefade" => Some(NEW_OUTPUT_FADE_ENV),
        "newoutputfadems" | "outputfadems" | "fadeinms" | "newoutputfadeduration" => {
            Some(NEW_OUTPUT_FADE_MS_ENV)
        }
        "windowborder" | "border" | "themedborder" | "windowframe" => Some(WINDOW_BORDER_ENV),
        "windowdecorations" | "decorations" | "titlebar" | "borderless" => {
            Some(WINDOW_DECORATIONS_ENV)
        }
        "windowtransparency" | "transparency" | "transparent" | "windowalpha" => {
            Some(WINDOW_TRANSPARENCY_ENV)
        }
        "windowopacity" | "opacity" => Some(WINDOW_OPACITY_ENV),
        "cursormotion" | "cursorslide" | "cursoranimation" => Some(CURSOR_MOTION_ENV),
        "osc52read" | "allowosc52read" | "clipboardread" => Some(OSC52_READ_ENV),
        "osc52write" | "allowosc52write" | "clipboardwrite" => Some(OSC52_WRITE_ENV),
        "syntheticstyles" | "synthstyles" | "syntheticfonts" => Some(SYNTHETIC_STYLES_ENV),
        "ligatures" | "programmingligatures" | "calt" => Some(LIGATURES_ENV),
        "ligaturesss01" | "ss01" | "stylisticset01" => Some(LIGATURE_SS01_ENV),
        "ligaturesss02" | "ss02" | "stylisticset02" => Some(LIGATURE_SS02_ENV),
        "kittynamedtransports" | "kittyfiletransports" | "kittysharedmemory" => {
            Some(KITTY_NAMED_TRANSPORTS_ENV)
        }
        "scrollwheellines" | "wheellines" | "scrollspeed" | "scrollwheelspeed" => {
            Some(SCROLL_WHEEL_LINES_ENV)
        }
        "scrollbacklines" | "scrollback" | "scrollbacklimit" | "historylines" => {
            Some(SCROLLBACK_LINES_ENV)
        }
        "scrolldragspeed" | "dragscrollspeed" | "autoscrollspeed" | "dragautoscroll" => {
            Some(SCROLL_DRAG_SPEED_ENV)
        }
        "pixelscroll" | "continuousscroll" | "precisescroll" => Some(PIXEL_SCROLL_ENV),
        "scrollpixelspeed" | "pixelscrollspeed" | "touchpadspeed" => Some(SCROLL_PIXEL_SPEED_ENV),
        "scrollglide" | "glidescroll" => Some(SCROLL_GLIDE_ENV),
        "copyonselect" | "selecttoclipboard" => Some(COPY_ON_SELECT_ENV),
        "smartctrlc" | "ctrlccopy" | "copyorinterrupt" | "smartcopy" => Some(SMART_CTRL_C_ENV),
        "selectiondragextend" | "dragextend" | "dragextendselection" => {
            Some(SELECTION_DRAG_EXTEND_ENV)
        }
        "scrollbardrag" | "draggablescrollbar" | "scrollthumbdrag" => Some(SCROLLBAR_DRAG_ENV),
        "wheelzoom" | "ctrlwheelzoom" | "fontzoom" => Some(WHEEL_ZOOM_ENV),
        "commandstatusgutter" | "statusgutter" | "commandgutter" => Some(COMMAND_STATUS_GUTTER_ENV),
        "alwaysshowtabbar" | "showtabbar" | "tabbaralwaysvisible" | "persistenttabbar" => {
            Some(ALWAYS_SHOW_TAB_BAR_ENV)
        }
        "tabbarplacement" | "tabbarside" | "tabbarposition" | "tabplacement" => {
            Some(TAB_BAR_PLACEMENT_ENV)
        }
        "workspacerail" | "workspacesidebar" | "workspacebar" | "railmode" => {
            Some(WORKSPACE_RAIL_ENV)
        }
        "workspacerailside" | "railside" => Some(WORKSPACE_RAIL_SIDE_ENV),
        "tabbarheight" | "barheight" | "tabheight" => Some(TAB_BAR_HEIGHT_ENV),
        // Canonical workspace-rail names (the rail shows workspaces). Each maps to
        // its own env alias; the read path prefers it over the legacy TAB_RAIL_*
        // twin when both are set.
        "workspacerailwidth" => Some(WORKSPACE_RAIL_WIDTH_ENV),
        "workspacerailmaxwidth" => Some(WORKSPACE_RAIL_MAX_WIDTH_ENV),
        "workspacerailgap" => Some(WORKSPACE_RAIL_GAP_ENV),
        "workspacerailslotrows" => Some(WORKSPACE_RAIL_SLOT_ROWS_ENV),
        "workspacerailautohide" => Some(WORKSPACE_RAIL_AUTOHIDE_ENV),
        "workspacerailrevealpx" => Some(WORKSPACE_RAIL_REVEAL_PX_ENV),
        "tabrailwidth" | "railwidth" => Some(TAB_RAIL_WIDTH_ENV),
        "tabrailmaxwidth" | "railmaxwidth" | "maxrailwidth" => Some(TAB_RAIL_MAX_WIDTH_ENV),
        "tabrailgap" | "railgap" | "slotgap" => Some(TAB_RAIL_GAP_ENV),
        "tabrailslotrows" | "railslotrows" | "slotrows" | "slotheight" => {
            Some(TAB_RAIL_SLOT_ROWS_ENV)
        }
        "tabpanelstrength" | "panelstrength" | "tabpanel" => Some(TAB_PANEL_STRENGTH_ENV),
        "tabseam" | "panelseam" | "seam" => Some(TAB_SEAM_ENV),
        "tabrailautohide" | "railautohide" | "autohiderail" => Some(TAB_RAIL_AUTOHIDE_ENV),
        "tabrailrevealpx" | "railrevealpx" | "revealpx" | "revealzone" => {
            Some(TAB_RAIL_REVEAL_PX_ENV)
        }
        "shclick" | "clicktoposition" | "clicktomovecursor" | "promptclick" => Some(SH_CLICK_ENV),
        "buttons" | "buttonprotocol" | "clickablebuttons" => Some(BUTTONS_ENV),
        "buttonsitermcompat" | "itermbuttons" | "buttonscompat" => Some(BUTTONS_ITERM_COMPAT_ENV),
        "buttonssticky" | "stickybuttons" => Some(BUTTONS_STICKY_ENV),
        "shellintegration" | "promptmarks" | "osc133" | "osc133integration" => {
            Some(SHELL_INTEGRATION_ENV)
        }
        "shellkeyenhancement" | "keyenhancement" | "promptkeyenhancement" | "keyprotocol" => {
            Some(SHELL_KEY_ENHANCEMENT_ENV)
        }
        "restoreworkspaces" | "restoresession" | "restoretabs" | "restorelayout" | "restore" => {
            Some(RESTORE_WORKSPACES_ENV)
        }
        "profileautoswitch" | "autoswitchprofile" | "profileswitch" | "profile_switch" => {
            Some(PROFILE_AUTO_SWITCH_ENV)
        }
        "cvdmode" | "colorblindmode" | "colourblindmode" | "daltonize" => Some(CVD_MODE_ENV),
        "cvdstrength" | "colorblindstrength" | "colourblindstrength" => Some(CVD_STRENGTH_ENV),
        "bell" | "bellmode" | "audiblebell" | "visualbell" => Some(BELL_ENV),
        "notifications" | "notificationmode" | "commandnotifications" => Some(NOTIFICATIONS_ENV),
        "confirmclose" | "closeconfirm" | "closeconfirmation" | "confirmonclose" => {
            Some(CONFIRM_CLOSE_ENV)
        }
        "warnonriskypaste" | "riskypastewarning" | "pastewarning" => Some(WARN_ON_RISKY_PASTE_ENV),
        "shellexitcloses" | "exitcloses" | "typingexitcloses" | "exitbehavior" => {
            Some(SHELL_EXIT_CLOSES_ENV)
        }
        "sshconfighosts" | "sshconfig" | "opensshhosts" | "sshhosts" => Some(SSH_CONFIG_HOSTS_ENV),
        "remoteintegration" | "sshintegration" | "remoteshellintegration" | "remoteosc133" => {
            Some(REMOTE_INTEGRATION_ENV)
        }
        "remotereuse" | "sshreuse" | "controlmaster" | "sshcontrolmaster" => Some(REMOTE_REUSE_ENV),
        "remotetmux" => Some(REMOTE_TMUX_ENV),
        "remotepersist" | "controlpersist" | "sshpersist" | "persist" => Some(REMOTE_PERSIST_ENV),
        "remoteimagepaste" => Some(REMOTE_IMAGE_PASTE_ENV),
        "sessionreplay" | "replay" | "outputreplay" | "scrollbackreplay" => {
            Some(SESSION_REPLAY_ENV)
        }
        "interactiveurls" | "urls" | "clickableurls" | "urllinks" | "linkify" => {
            Some(INTERACTIVE_URLS_ENV)
        }
        "interactivepaths" | "paths" | "clickablepaths" | "pathlinks" => {
            Some(INTERACTIVE_PATHS_ENV)
        }
        "interactivepathsbarewords" | "pathbarewords" | "barepaths" | "barewordpaths" => {
            Some(INTERACTIVE_PATHS_BAREWORDS_ENV)
        }
        "interactivepathsclickhint" | "pathclickhint" | "clickhint" | "pathopenhint" => {
            Some(INTERACTIVE_PATHS_CLICK_HINT_ENV)
        }
        "interactivepathsimageinline" | "pathimageinline" | "imageinline" | "inlineimages" => {
            Some(INTERACTIVE_PATHS_IMAGE_INLINE_ENV)
        }
        "interactivepathseditor" | "pathseditor" | "patheditor" | "pathopeneditor" => {
            Some(INTERACTIVE_PATHS_EDITOR_ENV)
        }
        "nativeautoclosems" => Some(NATIVE_AUTOCLOSE_ENV),
        _ => None,
    }
}

pub(crate) fn env_to_config_key(env: &str) -> Option<&'static str> {
    match env {
        THEME_ENV => Some("theme"),
        FOLLOW_OS_THEME_ENV => Some("follow_os_theme"),
        FOLLOW_EXTERNAL_PALETTE_ENV => Some("follow_external_palette"),
        EXTERNAL_PALETTE_PROVIDER_ENV => Some("external_palette_provider"),
        EXTERNAL_PALETTE_PATH_ENV => Some("external_palette_path"),
        OS_THEME_DARK_ENV => Some("os_theme_dark"),
        OS_THEME_LIGHT_ENV => Some("os_theme_light"),
        VISUAL_ENV => Some("visual"),
        BLOOM_ENV => Some("bloom"),
        BLOOM_THRESHOLD_ENV => Some("bloom_threshold"),
        BLOOM_INTENSITY_ENV => Some("bloom_intensity"),
        BLOOM_RADIUS_ENV => Some("bloom_radius"),
        RETRO_ENV => Some("retro"),
        CRT_ENV => Some("crt"),
        CRT_SCANLINE_INTENSITY_ENV => Some("crt_scanline_intensity"),
        CRT_SCANLINE_PERIOD_ENV => Some("crt_scanline_period"),
        CRT_VIGNETTE_STRENGTH_ENV => Some("crt_vignette_strength"),
        CRT_CURVATURE_ENV => Some("crt_curvature"),
        FONT_ENV => Some("font"),
        FONT_FAMILY_ENV => Some("font_family"),
        FONT_WEIGHT_ENV => Some("font_weight"),
        FONT_SIZE_ENV => Some("font_size"),
        TEXT_GAMMA_ENV => Some("text_gamma"),
        STEM_DARKEN_ENV => Some("stem_darken"),
        MIN_CONTRAST_ENV => Some("min_contrast"),
        FOCUS_DIM_ENV => Some("focus_dim"),
        INACTIVE_PANE_DIM_ENV => Some("inactive_pane_dim"),
        RENDER_QUALITY_ENV => Some("render_quality"),
        BACKGROUND_TREATMENT_ENV => Some("background_treatment"),
        BACKGROUND_IMAGE_ENV => Some("background_image"),
        BACKGROUND_BLUR_RADIUS_ENV => Some("background_blur_radius"),
        BACKGROUND_IMAGE_SCRIM_ENV => Some("background_image_scrim"),
        CELL_BG_OPACITY_ENV => Some("cell_bg_opacity"),
        COLORED_BG_OPACITY_ENV => Some("colored_bg_opacity"),
        SELECTION_OPACITY_ENV => Some("selection_opacity"),
        TEXT_BRIGHTNESS_ENV => Some("text_brightness"),
        WINDOW_PADDING_ENV => Some("window_padding"),
        GEOMETRIC_BOXDRAW_ENV => Some("geometric_boxdraw"),
        SYMBOL_FALLBACK_ENV => Some("symbol_fallback"),
        SYMBOL_FONT_ENV => Some("symbol_font"),
        SYMBOL_MAP_ENV => Some("symbol_map"),
        THEMED_UI_ROLES_ENV => Some("themed_ui_roles"),
        SUBPIXEL_ENV => Some("subpixel"),
        LINE_HEIGHT_ENV => Some("line_height"),
        BOX_THICKNESS_ENV => Some("box_thickness"),
        LIGATURES_ENV => Some("ligatures"),
        LIGATURE_SS01_ENV => Some("ss01"),
        LIGATURE_SS02_ENV => Some("ss02"),
        KITTY_NAMED_TRANSPORTS_ENV => Some("kitty_named_transports"),
        KEYBINDS_ENV => Some("keybinds"),
        PANE_PREFIX_ENV => Some("pane_prefix"),
        CURSOR_STYLE_ENV => Some("cursor_style"),
        CURSOR_BLINK_ENV => Some("cursor_blink"),
        CURSOR_EASING_ENV => Some("cursor_easing"),
        CURSOR_GLOW_ENV => Some("cursor_glow"),
        CURSOR_GLOW_INTENSITY_ENV => Some("cursor_glow_intensity"),
        CURSOR_TRAIL_ENV => Some("cursor_trail"),
        CURSOR_TRAIL_STRENGTH_ENV => Some("cursor_trail_strength"),
        CURSOR_MOTION_ENV => Some("cursor_motion"),
        REDUCED_MOTION_ENV => Some("reduced_motion"),
        OSC52_READ_ENV => Some("osc52_read"),
        OSC52_WRITE_ENV => Some("osc52_write"),
        SYNTHETIC_STYLES_ENV => Some("synthetic_styles"),
        SCROLL_WHEEL_LINES_ENV => Some("scroll_wheel_lines"),
        SCROLLBACK_LINES_ENV => Some("scrollback_lines"),
        SCROLL_DRAG_SPEED_ENV => Some("scroll_drag_speed"),
        COPY_ON_SELECT_ENV => Some("copy_on_select"),
        SMART_CTRL_C_ENV => Some("smart_ctrl_c"),
        SELECTION_DRAG_EXTEND_ENV => Some("selection_drag_extend"),
        SCROLLBAR_DRAG_ENV => Some("scrollbar_drag"),
        WHEEL_ZOOM_ENV => Some("wheel_zoom"),
        COMMAND_STATUS_GUTTER_ENV => Some("command_status_gutter"),
        ALWAYS_SHOW_TAB_BAR_ENV => Some("always_show_tab_bar"),
        TAB_BAR_PLACEMENT_ENV => Some("tab_bar_placement"),
        TAB_BAR_HEIGHT_ENV => Some("tab_bar_height"),
        WORKSPACE_RAIL_ENV => Some("workspace_rail"),
        WORKSPACE_RAIL_SIDE_ENV => Some("workspace_rail_side"),
        TAB_RAIL_WIDTH_ENV => Some("tab_rail_width"),
        TAB_RAIL_MAX_WIDTH_ENV => Some("tab_rail_max_width"),
        TAB_RAIL_GAP_ENV => Some("tab_rail_gap"),
        TAB_RAIL_SLOT_ROWS_ENV => Some("tab_rail_slot_rows"),
        WORKSPACE_RAIL_WIDTH_ENV => Some("workspace_rail_width"),
        WORKSPACE_RAIL_MAX_WIDTH_ENV => Some("workspace_rail_max_width"),
        WORKSPACE_RAIL_GAP_ENV => Some("workspace_rail_gap"),
        WORKSPACE_RAIL_SLOT_ROWS_ENV => Some("workspace_rail_slot_rows"),
        WORKSPACE_RAIL_AUTOHIDE_ENV => Some("workspace_rail_autohide"),
        WORKSPACE_RAIL_REVEAL_PX_ENV => Some("workspace_rail_reveal_px"),
        TAB_PANEL_STRENGTH_ENV => Some("tab_panel_strength"),
        TAB_SEAM_ENV => Some("tab_seam"),
        TAB_RAIL_AUTOHIDE_ENV => Some("tab_rail_autohide"),
        TAB_RAIL_REVEAL_PX_ENV => Some("tab_rail_reveal_px"),
        SH_CLICK_ENV => Some("sh_click"),
        BUTTONS_ENV => Some("buttons"),
        BUTTONS_ITERM_COMPAT_ENV => Some("buttons_iterm_compat"),
        BUTTONS_STICKY_ENV => Some("buttons_sticky"),
        SHELL_INTEGRATION_ENV => Some("shell_integration"),
        SHELL_KEY_ENHANCEMENT_ENV => Some("shell_key_enhancement"),
        RESTORE_WORKSPACES_ENV => Some("restore_workspaces"),
        PROFILE_AUTO_SWITCH_ENV => Some("profile_auto_switch"),
        NEW_OUTPUT_FADE_ENV => Some("new_output_fade"),
        NEW_OUTPUT_FADE_MS_ENV => Some("new_output_fade_ms"),
        WINDOW_BORDER_ENV => Some("window_border"),
        WINDOW_TRANSPARENCY_ENV => Some("window_transparency"),
        WINDOW_OPACITY_ENV => Some("window_opacity"),
        WINDOW_DECORATIONS_ENV => Some("window_decorations"),
        PIXEL_SCROLL_ENV => Some("pixel_scroll"),
        SCROLL_PIXEL_SPEED_ENV => Some("scroll_pixel_speed"),
        SCROLL_GLIDE_ENV => Some("scroll_glide"),
        CVD_MODE_ENV => Some("cvd_mode"),
        CVD_STRENGTH_ENV => Some("cvd_strength"),
        BELL_ENV => Some("bell"),
        NOTIFICATIONS_ENV => Some("notifications"),
        CONFIRM_CLOSE_ENV => Some("confirm_close"),
        WARN_ON_RISKY_PASTE_ENV => Some("warn_on_risky_paste"),
        SHELL_EXIT_CLOSES_ENV => Some("shell_exit_closes"),
        SSH_CONFIG_HOSTS_ENV => Some("ssh_config_hosts"),
        REMOTE_INTEGRATION_ENV => Some("remote_integration"),
        REMOTE_REUSE_ENV => Some("remote_reuse"),
        REMOTE_TMUX_ENV => Some("remote_tmux"),
        REMOTE_PERSIST_ENV => Some("remote_persist"),
        REMOTE_IMAGE_PASTE_ENV => Some("remote_image_paste"),
        SESSION_REPLAY_ENV => Some("session_replay"),
        INTERACTIVE_URLS_ENV => Some("interactive_urls"),
        INTERACTIVE_PATHS_ENV => Some("interactive_paths"),
        INTERACTIVE_PATHS_BAREWORDS_ENV => Some("interactive_paths_barewords"),
        INTERACTIVE_PATHS_CLICK_HINT_ENV => Some("interactive_paths_click_hint"),
        INTERACTIVE_PATHS_IMAGE_INLINE_ENV => Some("interactive_paths_image_inline"),
        INTERACTIVE_PATHS_EDITOR_ENV => Some("interactive_paths_editor"),
        NATIVE_AUTOCLOSE_ENV => Some("native_autoclose_ms"),
        _ => None,
    }
}
