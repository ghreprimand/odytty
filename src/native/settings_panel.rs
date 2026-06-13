use crate::settings::{SettingEdit, SettingInfo, SettingKind, Settings, SettingsEditOverlay};

use super::overlay::OverlayInput;

#[derive(Debug, Clone)]
pub(super) struct SettingsPanel {
    edits: SettingsEditOverlay,
    entries: Vec<SettingInfo>,
    selected: usize,
    scroll: usize,
    editing: Option<RowEdit>,
    message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsPanelSignature {
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) editing_key: Option<&'static str>,
    pub(super) changed_count: usize,
    pub(super) message: Option<String>,
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

#[derive(Debug, Clone, PartialEq)]
pub(super) enum SettingsPanelOutcome {
    Consumed,
    Apply(Settings),
    Save(Vec<SettingEdit>),
    OpenThemePicker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowEdit {
    key: &'static str,
    buffer: String,
}

impl SettingsPanel {
    pub(super) fn new(settings: &Settings) -> Self {
        let edits = SettingsEditOverlay::new(settings);
        let mut panel = Self {
            entries: edits.settings().setting_info(),
            edits,
            selected: 0,
            scroll: 0,
            editing: None,
            message: None,
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
        self.edits = SettingsEditOverlay::new(settings);
        self.entries = self.edits.settings().setting_info();
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.key == selected_key)
            .unwrap_or(0);
        self.editing = None;
        self.message = None;
        self.clamp();
    }

    pub(super) fn apply_settings(&mut self, settings: &Settings) {
        let selected_key = self
            .entries
            .get(self.selected)
            .map(|entry| entry.key)
            .unwrap_or("theme");
        self.entries = self.edits.settings().setting_info();
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.key == selected_key)
            .unwrap_or(0);
        if self.edits.settings() != settings {
            self.refresh(settings);
        }
        self.clamp();
    }

    pub(super) fn save_succeeded(&mut self, changed: usize) {
        self.edits.mark_saved();
        self.entries = self.edits.settings().setting_info();
        self.message = Some(format!("Saved {changed} setting change(s) to odytty.conf."));
        self.clamp();
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.message = Some(format!("Save failed: {message}"));
    }

    pub(super) fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        if self.editing.is_some() {
            return self.handle_editing_input(input);
        }

        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-6),
            OverlayInput::PageDown => self.move_selection(6),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => {
                self.set_selection(self.entries.len().saturating_sub(1));
            }
            OverlayInput::Left => return self.step_or_cycle_selected(-1),
            OverlayInput::Right => return self.step_or_cycle_selected(1),
            OverlayInput::Activate => return self.activate_selected(),
            OverlayInput::Save => return self.save_changes(),
            OverlayInput::Char(' ') => return self.activate_selected(),
            _ => {}
        }
        SettingsPanelOutcome::Consumed
    }

    pub(super) fn render_signature(&self) -> SettingsPanelSignature {
        SettingsPanelSignature {
            selected: self.selected,
            scroll: self.scroll,
            editing_key: self.editing.as_ref().map(|edit| edit.key),
            changed_count: self.edits.changed_count(),
            message: self.message.clone(),
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
            let mut value = self.display_value(entry);
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
            if focused && let Some(message) = self.message.as_deref() {
                for wrapped in wrap_words(message, body_width.saturating_sub(4)) {
                    if lines.len() >= body_height {
                        break;
                    }
                    lines.push(SettingsPanelLine {
                        text: format!("    ! {wrapped}"),
                        focused: false,
                    });
                }
            }
        }

        lines
    }

    fn display_value(&self, entry: &SettingInfo) -> String {
        if let Some(edit) = self.editing.as_ref().filter(|edit| edit.key == entry.key) {
            return format!("[{}]", edit.buffer);
        }
        let changed = self
            .edits
            .changes()
            .iter()
            .any(|change| change.key == entry.key);
        if changed {
            format!("{} *", entry.value)
        } else {
            entry.value.clone()
        }
    }

    fn activate_selected(&mut self) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        if !entry.reloadable {
            self.message = Some("Startup-only setting; edit odytty.conf and restart.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }
        match entry.kind {
            SettingKind::Bool => {
                let next = if entry.value == "on" { "off" } else { "on" };
                self.commit_value(entry.key, next)
            }
            SettingKind::Enum if entry.key == "theme" => {
                self.editing = Some(RowEdit {
                    key: entry.key,
                    buffer: entry.value,
                });
                self.message = Some(
                    "Editing: type a built-in theme, user theme name, or theme path.".to_owned(),
                );
                SettingsPanelOutcome::Consumed
            }
            SettingKind::Enum => self.cycle_selected(1),
            SettingKind::Number | SettingKind::String | SettingKind::Path | SettingKind::List => {
                self.editing = Some(RowEdit {
                    key: entry.key,
                    buffer: entry.value,
                });
                self.message =
                    Some("Editing: type a value, Enter applies, Esc cancels.".to_owned());
                SettingsPanelOutcome::Consumed
            }
        }
    }

    fn handle_editing_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        match input {
            OverlayInput::Close => {
                if let Some(edit) = self.editing.take() {
                    self.message = Some(format!("Cancelled edit for {}.", edit.key));
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Activate => {
                let Some(edit) = self.editing.take() else {
                    return SettingsPanelOutcome::Consumed;
                };
                let key = edit.key;
                let value = edit.buffer;
                self.commit_value(key, &value)
            }
            OverlayInput::Backspace => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.buffer.pop();
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.buffer.push(ch);
                }
                SettingsPanelOutcome::Consumed
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    fn step_or_cycle_selected(&mut self, direction: isize) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        if entry.key == "theme" {
            self.message = Some("Opening built-in theme picker.".to_owned());
            return SettingsPanelOutcome::OpenThemePicker;
        }
        match entry.kind {
            SettingKind::Enum => self.cycle_selected(direction),
            SettingKind::Number => {
                let step = number_step(entry.key) * direction as f32;
                let parsed = entry.value.parse::<f32>().unwrap_or(0.0);
                self.commit_value(entry.key, &format!("{:.3}", parsed + step))
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    fn cycle_selected(&mut self, direction: isize) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        let options = edit_options(&entry);
        let Some(current) = options.iter().position(|value| *value == entry.value) else {
            self.message = Some("Type a custom value with Enter.".to_owned());
            return SettingsPanelOutcome::Consumed;
        };
        let len = options.len() as isize;
        let next = (current as isize + direction).rem_euclid(len) as usize;
        self.commit_value(entry.key, options[next])
    }

    fn commit_value(&mut self, key: &'static str, value: &str) -> SettingsPanelOutcome {
        match self.edits.apply_raw(key, value) {
            Ok(Some(settings)) => {
                self.entries = self.edits.settings().setting_info();
                self.message = Some(format!("Applied {key}."));
                self.clamp();
                SettingsPanelOutcome::Apply(settings)
            }
            Ok(None) => {
                self.entries = self.edits.settings().setting_info();
                self.message = Some("No setting change.".to_owned());
                self.clamp();
                SettingsPanelOutcome::Consumed
            }
            Err(error) => {
                self.message = Some(error.message);
                SettingsPanelOutcome::Consumed
            }
        }
    }

    fn save_changes(&mut self) -> SettingsPanelOutcome {
        let changes = self.edits.changes();
        if changes.is_empty() {
            self.message = Some("No unsaved setting changes.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }
        SettingsPanelOutcome::Save(changes)
    }

    fn selected_entry(&self) -> Option<&SettingInfo> {
        self.entries.get(self.selected)
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
    } else if entry.key == "theme" {
        detail.push_str(
            " Enter edits a custom theme value; Left/Right opens the theme picker; Ctrl+S saves.",
        );
    } else {
        detail.push_str(" Enter edits/applies; Ctrl+S saves; Esc cancels an edit.");
    }
    detail
}

fn edit_options(entry: &SettingInfo) -> Vec<&'static str> {
    match entry.key {
        "theme" => vec!["plain", "odyssey", "odyssey-noir"],
        "visual" => vec!["off", "ambient"],
        "subpixel" => vec!["off", "rgb", "bgr"],
        "cursor_style" => vec!["block", "underline", "bar"],
        "cursor_blink" => vec!["auto", "on", "off"],
        _ => entry.options.to_vec(),
    }
}

fn number_step(key: &str) -> f32 {
    match key {
        "font_size" => 1.0,
        "text_gamma" => 0.1,
        "stem_darken" => 0.05,
        _ => 1.0,
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

        let _ = panel.handle_input(OverlayInput::Down);
        assert_eq!(panel.render_signature().selected, 1);
        let _ = panel.handle_input(OverlayInput::End);
        let end = panel.render_signature();
        assert_eq!(end.selected, end.entries.len() - 1);
        assert!(end.scroll > 0);
        let _ = panel.handle_input(OverlayInput::Home);
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

    #[test]
    fn bool_toggle_applies_and_revert_clears_diff() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "synthetic_styles");

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected bool toggle to apply");
        };
        assert!(!settings.synthetic_styles);
        assert_eq!(panel.render_signature().changed_count, 1);

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected bool revert to apply");
        };
        assert!(settings.synthetic_styles);
        assert_eq!(panel.render_signature().changed_count, 0);
    }

    #[test]
    fn save_reports_changes_and_success_clears_diff() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "visual");
        let SettingsPanelOutcome::Apply(_) = panel.handle_input(OverlayInput::Right) else {
            panic!("expected enum cycle to apply");
        };

        let SettingsPanelOutcome::Save(changes) = panel.handle_input(OverlayInput::Save) else {
            panic!("expected save request");
        };
        assert_eq!(changes.len(), 1);
        panel.save_succeeded(changes.len());
        let signature = panel.render_signature();
        assert_eq!(signature.changed_count, 0);
        assert!(
            signature
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Saved 1"))
        );
    }

    #[test]
    fn enum_cycle_applies_next_value() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "visual");

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Right) else {
            panic!("expected enum cycle to apply");
        };
        assert_eq!(settings.visual.as_str(), "ambient");
        assert_eq!(panel.render_signature().changed_count, 1);
    }

    #[test]
    fn theme_enter_starts_text_edit_for_user_theme_paths() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "theme");

        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(panel.render_signature().editing_key, Some("theme"));
    }

    #[test]
    fn theme_row_left_right_opens_picker() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "theme");

        assert_eq!(
            panel.handle_input(OverlayInput::Right),
            SettingsPanelOutcome::OpenThemePicker
        );
    }

    #[test]
    fn number_entry_uses_parser_clamp() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_size");
        let _ = panel.handle_input(OverlayInput::Activate);
        clear_edit_buffer(&mut panel);
        for ch in "200".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected number edit to apply");
        };
        assert_eq!(settings.font_size_px, crate::settings::MAX_FONT_SIZE_PX);
    }

    #[test]
    fn path_entry_commits_and_escape_cancels() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font");
        let _ = panel.handle_input(OverlayInput::Activate);
        clear_edit_buffer(&mut panel);
        for ch in "/tmp/TestMono.ttf".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        assert_eq!(panel.render_signature().editing_key, Some("font"));
        let _ = panel.handle_input(OverlayInput::Close);
        assert_eq!(panel.render_signature().editing_key, None);
        assert_eq!(panel.render_signature().changed_count, 0);

        let _ = panel.handle_input(OverlayInput::Activate);
        clear_edit_buffer(&mut panel);
        for ch in "/tmp/TestMono.ttf".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected path edit to apply");
        };
        assert_eq!(
            settings.font_path.as_deref(),
            Some(std::path::Path::new("/tmp/TestMono.ttf"))
        );
        assert_eq!(panel.render_signature().changed_count, 1);
    }

    #[test]
    fn invalid_edit_is_rejected_in_panel() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_size");
        let _ = panel.handle_input(OverlayInput::Activate);
        clear_edit_buffer(&mut panel);
        for ch in "nope".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }

        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::Consumed
        );
        let signature = panel.render_signature();
        assert_eq!(signature.changed_count, 0);
        assert!(
            signature
                .message
                .as_deref()
                .is_some_and(|message| message.contains("valid pixel size"))
        );
    }

    fn select_key(panel: &mut SettingsPanel, key: &str) {
        let index = panel
            .entries
            .iter()
            .position(|entry| entry.key == key)
            .unwrap();
        panel.set_selection(index);
    }

    fn clear_edit_buffer(panel: &mut SettingsPanel) {
        let len = panel
            .editing
            .as_ref()
            .map(|edit| edit.buffer.chars().count())
            .unwrap_or(0);
        for _ in 0..len {
            let _ = panel.handle_input(OverlayInput::Backspace);
        }
    }
}
