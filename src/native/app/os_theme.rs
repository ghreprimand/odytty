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
        if self.settings.follow_external_palette
            && let Some(theme) = self.external_palette_follow.last_known_good_theme()
        {
            return theme;
        }
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

    /// The CVD-adapted theme the active pane should drive the window chrome and
    /// GPU with: the active profile tab's authored profile theme when it set
    /// one, otherwise the global [`Self::effective_theme`]. CVD mode/strength
    /// apply on top of a profile theme exactly as they do for the global theme.
    pub(super) fn active_session_presentation_theme(&self) -> Theme {
        match self.sessions.active().profile_theme.as_ref() {
            Some(profile) => crate::native::cvd_theme::effective_theme(
                profile,
                self.settings.cvd_mode,
                self.settings.cvd_strength,
            ),
            None => self.effective_theme,
        }
    }

    /// Align the window chrome and GPU theme with the active pane. A profile tab
    /// presents its own theme; a plain tab presents the global effective theme.
    /// Idempotent: when the presented theme is unchanged (repeated switches
    /// between plain tabs, or re-entry of the same profile tab) this bumps no
    /// epoch and forces no rebuild, so the common case stays free.
    pub(super) fn present_active_session_chrome(&mut self) {
        let next = self.active_session_presentation_theme();
        if next == self.chrome_theme {
            return;
        }
        self.chrome_theme = next;
        text::set_default_colors(next.foreground, next.background);
        text::set_ansi_palette(&next.palette);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_theme(next);
        }
        self.last_render_signature = None;
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Reconfigure the external-palette follower from current settings and
    /// refresh immediately when following is on.
    pub(super) fn sync_external_palette_follow(&mut self, now: std::time::Instant) {
        let path = self
            .settings
            .external_palette_path
            .as_ref()
            .map(std::path::PathBuf::from);
        self.external_palette_follow.configure(
            self.settings.follow_external_palette,
            self.settings.external_palette_provider,
            path,
            now,
        );
        if self.settings.follow_external_palette {
            use crate::external_palette::FollowPollOutcome;
            match self.external_palette_follow.refresh_now(now) {
                FollowPollOutcome::Applied(_) | FollowPollOutcome::Retained => {
                    self.apply_os_theme_override();
                }
                FollowPollOutcome::Unchanged => {}
            }
        }
        self.sync_settings_external_palette_status();
    }

    /// Poll the external-palette follower when armed.
    pub(super) fn poll_external_palette_follow(&mut self, now: std::time::Instant) {
        if !self.settings.follow_external_palette {
            return;
        }
        use crate::external_palette::FollowPollOutcome;
        match self.external_palette_follow.poll(now) {
            FollowPollOutcome::Applied(_) => {
                self.apply_os_theme_override();
            }
            FollowPollOutcome::Unchanged | FollowPollOutcome::Retained => {}
        }
        self.sync_settings_external_palette_status();
    }

    pub(super) fn sync_settings_external_palette_status(&mut self) {
        self.overlay
            .sync_external_palette_status(&self.external_palette_follow.status().as_display());
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
        let global = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let cvd_mode = self.settings.cvd_mode;
        let cvd_strength = self.settings.cvd_strength;
        // NF21-4: an OS light/dark flip must reach EVERY session's terminal
        // model, not just the active one through `Deref` — otherwise a
        // background tab (or background workspace's tabs) keeps answering OSC
        // 4/10/11 with the old theme and paints a stale cursor default on
        // switch-back. Mirrors the reload-seam fan-out. Per session: a profile
        // tab keeps its profile theme (CVD on top) so an OS flip never flattens
        // it; a plain tab follows the new global theme.
        for session in self.sessions.iter() {
            let theme = match session.profile_theme.as_ref() {
                Some(profile) => {
                    crate::native::cvd_theme::effective_theme(profile, cvd_mode, cvd_strength)
                }
                None => global,
            };
            let cursor_default = if themed_ui_roles {
                rgb(theme.cursor)
            } else {
                rgb(theme.foreground)
            };
            let base_fg = rgb(theme.foreground);
            let base_bg = rgb(theme.background);
            // C29: keep OSC 4 replies in sync with the newly effective theme.
            let base_palette = theme.palette.map(rgb);
            if let Ok(mut terminal) = session.terminal.lock() {
                terminal.set_base_colors(base_fg, base_bg, cursor_default);
                terminal.set_base_palette(base_palette);
            }
        }
        // Present the active pane's theme on the window chrome/GPU (a profile tab
        // keeps its own; a plain tab follows the new global). This performs the
        // text-default / gpu.set_theme / epoch-bump / rebuild work.
        self.present_active_session_chrome();
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
