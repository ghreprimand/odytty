// SPDX-License-Identifier: GPL-3.0-only
//! Overlay coordinator state: the [`OverlayUi`] fields, construction, mode
//! transitions, pending payloads, and navigation latches.
//!
//! The coordinator is presentation-only. Frozen or cloned state enters through
//! the open/apply entry points and an [`OverlayOutcome`] leaves; nothing here
//! touches a live terminal or PTY.

use crate::connection_hosts::ConnectionHost;
use crate::core::Snapshot;
use crate::native::connection_form::ConnectionForm;
use crate::native::connection_overlay::{ConnectionOverlay, ConnectionPickerPurpose};
use crate::native::context_menu_ui::ContextMenuUi;
use crate::native::font_picker::FontPicker;
use crate::native::key_remap_ui::KeyRemapUi;
use crate::native::onboarding::OnboardingPanel;
use crate::native::open_with_overlay::OpenWithOverlay;
use crate::native::palette_overlay::PaletteOverlay;
use crate::native::profile_manager::ProfileManager;
use crate::native::replay_overlay::ReplayOverlay;
use crate::native::session::SessionToken;
use crate::native::session_attach_overlay::SessionAttachOverlay;
use crate::native::settings_panel::{SettingsPanel, SettingsPanelOutcome};
use crate::native::theme_builder::ThemeBuilder;
use crate::native::theme_picker::ThemePicker;
use crate::native::workspace_picker::{WorkspacePicker, WorkspacePickerEntry};
use crate::session_host::ListedSession;
use crate::settings::Settings;
use crate::theme::Theme;

use super::contracts::{
    LayoutSaveKind, OverlayMode, OverlayOutcome, PickerReturn, RiskyPasteDialog, SettingsTarget,
};

#[derive(Debug, Clone)]
pub(in crate::native) struct OverlayUi {
    pub(super) open: bool,
    pub(super) mode: OverlayMode,
    pub(super) settings: Settings,
    pub(super) panel: SettingsPanel,
    pub(super) theme_picker: ThemePicker,
    pub(super) theme_builder: ThemeBuilder,
    pub(super) profile_manager: ProfileManager,
    pub(super) font_picker: FontPicker,
    pub(super) key_remap: KeyRemapUi,
    pub(super) onboarding: OnboardingPanel,
    pub(super) context_menu: ContextMenuUi,
    pub(super) command_palette: PaletteOverlay,
    pub(super) replay: ReplayOverlay,
    pub(super) connections: ConnectionOverlay,
    pub(super) connection_form: ConnectionForm,
    pub(super) session_attach: SessionAttachOverlay,
    pub(super) open_with: OpenWithOverlay,
    pub(super) workspace_picker: WorkspacePicker,
    /// Caption (the image's filename) shown in the C4 image-viewer overlay's
    /// body. The image itself draws through the GPU image layer, over the panel;
    /// this presentation-only string is the only state the viewer mode carries.
    pub(super) image_view_caption: String,
    pub(super) risky_paste: RiskyPasteDialog,
    /// The pending host session-id carried by the attach-choice dialog (Phase
    /// 14). Set when the dialog opens; the "New tab"/"Replace current" arms emit
    /// it back to the App. Empty when the dialog is not open. The dialog body is
    /// static text, so this is the only state the mode carries (it does not enter
    /// the render signature — the card looks identical for any id).
    pub(super) attach_choice_session_id: String,
    /// The pending host session-id carried by the kill-confirmation dialog
    /// (Manage Sessions). Set when the dialog opens (right-click a session row);
    /// the confirm arm emits it back to the App, which calls
    /// `session_host::kill_session`. Empty when the dialog is not open. The card
    /// shows a short id hint but does not enter the render signature: the mode
    /// flips through `close()` between distinct kill dialogs, forcing a repaint,
    /// so the carried id never needs to gate the cache (same trick as
    /// `attach_choice_session_id`).
    pub(super) confirm_kill_session_id: String,
    /// The focused pane's cwd carried by the Detach & switch dialog.
    /// Set when the dialog opens; the Swap / Keep-both arms emit it back to the
    /// App, which spawns a managed session in it. Empty = unknown cwd (spawn in
    /// the default directory). Operator-controlled text (an OSC 7 path), so the
    /// body truncates it to the panel width and it is display-only here — it
    /// re-enters the App only as the `working_directory` of the same spawn config
    /// `odytty new` uses, never a raw shell arg. Not in the render signature: the
    /// card layout is identical for any cwd, and the mode flips through `close()`
    /// between opens, forcing a repaint.
    pub(super) detach_switch_cwd: String,
    /// Set when a `SaveAndClose` outcome arrives from the settings panel (dirty
    /// close prompt). On the next `save_succeeded` call for Settings mode, the
    /// overlay closes itself after recording the save (SETTINGS-REDESIGN §7).
    pub(super) close_after_save: bool,
    /// Set when the keybind editor's dirty-close prompt chose "save" (P1-6). On
    /// the next `save_succeeded` for KeyBindings mode, the overlay returns to the
    /// settings panel (rather than the in-modal Ctrl+S behaviour of staying
    /// open). Mirrors `close_after_save` for the keybind-editor lane.
    pub(super) key_remap_close_after_save: bool,
    pub(super) picker_return: Option<PickerReturn>,
    /// True while `ThemeBuilder` is the active mode AND it was entered from
    /// `ThemePicker` (via `ThemePickerOutcome::OpenBuilder`). Esc / back-button
    /// in this state navigates back to `ThemePicker` rather than closing the
    /// whole overlay. False for the Settings-launched path (which returns to
    /// the settings panel via `picker_return`, same contract as the pickers)
    /// and for the standalone paths (keyboard shortcut, theme capture), which
    /// close.
    pub(super) builder_from_picker: bool,
    /// The pending host + target tab carried by the replace-tab confirm dialog
    /// (ODP-5D). Set when the dialog opens (the clicked tab held a running
    /// foreground child); the confirm arm emits it back so the App closes that
    /// tab and opens the host in its slot. `None` when the dialog is not open.
    /// Not in the render signature: the card layout is identical for any target
    /// and the mode flips through `close()` between opens, forcing a repaint
    /// (same trick as `confirm_kill_session_id`).
    pub(super) confirm_replace_tab: Option<(Box<ConnectionHost>, SessionToken)>,
    /// The pending host carried by the remove-host confirm dialog (ODP-2C). Set
    /// when "Remove…" is chosen on a connection-manager row; the confirm arm
    /// emits it back so the App deletes its `hosts.conf` block. `None` when the
    /// dialog is not open. Not in the render signature for the same reason as
    /// `confirm_replace_tab`: the card layout is target-independent and the mode
    /// flips through `close()` between opens.
    pub(super) confirm_remove_host: Option<Box<ConnectionHost>>,
    /// The pending save carried by the overwrite-layout confirm dialog
    /// (OVERWRITE-WARN). Set when a Save as Layout resolves to a name that
    /// already exists on disk; carries the resolved layout `name` and which save
    /// it was (whole app vs. one workspace) so the confirm arm can either force
    /// the write (Replace) or reopen the name prompt (a different name). `None`
    /// when the dialog is not open. Not in the render signature: the card layout
    /// is name-independent and the mode flips through `close()` between opens.
    pub(super) confirm_overwrite_layout: Option<(String, LayoutSaveKind)>,
    /// The pending open carried by the open-layout mode dialog (LAYOUT-OPEN-MODE).
    /// Set when a layout is opened onto a window that holds real state (not a
    /// single pristine workspace); carries the layout `name` so the confirm arm
    /// can either replace the current workspaces with the saved set or append
    /// them beside it. `None` when the dialog is not open. Not in the render
    /// signature for the same reason as `confirm_overwrite_layout`.
    pub(super) confirm_open_layout: Option<String>,
}

impl Default for OverlayUi {
    fn default() -> Self {
        Self::new(&Settings::default())
    }
}

impl OverlayUi {
    pub(in crate::native) fn new(settings: &Settings) -> Self {
        Self {
            open: false,
            mode: OverlayMode::Settings,
            settings: settings.clone(),
            panel: SettingsPanel::new(settings),
            theme_picker: ThemePicker::new(settings),
            theme_builder: ThemeBuilder::new(settings),
            profile_manager: ProfileManager::new(),
            font_picker: FontPicker::new(settings),
            key_remap: KeyRemapUi::new(settings),
            onboarding: OnboardingPanel::new(settings),
            context_menu: ContextMenuUi::new(),
            command_palette: PaletteOverlay::new(),
            replay: ReplayOverlay::new(),
            connections: ConnectionOverlay::new(),
            connection_form: ConnectionForm::new(),
            session_attach: SessionAttachOverlay::new(),
            open_with: OpenWithOverlay::new(),
            workspace_picker: WorkspacePicker::new(),
            image_view_caption: String::new(),
            risky_paste: RiskyPasteDialog::default(),
            attach_choice_session_id: String::new(),
            confirm_kill_session_id: String::new(),
            detach_switch_cwd: String::new(),
            close_after_save: false,
            key_remap_close_after_save: false,
            picker_return: None,
            builder_from_picker: false,
            confirm_replace_tab: None,
            confirm_remove_host: None,
            confirm_overwrite_layout: None,
            confirm_open_layout: None,
        }
    }

    pub(in crate::native) fn is_open(&self) -> bool {
        self.open
    }

    pub(in crate::native) fn refresh_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        self.panel.refresh(settings);
        self.theme_picker.refresh(settings);
        self.theme_builder.refresh(settings);
        self.key_remap.refresh(settings);
        self.onboarding.refresh(settings);
    }

    pub(in crate::native) fn apply_settings(&mut self, settings: &Settings) {
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

    /// Adopt a setting changed by native chrome rather than by the panel. The
    /// panel rebase preserves unrelated pending edits and navigation while
    /// making the external value the clean baseline, so Save cannot write a
    /// stale snapshot back over a seam resize.
    pub(in crate::native) fn rebase_settings_panel_onto_external(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        self.panel.rebase_onto_external(settings);
    }

    pub(in crate::native) fn open_settings(&mut self) {
        // Defensive no-op for settings steppers; kept with the
        // shared close/switch cleanup path.
        self.panel.end_slider_drag();
        self.open = true;
        self.mode = OverlayMode::Settings;
    }

    pub(in crate::native) fn open_settings_target(&mut self, target: SettingsTarget) {
        self.open_settings();
        if target == SettingsTarget::TabsAndPanes {
            self.panel.open_section("Layout");
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn settings_active_section_for_test(&self) -> Option<&'static str> {
        self.panel.active_section_name_for_test()
    }

    #[cfg(test)]
    pub(in crate::native) fn settings_panel_value_for_test(&self, key: &str) -> Option<String> {
        self.panel.displayed_value_for_test(key)
    }

    /// Refresh the read-only About data on the settings panel (ABOUT). Called by
    /// the App when toggling the settings overlay so the About view reflects the
    /// live GPU adapter (available once the renderer is up).
    pub(in crate::native) fn set_about_info(&mut self, about: crate::native::about::AboutInfo) {
        self.panel.set_about(about);
    }

    pub(in crate::native) fn sync_external_palette_status(&mut self, display: &str) {
        self.panel.sync_external_palette_status(display);
    }

    pub(in crate::native) fn close(&mut self) {
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
        self.risky_paste = RiskyPasteDialog::default();
    }

    pub(in crate::native) fn open_theme_picker(&mut self, settings: &Settings) {
        // A mode switch also runs the shared pointer-capture cleanup path.
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        self.theme_picker.open(settings);
        self.mode = OverlayMode::ThemePicker;
        self.open = true;
    }

    /// Open the theme editor on a draft captured from a pane's live colors
    /// (THEME-CAPTURE). Same surface as [`Self::open_theme_builder`]; only the
    /// starting draft differs, so every editor affordance works unchanged.
    pub(in crate::native) fn open_theme_capture(
        &mut self,
        settings: &Settings,
        spec: crate::theme::ThemeSpec,
    ) {
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        self.theme_builder.open_captured(settings, spec);
        self.mode = OverlayMode::ThemeBuilder;
        self.open = true;
        self.builder_from_picker = false;
    }

    /// Feed a freshly resolved capture into the already-open theme editor
    /// (the in-editor `C` key). Ignored unless the editor is the active mode,
    /// so a stale capture can never overwrite another overlay's state.
    pub(in crate::native) fn apply_theme_capture(&mut self, spec: crate::theme::ThemeSpec) {
        if self.mode != OverlayMode::ThemeBuilder {
            return;
        }
        self.theme_builder.apply_capture(spec);
    }

    /// Test seam (THEME-CAPTURE): the theme editor's working draft when it is
    /// the active overlay mode.
    #[cfg(test)]
    pub(in crate::native) fn theme_builder_draft_for_test(
        &self,
    ) -> Option<crate::theme::ThemeSpec> {
        (self.mode == OverlayMode::ThemeBuilder).then(|| self.theme_builder.draft_for_test())
    }

    pub(in crate::native) fn open_theme_builder(&mut self, settings: &Settings) {
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        self.theme_builder.open(settings);
        self.mode = OverlayMode::ThemeBuilder;
        self.open = true;
        // Not entered from ThemePicker. Back-button behavior is decided by
        // `picker_return`: the Settings-launched path sets it (return to the
        // panel); the standalone keyboard-shortcut path leaves it `None`
        // (close).
        self.builder_from_picker = false;
    }

    /// Open the named-profile manager with an already-loaded local catalog.
    /// Catalog loading is the App's responsibility and happens only when this
    /// overlay opens, never on the default launch path.
    pub(in crate::native) fn open_profile_manager(
        &mut self,
        catalog: crate::profiles::ProfileCatalog,
    ) {
        self.panel.end_slider_drag();
        self.profile_manager.open(catalog);
        self.mode = OverlayMode::ProfileManager;
        self.open = true;
    }

    pub(in crate::native) fn set_profile_manager_message(&mut self, message: impl Into<String>) {
        self.profile_manager.set_message(message);
    }

    /// Open the font-family picker (FONT-PICKER). Runs a fresh metadata scan on
    /// open (typically <100 ms). Backs the picker with the grouped inventory
    /// (from [`crate::text::font_families_grouped`]): the always-present
    /// **Bundled Fonts** (Victor Mono, JetBrains Mono) and the host's distinct
    /// real monospace **System Fonts**.
    pub(in crate::native) fn open_font_picker(&mut self, settings: &Settings) {
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        let groups = crate::text::font_families_grouped();
        self.font_picker.open(settings, groups);
        self.mode = OverlayMode::FontPicker;
        self.open = true;
    }

    pub(in crate::native) fn open_key_bindings(&mut self, settings: &Settings) {
        self.panel.end_slider_drag();
        self.settings = settings.clone();
        self.key_remap.open(settings);
        self.mode = OverlayMode::KeyBindings;
        self.open = true;
    }

    pub(in crate::native) fn open_command_palette(
        &mut self,
        cwd: Option<&str>,
        workspaces: &crate::native::palette_overlay::WorkspacePaletteContext<'_>,
    ) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.command_palette.open_from_process_env(cwd, workspaces);
        self.mode = OverlayMode::CommandPalette;
        self.open = true;
    }

    /// Open the output-replay overlay over a frozen clone of the focused
    /// session's recorded frames (Phase 2). Presentation-only: the overlay owns
    /// the clone and never touches live core state. `frames` is empty when
    /// recording is off or nothing has been recorded yet, in which case the
    /// overlay shows a hint rather than failing to open.
    pub(in crate::native) fn open_replay(&mut self, frames: Vec<Snapshot>) {
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
    pub(in crate::native) fn open_connections(&mut self, entries: Vec<ConnectionHost>) {
        self.open_connections_for_purpose(entries, ConnectionPickerPurpose::Connect);
    }

    /// Open the connection list as a shared picker for a tagged pending action
    /// (ODP-1B). Identical presentation to [`Self::open_connections`]; only the
    /// meaning of accept differs (the App routes the pick per the purpose).
    pub(in crate::native) fn open_connections_for_purpose(
        &mut self,
        entries: Vec<ConnectionHost>,
        purpose: ConnectionPickerPurpose,
    ) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.connections.open_for_purpose(entries, purpose);
        self.mode = OverlayMode::Connections;
        self.open = true;
    }

    /// Open the in-window session-attach summon overlay over a frozen list of
    /// live detached sessions (Phase 5 / B2). Presentation-only: the overlay
    /// owns the list and never attaches anything itself. Accepting a row emits a
    /// [`OverlayOutcome::AttachSession`] for the App to attach into a new tab.
    /// `entries` is empty when no sessions are live, in which case the overlay
    /// shows a hint rather than failing to open.
    pub(in crate::native) fn open_session_attach(&mut self, entries: Vec<ListedSession>) {
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
    pub(in crate::native) fn open_open_with(&mut self, entries: Vec<crate::desktop::DesktopApp>) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.open_with.open(entries);
        self.mode = OverlayMode::OpenWith;
        self.open = true;
    }

    /// Open the "Move to Workspace…" destination picker over a frozen list of
    /// workspaces the clicked tab can move to (W4-v2). The App seeds the list
    /// (every workspace EXCEPT the source) and carries the clicked tab's
    /// `token`; accepting a row emits [`OverlayOutcome::MoveTabToWorkspacePicked`]
    /// with the token + the chosen workspace's original index for the App to
    /// splice. Presentation/filter-only.
    pub(in crate::native) fn open_workspace_picker(
        &mut self,
        entries: Vec<WorkspacePickerEntry>,
        token: SessionToken,
    ) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.workspace_picker.open(entries, token);
        self.mode = OverlayMode::WorkspacePicker;
        self.open = true;
    }

    /// Open the "Open Layout ▸" picker over a frozen list of saved layout names
    /// (LAYOUT-SURFACE). The App seeds the names; accepting a row emits
    /// [`OverlayOutcome::ContextMenuOpenLayout`] with the chosen name for the App
    /// to instantiate (APPEND a new workspace, WP3 8e). An empty list still opens
    /// and shows an explanatory line so the feature is discoverable.
    /// Presentation/filter-only.
    pub(in crate::native) fn open_layout_picker(&mut self, names: Vec<String>) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.workspace_picker.open_layouts(names);
        self.mode = OverlayMode::WorkspacePicker;
        self.open = true;
    }

    #[cfg(test)]
    pub(in crate::native) fn open_command_palette_for_test<H, S>(
        &mut self,
        history: H,
        cwd: Option<&str>,
    ) where
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
    pub(in crate::native) fn open_onboarding(&mut self) {
        self.panel.end_slider_drag();
        self.onboarding.refresh(&self.settings);
        self.mode = OverlayMode::Onboarding;
        self.open = true;
    }

    /// Open the in-terminal image-viewer overlay (Phase 9 / C4). The decoded
    /// image is uploaded into the GPU image layer by the App; this overlay only
    /// carries the presentation-only caption (the filename) and owns the
    /// open/close lifecycle. Esc dismisses; the App clears the GPU overlay image
    /// when the viewer is no longer open. Presentation-only — the live terminal
    /// stays unchanged behind it.
    pub(in crate::native) fn open_image_view(&mut self, caption: String) {
        self.panel.end_slider_drag();
        self.theme_builder.end_channel_drag();
        self.image_view_caption = caption;
        self.mode = OverlayMode::ImageView;
        self.open = true;
    }

    /// Whether the C4 image viewer is the active overlay mode. The App polls
    /// this each frame to clear the GPU overlay image once the viewer closes
    /// (via Esc, click-outside, or any mode switch).
    pub(in crate::native) fn image_view_open(&self) -> bool {
        self.open && self.mode == OverlayMode::ImageView
    }

    /// Whether the attach-choice dialog is the active overlay mode (Phase 14).
    /// Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_attach_choice(&self) -> bool {
        self.open && self.mode == OverlayMode::AttachChoice
    }

    /// Whether the kill-confirmation dialog is the active overlay mode (Manage
    /// Sessions). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_confirm_kill_session(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmKillSession
    }

    /// Whether the replace-tab confirm dialog is the active overlay mode
    /// (ODP-5D). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_confirm_replace_tab(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmReplaceTab
    }

    /// Whether the remove-host confirm dialog is the active overlay mode
    /// (ODP-2C). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_confirm_remove_host(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmRemoveHost
    }

    /// Whether the overwrite-layout confirm dialog is the active overlay mode
    /// (OVERWRITE-WARN). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_confirm_overwrite_layout(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmOverwriteLayout
    }

    /// Whether the open-layout mode dialog is the active overlay mode
    /// (LAYOUT-OPEN-MODE). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_confirm_open_layout(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmOpenLayout
    }

    /// Whether the Detach & switch dialog is the active overlay mode.
    /// Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_detach_switch_choice(&self) -> bool {
        self.open && self.mode == OverlayMode::DetachSwitchChoice
    }

    /// Whether the context menu is the active overlay mode (IN2). The App uses
    /// this to route bare hover Moves to the menu for hover-to-focus, alongside
    /// the slider-drag gate.
    pub(in crate::native) fn is_context_menu(&self) -> bool {
        self.open && self.mode == OverlayMode::ContextMenu
    }

    pub(in crate::native) fn is_risky_paste(&self) -> bool {
        self.open && self.mode == OverlayMode::RiskyPaste
    }

    /// Whether the open context menu is anchored to the workspace rail — a
    /// workspace slot or the empty rail region (RAIL-PIN). The auto-hide rail
    /// keeps itself revealed while such a menu is open so it does not vanish
    /// under the very menu targeting it.
    pub(in crate::native) fn is_workspace_rail_context_menu(&self) -> bool {
        self.is_context_menu()
            && matches!(
                self.context_menu.surface(),
                crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(_)
                    | crate::native::context_menu_ui::ContextMenuSurface::WorkspaceRailEmpty
            )
    }

    /// Whether the close-confirmation dialog is the active overlay mode
    /// (CLOSE-CONFIRM). Used by the App's test seam to assert the dialog opened.
    #[cfg(test)]
    pub(in crate::native) fn is_confirm_close(&self) -> bool {
        self.open && self.mode == OverlayMode::ConfirmClose
    }

    /// Whether the key-remap modal is armed to capture a raw chord (KB-REMAP).
    /// The App gates its chord-capture bypass on this: `true` ONLY when the
    /// KeyBindings mode is active AND a row/conflict is awaiting a chord, so
    /// normal overlay navigation is never diverted (R1).
    pub(in crate::native) fn is_capturing_chord(&self) -> bool {
        self.mode == OverlayMode::KeyBindings && self.key_remap.is_capturing_chord()
    }

    pub(in crate::native) fn toggle_settings(&mut self) {
        if self.open && self.mode == OverlayMode::Settings {
            self.close();
        } else {
            self.open_settings();
        }
    }

    pub(in crate::native) fn save_succeeded(&mut self, changed: usize) {
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
            OverlayMode::ProfileManager => {}
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
            | OverlayMode::WorkspacePicker
            | OverlayMode::ImageView
            | OverlayMode::RiskyPaste
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::DetachSwitchChoice
            | OverlayMode::ConfirmReplaceTab
            | OverlayMode::ConfirmRemoveHost
            | OverlayMode::ConfirmOverwriteLayout
            | OverlayMode::ConfirmOpenLayout => {}
            OverlayMode::ConnectionForm => {}
        }
    }

    pub(in crate::native) fn save_failed(&mut self, message: String) {
        match self.mode {
            OverlayMode::Settings => {
                // A failed save must clear the close-after-save latch: otherwise a
                // later successful save would close the panel the user never asked
                // to close (the SaveAndClose latch outliving its failed attempt).
                self.close_after_save = false;
                self.panel.save_failed(message);
            }
            OverlayMode::ThemePicker => self.theme_picker.save_failed(message),
            OverlayMode::ThemeBuilder => self.theme_builder.save_failed(message),
            OverlayMode::ProfileManager => {
                self.profile_manager.set_message(message);
            }
            OverlayMode::FontPicker => self.font_picker.save_failed(message),
            OverlayMode::KeyBindings => {
                // Same latch-clear for the keybind-editor lane: a failed save leaves
                // no armed close-after-save behind.
                self.key_remap_close_after_save = false;
                self.key_remap.save_failed(message);
            }
            OverlayMode::Onboarding
            | OverlayMode::ContextMenu
            | OverlayMode::CommandPalette
            | OverlayMode::Replay
            | OverlayMode::Connections
            | OverlayMode::SessionAttach
            | OverlayMode::OpenWith
            | OverlayMode::WorkspacePicker
            | OverlayMode::ImageView
            | OverlayMode::RiskyPaste
            | OverlayMode::ConfirmClose
            | OverlayMode::AttachChoice
            | OverlayMode::ConfirmKillSession
            | OverlayMode::DetachSwitchChoice
            | OverlayMode::ConfirmReplaceTab
            | OverlayMode::ConfirmRemoveHost
            | OverlayMode::ConfirmOverwriteLayout
            | OverlayMode::ConfirmOpenLayout => {}
            OverlayMode::ConnectionForm => {}
        }
    }

    pub(in crate::native) fn theme_builder_save_succeeded(
        &mut self,
        saved_name: &str,
        path: &std::path::Path,
        changed: usize,
    ) {
        self.theme_builder.save_succeeded(saved_name, path, changed);
        self.close();
    }

    /// Seed and open the IdentityFile key browser inside the open connection
    /// form (FORM-UX). A no-op unless the form is the active mode. `candidates`
    /// are the `~/.ssh` key PATHS the App discovered by filename heuristics.
    pub(in crate::native) fn open_identity_key_browse(&mut self, candidates: Vec<String>) {
        if self.mode == OverlayMode::ConnectionForm {
            self.connection_form.open_key_browse(candidates);
        }
    }

    /// Feed a completed Test Connection probe result back into the open form
    /// (ODP-8). A no-op unless the connection form is the active mode.
    pub(in crate::native) fn set_connection_form_test_result(
        &mut self,
        result: Result<crate::ssh_connect::ProbeClass, String>,
    ) {
        if self.mode == OverlayMode::ConnectionForm {
            self.connection_form.set_test_result(result);
        }
    }

    pub(super) fn settings_with_theme(&self, theme: Theme) -> Settings {
        let mut settings = self.settings.clone();
        settings.theme = theme;
        settings
    }

    /// Map a [`SettingsPanelOutcome`] into an [`OverlayOutcome`]. This is the
    /// single shared mapping for `handle_input`, `handle_pointer` press, and
    /// `handle_pointer` drag, so the three entry points can never diverge.
    /// `SaveAndClose` sets `close_after_save` so `save_succeeded` closes the
    /// overlay after the App persists the changes.
    pub(super) fn map_settings_outcome(&mut self, outcome: SettingsPanelOutcome) -> OverlayOutcome {
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
            SettingsPanelOutcome::OpenThemeBuilder => {
                // Same return contract as the pickers: remember the panel level
                // so the builder's Esc / back navigates back to the settings
                // panel instead of closing the whole overlay.
                self.picker_return = Some(PickerReturn {
                    level: self.panel.current_level(),
                });
                OverlayOutcome::OpenThemeBuilder
            }
            SettingsPanelOutcome::OpenProfileManager => {
                self.picker_return = Some(PickerReturn {
                    level: self.panel.current_level(),
                });
                OverlayOutcome::OpenProfileManager
            }
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

    pub(super) fn return_to_settings_panel(&mut self) {
        if let Some(PickerReturn { level }) = self.picker_return.take() {
            self.panel.set_level(level);
        }
        self.mode = OverlayMode::Settings;
        self.open = true;
    }
}
