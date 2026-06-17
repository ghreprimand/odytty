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
use super::key_remap_ui::{KeyRemapLine, KeyRemapOutcome, KeyRemapSignature, KeyRemapUi};
use super::onboarding::{OnboardingLine, OnboardingPanel, OnboardingSignature};
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
    key_remap: KeyRemapUi,
    onboarding: OnboardingPanel,
    context_menu: ContextMenuUi,
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
            key_remap: KeyRemapUi::new(settings),
            onboarding: OnboardingPanel::new(settings),
            context_menu: ContextMenuUi::new(),
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
        self.theme_builder.end_channel_drag();
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
    pub(super) fn open_context_menu(&mut self, spawn: CellPoint, copy: bool, paste: bool) {
        self.panel.end_slider_drag();
        self.context_menu.open(spawn, copy, paste);
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
                    ContextMenuItem::Paste => OverlayOutcome::ContextMenuPaste,
                    ContextMenuItem::SelectAll => OverlayOutcome::ContextMenuSelectAll,
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
            OverlayMode::KeyBindings => return self.handle_key_remap_input(input),
            OverlayMode::Onboarding => return self.handle_onboarding_input(input),
            OverlayMode::ContextMenu => return self.handle_context_menu_input(input),
            OverlayMode::ConfirmClose => return self.handle_confirm_close_input(input),
            OverlayMode::Settings => {}
        }

        match input {
            OverlayInput::Close if !self.panel.is_editing() && !self.panel.is_searching() => {
                OverlayOutcome::Close
            }
            input => settings_outcome(self.panel.handle_input(input)),
        }
    }

    /// Pointer entry point (UX4-P1), the mouse analogue of [`Self::handle_input`].
    /// `rect` is the live overlay geometry from [`overlay_rect`]. A press outside
    /// the panel dismisses exactly like Esc (per-mode: the theme picker restores
    /// its original theme). Inside the Settings panel a press is hit-tested to a
    /// row+zone and dispatched through the existing value seam. Inside the theme
    /// builder a press routes to role/channel focus and starts a slider drag on
    /// the focused-channel track (U2 Step 2/3); only the theme picker stays
    /// keyboard-driven (its inside presses remain inert).
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
                let Some(row_in_body) = cell.row.checked_sub(rect.body_top) else {
                    // The title row / top border: inside the box, inert.
                    return OverlayOutcome::Consumed;
                };
                let col_in_body = cell.column.saturating_sub(rect.body_left);
                match self.mode {
                    OverlayMode::Settings => settings_outcome(self.panel.handle_pointer_press(
                        rect.body_width,
                        rect.body_height,
                        row_in_body,
                        col_in_body,
                        button,
                    )),
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
                    | OverlayMode::KeyBindings
                    | OverlayMode::Onboarding
                    | OverlayMode::ConfirmClose => OverlayOutcome::Consumed,
                }
            }
            OverlayPointer::Move { cell } => {
                let col_in_body = cell.column.saturating_sub(rect.body_left);
                match self.mode {
                    OverlayMode::Settings => settings_outcome(self.panel.handle_pointer_drag(
                        rect.body_width,
                        rect.body_height,
                        col_in_body,
                    )),
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
                    OverlayMode::ThemePicker
                    | OverlayMode::Onboarding
                    | OverlayMode::ContextMenu
                    | OverlayMode::ConfirmClose => {}
                }
                OverlayOutcome::Consumed
            }
        }
    }

    /// Whether an overlay slider drag is in progress (UX4-P2 settings slider or
    /// U2 Step 2/3 builder channel slider). The App gates per-move routing on
    /// this so non-drag hover stays cheap. (Name kept for the App call sites,
    /// which live in a peer lane this wave; it now covers the builder too.)
    pub(super) fn is_settings_dragging(&self) -> bool {
        match self.mode {
            OverlayMode::Settings => self.panel.is_dragging(),
            OverlayMode::ThemeBuilder => self.theme_builder.is_dragging(),
            OverlayMode::ThemePicker
            | OverlayMode::KeyBindings
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::ConfirmClose => false,
        }
    }

    /// Abandon any in-progress settings slider drag WITHOUT closing the overlay
    /// (UX4-P2). The App calls this on focus loss while the overlay stays open:
    /// a press may have armed a drag whose release is delivered to another
    /// window after an alt-tab, so without this the drag would survive and the
    /// next bare hover Move on focus regain would commit a phantom slider value
    /// — the overlay-stays-open analogue of the close/reopen lost-release case.
    /// No-op unless the active mode currently holds a slider drag.
    pub(super) fn cancel_settings_drag(&mut self) {
        match self.mode {
            OverlayMode::Settings => self.panel.end_slider_drag(),
            OverlayMode::ThemeBuilder => self.theme_builder.end_channel_drag(),
            OverlayMode::ThemePicker
            | OverlayMode::KeyBindings
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::ConfirmClose => {}
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
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            ThemePickerOutcome::Cancel(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                self.close();
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
        }
    }

    fn handle_theme_builder_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.theme_builder.handle_input(input);
        self.apply_builder_outcome(outcome)
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
                self.close();
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
                self.close();
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
                OverlayOutcome::Close
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
    OpenThemePicker,
    OpenThemeBuilder,
    OpenKeyBindings,
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
    ContextMenuPaste,
    ContextMenuSelectAll,
    /// The user confirmed the close-confirmation dialog (CLOSE-CONFIRM): close
    /// the window. The overlay has already closed itself by the time this is
    /// emitted; the App sets its `pending_exit` flag and exits the event loop on
    /// the same turn (the outcome can't reach `ActiveEventLoop` directly).
    ForceClose,
}

/// Lift a [`SettingsPanelOutcome`] (from the keyboard or the pointer path) into
/// an [`OverlayOutcome`]. The single mapping shared by `handle_input`,
/// `handle_pointer` press, and `handle_pointer` drag so the three entry points
/// can never diverge.
fn settings_outcome(outcome: SettingsPanelOutcome) -> OverlayOutcome {
    match outcome {
        SettingsPanelOutcome::Consumed => OverlayOutcome::Consumed,
        SettingsPanelOutcome::Apply(settings) => OverlayOutcome::ApplySettings(Box::new(settings)),
        SettingsPanelOutcome::Save(changes) => OverlayOutcome::SaveSettings(changes),
        SettingsPanelOutcome::OpenThemePicker => OverlayOutcome::OpenThemePicker,
        SettingsPanelOutcome::OpenThemeBuilder => OverlayOutcome::OpenThemeBuilder,
        SettingsPanelOutcome::OpenKeyBindings => OverlayOutcome::OpenKeyBindings,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayMode {
    Settings,
    ThemePicker,
    ThemeBuilder,
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
    // The context menu has its own no-title layout (IN2); dispatch and return.
    if overlay.mode == OverlayMode::ContextMenu {
        apply_context_menu(snapshot, overlay, rect);
        return;
    }
    let rows = snapshot.dimensions.rows;
    let title = match overlay.mode {
        OverlayMode::Settings => "OdyTTY Settings",
        OverlayMode::ThemePicker => "OdyTTY Themes",
        OverlayMode::ThemeBuilder => "OdyTTY Theme Builder",
        OverlayMode::KeyBindings => "OdyTTY Key Bindings",
        OverlayMode::Onboarding => "Welcome to OdyTTY",
        // Unreachable: handled by the early dispatch above.
        OverlayMode::ContextMenu => "",
        OverlayMode::ConfirmClose => "Close?",
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

/// Render the right-click context menu (IN2): a bordered box at the spawn cell
/// with one row per item. The focused item gets the highlight attrs; a disabled
/// item (Copy with no selection, Paste with an empty clipboard) renders dim. No
/// title row. Item text starts at `left + 2` (border + one pad column), matching
/// the centered panels' body inset.
fn apply_context_menu(snapshot: &mut Snapshot, overlay: &OverlayUi, rect: OverlayRect) {
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
    for (index, (label, focused, enabled)) in overlay.context_menu.rows().iter().enumerate() {
        let y = rect.body_top + index;
        // Guard against a grid so short the body row falls on/under the bottom
        // border (defensive; `rect()` already sizes the box to fit).
        if y >= rect.top + rect.height.saturating_sub(1) || y >= snapshot.dimensions.rows {
            break;
        }
        let attrs = if *focused {
            focused_attrs()
        } else if *enabled {
            panel_attrs()
        } else {
            dim_attrs()
        };
        // Paint the full item row in its attrs so the focus highlight spans the
        // whole width, then write the label over it.
        fill_rect(snapshot, text_column, y, text_width, 1, attrs);
        write_text(snapshot, y, text_column, text_width, label, attrs);
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
                },
                OverlayLine {
                    text: String::new(),
                    focused: false,
                    swatch: None,
                },
                OverlayLine {
                    text: "Close anyway?   [Enter / Y] Yes     [Esc / N] No".to_owned(),
                    focused: true,
                    swatch: None,
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

impl From<KeyRemapLine> for OverlayLine {
    fn from(line: KeyRemapLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
        }
    }
}

impl From<OnboardingLine> for OverlayLine {
    fn from(line: OnboardingLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
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
    fn onboarding_opens_renders_and_dismisses() {
        let mut overlay = OverlayUi::default();
        overlay.open_onboarding();
        assert!(overlay.is_open());
        assert_eq!(overlay.render_signature().mode, OverlayMode::Onboarding);

        // The welcome card paints its title into the snapshot.
        let mut rendered = snapshot(70, 18);
        apply_overlay(&mut rendered, &overlay);
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
            OverlayOutcome::Close
        );
        for input in [
            OverlayInput::Close,
            OverlayInput::Char(' '),
            OverlayInput::Activate,
        ] {
            overlay.open_onboarding();
            assert_eq!(overlay.handle_input(input), OverlayOutcome::Close);
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
        apply_overlay(&mut rendered, &overlay);
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
            },
            rect,
        ) else {
            panic!("expected restoration settings on click-away");
        };
        assert_eq!(restored.theme, crate::theme::Theme::ODYSSEY);
        assert!(!overlay.is_open(), "click-away closes the builder");
    }
}
