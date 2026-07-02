// SPDX-License-Identifier: GPL-3.0-only
use crate::connection_hosts::ConnectionHost;
use crate::core::{Attrs, Cell, Color, Snapshot};
use crate::input::Modifiers;
use crate::selection::CellPoint;
use crate::session_host::ListedSession;
use crate::settings::{KeyChord, Settings};
use crate::theme::{Srgb, Theme};

use unicode_width::UnicodeWidthChar;
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::connection_overlay::{
    ConnectionOverlay, ConnectionOverlayLine, ConnectionOverlayOutcome, ConnectionOverlaySignature,
};
use super::context_menu_ui::{
    CONTEXT_MENU_ITEMS, ContextMenuItem, ContextMenuOutcome, ContextMenuSignature, ContextMenuUi,
};
use super::font_picker::{FontPicker, FontPickerLine, FontPickerOutcome, FontPickerSignature};
use super::key_remap_ui::{KeyRemapLine, KeyRemapOutcome, KeyRemapSignature, KeyRemapUi};
use super::onboarding::{OnboardingLine, OnboardingPanel, OnboardingSignature};
use super::open_with_overlay::{
    OpenWithOverlay, OpenWithOverlayLine, OpenWithOverlayOutcome, OpenWithOverlaySignature,
};
use super::palette_overlay::{
    PaletteOverlay, PaletteOverlayLine, PaletteOverlayOutcome, PaletteOverlaySignature,
};
use super::replay_overlay::{
    ReplayOverlay, ReplayOverlayLine, ReplayOverlayOutcome, ReplayOverlaySignature,
};
use super::session::SessionToken;
use super::session_attach_overlay::{
    SessionAttachOverlay, SessionAttachOverlayLine, SessionAttachOverlayOutcome,
    SessionAttachOverlaySignature,
};
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
    command_palette: PaletteOverlay,
    replay: ReplayOverlay,
    connections: ConnectionOverlay,
    session_attach: SessionAttachOverlay,
    open_with: OpenWithOverlay,
    /// Caption (the image's filename) shown in the C4 image-viewer overlay's
    /// body. The image itself draws through the GPU image layer, over the panel;
    /// this presentation-only string is the only state the viewer mode carries.
    image_view_caption: String,
    /// The pending host session-id carried by the attach-choice dialog (Phase
    /// 14). Set when the dialog opens; the "New tab"/"Replace current" arms emit
    /// it back to the App. Empty when the dialog is not open. The dialog body is
    /// static text, so this is the only state the mode carries (it does not enter
    /// the render signature — the card looks identical for any id).
    attach_choice_session_id: String,
    /// The pending host session-id carried by the kill-confirmation dialog
    /// (Manage Sessions). Set when the dialog opens (right-click a session row);
    /// the confirm arm emits it back to the App, which calls
    /// `session_host::kill_session`. Empty when the dialog is not open. The card
    /// shows a short id hint but does not enter the render signature: the mode
    /// flips through `close()` between distinct kill dialogs, forcing a repaint,
    /// so the carried id never needs to gate the cache (same trick as
    /// `attach_choice_session_id`).
    confirm_kill_session_id: String,
    /// The focused pane's cwd carried by the Detach & switch dialog (Packet 2).
    /// Set when the dialog opens; the Swap / Keep-both arms emit it back to the
    /// App, which spawns a managed session in it. Empty = unknown cwd (spawn in
    /// the default directory). Operator-controlled text (an OSC 7 path), so the
    /// body truncates it to the panel width and it is display-only here — it
    /// re-enters the App only as the `working_directory` of the same spawn config
    /// `odytty new` uses, never a raw shell arg. Not in the render signature: the
    /// card layout is identical for any cwd, and the mode flips through `close()`
    /// between opens, forcing a repaint.
    detach_switch_cwd: String,
    /// Set when a `SaveAndClose` outcome arrives from the settings panel (dirty
    /// close prompt). On the next `save_succeeded` call for Settings mode, the
    /// overlay closes itself after recording the save (SETTINGS-REDESIGN §7).
    close_after_save: bool,
    /// Set when the keybind editor's dirty-close prompt chose "save" (P1-6). On
    /// the next `save_succeeded` for KeyBindings mode, the overlay returns to the
    /// settings panel (rather than the in-modal Ctrl+S behaviour of staying
    /// open). Mirrors `close_after_save` for the keybind-editor lane.
    key_remap_close_after_save: bool,
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
            command_palette: PaletteOverlay::new(),
            replay: ReplayOverlay::new(),
            connections: ConnectionOverlay::new(),
            session_attach: SessionAttachOverlay::new(),
            open_with: OpenWithOverlay::new(),
            image_view_caption: String::new(),
            attach_choice_session_id: String::new(),
            confirm_kill_session_id: String::new(),
            detach_switch_cwd: String::new(),
            close_after_save: false,
            key_remap_close_after_save: false,
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

    /// Refresh the read-only About data on the settings panel (ABOUT). Called by
    /// the App when toggling the settings overlay so the About view reflects the
    /// live GPU adapter (available once the renderer is up).
    pub(super) fn set_about_info(&mut self, about: super::about::AboutInfo) {
        self.panel.set_about(about);
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

    pub(super) fn open_command_palette(&mut self, cwd: Option<&str>) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.command_palette.open_from_process_env(cwd);
        self.mode = OverlayMode::CommandPalette;
        self.open = true;
    }

    /// Open the output-replay overlay over a frozen clone of the focused
    /// session's recorded frames (Phase 2). Presentation-only: the overlay owns
    /// the clone and never touches live core state. `frames` is empty when
    /// recording is off or nothing has been recorded yet, in which case the
    /// overlay shows a hint rather than failing to open.
    pub(super) fn open_replay(&mut self, frames: Vec<Snapshot>) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.replay.open(frames);
        self.mode = OverlayMode::Replay;
        self.open = true;
    }

    /// Open the connection-manager overlay over a frozen list of local
    /// connection candidates (Phase 4). Presentation-only: the overlay owns the
    /// list and never spawns anything itself. Accepting a row emits a
    /// [`OverlayOutcome::Connect`] for the App's connect action. `entries` is
    /// empty when no hosts are configured, in which case the overlay shows a
    /// hint rather than failing to open.
    pub(super) fn open_connections(&mut self, entries: Vec<ConnectionHost>) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.connections.open(entries);
        self.mode = OverlayMode::Connections;
        self.open = true;
    }

    /// Open the in-window session-attach summon overlay over a frozen list of
    /// live detached sessions (Phase 5 / B2). Presentation-only: the overlay
    /// owns the list and never attaches anything itself. Accepting a row emits a
    /// [`OverlayOutcome::AttachSession`] for the App to attach into a new tab.
    /// `entries` is empty when no sessions are live, in which case the overlay
    /// shows a hint rather than failing to open.
    pub(super) fn open_session_attach(&mut self, entries: Vec<ListedSession>) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.session_attach.open(entries);
        self.mode = OverlayMode::SessionAttach;
        self.open = true;
    }

    /// Open the "Open With…" app-picker overlay over a frozen list of apps that
    /// can open the resolved file (C3b). Presentation-only: the App enumerated
    /// the apps (each row carries a pre-built, argv-only command) and this
    /// overlay only displays/filters them. Accepting a row emits a
    /// [`OverlayOutcome::OpenWithApp`] the App hands to `spawn_detached`. An
    /// empty list shows a hint rather than failing to open.
    pub(super) fn open_open_with(&mut self, entries: Vec<crate::desktop::DesktopApp>) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.open_with.open(entries);
        self.mode = OverlayMode::OpenWith;
        self.open = true;
    }

    #[cfg(test)]
    pub(super) fn open_command_palette_for_test<H, S>(&mut self, history: H, cwd: Option<&str>)
    where
        H: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.command_palette.open_for_test(history, cwd);
        self.mode = OverlayMode::CommandPalette;
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
    // A thin forwarding shim retained for the existing overlay tests, which call
    // the pre-hint 9-arg form. Production opens via
    // `open_context_menu_with_prompt_editing_hint`; this defaults the hint off.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_context_menu(
        &mut self,
        spawn: CellPoint,
        copy: bool,
        cut: bool,
        paste: bool,
        delete: bool,
        rename_target: Option<SessionToken>,
        multi_pane: bool,
        path_target: Option<crate::paths::Resolved>,
        accelerators: [Option<String>; CONTEXT_MENU_ITEMS],
    ) {
        self.open_context_menu_with_prompt_editing_hint(
            spawn,
            copy,
            cut,
            paste,
            delete,
            false,
            rename_target,
            multi_pane,
            path_target,
            accelerators,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_context_menu_with_prompt_editing_hint(
        &mut self,
        spawn: CellPoint,
        copy: bool,
        cut: bool,
        paste: bool,
        delete: bool,
        prompt_editing_hint: bool,
        rename_target: Option<SessionToken>,
        multi_pane: bool,
        path_target: Option<crate::paths::Resolved>,
        accelerators: [Option<String>; CONTEXT_MENU_ITEMS],
    ) {
        self.panel.end_slider_drag();
        self.context_menu.open_with_prompt_editing_hint(
            spawn,
            copy,
            cut,
            paste,
            delete,
            prompt_editing_hint,
            rename_target,
            multi_pane,
            path_target,
        );
        self.context_menu.set_accelerators(accelerators);
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

    /// Open the in-terminal image-viewer overlay (Phase 9 / C4). The decoded
    /// image is uploaded into the GPU image layer by the App; this overlay only
    /// carries the presentation-only caption (the filename) and owns the
    /// open/close lifecycle. Esc dismisses; the App clears the GPU overlay image
    /// when the viewer is no longer open. Presentation-only — the live terminal
    /// stays unchanged behind it.
    pub(super) fn open_image_view(&mut self, caption: String) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.image_view_caption = caption;
        self.mode = OverlayMode::ImageView;
        self.open = true;
    }

    /// Whether the C4 image viewer is the active overlay mode. The App polls
    /// this each frame to clear the GPU overlay image once the viewer closes
    /// (via Esc, click-outside, or any mode switch).
    pub(super) fn image_view_open(&self) -> bool {
        self.open && self.mode == OverlayMode::ImageView
    }

    /// Keyboard contract for the image viewer (C4): Esc / Enter dismisses; every
    /// other key is swallowed so nothing leaks to the PTY behind the overlay.
    /// Dismissal emits `Close`; the App's per-frame sync clears the GPU overlay
    /// image so the next frame is byte-identical.
    fn handle_image_view_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Close | OverlayInput::Activate => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
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

    /// Hit-test a left-click in the close-confirmation dialog body (UX4-P1
    /// click→Activate parity). The action line ([`CONFIRM_CLOSE_ACTION_LINE`])
    /// is the 3rd body row (index 2); a click on the "Yes" region confirms
    /// (`ForceClose`), the "No" region cancels (`Close`), and anywhere else —
    /// including the leading prompt text and the other two rows — is inert so a
    /// stray click never destroys a running job (TRAP-2: the dismiss path never
    /// emits `ForceClose`). `col_in_body` indexes the left-aligned body line; the
    /// line is ASCII, so byte offsets equal columns.
    fn confirm_close_click(&mut self, row_in_body: usize, col_in_body: usize) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_CLOSE_ACTION_LINE;
        let yes_start = text.find("[Enter").unwrap_or(0);
        let no_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= no_start {
            OverlayOutcome::Close
        } else if col_in_body >= yes_start {
            self.close();
            OverlayOutcome::ForceClose
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Open the attach-choice dialog (Phase 14) for host `session_id`. Called by
    /// the App when the selected session is NOT already open in a tab (the
    /// already-open case dedups by switching, never reaching here). Idempotent:
    /// starts with `close()` so a repeated open cannot stack dialogs. The id is
    /// stashed so the New-tab / Replace arms can emit it back to the App.
    pub(super) fn open_attach_choice(&mut self, session_id: String) {
        self.close();
        self.attach_choice_session_id = session_id;
        self.mode = OverlayMode::AttachChoice;
        self.open = true;
    }

    /// Keyboard contract for the attach-choice dialog (Phase 14). `N`/Enter
    /// attaches in a new tab; `R` replaces the current tab; Esc/`Close` cancels;
    /// every other key is swallowed so nothing leaks to the PTY behind the modal.
    /// Both action arms close the dialog before emitting, mirroring
    /// `ConfirmClose`, and carry the stashed host session-id.
    fn handle_attach_choice_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                let id = std::mem::take(&mut self.attach_choice_session_id);
                self.close();
                OverlayOutcome::AttachChoiceNewTab(id)
            }
            OverlayInput::Char('r') | OverlayInput::Char('R') => {
                let id = std::mem::take(&mut self.attach_choice_session_id);
                self.close();
                OverlayOutcome::AttachChoiceReplace(id)
            }
            OverlayInput::Close => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the attach-choice dialog body (Phase 14;
    /// click→key parity, mirroring `confirm_close_click`). The action line
    /// ([`ATTACH_CHOICE_ACTION_LINE`]) is the 3rd body row (index 2); a click on
    /// the "New tab" region attaches in a new tab, the "Replace" region replaces
    /// the current tab, and anywhere else — including the leading prompt text and
    /// the other rows — is inert so a stray click never attaches. ASCII line, so
    /// byte offsets equal columns.
    fn attach_choice_click(&mut self, row_in_body: usize, col_in_body: usize) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = ATTACH_CHOICE_ACTION_LINE;
        let new_start = text.find("[N").unwrap_or(0);
        let replace_start = text.find("[R").unwrap_or(text.len());
        if col_in_body >= replace_start {
            let id = std::mem::take(&mut self.attach_choice_session_id);
            self.close();
            OverlayOutcome::AttachChoiceReplace(id)
        } else if col_in_body >= new_start {
            let id = std::mem::take(&mut self.attach_choice_session_id);
            self.close();
            OverlayOutcome::AttachChoiceNewTab(id)
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Whether the attach-choice dialog is the active overlay mode (Phase 14).
    /// Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(super) fn is_attach_choice(&self) -> bool {
        self.open && self.mode == OverlayMode::AttachChoice
    }

    /// Open the kill-confirmation dialog (Manage Sessions) for host
    /// `session_id`. Called by the App when the user right-clicks a session row
    /// in the manager. Idempotent: starts with `close()` so a repeated open
    /// cannot stack dialogs. The id is stashed so the confirm arm can emit it
    /// back to the App, which runs `session_host::kill_session`.
    pub(super) fn open_confirm_kill_session(&mut self, session_id: String) {
        self.close();
        self.confirm_kill_session_id = session_id;
        self.mode = OverlayMode::ConfirmKillSession;
        self.open = true;
    }

    /// Keyboard contract for the kill-confirmation dialog (Manage Sessions).
    /// Enter or Y confirms (emits [`OverlayOutcome::KillSessionConfirmed`] with
    /// the stashed id); Esc or N cancels (closes the dialog, kills nothing);
    /// every other key is swallowed so nothing leaks to the PTY behind the
    /// modal. The confirm arm closes the dialog before emitting, mirroring
    /// `ConfirmClose`.
    fn handle_confirm_kill_session_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('y') | OverlayInput::Char('Y') => {
                let id = std::mem::take(&mut self.confirm_kill_session_id);
                self.close();
                OverlayOutcome::KillSessionConfirmed(id)
            }
            OverlayInput::Close | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                OverlayOutcome::Close
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the kill-confirmation dialog body (click→key
    /// parity, mirroring `confirm_close_click`). The action line
    /// ([`CONFIRM_KILL_SESSION_ACTION_LINE`]) is the 3rd body row (index 2); a
    /// click on the "Kill" region confirms, the "Cancel" region cancels, and
    /// anywhere else — including the leading prompt text and the other rows — is
    /// inert so a stray click never kills a session. ASCII line, so byte offsets
    /// equal columns.
    fn confirm_kill_session_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_KILL_SESSION_ACTION_LINE;
        let kill_start = text.find("[Enter").unwrap_or(0);
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            OverlayOutcome::Close
        } else if col_in_body >= kill_start {
            let id = std::mem::take(&mut self.confirm_kill_session_id);
            self.close();
            OverlayOutcome::KillSessionConfirmed(id)
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Whether the kill-confirmation dialog is the active overlay mode (Manage
    /// Sessions). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(super) fn is_confirm_kill_session(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmKillSession
    }

    /// Open the Detach & switch choice dialog (Packet 2) for the focused pane's
    /// `cwd` (empty = unknown → spawn in the default directory). Called by the
    /// App after it reads the focused pane's cwd (the overlay cannot read the
    /// terminal). Idempotent: starts with `close()` so a repeated open cannot
    /// stack dialogs. The cwd is stashed so the Swap / Keep-both arms can emit
    /// it back to the App.
    pub(super) fn open_detach_switch_choice(&mut self, cwd: String) {
        self.close();
        self.detach_switch_cwd = cwd;
        self.mode = OverlayMode::DetachSwitchChoice;
        self.open = true;
    }

    /// Keyboard contract for the Detach & switch dialog (Packet 2). `S` swaps
    /// (spawn + close this pane); `K` keeps both (spawn + leave this pane); Esc
    /// cancels; every other key — INCLUDING Enter — is swallowed. There is no
    /// Enter default on purpose: Swap is destructive (it closes the focused
    /// pane), so the user must explicitly choose S or K. Both action arms close
    /// the dialog before emitting, mirroring `ConfirmClose`, and carry the
    /// stashed cwd.
    fn handle_detach_switch_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Char('s') | OverlayInput::Char('S') => {
                let cwd = std::mem::take(&mut self.detach_switch_cwd);
                self.close();
                OverlayOutcome::DetachSwitchSwap(cwd)
            }
            OverlayInput::Char('k') | OverlayInput::Char('K') => {
                let cwd = std::mem::take(&mut self.detach_switch_cwd);
                self.close();
                OverlayOutcome::DetachSwitchKeepBoth(cwd)
            }
            OverlayInput::Close => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the Detach & switch dialog body (click→key
    /// parity, mirroring `attach_choice_click`). The action line
    /// ([`DETACH_SWITCH_ACTION_LINE`]) is the 4th body row (index 3); a click on
    /// the "Swap" region swaps, the "Keep both" region keeps both, the "Cancel"
    /// region cancels, and anywhere else — including the leading prompt text and
    /// the other rows — is inert so a stray click never spawns or closes a pane.
    /// ASCII line, so byte offsets equal columns. Regions are tested right-to-
    /// left (Cancel, then Keep, then Swap) since their anchors are ordered S < K
    /// < Esc in the line.
    fn detach_switch_click(&mut self, row_in_body: usize, col_in_body: usize) -> OverlayOutcome {
        const ACTION_ROW: usize = 3;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = DETACH_SWITCH_ACTION_LINE;
        let swap_start = text.find("[S").unwrap_or(0);
        let keep_start = text.find("[K").unwrap_or(text.len());
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            OverlayOutcome::Close
        } else if col_in_body >= keep_start {
            let cwd = std::mem::take(&mut self.detach_switch_cwd);
            self.close();
            OverlayOutcome::DetachSwitchKeepBoth(cwd)
        } else if col_in_body >= swap_start {
            let cwd = std::mem::take(&mut self.detach_switch_cwd);
            self.close();
            OverlayOutcome::DetachSwitchSwap(cwd)
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Whether the Detach & switch dialog is the active overlay mode (Packet 2).
    /// Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(super) fn is_detach_switch_choice(&self) -> bool {
        self.open && self.mode == OverlayMode::DetachSwitchChoice
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
                    ContextMenuItem::NewWindow => OverlayOutcome::ContextMenuNewWindow,
                    ContextMenuItem::RenameTab => {
                        if let Some(target) = self.context_menu.rename_target() {
                            OverlayOutcome::ContextMenuRenameTab(target)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    ContextMenuItem::CloseTab => OverlayOutcome::ContextMenuCloseTab,
                    ContextMenuItem::SplitColumns => OverlayOutcome::ContextMenuSplitColumns,
                    ContextMenuItem::SplitRows => OverlayOutcome::ContextMenuSplitRows,
                    ContextMenuItem::ClosePane => OverlayOutcome::ContextMenuClosePane,
                    ContextMenuItem::Settings => OverlayOutcome::ContextMenuSettings,
                    ContextMenuItem::ConnectionManager => {
                        OverlayOutcome::ContextMenuConnectionManager
                    }
                    ContextMenuItem::CommandPalette => OverlayOutcome::ContextMenuCommandPalette,
                    ContextMenuItem::SessionReplay => OverlayOutcome::ContextMenuSessionReplay,
                    ContextMenuItem::SessionAttach => OverlayOutcome::ContextMenuSessionAttach,
                    // C3 file section: build each outcome from the resolved path
                    // snapshotted at open time (survives the `close()` above, as
                    // the rename-target read does). A missing target (defensive —
                    // these items are only visible with a target) is inert.
                    ContextMenuItem::OpenPath => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuOpenPath(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    // C4: only ever visible on a resolved image span; build the
                    // viewer-open outcome from the snapshotted target.
                    ContextMenuItem::OpenInOdytty => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuOpenInOdytty(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    // C3b: only ever visible on a resolved file span; the App
                    // enumerates the handler apps and opens the picker overlay.
                    ContextMenuItem::OpenWith => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuOpenWith(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::CopyPath => match self.context_menu.path_target() {
                        Some(resolved) => OverlayOutcome::ContextMenuCopyPath(resolved.abs.clone()),
                        None => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::CopyFile => match self.context_menu.path_target() {
                        Some(resolved) => OverlayOutcome::ContextMenuCopyFile(
                            super::app::interactive_paths::file_uri(&resolved.abs),
                        ),
                        None => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::RevealPath => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuRevealPath(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    // Packet 2: the menu closed itself; the App reads the focused
                    // pane's cwd (it owns the terminal lock) and opens the 3-way
                    // Detach & switch choice dialog. The overlay cannot read the
                    // terminal, so the cwd is resolved App-side, not here.
                    ContextMenuItem::DetachSwitch => OverlayOutcome::ContextMenuDetachSwitch,
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
            OverlayMode::AttachChoice => return self.handle_attach_choice_input(input),
            OverlayMode::ConfirmKillSession => {
                return self.handle_confirm_kill_session_input(input);
            }
            OverlayMode::DetachSwitchChoice => return self.handle_detach_switch_input(input),
            OverlayMode::CommandPalette => return self.handle_command_palette_input(input),
            OverlayMode::Replay => return self.handle_replay_input(input),
            OverlayMode::Connections => return self.handle_connections_input(input),
            OverlayMode::SessionAttach => return self.handle_session_attach_input(input),
            OverlayMode::OpenWith => return self.handle_open_with_input(input),
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
                        if button == PointerButton::Left && self.key_remap.click_row(row_in_body) {
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
                    OverlayMode::Connections => {
                        if button == PointerButton::Left
                            && self.connections.click_row(row_in_body, rect.body_height)
                        {
                            self.handle_connections_input(OverlayInput::Activate)
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
                    OverlayMode::ConfirmClose => {
                        if button == PointerButton::Left {
                            self.confirm_close_click(row_in_body, col_in_body)
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
                    OverlayMode::DetachSwitchChoice => {
                        if button == PointerButton::Left {
                            self.detach_switch_click(row_in_body, col_in_body)
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
                    | OverlayMode::ImageView
                    | OverlayMode::ConfirmClose
                    | OverlayMode::AttachChoice
                    | OverlayMode::ConfirmKillSession
                    | OverlayMode::DetachSwitchChoice => OverlayOutcome::Consumed,
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
                    | OverlayMode::ImageView
                    | OverlayMode::ConfirmClose
                    | OverlayMode::AttachChoice
                    | OverlayMode::ConfirmKillSession
                    | OverlayMode::DetachSwitchChoice => {}
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
                    | OverlayMode::ConfirmClose
                    | OverlayMode::AttachChoice
                    | OverlayMode::ConfirmKillSession
                    | OverlayMode::DetachSwitchChoice
                    | OverlayMode::ImageView => {}
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
            | OverlayMode::CommandPalette
            | OverlayMode::Replay
            | OverlayMode::Connections
            | OverlayMode::SessionAttach
            | OverlayMode::OpenWith
            | OverlayMode::ImageView
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::DetachSwitchChoice => false,
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
            | OverlayMode::CommandPalette
            | OverlayMode::Replay
            | OverlayMode::Connections
            | OverlayMode::SessionAttach
            | OverlayMode::OpenWith
            | OverlayMode::ImageView
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::DetachSwitchChoice => {}
        }
    }

    fn settings_title_back_hit(&self, cell: CellPoint, rect: OverlayRect) -> bool {
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
            // KB-REMAP stays open after an in-modal Ctrl+S so the user can keep
            // editing; the modal reports the saved count and adopts the persisted
            // bindings as its new restore baseline. But if the save came from the
            // dirty-close prompt's "save" choice (P1-6), return to the settings
            // panel after recording it.
            OverlayMode::KeyBindings => {
                self.key_remap.save_succeeded(changed);
                if std::mem::take(&mut self.key_remap_close_after_save) {
                    self.return_to_settings_panel();
                }
            }
            // The onboarding card, context menu, and close dialog have no save
            // path of their own.
            OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::CommandPalette
            | OverlayMode::Replay
            | OverlayMode::Connections
            | OverlayMode::SessionAttach
            | OverlayMode::OpenWith
            | OverlayMode::ImageView
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::DetachSwitchChoice => {}
        }
    }

    pub(super) fn save_failed(&mut self, message: String) {
        match self.mode {
            OverlayMode::Settings => self.panel.save_failed(message),
            OverlayMode::ThemePicker => self.theme_picker.save_failed(message),
            OverlayMode::ThemeBuilder => self.theme_builder.save_failed(message),
            OverlayMode::FontPicker => self.font_picker.save_failed(message),
            OverlayMode::KeyBindings => self.key_remap.save_failed(message),
            OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::CommandPalette
            | OverlayMode::Replay
            | OverlayMode::Connections
            | OverlayMode::SessionAttach
            | OverlayMode::OpenWith
            | OverlayMode::ImageView
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::DetachSwitchChoice => {}
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

    /// Whether the open centered overlay has hidden body rows above / below the
    /// visible window, for the shared scroll affordance (OVERLAY-SMALL-WINDOW).
    /// `(false, false)` whenever the body fits, so a normal window draws no
    /// arrows and stays byte-identical. The context menu draws its own arrows
    /// (it is not a centered panel), so it returns `(false, false)` here. Each
    /// list-shaped overlay owns its windowing math and exposes a
    /// `scroll_indicator`; this just dispatches to the active mode's.
    pub(super) fn scroll_arrows(&self, body_height: usize) -> (bool, bool) {
        match self.mode {
            OverlayMode::Settings => self.panel.scroll_indicator(body_height),
            OverlayMode::ThemePicker => self.theme_picker.scroll_indicator(body_height),
            OverlayMode::FontPicker => self.font_picker.scroll_indicator(body_height),
            OverlayMode::KeyBindings => self.key_remap.scroll_indicator(body_height),
            OverlayMode::Connections => self.connections.scroll_indicator(body_height),
            OverlayMode::SessionAttach => self.session_attach.scroll_indicator(body_height),
            OverlayMode::OpenWith => self.open_with.scroll_indicator(body_height),
            OverlayMode::CommandPalette => self.command_palette.scroll_indicator(body_height),
            OverlayMode::ThemeBuilder => self.theme_builder.scroll_indicator(body_height),
            // Replay (read-only frame preview whose scroll axis is time, not a
            // list) keeps a different body model; it draws no list affordance
            // here. Its scrubbing and the static Onboarding/close cards have
            // nothing to window.
            OverlayMode::Replay
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::ImageView
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::DetachSwitchChoice => (false, false),
        }
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
            command_palette: self.command_palette.render_signature(),
            replay: self.replay.render_signature(),
            connections: self.connections.render_signature(),
            session_attach: self.session_attach.render_signature(),
            open_with: self.open_with.render_signature(),
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
    fn handle_onboarding_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Close | OverlayInput::Activate | OverlayInput::Char(' ') => {
                OverlayOutcome::CloseOnboarding
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    fn handle_command_palette_input(&mut self, input: OverlayInput) -> OverlayOutcome {
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
    fn handle_replay_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.replay.handle_input(input) {
            ReplayOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            ReplayOverlayOutcome::Close => OverlayOutcome::Close,
        }
    }

    /// Route a key to the connection-manager overlay (Phase 4). The overlay
    /// type-filters and selects (Consumed), requests Close, or accepts a host
    /// (Connect) which the App turns into a new connection — the overlay never
    /// spawns anything itself.
    fn handle_connections_input(&mut self, input: OverlayInput) -> OverlayOutcome {
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
        }
    }

    /// Route a key to the session-attach summon overlay (Phase 5 / B2). The
    /// overlay type-filters and selects (Consumed), requests Close, or accepts a
    /// session (Attach) which the App attaches into a new tab — the overlay
    /// never attaches anything itself.
    fn handle_session_attach_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.session_attach.handle_input(input) {
            SessionAttachOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            SessionAttachOverlayOutcome::Close => OverlayOutcome::Close,
            SessionAttachOverlayOutcome::Attach(id) => OverlayOutcome::AttachSession(id),
        }
    }

    /// Route a key to the "Open With…" app-picker overlay (C3b). The overlay
    /// type-filters and selects (Consumed), requests Close, or accepts an app
    /// (Open) whose pre-built argv the App hands to `spawn_detached` — the
    /// overlay never spawns anything itself.
    fn handle_open_with_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match self.open_with.handle_input(input) {
            OpenWithOverlayOutcome::Consumed => OverlayOutcome::Consumed,
            OpenWithOverlayOutcome::Close => OverlayOutcome::Close,
            OpenWithOverlayOutcome::Open(argv) => {
                self.close();
                OverlayOutcome::OpenWithApp(argv)
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
    /// Launch another OdyTTY window from the context menu (F1). The overlay has
    /// already closed itself; the App dispatches this to `handle_new_window`
    /// (the same handler the `Ctrl+Shift+N` chord fires).
    ContextMenuNewWindow,
    ContextMenuRenameTab(SessionToken),
    ContextMenuCloseTab,
    /// Split the focused pane from the context menu (Part B). The overlay has
    /// already closed itself; the App dispatches these to the same
    /// `split_active_pane` the keyboard split chords fire.
    ContextMenuSplitColumns,
    ContextMenuSplitRows,
    /// Close the focused pane from the context menu (multi-pane only). The
    /// overlay has already closed itself; the App dispatches this to the same
    /// `apply_pane_action(ClosePane)` the tmux `Ctrl-b x` prefix / palette fire.
    ContextMenuClosePane,
    /// Open the settings panel from the context menu (D-IN2-SETTINGS). The
    /// overlay has already closed itself; the App opens the settings panel
    /// through the existing toggle path.
    ContextMenuSettings,
    /// Open the connection manager / command palette / session replay overlays
    /// from the context menu's launcher section (v0.3.1 discoverability). The
    /// menu has already closed itself; the App opens each through the same entry
    /// the `Ctrl+Shift+S` / `Ctrl+Shift+P` / `Ctrl+Shift+R` chords fire.
    ContextMenuConnectionManager,
    ContextMenuCommandPalette,
    ContextMenuSessionReplay,
    /// Open the session-attach overlay from the context menu's launcher section
    /// (Phase 5 / B2). The menu has already closed itself; the App opens it
    /// through the same entry the `Ctrl+Shift+A` chord fires.
    ContextMenuSessionAttach,
    /// Open a resolved interactive path from the context menu's file section
    /// (Phase 8 / C3). The menu has already closed itself; the App dispatches
    /// through the same argv-only `path_open_argv` + `spawn_detached` the
    /// Ctrl+click path uses. Boxed to keep this short-lived enum small.
    ContextMenuOpenPath(Box<crate::paths::Resolved>),
    /// Open a resolved image span in the in-terminal viewer from the context
    /// menu's file section (Phase 9 / C4). The menu has already closed itself;
    /// the App decodes the image (with the FLAG B decode bound), uploads it to
    /// the GPU image layer, and opens the `ImageView` overlay. Boxed to keep
    /// this short-lived enum small.
    ContextMenuOpenInOdytty(Box<crate::paths::Resolved>),
    /// Open the "Open With…" app picker for a resolved file from the context
    /// menu's file section (C3b). The menu has already closed itself; the App
    /// enumerates the handler apps (`crate::desktop::enumerate_open_with`) and
    /// opens the `OpenWith` overlay. Boxed to keep this short-lived enum small.
    ContextMenuOpenWith(Box<crate::paths::Resolved>),
    /// Launch a chosen application from the "Open With…" picker (C3b). The
    /// overlay has already closed itself; the App hands the pre-built, argv-only
    /// command (path already a single inert element) to `spawn_detached`. Never
    /// a shell string.
    OpenWithApp(Vec<String>),
    /// Copy the resolved absolute path to the clipboard as text (C3).
    ContextMenuCopyPath(String),
    /// Copy a `file://<abs>` URI to the clipboard as text (C3). The clipboard is
    /// text-only; this pastes into file managers as a file reference.
    ContextMenuCopyFile(String),
    /// Reveal the resolved path in the desktop file manager (C3). Carries the
    /// full [`crate::paths::Resolved`] so the platform-opener seam picks the
    /// per-OS reveal verb at the App boundary: Linux opens the parent directory
    /// (a file) or the path itself (a directory) with `xdg-open`; macOS reveals
    /// the file ITSELF with `open -R`. Boxed to keep this short-lived enum small.
    ContextMenuRevealPath(Box<crate::paths::Resolved>),
    /// Type text accepted from the command palette into the active pane's PTY.
    /// The App writes the exact bytes with no trailing newline.
    PaletteTypeText(String),
    /// Run a local terminal action accepted from the command palette.
    PaletteAction(String),
    /// Connect to a host accepted from the connection-manager overlay (Phase 4).
    /// The overlay has already closed itself by the time this is emitted; the
    /// App's connect action spawns the connection (e.g. `ssh <host>`) in a new
    /// session. Boxed to keep this short-lived enum small. Carries the full
    /// [`ConnectionHost`] so the connect action has alias, host name, user, and
    /// port without re-reading any file.
    Connect(Box<ConnectionHost>),
    /// Attach to a live session accepted from the session-attach overlay (Phase
    /// 5 / B2). The overlay has already closed itself by the time this is
    /// emitted; the App attaches the session id into a new tab. A stale id (the
    /// session ended between list and accept) is swallowed gracefully, never
    /// panics.
    AttachSession(String),
    /// The user confirmed the close-confirmation dialog (CLOSE-CONFIRM): close
    /// the window. The overlay has already closed itself by the time this is
    /// emitted; the App sets its `pending_exit` flag and exits the event loop on
    /// the same turn (the outcome can't reach `ActiveEventLoop` directly).
    ForceClose,
    /// The user chose "New tab" in the attach-choice dialog (Phase 14): attach
    /// the carried host session-id in a new tab. The overlay has already closed
    /// itself; the App runs the existing new-tab attach path.
    AttachChoiceNewTab(String),
    /// The user chose "Replace current" in the attach-choice dialog (Phase 14):
    /// attach the carried host session-id in a new tab, then close the tab that
    /// was active when the manager opened. The overlay has already closed itself.
    AttachChoiceReplace(String),
    /// The user right-clicked a session row in the manager (Manage Sessions):
    /// open the kill-confirmation dialog for the carried host session-id. The
    /// App calls `open_confirm_kill_session`; the manager stays the prior
    /// surface to return to logically, but the dialog replaces it on screen.
    KillSessionRequest(String),
    /// The user confirmed the kill-confirmation dialog (Manage Sessions):
    /// terminate the carried host session-id. The overlay has already closed
    /// itself; the App calls `session_host::kill_session` and refreshes the
    /// manager so the row disappears.
    KillSessionConfirmed(String),
    /// The user chose "Detach & switch" on the focused pane (Packet 2). The menu
    /// has already closed itself; the App reads the focused pane's cwd and opens
    /// the [`OverlayMode::DetachSwitchChoice`] dialog.
    ContextMenuDetachSwitch,
    /// The user chose "Swap (close this)" in the Detach & switch dialog (Packet
    /// 2): spawn a fresh managed session in the carried cwd, attach + focus it,
    /// then close the original focused pane. The overlay has already closed
    /// itself. Empty string = unknown cwd (spawn in the default directory). The
    /// cwd is display+spawn-config only — it flows into the same
    /// `working_directory` `odytty new` uses, never a raw shell arg.
    DetachSwitchSwap(String),
    /// The user chose "Keep both" in the Detach & switch dialog (Packet 2):
    /// spawn a fresh managed session in the carried cwd, attach + focus it, and
    /// leave the original pane untouched. The overlay has already closed itself.
    /// Empty string = unknown cwd (spawn in the default directory).
    DetachSwitchKeepBoth(String),
    /// Open a project URL from the About view (ABOUT). The overlay stays open;
    /// the App routes the URL through the same allowlisted opener the bare-URL /
    /// OSC 8 paths use.
    SettingsOpenUrl(String),
    /// Copy the About diagnostics block to the clipboard (ABOUT). The overlay
    /// stays open; the App writes the text via the native clipboard.
    SettingsCopyDiagnostics(String),
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
            SettingsPanelOutcome::OpenUrl(url) => OverlayOutcome::SettingsOpenUrl(url),
            SettingsPanelOutcome::CopyToClipboard(text) => {
                OverlayOutcome::SettingsCopyDiagnostics(text)
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
    /// Command palette over local actions, shell-history rows, and recent OSC 7
    /// directories. Presentation-only while open.
    CommandPalette,
    /// Output-replay overlay (Phase 2): scrub a frozen clone of the focused
    /// session's recorded screen frames. Presentation-only; never mutates live
    /// core state.
    Replay,
    /// Connection-manager overlay (Phase 4): list saved hosts, type-to-filter,
    /// and quick-connect. Presentation-only; accepting a row emits a connect
    /// request for the App to spawn. Never mutates live core state.
    Connections,
    /// Session-attach summon overlay (Phase 5 / B2): list live detached
    /// sessions, type-to-filter, and attach into a new tab. Presentation-only;
    /// accepting a row emits an attach request for the App. Never mutates live
    /// core state.
    SessionAttach,
    /// "Open With…" app-picker overlay (C3b): list the applications that can
    /// open a resolved file, type-to-filter, and on Enter launch the chosen app
    /// (argv-only, via `spawn_detached`). Presentation-only; the rows carry
    /// pre-built argv. Never mutates live core state.
    OpenWith,
    /// In-terminal image viewer (Phase 9 / C4): a presentation-only overlay
    /// that renders a decoded image span ("Open in OdyTTY") centered over a
    /// dimmed backdrop panel, through the existing GPU image-layer raster path.
    /// Esc dismisses; closed = live frame byte-identical.
    ImageView,
    /// Close-confirmation dialog (CLOSE-CONFIRM). A centered, static two-line
    /// modal shown when a close is requested while a foreground job is running;
    /// Enter/Y confirms (emits [`OverlayOutcome::ForceClose`]), Esc/N cancels.
    ConfirmClose,
    /// Attach-choice dialog (Phase 14). A centered, static modal shown when the
    /// user selects a detached session that is NOT already open in a tab:
    /// `[N / Enter]` attaches it in a new tab, `[R]` replaces the current tab,
    /// Esc cancels. (When the session IS already open, the App dedups by
    /// switching to its tab and this dialog never appears.) Modeled on
    /// `ConfirmClose`; the pending host session-id is carried on the overlay.
    AttachChoice,
    /// Kill-confirmation dialog (Manage Sessions). A centered, static modal
    /// shown when the user right-clicks a session row in the manager:
    /// `[Enter / Y]` kills the session (emits
    /// [`OverlayOutcome::KillSessionConfirmed`]), `[Esc / N]` cancels. Modeled
    /// on `ConfirmClose`; the pending host session-id is carried on the overlay.
    ConfirmKillSession,
    /// Detach & switch choice dialog (Packet 2). A centered, static 3-way modal
    /// shown when the user picks "Detach & switch" on the focused pane: `[S]`
    /// swaps (spawn a managed session + close this pane), `[K]` keeps both
    /// (spawn + leave this pane), `[Esc]` cancels. Honest framing: a SPAWN of a
    /// fresh shell in the same cwd, not a live-process migration. Modeled on
    /// `AttachChoice`; the focused pane's cwd is carried on the overlay.
    DetachSwitchChoice,
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
    pub(super) command_palette: PaletteOverlaySignature,
    pub(super) replay: ReplayOverlaySignature,
    pub(super) connections: ConnectionOverlaySignature,
    pub(super) session_attach: SessionAttachOverlaySignature,
    pub(super) open_with: OpenWithOverlaySignature,
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

/// The close-confirmation dialog's action line, shared by the body builder and
/// the click hit-test ([`OverlayUi::confirm_close_click`]) so the two can never
/// drift. The `[Enter` and `[Esc` bracket tokens anchor the Yes / No regions.
const CONFIRM_CLOSE_ACTION_LINE: &str = "Close anyway?   [Enter / Y] Yes     [Esc / N] No";

/// Fixed body width (cells) for the attach-choice dialog (Phase 14). Sized for
/// the longest static line plus the panel border inset; the `.max(36)` floor in
/// [`overlay_rect`] keeps small grids sane.
const ATTACH_CHOICE_WIDTH: usize = 52;

/// The attach-choice dialog's action line, shared by the body builder and the
/// click hit-test ([`OverlayUi::attach_choice_click`]) so the two can never
/// drift. A leading prompt gives an inert region at col 0 (a stray click there
/// never attaches, mirroring the ConfirmClose guard); the `[N` and `[R` bracket
/// tokens anchor the New-tab / Replace regions.
const ATTACH_CHOICE_ACTION_LINE: &str = "Open where?  [N / Enter] New tab   [R] Replace";

/// Fixed body width (cells) for the kill-confirmation dialog (Manage Sessions).
/// Sized for the longest static line plus the panel border inset; the `.max(36)`
/// floor in [`overlay_rect`] keeps small grids sane.
const CONFIRM_KILL_SESSION_WIDTH: usize = 52;

/// The kill-confirmation dialog's action line, shared by the body builder and
/// the click hit-test ([`OverlayUi::confirm_kill_session_click`]) so the two can
/// never drift. A leading prompt gives an inert region at col 0 (a stray click
/// there never kills, mirroring the ConfirmClose guard); the `[Enter` and `[Esc`
/// bracket tokens anchor the Kill / Cancel regions.
const CONFIRM_KILL_SESSION_ACTION_LINE: &str = "Kill it?   [Enter / Y] Kill     [Esc / N] Cancel";

/// Fixed body width (cells) for the Detach & switch dialog (Packet 2). Sized for
/// the action line plus the panel border inset; the `.max(36)` floor in
/// [`overlay_rect`] keeps small grids sane.
const DETACH_SWITCH_WIDTH: usize = 64;

/// The Detach & switch dialog's action line, shared by the body builder and the
/// click hit-test ([`OverlayUi::detach_switch_click`]) so the two can never
/// drift. A leading prompt gives an inert region at col 0 (a stray click there
/// never spawns or closes a pane); the `[S`, `[K`, and `[Esc` bracket tokens
/// anchor the Swap / Keep-both / Cancel regions (ordered S < K < Esc, so the
/// click hit-test scans them right-to-left).
const DETACH_SWITCH_ACTION_LINE: &str =
    "Swap closes this.  [S] Swap   [K] Keep both   [Esc] Cancel";

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
        OverlayMode::CommandPalette => overlay.command_palette.desired_width(columns),
        OverlayMode::Replay => overlay.replay.desired_width(columns),
        OverlayMode::Connections => overlay.connections.desired_width(columns),
        OverlayMode::SessionAttach => overlay.session_attach.desired_width(columns),
        OverlayMode::OpenWith => overlay.open_with.desired_width(columns),
        // The image viewer (C4) uses a full-width backdrop panel; the decoded
        // image is drawn centered over it by the GPU image layer, so the panel
        // is just the dimmed frame behind the picture.
        OverlayMode::ImageView => columns,
        // Unreachable: handled by the early return above.
        OverlayMode::ContextMenu => overlay.context_menu.menu_width(),
        // Static two-line dialog; the `.max(36)` floor below gives it room and
        // the body text fits comfortably (CLOSE-CONFIRM).
        OverlayMode::ConfirmClose => CONFIRM_CLOSE_WIDTH,
        // Static choice dialog (Phase 14); same fixed-width/floor treatment.
        OverlayMode::AttachChoice => ATTACH_CHOICE_WIDTH,
        // Static kill-confirmation dialog (Manage Sessions); same treatment.
        OverlayMode::ConfirmKillSession => CONFIRM_KILL_SESSION_WIDTH,
        // Static Detach & switch choice dialog (Packet 2); same treatment.
        OverlayMode::DetachSwitchChoice => DETACH_SWITCH_WIDTH,
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
    // The image viewer is a LIGHTBOX (Phase 13c): the GPU draws a full-viewport
    // scrim + the image AFTER post-processing, so the cell grid must NOT paint a
    // bordered panel behind it. Paint ONLY a minimal caption and return — no
    // fill, no border. The scrim dims this caption to legible light gray.
    if overlay.mode == OverlayMode::ImageView {
        apply_image_view_caption(snapshot, overlay);
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
        OverlayMode::CommandPalette => "Command Palette".to_owned(),
        OverlayMode::Replay => "\u{2190} Session Replay  (Esc = back)".to_owned(),
        OverlayMode::Connections => "\u{2190} Connections  (Esc = back)".to_owned(),
        OverlayMode::SessionAttach => "\u{2190} Manage Sessions  (Esc = back)".to_owned(),
        OverlayMode::OpenWith => "\u{2190} Open With\u{2026}  (Esc = back)".to_owned(),
        OverlayMode::ImageView => {
            format!("\u{2190} {}  (Esc = close)", overlay.image_view_caption)
        }
        // Unreachable: handled by the early dispatch above.
        OverlayMode::ContextMenu => String::new(),
        OverlayMode::ConfirmClose => "Close?".to_owned(),
        OverlayMode::AttachChoice => "Attach session".to_owned(),
        OverlayMode::ConfirmKillSession => "Kill session".to_owned(),
        OverlayMode::DetachSwitchChoice => "Detach & switch".to_owned(),
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
    // Shared scroll affordance (OVERLAY-SMALL-WINDOW): a ▲ on the top border and
    // a ▼ on the bottom border when the body overflows the visible window.
    // Painted onto the border (right side, clear of the title), so a window tall
    // enough to show everything draws neither arrow and stays byte-identical.
    let (more_above, more_below) = overlay.scroll_arrows(rect.body_height);
    let arrow_col = rect.left + rect.width.saturating_sub(2);
    if more_above {
        write_text(snapshot, rect.top, arrow_col, 1, "▲", border_attrs());
    }
    if more_below {
        let bottom = rect.top + rect.height.saturating_sub(1);
        write_text(snapshot, bottom, arrow_col, 1, "▼", border_attrs());
    }
}

/// Paint ONLY the image-viewer lightbox caption (Phase 13c) — no panel fill, no
/// border. The image + a full-viewport dimming scrim are composited on the GPU
/// after post-processing; the cell grid contributes just this caption so the
/// viewer reads as a classic lightbox (dimmed terminal, bright photo). The
/// caption sits on the top row, clear of the centered ≤90% fit-rect, in clean
/// bold bright-white so the scrim dims it to a legible light gray.
fn apply_image_view_caption(snapshot: &mut Snapshot, overlay: &OverlayUi) {
    let columns = snapshot.dimensions.columns;
    if columns == 0 || snapshot.dimensions.rows == 0 {
        return;
    }
    let caption = format!("\u{2190} {}  (Esc = close)", overlay.image_view_caption);
    // Top row, small left inset; truncated to the terminal width by write_text.
    write_text(
        snapshot,
        0,
        2,
        columns.saturating_sub(2),
        &caption,
        image_caption_attrs(),
    );
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
    // When the window is too short to show every row, render only the visible
    // window starting at the scroll offset; otherwise `scroll == 0` and every
    // row renders, byte-identical to the pre-scroll layout.
    let rows = overlay.context_menu.rows();
    let scroll = overlay.context_menu.scroll_offset(rect.body_height);
    for (visible_index, row) in rows.iter().skip(scroll).take(rect.body_height).enumerate() {
        let y = rect.body_top + visible_index;
        // Guard against a grid so short the body row falls on/under the bottom
        // border (defensive; `rect()` already sizes the body window to fit).
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
                accelerator,
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
                // Part C: the effective keybind, right-aligned in the row. Only
                // drawn when it fits beside the label (rect() sizes the box to
                // fit via `menu_width`, so this normally holds).
                if let Some(accel) = accelerator {
                    let accel_len = accel.chars().count();
                    let label_len = label.chars().count();
                    if accel_len > 0 && accel_len + label_len < text_width {
                        let accel_col = text_column + text_width - accel_len;
                        write_text(snapshot, y, accel_col, accel_len, accel, attrs);
                    }
                }
            }
        }
    }
    // Scroll affordances: a ▲ on the top border when rows are hidden above the
    // visible window and a ▼ on the bottom border when rows are hidden below.
    // Painting onto the border (not a body row) keeps the body window full, so
    // the fits-on-screen case draws neither and stays byte-identical.
    let arrow_col = rect.left + rect.width / 2;
    if scroll > 0 {
        write_text(snapshot, rect.top, arrow_col, 1, "▲", border_attrs());
    }
    if scroll + rect.body_height < rows.len() {
        let bottom = rect.top + rect.height.saturating_sub(1);
        write_text(snapshot, bottom, arrow_col, 1, "▼", border_attrs());
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
            OverlayMode::CommandPalette => self
                .command_palette
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::Replay => self
                .replay
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::Connections => self
                .connections
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::SessionAttach => self
                .session_attach
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            OverlayMode::OpenWith => self
                .open_with
                .visible_lines(body_width, body_height)
                .into_iter()
                .map(OverlayLine::from)
                .collect(),
            // The image viewer (C4) draws the decoded picture over the panel via
            // the GPU image layer; the only cell-rendered body is a short hint,
            // which the image covers when it is large enough.
            OverlayMode::ImageView => vec![OverlayLine {
                text: "Press Esc to close.".to_owned(),
                focused: false,
                swatch: None,
                bold: false,
            }],
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
                    text: CONFIRM_CLOSE_ACTION_LINE.to_owned(),
                    focused: true,
                    swatch: None,
                    bold: false,
                },
            ],
            // Static choice copy (Phase 14). Row 0 prompt, row 1 blank, row 2 the
            // action line — the action row index (2) matches `ACTION_ROW` in
            // `attach_choice_click` so the click hit-test lands on it.
            OverlayMode::AttachChoice => vec![
                OverlayLine {
                    text: "This session is not open in a tab yet.".to_owned(),
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
                    text: ATTACH_CHOICE_ACTION_LINE.to_owned(),
                    focused: true,
                    swatch: None,
                    bold: false,
                },
            ],
            // Static kill-confirmation copy (Manage Sessions). Row 0 names the
            // target session (truncated to the body), row 1 blank, row 2 the
            // action line — the action row index (2) matches `ACTION_ROW` in
            // `confirm_kill_session_click`. The id is plain (validated to
            // alnum/._- by `safe_session_id`), so it cannot inject escapes.
            OverlayMode::ConfirmKillSession => {
                let prompt = format!("Terminate session \"{}\"?", self.confirm_kill_session_id);
                let prompt: String = prompt.chars().take(body_width.max(1)).collect();
                vec![
                    OverlayLine {
                        text: prompt,
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
                        text: CONFIRM_KILL_SESSION_ACTION_LINE.to_owned(),
                        focused: true,
                        swatch: None,
                        bold: false,
                    },
                ]
            }
            // Static Detach & switch copy (Packet 2). Row 0 names the cwd, row 1
            // is the honest data-loss warning, row 2 blank, row 3 the action
            // line — the action row index (3) matches `ACTION_ROW` in
            // `detach_switch_click`. The cwd is operator-controlled text, so it
            // is control-stripped and truncated to the body width; it is
            // display-only here.
            OverlayMode::DetachSwitchChoice => {
                let where_line = if self.detach_switch_cwd.is_empty() {
                    "New managed shell in the default directory.".to_owned()
                } else {
                    let cwd: String = self
                        .detach_switch_cwd
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .collect();
                    format!("New managed shell in {cwd}")
                };
                let where_line: String = where_line.chars().take(body_width.max(1)).collect();
                vec![
                    OverlayLine {
                        text: where_line,
                        focused: false,
                        swatch: None,
                        bold: false,
                    },
                    OverlayLine {
                        text: "Swap ends anything running in this pane.".to_owned(),
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
                        text: DETACH_SWITCH_ACTION_LINE.to_owned(),
                        focused: true,
                        swatch: None,
                        bold: false,
                    },
                ]
            }
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

impl From<PaletteOverlayLine> for OverlayLine {
    fn from(line: PaletteOverlayLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
        }
    }
}

impl From<ReplayOverlayLine> for OverlayLine {
    fn from(line: ReplayOverlayLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
        }
    }
}

impl From<ConnectionOverlayLine> for OverlayLine {
    fn from(line: ConnectionOverlayLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
        }
    }
}

impl From<SessionAttachOverlayLine> for OverlayLine {
    fn from(line: SessionAttachOverlayLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
        }
    }
}

impl From<OpenWithOverlayLine> for OverlayLine {
    fn from(line: OpenWithOverlayLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
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

/// Display width (in terminal cells) of `text`, matching the per-char width
/// [`write_text`] uses to lay glyphs out.
fn text_display_width(text: &str) -> usize {
    text.chars()
        .filter(|ch| !ch.is_control())
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(1).max(1))
        .sum()
}

/// Hard character-truncate `text` to at most `max_width` display cells (the
/// `write_text` clip rule), used as the last-resort fallback when not even one
/// word of a hint fits.
fn fit_chars(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        let w = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if width + w > max_width {
            break;
        }
        out.push(ch);
        width += w;
    }
    out
}

/// Fit a footer / hint line into `max_width` display cells **without cutting a
/// word in half** (OVERLAY-SMALL-WINDOW). When the whole hint already fits the
/// string is returned unchanged, so the normal/large-window render is
/// byte-identical to before this helper existed — the word-boundary trim only
/// engages on a window too narrow to show the full hint. If even the first word
/// overflows, it falls back to a hard character cut so something legible still
/// shows. Leading indentation spaces are preserved.
pub(super) fn fit_hint_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text_display_width(text) <= max_width {
        return text.to_owned();
    }
    // Keep the longest space-delimited prefix that fits. Splitting on a single
    // space preserves leading-indent spaces as empty leading tokens.
    let mut fitted = String::new();
    let mut width = 0usize;
    for (index, word) in text.split(' ').enumerate() {
        let sep = usize::from(index > 0);
        let word_w = text_display_width(word);
        if width + sep + word_w > max_width {
            break;
        }
        if index > 0 {
            fitted.push(' ');
            width += 1;
        }
        fitted.push_str(word);
        width += word_w;
    }
    if fitted.trim().is_empty() {
        return fit_chars(text, max_width);
    }
    // Drop any trailing whitespace left by stopping at a word boundary.
    fitted.truncate(fitted.trim_end().len());
    fitted
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

/// Caption attrs for the Phase 13c image-viewer LIGHTBOX: a clean bold
/// bright-white caption on the DEFAULT background (no inverse-video bar, no
/// panel chrome). The GPU scrim dims the whole terminal including this text, so
/// bright white reads as legible light gray over the dimmed surround.
fn image_caption_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(15);
    attrs.background = Color::Default;
    attrs.set_bold(true);
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
    fn fit_hint_returns_full_string_when_it_fits() {
        // Large-window byte-identity guard: a width that holds the whole hint
        // returns it unchanged (no behavior change on normal windows).
        let hint = "  Enter/\u{2192} open  / search  Ctrl+S save  Esc close";
        assert_eq!(fit_hint_to_width(hint, 80), hint);
        // Exactly-fits boundary also returns the full string.
        let w = text_display_width(hint);
        assert_eq!(fit_hint_to_width(hint, w), hint);
    }

    #[test]
    fn fit_hint_truncates_on_a_word_boundary_not_mid_word() {
        // A narrow window must never cut a word in half ("Ctrl+S sav").
        let hint = "Enter open search Ctrl+S save Esc close";
        // At/above the first word's width the trim is always on a word boundary;
        // below that the char-fallback (a legible head) is exercised separately.
        let first_word_w = text_display_width(hint.split(' ').next().unwrap());
        for width in first_word_w..text_display_width(hint) {
            let fitted = fit_hint_to_width(hint, width);
            assert!(
                text_display_width(&fitted) <= width,
                "fitted hint must fit in {width}: {fitted:?}"
            );
            // The fitted text is a whole-word prefix of the original: splitting
            // both on spaces, every fitted word equals the matching source word
            // (no partial trailing word).
            for (got, want) in fitted.split(' ').zip(hint.split(' ')) {
                assert_eq!(got, want, "word boundary preserved (width {width})");
            }
            assert!(
                !fitted.ends_with(' '),
                "no trailing space left by the word trim: {fitted:?}"
            );
        }
    }

    #[test]
    fn fit_hint_falls_back_to_char_cut_when_first_word_overflows() {
        // A single very long word can't be word-split; show a legible head
        // rather than nothing.
        let fitted = fit_hint_to_width("supercalifragilistic", 5);
        assert_eq!(fitted, "super");
        assert_eq!(fit_hint_to_width("anything", 0), "");
    }

    #[test]
    fn fit_hint_preserves_leading_indent() {
        // Leading indentation spaces are kept (the footer is indented two cols).
        let fitted = fit_hint_to_width("  Enter open close", 9);
        assert!(
            fitted.starts_with("  Enter"),
            "kept indent + first word: {fitted:?}"
        );
        assert!(text_display_width(&fitted) <= 9);
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
    fn command_palette_draws_into_snapshot_copy_only() {
        let mut overlay = OverlayUi::default();
        overlay.open_command_palette_for_test(["git status"], Some("/work/demo"));
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(
            original.cells,
            vec![Cell::new('.', Attrs::default()); 80 * 20]
        );
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
        assert!(rendered.cells.iter().any(|cell| cell.ch == 'g'));
    }

    #[test]
    fn closed_command_palette_is_pixel_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_command_palette_for_test(["git status"], Some("/work/demo"));
        overlay.close();
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(rendered, original);
    }

    /// A recorded frame whose first row spells `label`, for the replay tests.
    fn replay_frame(columns: usize, rows: usize, label: &str) -> Snapshot {
        let mut frame = Snapshot {
            dimensions: crate::core::Dimensions::new(columns, rows),
            cursor: Position::default(),
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![Cell::new(' ', Attrs::default()); columns * rows],
        };
        for (col, ch) in label.chars().take(columns).enumerate() {
            frame.cells[col] = Cell::new(ch, Attrs::default());
        }
        frame
    }

    #[test]
    fn replay_overlay_draws_into_snapshot_copy_only() {
        // REPLAY-OVERLAY-ISOLATION (render side): apply_overlay only mutates the
        // snapshot copy it is handed; the source frame is untouched, and the
        // recorded screen content is shown.
        let mut overlay = OverlayUi::default();
        overlay.open_replay(vec![
            replay_frame(40, 6, "alpha"),
            replay_frame(40, 6, "bravo"),
        ]);
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        // The input snapshot is never mutated in place.
        assert_eq!(
            original.cells,
            vec![Cell::new('.', Attrs::default()); 80 * 20]
        );
        // The panel border and the recorded content (live tail = "bravo") show.
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
        assert!(rendered.cells.iter().any(|cell| cell.ch == 'b'));
    }

    #[test]
    fn closed_replay_overlay_is_pixel_inert() {
        // OVERLAY-CLOSED-BYTE-IDENTICAL: once closed, the replay overlay paints
        // nothing — the frame is byte-identical to the input.
        let mut overlay = OverlayUi::default();
        overlay.open_replay(vec![replay_frame(40, 6, "alpha")]);
        overlay.close();
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(rendered, original);
    }

    #[test]
    fn empty_replay_overlay_opens_with_hint() {
        // Opening replay with no recorded frames still opens (showing a hint)
        // rather than failing; it draws a panel into the copy only.
        let mut overlay = OverlayUi::default();
        overlay.open_replay(Vec::new());
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(
            original.cells,
            vec![Cell::new('.', Attrs::default()); 80 * 20]
        );
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    }

    /// A synthetic connection host for the connection-overlay tests.
    fn connection_host(alias: &str) -> ConnectionHost {
        ConnectionHost {
            alias: alias.to_owned(),
            host_name: Some(format!("{alias}.example.invalid")),
            user: None,
            port: None,
            theme: None,
            font: None,
            title: None,
            source: crate::connection_hosts::ConnectionHostSource::Odytty,
        }
    }

    #[test]
    fn connection_overlay_draws_into_snapshot_copy_only() {
        // CONNECTION-OVERLAY-ISOLATION (render side): apply_overlay only mutates
        // the snapshot copy it is handed; the source frame is untouched, and the
        // host list shows.
        let mut overlay = OverlayUi::default();
        overlay.open_connections(vec![connection_host("web1"), connection_host("db1")]);
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        // The input snapshot is never mutated in place.
        assert_eq!(
            original.cells,
            vec![Cell::new('.', Attrs::default()); 80 * 20]
        );
        // The panel border and a host alias show.
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
        assert!(rendered.cells.iter().any(|cell| cell.ch == 'w'));
    }

    #[test]
    fn closed_connection_overlay_is_pixel_inert() {
        // OVERLAY-CLOSED-BYTE-IDENTICAL: once closed, the connection overlay
        // paints nothing — the frame is byte-identical to the input.
        let mut overlay = OverlayUi::default();
        overlay.open_connections(vec![connection_host("web1")]);
        overlay.close();
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(rendered, original);
    }

    #[test]
    fn empty_connection_overlay_opens_with_hint() {
        // Opening the connection manager with no hosts still opens (showing a
        // hint) rather than failing; it draws a panel into the copy only.
        let mut overlay = OverlayUi::default();
        overlay.open_connections(Vec::new());
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(
            original.cells,
            vec![Cell::new('.', Attrs::default()); 80 * 20]
        );
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    }

    #[test]
    fn connection_overlay_accept_emits_connect_outcome() {
        // The overlay routes an accepted host up as OverlayOutcome::Connect for
        // the App's connect action; presentation stays in the overlay.
        let mut overlay = OverlayUi::default();
        overlay.open_connections(vec![connection_host("web1")]);
        assert!(overlay.is_open());
        match overlay.handle_input(OverlayInput::Activate) {
            OverlayOutcome::Connect(host) => assert_eq!(host.alias, "web1"),
            other => panic!("expected Connect, got {other:?}"),
        }
        // C12 regression: the overlay must close itself on Connect. If it stays
        // open, keyboard dispatch keeps routing every key (possibly the SSH
        // password) into the type-to-filter box instead of the new SSH tab.
        assert!(
            !overlay.is_open(),
            "connection overlay must close after Connect"
        );
    }

    /// A synthetic live session for the session-attach overlay tests.
    fn listed_session(id: &str, name: &str) -> ListedSession {
        ListedSession {
            id: id.to_owned(),
            name: name.to_owned(),
            state: "running",
            age_ms: 1000,
            pane_count: 1,
        }
    }

    #[test]
    fn session_attach_overlay_draws_into_snapshot_copy_only() {
        // SESSION-ATTACH-OVERLAY-ISOLATION (render side): apply_overlay only
        // mutates the snapshot copy it is handed; the source frame is untouched,
        // and the session list shows.
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(vec![
            listed_session("s-0001-aaaa", "build"),
            listed_session("s-0002-bbbb", "web"),
        ]);
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        // The input snapshot is never mutated in place.
        assert_eq!(
            original.cells,
            vec![Cell::new('.', Attrs::default()); 80 * 20]
        );
        // The panel border and a session title show.
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
        assert!(rendered.cells.iter().any(|cell| cell.ch == 'b'));
    }

    #[test]
    fn closed_session_attach_overlay_is_pixel_inert() {
        // OVERLAY-CLOSED-BYTE-IDENTICAL: once closed, the session-attach overlay
        // paints nothing — the frame is byte-identical to the input.
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(vec![listed_session("s-0001-aaaa", "build")]);
        overlay.close();
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(rendered, original);
    }

    #[test]
    fn empty_session_attach_overlay_opens_with_hint() {
        // Opening the session-attach overlay with no live sessions still opens
        // (showing a hint) rather than failing; it draws a panel into the copy.
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(Vec::new());
        let original = snapshot(80, 20);
        let mut rendered = original.clone();

        apply_overlay(&mut rendered, &mut overlay);

        assert_eq!(
            original.cells,
            vec![Cell::new('.', Attrs::default()); 80 * 20]
        );
        assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    }

    #[test]
    fn session_attach_overlay_accept_emits_attach_outcome() {
        // The overlay routes an accepted session up as
        // OverlayOutcome::AttachSession(id) for the App's new-tab attach;
        // presentation stays in the overlay.
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(vec![listed_session("s-0001-aaaa", "build")]);
        match overlay.handle_input(OverlayInput::Activate) {
            OverlayOutcome::AttachSession(id) => assert_eq!(id, "s-0001-aaaa"),
            other => panic!("expected AttachSession, got {other:?}"),
        }
    }

    #[test]
    fn context_menu_split_items_emit_split_outcomes() {
        // Part B: activating the split items routes up as the split outcomes the
        // App dispatches to `split_active_pane` (the same action the keyboard
        // split chords fire).
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            None,
            false,
            None,
            Default::default(),
        );
        // Focus starts at item 0 (Copy); Split Right is item index 9 after F1's
        // New Window (index 6) extended the tab-actions section.
        for _ in 0..9 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::ContextMenuSplitColumns
        );

        // Reopen and walk to Split Down (item index 10).
        overlay.open_context_menu(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            None,
            false,
            None,
            Default::default(),
        );
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::ContextMenuSplitRows
        );
    }

    #[test]
    fn context_menu_new_window_emits_new_window_outcome() {
        // F1: activating the New Window item (single-pane visible index 6, right
        // after New Tab at 5) routes up as the ContextMenuNewWindow outcome the
        // App dispatches to `handle_new_window` — the same handler the
        // Ctrl+Shift+N chord fires.
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            None,
            false,
            None,
            Default::default(),
        );
        for _ in 0..6 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::ContextMenuNewWindow
        );
        // The menu closes itself on activation (the App relies on this before
        // dispatching the outcome).
        assert!(!overlay.is_open(), "context menu closes on New Window");
    }

    #[test]
    fn context_menu_close_pane_emits_close_pane_outcome_only_multi_pane() {
        // Multi-pane: Close Pane is visible item index 11 (right after Split
        // Down at 10; Settings is 12) after F1's New Window shift; activating it
        // routes up as the Close Pane outcome the App dispatches to
        // `apply_pane_action(ClosePane)`.
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            None,
            true,
            None,
            Default::default(),
        );
        for _ in 0..11 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::ContextMenuClosePane
        );

        // Single-pane: Close Pane is hidden, so item index 11 is Settings — the
        // Close Pane outcome is unreachable.
        overlay.open_context_menu(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            None,
            false,
            None,
            Default::default(),
        );
        for _ in 0..11 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::ContextMenuSettings
        );
    }

    #[test]
    fn context_menu_file_items_emit_path_outcomes() {
        // C3: with a resolved path target, the file section's four items lift
        // into the matching path outcomes carrying the resolved data. Synthetic
        // path only — no real filesystem.
        let resolved = crate::paths::Resolved {
            abs: "/proj/src/main.rs".to_owned(),
            kind: crate::paths::FsKind::File,
            line: Some(42),
            col: Some(7),
        };
        // Path-scoped visible order: Open(0) / Open With…(1) / CopyPath(2) /
        // CopyFile(3) / Reveal(4) for a non-image file, then pinned text
        // Copy/Paste after a separator.
        let open_menu = |steps: usize| {
            let mut overlay = OverlayUi::default();
            overlay.open_context_menu(
                CellPoint { row: 0, column: 0 },
                true,
                true,
                true,
                true,
                None,
                false,
                Some(resolved.clone()),
                Default::default(),
            );
            for _ in 0..steps {
                overlay.handle_input(OverlayInput::Down);
            }
            overlay.handle_input(OverlayInput::Activate)
        };

        assert_eq!(
            open_menu(0),
            OverlayOutcome::ContextMenuOpenPath(Box::new(resolved.clone()))
        );
        assert_eq!(
            open_menu(1),
            OverlayOutcome::ContextMenuOpenWith(Box::new(resolved.clone()))
        );
        assert_eq!(
            open_menu(2),
            OverlayOutcome::ContextMenuCopyPath("/proj/src/main.rs".to_owned())
        );
        assert_eq!(
            open_menu(3),
            OverlayOutcome::ContextMenuCopyFile("file:///proj/src/main.rs".to_owned())
        );
        assert_eq!(
            open_menu(4),
            OverlayOutcome::ContextMenuRevealPath(Box::new(resolved.clone()))
        );
    }

    #[test]
    fn context_menu_without_path_has_no_file_outcomes() {
        // With no path target the file items are not visible; walking to where
        // they would be lands on the launcher section instead (byte-identical
        // to the pre-C3 menu).
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            None,
            false,
            None,
            Default::default(),
        );
        // 17 visible items single-pane (F1 New Window added); Manage Sessions is
        // now index 15 (Detach & switch is the last, index 16).
        for _ in 0..15 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::ContextMenuSessionAttach,
            "walking to the launcher section lands on Manage Sessions, not a file item"
        );
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
    fn image_view_opens_renders_caption_and_dismisses() {
        // C4 + Phase 13c lightbox: the image-viewer overlay opens in its own
        // mode and paints ONLY a minimal caption (no bordered panel — the image
        // + dimming scrim are composited on the GPU). It dismisses on Esc / Enter
        // while swallowing every other key (no PTY leak behind the overlay).
        let mut overlay = OverlayUi::default();
        overlay.open_image_view("diagram.png".to_owned());
        assert!(overlay.is_open());
        assert!(overlay.image_view_open());
        assert_eq!(overlay.render_signature().mode, OverlayMode::ImageView);

        // The lightbox paints the filename caption with an inline close hint.
        let mut rendered = snapshot(70, 18);
        apply_overlay(&mut rendered, &mut overlay);
        let painted: String = rendered.cells.iter().map(|cell| cell.ch).collect();
        assert!(painted.contains("diagram.png"), "caption is painted");
        assert!(
            painted.contains("Esc = close"),
            "caption carries the close hint"
        );

        // A stray key is swallowed; the overlay stays open.
        assert_eq!(
            overlay.handle_input(OverlayInput::Char('x')),
            OverlayOutcome::Consumed
        );
        assert!(overlay.image_view_open());

        // Esc and Enter both dismiss (emit Close).
        for input in [OverlayInput::Close, OverlayInput::Activate] {
            overlay.open_image_view("pic.webp".to_owned());
            assert_eq!(overlay.handle_input(input), OverlayOutcome::Close);
        }
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
    fn pointer_press_on_about_back_arrow_returns_to_sections() {
        // NF15: the About view draws the same `← … (Esc = back)` title as a
        // SectionDetail, so clicking its arrow must navigate back too. Before
        // the fix, settings_title_back_hit matched only SectionDetail, so this
        // click fell through and left the panel stranded at About (Esc worked
        // via the panel's own input path, masking the dead click).
        let mut overlay = OverlayUi::default();
        overlay.open_settings();
        overlay.handle_input(OverlayInput::End); // select the synthetic About row
        overlay.handle_input(OverlayInput::Activate); // drill into About
        assert_eq!(
            overlay.render_signature().panel.level,
            SettingsLevel::About,
            "precondition: at the About level"
        );
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");

        // Click the title row (rect.top) where the ← arrow is drawn.
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
            "clicking the About title ← arrow returns to the section list"
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

    // --- UX4-P1 click→Activate parity: list overlays + ConfirmClose ---

    fn body_press(rect: OverlayRect, row_in_body: usize, col_in_body: usize) -> OverlayPointer {
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top + row_in_body,
                column: rect.body_left + col_in_body,
            },
            button: PointerButton::Left,
            x_in_body: None,
        }
    }

    #[test]
    fn pointer_click_session_row_attaches_like_enter() {
        let sessions = vec![
            ListedSession {
                id: "s-1".to_owned(),
                name: "build".to_owned(),
                state: "running",
                age_ms: 1,
                pane_count: 1,
            },
            ListedSession {
                id: "s-2".to_owned(),
                name: "web".to_owned(),
                state: "running",
                age_ms: 1,
                pane_count: 1,
            },
        ];
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(sessions);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Prime the scroll window with a render, then click body row 2 (the
        // second session; row 0 is the `> query` prompt, row 1 the first).
        let _ = overlay.visible_lines(rect.body_width, rect.body_height);
        let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
        assert_eq!(outcome, OverlayOutcome::AttachSession("s-2".to_owned()));
    }

    #[test]
    fn pointer_click_keybind_row_selects_and_arms_capture() {
        let mut overlay = OverlayUi::default();
        overlay.open_key_bindings(&overlay.settings.clone());
        let rect = overlay_rect(&overlay, 80, 30).expect("rect");
        let _ = overlay.visible_lines(rect.body_width, rect.body_height);
        // Row 0 is the help message; row 1 is the first action row.
        let outcome = overlay.handle_pointer(body_press(rect, 1, 0), rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        assert!(
            overlay.is_capturing_chord(),
            "a click on an action row selects + arms chord capture (parity with Enter)"
        );
    }

    #[test]
    fn pointer_click_yes_in_confirm_close_forces_close() {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_close();
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Action line is the 3rd body row; click inside the "[Enter / Y] Yes" span.
        let yes_col = CONFIRM_CLOSE_ACTION_LINE.find("[Enter").unwrap() + 2;
        let outcome = overlay.handle_pointer(body_press(rect, 2, yes_col), rect);
        assert_eq!(outcome, OverlayOutcome::ForceClose);
        assert!(!overlay.is_open(), "Yes confirms and closes the dialog");
    }

    #[test]
    fn pointer_click_no_in_confirm_close_cancels() {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_close();
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let no_col = CONFIRM_CLOSE_ACTION_LINE.find("[Esc").unwrap() + 2;
        let outcome = overlay.handle_pointer(body_press(rect, 2, no_col), rect);
        assert_eq!(outcome, OverlayOutcome::Close);
    }

    #[test]
    fn pointer_click_confirm_close_prompt_text_is_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_close();
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Column 0 of the action line is the "Close anyway?" prompt — never a
        // button, so a stray click cannot destroy a running job (TRAP-2).
        let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        assert!(overlay.is_open(), "a prompt-text click never force-closes");
    }

    // --- Phase 14: attach-choice dialog (open / key route / click parity) ---

    #[test]
    fn attach_choice_opens_in_attach_choice_mode() {
        let mut overlay = OverlayUi::default();
        overlay.open_attach_choice("s-0001-aaaa".to_owned());
        assert!(
            overlay.is_attach_choice(),
            "the dialog opens in AttachChoice"
        );
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::AttachChoice,
            "render signature reflects the new mode"
        );
        // The dialog has a real centered rect (renderable).
        assert!(overlay_rect(&overlay, 80, 24).is_some());
    }

    #[test]
    fn attach_choice_key_n_and_enter_emit_new_tab() {
        for input in [OverlayInput::Char('n'), OverlayInput::Activate] {
            let mut overlay = OverlayUi::default();
            overlay.open_attach_choice("s-0001-aaaa".to_owned());
            let outcome = overlay.handle_input(input);
            assert_eq!(
                outcome,
                OverlayOutcome::AttachChoiceNewTab("s-0001-aaaa".to_owned())
            );
            assert!(!overlay.is_open(), "the dialog closes after choosing");
        }
    }

    #[test]
    fn attach_choice_key_r_emits_replace() {
        let mut overlay = OverlayUi::default();
        overlay.open_attach_choice("s-0001-aaaa".to_owned());
        let outcome = overlay.handle_input(OverlayInput::Char('R'));
        assert_eq!(
            outcome,
            OverlayOutcome::AttachChoiceReplace("s-0001-aaaa".to_owned())
        );
        assert!(!overlay.is_open());
    }

    #[test]
    fn attach_choice_esc_cancels_without_attaching() {
        let mut overlay = OverlayUi::default();
        overlay.open_attach_choice("s-0001-aaaa".to_owned());
        let outcome = overlay.handle_input(OverlayInput::Close);
        assert_eq!(outcome, OverlayOutcome::Close);
    }

    #[test]
    fn pointer_click_new_tab_in_attach_choice_emits_new_tab() {
        let mut overlay = OverlayUi::default();
        overlay.open_attach_choice("s-0001-aaaa".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let new_col = ATTACH_CHOICE_ACTION_LINE.find("[N").unwrap() + 1;
        let outcome = overlay.handle_pointer(body_press(rect, 2, new_col), rect);
        assert_eq!(
            outcome,
            OverlayOutcome::AttachChoiceNewTab("s-0001-aaaa".to_owned())
        );
        assert!(!overlay.is_open());
    }

    #[test]
    fn pointer_click_replace_in_attach_choice_emits_replace() {
        let mut overlay = OverlayUi::default();
        overlay.open_attach_choice("s-0001-aaaa".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let replace_col = ATTACH_CHOICE_ACTION_LINE.find("[R").unwrap() + 1;
        let outcome = overlay.handle_pointer(body_press(rect, 2, replace_col), rect);
        assert_eq!(
            outcome,
            OverlayOutcome::AttachChoiceReplace("s-0001-aaaa".to_owned())
        );
    }

    #[test]
    fn pointer_click_attach_choice_prompt_text_is_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_attach_choice("s-0001-aaaa".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Col 0 of the action line is the "Open where?" prompt — never a button,
        // so a stray click cannot attach (parity with the ConfirmClose guard).
        let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        assert!(overlay.is_open(), "a prompt-text click never attaches");
        // A click on the prompt body row (row 0) is also inert.
        let outcome_row0 = overlay.handle_pointer(body_press(rect, 0, 5), rect);
        assert_eq!(outcome_row0, OverlayOutcome::Consumed);
        assert!(overlay.is_open());
    }

    // --- Manage Sessions: kill-confirmation dialog (open / key / click parity) ---

    #[test]
    fn confirm_kill_session_opens_in_confirm_kill_mode() {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
        assert!(
            overlay.is_confirm_kill_session(),
            "the dialog opens in ConfirmKillSession"
        );
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::ConfirmKillSession,
            "render signature reflects the new mode"
        );
        // The dialog has a real centered rect (renderable), and the body names
        // the target session.
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let lines = overlay.visible_lines(rect.body_width, rect.body_height);
        assert!(lines[0].text.contains("s-0001-aaaa"), "body names the id");
    }

    #[test]
    fn confirm_kill_session_key_y_and_enter_emit_confirmed() {
        for input in [OverlayInput::Char('y'), OverlayInput::Activate] {
            let mut overlay = OverlayUi::default();
            overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
            let outcome = overlay.handle_input(input);
            assert_eq!(
                outcome,
                OverlayOutcome::KillSessionConfirmed("s-0001-aaaa".to_owned())
            );
            assert!(!overlay.is_open(), "the dialog closes after confirming");
        }
    }

    #[test]
    fn confirm_kill_session_esc_and_n_cancel_without_killing() {
        for input in [OverlayInput::Close, OverlayInput::Char('n')] {
            let mut overlay = OverlayUi::default();
            overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
            let outcome = overlay.handle_input(input);
            assert_eq!(outcome, OverlayOutcome::Close, "cancel never kills");
        }
    }

    #[test]
    fn pointer_click_kill_in_confirm_kill_session_confirms() {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let kill_col = CONFIRM_KILL_SESSION_ACTION_LINE.find("[Enter").unwrap() + 2;
        let outcome = overlay.handle_pointer(body_press(rect, 2, kill_col), rect);
        assert_eq!(
            outcome,
            OverlayOutcome::KillSessionConfirmed("s-0001-aaaa".to_owned())
        );
        assert!(!overlay.is_open(), "Kill confirms and closes the dialog");
    }

    #[test]
    fn pointer_click_cancel_in_confirm_kill_session_cancels() {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let cancel_col = CONFIRM_KILL_SESSION_ACTION_LINE.find("[Esc").unwrap() + 2;
        let outcome = overlay.handle_pointer(body_press(rect, 2, cancel_col), rect);
        assert_eq!(outcome, OverlayOutcome::Close);
    }

    #[test]
    fn pointer_click_confirm_kill_session_prompt_text_is_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Col 0 of the action line is the "Kill it?" prompt — never a button, so
        // a stray click cannot kill (parity with the ConfirmClose guard).
        let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        assert!(overlay.is_open(), "a prompt-text click never kills");
    }

    #[test]
    fn right_click_session_row_requests_kill_left_click_still_attaches() {
        let sessions = vec![
            ListedSession {
                id: "s-1".to_owned(),
                name: "build".to_owned(),
                state: "running",
                age_ms: 1,
                pane_count: 1,
            },
            ListedSession {
                id: "s-2".to_owned(),
                name: "web".to_owned(),
                state: "running",
                age_ms: 1,
                pane_count: 1,
            },
        ];
        // Right-click body row 2 (the second session) requests a kill for its id
        // and leaves the manager open (the App opens the confirm dialog).
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(sessions.clone());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let _ = overlay.visible_lines(rect.body_width, rect.body_height);
        let right_press = OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top + 2,
                column: rect.body_left,
            },
            button: PointerButton::Right,
            x_in_body: None,
        };
        assert_eq!(
            overlay.handle_pointer(right_press, rect),
            OverlayOutcome::KillSessionRequest("s-2".to_owned())
        );

        // Left-click the same row still attaches (Phase 5 path unchanged).
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(sessions);
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let _ = overlay.visible_lines(rect.body_width, rect.body_height);
        assert_eq!(
            overlay.handle_pointer(body_press(rect, 2, 0), rect),
            OverlayOutcome::AttachSession("s-2".to_owned())
        );
    }

    #[test]
    fn right_click_off_a_session_row_is_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_session_attach(Vec::new());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let _ = overlay.visible_lines(rect.body_width, rect.body_height);
        // Row 0 is the `> query` prompt — never a session row.
        let right_press = OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top,
                column: rect.body_left,
            },
            button: PointerButton::Right,
            x_in_body: None,
        };
        assert_eq!(
            overlay.handle_pointer(right_press, rect),
            OverlayOutcome::Consumed
        );
    }

    // --- Packet 2: Detach & switch choice dialog (open / key / click parity) ---

    #[test]
    fn detach_switch_opens_in_detach_switch_mode_and_names_cwd() {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        assert!(
            overlay.is_detach_switch_choice(),
            "the dialog opens in DetachSwitchChoice"
        );
        assert_eq!(
            overlay.render_signature().mode,
            OverlayMode::DetachSwitchChoice,
            "render signature reflects the new mode"
        );
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let lines = overlay.visible_lines(rect.body_width, rect.body_height);
        assert!(
            lines[0].text.contains("/home/user/proj"),
            "body names the cwd"
        );
    }

    #[test]
    fn detach_switch_unknown_cwd_shows_default_directory_copy() {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice(String::new());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let lines = overlay.visible_lines(rect.body_width, rect.body_height);
        assert!(
            lines[0].text.contains("default directory"),
            "unknown cwd falls back to a clear default-directory line"
        );
    }

    #[test]
    fn detach_switch_key_s_emits_swap() {
        for input in [OverlayInput::Char('s'), OverlayInput::Char('S')] {
            let mut overlay = OverlayUi::default();
            overlay.open_detach_switch_choice("/home/user/proj".to_owned());
            let outcome = overlay.handle_input(input);
            assert_eq!(
                outcome,
                OverlayOutcome::DetachSwitchSwap("/home/user/proj".to_owned())
            );
            assert!(!overlay.is_open(), "the dialog closes after choosing");
        }
    }

    #[test]
    fn detach_switch_key_k_emits_keep_both() {
        for input in [OverlayInput::Char('k'), OverlayInput::Char('K')] {
            let mut overlay = OverlayUi::default();
            overlay.open_detach_switch_choice("/home/user/proj".to_owned());
            let outcome = overlay.handle_input(input);
            assert_eq!(
                outcome,
                OverlayOutcome::DetachSwitchKeepBoth("/home/user/proj".to_owned())
            );
            assert!(!overlay.is_open());
        }
    }

    #[test]
    fn detach_switch_esc_cancels_and_enter_is_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        // Enter has NO default here (Swap is destructive) — it is swallowed.
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OverlayOutcome::Consumed,
            "Enter must not trigger the destructive Swap"
        );
        assert!(overlay.is_open(), "Enter leaves the dialog open");
        // Esc cancels, spawning/closing nothing.
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OverlayOutcome::Close
        );
    }

    #[test]
    fn pointer_click_swap_in_detach_switch_emits_swap() {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Action line is the 4th body row (index 3).
        let swap_col = DETACH_SWITCH_ACTION_LINE.find("[S").unwrap() + 1;
        let outcome = overlay.handle_pointer(body_press(rect, 3, swap_col), rect);
        assert_eq!(
            outcome,
            OverlayOutcome::DetachSwitchSwap("/home/user/proj".to_owned())
        );
        assert!(!overlay.is_open());
    }

    #[test]
    fn pointer_click_keep_both_in_detach_switch_emits_keep_both() {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let keep_col = DETACH_SWITCH_ACTION_LINE.find("[K").unwrap() + 1;
        let outcome = overlay.handle_pointer(body_press(rect, 3, keep_col), rect);
        assert_eq!(
            outcome,
            OverlayOutcome::DetachSwitchKeepBoth("/home/user/proj".to_owned())
        );
    }

    #[test]
    fn pointer_click_cancel_in_detach_switch_cancels() {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        let cancel_col = DETACH_SWITCH_ACTION_LINE.find("[Esc").unwrap() + 1;
        let outcome = overlay.handle_pointer(body_press(rect, 3, cancel_col), rect);
        assert_eq!(outcome, OverlayOutcome::Close);
    }

    #[test]
    fn pointer_click_detach_switch_prompt_text_is_inert() {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // Col 0 of the action line is the "Swap closes this." prompt — never a
        // button, so a stray click cannot spawn or close a pane.
        let outcome = overlay.handle_pointer(body_press(rect, 3, 0), rect);
        assert_eq!(outcome, OverlayOutcome::Consumed);
        assert!(overlay.is_open(), "a prompt-text click never acts");
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
