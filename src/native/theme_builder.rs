// SPDX-License-Identifier: GPL-3.0-only
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::settings::Settings;
use crate::theme::{Appearance, Srgb, Theme, ThemeSpec, contrast_ratio, relative_luminance};

use super::overlay::OverlayInput;

#[derive(Debug, Clone)]
pub(super) struct ThemeBuilder {
    original: Theme,
    spec: ThemeSpec,
    selected: usize,
    scroll: usize,
    editing: Option<EditMode>,
    message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ThemeBuilderOutcome {
    Consumed,
    Preview(Theme),
    Save(ThemeBuilderSaveRequest),
    Cancel(Theme),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ThemeBuilderSaveRequest {
    pub(super) name: String,
    pub(super) spec: ThemeSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThemeBuilderSignature {
    pub(super) original: &'static str,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) editing: Option<ThemeBuilderEditSignature>,
    pub(super) message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ThemeBuilderEditSignature {
    Color { field: &'static str, buffer: String },
    Name { buffer: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThemeBuilderLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) swatch: Option<Srgb>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditMode {
    Color {
        field: ThemeField,
        previous: Srgb,
        buffer: String,
    },
    Name {
        buffer: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeField {
    Foreground,
    Background,
    Clear,
    Cursor,
    Selection,
    Search,
    Border,
    Inactive,
    Palette(usize),
}

const FIELDS: [ThemeField; 24] = [
    ThemeField::Foreground,
    ThemeField::Background,
    ThemeField::Clear,
    ThemeField::Cursor,
    ThemeField::Selection,
    ThemeField::Search,
    ThemeField::Border,
    ThemeField::Inactive,
    ThemeField::Palette(0),
    ThemeField::Palette(1),
    ThemeField::Palette(2),
    ThemeField::Palette(3),
    ThemeField::Palette(4),
    ThemeField::Palette(5),
    ThemeField::Palette(6),
    ThemeField::Palette(7),
    ThemeField::Palette(8),
    ThemeField::Palette(9),
    ThemeField::Palette(10),
    ThemeField::Palette(11),
    ThemeField::Palette(12),
    ThemeField::Palette(13),
    ThemeField::Palette(14),
    ThemeField::Palette(15),
];

impl ThemeBuilder {
    pub(super) fn new(settings: &Settings) -> Self {
        Self::from_theme(settings.theme)
    }

    pub(super) fn open(&mut self, settings: &Settings) {
        *self = Self::from_theme(settings.theme);
        self.message = Some(
            "Clone active theme. Enter edits hex, Left/Right nudges, Ctrl+S saves.".to_owned(),
        );
    }

    pub(super) fn refresh(&mut self, settings: &Settings) {
        if self.editing.is_none() {
            *self = Self::from_theme(settings.theme);
        }
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ThemeBuilderOutcome {
        if self.editing.is_some() {
            return self.handle_editing_input(input);
        }

        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-6),
            OverlayInput::PageDown => self.move_selection(6),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => self.set_selection(FIELDS.len().saturating_sub(1)),
            OverlayInput::Left => return self.nudge_selected(-1),
            OverlayInput::Right => return self.nudge_selected(1),
            OverlayInput::Activate => self.begin_color_edit(),
            OverlayInput::Save => self.begin_name_edit(),
            OverlayInput::Char('n') | OverlayInput::Char('N') => self.begin_name_edit(),
            OverlayInput::Close => return ThemeBuilderOutcome::Cancel(self.original),
            _ => {}
        }

        ThemeBuilderOutcome::Consumed
    }

    pub(super) fn save_succeeded(&mut self, saved_name: &str, path: &Path, changed: usize) {
        self.spec.name = saved_name.to_owned();
        self.original = self.preview_theme();
        self.message = Some(format!(
            "Saved {saved_name} to {} and odytty.conf ({changed} setting change).",
            path.display()
        ));
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.message = Some(format!("Save failed: {message}"));
    }

    pub(super) fn render_signature(&self) -> ThemeBuilderSignature {
        ThemeBuilderSignature {
            original: self.original.name,
            selected: self.selected,
            scroll: self.scroll,
            editing: self.editing.as_ref().map(|editing| match editing {
                EditMode::Color { field, buffer, .. } => ThemeBuilderEditSignature::Color {
                    field: field.key(),
                    buffer: buffer.clone(),
                },
                EditMode::Name { buffer } => ThemeBuilderEditSignature::Name {
                    buffer: buffer.clone(),
                },
            }),
            message: self.message.clone(),
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        72.min(columns)
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ThemeBuilderLine> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        lines.push(ThemeBuilderLine {
            text: ellipsize(
                "  Theme builder - Enter hex, Left/Right nudge, Ctrl+S save, Esc cancel",
                body_width,
            ),
            focused: false,
            swatch: None,
        });

        let ratio = contrast_ratio(self.spec.foreground, self.spec.background);
        lines.push(ThemeBuilderLine {
            text: ellipsize(
                &format!(
                    "  name={}  fg/bg contrast={ratio:.2}{}",
                    self.spec.name,
                    if ratio < 4.0 { " below 4.0" } else { "" }
                ),
                body_width,
            ),
            focused: false,
            swatch: None,
        });

        if let Some(message) = self.message.as_deref() {
            for wrapped in wrap_words(message, body_width.saturating_sub(4)) {
                if lines.len() >= body_height {
                    return lines;
                }
                lines.push(ThemeBuilderLine {
                    text: format!("    {wrapped}"),
                    focused: false,
                    swatch: None,
                });
            }
        }

        self.push_preview_lines(&mut lines, body_width, body_height);

        for (index, field) in FIELDS.iter().enumerate().skip(self.scroll) {
            if lines.len() >= body_height {
                break;
            }
            let focused = index == self.selected;
            let marker = if focused { ">" } else { " " };
            let color = self.color(*field);
            let mut value = hex(color);
            if let Some(EditMode::Color {
                field: editing_field,
                buffer,
                ..
            }) = self.editing.as_ref()
                && editing_field == field
            {
                value = format!("[{buffer}]");
            }
            let text = format!("{marker} {:<12} {value}", field.label());
            lines.push(ThemeBuilderLine {
                text: ellipsize(&text, body_width),
                focused,
                swatch: Some(color),
            });
        }

        lines.truncate(body_height);
        lines
    }

    fn from_theme(theme: Theme) -> Self {
        let mut spec = ThemeSpec::from_theme(&theme);
        spec.name = suggested_name(theme.name);
        spec.appearance = if relative_luminance(theme.background) > 0.18 {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        Self {
            original: theme,
            spec,
            selected: 0,
            scroll: 0,
            editing: None,
            message: None,
        }
    }

    fn handle_editing_input(&mut self, input: OverlayInput) -> ThemeBuilderOutcome {
        match input {
            OverlayInput::Close => {
                if let Some(EditMode::Color {
                    field, previous, ..
                }) = self.editing.take()
                {
                    self.set_color(field, previous);
                    self.message = Some(format!("Cancelled edit for {}.", field.label()));
                    return ThemeBuilderOutcome::Preview(self.preview_theme());
                }
                if matches!(self.editing, Some(EditMode::Name { .. })) {
                    self.editing = None;
                    self.message = Some("Cancelled save name.".to_owned());
                }
                ThemeBuilderOutcome::Consumed
            }
            OverlayInput::Activate | OverlayInput::Save => match self.editing.take() {
                Some(EditMode::Color { field, buffer, .. }) => match parse_hex(&buffer) {
                    Some(color) => {
                        self.set_color(field, color);
                        self.message = Some(format!("Applied {} = {}.", field.label(), hex(color)));
                        ThemeBuilderOutcome::Preview(self.preview_theme())
                    }
                    None => {
                        self.editing = Some(EditMode::Color {
                            field,
                            previous: self.color(field),
                            buffer,
                        });
                        self.message = Some("Use #rgb or #rrggbb.".to_owned());
                        ThemeBuilderOutcome::Consumed
                    }
                },
                Some(EditMode::Name { buffer }) => self.save_request(buffer),
                None => ThemeBuilderOutcome::Consumed,
            },
            OverlayInput::Backspace => {
                match self.editing.as_mut() {
                    Some(EditMode::Color { buffer, .. }) | Some(EditMode::Name { buffer }) => {
                        buffer.pop();
                    }
                    None => {}
                }
                ThemeBuilderOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                match self.editing.as_mut() {
                    Some(EditMode::Color { buffer, .. }) => {
                        if ch == '#' || ch.is_ascii_hexdigit() {
                            buffer.push(ch.to_ascii_lowercase());
                        }
                    }
                    Some(EditMode::Name { buffer }) => {
                        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                            buffer.push(ch.to_ascii_lowercase());
                        }
                    }
                    None => {}
                }
                ThemeBuilderOutcome::Consumed
            }
            _ => ThemeBuilderOutcome::Consumed,
        }
    }

    fn begin_color_edit(&mut self) {
        let field = FIELDS[self.selected];
        let color = self.color(field);
        self.editing = Some(EditMode::Color {
            field,
            previous: color,
            buffer: hex(color),
        });
        self.message = Some(format!(
            "Editing {}: type #rrggbb, Enter applies, Esc cancels.",
            field.label()
        ));
    }

    fn begin_name_edit(&mut self) {
        self.editing = Some(EditMode::Name {
            buffer: self.spec.name.clone(),
        });
        self.message =
            Some("Save as: type a theme name, Enter writes .theme, Esc cancels.".to_owned());
    }

    fn save_request(&mut self, name: String) -> ThemeBuilderOutcome {
        let name = name.trim().to_ascii_lowercase();
        if !valid_theme_name(&name) {
            self.editing = Some(EditMode::Name { buffer: name });
            self.message =
                Some("Use letters, numbers, dashes, or underscores; no paths.".to_owned());
            return ThemeBuilderOutcome::Consumed;
        }
        let mut spec = self.spec.clone();
        spec.name = name.clone();
        spec.appearance = if relative_luminance(spec.background) > 0.18 {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        self.spec = spec.clone();
        ThemeBuilderOutcome::Save(ThemeBuilderSaveRequest { name, spec })
    }

    fn nudge_selected(&mut self, direction: i16) -> ThemeBuilderOutcome {
        let field = FIELDS[self.selected];
        let color = self.color(field);
        let nudged = nudge(color, direction * 5);
        self.set_color(field, nudged);
        self.message = Some(format!("{} = {}.", field.label(), hex(nudged)));
        ThemeBuilderOutcome::Preview(self.preview_theme())
    }

    fn preview_theme(&self) -> Theme {
        self.spec.to_theme_with_name("custom")
    }

    fn color(&self, field: ThemeField) -> Srgb {
        match field {
            ThemeField::Foreground => self.spec.foreground,
            ThemeField::Background => self.spec.background,
            ThemeField::Clear => self.spec.clear,
            ThemeField::Cursor => self.spec.cursor,
            ThemeField::Selection => self.spec.selection,
            ThemeField::Search => self.spec.search,
            ThemeField::Border => self.spec.border,
            ThemeField::Inactive => self.spec.inactive,
            ThemeField::Palette(index) => self.spec.palette[index],
        }
    }

    fn set_color(&mut self, field: ThemeField, color: Srgb) {
        match field {
            ThemeField::Foreground => self.spec.foreground = color,
            ThemeField::Background => self.spec.background = color,
            ThemeField::Clear => self.spec.clear = color,
            ThemeField::Cursor => self.spec.cursor = color,
            ThemeField::Selection => self.spec.selection = color,
            ThemeField::Search => self.spec.search = color,
            ThemeField::Border => self.spec.border = color,
            ThemeField::Inactive => self.spec.inactive = color,
            ThemeField::Palette(index) => self.spec.palette[index] = color,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let next = self.selected as isize + delta;
        self.set_selection(next.clamp(0, FIELDS.len().saturating_sub(1) as isize) as usize);
    }

    fn set_selection(&mut self, selected: usize) {
        self.selected = selected.min(FIELDS.len().saturating_sub(1));
        self.clamp();
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(FIELDS.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let visible_slack = 8;
        if self.selected >= self.scroll + visible_slack {
            self.scroll = self.selected.saturating_sub(visible_slack - 1);
        }
        self.scroll = self.scroll.min(FIELDS.len().saturating_sub(1));
    }

    fn push_preview_lines(
        &self,
        lines: &mut Vec<ThemeBuilderLine>,
        body_width: usize,
        body_height: usize,
    ) {
        if lines.len() >= body_height {
            return;
        }
        lines.push(ThemeBuilderLine {
            text: ellipsize(
                "  Preview: Default  Black Red Green Yellow Blue Magenta Cyan White",
                body_width,
            ),
            focused: false,
            swatch: Some(self.spec.foreground),
        });
        if lines.len() < body_height {
            lines.push(ThemeBuilderLine {
                text: ellipsize("  Selection  Cursor  Search  Border  Inactive", body_width),
                focused: false,
                swatch: Some(self.spec.selection),
            });
        }
    }
}

impl ThemeField {
    fn key(self) -> &'static str {
        match self {
            ThemeField::Foreground => "foreground",
            ThemeField::Background => "background",
            ThemeField::Clear => "clear",
            ThemeField::Cursor => "cursor",
            ThemeField::Selection => "selection",
            ThemeField::Search => "search",
            ThemeField::Border => "border",
            ThemeField::Inactive => "inactive",
            ThemeField::Palette(0) => "color0",
            ThemeField::Palette(1) => "color1",
            ThemeField::Palette(2) => "color2",
            ThemeField::Palette(3) => "color3",
            ThemeField::Palette(4) => "color4",
            ThemeField::Palette(5) => "color5",
            ThemeField::Palette(6) => "color6",
            ThemeField::Palette(7) => "color7",
            ThemeField::Palette(8) => "color8",
            ThemeField::Palette(9) => "color9",
            ThemeField::Palette(10) => "color10",
            ThemeField::Palette(11) => "color11",
            ThemeField::Palette(12) => "color12",
            ThemeField::Palette(13) => "color13",
            ThemeField::Palette(14) => "color14",
            ThemeField::Palette(15) => "color15",
            ThemeField::Palette(_) => "color",
        }
    }

    fn label(self) -> &'static str {
        self.key()
    }
}

pub(super) fn save_theme_to_dir(
    theme_dir: &Path,
    request: &ThemeBuilderSaveRequest,
) -> io::Result<PathBuf> {
    fs::create_dir_all(theme_dir)?;
    let path = theme_dir.join(format!("{}.theme", request.name));
    fs::write(&path, request.spec.serialize())?;
    Ok(path)
}

pub(super) fn user_theme_dir_for_config(config_path: &Path) -> Option<PathBuf> {
    config_path.parent().map(|parent| parent.join("themes"))
}

fn suggested_name(name: &str) -> String {
    let base = name.trim().strip_suffix(".theme").unwrap_or(name.trim());
    let mut out = String::new();
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | ' ') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "custom-theme".to_owned()
    } else if out.ends_with("-custom") {
        out.to_owned()
    } else {
        format!("{out}-custom")
    }
}

fn valid_theme_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('.')
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn parse_hex(value: &str) -> Option<Srgb> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.is_empty() || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => Some((
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        )),
        3 => {
            let expand = |slice: &str| u8::from_str_radix(slice, 16).ok().map(|v| v * 17);
            Some((
                expand(&hex[0..1])?,
                expand(&hex[1..2])?,
                expand(&hex[2..3])?,
            ))
        }
        _ => None,
    }
}

fn hex((r, g, b): Srgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn nudge((r, g, b): Srgb, amount: i16) -> Srgb {
    let channel = |value: u8| (value as i16 + amount).clamp(0, 255) as u8;
    (channel(r), channel(g), channel(b))
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

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-theme-builder-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn edit_state_machine_previews_color_changes() {
        let mut builder = ThemeBuilder::new(&Settings::default());
        assert_eq!(
            builder.handle_input(OverlayInput::Activate),
            ThemeBuilderOutcome::Consumed
        );
        for _ in 0..7 {
            builder.handle_input(OverlayInput::Backspace);
        }
        for ch in "#123456".chars() {
            builder.handle_input(OverlayInput::Char(ch));
        }

        let ThemeBuilderOutcome::Preview(theme) = builder.handle_input(OverlayInput::Activate)
        else {
            panic!("expected preview");
        };

        assert_eq!(theme.foreground, (0x12, 0x34, 0x56));
        assert_eq!(builder.render_signature().editing, None);
    }

    #[test]
    fn serialize_round_trips_to_valid_theme() {
        let mut builder = ThemeBuilder::new(&Settings::default());
        builder.handle_input(OverlayInput::Save);
        for _ in 0..builder
            .render_signature()
            .editing
            .as_ref()
            .and_then(|edit| match edit {
                ThemeBuilderEditSignature::Name { buffer } => Some(buffer.len()),
                _ => None,
            })
            .unwrap()
        {
            builder.handle_input(OverlayInput::Backspace);
        }
        for ch in "my-theme".chars() {
            builder.handle_input(OverlayInput::Char(ch));
        }
        let ThemeBuilderOutcome::Save(request) = builder.handle_input(OverlayInput::Activate)
        else {
            panic!("expected save request");
        };

        let reparsed = ThemeSpec::parse(&request.spec.serialize(), |m| panic!("warn: {m}"));
        assert_eq!(reparsed, request.spec);
        assert_eq!(request.name, "my-theme");
    }

    #[test]
    fn save_writes_to_injected_temp_dir() {
        let dir = temp_dir("save");
        let mut spec = ThemeSpec::from_theme(&Theme::ODYSSEY);
        spec.name = "test-theme".to_owned();
        let request = ThemeBuilderSaveRequest {
            name: "test-theme".to_owned(),
            spec: spec.clone(),
        };

        let path = save_theme_to_dir(&dir, &request).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let reparsed = ThemeSpec::parse(&contents, |m| panic!("warn: {m}"));

        assert_eq!(path, dir.join("test-theme.theme"));
        assert_eq!(reparsed, spec);
        assert!(!contents.contains("/home/"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancel_restores_original_theme() {
        let settings = Settings {
            theme: Theme::ODYSSEY,
            ..Settings::default()
        };
        let mut builder = ThemeBuilder::new(&settings);
        let ThemeBuilderOutcome::Preview(_) = builder.handle_input(OverlayInput::Right) else {
            panic!("expected preview");
        };

        let ThemeBuilderOutcome::Cancel(theme) = builder.handle_input(OverlayInput::Close) else {
            panic!("expected cancel");
        };

        assert_eq!(theme, Theme::ODYSSEY);
    }
}
