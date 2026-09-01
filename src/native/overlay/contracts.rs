// SPDX-License-Identifier: GPL-3.0-only
//! Overlay data contracts: modes, outcomes, inputs, pointers, and the
//! presentation signature shared by every overlay component.
//!
//! This module sits at the bottom of the overlay dependency direction: state,
//! dialogs, input, layout, and rendering all depend on it, and it depends on
//! none of them.

use crate::connection_hosts::ConnectionHost;
use crate::native::connection_form::ConnectionFormSignature;
use crate::native::connection_overlay::ConnectionOverlaySignature;
use crate::native::context_menu_ui::ContextMenuSignature;
use crate::native::font_picker::FontPickerSignature;
use crate::native::key_remap_ui::KeyRemapSignature;
use crate::native::onboarding::OnboardingSignature;
use crate::native::open_with_overlay::OpenWithOverlaySignature;
use crate::native::palette_overlay::PaletteOverlaySignature;
use crate::native::profile_manager::ProfileManagerSignature;
use crate::native::replay_overlay::ReplayOverlaySignature;
use crate::native::session::SessionToken;
use crate::native::session_attach_overlay::SessionAttachOverlaySignature;
use crate::native::settings_panel::{SettingsLevel, SettingsPanelSignature};
use crate::native::theme_builder::{ThemeBuilderSaveRequest, ThemeBuilderSignature};
use crate::native::theme_picker::ThemePickerSignature;
use crate::native::workspace_picker::WorkspacePickerSignature;
use crate::selection::CellPoint;
use crate::settings::Settings;

/// Which Save as Layout a pending action refers to (OVERWRITE-WARN). Carried by
/// the overwrite-confirm dialog so its Replace / different-name arms can re-drive
/// the right save path: the whole application, or one workspace by rail index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum LayoutSaveKind {
    /// Save every workspace as one layout (the primary "Save as Layout…").
    WholeApp,
    /// Save the single workspace at this rail index ("Save Workspace as Layout…").
    Workspace(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum SettingsTarget {
    Root,
    TabsAndPanes,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::native) enum OverlayOutcome {
    Consumed,
    Close,
    /// Confirm the held original risky text without transformation.
    RiskyPaste,
    /// Confirm the held text using the explicit reversible one-line encoding.
    RiskyPasteOneLine,
    /// Dismiss the risky-paste modal and discard its held text.
    RiskyPasteCancel,
    /// Dismiss the first-run onboarding card. Like `Close`, but the App also
    /// persists a first-run marker (ensures `odytty.conf` exists) so the welcome
    /// card does not reshow on the next launch — dismissal alone otherwise
    /// writes nothing, and onboarding's gate is purely "does the config exist".
    CloseOnboarding,
    OpenThemePicker,
    OpenThemeBuilder,
    /// Open the named-profile manager from Settings (v0.14.0 A2).
    OpenProfileManager,
    /// The theme editor asked to (re-)capture the focused pane's live colors
    /// into its draft (THEME-CAPTURE). The App resolves the pane's effective
    /// dynamic-color state and feeds the resulting draft back into the builder;
    /// the overlay stays open and nothing is applied or saved.
    CaptureThemeColors,
    OpenKeyBindings,
    /// Open the font-family picker (FONT-PICKER). Emitted from the Fonts
    /// section's `font_family` row. The picker overlay is sequenced in the
    /// the font picker; for now `apply_overlay_outcome` handles it as a stub.
    OpenFontPicker,
    /// Boxed because `Settings` is by far the largest payload across this
    /// short-lived outcome enum; boxing keeps the enum small to move and clears
    /// the `large_enum_variant` lint as the settings surface grows.
    ApplySettings(Box<Settings>),
    SaveSettings(Vec<crate::settings::SettingEdit>),
    SaveTheme(ThemeBuilderSaveRequest),
    /// Persist a named profile from the profile manager (create/edit/duplicate/
    /// rename). `replace` deletes the prior profile file after a successful write.
    SaveProfile {
        profile: Box<crate::profiles::LaunchProfile>,
        replace: Option<String>,
    },
    /// Delete a named profile after confirmation.
    DeleteProfile(String),
    /// Open a file-picker to import a `.profile.json` document.
    ImportProfile,
    /// Open a save dialog to export the named profile.
    ExportProfile(String),
    /// Run the right-click menu's Copy / Paste / Select All action (IN2). The
    /// overlay has already closed itself by the time these are emitted; the App
    /// dispatches them to the existing copy/paste shortcuts and `handle_select_all`.
    ContextMenuCopy,
    ContextMenuCut,
    ContextMenuPaste,
    ContextMenuDelete,
    ContextMenuSelectAll,
    ContextMenuSelectCommandOutput,
    ContextMenuSelectCommandWithPrompt,
    ContextMenuCopyCommandOutput,
    ContextMenuCopyCommandWithPrompt,
    ContextMenuSearchCommandOutput,
    ContextMenuJumpFailedCommandPrev,
    ContextMenuJumpFailedCommandNext,
    ContextMenuExportCommandOutput,
    ContextMenuNewTab,
    /// Open a local shell in a new tab from a bound-workspace tab menu (F6-W5
    /// escape hatch). The overlay has closed itself; the App dispatches this to
    /// `handle_new_local_tab`, bypassing the workspace host binding.
    ContextMenuNewLocalTab,
    /// Duplicate the right-clicked tab from the tab context menu: a fresh local
    /// shell in the active pane's cwd (F1 cwd inheritance), not a process fork.
    /// The overlay has already closed itself; the App dispatches this to
    /// `handle_new_local_tab` (the same cwd-aware local-tab spawn New Local Tab
    /// uses), the same effect as the `duplicate-tab` bindable action.
    ContextMenuDuplicateTab,
    /// Launch another OdyTTY window from the context menu (F1). The overlay has
    /// already closed itself; the App dispatches this to `handle_new_window`
    /// (the same handler the `Ctrl+Shift+N` chord fires).
    ContextMenuNewWindow,
    ContextMenuRenameTab(SessionToken),
    /// Create a fresh workspace from the rail `+` slot / workspace context menu
    /// (§7.4). The overlay has closed itself; the App dispatches to
    /// `handle_new_workspace`.
    ContextMenuNewWorkspace,
    /// Duplicate the right-clicked workspace (WorkspaceSlot): open a fresh
    /// workspace whose first shell spawns in the active pane's cwd (F1 cwd
    /// inheritance), not a process fork. The overlay has already closed itself;
    /// the App dispatches this to `handle_duplicate_workspace`, the same effect
    /// as the `duplicate-workspace` bindable action.
    ContextMenuDuplicateWorkspace,
    /// Rename the workspace at rail index `usize` in place (WorkspaceSlot). The
    /// App opens the shared rename field targeting that workspace.
    ContextMenuRenameWorkspace(usize),
    /// Close the workspace at rail index `usize` entirely (WorkspaceSlot).
    ContextMenuCloseWorkspace(usize),
    /// Move the workspace at rail index `usize` one slot toward the front of the
    /// rail (RAIL-REORDER, WorkspaceSlot). The App reorders and follows the
    /// active workspace by identity.
    ContextMenuMoveWorkspaceUp(usize),
    /// Move the workspace at rail index `usize` one slot toward the back of the
    /// rail (RAIL-REORDER, WorkspaceSlot).
    ContextMenuMoveWorkspaceDown(usize),
    /// Rename the ACTIVE workspace (content-grid menu, which has no per-workspace
    /// click target). The App resolves the active index itself.
    ContextMenuRenameActiveWorkspace,
    /// Close the ACTIVE workspace (content-grid menu, no per-workspace target).
    ContextMenuCloseActiveWorkspace,
    /// Bind the ACTIVE workspace to a host (ODP-6B). The context menu closed
    /// itself; the App opens the shared host picker (ODP-1B) seeded for the
    /// BindWorkspace purpose, and the pick emits [`Self::BindWorkspaceToHost`].
    ContextMenuBindWorkspace,
    /// Unbind the ACTIVE workspace (ODP-6B), returning its New Tab to a local
    /// shell. The App clears the binding directly (no host choice needed).
    ContextMenuUnbindWorkspace,
    /// Bind the workspace at this rail index to a host (RAIL-BIND). The rail
    /// context menu targets the CLICKED slot; the App opens the shared host
    /// picker seeded for the `BindWorkspaceIndex` purpose.
    ContextMenuBindWorkspaceAt(usize),
    /// Unbind the workspace at this rail index (RAIL-BIND). The App clears the
    /// clicked slot's binding directly.
    ContextMenuUnbindWorkspaceAt(usize),
    /// Bind the active workspace to the saved host with this alias (ODP-1B/6B).
    /// Emitted by the shared host picker when opened for the BindWorkspace
    /// purpose; the App calls the frozen `set_active_workspace_default_profile`.
    BindWorkspaceToHost(String),
    /// Bind the workspace at this rail index to the saved host with this alias
    /// (RAIL-BIND). Emitted by the shared host picker when opened for the
    /// `BindWorkspaceIndex` purpose; the App binds the clicked slot.
    BindWorkspaceAtToHost(usize, String),
    /// Open the shared host picker for the tab holding this token, seeded for
    /// the ODP-5D "Connect to host" purpose (a new adjacent remote tab). The
    /// menu closed itself; the App opens the picker.
    ContextMenuConnectToHost(SessionToken),
    /// Open the shared host picker for the tab holding this token, seeded for
    /// the ODP-5D "Replace this tab with host" purpose. The menu closed itself.
    ContextMenuReplaceTabWithHost(SessionToken),
    /// Open the picked saved host in a NEW tab positioned right after the tab
    /// holding this token (ODP-5D). Emitted by the shared host picker opened for
    /// the ConnectTabAfter purpose; the picker closed itself.
    ConnectHostInTabAfter(Box<ConnectionHost>, SessionToken),
    /// Replace the tab holding this token with the picked saved host (ODP-5D).
    /// Emitted by the shared host picker opened for the ReplaceTab purpose; the
    /// App gates the destructive close behind a confirm when a foreground child
    /// is running, else replaces directly.
    ReplaceTabWithHostPicked(Box<ConnectionHost>, SessionToken),
    /// The replace-tab confirm dialog was accepted (ODP-5D): close the tab
    /// holding this token and open the picked host in its slot. Emitted only
    /// after the running-child confirm; the dialog closed itself.
    ReplaceTabWithHostConfirmed(Box<ConnectionHost>, SessionToken),
    /// Open a connection-manager row's host in a NEW workspace pre-bound to it
    /// (ODP-2C "Open in New Workspace"). The context menu closed itself; the App
    /// creates a fresh workspace, sets its `default_profile`, and connects its
    /// first tab. Boxed to keep this short-lived enum small.
    ConnectHostInNewWorkspace(Box<ConnectionHost>),
    /// The remove-host confirm dialog was accepted (ODP-2C "Remove…"): delete the
    /// OdyTTY-owned `hosts.conf` block for this host and reopen the connection
    /// manager so the row disappears. Emitted only after the confirm; the dialog
    /// closed itself. Boxed to keep this short-lived enum small.
    RemoveConnectionConfirmed(Box<ConnectionHost>),
    /// Persist a host built in the Add / Edit connection form (REMOTE-UX P4).
    /// The overlay has closed itself; the App appends a new block (`edit_target`
    /// `None`) or byte-splices over the named block, then raises a one-line
    /// notice.
    SaveConnection {
        host: Box<ConnectionHost>,
        edit_target: Option<String>,
    },
    /// Run a background Test Connection probe for the Add / Edit form (ODP-8).
    /// The overlay stays open; the App spawns the probe and feeds the tri-state
    /// result back through `set_connection_form_test_result`.
    TestConnection(Box<ConnectionHost>),
    /// Open the IdentityFile key browser for the Add / Edit form (FORM-UX). The
    /// overlay stays open; the App scans `~/.ssh` for candidate private keys
    /// (filename heuristics only — never key contents) and seeds the browser
    /// back through `open_identity_key_browse`.
    BrowseIdentityKeys,
    ContextMenuCloseTab,
    /// Close a specific tab by token from a tab-slot right-click (NF-F7-1). The
    /// overlay has already closed itself; the App reaps the tab that holds
    /// `token`, not the active one.
    ContextMenuCloseTabToken(SessionToken),
    /// Close every tab except the one holding `token` (F7 "Close Other Tabs").
    ContextMenuCloseOtherTabs(SessionToken),
    /// Open the "Move to Workspace" destination picker for the tab holding
    /// `token` (W4-v2). The context menu has already closed itself; the App
    /// seeds the picker with every workspace but the source and routes the
    /// accepted destination back as [`Self::MoveTabToWorkspacePicked`].
    ContextMenuMoveToWorkspace(SessionToken),
    /// Move the tab holding `token` into the workspace at the given rail index,
    /// chosen from the "Move to Workspace" picker (W4-v2). The overlay has
    /// already closed itself; the App splices the tab between workspaces without
    /// switching (unless the source workspace empties).
    MoveTabToWorkspacePicked(SessionToken, usize),
    /// Save the workspace at the given rail index as a named layout, chosen from
    /// a WorkspaceSlot rail menu (LAYOUT-SURFACE). The menu closed itself; the
    /// App opens the "Layout name:" prompt seeded from that workspace.
    ContextMenuSaveLayoutAt(usize),
    /// Save the ACTIVE workspace as a named layout, chosen from the content-grid
    /// workspace section (LAYOUT-SURFACE). The App opens the "Layout name:"
    /// prompt seeded from the active workspace.
    ContextMenuSaveActiveLayout,
    /// Save the WHOLE application (every workspace) as one named layout
    /// (SAVE-ALL-LAYOUT), chosen from the content-grid section or the empty rail.
    /// The App opens the "Layout name:" prompt seeded from the active workspace.
    ContextMenuSaveAllLayout,
    /// The overwrite-layout confirm dialog's Replace arm was accepted
    /// (OVERWRITE-WARN). The App re-captures the current state and force-writes
    /// the layout under `name`, clobbering the existing file. `kind` says which
    /// save it was (whole app vs. one workspace).
    OverwriteLayoutConfirmed {
        name: String,
        kind: LayoutSaveKind,
    },
    /// The overwrite-layout confirm dialog's "different name" arm was chosen
    /// (OVERWRITE-WARN). The App reopens the "Layout name:" prompt seeded with
    /// `name` so the user can pick a non-colliding name; `kind` restores the
    /// same save target.
    RenameLayoutInstead {
        name: String,
        kind: LayoutSaveKind,
    },
    /// The open-layout mode dialog's Replace arm was accepted (LAYOUT-OPEN-MODE).
    /// The App tears down every current workspace and instantiates the saved
    /// layout `name` as the whole application.
    OpenLayoutReplace(String),
    /// The open-layout mode dialog's Add arm was chosen (LAYOUT-OPEN-MODE). The
    /// App appends the saved layout `name` beside the current workspaces (the
    /// pre-existing open-append behavior).
    OpenLayoutAdd(String),
    /// Open the "Open Layout ▸" picker (LAYOUT-SURFACE). The menu closed itself;
    /// the App seeds the picker with the saved layout names.
    ContextMenuOpenLayoutPicker,
    /// The layout picker chose a saved layout by name (LAYOUT-SURFACE). The
    /// overlay closed itself; the App instantiates it (APPEND a new workspace).
    ContextMenuOpenLayout(String),
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
    /// overlay has already closed itself; the target distinguishes generic
    /// content entry from tab/workspace entry into the Layout section.
    ContextMenuSettings(SettingsTarget),
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
    /// Connect to an ad-hoc host AND append it to `hosts.conf` (ADHOC-CONNECT
    /// save offer). Emitted only from the synthetic "Connect to: …" row when the
    /// query matched no saved host; the App spawns the connection and persists a
    /// `Host` block. Boxed for the same size reason as `Connect`.
    ConnectAndSave(Box<ConnectionHost>),
    /// Launch a new tab through the named-profile resolver (v0.14). Emitted from
    /// a profile row in the connection manager; the App routes through the same
    /// path as palette profile launch.
    LaunchProfile(String),
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
    /// The user chose "Detach & switch" on the focused pane. The menu
    /// has already closed itself; the App reads the focused pane's cwd and opens
    /// the [`OverlayMode::DetachSwitchChoice`] dialog.
    ContextMenuDetachSwitch,
    /// The user chose "Swap (close this)" in the Detach & switch dialog:
    /// spawn a fresh managed session in the carried cwd, attach + focus it,
    /// then close the original focused pane. The overlay has already closed
    /// itself. Empty string = unknown cwd (spawn in the default directory). The
    /// cwd is display+spawn-config only — it flows into the same
    /// `working_directory` `odytty new` uses, never a raw shell arg.
    DetachSwitchSwap(String),
    /// The user chose "Keep both" in the Detach & switch dialog:
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum OverlayMode {
    Settings,
    ThemePicker,
    ThemeBuilder,
    /// Named-profile manager (v0.14.0 A2): list, create, duplicate, rename,
    /// edit, validate, import, export, and delete local profiles. Opened from
    /// Settings; catalog load is on-demand and never runs on default startup.
    ProfileManager,
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
    /// Add / Edit connection form (REMOTE-UX P4): create or edit a hosts.conf
    /// block. Presentation + validation only; Save emits the built host for the
    /// App to persist (append for Add, byte-splice for Edit).
    ConnectionForm,
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
    /// "Move to Workspace" destination picker (W4-v2): list the workspaces
    /// the clicked tab can move to (all but its source), type-to-filter, and
    /// on Enter splice the tab into the chosen workspace. Presentation/filter
    /// only; never mutates live core state.
    WorkspacePicker,
    /// In-terminal image viewer (Phase 9 / C4): a presentation-only overlay
    /// that renders a decoded image span ("Open in OdyTTY") centered over a
    /// dimmed backdrop panel, through the existing GPU image-layer raster path.
    /// Esc dismisses; closed = live frame byte-identical.
    ImageView,
    /// Suspicious-paste confirmation. Only escaped, bounded presentation data
    /// enters the overlay; original text remains in App-owned transient state.
    RiskyPaste,
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
    /// Detach & switch choice dialog. A centered, static 3-way modal
    /// shown when the user picks "Detach & switch" on the focused pane: `[S]`
    /// swaps (spawn a managed session + close this pane), `[K]` keeps both
    /// (spawn + leave this pane), `[Esc]` cancels. Honest framing: a SPAWN of a
    /// fresh shell in the same cwd, not a live-process migration. Modeled on
    /// `AttachChoice`; the focused pane's cwd is carried on the overlay.
    DetachSwitchChoice,
    /// Replace-tab confirm dialog (ODP-5D). A centered, static modal shown when
    /// "Replace with Host…" targets a tab whose pane holds a running foreground
    /// child: `[Enter / Y]` closes that tab and opens the picked host in its
    /// slot (emits [`OverlayOutcome::ReplaceTabWithHostConfirmed`]), `[Esc / N]`
    /// cancels. An idle tab replaces directly, never reaching this dialog.
    /// Modeled on `ConfirmKillSession`; the pending host + target token are
    /// carried on the overlay.
    ConfirmReplaceTab,
    /// Remove-host confirm dialog (ODP-2C). A centered, static modal shown when
    /// "Remove…" is chosen on a connection-manager row: `[Enter / Y]` deletes
    /// the OdyTTY-owned `hosts.conf` block (emits
    /// [`OverlayOutcome::RemoveConnectionConfirmed`]) and reopens the manager;
    /// `[Esc / N]` cancels back to the manager with its selection intact.
    /// Modeled on `ConfirmKillSession`; the pending host is carried on the
    /// overlay.
    ConfirmRemoveHost,
    /// Overwrite-layout confirm dialog (OVERWRITE-WARN). A centered, static
    /// three-way modal shown when a Save as Layout resolves to a name that
    /// already exists on disk: `[Enter]` replaces the existing layout (emits
    /// [`OverlayOutcome::OverwriteLayoutConfirmed`]), `[R]` reopens the name
    /// prompt for a different name (emits [`OverlayOutcome::RenameLayoutInstead`]),
    /// `[Esc]` cancels the save entirely. The resolved name + which save it was
    /// are carried on the overlay (`confirm_overwrite_layout`).
    ConfirmOverwriteLayout,
    /// Open-layout mode dialog (LAYOUT-OPEN-MODE). A centered, static three-way
    /// modal shown when a saved layout is opened onto a window that already holds
    /// real state (more than a single pristine workspace): `[Enter]` replaces the
    /// current workspaces with the saved set (emits
    /// [`OverlayOutcome::OpenLayoutReplace`]), `[A]` appends the saved set beside
    /// the current one (emits [`OverlayOutcome::OpenLayoutAdd`]), `[Esc]` cancels.
    /// The layout name is carried on the overlay (`confirm_open_layout`).
    ConfirmOpenLayout,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(in crate::native) struct RiskyPasteDialog {
    pub(in crate::native) line_count: usize,
    pub(in crate::native) byte_count: usize,
    pub(in crate::native) escaped_preview: String,
    pub(in crate::native) preview_truncated: bool,
    pub(in crate::native) one_line_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PickerReturn {
    pub(super) level: SettingsLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum OverlayInput {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    Activate,
    /// Activate with the Shift modifier held (Shift+Enter) — a secondary accept
    /// most overlays ignore. The connection manager uses it for
    /// connect-and-save; other overlays treat it as inert.
    ActivateAlt,
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
pub(in crate::native) enum PointerButton {
    Left,
    Right,
}

/// Pointer events delivered to the overlay (UX4-P1/P2), the mouse analogue of
/// [`OverlayInput`]. `Press` drives clicks, `Wheel` drives free scroll, and
/// `Move`/`Release` drive modes that still capture pointer motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::native) enum OverlayPointer {
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
pub(in crate::native) struct OverlayRenderSignature {
    pub(in crate::native) open: bool,
    pub(in crate::native) mode: OverlayMode,
    pub(in crate::native) panel: SettingsPanelSignature,
    pub(in crate::native) theme_picker: ThemePickerSignature,
    pub(in crate::native) theme_builder: ThemeBuilderSignature,
    pub(in crate::native) profile_manager: ProfileManagerSignature,
    pub(in crate::native) font_picker: FontPickerSignature,
    pub(in crate::native) key_remap: KeyRemapSignature,
    pub(in crate::native) onboarding: OnboardingSignature,
    pub(in crate::native) context_menu: ContextMenuSignature,
    pub(in crate::native) command_palette: PaletteOverlaySignature,
    pub(in crate::native) replay: ReplayOverlaySignature,
    pub(in crate::native) connections: ConnectionOverlaySignature,
    pub(in crate::native) connection_form: ConnectionFormSignature,
    pub(in crate::native) session_attach: SessionAttachOverlaySignature,
    pub(in crate::native) open_with: OpenWithOverlaySignature,
    pub(in crate::native) workspace_picker: WorkspacePickerSignature,
}
