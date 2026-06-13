use crate::settings::{SettingInfo, Settings};

use super::overlay::OverlayInput;

#[derive(Debug, Clone)]
pub(super) struct SettingsPanel {
    entries: Vec<SettingInfo>,
    selected: usize,
    scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsPanelSignature {
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) entries: Vec<SettingsPanelEntrySignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsPanelEntrySignature {
    pub(super) key: &'static str,
    pub(super) value: String,
    pub(super) description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsPanelLine {
    pub(super) text: String,
    pub(super) focused: bool,
}

impl SettingsPanel {
    pub(super) fn new(settings: &Settings) -> Self {
        let mut panel = Self {
            entries: settings.setting_info(),
            selected: 0,
            scroll: 0,
        };
        panel.clamp();
        panel
    }

    pub(super) fn refresh(&mut self, settings: &Settings) {
        let selected_key = self
            .entries
            .get(self.selected)
            .map(|entry| entry.key)
            .unwrap_or("theme");
        self.entries = settings.setting_info();
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.key == selected_key)
            .unwrap_or(0);
        self.clamp();
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) {
        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-6),
            OverlayInput::PageDown => self.move_selection(6),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => {
                self.set_selection(self.entries.len().saturating_sub(1));
            }
            _ => {}
        }
    }

    pub(super) fn render_signature(&self) -> SettingsPanelSignature {
        SettingsPanelSignature {
            selected: self.selected,
            scroll: self.scroll,
            entries: self
                .entries
                .iter()
                .map(|entry| SettingsPanelEntrySignature {
                    key: entry.key,
                    value: entry.value.clone(),
                    description: entry.description,
                })
                .collect(),
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        let content_width = self
            .entries
            .iter()
            .map(|entry| entry.name.chars().count() + entry.value.chars().count() + 8)
            .max()
            .unwrap_or(48)
            .max(64);
        content_width.saturating_add(4).min(columns)
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<SettingsPanelLine> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        let mut current_group = "";
        for (index, entry) in self.entries.iter().enumerate().skip(self.scroll) {
            if lines.len() >= body_height {
                break;
            }
            if entry.group != current_group {
                current_group = entry.group;
                lines.push(SettingsPanelLine {
                    text: format!("  {current_group}"),
                    focused: false,
                });
                if lines.len() >= body_height {
                    break;
                }
            }

            let focused = index == self.selected;
            let marker = if focused { ">" } else { " " };
            let mut value = entry.value.clone();
            let max_value = body_width.saturating_sub(entry.name.chars().count() + 6);
            if value.chars().count() > max_value {
                value = ellipsize(&value, max_value);
            }
            lines.push(SettingsPanelLine {
                text: format!("{marker} {}: {value}", entry.name),
                focused,
            });
            if lines.len() >= body_height {
                break;
            }

            let detail = setting_detail(entry);
            for wrapped in wrap_words(&detail, body_width.saturating_sub(4)) {
                if lines.len() >= body_height {
                    break;
                }
                lines.push(SettingsPanelLine {
                    text: format!("    {wrapped}"),
                    focused: false,
                });
            }
        }

        lines
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
        let visible_slack = 5;
        if self.selected >= self.scroll + visible_slack {
            self.scroll = self.selected.saturating_sub(visible_slack - 1);
        }
        self.scroll = self.scroll.min(self.entries.len() - 1);
    }
}

fn setting_detail(entry: &SettingInfo) -> String {
    let mut detail = entry.description.to_owned();
    detail.push_str(" Env: ");
    detail.push_str(entry.env);
    detail.push('.');
    if let Some(range) = entry.range {
        detail.push_str(" Range: ");
        detail.push_str(range);
        detail.push('.');
    }
    if !entry.options.is_empty() {
        detail.push_str(" Values: ");
        detail.push_str(&entry.options.join(", "));
        detail.push('.');
    }
    if !entry.reloadable {
        detail.push_str(" Startup-only.");
    }
    detail
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
    use crate::settings::{FONT_SIZE_ENV, Settings};

    #[test]
    fn descriptions_are_complete_for_every_setting() {
        let settings = Settings::default();
        let entries = settings.setting_info();
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|entry| !entry.description.trim().is_empty())
        );
    }

    #[test]
    fn panel_navigation_is_bounded_and_scrolls() {
        let mut panel = SettingsPanel::new(&Settings::default());
        assert_eq!(panel.render_signature().selected, 0);

        panel.handle_input(OverlayInput::Down);
        assert_eq!(panel.render_signature().selected, 1);
        panel.handle_input(OverlayInput::End);
        let end = panel.render_signature();
        assert_eq!(end.selected, end.entries.len() - 1);
        assert!(end.scroll > 0);
        panel.handle_input(OverlayInput::Home);
        assert_eq!(panel.render_signature().selected, 0);
    }

    #[test]
    fn display_rows_include_current_values_and_help_text() {
        let settings = Settings {
            font_size_px: 18.0,
            ..Settings::default()
        };
        let panel = SettingsPanel::new(&settings);
        let lines = panel.visible_lines(70, 80);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Font size: 18"));
        assert!(text.contains(FONT_SIZE_ENV));
    }
}
