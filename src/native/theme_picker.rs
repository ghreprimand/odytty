// SPDX-License-Identifier: GPL-3.0-only
use crate::settings::{SYSTEM_THEME_NAME, SettingEdit, Settings, THEME_ENV};
use crate::theme::{Theme, all as built_in_themes, relative_luminance};

use super::overlay::OverlayInput;

#[derive(Debug, Clone)]
pub(super) struct ThemePicker {
    entries: Vec<ThemeEntry>,
    selected: usize,
    scroll: usize,
    original: Theme,
    /// Whether the picker was opened with `theme = system` active, so the
    /// `original` marker lands on the system alias row.
    original_is_system: bool,
    message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThemePickerSignature {
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) original: &'static str,
    pub(super) current: &'static str,
    pub(super) message: Option<String>,
    pub(super) entries: Vec<ThemePickerEntrySignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThemePickerEntrySignature {
    pub(super) name: &'static str,
    pub(super) appearance: ThemeAppearance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ThemeAppearance {
    Dark,
    Light,
}

impl ThemeAppearance {
    fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThemePickerLine {
    pub(super) text: String,
    pub(super) focused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ThemePickerOutcome {
    Consumed,
    Preview(Theme),
    Persist(Vec<SettingEdit>),
    OpenBuilder(Theme),
    Cancel(Theme),
}

#[derive(Debug, Clone)]
struct ThemeEntry {
    theme: Theme,
    appearance: ThemeAppearance,
    /// Whether this row represents the `system` alias rather than a concrete
    /// built-in theme. The `theme` field then holds the entry used for live
    /// preview rendering only; persistence emits the `system` token.
    is_system: bool,
}

impl ThemePicker {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut picker = Self {
            // The `system` alias is offered as the first row so users can opt
            // into OS dark/light following from the picker without editing raw
            // config. Its `theme` is the default dark theme for preview only.
            entries: std::iter::once(ThemeEntry::system_alias())
                .chain(built_in_themes().iter().copied().map(ThemeEntry::new))
                .collect(),
            selected: 0,
            scroll: 0,
            original: settings.theme,
            original_is_system: settings.theme_is_system,
            message: None,
        };
        picker.select_theme(settings);
        picker
    }

    pub(super) fn open(&mut self, settings: &Settings) {
        self.original = settings.theme;
        self.original_is_system = settings.theme_is_system;
        self.message = Some(
            "Built-in themes only in this picker. User theme files remain editable in settings."
                .to_owned(),
        );
        self.select_theme(settings);
    }

    pub(super) fn refresh(&mut self, settings: &Settings) {
        self.original = settings.theme;
        self.original_is_system = settings.theme_is_system;
        self.select_theme(settings);
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ThemePickerOutcome {
        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-6),
            OverlayInput::PageDown => self.move_selection(6),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => self.set_selection(self.entries.len().saturating_sub(1)),
            OverlayInput::Activate => return self.persist_selected(),
            OverlayInput::Char('b') | OverlayInput::Char('B') => {
                if let Some(theme) = self.selected_theme() {
                    return ThemePickerOutcome::OpenBuilder(theme);
                }
            }
            OverlayInput::Close => return ThemePickerOutcome::Cancel(self.original),
            _ => return ThemePickerOutcome::Consumed,
        }

        self.selected_theme()
            .map(ThemePickerOutcome::Preview)
            .unwrap_or(ThemePickerOutcome::Consumed)
    }

    pub(super) fn save_succeeded(&mut self, changed: usize) {
        self.original = self.selected_theme().unwrap_or(self.original);
        self.message = Some(format!("Saved theme to odytty.conf ({changed} change)."));
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.message = Some(format!("Save failed: {message}"));
    }

    pub(super) fn render_signature(&self) -> ThemePickerSignature {
        ThemePickerSignature {
            selected: self.selected,
            scroll: self.scroll,
            original: self.original.name,
            current: self
                .selected_entry()
                .map(|entry| entry.display_name())
                .unwrap_or(self.original.name),
            message: self.message.clone(),
            entries: self
                .entries
                .iter()
                .map(|entry| ThemePickerEntrySignature {
                    name: entry.display_name(),
                    appearance: entry.appearance,
                })
                .collect(),
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        let longest = self
            .entries
            .iter()
            .map(|entry| entry.display_name().chars().count())
            .max()
            .unwrap_or(16);
        longest.saturating_add(28).max(54).min(columns)
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ThemePickerLine> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        lines.push(ThemePickerLine {
            text: ellipsize(
                "  Theme library - arrows preview, Enter saves, B edits, Esc cancels",
                body_width,
            ),
            focused: false,
        });

        if let Some(message) = self.message.as_deref() {
            for wrapped in wrap_words(message, body_width.saturating_sub(4)) {
                if lines.len() >= body_height {
                    return lines;
                }
                lines.push(ThemePickerLine {
                    text: format!("    {wrapped}"),
                    focused: false,
                });
            }
        }

        for (index, entry) in self.entries.iter().enumerate().skip(self.scroll) {
            if lines.len() >= body_height {
                break;
            }
            let focused = index == self.selected;
            let marker = if focused { ">" } else { " " };
            let is_original = entry.is_system && self.original_is_system
                || !entry.is_system && entry.theme == self.original;
            let original = if is_original { " original" } else { "" };
            let label = if entry.is_system {
                "auto "
            } else {
                entry.appearance.label()
            };
            let text = format!(
                "{marker} {:<20} {:<5}{original}",
                entry.display_name(),
                label
            );
            lines.push(ThemePickerLine {
                text: ellipsize(&text, body_width),
                focused,
            });
        }

        lines.truncate(body_height);
        lines
    }

    fn persist_selected(&mut self) -> ThemePickerOutcome {
        let Some(entry) = self.entries.get(self.selected) else {
            return ThemePickerOutcome::Consumed;
        };
        // The system alias persists the `system` token; concrete themes persist
        // their canonical name.
        let value = if entry.is_system {
            SYSTEM_THEME_NAME.to_owned()
        } else {
            entry.theme.name.to_owned()
        };
        ThemePickerOutcome::Persist(vec![SettingEdit {
            key: "theme",
            env: THEME_ENV,
            value,
        }])
    }

    fn selected_entry(&self) -> Option<&ThemeEntry> {
        self.entries.get(self.selected)
    }

    /// The concrete theme for live preview rendering. The system alias row
    /// previews as its underlying default theme; it is never persisted as that
    /// name (see [`Self::persist_selected`]).
    fn selected_theme(&self) -> Option<Theme> {
        self.selected_entry().map(|entry| entry.theme)
    }

    /// Select the row matching the active settings. When `theme = system` is
    /// active the system alias row is selected; otherwise the concrete theme.
    fn select_theme(&mut self, settings: &Settings) {
        let target_is_system = settings.theme_is_system;
        let target_name = settings.theme.name;
        self.selected = self
            .entries
            .iter()
            .position(|entry| {
                if target_is_system {
                    entry.is_system
                } else {
                    !entry.is_system && entry.theme.name == target_name
                }
            })
            .unwrap_or(0);
        self.clamp();
    }

    fn move_selection(&mut self, delta: isize) {
        let next = self.selected as isize + delta;
        self.set_selection(next.clamp(0, self.entries.len().saturating_sub(1) as isize) as usize);
    }

    fn set_selection(&mut self, selected: usize) {
        self.selected = selected.min(self.entries.len().saturating_sub(1));
        self.clamp();
    }

    fn clamp(&mut self) {
        if self.entries.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = self.selected.min(self.entries.len() - 1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let visible_slack = 8;
        if self.selected >= self.scroll + visible_slack {
            self.scroll = self.selected.saturating_sub(visible_slack - 1);
        }
        self.scroll = self.scroll.min(self.entries.len() - 1);
    }
}

impl ThemeEntry {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            appearance: appearance_for(theme),
            is_system: false,
        }
    }

    /// The `system` alias row. Preview rendering uses the default dark theme;
    /// persistence emits the `system` token via [`Self::is_system`].
    fn system_alias() -> Self {
        Self {
            theme: Theme::ODYSSEY,
            appearance: ThemeAppearance::Dark,
            is_system: true,
        }
    }

    /// Display label for this row: the alias token for `system`, otherwise the
    /// built-in theme name.
    fn display_name(&self) -> &'static str {
        if self.is_system {
            SYSTEM_THEME_NAME
        } else {
            self.theme.name
        }
    }
}

fn appearance_for(theme: Theme) -> ThemeAppearance {
    if relative_luminance(theme.background) > 0.18 {
        ThemeAppearance::Light
    } else {
        ThemeAppearance::Dark
    }
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(12);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let separator = usize::from(!current.is_empty());
        if current.chars().count() + separator + word.chars().count() > width {
            if !current.is_empty() {
                lines.push(current);
                current = String::new();
            }
            if word.chars().count() > width {
                lines.push(ellipsize(word, width));
                continue;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
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
    use crate::settings::write_settings_changes_to_path;

    fn select_theme(picker: &mut ThemePicker, name: &str) {
        let index = picker
            .entries
            .iter()
            .position(|entry| entry.display_name() == name)
            .unwrap_or_else(|| panic!("missing theme {name}"));
        picker.set_selection(index);
    }

    #[test]
    fn picker_lists_system_alias_then_every_builtin_theme() {
        let picker = ThemePicker::new(&Settings::default());
        let signature = picker.render_signature();
        let builtins = crate::theme::names().collect::<Vec<_>>();

        // system alias first, then the full builtin roster.
        assert_eq!(signature.entries.len(), builtins.len() + 1);
        assert_eq!(signature.entries[0].name, SYSTEM_THEME_NAME);
        assert_eq!(
            signature
                .entries
                .iter()
                .skip(1)
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            builtins
        );
    }

    #[test]
    fn navigation_previews_without_persisting() {
        let mut picker = ThemePicker::new(&Settings::default());
        let original = picker.render_signature().original;

        let ThemePickerOutcome::Preview(theme) = picker.handle_input(OverlayInput::Down) else {
            panic!("expected preview");
        };

        assert_ne!(theme.name, original);
        assert_eq!(picker.render_signature().original, original);
    }

    #[test]
    fn cancel_restores_the_original_theme() {
        let mut settings = Settings::default();
        settings.theme = Theme::ODYSSEY;
        let mut picker = ThemePicker::new(&settings);
        select_theme(&mut picker, "monokai");

        let ThemePickerOutcome::Cancel(theme) = picker.handle_input(OverlayInput::Close) else {
            panic!("expected cancel");
        };

        assert_eq!(theme, Theme::ODYSSEY);
    }

    #[test]
    fn enter_emits_theme_writeback_edit() {
        let mut picker = ThemePicker::new(&Settings::default());
        select_theme(&mut picker, "tokyo-night");

        let ThemePickerOutcome::Persist(changes) = picker.handle_input(OverlayInput::Activate)
        else {
            panic!("expected persistence request");
        };

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].key, "theme");
        assert_eq!(changes[0].env, THEME_ENV);
        assert_eq!(changes[0].value, "tokyo-night");
    }

    #[test]
    fn select_persists_via_writeback_without_touching_home() {
        let base = std::env::temp_dir().join(format!(
            "odytty-theme-picker-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("odytty.conf");
        std::fs::write(&path, "# kept\ntheme = plain\nfont_size = 18\n").unwrap();

        let mut picker = ThemePicker::new(&Settings::default());
        select_theme(&mut picker, "dracula");
        let ThemePickerOutcome::Persist(changes) = picker.handle_input(OverlayInput::Activate)
        else {
            panic!("expected persistence request");
        };

        let result = write_settings_changes_to_path(&path, &changes).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result.changed, 1);
        assert!(written.contains("# kept"));
        assert!(written.contains("theme = dracula"));
        assert!(written.contains("font_size = 18"));
        assert!(!written.contains("/home/"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn system_alias_row_persists_system_token() {
        let mut picker = ThemePicker::new(&Settings::default());
        select_theme(&mut picker, SYSTEM_THEME_NAME);
        let ThemePickerOutcome::Persist(changes) = picker.handle_input(OverlayInput::Activate)
        else {
            panic!("expected persistence request");
        };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].key, "theme");
        assert_eq!(changes[0].env, THEME_ENV);
        assert_eq!(changes[0].value, SYSTEM_THEME_NAME);
    }

    #[test]
    fn system_alias_row_is_selected_when_settings_use_system() {
        let mut settings = Settings::default();
        settings.theme_is_system = true;
        let picker = ThemePicker::new(&settings);
        let entry = picker.selected_entry().expect("an entry must be selected");
        assert!(entry.is_system, "system alias must be selected");
    }
}
