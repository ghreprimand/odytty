// SPDX-License-Identifier: GPL-3.0-only
//! Canonical settings serialization for editing and writeback.

use super::*;

impl Settings {
    /// The `theme` value as it should round-trip to config and display in the
    /// panel: the `system` alias token when the OS-following alias is active,
    /// else the raw `theme` config string the user set (a file path or user
    /// theme name, preserved so a file-loaded theme re-reads its file rather
    /// than the non-resolving `"custom"` placeholder), else the resolved
    /// theme's own name. Shared by `to_edit_values` and `setting_info` so the
    /// writeback baseline and the displayed value never diverge (C4).
    pub(crate) fn theme_config_value(&self) -> String {
        if self.theme_is_system {
            crate::settings::SYSTEM_THEME_NAME.to_owned()
        } else if let Some(cfg) = self.theme_config.as_deref() {
            cfg.to_owned()
        } else {
            self.theme.name.to_owned()
        }
    }

    pub(super) fn to_edit_values(&self) -> BTreeMap<&'static str, String> {
        let mut values = BTreeMap::new();
        // When the `system` alias is active, write back the alias token (not the
        // internal fallback theme name) so the config round-trips as `theme =
        // system` and the user's intent is preserved.
        values.insert(THEME_ENV, self.theme_config_value());
        values.insert(VISUAL_ENV, self.visual.as_str().to_owned());
        // The `font` config key reflects the RAW explicit override, never the
        // face resolved from `font_family` (RC4): a family pick must not make the
        // writeback baseline carry a `font` value.
        if let Some(path) = self.explicit_font_path.as_ref() {
            values.insert(FONT_ENV, path.display().to_string());
        }
        if let Some(family) = self.font_family.as_ref() {
            values.insert(FONT_FAMILY_ENV, family.clone());
        }
        if !self.font_weight.is_empty() {
            values.insert(FONT_WEIGHT_ENV, self.font_weight.clone());
        }
        values.insert(FONT_SIZE_ENV, format_float(self.font_size_px));
        values.insert(TEXT_GAMMA_ENV, format_float(self.text_gamma));
        values.insert(STEM_DARKEN_ENV, format_float(self.stem_darken));
        values.insert(MIN_CONTRAST_ENV, format_float(self.min_contrast));
        values.insert(FOCUS_DIM_ENV, format_float(self.focus_dim));
        values.insert(INACTIVE_PANE_DIM_ENV, format_float(self.inactive_pane_dim));
        values.insert(RENDER_QUALITY_ENV, self.render_quality.as_str().to_owned());
        values.insert(
            BACKGROUND_TREATMENT_ENV,
            self.background_treatment.as_str().to_owned(),
        );
        if let Some(path) = self.background_image.as_ref() {
            // Render the bundled-default sentinel as the friendly opt-in token so
            // the written config round-trips and never exposes the raw marker.
            let rendered = if is_bundled_background(path) {
                BUNDLED_BACKGROUND_TOKEN.to_owned()
            } else {
                path.display().to_string()
            };
            values.insert(BACKGROUND_IMAGE_ENV, rendered);
        }
        values.insert(
            BACKGROUND_BLUR_RADIUS_ENV,
            self.background_blur_radius.to_string(),
        );
        if let Some(scrim) = self.background_image_scrim {
            values.insert(BACKGROUND_IMAGE_SCRIM_ENV, format_float(scrim));
        }
        values.insert(CELL_BG_OPACITY_ENV, format_float(self.cell_bg_opacity));
        values.insert(
            COLORED_BG_OPACITY_ENV,
            format_float(self.colored_bg_opacity),
        );
        values.insert(SELECTION_OPACITY_ENV, format_float(self.selection_opacity));
        values.insert(TEXT_BRIGHTNESS_ENV, format_float(self.text_brightness));
        values.insert(WINDOW_PADDING_ENV, format_float(self.window_padding_px));
        values.insert(BLOOM_ENV, bool_display(self.bloom).to_owned());
        values.insert(BLOOM_THRESHOLD_ENV, format_float(self.bloom_threshold));
        values.insert(BLOOM_INTENSITY_ENV, format_float(self.bloom_intensity));
        values.insert(BLOOM_RADIUS_ENV, format_float(self.bloom_radius));
        values.insert(RETRO_ENV, bool_display(self.retro).to_owned());
        values.insert(CRT_ENV, bool_display(self.crt).to_owned());
        values.insert(
            CRT_SCANLINE_INTENSITY_ENV,
            format_float(self.crt_scanline_intensity),
        );
        values.insert(
            CRT_SCANLINE_PERIOD_ENV,
            format_float(self.crt_scanline_period),
        );
        values.insert(
            CRT_VIGNETTE_STRENGTH_ENV,
            format_float(self.crt_vignette_strength),
        );
        values.insert(CRT_CURVATURE_ENV, format_float(self.crt_curvature));
        values.insert(SUBPIXEL_ENV, subpixel_display(self.subpixel).to_owned());
        values.insert(LINE_HEIGHT_ENV, format_float(self.line_height));
        values.insert(BOX_THICKNESS_ENV, format_float(self.box_thickness));
        values.insert(LIGATURES_ENV, bool_display(self.ligatures).to_owned());
        values.insert(
            LIGATURE_SS01_ENV,
            bool_display(self.ligature_ss01).to_owned(),
        );
        values.insert(
            LIGATURE_SS02_ENV,
            bool_display(self.ligature_ss02).to_owned(),
        );
        values.insert(
            KITTY_NAMED_TRANSPORTS_ENV,
            bool_display(self.kitty_named_transports).to_owned(),
        );
        values.insert(KEYBINDS_ENV, key_bindings_edit_value(&self.key_bindings));
        values.insert(PANE_PREFIX_ENV, pane_prefix_display(self.pane_prefix));
        values.insert(
            CURSOR_STYLE_ENV,
            cursor_style_display(self.cursor_style).to_owned(),
        );
        values.insert(CURSOR_BLINK_ENV, self.cursor_blink.as_str().to_owned());
        values.insert(
            CURSOR_EASING_ENV,
            bool_display(self.cursor_easing).to_owned(),
        );
        values.insert(CURSOR_GLOW_ENV, bool_display(self.cursor_glow).to_owned());
        values.insert(
            CURSOR_GLOW_INTENSITY_ENV,
            format_float(self.cursor_glow_intensity),
        );
        values.insert(CURSOR_TRAIL_ENV, bool_display(self.cursor_trail).to_owned());
        values.insert(
            CURSOR_TRAIL_STRENGTH_ENV,
            self.cursor_trail_strength.as_str().to_owned(),
        );
        values.insert(
            CURSOR_MOTION_ENV,
            bool_display(self.cursor_motion).to_owned(),
        );
        values.insert(
            REDUCED_MOTION_ENV,
            bool_display(self.reduced_motion).to_owned(),
        );
        values.insert(OSC52_READ_ENV, bool_display(self.osc52_read).to_owned());
        values.insert(OSC52_WRITE_ENV, self.osc52_write.as_str().to_owned());
        values.insert(
            SYNTHETIC_STYLES_ENV,
            bool_display(self.synthetic_styles).to_owned(),
        );
        values.insert(
            GEOMETRIC_BOXDRAW_ENV,
            bool_display(self.geometric_boxdraw).to_owned(),
        );
        values.insert(
            SYMBOL_FALLBACK_ENV,
            bool_display(self.symbol_fallback).to_owned(),
        );
        if let Some(path) = self.symbol_font.as_ref() {
            values.insert(SYMBOL_FONT_ENV, path.display().to_string());
        }
        if !self.symbol_map.is_empty() {
            values.insert(SYMBOL_MAP_ENV, format_symbol_map(&self.symbol_map));
        }
        values.insert(
            THEMED_UI_ROLES_ENV,
            bool_display(self.themed_ui_roles).to_owned(),
        );
        values.insert(
            SCROLL_WHEEL_LINES_ENV,
            format_float(self.scroll_wheel_lines),
        );
        values.insert(SCROLLBACK_LINES_ENV, format_float(self.scrollback_lines));
        values.insert(
            SCROLL_DRAG_SPEED_ENV,
            self.scroll_drag_speed.as_str().to_owned(),
        );
        values.insert(
            COPY_ON_SELECT_ENV,
            bool_display(self.copy_on_select).to_owned(),
        );
        values.insert(SMART_CTRL_C_ENV, self.smart_ctrl_c.as_str().to_owned());
        values.insert(
            SELECTION_DRAG_EXTEND_ENV,
            bool_display(self.selection_drag_extend).to_owned(),
        );
        values.insert(
            SCROLLBAR_DRAG_ENV,
            bool_display(self.scrollbar_drag).to_owned(),
        );
        values.insert(WHEEL_ZOOM_ENV, bool_display(self.wheel_zoom).to_owned());
        values.insert(
            COMMAND_STATUS_GUTTER_ENV,
            bool_display(self.command_status_gutter).to_owned(),
        );
        values.insert(
            ALWAYS_SHOW_TAB_BAR_ENV,
            bool_display(self.always_show_tab_bar).to_owned(),
        );
        values.insert(
            TAB_BAR_PLACEMENT_ENV,
            self.tab_bar_placement.as_str().to_owned(),
        );
        values.insert(WORKSPACE_RAIL_ENV, self.workspace_rail.as_str().to_owned());
        values.insert(TAB_RAIL_WIDTH_ENV, self.tab_rail_width.as_config_string());
        values.insert(TAB_BAR_HEIGHT_ENV, self.tab_bar_height.as_config_string());
        values.insert(
            TAB_RAIL_MAX_WIDTH_ENV,
            format_float(self.tab_rail_max_width),
        );
        values.insert(TAB_RAIL_GAP_ENV, format_float(self.tab_rail_gap));
        values.insert(
            TAB_RAIL_SLOT_ROWS_ENV,
            format_float(self.tab_rail_slot_rows),
        );
        values.insert(
            TAB_PANEL_STRENGTH_ENV,
            format_float(self.tab_panel_strength),
        );
        values.insert(TAB_SEAM_ENV, bool_display(self.tab_seam).to_owned());
        values.insert(
            TAB_RAIL_AUTOHIDE_ENV,
            bool_display(self.tab_rail_autohide).to_owned(),
        );
        values.insert(
            TAB_RAIL_REVEAL_PX_ENV,
            format_float(self.tab_rail_reveal_px),
        );
        values.insert(SH_CLICK_ENV, bool_display(self.sh_click).to_owned());
        values.insert(BUTTONS_ENV, bool_display(self.buttons).to_owned());
        values.insert(
            BUTTONS_ITERM_COMPAT_ENV,
            bool_display(self.buttons_iterm_compat).to_owned(),
        );
        values.insert(
            BUTTONS_STICKY_ENV,
            bool_display(self.buttons_sticky).to_owned(),
        );
        values.insert(
            SHELL_INTEGRATION_ENV,
            bool_display(self.shell_integration).to_owned(),
        );
        values.insert(
            SHELL_KEY_ENHANCEMENT_ENV,
            bool_display(self.shell_key_enhancement).to_owned(),
        );
        values.insert(
            RESTORE_WORKSPACES_ENV,
            bool_display(self.restore_workspaces).to_owned(),
        );
        values.insert(
            NEW_OUTPUT_FADE_ENV,
            bool_display(self.new_output_fade).to_owned(),
        );
        values.insert(
            NEW_OUTPUT_FADE_MS_ENV,
            format_float(self.new_output_fade_ms),
        );
        values.insert(
            WINDOW_BORDER_ENV,
            bool_display(self.window_border).to_owned(),
        );
        values.insert(
            WINDOW_DECORATIONS_ENV,
            bool_display(self.window_decorations).to_owned(),
        );
        values.insert(
            WINDOW_TRANSPARENCY_ENV,
            bool_display(self.window_transparency).to_owned(),
        );
        values.insert(WINDOW_OPACITY_ENV, format_float(self.window_opacity));
        values.insert(PIXEL_SCROLL_ENV, bool_display(self.pixel_scroll).to_owned());
        values.insert(
            SCROLL_PIXEL_SPEED_ENV,
            format_float(self.scroll_pixel_speed),
        );
        values.insert(SCROLL_GLIDE_ENV, bool_display(self.scroll_glide).to_owned());
        values.insert(CVD_MODE_ENV, self.cvd_mode.as_str().to_owned());
        values.insert(CVD_STRENGTH_ENV, format_float(self.cvd_strength));
        values.insert(BELL_ENV, self.bell.as_str().to_owned());
        values.insert(NOTIFICATIONS_ENV, self.notifications.as_str().to_owned());
        values.insert(
            SHELL_EXIT_CLOSES_ENV,
            self.shell_exit_closes.as_str().to_owned(),
        );
        values.insert(
            FOLLOW_OS_THEME_ENV,
            bool_display(self.follow_os_theme).to_owned(),
        );
        values.insert(
            FOLLOW_EXTERNAL_PALETTE_ENV,
            bool_display(self.follow_external_palette).to_owned(),
        );
        values.insert(
            EXTERNAL_PALETTE_PROVIDER_ENV,
            self.external_palette_provider.as_str().to_owned(),
        );
        if let Some(path) = self.external_palette_path.as_ref() {
            values.insert(EXTERNAL_PALETTE_PATH_ENV, path.clone());
        }
        if let Some(name) = self.os_theme_dark.as_ref() {
            values.insert(OS_THEME_DARK_ENV, name.clone());
        }
        if let Some(name) = self.os_theme_light.as_ref() {
            values.insert(OS_THEME_LIGHT_ENV, name.clone());
        }
        values.insert(
            CONFIRM_CLOSE_ENV,
            bool_display(self.confirm_close).to_owned(),
        );
        values.insert(
            WARN_ON_RISKY_PASTE_ENV,
            bool_display(self.warn_on_risky_paste).to_owned(),
        );
        values.insert(
            SSH_CONFIG_HOSTS_ENV,
            bool_display(self.ssh_config_hosts).to_owned(),
        );
        values.insert(
            REMOTE_INTEGRATION_ENV,
            bool_display(self.remote_integration).to_owned(),
        );
        values.insert(REMOTE_REUSE_ENV, bool_display(self.remote_reuse).to_owned());
        values.insert(REMOTE_TMUX_ENV, bool_display(self.remote_tmux).to_owned());
        values.insert(REMOTE_PERSIST_ENV, self.remote_persist.as_str().to_owned());
        values.insert(
            REMOTE_IMAGE_PASTE_ENV,
            self.remote_image_paste.as_str().to_owned(),
        );
        values.insert(
            SESSION_REPLAY_ENV,
            bool_display(self.session_replay).to_owned(),
        );
        values.insert(
            INTERACTIVE_URLS_ENV,
            bool_display(self.interactive_urls).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_ENV,
            bool_display(self.interactive_paths).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_BAREWORDS_ENV,
            bool_display(self.interactive_paths_barewords).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_CLICK_HINT_ENV,
            bool_display(self.interactive_paths_click_hint).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_IMAGE_INLINE_ENV,
            bool_display(self.interactive_paths_image_inline).to_owned(),
        );
        if !self.interactive_paths_editor.is_empty() {
            values.insert(
                INTERACTIVE_PATHS_EDITOR_ENV,
                self.interactive_paths_editor.clone(),
            );
        }
        if let Some(duration) = self.native_autoclose {
            values.insert(NATIVE_AUTOCLOSE_ENV, duration.as_millis().to_string());
        }
        values
    }
}
