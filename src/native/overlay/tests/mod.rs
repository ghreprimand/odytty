// SPDX-License-Identifier: GPL-3.0-only
//! Overlay coordinator tests, grouped by responsibility.
//!
//! Shared fixtures live here; the per-responsibility suites are split into
//! state, input, and render modules.

use crate::connection_hosts::ConnectionHost;
use crate::core::{Attrs, Cell, Snapshot};
use crate::input::Modifiers;
use crate::native::connection_overlay::ConnectionPickerPurpose;
use crate::native::session::SessionToken;
use crate::native::settings_panel::SettingsLevel;
use crate::selection::CellPoint;
use crate::session_host::ListedSession;
use crate::settings::Settings;
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::contracts::*;
use super::input::*;
use super::layout::*;
use super::render::*;
use super::state::*;
use crate::core::{Dimensions, Position};

// --- C22/C23: a failed save must disarm the close-after-save latches ---

mod input;
mod render;
mod state;

fn snapshot(columns: usize, rows: usize) -> Snapshot {
    Snapshot {
        dimensions: Dimensions::new(columns, rows),
        cursor: Position::default(),
        cursor_visible: true,
        colors: crate::core::DynamicColors::default(),
        cells: vec![Cell::new('.', Attrs::default()); columns * rows],
    }
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
        integration: None,
        reuse: None,
        tmux: None,
        protocol: None,
        identity_file: None,
        persist: None,
        source: crate::connection_hosts::ConnectionHostSource::Odytty,
    }
}

/// A synthetic connection host with an explicit source, for the ODP-2C
/// connection-row menu gating tests.
fn connection_host_sourced(
    alias: &str,
    source: crate::connection_hosts::ConnectionHostSource,
) -> ConnectionHost {
    ConnectionHost {
        source,
        ..connection_host(alias)
    }
}

// ── ODP-2C connection-row right-click menu (menu-over-overlay) ──────────

/// Right-click the first saved-host row of an open connection manager,
/// returning the resulting outcome. The manager must have >=1 host; row 0 is
/// the query prompt, so the first host sits at body row 1.
fn right_click_first_host(overlay: &mut OverlayUi) -> OverlayOutcome {
    let rect = overlay_rect(overlay, 80, 24).expect("connection rect");
    // Prime the render-derived body window so host_at_row resolves.
    let _ = overlay
        .connections
        .visible_lines(rect.body_width, rect.body_height);
    overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top + 1,
                column: rect.body_left + 1,
            },
            button: PointerButton::Right,
            x_in_body: None,
        },
        rect,
    )
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

// --- UX4-P1: pointer entry (handle_pointer / overlay_rect) ---

fn theme_value_cell(rect: OverlayRect) -> CellPoint {
    // Row 0 of the body is the first group header ("Theme"); row 1 is the
    // theme value line. SETTINGS-CLICKZONES: the compact row splits into a
    // focus-only NAME zone and an action VALUE zone; the "Theme" name is 5
    // columns, so the value zone begins at body column 9 — click inside it.
    CellPoint {
        row: rect.body_top + 1,
        column: rect.body_left + 12,
    }
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
