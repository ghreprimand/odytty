// SPDX-License-Identifier: GPL-3.0-only
use crate::core::{Attrs, Cell, Color, Snapshot};
use crate::input::Modifiers;
use crate::selection::CellPoint;
use crate::settings::Settings;
use crate::theme::{Srgb, Theme};

use unicode_width::UnicodeWidthChar;
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::settings_panel::{SettingsPanel, SettingsPanelOutcome, SettingsPanelSignature};
use super::theme_builder::{
    ThemeBuilder, ThemeBuilderLine, ThemeBuilderOutcome, ThemeBuilderSaveRequest,
    ThemeBuilderSignature,
};
use super::theme_picker::{ThemePicker, ThemePickerLine, ThemePickerOutcome, ThemePickerSignature};

#[derive(Debug, Clone)]
pub(super) struct OverlayUi {
    open: bool,
    mode: OverlayMode,
    settings: Settings,
    panel: SettingsPanel,
    theme_picker: ThemePicker,
    theme_builder: ThemeBuilder,
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
            mode: OverlayMode::Settings,
            settings: settings.clone(),
            panel: SettingsPanel::new(settings),
            theme_picker: ThemePicker::new(settings),
            theme_builder: ThemeBuilder::new(settings),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn refresh_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        self.panel.refresh(settings);
        self.theme_picker.refresh(settings);
        self.theme_builder.refresh(settings);
    }

    pub(super) fn apply_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        if self.mode == OverlayMode::Settings {
            self.panel.apply_settings(settings);
        }
    }

    pub(super) fn open_settings(&mut self) {
        // Defensive: never (re)enter with a stale slider drag armed (UX4-P2).
        self.panel.end_slider_drag();
        self.open = true;
        self.mode = OverlayMode::Settings;
    }

    pub(super) fn close(&mut self) {
        // Clear any in-progress slider drag on exit so a lost release (pointer
        // left the window / focus loss mid-drag) cannot leave it armed for the
        // next open — the P2 analogue of the P1 held-report_button cleanup.
        self.panel.end_slider_drag();
        self.open = false;
        self.mode = OverlayMode::Settings;
    }

    pub(super) fn open_theme_picker(&mut self, settings: &Settings) {
        // A mode switch also abandons any settings-panel slider drag (UX4-P2).
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        self.theme_picker.open(settings);
        self.mode = OverlayMode::ThemePicker;
        self.open = true;
    }

    pub(super) fn open_theme_builder(&mut self, settings: &Settings) {
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        self.theme_builder.open(settings);
        self.mode = OverlayMode::ThemeBuilder;
        self.open = true;
    }

    pub(super) fn toggle_settings(&mut self) {
        if self.open && self.mode == OverlayMode::Settings {
            self.close();
        } else {
            self.open_settings();
        }
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.mode {
            OverlayMode::ThemePicker => return self.handle_theme_picker_input(input),
            OverlayMode::ThemeBuilder => return self.handle_theme_builder_input(input),
            OverlayMode::Settings => {}
        }

        match input {
            OverlayInput::Close if !self.panel.is_editing() => OverlayOutcome::Close,
            input => settings_outcome(self.panel.handle_input(input)),
        }
    }

    /// Pointer entry point (UX4-P1), the mouse analogue of [`Self::handle_input`].
    /// `rect` is the live overlay geometry from [`overlay_rect`]. A press outside
    /// the panel dismisses exactly like Esc (per-mode: the theme picker restores
    /// its original theme). Inside the Settings panel a press is hit-tested to a
    /// row+zone and dispatched through the existing value seam; the theme
    /// picker/builder stay keyboard-driven for P1 (inside presses are inert).
    pub(super) fn handle_pointer(
        &mut self,
        pointer: OverlayPointer,
        rect: OverlayRect,
    ) -> OverlayOutcome {
        match pointer {
            OverlayPointer::Press { cell, button } => {
                if !rect.contains(cell) {
                    // Click-away = Esc; routes through the per-mode close path so
                    // the theme picker/builder restore their original theme.
                    return self.handle_input(OverlayInput::Close);
                }
                if self.mode != OverlayMode::Settings {
                    return OverlayOutcome::Consumed;
                }
                let Some(row_in_body) = cell.row.checked_sub(rect.body_top) else {
                    // The title row / top border: inside the box, inert.
                    return OverlayOutcome::Consumed;
                };
                let col_in_body = cell.column.saturating_sub(rect.body_left);
                let outcome = self.panel.handle_pointer_press(
                    rect.body_width,
                    rect.body_height,
                    row_in_body,
                    col_in_body,
                    button,
                );
                settings_outcome(outcome)
            }
            OverlayPointer::Move { cell } => {
                if self.mode != OverlayMode::Settings {
                    return OverlayOutcome::Consumed;
                }
                let col_in_body = cell.column.saturating_sub(rect.body_left);
                let outcome =
                    self.panel
                        .handle_pointer_drag(rect.body_width, rect.body_height, col_in_body);
                settings_outcome(outcome)
            }
            OverlayPointer::Release { .. } => {
                if self.mode == OverlayMode::Settings {
                    self.panel.end_slider_drag();
                }
                OverlayOutcome::Consumed
            }
            OverlayPointer::Wheel { lines } => {
                if self.mode == OverlayMode::Settings {
                    self.panel.scroll_lines(lines);
                }
                OverlayOutcome::Consumed
            }
        }
    }

    /// Whether a settings-panel slider drag is in progress (UX4-P2). The App
    /// gates per-move routing on this so non-drag hover stays cheap.
    pub(super) fn is_settings_dragging(&self) -> bool {
        self.mode == OverlayMode::Settings && self.panel.is_dragging()
    }

    /// Abandon any in-progress settings slider drag WITHOUT closing the overlay
    /// (UX4-P2). The App calls this on focus loss while the overlay stays open:
    /// a press may have armed a drag whose release is delivered to another
    /// window after an alt-tab, so without this the drag would survive and the
    /// next bare hover Move on focus regain would commit a phantom slider value
    /// — the overlay-stays-open analogue of the close/reopen lost-release case.
    /// No-op unless the Settings panel currently holds a drag.
    pub(super) fn cancel_settings_drag(&mut self) {
        if self.mode == OverlayMode::Settings {
            self.panel.end_slider_drag();
        }
    }

    /// Test seam (UX4-P2): absolute grid cells for the first visible slider's
    /// track ends (`track_left`, `track_right`) for a `columns`×`rows` grid, so
    /// a test can drive a real press/move/release through the App layer without
    /// reaching into private panel geometry.
    #[cfg(test)]
    pub(super) fn first_slider_track_cells(
        &self,
        columns: usize,
        rows: usize,
    ) -> Option<(CellPoint, CellPoint)> {
        let rect = overlay_rect(self, columns, rows)?;
        let (row, track_x0, track_w) = self
            .panel
            .first_slider_zone_for_test(rect.body_width, rect.body_height)?;
        let grid_row = rect.body_top + row;
        let left = CellPoint {
            row: grid_row,
            column: rect.body_left + track_x0,
        };
        let right = CellPoint {
            row: grid_row,
            column: rect.body_left + track_x0 + track_w - 1,
        };
        Some((left, right))
    }

    pub(super) fn save_succeeded(&mut self, changed: usize) {
        match self.mode {
            OverlayMode::Settings => self.panel.save_succeeded(changed),
            OverlayMode::ThemePicker => {
                self.theme_picker.save_succeeded(changed);
                self.close();
            }
            OverlayMode::ThemeBuilder => {}
        }
    }

    pub(super) fn save_failed(&mut self, message: String) {
        match self.mode {
            OverlayMode::Settings => self.panel.save_failed(message),
            OverlayMode::ThemePicker => self.theme_picker.save_failed(message),
            OverlayMode::ThemeBuilder => self.theme_builder.save_failed(message),
        }
    }

    pub(super) fn theme_builder_save_succeeded(
        &mut self,
        saved_name: &str,
        path: &std::path::Path,
        changed: usize,
    ) {
        self.theme_builder.save_succeeded(saved_name, path, changed);
        self.close();
    }

    pub(super) fn render_signature(&self) -> OverlayRenderSignature {
        OverlayRenderSignature {
            open: self.open,
            mode: self.mode,
            panel: self.panel.render_signature(),
            theme_picker: self.theme_picker.render_signature(),
            theme_builder: self.theme_builder.render_signature(),
        }
    }

    fn handle_theme_picker_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.theme_picker.handle_input(input) {
            ThemePickerOutcome::Consumed => OverlayOutcome::Consumed,
            ThemePickerOutcome::Preview(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                OverlayOutcome::ApplySettings(settings)
            }
            ThemePickerOutcome::Persist(changes) => OverlayOutcome::SaveSettings(changes),
            ThemePickerOutcome::OpenBuilder(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                self.theme_builder.open(&settings);
                self.mode = OverlayMode::ThemeBuilder;
                OverlayOutcome::ApplySettings(settings)
            }
            ThemePickerOutcome::Cancel(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                self.close();
                OverlayOutcome::ApplySettings(settings)
            }
        }
    }

    fn handle_theme_builder_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.theme_builder.handle_input(input) {
            ThemeBuilderOutcome::Consumed => OverlayOutcome::Consumed,
            ThemeBuilderOutcome::Preview(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                OverlayOutcome::ApplySettings(settings)
            }
            ThemeBuilderOutcome::Save(request) => OverlayOutcome::SaveTheme(request),
            ThemeBuilderOutcome::Cancel(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                self.close();
                OverlayOutcome::ApplySettings(settings)
            }
        }
    }

    fn settings_with_theme(&self, theme: Theme) -> Settings {
        let mut settings = self.settings.clone();
        settings.theme = theme;
        settings
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum OverlayOutcome {
    Consumed,
    Close,
    OpenThemePicker,
    OpenThemeBuilder,
    ApplySettings(Settings),
    SaveSettings(Vec<crate::settings::SettingEdit>),
    SaveTheme(ThemeBuilderSaveRequest),
}

/// Lift a [`SettingsPanelOutcome`] (from the keyboard or the pointer path) into
/// an [`OverlayOutcome`]. The single mapping shared by `handle_input`,
/// `handle_pointer` press, and `handle_pointer` drag so the three entry points
/// can never diverge.
fn settings_outcome(outcome: SettingsPanelOutcome) -> OverlayOutcome {
    match outcome {
        SettingsPanelOutcome::Consumed => OverlayOutcome::Consumed,
        SettingsPanelOutcome::Apply(settings) => OverlayOutcome::ApplySettings(settings),
        SettingsPanelOutcome::Save(changes) => OverlayOutcome::SaveSettings(changes),
        SettingsPanelOutcome::OpenThemePicker => OverlayOutcome::OpenThemePicker,
        SettingsPanelOutcome::OpenThemeBuilder => OverlayOutcome::OpenThemeBuilder,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayMode {
    Settings,
    ThemePicker,
    ThemeBuilder,
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

/// Which mouse button drove a pointer event into the overlay. Only the buttons
/// the overlay acts on are modeled (left = activate, right = reverse-cycle an
/// enum); middle and others never reach `handle_pointer` (the App layer drops
/// them while the overlay is open).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointerButton {
    Left,
    Right,
}

/// Pointer events delivered to the overlay (UX4-P1/P2), the mouse analogue of
/// [`OverlayInput`]. `Press` (click) and `Wheel` (free scroll) landed with P1;
/// `Move`/`Release` drive the UX4-P2 slider drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayPointer {
    /// A button went down at `cell` (grid coordinates over the visible grid,
    /// the same space the overlay is drawn in).
    Press {
        cell: CellPoint,
        button: PointerButton,
    },
    /// The pointer moved to `cell` while a slider drag is in progress (UX4-P2).
    /// The App only routes this during an active drag (drag state lives on the
    /// panel), so no "button held" flag is needed.
    Move { cell: CellPoint },
    /// A button was released at `cell` (UX4-P2): ends any slider drag.
    Release {
        cell: CellPoint,
        button: PointerButton,
    },
    /// A wheel notch translated to `lines` (positive = scroll toward later
    /// entries) over the open overlay.
    Wheel { lines: isize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OverlayRenderSignature {
    pub(super) open: bool,
    pub(super) mode: OverlayMode,
    pub(super) panel: SettingsPanelSignature,
    pub(super) theme_picker: ThemePickerSignature,
    pub(super) theme_builder: ThemeBuilderSignature,
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

/// Geometry of the open overlay panel, in terminal cells. The single source of
/// truth shared by rendering ([`apply_overlay`]) and pointer hit-testing
/// ([`overlay_rect`]) so a resize can never desync the two. Computed on demand
/// from the current grid dimensions; never cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OverlayRect {
    /// Outer panel box (includes border + title row).
    pub(super) left: usize,
    pub(super) top: usize,
    pub(super) width: usize,
    pub(super) height: usize,
    /// First body cell (inside the border, below the title).
    pub(super) body_left: usize,
    pub(super) body_top: usize,
    /// Body content extent (matches the args passed to `visible_lines`).
    pub(super) body_width: usize,
    pub(super) body_height: usize,
}

impl OverlayRect {
    /// Whether a grid cell falls inside the outer panel box.
    pub(super) fn contains(&self, cell: CellPoint) -> bool {
        cell.row >= self.top
            && cell.row < self.top + self.height
            && cell.column >= self.left
            && cell.column < self.left + self.width
    }
}

/// Compute the open overlay's cell geometry for a grid of `columns`×`rows`, or
/// `None` when the overlay is closed or the grid is empty. The math is the exact
/// rect [`apply_overlay`] draws into, so render and hit-test stay in lockstep.
pub(super) fn overlay_rect(
    overlay: &OverlayUi,
    columns: usize,
    rows: usize,
) -> Option<OverlayRect> {
    if !overlay.open || rows == 0 || columns == 0 {
        return None;
    }
    let width = match overlay.mode {
        OverlayMode::Settings => overlay.panel.desired_width(columns),
        OverlayMode::ThemePicker => overlay.theme_picker.desired_width(columns),
        OverlayMode::ThemeBuilder => overlay.theme_builder.desired_width(columns),
    }
    .max(36)
    .min(columns);
    let height = rows.min(22);
    let left = (columns - width) / 2;
    let top = (rows - height) / 2;
    Some(OverlayRect {
        left,
        top,
        width,
        height,
        body_left: left + 2,
        body_top: top + 2,
        body_width: width.saturating_sub(4),
        body_height: height.saturating_sub(3),
    })
}

pub(super) fn apply_overlay(snapshot: &mut Snapshot, overlay: &OverlayUi) {
    let Some(rect) = overlay_rect(
        overlay,
        snapshot.dimensions.columns,
        snapshot.dimensions.rows,
    ) else {
        return;
    };
    let rows = snapshot.dimensions.rows;
    let title = match overlay.mode {
        OverlayMode::Settings => "OdyTTY Settings",
        OverlayMode::ThemePicker => "OdyTTY Themes",
        OverlayMode::ThemeBuilder => "OdyTTY Theme Builder",
    };

    fill_rect(
        snapshot,
        rect.left,
        rect.top,
        rect.width,
        rect.height,
        panel_attrs(),
    );
    draw_border(
        snapshot,
        rect.left,
        rect.top,
        rect.width,
        rect.height,
        border_attrs(),
    );
    write_text(
        snapshot,
        rect.top,
        rect.left + 2,
        rect.width.saturating_sub(4),
        title,
        title_attrs(),
    );

    let body_width = rect.body_width;
    let lines = overlay.visible_lines(body_width, rect.body_height);
    for (row_index, row) in lines.iter().enumerate() {
        let y = rect.top + 2 + row_index;
        if y >= rect.top + rect.height.saturating_sub(1) || y >= rows {
            break;
        }
        let attrs = if row.focused {
            focused_attrs()
        } else {
            panel_attrs()
        };
        let text_column = if let Some(color) = row.swatch {
            draw_swatch(snapshot, y, rect.left + 2, color);
            rect.left + 5
        } else {
            rect.left + 2
        };
        let text_width = body_width.saturating_sub(text_column.saturating_sub(rect.left + 2));
        write_text(snapshot, y, text_column, text_width, &row.text, attrs);
    }
}

impl OverlayUi {
    fn visible_lines(&self, body_width: usize, body_height: usize) -> Vec<OverlayLine> {
        match self.mode {
            OverlayMode::Settings => self
                .panel
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::ThemePicker => self
                .theme_picker
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::ThemeBuilder => self
                .theme_builder
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlayLine {
    text: String,
    focused: bool,
    swatch: Option<Srgb>,
}

impl From<super::settings_panel::SettingsPanelLine> for OverlayLine {
    fn from(line: super::settings_panel::SettingsPanelLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
        }
    }
}

impl From<ThemePickerLine> for OverlayLine {
    fn from(line: ThemePickerLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
        }
    }
}

impl From<ThemeBuilderLine> for OverlayLine {
    fn from(line: ThemeBuilderLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: line.swatch,
        }
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

fn draw_swatch(snapshot: &mut Snapshot, row: usize, column: usize, color: Srgb) {
    if row >= snapshot.dimensions.rows || column + 1 >= snapshot.dimensions.columns {
        return;
    }
    let mut attrs = Attrs::default();
    attrs.background = Color::Rgb(color.0, color.1, color.2);
    write_cell(snapshot, row, column, ' ', attrs);
    write_cell(snapshot, row, column + 1, ' ', attrs);
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
        overlay.open_settings();
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
        overlay.open_settings();
        let before = overlay.render_signature();

        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Close
        );
        assert_eq!(overlay.render_signature(), before);
    }

    #[test]
    fn theme_picker_cancel_restores_original_theme_and_closes() {
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);

        assert!(matches!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::ApplySettings(_)
        ));
        let OverlayOutcome::ApplySettings(settings) = overlay.handle_input(OverlayInput::Close)
        else {
            panic!("expected restoration settings");
        };

        assert_eq!(settings.theme, crate::theme::Theme::ODYSSEY);
        assert!(!overlay.is_open());
    }

    // --- UX4-P1: pointer entry (handle_pointer / overlay_rect) ---

    fn theme_value_cell(rect: OverlayRect) -> CellPoint {
        // Row 0 of the body is the first group header ("Theme"); row 1 is the
        // theme value line. Any body column maps to its Value zone in P1.
        CellPoint {
            row: rect.body_top + 1,
            column: rect.body_left,
        }
    }

    #[test]
    fn pointer_press_outside_the_panel_dismisses_settings() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Top-left corner is well outside the centered panel.
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint { row: 0, column: 0 },
                button: PointerButton::Left,
            },
            rect,
        );
        assert_eq!(outcome, OverlayOutcome::Close);
    }

    #[test]
    fn pointer_press_outside_in_theme_picker_restores_and_closes() {
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // A move within the picker previews a different theme...
        assert!(matches!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::ApplySettings(_)
        ));
        // ...and a click outside dismisses exactly like Esc: restore + close.
        let OverlayOutcome::ApplySettings(restored) = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint { row: 0, column: 0 },
                button: PointerButton::Left,
            },
            rect,
        ) else {
            panic!("expected restoration settings on click-away");
        };
        assert_eq!(restored.theme, crate::theme::Theme::ODYSSEY);
        assert!(!overlay.is_open());
    }

    #[test]
    fn pointer_click_on_theme_value_opens_picker() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: theme_value_cell(rect),
                button: PointerButton::Left,
            },
            rect,
        );
        assert_eq!(outcome, OverlayOutcome::OpenThemePicker);
    }

    #[test]
    fn pointer_press_on_title_row_is_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // The title row sits above body_top but inside the panel box.
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
            },
            rect,
        );
        assert_eq!(outcome, OverlayOutcome::Consumed);
    }

    #[test]
    fn pointer_wheel_scrolls_settings_without_changing_selection() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let before = overlay.render_signature().panel;
        let outcome = overlay.handle_pointer(OverlayPointer::Wheel { lines: 4 }, rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        let after = overlay.render_signature().panel;
        assert!(after.scroll > before.scroll, "wheel scrolled the list");
        assert_eq!(after.selected, before.selected, "selection did not move");
    }

    // --- UX4-P2: slider drag through OverlayPointer Move/Release ---

    #[test]
    fn pointer_press_move_release_drives_a_slider_drag() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        // Scroll a numeric (slider) row into the capped 22-row panel window.
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Down);
        }
        let (left, right) = overlay
            .first_slider_track_cells(80, 24)
            .expect("a slider row is visible");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Press the far-right of the track → applies a value + arms the drag.
        assert!(matches!(
            overlay.handle_pointer(
                OverlayPointer::Press {
                    cell: right,
                    button: PointerButton::Left,
                },
                rect,
            ),
            OverlayOutcome::ApplySettings(_)
        ));
        assert!(overlay.is_settings_dragging(), "track press arms the drag");

        // Move to the far-left of the track → applies the min value.
        assert!(matches!(
            overlay.handle_pointer(OverlayPointer::Move { cell: left }, rect),
            OverlayOutcome::ApplySettings(_)
        ));

        // Release ends the drag; a later Move is inert.
        assert_eq!(
            overlay.handle_pointer(
                OverlayPointer::Release {
                    cell: left,
                    button: PointerButton::Left,
                },
                rect,
            ),
            OverlayOutcome::Consumed
        );
        assert!(!overlay.is_settings_dragging(), "release ends the drag");
        assert_eq!(
            overlay.handle_pointer(OverlayPointer::Move { cell: right }, rect),
            OverlayOutcome::Consumed,
            "no drag after release"
        );
    }

    #[test]
    fn a_lost_release_drag_cannot_survive_close_and_reopen() {
        // Regression (UX4-P2): if a release is never delivered (pointer leaves
        // the window / focus loss mid-drag), the armed drag must not persist
        // across close/reopen, or a later hover Move would drive a phantom drag.
        // Closing and any (re)open clear it.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Down);
        }
        let (_, right) = overlay
            .first_slider_track_cells(80, 24)
            .expect("a slider row is visible");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Arm a drag, then close WITHOUT a release (the lost-release case).
        let _ = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: right,
                button: PointerButton::Left,
            },
            rect,
        );
        assert!(
            overlay.is_settings_dragging(),
            "precondition: drag is armed"
        );
        overlay.close();
        assert!(!overlay.is_settings_dragging(), "close clears the drag");

        // Reopen and assert a bare Move does nothing (no phantom drag).
        overlay.open_settings();
        assert!(!overlay.is_settings_dragging(), "reopen has no stale drag");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        assert_eq!(
            overlay.handle_pointer(OverlayPointer::Move { cell: right }, rect),
            OverlayOutcome::Consumed,
            "hover after reopen is inert"
        );
    }

    #[test]
    fn focus_loss_drag_cancel_keeps_overlay_open_and_inert() {
        // Regression (UX4-P2): a press can arm a drag whose release is delivered
        // to ANOTHER window after an alt-tab. The overlay stays OPEN, so
        // close/reopen never runs — only `cancel_settings_drag` (driven by
        // `WindowEvent::Focused(false)`) clears the drag. Without it, a bare
        // hover Move on focus regain would commit a phantom slider value.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Down);
        }
        let (left, right) = overlay
            .first_slider_track_cells(80, 24)
            .expect("a slider row is visible");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Arm a drag at the right end of the track.
        let _ = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: right,
                button: PointerButton::Left,
            },
            rect,
        );
        assert!(
            overlay.is_settings_dragging(),
            "precondition: drag is armed"
        );

        // Focus loss WITHOUT a release and WITHOUT a close (the overlay-stays-
        // open lost-release case).
        overlay.cancel_settings_drag();
        assert!(overlay.is_open(), "focus loss does not close the overlay");
        assert!(
            !overlay.is_settings_dragging(),
            "focus loss cancels the drag"
        );

        // A bare hover Move (no held button) after focus regain is inert — no
        // phantom drag re-armed.
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        assert_eq!(
            overlay.handle_pointer(OverlayPointer::Move { cell: left }, rect),
            OverlayOutcome::Consumed,
            "hover after focus regain is inert"
        );
        assert!(
            !overlay.is_settings_dragging(),
            "hover did not re-arm the drag"
        );
    }
}
