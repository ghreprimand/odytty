// SPDX-License-Identifier: GPL-3.0-only
//! Overlay input dispatch: winit key mapping, keyboard and pointer routing,
//! and the per-component adapters that translate component outcomes into an
//! [`OverlayOutcome`].

use crate::input::Modifiers;
use crate::native::connection_form::ConnectionFormOutcome;
use crate::native::connection_overlay::{ConnectionOverlayOutcome, ConnectionPickerPurpose};
use crate::native::font_picker::FontPickerOutcome;
use crate::native::key_remap_ui::KeyRemapOutcome;
use crate::native::open_with_overlay::OpenWithOverlayOutcome;
use crate::native::palette_overlay::PaletteOverlayOutcome;
use crate::native::profile_picker::ProfilePickerOutcome;
use crate::native::replay_overlay::ReplayOverlayOutcome;
use crate::native::session_attach_overlay::SessionAttachOverlayOutcome;
use crate::native::settings_panel::SettingsLevel;
use crate::native::theme_builder::ThemeBuilderOutcome;
use crate::native::theme_picker::ThemePickerOutcome;
use crate::native::workspace_picker::WorkspacePickerOutcome;
use crate::selection::CellPoint;
use crate::settings::KeyChord;
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::contracts::{OverlayInput, OverlayMode, OverlayOutcome, OverlayPointer, PointerButton};
use super::layout::OverlayRect;
use super::state::OverlayUi;

impl OverlayUi {
    /// Deliver a raw captured chord to the key-remap modal (KB-REMAP). Only
    /// called by the App while [`Self::is_capturing_chord`] is `true`.
    pub(in crate::native) fn deliver_chord(&mut self, chord: Option<KeyChord>) -> OverlayOutcome {
        let outcome = self.key_remap.deliver_chord(chord);
        self.apply_key_remap_outcome(outcome)
    }

    pub(in crate::native) fn handle_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.mode {
            OverlayMode::ThemePicker => return self.handle_theme_picker_input(input),
            OverlayMode::ThemeBuilder => return self.handle_theme_builder_input(input),
            OverlayMode::ProfileManager => return self.handle_profile_manager_input(input),
            OverlayMode::FontPicker => return self.handle_font_picker_input(input),
            OverlayMode::KeyBindings => return self.handle_key_remap_input(input),
            OverlayMode::Onboarding => return self.handle_onboarding_input(input),
            OverlayMode::ContextMenu => return self.handle_context_menu_input(input),
            OverlayMode::ConfirmClose => return self.handle_confirm_close_input(input),
            OverlayMode::RiskyPaste => return self.handle_risky_paste_input(input),
            OverlayMode::AttachChoice => return self.handle_attach_choice_input(input),
            OverlayMode::ConfirmKillSession => {
                return self.handle_confirm_kill_session_input(input);
            }
            OverlayMode::ConfirmNavigatorClose => {
                return self.handle_confirm_navigator_close_input(input);
            }
            OverlayMode::DetachSwitchChoice => return self.handle_detach_switch_input(input),
            OverlayMode::ConfirmReplaceTab => {
                return self.handle_confirm_replace_tab_input(input);
            }
            OverlayMode::ConfirmRemoveHost => {
                return self.handle_confirm_remove_host_input(input);
            }
            OverlayMode::ConfirmOverwriteLayout => {
                return self.handle_confirm_overwrite_layout_input(input);
            }
            OverlayMode::ConfirmOpenLayout => {
                return self.handle_confirm_open_layout_input(input);
            }
            OverlayMode::CommandPalette => return self.handle_command_palette_input(input),
            OverlayMode::Replay => return self.handle_replay_input(input),
            OverlayMode::Connections => return self.handle_connections_input(input),
            OverlayMode::ConnectionForm => return self.handle_connection_form_input(input),
            OverlayMode::SessionAttach => return self.handle_session_attach_input(input),
            OverlayMode::OpenWith => return self.handle_open_with_input(input),
            OverlayMode::WorkspacePicker => {
                return self.handle_workspace_picker_input(input);
            }
            OverlayMode::ProfilePicker => {
                return self.handle_profile_picker_input(input);
            }
            OverlayMode::ImageView => return self.handle_image_view_input(input),
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
    pub(in crate::native) fn handle_pointer(
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
                    OverlayMode::ProfileManager => {
                        if button == PointerButton::Left {
                            use crate::native::profile_manager::ProfileManagerOutcome;
                            match self.profile_manager.handle_pointer_press(
                                rect.body_width,
                                rect.body_height,
                                row_in_body,
                                col_in_body,
                            ) {
                                ProfileManagerOutcome::Consumed => OverlayOutcome::Consumed,
                                ProfileManagerOutcome::Close => {
                                    if self.picker_return.is_some() {
                                        self.return_to_settings_panel();
                                        OverlayOutcome::Consumed
                                    } else {
                                        self.close();
                                        OverlayOutcome::Close
                                    }
                                }
                                ProfileManagerOutcome::Persist { profile, replace } => {
                                    OverlayOutcome::SaveProfile { profile, replace }
                                }
                                ProfileManagerOutcome::Delete(name) => {
                                    OverlayOutcome::DeleteProfile(name)
                                }
                                ProfileManagerOutcome::RequestImport => {
                                    OverlayOutcome::ImportProfile
                                }
                                ProfileManagerOutcome::RequestExport(name) => {
                                    OverlayOutcome::ExportProfile(name)
                                }
                                ProfileManagerOutcome::SetDefaultLaunchProfile(name) => {
                                    OverlayOutcome::SetDefaultLaunchProfile(name)
                                }
                            }
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ContextMenu => {
                        let outcome =
                            self.context_menu
                                .handle_press(row_in_body, rect.body_height, button);
                        self.apply_context_menu_outcome(outcome)
                    }
                    // UX4-P1 click→Activate parity: a left-click on a list row
                    // selects the row under the pointer (the inverse of each
                    // overlay's `visible_lines` windowing) and routes the SAME
                    // Activate the keyboard uses, so click-on-row-N == Down×N +
                    // Enter. Right-clicks and clicks that miss a row are inert.
                    // Onboarding / Replay / ImageView have no list rows.
                    OverlayMode::ThemePicker => {
                        if button == PointerButton::Left
                            && self.theme_picker.click_row(
                                row_in_body,
                                rect.body_width,
                                rect.body_height,
                            )
                        {
                            self.handle_theme_picker_input(OverlayInput::Activate)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::FontPicker => {
                        if button == PointerButton::Left
                            && self.font_picker.click_row(
                                row_in_body,
                                rect.body_width,
                                rect.body_height,
                            )
                        {
                            self.handle_font_picker_input(OverlayInput::Activate)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::KeyBindings => {
                        if button == PointerButton::Left
                            && self.key_remap.click_row(row_in_body, rect.body_height)
                        {
                            self.handle_key_remap_input(OverlayInput::Activate)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::CommandPalette => {
                        if button == PointerButton::Left
                            && self
                                .command_palette
                                .click_row(row_in_body, rect.body_height)
                        {
                            self.handle_command_palette_input(OverlayInput::Activate)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::Connections => match button {
                        PointerButton::Left => {
                            if self.connections.click_row(row_in_body, rect.body_height) {
                                self.handle_connections_input(OverlayInput::Activate)
                            } else {
                                OverlayOutcome::Consumed
                            }
                        }
                        // ODP-2C: a right-click on a saved-host row opens the
                        // connection-row menu over the still-loaded manager. Off
                        // a real host row (prompt / hint / ad-hoc / past end) it
                        // is inert — no menu, selection untouched.
                        PointerButton::Right => {
                            match self.connections.host_at_row(row_in_body, rect.body_height) {
                                Some((row_index, host)) => {
                                    self.open_connection_row_menu(cell, row_index, host);
                                    OverlayOutcome::Consumed
                                }
                                None => OverlayOutcome::Consumed,
                            }
                        }
                    },
                    OverlayMode::ConnectionForm => {
                        if button == PointerButton::Left {
                            self.handle_connection_form_pointer(
                                row_in_body,
                                col_in_body,
                                rect.body_width,
                            )
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::SessionAttach => match button {
                        // Left-click on a row attaches (unchanged from Phase 5).
                        PointerButton::Left => {
                            if self.session_attach.click_row(row_in_body, rect.body_height) {
                                self.handle_session_attach_input(OverlayInput::Activate)
                            } else {
                                OverlayOutcome::Consumed
                            }
                        }
                        // Right-click on a row asks to kill that session (Manage
                        // Sessions): emit its id so the App opens the confirm
                        // dialog. A right-click off a row is inert. The attach
                        // (left-click) path stays byte-identical.
                        PointerButton::Right => {
                            match self.session_attach.id_at_row(row_in_body, rect.body_height) {
                                Some(id) => OverlayOutcome::KillSessionRequest(id),
                                None => OverlayOutcome::Consumed,
                            }
                        }
                    },
                    OverlayMode::OpenWith => {
                        if button == PointerButton::Left
                            && self.open_with.click_row(row_in_body, rect.body_height)
                        {
                            self.handle_open_with_input(OverlayInput::Activate)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::WorkspacePicker => {
                        if button == PointerButton::Left
                            && self
                                .workspace_picker
                                .click_row(row_in_body, rect.body_height)
                        {
                            self.handle_workspace_picker_input(OverlayInput::Activate)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ProfilePicker => {
                        if button == PointerButton::Left
                            && self.profile_picker.click_row(row_in_body, rect.body_height)
                        {
                            self.handle_profile_picker_input(OverlayInput::Activate)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ConfirmClose => {
                        if button == PointerButton::Left {
                            self.confirm_close_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::RiskyPaste => {
                        if button == PointerButton::Left {
                            self.risky_paste_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::AttachChoice => {
                        if button == PointerButton::Left {
                            self.attach_choice_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ConfirmKillSession => {
                        if button == PointerButton::Left {
                            self.confirm_kill_session_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ConfirmNavigatorClose => {
                        if button == PointerButton::Left {
                            self.confirm_navigator_close_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::DetachSwitchChoice => {
                        if button == PointerButton::Left {
                            self.detach_switch_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ConfirmReplaceTab => {
                        if button == PointerButton::Left {
                            self.confirm_replace_tab_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ConfirmRemoveHost => {
                        if button == PointerButton::Left {
                            self.confirm_remove_host_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ConfirmOverwriteLayout => {
                        if button == PointerButton::Left {
                            self.confirm_overwrite_layout_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::ConfirmOpenLayout => {
                        if button == PointerButton::Left {
                            self.confirm_open_layout_click(row_in_body, col_in_body)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    OverlayMode::Onboarding | OverlayMode::Replay | OverlayMode::ImageView => {
                        OverlayOutcome::Consumed
                    }
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
                        self.context_menu
                            .handle_hover(row_in_body, rect.body_height);
                        OverlayOutcome::Consumed
                    }
                    OverlayMode::ThemePicker
                    | OverlayMode::FontPicker
                    | OverlayMode::KeyBindings
                    | OverlayMode::Onboarding
                    | OverlayMode::CommandPalette
                    | OverlayMode::Replay
                    | OverlayMode::Connections
                    | OverlayMode::SessionAttach
                    | OverlayMode::OpenWith
                    | OverlayMode::WorkspacePicker
                    | OverlayMode::ProfilePicker
                    | OverlayMode::ImageView
                    | OverlayMode::RiskyPaste
                    | OverlayMode::ConfirmClose
                    | OverlayMode::AttachChoice
                    | OverlayMode::ConfirmKillSession
                    | OverlayMode::ConfirmNavigatorClose
                    | OverlayMode::DetachSwitchChoice
                    | OverlayMode::ConfirmReplaceTab
                    | OverlayMode::ConfirmRemoveHost
                    | OverlayMode::ConfirmOverwriteLayout
                    | OverlayMode::ConfirmOpenLayout => OverlayOutcome::Consumed,
                    OverlayMode::ConnectionForm | OverlayMode::ProfileManager => {
                        OverlayOutcome::Consumed
                    }
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
                    | OverlayMode::CommandPalette
                    | OverlayMode::Replay
                    | OverlayMode::Connections
                    | OverlayMode::SessionAttach
                    | OverlayMode::OpenWith
                    | OverlayMode::WorkspacePicker
                    | OverlayMode::ProfilePicker
                    | OverlayMode::ImageView
                    | OverlayMode::RiskyPaste
                    | OverlayMode::ConfirmClose
                    | OverlayMode::AttachChoice
                    | OverlayMode::ConfirmKillSession
                    | OverlayMode::ConfirmNavigatorClose
                    | OverlayMode::DetachSwitchChoice
                    | OverlayMode::ConfirmReplaceTab
                    | OverlayMode::ConfirmRemoveHost
                    | OverlayMode::ConfirmOverwriteLayout
                    | OverlayMode::ConfirmOpenLayout => {}
                    OverlayMode::ConnectionForm | OverlayMode::ProfileManager => {}
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
                    OverlayMode::CommandPalette => {
                        self.command_palette.handle_input(if lines < 0 {
                            OverlayInput::Up
                        } else {
                            OverlayInput::Down
                        });
                    }
                    OverlayMode::Replay => self.replay.scroll_lines(lines),
                    OverlayMode::Connections => self.connections.scroll_lines(lines),
                    OverlayMode::SessionAttach => self.session_attach.scroll_lines(lines),
                    OverlayMode::OpenWith => self.open_with.scroll_lines(lines),
                    OverlayMode::WorkspacePicker => self.workspace_picker.scroll_lines(lines),
                    OverlayMode::ProfilePicker => self.profile_picker.scroll_lines(lines),
                    OverlayMode::ProfileManager => self.profile_manager.scroll_lines(lines),
                    OverlayMode::ContextMenu => {
                        // Wheel moves the focused item (and thus the focus-
                        // derived scroll window), mirroring the picker overlays.
                        self.context_menu.handle_input(if lines < 0 {
                            OverlayInput::Up
                        } else {
                            OverlayInput::Down
                        });
                    }
                    // Onboarding, the close/attach dialogs, and the image viewer
                    // are static, non-scrolling cards: the wheel has nothing to
                    // move.
                    OverlayMode::Onboarding
                    | OverlayMode::RiskyPaste
                    | OverlayMode::ConfirmClose
                    | OverlayMode::AttachChoice
                    | OverlayMode::ConfirmKillSession
                    | OverlayMode::ConfirmNavigatorClose
                    | OverlayMode::DetachSwitchChoice
                    | OverlayMode::ConfirmReplaceTab
                    | OverlayMode::ConfirmRemoveHost
                    | OverlayMode::ConfirmOverwriteLayout
                    | OverlayMode::ConfirmOpenLayout
                    | OverlayMode::ConnectionForm
                    | OverlayMode::ImageView => {}
                }
                OverlayOutcome::Consumed
            }
        }
    }

    /// Whether an overlay drag is in progress. Settings steppers never capture
    /// pointer motion, so this is only true for modes that still drag, such as
    /// the theme builder channel slider.
    pub(in crate::native) fn is_settings_dragging(&self) -> bool {
        match self.mode {
            OverlayMode::Settings => self.panel.is_dragging(),
            OverlayMode::ThemeBuilder => self.theme_builder.is_dragging(),
            OverlayMode::ThemePicker
            | OverlayMode::FontPicker
            | OverlayMode::KeyBindings
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::CommandPalette
            | OverlayMode::Replay
            | OverlayMode::Connections
            | OverlayMode::SessionAttach
            | OverlayMode::OpenWith
            | OverlayMode::WorkspacePicker
            | OverlayMode::ProfilePicker
            | OverlayMode::ImageView
            | OverlayMode::RiskyPaste
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::ConfirmNavigatorClose
            | OverlayMode::DetachSwitchChoice
            | OverlayMode::ConfirmReplaceTab
            | OverlayMode::ConfirmRemoveHost
            | OverlayMode::ConfirmOverwriteLayout
            | OverlayMode::ConfirmOpenLayout => false,
            OverlayMode::ConnectionForm | OverlayMode::ProfileManager => false,
        }
    }

    /// Abandon any in-progress overlay drag WITHOUT closing the overlay. The App
    /// calls this on focus loss while the overlay stays open; no-op unless the
    /// active mode currently holds a pointer-captured drag.
    pub(in crate::native) fn cancel_settings_drag(&mut self) {
        match self.mode {
            OverlayMode::Settings => self.panel.end_slider_drag(),
            OverlayMode::ThemeBuilder => self.theme_builder.end_channel_drag(),
            OverlayMode::ThemePicker
            | OverlayMode::FontPicker
            | OverlayMode::KeyBindings
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::CommandPalette
            | OverlayMode::Replay
            | OverlayMode::Connections
            | OverlayMode::SessionAttach
            | OverlayMode::OpenWith
            | OverlayMode::WorkspacePicker
            | OverlayMode::ProfilePicker
            | OverlayMode::ImageView
            | OverlayMode::RiskyPaste
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::ConfirmNavigatorClose
            | OverlayMode::DetachSwitchChoice
            | OverlayMode::ConfirmReplaceTab
            | OverlayMode::ConfirmRemoveHost
            | OverlayMode::ConfirmOverwriteLayout
            | OverlayMode::ConfirmOpenLayout => {}
            OverlayMode::ConnectionForm | OverlayMode::ProfileManager => {}
        }
    }

    pub(super) fn settings_title_back_hit(&self, cell: CellPoint, rect: OverlayRect) -> bool {
        self.mode == OverlayMode::Settings
            // Both drilled-in levels draw the `← … (Esc = back)` title, so both
            // must accept a click on the arrow. About was a late addition and
            // was missed here (NF15): only SectionDetail matched, so the About
            // back-arrow was click-dead while Esc still worked (the panel's own
            // input path pops About). Close routing through `handle_input` pops
            // either level to the section list — the same path Esc takes.
            && matches!(
                self.panel.current_level(),
                SettingsLevel::SectionDetail { .. } | SettingsLevel::About
            )
            // The ← arrow is drawn at rect.top (the title/border row); also
            // accept rect.top + 1 (the gap row) for a forgiving click target.
            && cell.row >= rect.top
            && cell.row < rect.body_top
            && cell.column >= rect.body_left
            && cell.column < rect.body_left + 3
    }

    /// Whether this mode's title carries the leading `\u{2190}` back/close
    /// affordance. Derived from [`Self::title`] so the painter and the hit-test
    /// can never disagree about which modes offer a clickable arrow.
    pub(super) fn title_has_back_affordance(&self) -> bool {
        self.title().starts_with('\u{2190}')
    }

    /// True if `cell` is in the `\u{2190}` back-arrow hit zone of a picker-style
    /// title row. Covers EVERY non-Settings mode whose title starts with the
    /// arrow — theme/font picker, theme builder, key bindings, replay,
    /// connections, session-attach, open-with, and the image viewer — by reading
    /// the affordance off the title text itself (Settings has its own
    /// level-aware [`Self::settings_title_back_hit`]). A future `\u{2190}`-titled
    /// mode is covered automatically.
    pub(super) fn picker_title_back_hit(&self, cell: CellPoint, rect: OverlayRect) -> bool {
        self.mode != OverlayMode::Settings
            && self.title_has_back_affordance()
            // Accept the title row and the gap row (rect.top through body_top-1)
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
    pub(in crate::native) fn first_stepper_button_cells(
        &self,
        columns: usize,
        rows: usize,
    ) -> Option<(CellPoint, CellPoint)> {
        let rect = super::layout::overlay_rect(self, columns, rows)?;
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

    pub(super) fn handle_theme_picker_input(&mut self, input: OverlayInput) -> OverlayOutcome {
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

    pub(super) fn handle_theme_builder_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.theme_builder.handle_input(input);
        self.apply_builder_outcome(outcome)
    }

    pub(super) fn handle_profile_manager_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        use crate::native::profile_manager::ProfileManagerOutcome;
        match self.profile_manager.handle_input(input) {
            ProfileManagerOutcome::Consumed => OverlayOutcome::Consumed,
            ProfileManagerOutcome::Close => {
                if self.picker_return.is_some() {
                    self.return_to_settings_panel();
                    OverlayOutcome::Consumed
                } else {
                    self.close();
                    OverlayOutcome::Close
                }
            }
            ProfileManagerOutcome::Persist { profile, replace } => {
                OverlayOutcome::SaveProfile { profile, replace }
            }
            ProfileManagerOutcome::Delete(name) => OverlayOutcome::DeleteProfile(name),
            ProfileManagerOutcome::RequestImport => OverlayOutcome::ImportProfile,
            ProfileManagerOutcome::RequestExport(name) => OverlayOutcome::ExportProfile(name),
            ProfileManagerOutcome::SetDefaultLaunchProfile(name) => {
                OverlayOutcome::SetDefaultLaunchProfile(name)
            }
        }
    }

    pub(super) fn handle_font_picker_input(&mut self, input: OverlayInput) -> OverlayOutcome {
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
    pub(super) fn apply_builder_outcome(&mut self, outcome: ThemeBuilderOutcome) -> OverlayOutcome {
        match outcome {
            ThemeBuilderOutcome::Consumed => OverlayOutcome::Consumed,
            ThemeBuilderOutcome::CaptureLiveColors => OverlayOutcome::CaptureThemeColors,
            ThemeBuilderOutcome::Preview(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            ThemeBuilderOutcome::Save(request) => OverlayOutcome::SaveTheme(request),
            ThemeBuilderOutcome::Cancel(theme) => {
                let settings = self.settings_with_theme(theme);
                self.settings = settings.clone();
                // Esc / back-button navigates to wherever the builder was
                // opened from: ThemePicker when entered via its edit row,
                // the settings panel (at the remembered level) when entered
                // via the Themes section action row, and a full close only
                // for the standalone paths (keyboard shortcut, theme
                // capture).
                if self.builder_from_picker {
                    self.builder_from_picker = false;
                    self.mode = OverlayMode::ThemePicker;
                } else if self.picker_return.is_some() {
                    self.return_to_settings_panel();
                } else {
                    self.close();
                }
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
        }
    }

    pub(super) fn handle_key_remap_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.key_remap.handle_input(input);
        self.apply_key_remap_outcome(outcome)
    }

    /// Lift a [`KeyRemapOutcome`] (from the browsing keyboard path or the
    /// chord-capture path) into an [`OverlayOutcome`] — the single mapping
    /// shared by `handle_key_remap_input` and `deliver_chord` so the two entry
    /// points can never diverge.
    pub(super) fn apply_key_remap_outcome(&mut self, outcome: KeyRemapOutcome) -> OverlayOutcome {
        match outcome {
            KeyRemapOutcome::Consumed => OverlayOutcome::Consumed,
            KeyRemapOutcome::Preview(settings) => {
                self.settings = settings.clone();
                OverlayOutcome::ApplySettings(Box::new(settings))
            }
            KeyRemapOutcome::Save(changes) => OverlayOutcome::SaveSettings(changes),
            KeyRemapOutcome::SaveAndClose(changes) => {
                // Persist the working overrides; after the App reports
                // `save_succeeded`, the KeyBindings arm returns to the settings
                // panel (P1-6 dirty-close prompt "save" choice).
                self.key_remap_close_after_save = true;
                OverlayOutcome::SaveSettings(changes)
            }
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
    pub(super) fn handle_onboarding_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Close | OverlayInput::Activate | OverlayInput::Char(' ') => {
                OverlayOutcome::CloseOnboarding
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    pub(super) fn handle_command_palette_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.command_palette.handle_input(input) {
            PaletteOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            PaletteOverlayOutcome::Close => OverlayOutcome::Close,
            PaletteOverlayOutcome::TypeText(text) => {
                self.close();
                OverlayOutcome::PaletteTypeText(text)
            }
            PaletteOverlayOutcome::Action(id) => {
                self.close();
                OverlayOutcome::PaletteAction(id)
            }
        }
    }

    /// Route a key to the replay overlay (Phase 2). Replay is presentation-only,
    /// so it can only scrub (Consumed) or request Close — it never emits an
    /// App-side action.
    pub(super) fn handle_replay_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.replay.handle_input(input) {
            ReplayOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            ReplayOverlayOutcome::Close => OverlayOutcome::Close,
        }
    }

    /// Route a key to the connection-manager overlay (Phase 4). The overlay
    /// type-filters and selects (Consumed), requests Close, or accepts a host
    /// (Connect) which the App turns into a new connection — the overlay never
    /// spawns anything itself.
    pub(super) fn handle_connections_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.connections.handle_input(input) {
            ConnectionOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            ConnectionOverlayOutcome::Close => OverlayOutcome::Close,
            // C12: close the overlay before emitting Connect — otherwise it
            // stays open on top of the freshly-spawned SSH tab and eats every
            // keystroke (possibly a password) into its type-to-filter box until
            // Esc. Mirrors the OpenWith / command-palette arms below.
            ConnectionOverlayOutcome::Connect(host) => {
                self.close();
                OverlayOutcome::Connect(host)
            }
            // Ad-hoc connect + save: close first (same password-safety reason as
            // Connect), then hand the App the host to spawn AND persist.
            ConnectionOverlayOutcome::ConnectAndSave(host) => {
                self.close();
                OverlayOutcome::ConnectAndSave(host)
            }
            ConnectionOverlayOutcome::LaunchProfile(name) => {
                self.close();
                OverlayOutcome::LaunchProfile(name)
            }
            // ODP-1B shared picker: a tagged pick routes per its purpose. Close
            // first (same reason as Connect), then hand the App the chosen host.
            ConnectionOverlayOutcome::Pick(host, purpose) => {
                self.close();
                match purpose {
                    ConnectionPickerPurpose::BindWorkspace => {
                        OverlayOutcome::BindWorkspaceToHost(host.alias)
                    }
                    // RAIL-BIND: bind the clicked rail slot to the picked host.
                    ConnectionPickerPurpose::BindWorkspaceIndex(idx) => {
                        OverlayOutcome::BindWorkspaceAtToHost(idx, host.alias)
                    }
                    // ODP-5D: open the picked host in a new tab adjacent to the
                    // clicked tab, or replace that tab (App gates the destructive
                    // close behind a confirm when a foreground child runs).
                    ConnectionPickerPurpose::ConnectTabAfter(token) => {
                        OverlayOutcome::ConnectHostInTabAfter(host, token)
                    }
                    ConnectionPickerPurpose::ReplaceTab(token) => {
                        OverlayOutcome::ReplaceTabWithHostPicked(host, token)
                    }
                    // The default purpose never emits Pick (it emits Connect),
                    // so this arm is unreachable in practice; route it to Close
                    // defensively rather than panicking.
                    ConnectionPickerPurpose::Connect => OverlayOutcome::Close,
                }
            }
            // REMOTE-UX P4: Tab / \u{2192} in the connection manager switch this
            // overlay into the Add / Edit form. The list's frozen OdyTTY-owned
            // aliases supply the collision guard; the form is a sibling overlay
            // mode, so no App round-trip is needed to open it.
            ConnectionOverlayOutcome::AddConnection => {
                let aliases = self.connections.odytty_aliases();
                self.connection_form.open_add(aliases);
                self.mode = OverlayMode::ConnectionForm;
                OverlayOutcome::Consumed
            }
            ConnectionOverlayOutcome::EditConnection(host) => {
                let aliases = self
                    .connections
                    .odytty_aliases()
                    .into_iter()
                    .filter(|alias| *alias != host.alias)
                    .collect();
                self.connection_form.open_edit(&host, aliases);
                self.mode = OverlayMode::ConnectionForm;
                OverlayOutcome::Consumed
            }
        }
    }

    /// Route a key to the Add / Edit connection form (REMOTE-UX P4). The form
    /// edits presentation state (Consumed), cancels (Close), or accepts (Save)
    /// \u{2014} the App persists the built host; the form never writes.
    pub(super) fn handle_connection_form_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.connection_form.handle_input(input);
        self.apply_connection_form_outcome(outcome)
    }

    /// Route a left-click on a form body row to the form (focus a field or press
    /// an action button).
    pub(super) fn handle_connection_form_pointer(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
        body_width: usize,
    ) -> OverlayOutcome {
        let outcome =
            self.connection_form
                .handle_pointer_press(row_in_body, col_in_body, body_width);
        self.apply_connection_form_outcome(outcome)
    }

    /// Map a form outcome to an App-side outcome. Save closes the overlay first
    /// (the connection was just described, not connected), then hands the App the
    /// built host + edit target to persist.
    pub(super) fn apply_connection_form_outcome(
        &mut self,
        outcome: ConnectionFormOutcome,
    ) -> OverlayOutcome {
        match outcome {
            ConnectionFormOutcome::Consumed => OverlayOutcome::Consumed,
            ConnectionFormOutcome::Close => OverlayOutcome::Close,
            ConnectionFormOutcome::Save { host, edit_target } => {
                self.close();
                OverlayOutcome::SaveConnection { host, edit_target }
            }
            // The form stays open through a Test so the tri-state result renders
            // when the App's background probe lands.
            ConnectionFormOutcome::Test(host) => OverlayOutcome::TestConnection(host),
            // The form stays open; the App scans ~/.ssh (names only) and seeds
            // the browser back through `open_identity_key_browse` (FORM-UX).
            ConnectionFormOutcome::BrowseIdentityKeys => OverlayOutcome::BrowseIdentityKeys,
        }
    }

    /// Route a key to the session-attach summon overlay (Phase 5 / B2). The
    /// overlay type-filters and selects (Consumed), requests Close, or accepts a
    /// session (Attach) which the App attaches into a new tab — the overlay
    /// never attaches anything itself.
    pub(super) fn handle_session_attach_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.session_attach.handle_input(input) {
            SessionAttachOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            SessionAttachOverlayOutcome::Close => OverlayOutcome::Close,
            SessionAttachOverlayOutcome::Attach(id) => OverlayOutcome::AttachSession(id),
            SessionAttachOverlayOutcome::Focus(token) => {
                self.close();
                OverlayOutcome::FocusSession(token)
            }
            SessionAttachOverlayOutcome::NavigatorAction(action) => {
                self.close();
                match &action {
                    crate::native::session_navigator::NavigatorAction::Close(
                        crate::native::session_navigator::NavigatorTarget::Detached(id),
                    ) => OverlayOutcome::KillSessionRequest(id.clone()),
                    crate::native::session_navigator::NavigatorAction::Close(target) => {
                        OverlayOutcome::NavigatorCloseRequest(target.clone())
                    }
                    _ => OverlayOutcome::NavigatorAction(action),
                }
            }
        }
    }

    /// Route a key to the "Open With…" app-picker overlay (C3b). The overlay
    /// type-filters and selects (Consumed), requests Close, or accepts an app
    /// (Open) whose pre-built argv the App hands to `spawn_detached` — the
    /// overlay never spawns anything itself.
    pub(super) fn handle_open_with_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.open_with.handle_input(input) {
            OpenWithOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            OpenWithOverlayOutcome::Close => OverlayOutcome::Close,
            OpenWithOverlayOutcome::Open(argv) => {
                self.close();
                OverlayOutcome::OpenWithApp(argv)
            }
        }
    }

    /// Route a key to the "Move to Workspace" destination picker (W4-v2). The
    /// overlay type-filters and selects (Consumed), requests Close, or accepts a
    /// destination (Move), whose token + chosen workspace index the App splices
    /// -- the overlay never mutates the model itself.
    pub(super) fn handle_workspace_picker_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.workspace_picker.handle_input(input) {
            WorkspacePickerOutcome::Consumed => OverlayOutcome::Consumed,
            WorkspacePickerOutcome::Close => OverlayOutcome::Close,
            WorkspacePickerOutcome::Move(token, dest_ws) => {
                self.close();
                OverlayOutcome::MoveTabToWorkspacePicked(token, dest_ws)
            }
            WorkspacePickerOutcome::OpenLayout(name) => {
                self.close();
                OverlayOutcome::ContextMenuOpenLayout(name)
            }
        }
    }

    pub(super) fn handle_profile_picker_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.profile_picker.handle_input(input) {
            ProfilePickerOutcome::Consumed => OverlayOutcome::Consumed,
            ProfilePickerOutcome::Close => OverlayOutcome::Close,
            ProfilePickerOutcome::NewTab(name) => {
                self.close();
                OverlayOutcome::ProfilePickerNewTab(name)
            }
            ProfilePickerOutcome::NewWorkspace(name) => {
                self.close();
                OverlayOutcome::ProfilePickerNewWorkspace(name)
            }
        }
    }
}

pub(in crate::native) fn overlay_input_from_winit(
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
        WinitKey::Named(NamedKey::Enter) if mods.shift => Some(OverlayInput::ActivateAlt),
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
