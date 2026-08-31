// SPDX-License-Identifier: GPL-3.0-only
//! Rebindable terminal-local actions and their configuration identities.

use super::normalize_name;

/// Terminal-local actions that can be rebound without changing PTY input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindableAction {
    Search,
    SettingsPanel,
    ThemePicker,
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    /// Jump the viewport to the previous shell prompt (OSC 133 boundary).
    JumpPromptPrev,
    /// Jump the viewport to the next shell prompt (OSC 133 boundary).
    JumpPromptNext,
    /// Select visible output from a verified OSC 133 command range.
    SelectCommandOutput,
    /// Select the prompt, command line, and output from a verified range.
    SelectCommandWithPrompt,
    /// Copy visible output from a verified OSC 133 command range.
    CopyCommandOutput,
    /// Copy the prompt, command line, and output from a verified range.
    CopyCommandWithPrompt,
    /// Open search restricted to a verified command-output range.
    SearchCommandOutput,
    /// Navigate to the previous command with an explicit nonzero exit status.
    JumpFailedCommandPrev,
    /// Navigate to the next command with an explicit nonzero exit status.
    JumpFailedCommandNext,
    /// Export visible command output through an explicit native save dialog.
    ExportCommandOutput,
    /// Arm a one-shot notification for the currently running OSC 133 command.
    NotifyCommandFinished,
    /// Enter keyboard scrollback selection ("copy") mode.
    CopyMode,
    /// Activate keyboard pattern-select hints (URLs, paths, and SHAs).
    Hints,
    /// Clear the current shell input line with the configured PTY action.
    ClearInput,
    /// Open the in-window command palette.
    CommandPalette,
    /// Open the output-replay overlay.
    SessionReplay,
    /// Open the connection-manager overlay.
    ConnectionManager,
    /// Open the theme builder overlay directly.
    ThemeBuilder,
    /// Open the in-window session-attach overlay.
    SessionAttach,
    NewTab,
    /// Launch another top-level OdyTTY process.
    NewWindow,
    NextTab,
    PrevTab,
    CloseTab,
    /// Open a fresh local tab in the active pane's working directory.
    DuplicateTab,
    /// Create a fresh workspace and switch to it.
    NewWorkspace,
    /// Duplicate the active workspace's shape into fresh shell sessions.
    DuplicateWorkspace,
    /// Close the active workspace and all of its tabs and panes.
    CloseWorkspace,
    /// Rename the active workspace in place.
    RenameWorkspace,
    /// Switch to the next workspace in rail order.
    NextWorkspace,
    /// Switch to the previous workspace in rail order.
    PrevWorkspace,
    /// Open the command palette focused on workspace navigation.
    WorkspacePicker,
    /// Split the focused pane side by side.
    SplitColumns,
    /// Split the focused pane into stacked rows.
    SplitRows,
    /// Move focus to the pane left of the focused pane.
    FocusPaneLeft,
    /// Move focus to the pane right of the focused pane.
    FocusPaneRight,
    /// Move focus to the pane above the focused pane.
    FocusPaneUp,
    /// Move focus to the pane below the focused pane.
    FocusPaneDown,
    /// Cycle focus to the next pane in tree order.
    FocusPaneNext,
    /// Close the focused pane.
    ClosePane,
    /// Toggle zoom for the focused pane.
    ZoomPane,
    /// Reset split ratios in the active tab to equal shares.
    EqualizePanes,
}

impl BindableAction {
    /// Every action in canonical keybinding-editor order.
    ///
    /// This is the single source of truth used by the editor and coverage
    /// guards, so additions must remain exhaustive.
    pub const ALL: [Self; 49] = [
        Self::Search,
        Self::SettingsPanel,
        Self::ThemePicker,
        Self::Copy,
        Self::Paste,
        Self::ScrollPageUp,
        Self::ScrollPageDown,
        Self::JumpPromptPrev,
        Self::JumpPromptNext,
        Self::SelectCommandOutput,
        Self::SelectCommandWithPrompt,
        Self::CopyCommandOutput,
        Self::CopyCommandWithPrompt,
        Self::SearchCommandOutput,
        Self::JumpFailedCommandPrev,
        Self::JumpFailedCommandNext,
        Self::ExportCommandOutput,
        Self::NotifyCommandFinished,
        Self::CopyMode,
        Self::Hints,
        Self::ClearInput,
        Self::CommandPalette,
        Self::ConnectionManager,
        Self::SessionReplay,
        Self::ThemeBuilder,
        Self::SessionAttach,
        Self::NewTab,
        Self::NewWindow,
        Self::NextTab,
        Self::PrevTab,
        Self::CloseTab,
        Self::DuplicateTab,
        Self::NewWorkspace,
        Self::DuplicateWorkspace,
        Self::CloseWorkspace,
        Self::RenameWorkspace,
        Self::NextWorkspace,
        Self::PrevWorkspace,
        Self::WorkspacePicker,
        Self::SplitColumns,
        Self::SplitRows,
        Self::FocusPaneLeft,
        Self::FocusPaneRight,
        Self::FocusPaneUp,
        Self::FocusPaneDown,
        Self::FocusPaneNext,
        Self::ClosePane,
        Self::ZoomPane,
        Self::EqualizePanes,
    ];

    pub(super) fn parse(raw: &str) -> Option<Self> {
        match normalize_name(raw).as_str() {
            "search" | "searchtoggle" | "togglesearch" => Some(Self::Search),
            "settings" | "settingspanel" | "togglesettings" | "preferences" | "prefs" => {
                Some(Self::SettingsPanel)
            }
            "theme" | "themes" | "themepicker" | "picktheme" | "choosetheme" => {
                Some(Self::ThemePicker)
            }
            "copy" => Some(Self::Copy),
            "paste" => Some(Self::Paste),
            "scrollup" | "pageup" | "scrollpageup" | "scrollbackpageup" => Some(Self::ScrollPageUp),
            "scrolldown" | "pagedown" | "scrollpagedown" | "scrollbackpagedown" => {
                Some(Self::ScrollPageDown)
            }
            "jumppromptprev" | "promptprev" | "prevprompt" | "jumpprevprompt" => {
                Some(Self::JumpPromptPrev)
            }
            "jumppromptnext" | "promptnext" | "nextprompt" | "jumpnextprompt" => {
                Some(Self::JumpPromptNext)
            }
            "selectcommandoutput" | "selectoutput" => Some(Self::SelectCommandOutput),
            "selectcommandwithprompt" | "selectcommand" => Some(Self::SelectCommandWithPrompt),
            "copycommandoutput" | "copyoutput" => Some(Self::CopyCommandOutput),
            "copycommandwithprompt" | "copycommand" => Some(Self::CopyCommandWithPrompt),
            "searchcommandoutput" | "searchcommand" => Some(Self::SearchCommandOutput),
            "jumpfailedcommandprev" | "prevfailedcommand" => Some(Self::JumpFailedCommandPrev),
            "jumpfailedcommandnext" | "nextfailedcommand" => Some(Self::JumpFailedCommandNext),
            "exportcommandoutput" | "savecommandoutput" => Some(Self::ExportCommandOutput),
            "notifycommandfinished" | "notifywhencommandfinishes" | "commandfinishnotification" => {
                Some(Self::NotifyCommandFinished)
            }
            "copymode" | "selectmode" => Some(Self::CopyMode),
            "hints" | "hint" | "quickselect" | "patternselect" => Some(Self::Hints),
            "clearinput" | "clearline" | "killline" | "clear" => Some(Self::ClearInput),
            "commandpalette" | "palette" | "cmdpalette" | "fuzzypalette" => {
                Some(Self::CommandPalette)
            }
            "sessionreplay" | "replay" | "outputreplay" | "replayoverlay" => {
                Some(Self::SessionReplay)
            }
            "connectionmanager" | "connections" | "connect" | "sshmanager" | "hosts" => {
                Some(Self::ConnectionManager)
            }
            "themebuilder" | "buildtheme" | "newtheme" | "themeeditor" => Some(Self::ThemeBuilder),
            "sessionattach" | "attach" | "attachsession" | "sessions" | "sessionpicker" => {
                Some(Self::SessionAttach)
            }
            "newtab" | "tabnew" => Some(Self::NewTab),
            "newwindow" | "windownew" => Some(Self::NewWindow),
            "nexttab" | "tabnext" => Some(Self::NextTab),
            "prevtab" | "previoustab" | "tabprev" => Some(Self::PrevTab),
            "closetab" | "tabclose" => Some(Self::CloseTab),
            "duplicatetab" | "tabduplicate" | "duplicate" => Some(Self::DuplicateTab),
            "newworkspace" | "workspacenew" => Some(Self::NewWorkspace),
            "duplicateworkspace" | "workspaceduplicate" => Some(Self::DuplicateWorkspace),
            "closeworkspace" | "workspaceclose" => Some(Self::CloseWorkspace),
            "renameworkspace" | "workspacerename" => Some(Self::RenameWorkspace),
            "nextworkspace" | "workspacenext" => Some(Self::NextWorkspace),
            "prevworkspace" | "previousworkspace" | "workspaceprev" => Some(Self::PrevWorkspace),
            "workspacepicker" | "workspaceswitcher" | "workspaces" | "pickworkspace" => {
                Some(Self::WorkspacePicker)
            }
            "splitcolumns" | "splitsidebyside" | "splitright" => Some(Self::SplitColumns),
            "splitrows" | "splitstacked" | "splitdown" => Some(Self::SplitRows),
            "focuspaneleft" | "paneleft" => Some(Self::FocusPaneLeft),
            "focuspaneright" | "paneright" => Some(Self::FocusPaneRight),
            "focuspaneup" | "paneup" => Some(Self::FocusPaneUp),
            "focuspanedown" | "panedown" => Some(Self::FocusPaneDown),
            "focuspanenext" | "panenext" | "nextpane" => Some(Self::FocusPaneNext),
            "closepane" | "paneclose" => Some(Self::ClosePane),
            "zoompane" | "panezoom" | "togglezoom" => Some(Self::ZoomPane),
            "equalizepanes" | "equalize" | "panesequalize" => Some(Self::EqualizePanes),
            _ => None,
        }
    }

    pub fn is_pane_action(self) -> bool {
        matches!(
            self,
            Self::SplitColumns
                | Self::SplitRows
                | Self::FocusPaneLeft
                | Self::FocusPaneRight
                | Self::FocusPaneUp
                | Self::FocusPaneDown
                | Self::FocusPaneNext
                | Self::ClosePane
                | Self::ZoomPane
                | Self::EqualizePanes
        )
    }
}
