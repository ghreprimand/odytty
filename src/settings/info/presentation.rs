// SPDX-License-Identifier: GPL-3.0-only
//! Per-setting value presentation for incremental panel updates.

use super::super::*;

impl Settings {
    /// Return only the human-readable `value` string for a single setting key,
    /// mirroring the per-field derivation in [`Self::setting_info`]. Used by the
    /// settings panel to update one row in place after a live edit instead of
    /// rebuilding the full [`SettingInfo`] table.
    /// Returns `None` for an unknown key so callers can fall back to a full
    /// rebuild if the inventory changes shape.
    pub fn display_value_for_key(&self, key: &str) -> Option<String> {
        let value = match key {
            "theme" => self.theme_config_value(),
            "follow_os_theme" => bool_display(self.follow_os_theme).to_owned(),
            "follow_external_palette" => bool_display(self.follow_external_palette).to_owned(),
            "external_palette_provider" => self.external_palette_provider.as_str().to_owned(),
            "external_palette_path" => self
                .external_palette_path
                .clone()
                .unwrap_or_else(|| "unset".to_owned()),
            "external_palette_status" => {
                if self.follow_external_palette {
                    "pending".to_owned()
                } else {
                    "off".to_owned()
                }
            }
            "os_theme_dark" => self
                .os_theme_dark
                .clone()
                .unwrap_or_else(|| "unset".to_owned()),
            "os_theme_light" => self
                .os_theme_light
                .clone()
                .unwrap_or_else(|| "unset".to_owned()),
            "visual" => self.visual.as_str().to_owned(),
            "font" => self
                .explicit_font_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            "font_family" => self
                .font_family
                .clone()
                .unwrap_or_else(|| "unset".to_owned()),
            "font_weight" => {
                if self.font_weight.is_empty() {
                    "regular".to_owned()
                } else {
                    self.font_weight.clone()
                }
            }
            "font_size" => format_float(self.font_size_px),
            "text_gamma" => format_float(self.text_gamma),
            "text_brightness" => format_float(self.text_brightness),
            "stem_darken" => format_float(self.stem_darken),
            "min_contrast" => format_float(self.min_contrast),
            "focus_dim" => format_float(self.focus_dim),
            "inactive_pane_dim" => format_float(self.inactive_pane_dim),
            "render_quality" => self.render_quality.as_str().to_owned(),
            "window_padding" => format_float(self.window_padding_px),
            "window_border" => bool_display(self.window_border).to_owned(),
            "window_decorations" => bool_display(self.window_decorations).to_owned(),
            "window_transparency" => bool_display(self.window_transparency).to_owned(),
            "window_opacity" => format_float(self.window_opacity),
            "retro" => bool_display(self.retro).to_owned(),
            "crt" => bool_display(self.crt).to_owned(),
            "bloom" => bool_display(self.bloom).to_owned(),
            "bloom_threshold" => format_float(self.bloom_threshold),
            "bloom_intensity" => format_float(self.bloom_intensity),
            "bloom_radius" => format_float(self.bloom_radius),
            "crt_scanline_intensity" => format_float(self.crt_scanline_intensity),
            "crt_scanline_period" => format_float(self.crt_scanline_period),
            "crt_vignette_strength" => format_float(self.crt_vignette_strength),
            "crt_curvature" => format_float(self.crt_curvature),
            "background_treatment" => self.background_treatment.as_str().to_owned(),
            "background_image" => self
                .background_image
                .as_ref()
                .map(|path| {
                    if crate::settings::is_bundled_background(path) {
                        format!("{} (bundled)", crate::settings::BUNDLED_BACKGROUND_TOKEN)
                    } else {
                        path.display().to_string()
                    }
                })
                .unwrap_or_else(|| "none".to_owned()),
            "cell_bg_opacity" => format_float(1.0 - self.cell_bg_opacity),
            "colored_bg_opacity" => format_float(self.colored_bg_opacity),
            "selection_opacity" => format_float(self.selection_opacity),
            "background_blur_radius" => self.background_blur_radius.to_string(),
            "background_image_scrim" => self
                .background_image_scrim
                .map(format_float)
                .unwrap_or_else(|| "auto".to_owned()),
            "new_output_fade" => bool_display(self.new_output_fade).to_owned(),
            "new_output_fade_ms" => format_float(self.new_output_fade_ms),
            "reduced_motion" => bool_display(self.reduced_motion).to_owned(),
            "subpixel" => subpixel_display(self.subpixel).to_owned(),
            "line_height" => format_float(self.line_height),
            "ligatures" => bool_display(self.ligatures).to_owned(),
            "ss01" => bool_display(self.ligature_ss01).to_owned(),
            "ss02" => bool_display(self.ligature_ss02).to_owned(),
            "kitty_named_transports" => bool_display(self.kitty_named_transports).to_owned(),
            "synthetic_styles" => bool_display(self.synthetic_styles).to_owned(),
            "geometric_boxdraw" => bool_display(self.geometric_boxdraw).to_owned(),
            "box_thickness" => format_float(self.box_thickness),
            "symbol_fallback" => bool_display(self.symbol_fallback).to_owned(),
            "symbol_font" => self
                .symbol_font
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "auto".to_owned()),
            "symbol_map" => {
                if self.symbol_map.is_empty() {
                    "none".to_owned()
                } else {
                    super::super::format_symbol_map(&self.symbol_map)
                }
            }
            "themed_ui_roles" => bool_display(self.themed_ui_roles).to_owned(),
            "cursor_style" => cursor_style_display(self.cursor_style).to_owned(),
            "cursor_blink" => self.cursor_blink.as_str().to_owned(),
            "cursor_easing" => bool_display(self.cursor_easing).to_owned(),
            "cursor_glow" => bool_display(self.cursor_glow).to_owned(),
            "cursor_glow_intensity" => format_float(self.cursor_glow_intensity),
            "cursor_trail" => bool_display(self.cursor_trail).to_owned(),
            "cursor_trail_strength" => self.cursor_trail_strength.as_str().to_owned(),
            "cursor_motion" => bool_display(self.cursor_motion).to_owned(),
            "keybinds" => key_bindings_display(&self.key_bindings),
            "pane_prefix" => pane_prefix_display(self.pane_prefix),
            "scroll_wheel_lines" => format_float(self.scroll_wheel_lines),
            "scrollback_lines" => format_float(self.scrollback_lines),
            "selection_drag_extend" => bool_display(self.selection_drag_extend).to_owned(),
            "scroll_drag_speed" => self.scroll_drag_speed.as_str().to_owned(),
            "pixel_scroll" => bool_display(self.pixel_scroll).to_owned(),
            "scroll_pixel_speed" => format_float(self.scroll_pixel_speed),
            "scroll_glide" => bool_display(self.scroll_glide).to_owned(),
            "scrollbar_drag" => bool_display(self.scrollbar_drag).to_owned(),
            "wheel_zoom" => bool_display(self.wheel_zoom).to_owned(),
            "command_status_gutter" => bool_display(self.command_status_gutter).to_owned(),
            "always_show_tab_bar" => bool_display(self.always_show_tab_bar).to_owned(),
            "tab_bar_placement" => self.tab_bar_placement.rail_side_str().to_owned(),
            "workspace_rail" => self.workspace_rail.as_str().to_owned(),
            "tab_bar_height" => self.tab_bar_height.as_config_string(),
            "tab_rail_width" => self.tab_rail_width.as_config_string(),
            "tab_rail_max_width" => format_float(self.tab_rail_max_width),
            "tab_rail_gap" => format_float(self.tab_rail_gap),
            "tab_rail_slot_rows" => format_float(self.tab_rail_slot_rows),
            "tab_panel_strength" => format_float(self.tab_panel_strength),
            "tab_seam" => bool_display(self.tab_seam).to_owned(),
            "tab_rail_autohide" => bool_display(self.tab_rail_autohide).to_owned(),
            "tab_rail_reveal_px" => format_float(self.tab_rail_reveal_px),
            "sh_click" => bool_display(self.sh_click).to_owned(),
            "buttons" => bool_display(self.buttons).to_owned(),
            "buttons_iterm_compat" => bool_display(self.buttons_iterm_compat).to_owned(),
            "buttons_sticky" => bool_display(self.buttons_sticky).to_owned(),
            "shell_integration" => bool_display(self.shell_integration).to_owned(),
            "shell_key_enhancement" => bool_display(self.shell_key_enhancement).to_owned(),
            "confirm_close" => bool_display(self.confirm_close).to_owned(),
            "warn_on_risky_paste" => bool_display(self.warn_on_risky_paste).to_owned(),
            "ssh_config_hosts" => bool_display(self.ssh_config_hosts).to_owned(),
            "remote_integration" => bool_display(self.remote_integration).to_owned(),
            "remote_reuse" => bool_display(self.remote_reuse).to_owned(),
            "remote_tmux" => bool_display(self.remote_tmux).to_owned(),
            "remote_persist" => self.remote_persist.as_str().to_owned(),
            "remote_image_paste" => self.remote_image_paste.as_str().to_owned(),
            "session_replay" => bool_display(self.session_replay).to_owned(),
            "restore_workspaces" => bool_display(self.restore_workspaces).to_owned(),
            "shell_exit_closes" => self.shell_exit_closes.as_str().to_owned(),
            "interactive_urls" => bool_display(self.interactive_urls).to_owned(),
            "interactive_paths" => bool_display(self.interactive_paths).to_owned(),
            "interactive_paths_barewords" => {
                bool_display(self.interactive_paths_barewords).to_owned()
            }
            "interactive_paths_click_hint" => {
                bool_display(self.interactive_paths_click_hint).to_owned()
            }
            "interactive_paths_image_inline" => {
                bool_display(self.interactive_paths_image_inline).to_owned()
            }
            "interactive_paths_editor" => {
                if self.interactive_paths_editor.is_empty() {
                    "default".to_owned()
                } else {
                    self.interactive_paths_editor.clone()
                }
            }
            "osc52_read" => bool_display(self.osc52_read).to_owned(),
            "osc52_write" => self.osc52_write.as_str().to_owned(),
            "copy_on_select" => bool_display(self.copy_on_select).to_owned(),
            "smart_ctrl_c" => self.smart_ctrl_c.as_str().to_owned(),
            "cvd_mode" => self.cvd_mode.as_str().to_owned(),
            "cvd_strength" => format_float(self.cvd_strength),
            "bell" => self.bell.as_str().to_owned(),
            "notifications" => self.notifications.as_str().to_owned(),
            "native_autoclose_ms" => self
                .native_autoclose
                .map(|duration| format!("{} ms", duration.as_millis()))
                .unwrap_or_else(|| "unset".to_owned()),
            _ => return None,
        };
        Some(value)
    }
}
