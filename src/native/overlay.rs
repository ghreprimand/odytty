use crate::core::{Attrs, Cell, Color, Snapshot};
use crate::input::Modifiers;

use unicode_width::UnicodeWidthChar;
use winit::keyboard::{Key as WinitKey, NamedKey};

#[derive(Debug, Clone)]
pub(super) struct OverlayUi {
    open: bool,
    panel: OverlayPanel,
}

impl Default for OverlayUi {
    fn default() -> Self {
        Self {
            open: false,
            panel: OverlayPanel::demo(),
        }
    }
}

impl OverlayUi {
    pub(super) fn is_open(&self) -> bool {
        self.open
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
            OverlayInput::Close => OverlayOutcome::Close,
            input => {
                self.panel.handle_input(input);
                OverlayOutcome::Consumed
            }
        }
    }

    pub(super) fn render_signature(&self) -> OverlayRenderSignature {
        OverlayRenderSignature {
            open: self.open,
            panel: self.panel.render_signature(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayOutcome {
    Consumed,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayInput {
    Up,
    Down,
    Left,
    Right,
    Activate,
    Backspace,
    Close,
    Char(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OverlayRenderSignature {
    pub(super) open: bool,
    pub(super) panel: OverlayPanelSignature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OverlayPanelSignature {
    pub(super) focus: usize,
    pub(super) rows: Vec<OverlayRowSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OverlayRowSignature {
    Label(String),
    Text {
        label: String,
        value: String,
        cursor: usize,
    },
    Toggle {
        label: String,
        value: bool,
    },
    Stepper {
        label: String,
        value: i32,
    },
}

#[derive(Debug, Clone)]
struct OverlayPanel {
    title: String,
    focus: usize,
    rows: Vec<OverlayRow>,
}

impl OverlayPanel {
    fn demo() -> Self {
        Self {
            title: "OdyTTY Settings".to_owned(),
            focus: 1,
            rows: vec![
                OverlayRow::Label("UX1 overlay framework demo".to_owned()),
                OverlayRow::Text(TextField::new("Profile", "Default")),
                OverlayRow::Toggle(Toggle::new("Live preview", true)),
                OverlayRow::Stepper(Stepper::new("Intensity", 50, 0, 100, 5)),
            ],
        }
    }

    fn handle_input(&mut self, input: OverlayInput) {
        match input {
            OverlayInput::Up => self.focus_previous(),
            OverlayInput::Down => self.focus_next(),
            input => {
                if let Some(row) = self.rows.get_mut(self.focus) {
                    row.handle_input(input);
                }
            }
        }
    }

    fn focus_previous(&mut self) {
        self.move_focus(-1);
    }

    fn focus_next(&mut self) {
        self.move_focus(1);
    }

    fn move_focus(&mut self, delta: isize) {
        let focusable = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.focusable().then_some(index))
            .collect::<Vec<_>>();
        if focusable.is_empty() {
            self.focus = 0;
            return;
        }

        let current = focusable
            .iter()
            .position(|index| *index == self.focus)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(focusable.len() as isize) as usize;
        self.focus = focusable[next];
    }

    fn render_signature(&self) -> OverlayPanelSignature {
        OverlayPanelSignature {
            focus: self.focus,
            rows: self.rows.iter().map(OverlayRow::render_signature).collect(),
        }
    }
}

#[derive(Debug, Clone)]
enum OverlayRow {
    Label(String),
    Text(TextField),
    Toggle(Toggle),
    Stepper(Stepper),
}

impl OverlayRow {
    fn focusable(&self) -> bool {
        !matches!(self, Self::Label(_))
    }

    fn handle_input(&mut self, input: OverlayInput) {
        match self {
            Self::Label(_) => {}
            Self::Text(field) => field.handle_input(input),
            Self::Toggle(toggle) => toggle.handle_input(input),
            Self::Stepper(stepper) => stepper.handle_input(input),
        }
    }

    fn display(&self, focused: bool) -> String {
        let marker = if focused { ">" } else { " " };
        match self {
            Self::Label(text) => format!("  {text}"),
            Self::Text(field) => format!("{marker} {}: {}", field.label, field.display(focused)),
            Self::Toggle(toggle) => {
                let value = if toggle.value { "on" } else { "off" };
                format!("{marker} {}: [{value}]", toggle.label)
            }
            Self::Stepper(stepper) => {
                format!(
                    "{marker} {}: < {} >",
                    stepper.label,
                    stepper.value.clamp(stepper.min, stepper.max)
                )
            }
        }
    }

    fn render_signature(&self) -> OverlayRowSignature {
        match self {
            Self::Label(text) => OverlayRowSignature::Label(text.clone()),
            Self::Text(field) => OverlayRowSignature::Text {
                label: field.label.clone(),
                value: field.value.clone(),
                cursor: field.cursor,
            },
            Self::Toggle(toggle) => OverlayRowSignature::Toggle {
                label: toggle.label.clone(),
                value: toggle.value,
            },
            Self::Stepper(stepper) => OverlayRowSignature::Stepper {
                label: stepper.label.clone(),
                value: stepper.value,
            },
        }
    }
}

#[derive(Debug, Clone)]
struct TextField {
    label: String,
    value: String,
    cursor: usize,
}

impl TextField {
    fn new(label: &str, value: &str) -> Self {
        Self {
            label: label.to_owned(),
            value: value.to_owned(),
            cursor: value.chars().count(),
        }
    }

    fn handle_input(&mut self, input: OverlayInput) {
        match input {
            OverlayInput::Left => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            OverlayInput::Right => {
                self.cursor = (self.cursor + 1).min(self.value.chars().count());
            }
            OverlayInput::Backspace => self.backspace(),
            OverlayInput::Char(ch) if !ch.is_control() => self.insert(ch),
            _ => {}
        }
    }

    fn insert(&mut self, ch: char) {
        let byte_index = self.byte_index();
        self.value.insert(byte_index, ch);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let remove_at = self
            .value
            .char_indices()
            .nth(self.cursor - 1)
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.value.remove(remove_at);
        self.cursor -= 1;
    }

    fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.value.len())
    }

    fn display(&self, focused: bool) -> String {
        if !focused {
            return self.value.clone();
        }

        let mut display = String::new();
        for (index, ch) in self.value.chars().enumerate() {
            if index == self.cursor {
                display.push('|');
            }
            display.push(ch);
        }
        if self.cursor == self.value.chars().count() {
            display.push('|');
        }
        display
    }
}

#[derive(Debug, Clone)]
struct Toggle {
    label: String,
    value: bool,
}

impl Toggle {
    fn new(label: &str, value: bool) -> Self {
        Self {
            label: label.to_owned(),
            value,
        }
    }

    fn handle_input(&mut self, input: OverlayInput) {
        if matches!(
            input,
            OverlayInput::Activate | OverlayInput::Left | OverlayInput::Right
        ) {
            self.value = !self.value;
        }
    }
}

#[derive(Debug, Clone)]
struct Stepper {
    label: String,
    value: i32,
    min: i32,
    max: i32,
    step: i32,
}

impl Stepper {
    fn new(label: &str, value: i32, min: i32, max: i32, step: i32) -> Self {
        Self {
            label: label.to_owned(),
            value: value.clamp(min, max),
            min,
            max,
            step: step.max(1),
        }
    }

    fn handle_input(&mut self, input: OverlayInput) {
        match input {
            OverlayInput::Left => self.value = (self.value - self.step).max(self.min),
            OverlayInput::Right | OverlayInput::Activate => {
                self.value = (self.value + self.step).min(self.max);
            }
            _ => {}
        }
    }
}

pub(super) fn overlay_input_from_winit(
    logical: &WinitKey,
    mods: Modifiers,
) -> Option<OverlayInput> {
    match logical {
        WinitKey::Named(NamedKey::Escape) => Some(OverlayInput::Close),
        WinitKey::Named(NamedKey::ArrowUp) => Some(OverlayInput::Up),
        WinitKey::Named(NamedKey::ArrowDown) => Some(OverlayInput::Down),
        WinitKey::Named(NamedKey::ArrowLeft) => Some(OverlayInput::Left),
        WinitKey::Named(NamedKey::ArrowRight) => Some(OverlayInput::Right),
        WinitKey::Named(NamedKey::Enter) => Some(OverlayInput::Activate),
        WinitKey::Named(NamedKey::Backspace) => Some(OverlayInput::Backspace),
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

    let panel = &overlay.panel;
    let columns = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let desired_width = panel
        .rows
        .iter()
        .map(|row| row.display(false).chars().count() + 4)
        .chain(std::iter::once(panel.title.chars().count() + 4))
        .max()
        .unwrap_or(36)
        .max(36);
    let width = desired_width.min(columns);
    let height = (panel.rows.len() + 4).min(rows);
    let left = (columns - width) / 2;
    let top = (rows - height) / 2;

    fill_rect(snapshot, left, top, width, height, panel_attrs());
    draw_border(snapshot, left, top, width, height, border_attrs());
    write_text(
        snapshot,
        top,
        left + 2,
        width.saturating_sub(4),
        &panel.title,
        title_attrs(),
    );

    let body_width = width.saturating_sub(4);
    for (row_index, row) in panel.rows.iter().enumerate() {
        let y = top + 2 + row_index;
        if y >= top + height.saturating_sub(1) || y >= rows {
            break;
        }
        let focused = row_index == panel.focus;
        let attrs = if focused {
            focused_attrs()
        } else {
            panel_attrs()
        };
        write_text(
            snapshot,
            y,
            left + 2,
            body_width,
            &row.display(focused),
            attrs,
        );
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

    fn panel() -> OverlayPanel {
        OverlayPanel::demo()
    }

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
    fn list_navigation_wraps_over_focusable_rows() {
        let mut panel = panel();
        assert_eq!(panel.focus, 1);

        panel.handle_input(OverlayInput::Down);
        assert_eq!(panel.focus, 2);
        panel.handle_input(OverlayInput::Down);
        assert_eq!(panel.focus, 3);
        panel.handle_input(OverlayInput::Down);
        assert_eq!(panel.focus, 1);
        panel.handle_input(OverlayInput::Up);
        assert_eq!(panel.focus, 3);
    }

    #[test]
    fn text_field_edits_at_the_caret() {
        let mut field = TextField::new("Name", "ab");
        field.handle_input(OverlayInput::Left);
        field.handle_input(OverlayInput::Char('X'));
        assert_eq!(field.value, "aXb");
        assert_eq!(field.cursor, 2);

        field.handle_input(OverlayInput::Backspace);
        assert_eq!(field.value, "ab");
        assert_eq!(field.cursor, 1);
    }

    #[test]
    fn toggle_changes_only_on_activation_inputs() {
        let mut toggle = Toggle::new("Preview", false);
        toggle.handle_input(OverlayInput::Char('x'));
        assert!(!toggle.value);
        toggle.handle_input(OverlayInput::Activate);
        assert!(toggle.value);
        toggle.handle_input(OverlayInput::Left);
        assert!(!toggle.value);
    }

    #[test]
    fn stepper_clamps_to_bounds() {
        let mut stepper = Stepper::new("Scale", 5, 0, 10, 4);
        stepper.handle_input(OverlayInput::Right);
        assert_eq!(stepper.value, 9);
        stepper.handle_input(OverlayInput::Right);
        assert_eq!(stepper.value, 10);
        stepper.handle_input(OverlayInput::Left);
        stepper.handle_input(OverlayInput::Left);
        stepper.handle_input(OverlayInput::Left);
        assert_eq!(stepper.value, 0);
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
