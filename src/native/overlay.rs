// SPDX-License-Identifier: GPL-3.0-only
use crate::core::{Attrs, Cell, Color, Snapshot};
use crate::input::Modifiers;
use crate::selection::CellPoint;
use crate::settings::{KeyChord, Settings};
use crate::theme::{Srgb, Theme};

use unicode_width::UnicodeWidthChar;
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::context_menu_ui::{
    ContextMenuItem, ContextMenuOutcome, ContextMenuSignature, ContextMenuUi,
};
use super::font_picker::{FontPicker, FontPickerLine, FontPickerOutcome, FontPickerSignature};
use super::key_remap_ui::{KeyRemapLine, KeyRemapOutcome, KeyRemapSignature, KeyRemapUi};
use super::onboarding::{OnboardingLine, OnboardingPanel, OnboardingSignature};
use super::session::SessionToken;
use super::settings_panel::{
    SettingsLevel, SettingsPanel, SettingsPanelOutcome, SettingsPanelSignature,
};
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
    font_picker: FontPicker,
    key_remap: KeyRemapUi,
    onboarding: OnboardingPanel,
    context_menu: ContextMenuUi,
    /// Set when a `SaveAndClose` outcome arrives from the settings panel (dirty
    /// close prompt). On the next `save_succeeded` call for Settings mode, the
    /// overlay closes itself after recording the save (SETTINGS-REDESIGN §7).
    close_after_save: bool,
    picker_return: Option<PickerReturn>,
    /// True while `ThemeBuilder` is the active mode AND it was entered from
    /// `ThemePicker` (via `ThemePickerOutcome::OpenBuilder`). Esc / back-button
    /// in this state navigates back to `ThemePicker` rather than closing the
    /// whole overlay. False for the standalone / Settings-launched path.
    builder_from_picker: bool,
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
            font_picker: FontPicker::new(settings),
            key_remap: KeyRemapUi::new(settings),
            onboarding: OnboardingPanel::new(settings),
            context_menu: ContextMenuUi::new(),
            close_after_save: false,
            picker_return: None,
            builder_from_picker: false,
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
        self.key_remap.refresh(settings);
        self.onboarding.refresh(settings);
    }

    pub(super) fn apply_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        // The panel's own commits already updated `self.panel.edits`; rebasing
        // them here would erase its dirty state. Picker-origin changes arrive
        // through the same apply path while another overlay mode is active, so
        // adopt those as the panel's clean baseline instead.
        if self.mode == OverlayMode::Settings {
            self.panel.apply_settings(settings);
        } else {
            self.panel.rebase_onto_external(settings);
        }
    }

    pub(super) fn open_settings(&mut self) {
        // Defensive no-op for settings steppers; kept with the
        // shared close/switch cleanup path.
        self.panel.end_slider_drag();
        self.open = true;
        self.mode = OverlayMode::Settings;
    }

    pub(super) fn close(&mut self) {
        // Clear any in-progress overlay drag on exit so a lost release (pointer
        // left the window / focus loss mid-drag) cannot leave it armed for the
        // next open.
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.open = false;
        self.mode = OverlayMode::Settings;
        self.close_after_save = false;
        self.picker_return = None;
        self.builder_from_picker = false;
    }

    pub(super) fn open_theme_picker(&mut self, settings: &Settings) {
        // A mode switch also runs the shared pointer-capture cleanup path.
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
        // Standalone / Settings-launched path: back-button closes, not ThemePicker.
        self.builder_from_picker = false;
    }

    /// Open the font-family picker (FONT-PICKER). Runs a fresh metadata scan on
    /// open (typically <100 ms). Backs the picker with the grouped inventory
    /// (from [`crate::text::font_families_grouped`]): the always-present
    /// **Bundled Fonts** (Victor Mono, JetBrains Mono) and the host's distinct
    /// real monospace **System Fonts**.
    pub(super) fn open_font_picker(&mut self, settings: &Settings) {
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        let groups = crate::text::font_families_grouped();
        self.font_picker.open(settings, groups);
        self.mode = OverlayMode::FontPicker;
        self.open = true;
    }

    pub(super) fn open_key_bindings(&mut self, settings: &Settings) {
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        self.key_remap.open(settings);
        self.mode = OverlayMode::KeyBindings;
        self.open = true;
    }

    /// Open the first-run onboarding card (ONBOARD). Called once at startup by
    /// `App::new` when the config file does not yet exist (or the
    /// `ODYTTY_ONBOARDING` override is set). Refreshes the card from the current
    /// settings so the shortcut hints reflect the live bindings (D-OB-3).
    pub(super) fn open_onboarding(&mut self) {
        self.panel.end_slider_drag();
        self.onboarding.refresh(&self.settings);
        self.mode = OverlayMode::Onboarding;
        self.open = true;
    }

    /// Open the right-click context menu (IN2) at `spawn` (a grid cell), with
    /// the item-enabled snapshot the App computed from the live selection /
    /// clipboard. Unlike the other openers this does NOT clear the selection —
    /// the Copy item needs it — so the App must not route through
    /// `reset_pointer_state_for_overlay` here.
    pub(super) fn open_context_menu(
        &mut self,
        spawn: CellPoint,
        copy: bool,
        cut: bool,
        paste: bool,
        delete: bool,
        rename_target: Option<SessionToken>,
    ) {
        self.panel.end_slider_drag();
        self.context_menu
            .open(spawn, copy, cut, paste, delete, rename_target);
        self.mode = OverlayMode::ContextMenu;
        self.open = true;
    }

    /// Open the close-confirmation dialog (CLOSE-CONFIRM). Called from the App's
    /// `CloseRequested` handler when `confirm_close` is on and a foreground job
    /// is running. Idempotent: starts with `close()` so a repeated close request
    /// (some window managers fire it twice) cannot stack dialogs (TRAP-3).
    pub(super) fn open_confirm_close(&mut self) {
        self.close();
        self.mode = OverlayMode::ConfirmClose;
        self.open = true;
    }

    /// Keyboard contract for the close-confirmation dialog (CLOSE-CONFIRM).
    /// Enter or Y confirms the close (`ForceClose`); Esc or N cancels (closes the
    /// dialog, the window stays open); every other key is swallowed so nothing
    /// leaks to the PTY behind the modal. The `Close` arm must emit `Close`, not
    /// `ForceClose`, so dismissing never exits (TRAP-2).
    fn handle_confirm_close_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('y') | OverlayInput::Char('Y') => {
                self.close();
                OverlayOutcome::ForceClose
            }
            OverlayInput::Close | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                OverlayOutcome::Close
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Whether the context menu is the active overlay mode (IN2). The App uses
    /// this to route bare hover Moves to the menu for hover-to-focus, alongside
    /// the slider-drag gate.
    pub(super) fn is_context_menu(&self) -> bool {
        self.open && self.mode == OverlayMode::ContextMenu
    }

    /// Whether the close-confirmation dialog is the active overlay mode
    /// (CLOSE-CONFIRM). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(super) fn is_confirm_close(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmClose
    }

    /// Lift a [`ContextMenuOutcome`] into an [`OverlayOutcome`] (IN2). An
    /// `Activate` closes the menu and emits the matching App-side action; the
    /// App runs it after the overlay has closed.
    fn apply_context_menu_outcome(&mut self, outcome: ContextMenuOutcome) -> OverlayOutcome {
        match outcome {
            ContextMenuOutcome::Consumed => OverlayOutcome::Consumed,
            ContextMenuOutcome::Close => OverlayOutcome::Close,
            ContextMenuOutcome::Activate(item) => {
                self.close();
                match item {
                    ContextMenuItem::Copy => OverlayOutcome::ContextMenuCopy,
                    ContextMenuItem::Cut => OverlayOutcome::ContextMenuCut,
                    ContextMenuItem::Paste => OverlayOutcome::ContextMenuPaste,
                    ContextMenuItem::Delete => OverlayOutcome::ContextMenuDelete,
                    ContextMenuItem::SelectAll => OverlayOutcome::ContextMenuSelectAll,
                    ContextMenuItem::NewTab => OverlayOutcome::ContextMenuNewTab,
                    ContextMenuItem::RenameTab => {
                        if let Some(target) = self.context_menu.rename_target() {
                            OverlayOutcome::ContextMenuRenameTab(target)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    ContextMenuItem::CloseTab => OverlayOutcome::ContextMenuCloseTab,
                    ContextMenuItem::Settings => OverlayOutcome::ContextMenuSettings,
                }
            }
        }
    }

    fn handle_context_menu_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.context_menu.handle_input(input);
        self.apply_context_menu_outcome(outcome)
    }

    /// Whether the key-remap modal is armed to capture a raw chord (KB-REMAP).
    /// The App gates its chord-capture bypass on this: `true` ONLY when the
    /// KeyBindings mode is active AND a row/conflict is awaiting a chord, so
    /// normal overlay navigation is never diverted (R1).
    pub(super) fn is_capturing_chord(&self) -> bool {
        self.mode == OverlayMode::KeyBindings && self.key_remap.is_capturing_chord()
    }

    /// Deliver a raw captured chord to the key-remap modal (KB-REMAP). Only
    /// called by the App while [`Self::is_capturing_chord`] is `true`.
    pub(super) fn deliver_chord(&mut self, chord: Option<KeyChord>) -> OverlayOutcome {
        let outcome = self.key_remap.deliver_chord(chord);
        self.apply_key_remap_outcome(outcome)
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
            OverlayMode::FontPicker => return self.handle_font_picker_input(input),
            OverlayMode::KeyBindings => return self.handle_key_remap_input(input),
            OverlayMode::Onboarding => return self.handle_onboarding_input(input),
            OverlayMode::ContextMenu => return self.handle_context_menu_input(input),
            OverlayMode::ConfirmClose => return self.handle_confirm_close_input(input),
            OverlayMode::Settings => {}
        }

        // All inputs route through the panel. The panel decides whether to Close
        // (via SettingsPanelOutcome::Close at Level 1 clean) or consume Esc for
        // dirty-close, search, edit cancel, or Level-2 → Level-1 back navigation.
        let outcome = self.panel.handle_input(input);
        self.map_settings_outcome(outcome)
    }

    /// Pointer entry point (UX4-P1), the mouse analogue of [`Self::handle_input`].
    /// `rect` is the live overlay geometry from [`overlay_rect`]. A press outside
    /// the panel dismisses exactly like Esc (per-mode: the theme picker restores
    /// its original theme). Inside the Settings panel a press is hit-tested to a
    /// row+zone and dispatched through the existing value seam. Inside the theme
    /// builder a press routes to role/channel focus and starts a slider drag on
    /// the focused-channel track (U2 Step 2/3); inside the theme/font pickers,
    /// title-area presses route to Close (the `←` back affordance) and body
    /// presses are inert.
    pub(super) fn handle_pointer(
        &mut self,
        pointer: OverlayPointer,
        rect: OverlayRect,
    ) -> OverlayOutcome {
        match pointer {
            OverlayPointer::Press {
                cell,
                button,
                x_in_body,
            } => {
                if !rect.contains(cell) {
                    // Click-away = Esc; routes through the per-mode close path so
                    // the theme picker/builder restore their original theme.
                    return self.handle_input(OverlayInput::Close);
                }
                let Some(row_in_body) = cell.row.checked_sub(rect.body_top) else {
                    if self.settings_title_back_hit(cell, rect)
                        || self.picker_title_back_hit(cell, rect)
                    {
                        return self.handle_input(OverlayInput::Close);
                    }
                    // The title row / top border outside the back affordance:
                    // inside the box, inert.
                    return OverlayOutcome::Consumed;
                };
                let col_in_body = cell.column.saturating_sub(rect.body_left);
                match self.mode {
                    OverlayMode::Settings => {
                        let o = self.panel.handle_pointer_press(
                            rect.body_width,
                            rect.body_height,
                            row_in_body,
                            col_in_body,
                            button,
                            x_in_body,
                        );
                        self.map_settings_outcome(o)
                    }
                    OverlayMode::ThemeBuilder => {
                        let outcome = self.theme_builder.handle_pointer_press(
                            rect.body_width,
                            rect.body_height,
                            row_in_body,
                            col_in_body,
                            button,
                        );
                        self.apply_builder_outcome(outcome)
                    }
                    OverlayMode::ContextMenu => {
                        let outcome = self.context_menu.handle_press(row_in_body, button);
                        self.apply_context_menu_outcome(outcome)
                    }
                    OverlayMode::ThemePicker
                    | OverlayMode::FontPicker
                    | OverlayMode::KeyBindings
                    | OverlayMode::Onboarding
                    | OverlayMode::ConfirmClose => OverlayOutcome::Consumed,
                }
            }
            OverlayPointer::Move { cell, x_in_body } => {
                let col_in_body = cell.column.saturating_sub(rect.body_left);
                match self.mode {
                    OverlayMode::Settings => {
                        let o = self.panel.handle_pointer_drag(
                            rect.body_width,
                            rect.body_height,
                            col_in_body,
                            x_in_body,
                        );
                        self.map_settings_outcome(o)
                    }
                    OverlayMode::ThemeBuilder => {
                        let outcome = self.theme_builder.handle_pointer_drag(
                            rect.body_width,
                            rect.body_height,
                            col_in_body,
                        );
                        self.apply_builder_outcome(outcome)
                    }
                    OverlayMode::ContextMenu => {
                        // Hover-to-focus (D-IN2-6): move focus to the item under
                        // the pointer; off-item (border) hovers leave it as is.
                        let row_in_body = cell.row.checked_sub(rect.body_top);
                        self.context_menu.handle_hover(row_in_body);
                        OverlayOutcome::Consumed
                    }
                    OverlayMode::ThemePicker
                    | OverlayMode::FontPicker
                    | OverlayMode::KeyBindings
                    | OverlayMode::Onboarding
                    | OverlayMode::ConfirmClose => OverlayOutcome::Consumed,
                }
            }
            OverlayPointer::Release { .. } => {
                match self.mode {
                    OverlayMode::Settings => self.panel.end_slider_drag(),
                    OverlayMode::ThemeBuilder => self.theme_builder.end_channel_drag(),
                    OverlayMode::ThemePicker
                    | OverlayMode::FontPicker
                    | OverlayMode::KeyBindings
                    | OverlayMode::Onboarding
                    | OverlayMode::ContextMenu
                    | OverlayMode::ConfirmClose => {}
                }
                OverlayOutcome::Consumed
            }
            OverlayPointer::Wheel { lines } => {
                match self.mode {
                    OverlayMode::Settings => self.panel.scroll_lines(lines),
                    OverlayMode::ThemeBuilder => self.theme_builder.scroll_lines(lines),
                    OverlayMode::KeyBindings => self.key_remap.scroll_lines(lines),
                    OverlayMode::FontPicker => {
                        self.font_picker.handle_input(if lines < 0 {
                            OverlayInput::Up
                        } else {
                            OverlayInput::Down
                        });
                    }
                    OverlayMode::ThemePicker => {
                        self.theme_picker.handle_input(if lines < 0 {
                            OverlayInput::Up
                        } else {
                            OverlayInput::Down
                        });
                    }
                    OverlayMode::Onboarding
                    | OverlayMode::ContextMenu
                    | OverlayMode::ConfirmClose => {}
                }
                OverlayOutcome::Consumed
            }
        }
    }

    /// Whether an overlay drag is in progress. Settings steppers never capture
    /// pointer motion, so this is only true for modes that still drag, such as
    /// the theme builder channel slider.
    pub(super) fn is_settings_dragging(&self) -> bool {
        match self.mode {
            OverlayMode::Settings => self.panel.is_dragging(),
            OverlayMode::ThemeBuilder => self.theme_builder.is_dragging(),
            OverlayMode::ThemePicker
            | OverlayMode::FontPicker
            | OverlayMode::KeyBindings
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::ConfirmClose => false,
        }
    }

    /// Abandon any in-progress overlay drag WITHOUT closing the overlay. The App
    /// calls this on focus loss while the overlay stays open; no-op unless the
    /// active mode currently holds a pointer-captured drag.
    pub(super) fn cancel_settings_drag(&mut self) {
        match self.mode {
            OverlayMode::Settings => self.panel.end_slider_drag(),
            OverlayMode::ThemeBuilder => self.theme_builder.end_channel_drag(),
            OverlayMode::ThemePicker
            | OverlayMode::FontPicker
            | OverlayMode::KeyBindings
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::ConfirmClose => {}
        }
    }

    fn settings_title_back_hit(&self, cell: CellPoint, rect: OverlayRect) -> bool {
        self.mode == OverlayMode::Settings
            && matches!(
                self.panel.current_level(),
                SettingsLevel::SectionDetail { .. }
            )
            // The ← arrow is drawn at rect.top (the title/border row); also
            // accept rect.top + 1 (the gap row) for a forgiving click target.
            && cell.row >= rect.top
            && cell.row < rect.body_top
            && cell.column >= rect.body_left
            && cell.column < rect.body_left + 3
    }

    /// True if `cell` is in the `←` back-arrow hit zone of the theme or font
    /// picker/builder/key-bindings title row. All four modes show `← … (Esc =
    /// back)` in their title, so this hit-test is unconditional on those modes.
    fn picker_title_back_hit(&self, cell: CellPoint, rect: OverlayRect) -> bool {
        matches!(
            self.mode,
            OverlayMode::ThemePicker
                | OverlayMode::FontPicker
                | OverlayMode::ThemeBuilder
                | OverlayMode::KeyBindings
        )
        // Accept the title row and the gap row (rect.top through body_top - 1)
        // for a forgiving click target matching the Settings back-arrow zone.
        && cell.row >= rect.top
        && cell.row < rect.body_top
        && cell.column >= rect.body_left
        && cell.column < rect.body_left + 3
    }

    /// Test seam (UX4-P2): absolute grid cells for the first visible numeric
    /// stepper's down/up buttons for a `columns`×`rows` grid, so a test can
    /// drive real clicks through the App layer without reaching into private
    /// panel geometry.
    #[cfg(test)]
    pub(super) fn first_stepper_button_cells(
        &self,
        columns: usize,
        rows: usize,
    ) -> Option<(CellPoint, CellPoint)> {
        let rect = overlay_rect(self, columns, rows)?;
        let (row, down_x0, up_x0) = self
            .panel
            .first_stepper_zone_for_test(rect.body_width, rect.body_height)?;
        let grid_row = rect.body_top + row;
        let down = CellPoint {
            row: grid_row,
            column: rect.body_left + down_x0,
        };
        let up = CellPoint {
            row: grid_row,
            column: rect.body_left + up_x0,
        };
        Some((down, up))
    }

    pub(super) fn save_succeeded(&mut self, changed: usize) {
        match self.mode {
            OverlayMode::Settings => {
                self.panel.save_succeeded(changed);
                // If this save came from the SaveAndClose dirty-close prompt,
                // close the overlay now that the save has succeeded.
                if self.close_after_save {
                    self.close(); // resets close_after_save via close()
                }
            }
            OverlayMode::ThemePicker => {
                self.theme_picker.save_succeeded(changed);
                if self.picker_return.is_some() {
                    self.return_to_settings_panel();
                } else {
                    self.close();
                }
            }
            OverlayMode::FontPicker => {
                // FONT-PICKER-STAY-OPEN: applying a font (Enter) live-applies
                // and saves it, then KEEPS the picker open so the user can keep
                // cycling fonts. font_picker.save_succeeded adopts the applied
                // family as the new baseline (self.original), so it shows the
                // "current" marker and Esc no longer reverts past it. Esc
                // (Cancel) is what closes the picker / returns to the panel.
                self.font_picker.save_succeeded(changed);
            }
            OverlayMode::ThemeBuilder => {}
            // KB-REMAP stays open after a save so the user can keep editing; the
            // modal reports the saved count and adopts the persisted bindings as
            // its new restore baseline.
            OverlayMode::KeyBindings => self.key_remap.save_succeeded(changed),
            // The onboarding card, context menu, and close dialog have no save
            // path of their own.
            OverlayMode::Onboarding | OverlayMode::ContextMenu | OverlayMode::ConfirmClose => {}
        }
    }

    pub(super) fn save_failed(&mut self, message: String) {
        match self.mode {
            OverlayMode::Settings => self.panel.save_failed(message),
            OverlayMode::ThemePicker => self.theme_picker.save_failed(message),
            OverlayMode::ThemeBuilder => self.theme_builder.save_failed(message),
            OverlayMode::FontPicker => self.font_picker.save_failed(message),
            OverlayMode::KeyBindings => self.key_remap.save_failed(message),
            OverlayMode::Onboarding | OverlayMode::ContextMenu | OverlayMode::ConfirmClose => {}
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
            font_picker: self.font_picker.render_signature(),
            key_remap: self.key_remap.render_signature(),
            onboarding: self.onboarding.render_signature(),
            context_menu: self.context_menu.render_signature(),
        }
    }

    fn handle_theme_picker_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.theme_picker.handle_input(input) {
            ThemePickerOutcome::Consumed => OverlayOutcome::Consumed,
            ThemePickerOutcome::Preview(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            ThemePickerOutcome::Persist(changes) => OverlayOutcome::SaveSettings(changes),
            ThemePickerOutcome::OpenBuilder(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                self.theme_builder.open(&settings);
                self.mode = OverlayMode::ThemeBuilder;
                // Remember we came from ThemePicker so Esc / back navigates
                // back to it rather than closing the overlay entirely.
                self.builder_from_picker = true;
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            ThemePickerOutcome::Cancel(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                if self.picker_return.is_some() {
                    self.return_to_settings_panel();
                } else {
                    self.close();
                }
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
        }
    }

    fn handle_theme_builder_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.theme_builder.handle_input(input);
        self.apply_builder_outcome(outcome)
    }

    fn handle_font_picker_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.font_picker.handle_input(input) {
            FontPickerOutcome::Consumed => OverlayOutcome::Consumed,
            FontPickerOutcome::Persist(changes) => OverlayOutcome::SaveSettings(changes),
            FontPickerOutcome::Cancel(_original) => {
                // Font was never changed in-memory (no live preview), so just
                // close the picker — no ApplySettings needed.
                if self.picker_return.is_some() {
                    self.return_to_settings_panel();
                    OverlayOutcome::Consumed
                } else {
                    self.close();
                    OverlayOutcome::Close
                }
            }
        }
    }

    /// Lift a [`ThemeBuilderOutcome`] (from the keyboard or the pointer path)
    /// into an [`OverlayOutcome`] — the single mapping shared by
    /// `handle_theme_builder_input` and the builder branch of `handle_pointer`,
    /// so the two entry points can never diverge.
    fn apply_builder_outcome(&mut self, outcome: ThemeBuilderOutcome) -> OverlayOutcome {
        match outcome {
            ThemeBuilderOutcome::Consumed => OverlayOutcome::Consumed,
            ThemeBuilderOutcome::Preview(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            ThemeBuilderOutcome::Save(request) => OverlayOutcome::SaveTheme(request),
            ThemeBuilderOutcome::Cancel(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                // If the builder was opened from ThemePicker, Esc / back-button
                // navigates back to it rather than closing the whole overlay.
                // For the standalone / Settings-launched path, close as before.
                if self.builder_from_picker {
                    self.builder_from_picker = false;
                    self.mode = OverlayMode::ThemePicker;
                } else {
                    self.close();
                }
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
        }
    }

    fn handle_key_remap_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.key_remap.handle_input(input);
        self.apply_key_remap_outcome(outcome)
    }

    /// Lift a [`KeyRemapOutcome`] (from the browsing keyboard path or the
    /// chord-capture path) into an [`OverlayOutcome`] — the single mapping
    /// shared by `handle_key_remap_input` and `deliver_chord` so the two entry
    /// points can never diverge.
    fn apply_key_remap_outcome(&mut self, outcome: KeyRemapOutcome) -> OverlayOutcome {
        match outcome {
            KeyRemapOutcome::Consumed => OverlayOutcome::Consumed,
            KeyRemapOutcome::Preview(settings) => {
                self.settings = settings.clone();
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            KeyRemapOutcome::Save(changes) => OverlayOutcome::SaveSettings(changes),
            KeyRemapOutcome::Cancel(settings) => {
                self.settings = settings.clone();
                // KeyBindings is always opened from Settings; Esc / back-button
                // navigates back to the Settings panel rather than closing the
                // whole overlay (consistent with the pickers' return path).
                self.return_to_settings_panel();
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
        }
    }

    /// Handle a key in the first-run onboarding card (ONBOARD). The card is a
    /// static info panel: Enter / Esc / Space dismiss it (close the overlay);
    /// every other key is swallowed so nothing leaks to the PTY behind it. The
    /// terminal stays live throughout — dismissal is the only state change.
    fn handle_onboarding_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Close | OverlayInput::Activate | OverlayInput::Char(' ') => {
                OverlayOutcome::CloseOnboarding
            }
            _ => OverlayOutcome::Consumed,
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
    /// Dismiss the first-run onboarding card. Like `Close`, but the App also
    /// persists a first-run marker (ensures `odytty.conf` exists) so the welcome
    /// card does not reshow on the next launch — dismissal alone otherwise
    /// writes nothing, and onboarding's gate is purely "does the config exist".
    CloseOnboarding,
    OpenThemePicker,
    OpenThemeBuilder,
    OpenKeyBindings,
    /// Open the font-family picker (FONT-PICKER). Emitted from the Fonts
    /// section's `font_family` row. The picker overlay is sequenced in the
    /// FONT-PICKER packet; for now `apply_overlay_outcome` handles it as a stub.
    OpenFontPicker,
    /// Boxed because `Settings` is by far the largest payload across this
    /// short-lived outcome enum; boxing keeps the enum small to move and clears
    /// the `large_enum_variant` lint as the settings surface grows.
    ApplySettings(Box<Settings>),
    SaveSettings(Vec<crate::settings::SettingEdit>),
    SaveTheme(ThemeBuilderSaveRequest),
    /// Run the right-click menu's Copy / Paste / Select All action (IN2). The
    /// overlay has already closed itself by the time these are emitted; the App
    /// dispatches them to the existing copy/paste shortcuts and `handle_select_all`.
    ContextMenuCopy,
    ContextMenuCut,
    ContextMenuPaste,
    ContextMenuDelete,
    ContextMenuSelectAll,
    ContextMenuNewTab,
    ContextMenuRenameTab(SessionToken),
    ContextMenuCloseTab,
    /// Open the settings panel from the context menu (D-IN2-SETTINGS). The
    /// overlay has already closed itself; the App opens the settings panel
    /// through the existing toggle path.
    ContextMenuSettings,
    /// The user confirmed the close-confirmation dialog (CLOSE-CONFIRM): close
    /// the window. The overlay has already closed itself by the time this is
    /// emitted; the App sets its `pending_exit` flag and exits the event loop on
    /// the same turn (the outcome can't reach `ActiveEventLoop` directly).
    ForceClose,
}

impl OverlayUi {
    /// Map a [`SettingsPanelOutcome`] into an [`OverlayOutcome`]. This is the
    /// single shared mapping for `handle_input`, `handle_pointer` press, and
    /// `handle_pointer` drag, so the three entry points can never diverge.
    /// `SaveAndClose` sets `close_after_save` so `save_succeeded` closes the
    /// overlay after the App persists the changes.
    fn map_settings_outcome(&mut self, outcome: SettingsPanelOutcome) -> OverlayOutcome {
        match outcome {
            SettingsPanelOutcome::Consumed => OverlayOutcome::Consumed,
            SettingsPanelOutcome::Apply(settings) => {
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            SettingsPanelOutcome::Save(changes) => OverlayOutcome::SaveSettings(changes),
            SettingsPanelOutcome::OpenThemePicker => {
                self.picker_return = Some(PickerReturn {
                    level: self.panel.current_level(),
                });
                OverlayOutcome::OpenThemePicker
            }
            SettingsPanelOutcome::OpenThemeBuilder => OverlayOutcome::OpenThemeBuilder,
            SettingsPanelOutcome::OpenKeyBindings => OverlayOutcome::OpenKeyBindings,
            SettingsPanelOutcome::OpenFontPicker => {
                self.picker_return = Some(PickerReturn {
                    level: self.panel.current_level(),
                });
                OverlayOutcome::OpenFontPicker
            }
            SettingsPanelOutcome::Close => OverlayOutcome::Close,
            SettingsPanelOutcome::DiscardAndClose => OverlayOutcome::Close,
            SettingsPanelOutcome::SaveAndClose(edits) => {
                // Save the changes; after save_succeeded, close the overlay.
                self.close_after_save = true;
                OverlayOutcome::SaveSettings(edits)
            }
        }
    }

    fn return_to_settings_panel(&mut self) {
        if let Some(PickerReturn { level }) = self.picker_return.take() {
            self.panel.set_level(level);
        }
        self.mode = OverlayMode::Settings;
        self.open = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayMode {
    Settings,
    ThemePicker,
    ThemeBuilder,
    /// Font-family picker (FONT-PICKER). Lists monospace families from the
    /// host's font search dirs; type-to-filter + Enter saves `font_family`.
    FontPicker,
    KeyBindings,
    Onboarding,
    /// Right-click context menu (IN2). Spawns at the pointer cell rather than
    /// centered; carries no title bar.
    ContextMenu,
    /// Close-confirmation dialog (CLOSE-CONFIRM). A centered, static two-line
    /// modal shown when a close is requested while a foreground job is running;
    /// Enter/Y confirms (emits [`OverlayOutcome::ForceClose`]), Esc/N cancels.
    ConfirmClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PickerReturn {
    level: SettingsLevel,
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
    /// Cycle the theme builder's focused OKLCH channel (U2 Step 2/3). Ignored by
    /// the settings panel and theme picker (their `handle_input` default arms
    /// drop it).
    Tab,
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
/// [`OverlayInput`]. `Press` drives clicks, `Wheel` drives free scroll, and
/// `Move`/`Release` drive modes that still capture pointer motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum OverlayPointer {
    /// A button went down at `cell` (grid coordinates over the visible grid,
    /// the same space the overlay is drawn in). `x_in_body` is the fractional
    /// body-relative x from physical pixel data; `None` in tests / headless
    /// mode.
    Press {
        cell: CellPoint,
        button: PointerButton,
        x_in_body: Option<f32>,
    },
    /// The pointer moved to `cell` while an overlay drag is in progress (UX4-P2).
    /// `x_in_body` is the fractional body-relative x from physical pixel data;
    /// `None` in tests / headless mode.
    Move {
        cell: CellPoint,
        x_in_body: Option<f32>,
    },
    /// A button was released at `cell` (UX4-P2): ends any overlay drag.
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
    pub(super) font_picker: FontPickerSignature,
    pub(super) key_remap: KeyRemapSignature,
    pub(super) onboarding: OnboardingSignature,
    pub(super) context_menu: ContextMenuSignature,
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
        WinitKey::Named(NamedKey::Tab) if !mods.ctrl && !mods.alt => Some(OverlayInput::Tab),
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
/// Fixed body width (cells) for the close-confirmation dialog (CLOSE-CONFIRM).
/// Wide enough for the longest static line plus the panel border inset; the
/// `.max(36)` floor in [`overlay_rect`] keeps small grids sane.
const CONFIRM_CLOSE_WIDTH: usize = 52;

pub(super) fn overlay_rect(
    overlay: &OverlayUi,
    columns: usize,
    rows: usize,
) -> Option<OverlayRect> {
    if !overlay.open || rows == 0 || columns == 0 {
        return None;
    }
    // The context menu spawns at the pointer cell (not centered) and is sized to
    // its three items, so it bypasses the centered-panel geometry below (IN2).
    if overlay.mode == OverlayMode::ContextMenu {
        return Some(overlay.context_menu.rect(columns, rows));
    }
    let width = match overlay.mode {
        OverlayMode::Settings => overlay.panel.desired_width(columns),
        OverlayMode::ThemePicker => overlay.theme_picker.desired_width(columns),
        OverlayMode::ThemeBuilder => overlay.theme_builder.desired_width(columns),
        OverlayMode::FontPicker => overlay.font_picker.desired_width(columns),
        OverlayMode::KeyBindings => overlay.key_remap.desired_width(columns),
        OverlayMode::Onboarding => overlay.onboarding.desired_width(columns),
        // Unreachable: handled by the early return above.
        OverlayMode::ContextMenu => overlay.context_menu.menu_width(),
        // Static two-line dialog; the `.max(36)` floor below gives it room and
        // the body text fits comfortably (CLOSE-CONFIRM).
        OverlayMode::ConfirmClose => CONFIRM_CLOSE_WIDTH,
    }
    .max(36)
    .min(columns);
    // Target ~80 % of rows so the panel is tall enough to show many settings
    // at once; still capped at `rows - 2` to leave at least one terminal row
    // above/below, and floored at 22 for small terminals (OVERLAY-SIZE).
    let height = (rows * 4 / 5).max(22).min(rows.saturating_sub(2)).max(1);
    let left = (columns - width) / 2;
    let top = (rows.saturating_sub(height)) / 2;
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

pub(super) fn apply_overlay(snapshot: &mut Snapshot, overlay: &mut OverlayUi) {
    let Some(rect) = overlay_rect(
        overlay,
        snapshot.dimensions.columns,
        snapshot.dimensions.rows,
    ) else {
        return;
    };
    // The context menu has its own no-title layout (IN2); dispatch and return.
    if overlay.mode == OverlayMode::ContextMenu {
        apply_context_menu(snapshot, overlay, rect);
        return;
    }
    let rows = snapshot.dimensions.rows;
    // The Settings title is dynamic (shows level, editing state, search query).
    // ThemePicker, FontPicker, ThemeBuilder, and KeyBindings show a ← back
    // affordance so mouse users can click to return to the parent screen.
    let title: String = match overlay.mode {
        OverlayMode::Settings => overlay.panel.panel_title(),
        OverlayMode::ThemePicker => "\u{2190} OdyTTY Themes  (Esc = back)".to_owned(),
        OverlayMode::ThemeBuilder => "\u{2190} OdyTTY Theme Builder  (Esc = back)".to_owned(),
        OverlayMode::FontPicker => "\u{2190} OdyTTY Font Picker  (Esc = back)".to_owned(),
        OverlayMode::KeyBindings => "\u{2190} OdyTTY Key Bindings  (Esc = back)".to_owned(),
        OverlayMode::Onboarding => "Welcome to OdyTTY".to_owned(),
        // Unreachable: handled by the early dispatch above.
        OverlayMode::ContextMenu => String::new(),
        OverlayMode::ConfirmClose => "Close?".to_owned(),
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
        &title,
        title_attrs(),
    );

    let body_width = rect.body_width;
    // Sync the body dimensions into the panel before rendering so that keyboard
    // navigation (`clamp`) uses the real visible window (VIEWPORT-FOLLOW-LAG).
    if overlay.mode == OverlayMode::Settings {
        overlay.panel.update_body_height(rect.body_height);
        overlay.panel.update_body_width(rect.body_width);
    }
    let lines = overlay.visible_lines(body_width, rect.body_height);
    for (row_index, row) in lines.iter().enumerate() {
        let y = rect.top + 2 + row_index;
        if y >= rect.top + rect.height.saturating_sub(1) || y >= rows {
            break;
        }
        let attrs = if row.focused {
            focused_attrs()
        } else if row.bold {
            bold_panel_attrs()
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

/// Render the right-click context menu (IN2): a bordered box at the spawn cell
/// with one row per item. The focused item gets the highlight attrs; a disabled
/// item (Copy with no selection, Paste with an empty clipboard) renders dim. No
/// title row. Item text starts at `left + 2` (border + one pad column), matching
/// the centered panels' body inset.
fn apply_context_menu(snapshot: &mut Snapshot, overlay: &OverlayUi, rect: OverlayRect) {
    use super::context_menu_ui::ContextMenuRow;

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
    let text_column = rect.left + 2;
    let text_width = rect.width.saturating_sub(4);
    for (index, row) in overlay.context_menu.rows().iter().enumerate() {
        let y = rect.body_top + index;
        // Guard against a grid so short the body row falls on/under the bottom
        // border (defensive; `rect()` already sizes the box to fit).
        if y >= rect.top + rect.height.saturating_sub(1) || y >= snapshot.dimensions.rows {
            break;
        }
        match row {
            ContextMenuRow::Separator => {
                // Render a full-width horizontal rule in the border style.
                let sep = "─".repeat(text_width);
                fill_rect(snapshot, text_column, y, text_width, 1, border_attrs());
                write_text(snapshot, y, text_column, text_width, &sep, border_attrs());
            }
            ContextMenuRow::Item {
                label,
                focused,
                enabled,
            } => {
                let attrs = if *focused {
                    focused_attrs()
                } else if *enabled {
                    panel_attrs()
                } else {
                    dim_attrs()
                };
                // Paint the full item row in its attrs so the focus highlight
                // spans the whole width, then write the label over it.
                fill_rect(snapshot, text_column, y, text_width, 1, attrs);
                write_text(snapshot, y, text_column, text_width, label, attrs);
            }
        }
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
            OverlayMode::FontPicker => self
                .font_picker
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::KeyBindings => self
                .key_remap
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::Onboarding => self
                .onboarding
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            // The context menu renders via `apply_context_menu`, not this shared
            // body walker (IN2).
            OverlayMode::ContextMenu => Vec::new(),
            // Static confirmation copy (CLOSE-CONFIRM). No state, no swatch; the
            // shared centered-panel painter draws it like any other modal body.
            OverlayMode::ConfirmClose => vec![
                OverlayLine {
                    text: "A program is still running in this terminal.".to_owned(),
                    focused: false,
                    swatch: None,
                    bold: false,
                },
                OverlayLine {
                    text: String::new(),
                    focused: false,
                    swatch: None,
                    bold: false,
                },
                OverlayLine {
                    text: "Close anyway?   [Enter / Y] Yes     [Esc / N] No".to_owned(),
                    focused: true,
                    swatch: None,
                    bold: false,
                },
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlayLine {
    text: String,
    focused: bool,
    swatch: Option<Srgb>,
    /// Whether to render this line in bold weight. Set for primary setting
    /// name/value rows; unset for group headers, help text, and notices.
    bold: bool,
}

impl From<super::settings_panel::SettingsPanelLine> for OverlayLine {
    fn from(line: super::settings_panel::SettingsPanelLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
        }
    }
}

impl From<ThemePickerLine> for OverlayLine {
    fn from(line: ThemePickerLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: false,
        }
    }
}

impl From<ThemeBuilderLine> for OverlayLine {
    fn from(line: ThemeBuilderLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: line.swatch,
            bold: false,
        }
    }
}

impl From<FontPickerLine> for OverlayLine {
    fn from(line: FontPickerLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: false,
        }
    }
}

impl From<KeyRemapLine> for OverlayLine {
    fn from(line: KeyRemapLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: false,
        }
    }
}

impl From<OnboardingLine> for OverlayLine {
    fn from(line: OnboardingLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: false,
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

/// Bold variant of `panel_attrs` for primary setting name/value rows.
fn bold_panel_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.set_bold(true);
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

/// Attrs for a disabled context-menu item (IN2): the panel fill with a muted
/// (bright-black) foreground so the label reads as unavailable.
fn dim_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.foreground = Color::Indexed(8);
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

        apply_overlay(&mut rendered, &mut overlay);

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
    fn onboarding_opens_renders_and_dismisses() {
        let mut overlay = OverlayUi::default();
        overlay.open_onboarding();
        assert!(overlay.is_open());
        assert_eq!(overlay.render_signature().mode, OverlayMode::Onboarding);

        // The welcome card paints its title into the snapshot.
        let mut rendered = snapshot(70, 18);
        apply_overlay(&mut rendered, &mut overlay);
        let painted: String = rendered.cells.iter().map(|cell| cell.ch).collect();
        assert!(painted.contains("Welcome to OdyTTY"));

        // Enter, Esc, and Space each dismiss; any other key is swallowed.
        assert_eq!(
            overlay.handle_input(OverlayInput::Char('x')),
            OverlayOutcome::Consumed
        );
        assert!(overlay.is_open());
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::CloseOnboarding
        );
        for input in [
            OverlayInput::Close,
            OverlayInput::Char(' '),
            OverlayInput::Activate,
        ] {
            overlay.open_onboarding();
            assert_eq!(overlay.handle_input(input), OverlayOutcome::CloseOnboarding);
        }
    }

    #[test]
    fn confirm_close_dialog_opens_renders_and_routes_keys() {
        // CLOSE-CONFIRM: the dialog opens in its own mode, paints its title and
        // copy, and routes keys per the keyboard contract.
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_close();
        assert!(overlay.is_open());
        assert_eq!(overlay.render_signature().mode, OverlayMode::ConfirmClose);

        // The dialog paints its title and a non-empty body.
        let mut rendered = snapshot(70, 18);
        apply_overlay(&mut rendered, &mut overlay);
        let painted: String = rendered.cells.iter().map(|cell| cell.ch).collect();
        assert!(painted.contains("Close?"));
        assert!(painted.contains("Close anyway?"));

        // Enter confirms: emits ForceClose AND closes the dialog so the UI is
        // clean before the App exits (TRAP-4).
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::ForceClose
        );
        assert!(!overlay.is_open());

        // 'y' / 'Y' also confirm.
        for ch in ['y', 'Y'] {
            overlay.open_confirm_close();
            assert_eq!(
                overlay.handle_input(OverlayInput::Char(ch)),
                OverlayOutcome::ForceClose
            );
            assert!(!overlay.is_open());
        }

        // Esc and 'n' / 'N' cancel: they emit Close (NOT ForceClose), so the
        // window never exits on a dismiss (TRAP-2).
        for input in [
            OverlayInput::Close,
            OverlayInput::Char('n'),
            OverlayInput::Char('N'),
        ] {
            overlay.open_confirm_close();
            assert_eq!(overlay.handle_input(input), OverlayOutcome::Close);
        }

        // Any other key is swallowed (no PTY leak behind the modal).
        overlay.open_confirm_close();
        assert_eq!(
            overlay.handle_input(OverlayInput::Char('x')),
            OverlayOutcome::Consumed
        );
        assert!(overlay.is_open());
    }

    #[test]
    fn confirm_close_open_is_idempotent() {
        // TRAP-3: a repeated close request (some window managers fire twice)
        // must not stack dialogs — open_confirm_close starts with close().
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_close();
        overlay.open_confirm_close();
        assert!(overlay.is_open());
        assert_eq!(overlay.render_signature().mode, OverlayMode::ConfirmClose);
    }

    #[test]
    fn escape_while_searching_does_not_close_overlay() {
        // R7: the overlay-close Esc is gated on `!is_searching()`, so an Esc in
        // search mode runs the panel's two-step exit instead of closing.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        assert_eq!(
            overlay.handle_input(OverlayInput::Char('/')),
            OverlayOutcome::Consumed
        );
        for ch in "cursor".chars() {
            let _ = overlay.handle_input(OverlayInput::Char(ch));
        }
        // First Esc clears the query, overlay stays open.
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Consumed
        );
        assert!(overlay.is_open());
        // Second Esc exits search, still not closing the overlay.
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Consumed
        );
        assert!(overlay.is_open());
        // With search fully exited, Esc now closes.
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Close
        );
    }

    /// OVERLAY-SIZE: on a large terminal the panel must be substantially wider
    /// and taller than the old 22-row / 64-col-min caps. Also verifies that the
    /// hit-map still aligns 1:1 with visible_lines after the resize (their shared
    /// `build_visible_rows` walker guarantees this by construction).
    #[test]
    fn overlay_rect_is_wider_and_taller_on_large_terminal() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();

        // 120×50 grid — large enough to show the effect of the raised caps.
        let rect = overlay_rect(&overlay, 120, 50).expect("rect");

        // Width: must be substantially wider than the old 64-col floor.
        // At 120 cols: (120*3/4).max(80)+4 = 94 → capped at 120. At least 90.
        assert!(
            rect.width >= 90,
            "panel width should be wide on a 120-col terminal, got {}",
            rect.width
        );

        // Height: must be taller than the old 22-row cap. At 50 rows:
        // (50*4/5).max(22).min(48) = 40.
        assert!(
            rect.height > 22,
            "panel height should exceed 22 on a 50-row terminal, got {}",
            rect.height
        );

        // visible_lines must produce at least as many rows as there are entries
        // in the first group (the shared walker never drops rows vs. the hit-map).
        let lines = overlay
            .panel
            .visible_lines(rect.body_width, rect.body_height);
        assert!(
            !lines.is_empty(),
            "visible_lines must be non-empty after resize"
        );
        // All lines must be within the body_height window.
        assert!(
            lines.len() <= rect.body_height,
            "visible_lines must not exceed body_height: {} > {}",
            lines.len(),
            rect.body_height
        );
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

    #[test]
    fn theme_picker_cancel_returns_to_settings_when_launched_from_settings_panel() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        let original_theme = overlay.settings.theme;
        // Open Themes section and activate the theme row.
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::OpenThemePicker
        );

        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);

        let OverlayOutcome::ApplySettings(restored) = overlay.handle_input(OverlayInput::Close)
        else {
            panic!("expected settings restore when canceling the theme picker");
        };
        assert_eq!(restored.theme, original_theme);
        assert!(overlay.is_open());
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "cancel should return to Settings from picker"
        );
    }

    #[test]
    fn theme_picker_save_returns_to_settings_when_launched_from_settings_panel() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();

        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::OpenThemePicker
        );

        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);

        assert!(matches!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::ApplySettings(_)
        ));
        let OverlayOutcome::SaveSettings(changes) = overlay.handle_input(OverlayInput::Activate)
        else {
            panic!("expected theme picker save request");
        };
        assert_eq!(changes.len(), 1);

        overlay.save_succeeded(changes.len());
        assert!(overlay.is_open());
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "theme picker apply from settings should return to settings panel"
        );
    }

    #[test]
    fn theme_picker_save_then_panel_commit_keeps_external_theme() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();

        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::OpenThemePicker
        );

        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);

        let OverlayOutcome::ApplySettings(preview) = overlay.handle_input(OverlayInput::Down)
        else {
            panic!("expected theme preview");
        };
        let preview_theme = preview.theme;
        overlay.apply_settings(&preview);

        let OverlayOutcome::SaveSettings(changes) = overlay.handle_input(OverlayInput::Activate)
        else {
            panic!("expected theme picker save request");
        };
        overlay.save_succeeded(changes.len());

        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Consumed
        );
        while overlay.render_signature().panel.section_selected != 1 {
            assert_eq!(
                overlay.handle_input(OverlayInput::Down),
                OverlayOutcome::Consumed
            );
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        for _ in 0..8 {
            if overlay
                .render_signature()
                .panel
                .entries
                .get(overlay.render_signature().panel.selected)
                .is_some_and(|entry| entry.key == "font_size")
            {
                break;
            }
            assert_eq!(
                overlay.handle_input(OverlayInput::Down),
                OverlayOutcome::Consumed
            );
        }
        assert_eq!(
            overlay
                .render_signature()
                .panel
                .entries
                .get(overlay.render_signature().panel.selected)
                .map(|entry| entry.key),
            Some("font_size")
        );
        let OverlayOutcome::ApplySettings(committed) = overlay.handle_input(OverlayInput::Right)
        else {
            panic!("expected second settings edit to apply");
        };

        assert_eq!(
            committed.theme, preview_theme,
            "panel commit must not rebuild settings from the old theme baseline"
        );
    }

    #[test]
    fn font_picker_cancel_returns_to_settings_when_launched_from_settings_panel() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();

        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        let original_font_family = overlay.settings.font_family.clone();
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::OpenFontPicker
        );

        let settings = overlay.settings.clone();
        overlay.open_font_picker(&settings);
        let outcome = overlay.handle_input(OverlayInput::Close);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        assert!(overlay.is_open());
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "font picker cancel should return to Settings panel"
        );
        assert_eq!(overlay.settings.font_family, original_font_family);
    }

    #[test]
    fn font_picker_apply_stays_open_when_launched_from_settings_panel() {
        // FONT-PICKER-STAY-OPEN: Enter applies+saves the font but KEEPS the
        // picker open so the user can keep cycling. It must NOT return to the
        // settings panel after the save succeeds.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();

        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::OpenFontPicker
        );

        let settings = overlay.settings.clone();
        overlay.open_font_picker(&settings);
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        let outcome = overlay.handle_input(OverlayInput::Activate);
        let OverlayOutcome::SaveSettings(changes) = outcome else {
            panic!("expected font picker save request");
        };
        assert_eq!(changes.len(), 1);
        // The applied family is the value in the SettingEdit (the source of
        // truth for what was just applied); `overlay.settings` is only refreshed
        // by the app's reload path, not by `save_succeeded`.
        let applied = changes[0].value.clone();

        overlay.save_succeeded(changes.len());
        // Stay-open contract: still open, still in FontPicker mode.
        assert!(overlay.is_open(), "picker must stay open after apply");
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::FontPicker,
            "font picker apply must stay in FontPicker mode, not return to panel"
        );

        // The applied family now shows the "current" marker in the rendered list
        // (font_picker.save_succeeded adopts it as self.original).
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let lines = overlay.visible_lines(rect.body_width, rect.body_height);
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("current") && line.text.contains(&applied)),
            "applied family {applied:?} must render with the current marker after apply"
        );

        // Esc still returns to the settings panel (the panel-launched path).
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Consumed
        );
        assert!(overlay.is_open());
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "Esc must return to the settings panel after the panel-launched picker"
        );
    }

    #[test]
    fn font_picker_apply_stays_open_then_esc_closes_standalone() {
        // FONT-PICKER-STAY-OPEN standalone path (Ctrl+Shift+F): Enter applies and
        // keeps the picker open; a second Enter applies another font; Esc closes.
        let mut overlay = OverlayUi::default();
        overlay.open_font_picker(&overlay.settings.clone());
        assert_eq!(overlay.render_signature().mode, OverlayMode::FontPicker);

        // First Enter: apply + stay open.
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        let outcome = overlay.handle_input(OverlayInput::Activate);
        let OverlayOutcome::SaveSettings(changes) = outcome else {
            panic!("expected first save request");
        };
        overlay.save_succeeded(changes.len());
        assert!(overlay.is_open(), "picker stays open after first apply");
        assert_eq!(overlay.render_signature().mode, OverlayMode::FontPicker);

        // Second Enter: still persists and still stays open (cycling works).
        let outcome = overlay.handle_input(OverlayInput::Activate);
        let OverlayOutcome::SaveSettings(changes2) = outcome else {
            panic!("expected second save request");
        };
        overlay.save_succeeded(changes2.len());
        assert!(overlay.is_open(), "picker stays open after second apply");
        assert_eq!(overlay.render_signature().mode, OverlayMode::FontPicker);

        // Esc on the standalone path fully closes the overlay.
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Close
        );
        assert!(!overlay.is_open(), "standalone Esc closes the picker");
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
                x_in_body: None,
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
                x_in_body: None,
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
        // Drill into Themes section first (Enter on the focused first section).
        overlay.handle_input(OverlayInput::Activate);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // body_top + 1 = theme value row (after "Theme" group header at row 0).
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: theme_value_cell(rect),
                button: PointerButton::Left,
                x_in_body: None,
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
        // The top border row sits above body_top but inside the panel box.
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(outcome, OverlayOutcome::Consumed);
    }

    #[test]
    fn pointer_press_on_settings_back_arrow_returns_to_sections() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        overlay.handle_input(OverlayInput::Down); // Fonts
        overlay.handle_input(OverlayInput::Activate); // drill in
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Clicking the TITLE ROW (rect.top) where the ← arrow is actually drawn.
        // This is the correct click target; the previous test used rect.top + 1
        // (the row below the title) which was wrong — the arrow is at rect.top.
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(outcome, OverlayOutcome::Consumed);
        assert_eq!(
            overlay.render_signature().panel.level,
            SettingsLevel::SectionList,
            "clicking the title row ← arrow returns to section list"
        );

        // The row below the title (rect.top + 1) is also accepted as a forgiving
        // click target for the back affordance.
        overlay.handle_input(OverlayInput::Activate); // re-enter detail
        let rect2 = overlay_rect(&overlay, 80, 24).expect("rect");
        let outcome2 = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect2.top + 1,
                    column: rect2.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect2,
        );
        assert_eq!(outcome2, OverlayOutcome::Consumed);
        assert_eq!(
            overlay.render_signature().panel.level,
            SettingsLevel::SectionList,
            "clicking one row below title also navigates back"
        );
    }

    #[test]
    fn pointer_wheel_scrolls_settings_without_changing_selection() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        // Drill into Rendering (many entries) so wheel scrolls the Level-2
        // entry list (self.scroll), not the Level-1 section_scroll.
        overlay.handle_input(OverlayInput::Down); // Fonts
        overlay.handle_input(OverlayInput::Down); // Rendering
        overlay.handle_input(OverlayInput::Activate); // drill in
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let before = overlay.render_signature().panel;
        let outcome = overlay.handle_pointer(OverlayPointer::Wheel { lines: 4 }, rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        let after = overlay.render_signature().panel;
        assert!(after.scroll > before.scroll, "wheel scrolled the list");
        assert_eq!(after.selected, before.selected, "selection did not move");
    }

    // --- Settings numeric steppers: no live drag capture ---

    #[test]
    fn pointer_press_steps_numeric_once_and_move_release_are_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        // Drill into Fonts section (contains font_size, a stepper row).
        overlay.handle_input(OverlayInput::Down); // section_selected = 1 (Fonts)
        overlay.handle_input(OverlayInput::Activate); // drill in
        let (down, up) = overlay
            .first_stepper_button_cells(80, 24)
            .expect("a stepper row is visible in Fonts section");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Press the up button -> applies once without arming drag.
        assert!(matches!(
            overlay.handle_pointer(
                OverlayPointer::Press {
                    cell: up,
                    button: PointerButton::Left,
                    x_in_body: None,
                },
                rect,
            ),
            OverlayOutcome::ApplySettings(_)
        ));
        assert!(
            !overlay.is_settings_dragging(),
            "settings stepper click does not arm a drag"
        );

        // Move to the down button -> inert, because settings steppers do not
        // capture pointer motion.
        assert_eq!(
            overlay.handle_pointer(
                OverlayPointer::Move {
                    cell: down,
                    x_in_body: None
                },
                rect
            ),
            OverlayOutcome::Consumed
        );

        // Release and later move stay inert.
        assert_eq!(
            overlay.handle_pointer(
                OverlayPointer::Release {
                    cell: down,
                    button: PointerButton::Left,
                },
                rect,
            ),
            OverlayOutcome::Consumed
        );
        assert!(!overlay.is_settings_dragging());
        assert_eq!(
            overlay.handle_pointer(
                OverlayPointer::Move {
                    cell: up,
                    x_in_body: None
                },
                rect
            ),
            OverlayOutcome::Consumed,
            "no drag after release"
        );
    }

    #[test]
    fn settings_stepper_click_cannot_leave_drag_state_across_close_reopen() {
        // Settings steppers do not arm drag state, so a missing release cannot
        // survive close/reopen or drive a phantom value.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        // Drill into Fonts section to get a stepper row.
        overlay.handle_input(OverlayInput::Down); // Fonts
        overlay.handle_input(OverlayInput::Activate); // drill in
        let (_, up) = overlay
            .first_stepper_button_cells(80, 24)
            .expect("a stepper row is visible in Fonts section");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Click a stepper, then close WITHOUT a release.
        let _ = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: up,
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert!(
            !overlay.is_settings_dragging(),
            "settings stepper click does not arm drag state"
        );
        overlay.close();
        assert!(!overlay.is_settings_dragging());

        // Reopen and assert a bare Move does nothing.
        overlay.open_settings();
        assert!(!overlay.is_settings_dragging(), "reopen has no stale drag");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        assert_eq!(
            overlay.handle_pointer(
                OverlayPointer::Move {
                    cell: up,
                    x_in_body: None
                },
                rect
            ),
            OverlayOutcome::Consumed,
            "hover after reopen is inert"
        );
    }

    #[test]
    fn focus_loss_after_settings_stepper_click_keeps_overlay_open_and_inert() {
        // Settings steppers never arm drag state. Focus-loss cleanup remains
        // safe and a bare hover Move on focus regain cannot commit a phantom
        // numeric value.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        // Drill into Fonts section to get a stepper row.
        overlay.handle_input(OverlayInput::Down); // Fonts
        overlay.handle_input(OverlayInput::Activate); // drill in
        let (down, up) = overlay
            .first_stepper_button_cells(80, 24)
            .expect("a stepper row is visible in Fonts section");
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Click the up button.
        let _ = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: up,
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert!(
            !overlay.is_settings_dragging(),
            "settings stepper click does not arm drag state"
        );

        // Focus loss WITHOUT a release and WITHOUT a close.
        overlay.cancel_settings_drag();
        assert!(overlay.is_open(), "focus loss does not close the overlay");
        assert!(!overlay.is_settings_dragging());

        // A bare hover Move after focus regain is inert.
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        assert_eq!(
            overlay.handle_pointer(
                OverlayPointer::Move {
                    cell: down,
                    x_in_body: None
                },
                rect
            ),
            OverlayOutcome::Consumed,
            "hover after focus regain is inert"
        );
        assert!(
            !overlay.is_settings_dragging(),
            "hover did not re-arm the drag"
        );
    }

    // --- U2 Step 2/3: builder pointer routing through handle_pointer ---

    #[test]
    fn builder_slider_press_routes_through_handle_pointer_and_arms_a_drag() {
        let mut overlay = OverlayUi::default();
        let settings = overlay.settings.clone();
        overlay.open_theme_builder(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Row 4 of the body is the focused-channel slider (title, name, contrast,
        // channel-picker, slider). A left press on it applies a theme and arms a
        // drag the App's Move gate (`is_settings_dragging`) now reports.
        let cell = CellPoint {
            row: rect.body_top + 4,
            column: rect.body_left + rect.body_width.saturating_sub(1),
        };
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell,
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert!(
            matches!(outcome, OverlayOutcome::ApplySettings(_)),
            "slider press previews a theme"
        );
        assert!(
            overlay.is_settings_dragging(),
            "builder slider press arms a drag routed via the shared gate"
        );

        // Release ends the drag through the same gate.
        overlay.handle_pointer(
            OverlayPointer::Release {
                cell,
                button: PointerButton::Left,
            },
            rect,
        );
        assert!(
            !overlay.is_settings_dragging(),
            "release ends the builder drag"
        );
    }

    #[test]
    fn builder_press_outside_restores_and_closes() {
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_builder(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let OverlayOutcome::ApplySettings(restored) = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint { row: 0, column: 0 },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        ) else {
            panic!("expected restoration settings on click-away");
        };
        assert_eq!(restored.theme, crate::theme::Theme::ODYSSEY);
        assert!(!overlay.is_open(), "click-away closes the builder");
    }

    // --- Picker back-button mouse click ---

    #[test]
    fn theme_picker_title_back_arrow_click_closes_standalone() {
        // Standalone ThemePicker (no picker_return): clicking the ← title area
        // restores the original theme and closes the overlay.
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);
        // Navigate away from the original so cancel is visible.
        let _ = overlay.handle_input(OverlayInput::Down);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        // Restores theme (ApplySettings) and closes.
        assert!(
            matches!(outcome, OverlayOutcome::ApplySettings(_)),
            "theme picker ← click should restore theme"
        );
        assert!(
            !overlay.is_open(),
            "standalone theme picker ← click closes the overlay"
        );
    }

    #[test]
    fn theme_picker_title_back_arrow_click_returns_to_settings_when_from_settings() {
        // ThemePicker opened from settings: clicking ← should return to settings.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        // Drill into Themes section then activate the theme row to set picker_return.
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::OpenThemePicker
        );
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Click the ← area in the title row.
        let _outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert!(
            overlay.is_open(),
            "overlay stays open after returning to settings"
        );
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "theme picker ← click returns to settings panel"
        );
    }

    #[test]
    fn font_picker_title_back_arrow_click_closes_standalone() {
        // Standalone FontPicker (no picker_return): clicking the ← title area
        // closes the overlay.
        let mut overlay = OverlayUi::default();
        overlay.open_font_picker(&overlay.settings.clone());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(
            outcome,
            OverlayOutcome::Close,
            "standalone font picker ← click emits Close"
        );
        assert!(
            !overlay.is_open(),
            "standalone font picker ← click closes the overlay"
        );
    }

    #[test]
    fn font_picker_title_back_arrow_click_returns_to_settings_when_from_settings() {
        // FontPicker opened from settings: clicking ← should return to settings.
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        // Navigate: Down → Fonts section, Activate → drill in, Down → font_family
        // row, Activate → OpenFontPicker (sets picker_return).
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::OpenFontPicker
        );
        let settings = overlay.settings.clone();
        overlay.open_font_picker(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Click the ← area in the title row.
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(
            outcome,
            OverlayOutcome::Consumed,
            "font picker ← click from settings emits Consumed"
        );
        assert!(
            overlay.is_open(),
            "overlay stays open after returning to settings"
        );
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "font picker ← click returns to settings panel"
        );
    }

    // --- Theme picker mouse wheel scrolling ---

    #[test]
    fn theme_picker_wheel_scrolls_selection() {
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let before = overlay.render_signature().theme_picker.selected;

        // Wheel down moves selection forward.
        let outcome = overlay.handle_pointer(OverlayPointer::Wheel { lines: 1 }, rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        let after = overlay.render_signature().theme_picker.selected;
        assert!(
            after > before,
            "wheel down advances selection in theme picker"
        );
    }

    #[test]
    fn theme_picker_wheel_up_moves_selection_back() {
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // First advance a few entries so there's room to scroll back.
        for _ in 0..3 {
            overlay.handle_pointer(OverlayPointer::Wheel { lines: 1 }, rect);
        }
        let mid = overlay.render_signature().theme_picker.selected;
        assert!(mid >= 3, "should have advanced at least 3 entries");

        // Wheel up moves selection backward.
        overlay.handle_pointer(OverlayPointer::Wheel { lines: -1 }, rect);
        let after = overlay.render_signature().theme_picker.selected;
        assert!(after < mid, "wheel up moves selection back in theme picker");
    }

    // --- Picker title back-arrow: non-back-area title click is inert ---

    #[test]
    fn theme_picker_title_click_far_right_is_inert() {
        // Clicking outside the ← area in the title row (far right) is inert.
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left + 20, // far right of title, outside ← zone
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(
            outcome,
            OverlayOutcome::Consumed,
            "title click outside ← zone is inert"
        );
        assert!(
            overlay.is_open(),
            "inert title click does not close the picker"
        );
    }

    // --- KeyBindings back-button ---

    #[test]
    fn key_bindings_title_back_arrow_click_returns_to_settings() {
        // KeyBindings is always opened from Settings. Clicking ← in the title
        // area must return to Settings (not close the overlay entirely).
        let mut overlay = OverlayUi::default();
        let settings = overlay.settings.clone();
        overlay.open_settings();
        overlay.open_key_bindings(&settings);
        assert_eq!(overlay.render_signature().mode, OverlayMode::KeyBindings);

        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        // Returns ApplySettings (restores undone key-binding changes) and stays open.
        assert!(
            matches!(outcome, OverlayOutcome::ApplySettings(_)),
            "key bindings ← click should emit ApplySettings"
        );
        assert!(
            overlay.is_open(),
            "overlay stays open after returning to settings"
        );
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "key bindings ← click returns to Settings panel"
        );
    }

    #[test]
    fn key_bindings_esc_returns_to_settings() {
        // Keyboard Esc in KeyBindings navigates back to Settings (consistent
        // with pickers' cancel-to-return path).
        let mut overlay = OverlayUi::default();
        let settings = overlay.settings.clone();
        overlay.open_settings();
        overlay.open_key_bindings(&settings);
        assert_eq!(overlay.render_signature().mode, OverlayMode::KeyBindings);

        let outcome = overlay.handle_input(OverlayInput::Close);
        assert!(
            matches!(outcome, OverlayOutcome::ApplySettings(_)),
            "key bindings Esc should emit ApplySettings"
        );
        assert!(overlay.is_open(), "overlay stays open after Esc");
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::Settings,
            "key bindings Esc returns to Settings panel"
        );
    }

    #[test]
    fn key_bindings_title_back_zone_not_matched_outside_arrow_area() {
        // Clicking outside the ← zone in the KeyBindings title row is inert.
        let mut overlay = OverlayUi::default();
        let settings = overlay.settings.clone();
        overlay.open_key_bindings(&settings);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left + 20, // far right, outside ← zone
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(
            outcome,
            OverlayOutcome::Consumed,
            "title click outside ← zone in key bindings is inert"
        );
        assert!(
            overlay.is_open(),
            "inert title click does not close key bindings"
        );
    }

    // --- ThemeBuilder back-button ---

    #[test]
    fn theme_builder_title_back_arrow_click_from_picker_returns_to_picker() {
        // ThemeBuilder opened from ThemePicker: clicking ← in the title area
        // must return to ThemePicker (not close the overlay).
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_picker(&settings);
        // Simulate opening the builder from the picker (sets builder_from_picker).
        let _ = overlay.handle_input(OverlayInput::Activate); // OpenBuilder for focused theme
        // If OpenBuilder wasn't triggered (no customizable theme focused), open manually.
        if overlay.render_signature().mode != OverlayMode::ThemeBuilder {
            // Force into ThemeBuilder via the picker outcome path.
            overlay.open_theme_picker(&settings);
            // Directly transition as the picker would.
            overlay.theme_builder.open(&settings);
            overlay.mode = OverlayMode::ThemeBuilder;
            overlay.builder_from_picker = true;
        }
        assert_eq!(overlay.render_signature().mode, OverlayMode::ThemeBuilder);

        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let outcome = overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row: rect.top,
                    column: rect.body_left,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert!(
            matches!(outcome, OverlayOutcome::ApplySettings(_)),
            "theme builder ← click should emit ApplySettings (restore theme)"
        );
        assert!(
            overlay.is_open(),
            "overlay stays open after returning to theme picker"
        );
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::ThemePicker,
            "theme builder ← click returns to ThemePicker"
        );
    }

    #[test]
    fn theme_builder_esc_from_picker_returns_to_theme_picker() {
        // ThemeBuilder opened from ThemePicker: keyboard Esc returns to
        // ThemePicker (cancel edits, restore original theme, stay open).
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        // Manually set up the picker → builder transition.
        overlay.open_theme_picker(&settings);
        overlay.theme_builder.open(&settings);
        overlay.mode = OverlayMode::ThemeBuilder;
        overlay.builder_from_picker = true;

        let outcome = overlay.handle_input(OverlayInput::Close);
        assert!(
            matches!(outcome, OverlayOutcome::ApplySettings(_)),
            "theme builder Esc should emit ApplySettings"
        );
        assert!(overlay.is_open(), "overlay stays open after Esc");
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::ThemePicker,
            "theme builder Esc returns to ThemePicker when opened from picker"
        );
    }

    #[test]
    fn theme_builder_esc_standalone_closes_overlay() {
        // ThemeBuilder opened standalone (not from ThemePicker): Esc / click-away
        // closes the overlay entirely (existing behavior, unaffected by back-nav).
        let mut overlay = OverlayUi::new(&Settings {
            theme: crate::theme::Theme::ODYSSEY,
            ..Settings::default()
        });
        let settings = overlay.settings.clone();
        overlay.open_theme_builder(&settings); // standalone path, builder_from_picker = false
        assert_eq!(overlay.render_signature().mode, OverlayMode::ThemeBuilder);

        let outcome = overlay.handle_input(OverlayInput::Close);
        assert!(
            matches!(outcome, OverlayOutcome::ApplySettings(_)),
            "standalone builder Esc emits ApplySettings (restore theme)"
        );
        assert!(
            !overlay.is_open(),
            "standalone builder Esc closes the overlay"
        );
    }
}
