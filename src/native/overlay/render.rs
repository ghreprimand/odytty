// SPDX-License-Identifier: GPL-3.0-only
//! Overlay rendering: panel application, the mode's visible body lines, line
//! conversions from component leaves, and the cell painters.
//!
//! Rendering draws into a snapshot copy only. A closed overlay paints nothing.

use crate::core::{Attrs, Cell, Color, Snapshot};
use crate::native::connection_form::ConnectionFormLine;
use crate::native::connection_overlay::ConnectionOverlayLine;
use crate::native::font_picker::FontPickerLine;
use crate::native::key_remap_ui::KeyRemapLine;
use crate::native::onboarding::OnboardingLine;
use crate::native::open_with_overlay::OpenWithOverlayLine;
use crate::native::palette_overlay::PaletteOverlayLine;
use crate::native::replay_overlay::ReplayOverlayLine;
use crate::native::session_attach_overlay::SessionAttachOverlayLine;
use crate::native::theme_builder::ThemeBuilderLine;
use crate::native::theme_picker::ThemePickerLine;
use crate::native::workspace_picker::WorkspacePickerLine;
use crate::theme::Srgb;
use unicode_width::UnicodeWidthChar;

use super::contracts::{OverlayMode, OverlayRenderSignature};
use super::layout::*;
use super::state::OverlayUi;

impl OverlayUi {
    /// The overlay's title-bar text — the single source of truth shared by the
    /// painter ([`apply_overlay`]) and the back-arrow hit-test
    /// ([`Self::picker_title_back_hit`]). A leading `\u{2190}` marks the modes
    /// that carry a clickable back/close affordance; deriving the hit-test from
    /// this string (rather than a hand-maintained mode list) is what stops a new
    /// `\u{2190}`-titled mode from drifting into a click-dead arrow (the NF15
    /// recurrence class — About, then Connections, were each such a miss).
    /// `ContextMenu` has no title bar (early-dispatched to its own layout) and
    /// returns an empty string.
    pub(in crate::native) fn title(&self) -> String {
        match self.mode {
            OverlayMode::Settings => self.panel.panel_title(),
            OverlayMode::ThemePicker => "\u{2190} OdyTTY Themes  (Esc = back)".to_owned(),
            OverlayMode::ThemeBuilder => "\u{2190} OdyTTY Theme Builder  (Esc = back)".to_owned(),
            OverlayMode::FontPicker => "\u{2190} OdyTTY Font Picker  (Esc = back)".to_owned(),
            OverlayMode::KeyBindings => "\u{2190} OdyTTY Key Bindings  (Esc = back)".to_owned(),
            OverlayMode::Onboarding => "Welcome to OdyTTY".to_owned(),
            OverlayMode::CommandPalette => "Command Palette".to_owned(),
            OverlayMode::Replay => "\u{2190} Session Replay  (Esc = back)".to_owned(),
            OverlayMode::Connections => "\u{2190} Connections  (Esc = back)".to_owned(),
            OverlayMode::ConnectionForm => self.connection_form.title(),
            OverlayMode::SessionAttach => "\u{2190} Manage Sessions  (Esc = back)".to_owned(),
            OverlayMode::OpenWith => "\u{2190} Open With\u{2026}  (Esc = back)".to_owned(),
            OverlayMode::WorkspacePicker => {
                "\u{2190} Move to Workspace\u{2026}  (Esc = back)".to_owned()
            }
            OverlayMode::ImageView => {
                format!("\u{2190} {}  (Esc = close)", self.image_view_caption)
            }
            // No title bar — early-dispatched to `apply_context_menu`.
            OverlayMode::ContextMenu => String::new(),
            OverlayMode::ConfirmClose => "Close?".to_owned(),
            OverlayMode::AttachChoice => "Attach session".to_owned(),
            OverlayMode::ConfirmKillSession => "Kill session".to_owned(),
            OverlayMode::DetachSwitchChoice => "Detach & switch".to_owned(),
            OverlayMode::ConfirmReplaceTab => "Replace tab?".to_owned(),
            OverlayMode::ConfirmRemoveHost => "Remove host?".to_owned(),
            OverlayMode::ConfirmOverwriteLayout => "Layout exists".to_owned(),
            OverlayMode::ConfirmOpenLayout => "Open layout".to_owned(),
        }
    }

    /// Whether the open centered overlay has hidden body rows above / below the
    /// visible window, for the shared scroll affordance (OVERLAY-SMALL-WINDOW).
    /// `(false, false)` whenever the body fits, so a normal window draws no
    /// arrows and stays byte-identical. The context menu draws its own arrows
    /// (it is not a centered panel), so it returns `(false, false)` here. Each
    /// list-shaped overlay owns its windowing math and exposes a
    /// `scroll_indicator`; this just dispatches to the active mode's.
    pub(in crate::native) fn scroll_arrows(&self, body_height: usize) -> (bool, bool) {
        match self.mode {
            OverlayMode::Settings => self.panel.scroll_indicator(body_height),
            OverlayMode::ThemePicker => self.theme_picker.scroll_indicator(body_height),
            OverlayMode::FontPicker => self.font_picker.scroll_indicator(body_height),
            OverlayMode::KeyBindings => self.key_remap.scroll_indicator(body_height),
            OverlayMode::Connections => self.connections.scroll_indicator(body_height),
            OverlayMode::SessionAttach => self.session_attach.scroll_indicator(body_height),
            OverlayMode::OpenWith => self.open_with.scroll_indicator(body_height),
            OverlayMode::WorkspacePicker => self.workspace_picker.scroll_indicator(body_height),
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
            | OverlayMode::DetachSwitchChoice
            | OverlayMode::ConfirmReplaceTab
            | OverlayMode::ConfirmRemoveHost
            | OverlayMode::ConfirmOverwriteLayout
            | OverlayMode::ConfirmOpenLayout => (false, false),
            OverlayMode::ConnectionForm => (false, false),
        }
    }

    pub(in crate::native) fn render_signature(&self) -> OverlayRenderSignature {
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
            connection_form: self.connection_form.render_signature(),
            session_attach: self.session_attach.render_signature(),
            open_with: self.open_with.render_signature(),
            workspace_picker: self.workspace_picker.render_signature(),
        }
    }
}

pub(in crate::native) fn apply_overlay(snapshot: &mut Snapshot, overlay: &mut OverlayUi) {
    let Some(rect) = overlay_rect(
        overlay,
        snapshot.dimensions.columns,
        snapshot.dimensions.rows,
    ) else {
        return;
    };
    // The context menu has its own no-title layout (IN2); dispatch and return.
    if overlay.mode == OverlayMode::ContextMenu {
        // MENU-OVER-MANAGER: a connection-row menu is spawned from WITHIN the
        // connection manager, which stays loaded underneath (the mode is
        // ContextMenu, but `picker_return` restores Connections on dismiss).
        // The overlay system draws only the active mode, so without this the
        // manager vanishes and the menu floats on a blank screen. Paint the
        // manager panel first (temporarily viewing the overlay AS Connections so
        // `overlay_rect` / `visible_lines` / `title` resolve the manager), then
        // let the opaque menu box composite over it.
        if matches!(
            overlay.context_menu.surface(),
            crate::native::context_menu_ui::ContextMenuSurface::ConnectionRow(_)
        ) {
            let restore = overlay.mode;
            overlay.mode = OverlayMode::Connections;
            if let Some(mgr_rect) = overlay_rect(
                overlay,
                snapshot.dimensions.columns,
                snapshot.dimensions.rows,
            ) {
                apply_panel(snapshot, overlay, mgr_rect);
            }
            overlay.mode = restore;
        }
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
    apply_panel(snapshot, overlay, rect);
}

/// Paint the generic bordered overlay panel (fill + border + title + the mode's
/// visible body lines + scroll affordances) at `rect`. Extracted from
/// [`apply_overlay`] so the connection-row context menu can render the still-open
/// connection manager UNDERNEATH itself (MENU-OVER-MANAGER) before the menu box
/// composites over it — the overlay system only draws the active mode, so the
/// manager would otherwise vanish the instant its row menu opened.
pub(super) fn apply_panel(snapshot: &mut Snapshot, overlay: &mut OverlayUi, rect: OverlayRect) {
    let rows = snapshot.dimensions.rows;
    // Single source of truth for the title text (see `OverlayUi::title`). The
    // leading `\u{2190}` on a mode's title is what the back-arrow hit-test keys
    // off, so both the painter and the hit-test read the same string.
    let title = overlay.title();

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
pub(super) fn apply_image_view_caption(snapshot: &mut Snapshot, overlay: &OverlayUi) {
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
pub(super) fn apply_context_menu(snapshot: &mut Snapshot, overlay: &OverlayUi, rect: OverlayRect) {
    use crate::native::context_menu_ui::ContextMenuRow;

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
    pub(super) fn visible_lines(&self, body_width: usize, body_height: usize) -> Vec<OverlayLine> {
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
            OverlayMode::ConnectionForm => self
                .connection_form
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
            OverlayMode::WorkspacePicker => self
                .workspace_picker
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
            // Static Detach & switch copy. Row 0 names the cwd, row 1
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
            // Static replace-tab confirm copy (ODP-5D). Row 0 names the host and
            // the running-shell hazard, row 1 blank, row 2 the action line — the
            // action row index (2) matches `ACTION_ROW` in
            // `confirm_replace_tab_click`. The host alias is OdyTTY-owned config
            // text; it is truncated to the body width and display-only here.
            OverlayMode::ConfirmReplaceTab => {
                let prompt = match self.confirm_replace_tab.as_ref() {
                    Some((host, _)) => {
                        format!(
                            "A program is running here — replace it with {}?",
                            host.alias
                        )
                    }
                    None => "A program is running in this tab.".to_owned(),
                };
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
                        text: CONFIRM_REPLACE_TAB_ACTION_LINE.to_owned(),
                        focused: true,
                        swatch: None,
                        bold: false,
                    },
                ]
            }
            // Static remove-host confirm copy (ODP-2C). Row 0 names the host
            // being deleted, row 1 blank, row 2 the action line — the action row
            // index (2) matches `ACTION_ROW` in `confirm_remove_host_click`. The
            // host alias is OdyTTY-owned config text; truncated to the body width
            // and display-only here.
            OverlayMode::ConfirmRemoveHost => {
                let prompt = match self.confirm_remove_host.as_ref() {
                    Some(host) => format!("Remove \u{201c}{}\u{201d} from hosts.conf?", host.alias),
                    None => "Remove this host from hosts.conf?".to_owned(),
                };
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
                        text: CONFIRM_REMOVE_HOST_ACTION_LINE.to_owned(),
                        focused: true,
                        swatch: None,
                        bold: false,
                    },
                ]
            }
            // Static overwrite-layout confirm copy (OVERWRITE-WARN). Row 0 names
            // the colliding layout, row 1 blank, row 2 the three-way action line
            // — the action row index (2) matches `ACTION_ROW` in
            // `confirm_overwrite_layout_click`. The layout name is user-entered
            // text; truncated to the body width and display-only here.
            OverlayMode::ConfirmOverwriteLayout => {
                let prompt = match self.confirm_overwrite_layout.as_ref() {
                    Some((name, _)) => {
                        format!("Layout \u{201c}{name}\u{201d} already exists.")
                    }
                    None => "A layout with that name already exists.".to_owned(),
                };
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
                        text: CONFIRM_OVERWRITE_LAYOUT_ACTION_LINE.to_owned(),
                        focused: true,
                        swatch: None,
                        bold: false,
                    },
                ]
            }
            // Static open-layout mode copy (LAYOUT-OPEN-MODE). Row 0 names the
            // layout being opened, row 1 blank, row 2 the three-way action line
            // — the action row index (2) matches `ACTION_ROW` in
            // `confirm_open_layout_click`. The layout name is user-entered text;
            // truncated to the body width and display-only here.
            OverlayMode::ConfirmOpenLayout => {
                let prompt = match self.confirm_open_layout.as_ref() {
                    Some(name) => format!("Open layout \u{201c}{name}\u{201d} onto this window?"),
                    None => "Open this layout onto the current window?".to_owned(),
                };
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
                        text: CONFIRM_OPEN_LAYOUT_ACTION_LINE.to_owned(),
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
pub(super) struct OverlayLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) swatch: Option<Srgb>,
    /// Whether to render this line in bold weight. Set for primary setting
    /// name/value rows; unset for group headers, help text, and notices.
    pub(super) bold: bool,
}

impl From<crate::native::settings_panel::SettingsPanelLine> for OverlayLine {
    fn from(line: crate::native::settings_panel::SettingsPanelLine) -> Self {
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

impl From<ConnectionFormLine> for OverlayLine {
    fn from(line: ConnectionFormLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: line.swatch,
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

impl From<WorkspacePickerLine> for OverlayLine {
    fn from(line: WorkspacePickerLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
        }
    }
}

pub(super) fn fill_rect(
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

pub(super) fn draw_border(
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
pub(super) fn text_display_width(text: &str) -> usize {
    text.chars()
        .filter(|ch| !ch.is_control())
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(1).max(1))
        .sum()
}

/// Hard character-truncate `text` to at most `max_width` display cells (the
/// `write_text` clip rule), used as the last-resort fallback when not even one
/// word of a hint fits.
pub(super) fn fit_chars(text: &str, max_width: usize) -> String {
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
pub(in crate::native) fn fit_hint_to_width(text: &str, max_width: usize) -> String {
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

pub(super) fn write_text(
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

pub(super) fn draw_swatch(snapshot: &mut Snapshot, row: usize, column: usize, color: Srgb) {
    if row >= snapshot.dimensions.rows || column + 1 >= snapshot.dimensions.columns {
        return;
    }
    let mut attrs = Attrs::default();
    attrs.background = Color::Rgb(color.0, color.1, color.2);
    write_cell(snapshot, row, column, ' ', attrs);
    write_cell(snapshot, row, column + 1, ' ', attrs);
}

pub(super) fn write_cell(
    snapshot: &mut Snapshot,
    row: usize,
    column: usize,
    ch: char,
    attrs: Attrs,
) {
    let offset = row * snapshot.dimensions.columns + column;
    snapshot.cells[offset] = Cell::new(ch, attrs);
}

pub(super) fn panel_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Default;
    attrs.background = Color::Default;
    attrs.set_inverse(true);
    attrs
}

/// Bold variant of `panel_attrs` for primary setting name/value rows.
pub(super) fn bold_panel_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.set_bold(true);
    attrs
}

pub(super) fn border_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.foreground = Color::Indexed(14);
    attrs
}

pub(super) fn title_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.foreground = Color::Indexed(15);
    attrs
}

/// Caption attrs for the Phase 13c image-viewer LIGHTBOX: a clean bold
/// bright-white caption on the DEFAULT background (no inverse-video bar, no
/// panel chrome). The GPU scrim dims the whole terminal including this text, so
/// bright white reads as legible light gray over the dimmed surround.
pub(super) fn image_caption_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(15);
    attrs.background = Color::Default;
    attrs.set_bold(true);
    attrs
}

pub(super) fn focused_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(0);
    attrs.background = Color::Indexed(11);
    attrs
}

/// Attrs for a disabled context-menu item (IN2): the panel fill with a muted
/// (bright-black) foreground so the label reads as unavailable.
pub(super) fn dim_attrs() -> Attrs {
    let mut attrs = panel_attrs();
    attrs.foreground = Color::Indexed(8);
    attrs
}
