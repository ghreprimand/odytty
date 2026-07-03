// SPDX-License-Identifier: GPL-3.0-only
//! First-run onboarding overlay (ONBOARD). A static welcome card shown once on
//! the very first launch — when no `odytty.conf` exists yet — to surface the
//! settings/theme overlay and a few key shortcuts, then get out of the way.
//!
//! There is no telemetry and no "seen" flag on disk (U6): first-run memory is
//! simply the existence of the user-owned config file, which materialises the
//! first time the user saves any setting. The App opens this panel at startup
//! iff the config path resolves to a file that does not yet exist (or the
//! `ODYTTY_ONBOARDING` env override is set); see `App::new`.
//!
//! The shortcut hints read the *live* bindings (`KeyBindings::chord_for_action`)
//! rather than hardcoded strings, so the card stays truthful after a KB-REMAP
//! rebind and reflects the user's actual config.
//!
//! Like every other [`super::overlay::OverlayMode`], this panel never blocks the
//! PTY: the terminal stays live behind it, and any key dismisses or is swallowed
//! (`super::overlay::OverlayUi::handle_onboarding_input`). It is off-path-
//! identical when not first-run: the `Onboarding` mode is simply never entered,
//! so the default render path is byte-for-byte unchanged.

use crate::settings::{BindableAction, Settings, format_key_chord};

use super::bindings::KeyBindings;

/// One rendered body line of the onboarding card. The overlay lifts this into
/// its shared `OverlayLine` for painting (mirrors the other panels' line types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OnboardingLine {
    pub(super) text: String,
    pub(super) focused: bool,
}

/// Render-cache signature for the onboarding card. The content is static for a
/// given binding set, so the lines fully describe the render. `Default` (an
/// empty card) backs the test fixtures' closed-overlay signatures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct OnboardingSignature {
    lines: Vec<String>,
}

/// The first-run welcome card. Holds the resolved content lines (built from the
/// live key bindings at construction / `refresh`), nothing more — it is a static
/// info panel with no navigation or scroll state.
#[derive(Debug, Clone)]
pub(super) struct OnboardingPanel {
    lines: Vec<String>,
}

impl OnboardingPanel {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            lines: build_lines(settings),
        }
    }

    /// Rebuild the card content from the current bindings (called when the
    /// overlay refreshes its settings, e.g. a live config reload, so the hints
    /// never drift from the active chords).
    pub(super) fn refresh(&mut self, settings: &Settings) {
        self.lines = build_lines(settings);
    }

    /// Desired panel width in cells for a `columns`-wide grid: the longest
    /// content line plus border/padding, clamped to the grid.
    pub(super) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        let content_width = self
            .lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(48)
            .max(48);
        content_width.saturating_add(4).min(columns)
    }

    /// The rendered body lines, clipped to the visible body box. No line is
    /// "focused" — the card is static, with no selectable rows.
    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<OnboardingLine> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }
        self.lines
            .iter()
            .take(body_height)
            .map(|line| OnboardingLine {
                text: clip(line, body_width),
                focused: false,
            })
            .collect()
    }

    pub(super) fn render_signature(&self) -> OnboardingSignature {
        OnboardingSignature {
            lines: self.lines.clone(),
        }
    }
}

/// Build the static welcome content with live-chord shortcut hints (D-OB-3).
fn build_lines(settings: &Settings) -> Vec<String> {
    let bindings = KeyBindings::from_overrides(&settings.key_bindings);
    let hint = |action: BindableAction, label: &str| {
        let chord = bindings
            .chord_for_action(action)
            .map(format_key_chord)
            .unwrap_or_else(|| "unbound".to_owned());
        format!("  {chord}   {label}")
    };

    vec![
        "Welcome to OdyTTY — your own custom terminal.".to_owned(),
        String::new(),
        "This looks like your first launch. A few shortcuts to begin:".to_owned(),
        String::new(),
        hint(BindableAction::SettingsPanel, "Open settings"),
        hint(BindableAction::ThemePicker, "Browse themes"),
        hint(BindableAction::Search, "Search the scrollback"),
        hint(BindableAction::Copy, "Copy selection"),
        hint(BindableAction::Paste, "Paste"),
        String::new(),
        "In Settings, press  /  to search settings by name.".to_owned(),
        "Right-click a tab and choose Rename Tab to name a workflow.".to_owned(),
        "OdyTTY writes your config when you save changes — no hand-editing.".to_owned(),
        String::new(),
        "Press Enter or Esc to dismiss.".to_owned(),
    ]
}

/// Clip a line to `width` cells (char-count), appending a tilde when truncated.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "~".to_owned();
    }
    let mut out = text.chars().take(width - 1).collect::<String>();
    out.push('~');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_lists_live_shortcut_chords() {
        let panel = OnboardingPanel::new(&Settings::default());
        let lines = panel.visible_lines(80, 40);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Welcome to OdyTTY"));
        assert!(text.contains("Open settings"));
        assert!(text.contains("Browse themes"));
        // F4 ODP-5: the rename-tab discoverability tip is surfaced on the card.
        assert!(text.contains("Rename Tab"));
        // The default settings-panel chord is Ctrl+Shift+, — the hint must show
        // the live chord, never a hardcoded placeholder (D-OB-3).
        let settings_chord = KeyBindings::default()
            .chord_for_action(BindableAction::SettingsPanel)
            .map(format_key_chord)
            .unwrap();
        assert!(
            text.contains(&settings_chord),
            "card shows the live settings chord {settings_chord:?}: {text}"
        );
    }

    #[test]
    fn hints_follow_a_remapped_chord() {
        // Rebind the settings-panel action and confirm the card reflects it.
        let mut settings = Settings::default();
        let remapped = KeyBindings::default()
            .chord_for_action(BindableAction::ThemePicker)
            .unwrap();
        settings.key_bindings = vec![crate::settings::KeyBindingOverride {
            chord: remapped,
            action: BindableAction::SettingsPanel,
        }];
        let panel = OnboardingPanel::new(&settings);
        let text = panel
            .visible_lines(80, 40)
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(&format_key_chord(remapped)));
    }

    #[test]
    fn signature_changes_with_bindings() {
        let plain = OnboardingPanel::new(&Settings::default()).render_signature();
        let mut settings = Settings::default();
        settings.key_bindings = vec![crate::settings::KeyBindingOverride {
            chord: KeyBindings::default()
                .chord_for_action(BindableAction::ThemePicker)
                .unwrap(),
            action: BindableAction::SettingsPanel,
        }];
        let remapped = OnboardingPanel::new(&settings).render_signature();
        assert_ne!(plain, remapped);
    }

    #[test]
    fn empty_body_box_yields_no_lines() {
        let panel = OnboardingPanel::new(&Settings::default());
        assert!(panel.visible_lines(0, 40).is_empty());
        assert!(panel.visible_lines(80, 0).is_empty());
    }
}
