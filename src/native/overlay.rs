use crate::core::{Attrs, Cell, Color, Snapshot};
use crate::input::Modifiers;
use crate::settings::Settings;

use unicode_width::UnicodeWidthChar;
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::settings_panel::{SettingsPanel, SettingsPanelOutcome, SettingsPanelSignature};

#[derive(Debug, Clone)]
pub(super) struct OverlayUi {
    open: bool,
    panel: SettingsPanel,
}

impl Default for OverlayUi {
    fn default() -> Self {
        Self::new(&Settings::default())
    }
}

impl OverlayUi {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            open: false,
            panel: SettingsPanel::new(settings),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn refresh_settings(&mut self, settings: &Settings) {
        self.panel.refresh(settings);
    }

    pub(super) fn apply_settings(&mut self, settings: &Settings) {
        self.panel.apply_settings(settings);
    }

    pub(super) fn open(&mut self) {
        self.open = true;
    }

    pub(super) fn close(&mut self) {
        self.open = false;
    }

    pub(super) fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Close if !self.panel.is_editing() => OverlayOutcome::Close,
            input => match self.panel.handle_input(input) {
                SettingsPanelOutcome::Consumed => OverlayOutcome::Consumed,
                SettingsPanelOutcome::Apply(settings) => OverlayOutcome::ApplySettings(settings),
                SettingsPanelOutcome::Save(changes) => OverlayOutcome::SaveSettings(changes),
            },
        }
    }

    pub(super) fn save_succeeded(&mut self, changed: usize) {
        self.panel.save_succeeded(changed);
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.panel.save_failed(message);
    }

    pub(super) fn render_signature(&self) -> OverlayRenderSignature {
        OverlayRenderSignature {
            open: self.open,
            panel: self.panel.render_signature(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum OverlayOutcome {
    Consumed,
    Close,
    ApplySettings(Settings),
    SaveSettings(Vec<crate::settings::SettingEdit>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayInput {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    Activate,
    Save,
    Backspace,
    Close,
    Char(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OverlayRenderSignature {
    pub(super) open: bool,
    pub(super) panel: SettingsPanelSignature,
}

pub(super) fn overlay_input_from_winit(
    logical: &WinitKey,
    mods: Modifiers,
) -> Option<OverlayInput> {
    match logical {
        WinitKey::Named(NamedKey::Escape) => Some(OverlayInput::Close),
        WinitKey::Named(NamedKey::ArrowUp) => Some(OverlayInput::Up),
        WinitKey::Named(NamedKey::ArrowDown) => Some(OverlayInput::Down),
        WinitKey::Named(NamedKey::PageUp) => Some(OverlayInput::PageUp),
        WinitKey::Named(NamedKey::PageDown) => Some(OverlayInput::PageDown),
        WinitKey::Named(NamedKey::Home) => Some(OverlayInput::Home),
        WinitKey::Named(NamedKey::End) => Some(OverlayInput::End),
        WinitKey::Named(NamedKey::ArrowLeft) => Some(OverlayInput::Left),
        WinitKey::Named(NamedKey::ArrowRight) => Some(OverlayInput::Right),
        WinitKey::Named(NamedKey::Enter) => Some(OverlayInput::Activate),
        WinitKey::Named(NamedKey::Backspace) => Some(OverlayInput::Backspace),
        WinitKey::Character(text) if mods.ctrl && !mods.alt && text.eq_ignore_ascii_case("s") => {
            Some(OverlayInput::Save)
        }
        WinitKey::Named(NamedKey::Space) if !mods.ctrl && !mods.alt => {
            Some(OverlayInput::Char(' '))
        }
        WinitKey::Character(text) if !mods.ctrl && !mods.alt => {
            let mut chars = text.chars();
            let ch = chars.next()?;
            chars.next().is_none().then_some(OverlayInput::Char(ch))
        }
        _ => None,
    }
}

pub(super) fn apply_overlay(snapshot: &mut Snapshot, overlay: &OverlayUi) {
    if !overlay.open || snapshot.dimensions.rows == 0 || snapshot.dimensions.columns == 0 {
        return;
    }

    let columns = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let title = "OdyTTY Settings";
    let width = overlay.panel.desired_width(columns).max(36).min(columns);
    let height = rows.min(22);
    let left = (columns - width) / 2;
    let top = (rows - height) / 2;

    fill_rect(snapshot, left, top, width, height, panel_attrs());
    draw_border(snapshot, left, top, width, height, border_attrs());
    write_text(
        snapshot,
        top,
        left + 2,
        width.saturating_sub(4),
        title,
        title_attrs(),
    );

    let body_width = width.saturating_sub(4);
    let body_height = height.saturating_sub(3);
    for (row_index, row) in overlay
        .panel
        .visible_lines(body_width, body_height)
        .iter()
        .enumerate()
    {
        let y = top + 2 + row_index;
        if y >= top + height.saturating_sub(1) || y >= rows {
            break;
        }
        let attrs = if row.focused {
            focused_attrs()
        } else {
            panel_attrs()
        };
        write_text(snapshot, y, left + 2, body_width, &row.text, attrs);
    }
}

fn fill_rect(
    snapshot: &mut Snapshot,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    attrs: Attrs,
) {
    for row in top..top + height {
        let offset = row * snapshot.dimensions.columns;
        for column in left..left + width {
            snapshot.cells[offset + column] = Cell::new(' ', attrs);
        }
    }
}

fn draw_border(
    snapshot: &mut Snapshot,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    attrs: Attrs,
) {
    if width < 2 || height < 2 {
        return;
    }

    let right = left + width - 1;
    let bottom = top + height - 1;
    write_cell(snapshot, top, left, '+', attrs);
    write_cell(snapshot, top, right, '+', attrs);
    write_cell(snapshot, bottom, left, '+', attrs);
    write_cell(snapshot, bottom, right, '+', attrs);
    for column in left + 1..right {
        write_cell(snapshot, top, column, '-', attrs);
        write_cell(snapshot, bottom, column, '-', attrs);
    }
    for row in top + 1..bottom {
        write_cell(snapshot, row, left, '|', attrs);
        write_cell(snapshot, row, right, '|', attrs);
    }
}

fn write_text(
    snapshot: &mut Snapshot,
    row: usize,
    column: usize,
    max_width: usize,
    text: &str,
    attrs: Attrs,
) {
    if row >= snapshot.dimensions.rows || column >= snapshot.dimensions.columns || max_width == 0 {
        return;
    }

    let mut x = column;
    let right = (column + max_width).min(snapshot.dimensions.columns);
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if width > 2 || x + width > right {
            break;
        }
        write_cell(snapshot, row, x, ch, attrs);
        if width == 2 {
            write_cell(snapshot, row, x + 1, ' ', attrs);
        }
        x += width;
    }
}

fn write_cell(snapshot: &mut Snapshot, row: usize, column: usize, ch: char, attrs: Attrs) {
    let offset = row * snapshot.dimensions.columns + column;
    snapshot.cells[offset] = Cell::new(ch, attrs);
}

fn panel_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Default;
    attrs.background = Color::Default;
    attrs.set_inverse(true);
    attrs
}

fn border_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.foreground = Color::Indexed(14);
    attrs
}

fn title_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.foreground = Color::Indexed(15);
    attrs
}

fn focused_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(0);
    attrs.background = Color::Indexed(11);
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Dimensions, Position};

    fn snapshot(columns: usize, rows: usize) -> Snapshot {
        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position::default(),
            cursor_visible: true,
            colors: crate::core::DynamicColors::default(),
            cells: vec![Cell::new('.', Attrs::default()); columns * rows],
        }
    }

    #[test]
    fn input_mapping_covers_settings_panel_navigation() {
        assert_eq!(
            overlay_input_from_winit(&WinitKey::Named(NamedKey::PageDown), Modifiers::default()),
            Some(OverlayInput::PageDown)
        );
        assert_eq!(
            overlay_input_from_winit(&WinitKey::Named(NamedKey::Home), Modifiers::default()),
            Some(OverlayInput::Home)
        );
        assert_eq!(
            overlay_input_from_winit(&WinitKey::Named(NamedKey::End), Modifiers::default()),
            Some(OverlayInput::End)
        );
        assert_eq!(
            overlay_input_from_winit(
                &WinitKey::Character("s".into()),
                Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                }
            ),
            Some(OverlayInput::Save)
        );
    }

    #[test]
    fn overlay_draws_into_snapshot_copy_only() {
        let mut overlay = OverlayUi::default();
        overlay.open();
        let original = snapshot(40, 10);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &overlay);

        assert_eq!(original.cells[0].ch, '.');
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
        assert!(rendered.cells.iter().any(|cell| cell.ch == '>'));
    }

    #[test]
    fn escape_requests_close_without_mutating_state() {
        let mut overlay = OverlayUi::default();
        overlay.open();
        let before = overlay.render_signature();

        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Close
        );
        assert_eq!(overlay.render_signature(), before);
    }
}
