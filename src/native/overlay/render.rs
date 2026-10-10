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
use crate::native::profile_picker::ProfilePickerLine;
use crate::native::replay_overlay::ReplayOverlayLine;
use crate::native::session_attach_overlay::SessionAttachOverlayLine;
use crate::native::theme_builder::ThemeBuilderLine;
use crate::native::theme_picker::ThemePickerLine;
use crate::native::workspace_picker::WorkspacePickerLine;
use crate::theme::Srgb;

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
            OverlayMode::ProfileManager => self.profile_manager.title(),
            OverlayMode::FontPicker => "\u{2190} OdyTTY Font Picker  (Esc = back)".to_owned(),
            OverlayMode::KeyBindings => "\u{2190} OdyTTY Key Bindings  (Esc = back)".to_owned(),
            OverlayMode::Onboarding => "Welcome to OdyTTY".to_owned(),
            OverlayMode::CommandPalette => "Command Palette".to_owned(),
            OverlayMode::Replay => "\u{2190} Session Replay  (Esc = back)".to_owned(),
            OverlayMode::Connections => "\u{2190} Connections  (Esc = back)".to_owned(),
            OverlayMode::ConnectionForm => self.connection_form.title(),
            // The shortcut legend moved into the body as wrapped rows
            // (`session_attach` `visible_lines`) so every shortcut stays visible
            // at narrow widths instead of clipping off the single title line.
            OverlayMode::SessionAttach => "\u{2190} Session Navigator  (Esc = back)".to_owned(),
            OverlayMode::OpenWith => "\u{2190} Open With\u{2026}  (Esc = back)".to_owned(),
            OverlayMode::WorkspacePicker => {
                "\u{2190} Move to Workspace\u{2026}  (Esc = back)".to_owned()
            }
            OverlayMode::ProfilePicker => {
                format!("\u{2190} {}  (Esc = back)", self.profile_picker.title())
            }
            OverlayMode::ImageView => {
                format!("\u{2190} {}  (Esc = close)", self.image_view_caption)
            }
            // No title bar — early-dispatched to `apply_context_menu`.
            OverlayMode::ContextMenu => String::new(),
            OverlayMode::ConfirmClose => "Close?".to_owned(),
            OverlayMode::RiskyPaste if self.risky_paste.broadcast.is_some() => {
                "Confirm broadcast paste".to_owned()
            }
            OverlayMode::RiskyPaste => "Confirm paste".to_owned(),
            OverlayMode::AttachChoice => "Attach session".to_owned(),
            OverlayMode::ConfirmKillSession => "Kill session".to_owned(),
            OverlayMode::ConfirmNavigatorClose => "Close navigator item?".to_owned(),
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
            OverlayMode::ProfilePicker => self.profile_picker.scroll_indicator(body_height),
            OverlayMode::CommandPalette => self.command_palette.scroll_indicator(body_height),
            OverlayMode::ThemeBuilder => self.theme_builder.scroll_indicator(body_height),
            OverlayMode::ProfileManager => self.profile_manager.scroll_indicator(body_height),
            // Replay (read-only frame preview whose scroll axis is time, not a
            // list) keeps a different body model; it draws no list affordance
            // here. Its scrubbing and the static Onboarding/close cards have
            // nothing to window.
            OverlayMode::Replay
            | OverlayMode::Onboarding
            | OverlayMode::ContextMenu
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
            | OverlayMode::ConfirmOpenLayout => (false, false),
            OverlayMode::ConnectionForm => (false, false),
        }
    }

    /// The still-loaded overlay mode painted UNDERNEATH an open menu-over-overlay
    /// context menu, or `None` for a plain context menu or a non-menu overlay. A
    /// connection-row menu keeps the connection manager loaded beneath it; a
    /// navigator-row menu keeps the session navigator loaded beneath it.
    /// Both the cell painter ([`apply_overlay`]) and the multi-pane composite-rect
    /// crop ([`overlay_composite_rect`]) read this single source of truth so the
    /// underlay survives in BOTH render paths - the single-pane path paints onto
    /// the full terminal snapshot, but the multi-pane path crops to the overlay
    /// rect and would otherwise discard every underlay cell outside the small menu
    /// box (the disappearing-navigator defect).
    pub(in crate::native) fn menu_underlay_mode(&self) -> Option<OverlayMode> {
        if self.mode != OverlayMode::ContextMenu {
            return None;
        }
        match self.context_menu.surface() {
            crate::native::context_menu_ui::ContextMenuSurface::ConnectionRow(_) => {
                Some(OverlayMode::Connections)
            }
            crate::native::context_menu_ui::ContextMenuSurface::NavigatorRow => {
                Some(OverlayMode::SessionAttach)
            }
            _ => None,
        }
    }

    pub(in crate::native) fn render_signature(&self) -> OverlayRenderSignature {
        OverlayRenderSignature {
            open: self.open,
            mode: self.mode,
            panel: self.panel.render_signature(),
            theme_picker: self.theme_picker.render_signature(),
            theme_builder: self.theme_builder.render_signature(),
            profile_manager: self.profile_manager.render_signature(),
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
            profile_picker: self.profile_picker.render_signature(),
            dialog_payload: self.dialog_payload_fingerprint(),
        }
    }

    /// See [`OverlayRenderSignature::dialog_payload`]. Hashes the debug form of
    /// the active dialog's carried state, which holds everything its body
    /// prints; the payloads are small and bounded (the paste preview is at
    /// most `MAX_ESCAPED_PREVIEW_BYTES`).
    fn dialog_payload_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let payload = match self.mode {
            OverlayMode::RiskyPaste => format!("{:?}", self.risky_paste),
            OverlayMode::ImageView => format!("{:?}", self.image_view_caption),
            OverlayMode::AttachChoice => format!("{:?}", self.attach_choice_session_id),
            OverlayMode::ConfirmKillSession => format!("{:?}", self.confirm_kill_session_id),
            OverlayMode::ConfirmNavigatorClose => format!("{:?}", self.confirm_navigator_close),
            OverlayMode::DetachSwitchChoice => format!("{:?}", self.detach_switch_cwd),
            OverlayMode::ConfirmReplaceTab => format!("{:?}", self.confirm_replace_tab),
            OverlayMode::ConfirmRemoveHost => format!("{:?}", self.confirm_remove_host),
            OverlayMode::ConfirmOverwriteLayout => format!("{:?}", self.confirm_overwrite_layout),
            OverlayMode::ConfirmOpenLayout => format!("{:?}", self.confirm_open_layout),
            _ => return 0,
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        payload.hash(&mut hasher);
        hasher.finish()
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
        // A connection-row menu paints the connection manager underneath; a
        // navigator-row menu paints the session navigator underneath -
        // both are menu-over-overlay surfaces whose underlying panel would
        // otherwise vanish (the overlay system draws only the active mode).
        if let Some(underlay) = overlay.menu_underlay_mode() {
            let restore = overlay.mode;
            overlay.mode = underlay;
            if let Some(under_rect) = overlay_rect(
                overlay,
                snapshot.dimensions.columns,
                snapshot.dimensions.rows,
            ) {
                apply_panel(snapshot, overlay, under_rect);
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
    // The replay preview's recorded rows paint the owners and spans the
    // frame held rather than resegmenting their text (other modes: empty).
    let recorded = if overlay.mode == OverlayMode::Replay {
        overlay
            .replay
            .visible_recorded_owners(body_width, rect.body_height)
    } else {
        Vec::new()
    };
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
        if let Some(Some(owners)) = recorded.get(row_index) {
            write_owners(snapshot, y, text_column, text_width, owners.clone(), attrs);
        } else {
            write_text(snapshot, y, text_column, text_width, &row.text, attrs);
        }
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
    use crate::native::context_menu_ui::{ContextMenuRow, ContextMenuUi};

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
    let arrow_col = ContextMenuUi::overflow_arrow_column(&rect);
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
            OverlayMode::ProfileManager => self
                .profile_manager
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
            OverlayMode::ProfilePicker => self
                .profile_picker
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
            OverlayMode::RiskyPaste => {
                let (mut chunks, overflow) =
                    chunk_by_columns(&self.risky_paste.escaped_preview, body_width.max(1), 3);
                chunks.resize(3, String::new());
                let was_truncated = self.risky_paste.preview_truncated || overflow;
                let detail = if let Some(summary) = self.risky_paste.broadcast {
                    summary.confirm_line()
                } else if self.risky_paste.one_line_available {
                    if was_truncated {
                        "Preview truncated. One Line escapes CR/LF and backslashes.".to_owned()
                    } else {
                        "One Line escapes CR/LF and doubles existing backslashes.".to_owned()
                    }
                } else if was_truncated {
                    format!(
                        "Preview truncated (bounded to {} escaped bytes).",
                        crate::native::paste_policy::MAX_ESCAPED_PREVIEW_BYTES
                    )
                } else {
                    String::new()
                };
                let action = if self.risky_paste.one_line_available {
                    RISKY_PASTE_ACTION_LINE
                } else {
                    RISKY_PASTE_ACTION_LINE_NO_ONE_LINE
                };
                vec![
                    OverlayLine {
                        text: format!(
                            "Original: {} lines, {} bytes",
                            self.risky_paste.line_count, self.risky_paste.byte_count
                        ),
                        focused: false,
                        swatch: None,
                        bold: false,
                    },
                    OverlayLine {
                        text: "Escaped preview (not shell output):".to_owned(),
                        focused: false,
                        swatch: None,
                        bold: false,
                    },
                    OverlayLine {
                        text: chunks[0].clone(),
                        focused: false,
                        swatch: None,
                        bold: false,
                    },
                    OverlayLine {
                        text: chunks[1].clone(),
                        focused: false,
                        swatch: None,
                        bold: false,
                    },
                    OverlayLine {
                        text: chunks[2].clone(),
                        focused: false,
                        swatch: None,
                        bold: false,
                    },
                    OverlayLine {
                        text: detail,
                        focused: false,
                        swatch: None,
                        bold: false,
                    },
                    OverlayLine {
                        text: action.to_owned(),
                        focused: true,
                        swatch: None,
                        bold: false,
                    },
                ]
            }
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
            // The prompt names the actual close scope so the confirmation is
            // honest about what a pane close reaps versus a tab or workspace
            // close: a pane close keeps sibling panes, a tab close reaps every
            // pane in the tab, a workspace close reaps every tab.
            OverlayMode::ConfirmNavigatorClose => {
                use crate::native::session_navigator::NavigatorTarget;
                let prompt = match self.confirm_navigator_close {
                    Some(NavigatorTarget::Live(_)) => "Close this pane? Sibling panes stay open.",
                    Some(NavigatorTarget::Tab(_)) => "Close this tab and all its panes?",
                    Some(NavigatorTarget::Workspace(_)) => "Close this workspace and all its tabs?",
                    Some(NavigatorTarget::Detached(_)) => "Close this detached session?",
                    None => "Close this live tab or workspace?",
                };
                vec![
                    OverlayLine {
                        text: prompt.to_owned(),
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
                        text: CONFIRM_NAVIGATOR_CLOSE_ACTION_LINE.to_owned(),
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

impl From<crate::native::profile_manager::ProfileManagerLine> for OverlayLine {
    fn from(line: crate::native::profile_manager::ProfileManagerLine) -> Self {
        Self {
            text: line.text,
            focused: line.focused,
            swatch: None,
            bold: line.bold,
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

impl From<ProfilePickerLine> for OverlayLine {
    fn from(line: ProfilePickerLine) -> Self {
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

/// The overlay layout metric: the terminal owners of `text` (see
/// [`crate::core::text_owners`]), each with the cells it takes. A combining
/// mark, joiner, variation selector, emoji modifier or script extension
/// belongs to its owner exactly as in the terminal grid, so an emoji ZWJ
/// sequence or a script cluster is measured, cut and painted as one glyph.
///
/// Chrome policy for controls: they are removed before segmentation, so a
/// control never separates owners here. The cuts below return strings that
/// carry no control, and repainting such a string must give the owners that
/// were measured; a control that still split owners at measure time would be
/// gone by paint time and the owners would join. Every measure, cut and
/// paint below uses this, so layout and paint always agree.
fn glyph_widths(text: &str) -> Vec<(Cell, usize)> {
    if text.chars().any(char::is_control) {
        let stripped: String = text.chars().filter(|ch| !ch.is_control()).collect();
        crate::core::text_owners(&stripped, false)
    } else {
        crate::core::text_owners(text, false)
    }
}

/// Display width (in terminal cells) of text, using the same owner widths as
/// [`write_text`].
pub(in crate::native) fn text_display_width(text: &str) -> usize {
    glyph_widths(text).iter().map(|(_, width)| width).sum()
}

/// Split `text` into at most `max_lines` lines of at most `width` display
/// cells each, by the per-glyph width [`write_text`] uses, so no line is
/// clipped by the painter. Returns the lines and whether text was left over.
pub(in crate::native) fn chunk_by_columns(
    text: &str,
    width: usize,
    max_lines: usize,
) -> (Vec<String>, bool) {
    if max_lines == 0 || width == 0 {
        return (Vec::new(), !text.is_empty());
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0usize;
    for (owner, w) in glyph_widths(text) {
        if used + w > width && !line.is_empty() {
            if lines.len() + 1 == max_lines {
                lines.push(line);
                return (lines, true);
            }
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if w > width {
            // A glyph wider than the whole line cannot be shown.
            return (lines, true);
        }
        line.push_str(&owner.grapheme());
        used += w;
    }
    if !line.is_empty() && lines.len() < max_lines {
        lines.push(line);
    }
    (lines, false)
}

/// Hard character-truncate `text` to at most `max_width` display cells (the
/// `write_text` clip rule), used as the last-resort fallback when not even one
/// word of a hint fits.
pub(in crate::native) fn fit_chars(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for (owner, w) in glyph_widths(text) {
        if width + w > max_width {
            break;
        }
        out.push_str(&owner.grapheme());
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

/// Word-wrap a shortcut legend given as discrete `segments` joined by `sep`
/// across as many rows as `width` display cells needs, packing greedily on
/// segment boundaries so a shortcut is never split mid-token. A single segment
/// wider than the whole body is emitted on its own row (the caller truncates it)
/// rather than dropped. Returns an empty vector when `width` is zero. Shared by
/// overlays that must keep every shortcut discoverable at narrow widths.
pub(in crate::native) fn wrap_segments(segments: &[&str], sep: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let sep_w = text_display_width(sep);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for seg in segments {
        let seg_w = text_display_width(seg);
        if current.is_empty() {
            current.push_str(seg);
            used = seg_w;
        } else if used + sep_w + seg_w <= width {
            current.push_str(sep);
            current.push_str(seg);
            used += sep_w + seg_w;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(seg);
            used = seg_w;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Paint `text` at `row`, `column` within `max_width` cells by terminal
/// owners ([`glyph_widths`]): a wide owner takes a real wide tail, an owner
/// that does not fit before the right edge is left out whole, and controls
/// are dropped. The shared painter for overlay rows, the search bar and the
/// rename prompt.
pub(in crate::native) fn write_text(
    snapshot: &mut Snapshot,
    row: usize,
    column: usize,
    max_width: usize,
    text: &str,
    attrs: Attrs,
) {
    write_owners(snapshot, row, column, max_width, glyph_widths(text), attrs);
}

/// Paint already-segmented owners at `row`, `column` within `max_width`
/// cells: each owner's cell and the columns it takes (1 or 2), as
/// [`write_text`] paints them. The replay preview passes the owners and
/// spans a recorded frame held, so a frame recorded with ambiguous-wide text
/// keeps its two-cell owners.
pub(in crate::native) fn write_owners(
    snapshot: &mut Snapshot,
    row: usize,
    column: usize,
    max_width: usize,
    owners: Vec<(Cell, usize)>,
    attrs: Attrs,
) {
    if row >= snapshot.dimensions.rows || column >= snapshot.dimensions.columns || max_width == 0 {
        return;
    }
    let mut x = column;
    let right = (column + max_width).min(snapshot.dimensions.columns);
    for (mut owner, width) in owners {
        if x + width > right {
            break;
        }
        owner.attrs = attrs;
        snapshot.cells[row * snapshot.dimensions.columns + x] = owner;
        if width == 2 {
            // A real wide tail, so the color-glyph path sizes an emoji to
            // both cells and the monochrome path skips the spacer.
            write_cell(snapshot, row, x + 1, ' ', attrs);
            snapshot.cells[row * snapshot.dimensions.columns + x + 1].wide_continuation = true;
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
