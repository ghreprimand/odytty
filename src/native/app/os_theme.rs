// SPDX-License-Identifier: GPL-3.0-only
//! OS dark/light appearance following for the native app (OS-THEME).
//!
//! Mechanically split out of `app/mod.rs` to keep that file under the
//! source-size cap; no behavior change. These `App` methods turn the desktop's
//! color-scheme preference — delivered live by the compositor on Wayland via
//! `WindowEvent::ThemeChanged`, seeded from `ODYTTY_APPEARANCE` on X11 where no
//! live signal exists — into the active presentation theme. They live in a
//! child module so they can reach `App`'s private fields and the sibling free
//! helpers (`rgb`, `text::*`) directly; `app/mod.rs` calls
//! [`App::resolve_active_theme`] from the settings-reload seam and
//! [`App::apply_os_theme_override`] from `resumed` and the `ThemeChanged` arm.

use super::*;

impl App {
    /// Resolve the theme that should currently drive presentation. Returns the
    /// OS-selected dark/light theme when following is on and the OS signal maps
    /// to a configured, resolvable theme name; otherwise returns the authored
    /// [`Settings::theme`] unchanged. The authored theme is always the fallback,
    /// so an unset or unknown direction never guesses (D-OST-3/T3), and the off
    /// path (`follow_os_theme = false`) returns exactly `self.settings.theme` —
    /// byte-identical to before the feature existed.
    pub(super) fn resolve_active_theme(&self) -> Theme {
        if self.settings.follow_os_theme {
            // `theme = system` is an alias for following with default dark/light
            // mappings. A user-supplied `os_theme_dark`/`os_theme_light`
            // override always wins; the defaults apply only when the alias is
            // active AND that direction is unset.
            let default_dark = self
                .settings
                .theme_is_system
                .then_some(crate::settings::DEFAULT_OS_THEME_DARK);
            let default_light = self
                .settings
                .theme_is_system
                .then_some(crate::settings::DEFAULT_OS_THEME_LIGHT);
            let name = match self.os_theme {
                Some(winit::window::Theme::Dark) => {
                    self.settings.os_theme_dark.as_deref().or(default_dark)
                }
                Some(winit::window::Theme::Light) => {
                    self.settings.os_theme_light.as_deref().or(default_light)
                }
                None => None,
            };
            if let Some(name) = name
                && let Some(theme) = Theme::from_name(name)
            {
                return theme;
            }
        }
        self.settings.theme
    }

    /// Re-resolve and republish the active theme after an OS appearance change
    /// (or at startup when following is on). Recomputes `self.theme` via
    /// [`Self::resolve_active_theme`], re-derives the CVD-adapted
    /// `effective_theme`, and pushes it to every renderer color seam — the
    /// theme-only subset of the settings-reload publish (no font/padding/bloom/
    /// crt work, which a pure theme switch never changes). A no-op (no epoch
    /// bump, no rebuild) when the resolved theme already equals the live one, so
    /// repeated `ThemeChanged` events at the same preference are idempotent and
    /// free (T4).
    pub(super) fn apply_os_theme_override(&mut self) {
        let next = self.resolve_active_theme();
        if next == self.theme {
            return;
        }
        self.theme = next;
        self.effective_theme = self.cvd_cache.resolve(
            &self.theme,
            self.settings.cvd_mode,
            self.settings.cvd_strength,
        );
        text::set_default_colors(
            self.effective_theme.foreground,
            self.effective_theme.background,
        );
        text::set_ansi_palette(&self.effective_theme.palette);
        if let Ok(mut terminal) = self.terminal.lock() {
            let cursor_default = if self.themed_ui_roles {
                rgb(self.effective_theme.cursor)
            } else {
                rgb(self.effective_theme.foreground)
            };
            terminal.set_base_colors(
                rgb(self.effective_theme.foreground),
                rgb(self.effective_theme.background),
                cursor_default,
            );
            // C29: keep OSC 4 replies in sync with the newly effective theme.
            terminal.set_base_palette(self.effective_theme.palette.map(rgb));
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_theme(self.effective_theme);
        }
        self.last_render_signature = None;
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// X11 appearance fallback: read `ODYTTY_APPEARANCE=dark|light` as a manual
/// appearance seed for platforms where the compositor never delivers a live
/// `ThemeChanged` signal (notably X11). Returns `None` for an unset or
/// unrecognized value, so it only ever supplies an explicit user choice.
pub(super) fn env_appearance_override() -> Option<winit::window::Theme> {
    match std::env::var("ODYTTY_APPEARANCE")
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "dark" => Some(winit::window::Theme::Dark),
        "light" => Some(winit::window::Theme::Light),
        _ => None,
    }
}
