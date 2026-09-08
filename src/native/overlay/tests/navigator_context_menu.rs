// SPDX-License-Identifier: GPL-3.0-only
//! Navigator row right-click menu regressions.
//!
//! Contract (menu-over-navigator like ConnectionRow):
//! - Right-click selects a stable `NavigatorTarget` WITHOUT moving selection and
//!   WITHOUT focusing/attaching. Left-click focus/attach stays byte-identical.
//! - Menu opens over the still-loaded navigator; Esc / outside click dismisses
//!   ONLY the menu, returns to `SessionAttach`, retains filter/scroll/selection,
//!   and does NOT activate the underlying row.
//! - Off-row right-click (prompt, empty, past end) is inert.
//! - Applicable actions by class:
//!   - Workspace / Tab / Live: Focus, Rename, Duplicate, Move, Close
//!   - Detached: Attach (enabled only when available), Close (kill)
//! - Activation: Focus -> `FocusSession`; Attach -> `AttachSession`;
//!   Rename/Duplicate/Move -> `NavigatorAction`; Close live ->
//!   `NavigatorCloseRequest`; Close detached -> `KillSessionRequest`.
//!
//! Render/compositing: `apply_overlay` must paint the navigator underlay, and
//! the production multi-pane crop (`build_overlay_top`) must retain that underlay
//! extent (not menu-box-only). See sibling tests in `render.rs`.

use super::*;
use crate::native::context_menu_ui::ContextMenuRow;
use crate::native::session::SessionToken;
use crate::native::session_navigator::{NavigatorAction, NavigatorEntry, NavigatorTarget};
use crate::session_host::ListedSession;

fn live_entry(token: u64, name: &str) -> NavigatorEntry {
    NavigatorEntry {
        target: NavigatorTarget::Live(SessionToken(token)),
        stable_id: format!("live:{token}"),
        name: name.to_owned(),
        detail: "local /tmp".to_owned(),
        status: "running".to_owned(),
        unread: false,
        profile: None,
        preview: Vec::new(),
    }
}

fn tab_entry(token: u64, name: &str) -> NavigatorEntry {
    NavigatorEntry {
        target: NavigatorTarget::Tab(SessionToken(token)),
        stable_id: format!("tab:{token}"),
        name: name.to_owned(),
        detail: "tab 2 panes".to_owned(),
        status: "idle".to_owned(),
        unread: false,
        profile: None,
        preview: Vec::new(),
    }
}

fn workspace_entry(token: u64, name: &str) -> NavigatorEntry {
    NavigatorEntry {
        target: NavigatorTarget::Workspace(SessionToken(token)),
        stable_id: format!("workspace:{token}"),
        name: name.to_owned(),
        detail: "workspace 1 tabs".to_owned(),
        status: "idle".to_owned(),
        unread: false,
        profile: None,
        preview: Vec::new(),
    }
}

fn detached_entry(id: &str, name: &str, state: &'static str, pane_count: usize) -> NavigatorEntry {
    NavigatorEntry::from(ListedSession {
        id: id.to_owned(),
        name: name.to_owned(),
        state,
        age_ms: 1,
        pane_count,
    })
}

fn mixed_catalog() -> Vec<NavigatorEntry> {
    vec![
        workspace_entry(10, "ws-main"),
        tab_entry(11, "tab-build"),
        live_entry(12, "pane-a"),
        live_entry(13, "pane-b"),
        detached_entry("s-detach", "detached-web", "running", 1),
    ]
}

fn open_navigator(entries: Vec<NavigatorEntry>, stable_id: Option<&str>) -> OverlayUi {
    let mut overlay = OverlayUi::default();
    overlay.open_session_navigator_selected(entries, stable_id);
    overlay
}

fn navigator_row_for(overlay: &OverlayUi, needle: &str, columns: usize, rows: usize) -> usize {
    let rect = overlay_rect(overlay, columns, rows).expect("navigator rect");
    let lines = overlay
        .session_attach
        .visible_lines(rect.body_width, rect.body_height);
    lines
        .iter()
        .enumerate()
        .find_map(|(row, line)| (row > 0 && line.text.contains(needle)).then_some(row))
        .unwrap_or_else(|| panic!("missing rendered navigator row containing {needle:?}"))
}

fn right_click_navigator_row(
    overlay: &mut OverlayUi,
    needle: &str,
    columns: usize,
    rows: usize,
) -> OverlayOutcome {
    let rect = overlay_rect(overlay, columns, rows).expect("navigator rect");
    let row_in_body = navigator_row_for(overlay, needle, columns, rows);
    overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top + row_in_body,
                column: rect.body_left + 1,
            },
            button: PointerButton::Right,
            x_in_body: None,
        },
        rect,
    )
}

fn left_click_navigator_row(
    overlay: &mut OverlayUi,
    needle: &str,
    columns: usize,
    rows: usize,
) -> OverlayOutcome {
    let rect = overlay_rect(overlay, columns, rows).expect("navigator rect");
    let row_in_body = navigator_row_for(overlay, needle, columns, rows);
    overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top + row_in_body,
                column: rect.body_left + 1,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    )
}

fn assert_menu_over_navigator(overlay: &OverlayUi, after: &str) {
    assert!(
        overlay.is_open(),
        "{after}: navigator must stay loaded under the menu"
    );
    assert!(
        overlay.is_context_menu(),
        "{after}: right-click must open a context menu (got mode {:?})",
        overlay.render_signature().mode
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::ContextMenu,
        "{after}: mode must be ContextMenu"
    );
}

fn menu_item_labels(overlay: &OverlayUi) -> Vec<&'static str> {
    overlay
        .context_menu
        .rows()
        .into_iter()
        .filter_map(|row| match row {
            ContextMenuRow::Item { label, .. } => Some(label),
            ContextMenuRow::Separator => None,
        })
        .collect()
}

fn menu_item_enabled(overlay: &OverlayUi, want: &str) -> Option<bool> {
    overlay
        .context_menu
        .rows()
        .into_iter()
        .find_map(|row| match row {
            ContextMenuRow::Item { label, enabled, .. } if label == want => Some(enabled),
            _ => None,
        })
}

const LIVE_ACTIONS: &[&str] = &["Focus", "Rename", "Duplicate", "Move", "Close"];
const DETACHED_ACTIONS: &[&str] = &["Attach", "Close"];

#[test]
fn navigator_left_click_live_still_focuses() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:12"));
    let outcome = left_click_navigator_row(&mut overlay, "pane-b", 80, 24);
    assert_eq!(
        outcome,
        OverlayOutcome::FocusSession(SessionToken(13)),
        "left-click Live must keep focusing (unchanged by the right-click menu)"
    );
}

#[test]
fn navigator_right_click_live_opens_menu_without_focus_or_attach() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:12"));
    let selected_before = overlay.session_attach.render_signature().selected;
    let outcome = right_click_navigator_row(&mut overlay, "pane-b", 80, 24);
    assert_eq!(
        outcome,
        OverlayOutcome::Consumed,
        "menu-over-navigator must not emit Focus/Attach/Kill on open"
    );
    assert_menu_over_navigator(&overlay, "Live row right-click");
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before,
        "right-click must not move the navigator selection cursor"
    );
    assert_eq!(menu_item_labels(&overlay), LIVE_ACTIONS);
}

#[test]
fn navigator_right_click_tab_opens_menu_without_selection_change() {
    let mut overlay = open_navigator(mixed_catalog(), Some("tab:11"));
    let selected_before = overlay.session_attach.render_signature().selected;
    let outcome = right_click_navigator_row(&mut overlay, "tab-build", 80, 24);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_menu_over_navigator(&overlay, "Tab row right-click");
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before
    );
    assert_eq!(menu_item_labels(&overlay), LIVE_ACTIONS);
}

#[test]
fn navigator_right_click_workspace_opens_menu_without_selection_change() {
    let mut overlay = open_navigator(mixed_catalog(), Some("workspace:10"));
    let selected_before = overlay.session_attach.render_signature().selected;
    let outcome = right_click_navigator_row(&mut overlay, "ws-main", 80, 24);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_menu_over_navigator(&overlay, "Workspace row right-click");
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before
    );
    assert_eq!(menu_item_labels(&overlay), LIVE_ACTIONS);
}

#[test]
fn navigator_right_click_detached_opens_menu_not_immediate_kill() {
    // Detached kill must go through the menu (+ confirm), matching Live/Tab
    // Close. Immediate KillSessionRequest on right-click is the pre-menu path.
    let mut overlay = open_navigator(mixed_catalog(), Some("detached:s-detach"));
    let selected_before = overlay.session_attach.render_signature().selected;
    let outcome = right_click_navigator_row(&mut overlay, "detached-web", 80, 24);
    assert_ne!(
        outcome,
        OverlayOutcome::KillSessionRequest("s-detach".to_owned()),
        "right-click must not fire KillSessionRequest before a menu confirm"
    );
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_menu_over_navigator(&overlay, "Detached row right-click");
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before
    );
    assert_eq!(menu_item_labels(&overlay), DETACHED_ACTIONS);
    assert_eq!(menu_item_enabled(&overlay, "Attach"), Some(true));
}

#[test]
fn navigator_detached_unavailable_disables_attach() {
    let entries = vec![detached_entry("s-stale", "stale-detached", "error", 0)];
    let mut overlay = open_navigator(entries, Some("detached:s-stale"));
    let outcome = right_click_navigator_row(&mut overlay, "stale-detached", 80, 24);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_menu_over_navigator(&overlay, "unavailable Detached");
    assert_eq!(menu_item_labels(&overlay), DETACHED_ACTIONS);
    assert_eq!(
        menu_item_enabled(&overlay, "Attach"),
        Some(false),
        "unavailable detached Attach must render disabled"
    );
}

#[test]
fn navigator_right_click_prompt_row_is_inert() {
    let mut overlay = open_navigator(mixed_catalog(), None);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let _ = overlay
        .session_attach
        .visible_lines(rect.body_width, rect.body_height);
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top,
                column: rect.body_left + 1,
            },
            button: PointerButton::Right,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::SessionAttach,
        "prompt right-click must not open a menu"
    );
}

#[test]
fn navigator_right_click_outside_panel_does_not_open_menu() {
    let mut overlay = open_navigator(mixed_catalog(), None);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let _ = overlay
        .session_attach
        .visible_lines(rect.body_width, rect.body_height);
    let _outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint { row: 0, column: 0 },
            button: PointerButton::Right,
            x_in_body: None,
        },
        rect,
    );
    assert!(!overlay.is_context_menu());
    assert_ne!(overlay.render_signature().mode, OverlayMode::ContextMenu);
}

#[test]
fn navigator_row_menu_esc_returns_to_navigator() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:12"));
    let selected_before = overlay.session_attach.render_signature().selected;
    right_click_navigator_row(&mut overlay, "pane-a", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::SessionAttach,
        "Esc on the menu must return to the navigator, not the grid"
    );
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before,
        "dismiss must leave navigator selection intact"
    );
}

#[test]
fn navigator_row_menu_focus_live_emits_focus_session() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:13"));
    right_click_navigator_row(&mut overlay, "pane-b", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");
    // Focus is the first NavigatorRow item.
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::FocusSession(SessionToken(13))
    );
}

#[test]
fn navigator_row_menu_close_live_requests_navigator_close_for_pane() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:13"));
    right_click_navigator_row(&mut overlay, "pane-b", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");
    // Focus, Rename, Duplicate, Move, then Close.
    for _ in 0..4 {
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::NavigatorCloseRequest(NavigatorTarget::Live(SessionToken(13)))
    );
}

#[test]
fn navigator_row_menu_close_tab_requests_navigator_close_for_tab() {
    let mut overlay = open_navigator(mixed_catalog(), Some("tab:11"));
    right_click_navigator_row(&mut overlay, "tab-build", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");
    for _ in 0..4 {
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::NavigatorCloseRequest(NavigatorTarget::Tab(SessionToken(11)))
    );
}

#[test]
fn navigator_row_menu_close_detached_requests_kill() {
    let mut overlay = open_navigator(mixed_catalog(), Some("detached:s-detach"));
    right_click_navigator_row(&mut overlay, "detached-web", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");
    // Attach, then Close.
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::KillSessionRequest("s-detach".to_owned())
    );
}

#[test]
fn navigator_row_menu_rename_live_routes_navigator_action() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:12"));
    right_click_navigator_row(&mut overlay, "pane-a", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::NavigatorAction(NavigatorAction::Rename(NavigatorTarget::Live(
            SessionToken(12)
        )))
    );
}

#[test]
fn navigator_row_menu_hits_the_rendered_filtered_row() {
    let mut entries = mixed_catalog();
    entries.extend((0..8).map(|token| live_entry(100 + token, &format!("filter-pane-{token}"))));
    let mut overlay = open_navigator(entries, None);
    for character in "filter-pane-7".chars() {
        let _ = overlay.handle_input(OverlayInput::Char(character));
    }
    let selected_before = overlay.session_attach.render_signature().selected;
    let outcome = right_click_navigator_row(&mut overlay, "filter-pane-7", 80, 12);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_menu_over_navigator(&overlay, "filtered rendered-row right-click");
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before,
        "rendered-row right-click must not retarget selection"
    );
}

#[test]
fn navigator_row_menu_clamps_at_narrow_bottom_right_edge() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:12"));
    let columns = 40;
    let rows = 12;
    let rect = overlay_rect(&overlay, columns, rows).expect("navigator rect");
    let row_in_body = navigator_row_for(&overlay, "pane-a", columns, rows);
    let cell = CellPoint {
        row: (rect.body_top + row_in_body).min(rows.saturating_sub(1)),
        column: (rect.body_left + rect.body_width.saturating_sub(1)).min(columns.saturating_sub(1)),
    };
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell,
            button: PointerButton::Right,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_menu_over_navigator(&overlay, "edge spawn");
    let menu_rect = overlay.context_menu.rect(columns, rows);
    assert!(
        menu_rect.left + menu_rect.width <= columns,
        "menu must clamp horizontally: {menu_rect:?} in {columns} cols"
    );
    assert!(
        menu_rect.top + menu_rect.height <= rows,
        "menu must clamp vertically: {menu_rect:?} in {rows} rows"
    );
}

/// Cell inside the SessionAttach panel but outside the open menu box. Mirrors
/// App routing: `handle_overlay_pointer_button` passes the ContextMenu rect.
fn underlay_cell_outside_menu(overlay: &mut OverlayUi, columns: usize, rows: usize) -> CellPoint {
    let menu_rect = overlay_rect(overlay, columns, rows).expect("menu rect");
    let restore = overlay.mode;
    overlay.mode = OverlayMode::SessionAttach;
    let underlay = overlay_rect(overlay, columns, rows).expect("navigator underlay rect");
    overlay.mode = restore;
    // Prefer the underlay title row (above the body), which the small menu box
    // almost never covers; fall back to the underlay's leading body corner.
    let candidates = [
        CellPoint {
            row: underlay.top,
            column: underlay.left + 2,
        },
        CellPoint {
            row: underlay.body_top,
            column: underlay.body_left,
        },
        CellPoint {
            row: underlay.top + underlay.height.saturating_sub(1),
            column: underlay.left,
        },
    ];
    candidates
        .into_iter()
        .find(|cell| underlay.contains(*cell) && !menu_rect.contains(*cell))
        .unwrap_or_else(|| {
            panic!("no underlay cell outside menu: underlay={underlay:?} menu={menu_rect:?}")
        })
}

#[test]
fn navigator_row_menu_outside_click_dismisses_without_activating_row() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:12"));
    for character in "pane-b".chars() {
        let _ = overlay.handle_input(OverlayInput::Char(character));
    }
    let query_before = overlay.session_attach.render_signature().query.clone();
    let selected_before = overlay.session_attach.render_signature().selected;
    right_click_navigator_row(&mut overlay, "pane-b", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");

    let menu_rect = overlay_rect(&overlay, 80, 24).expect("menu rect");
    let cell = underlay_cell_outside_menu(&mut overlay, 80, 24);
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell,
            button: PointerButton::Left,
            x_in_body: None,
        },
        menu_rect,
    );
    assert_ne!(
        outcome,
        OverlayOutcome::FocusSession(SessionToken(13)),
        "outside/underlay click must not activate the underlying Live row"
    );
    assert_eq!(
        outcome,
        OverlayOutcome::Consumed,
        "dismiss must stay Consumed (navigator remains open)"
    );
    assert!(overlay.is_open());
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::SessionAttach,
        "outside click dismisses ONLY the submenu back to the navigator"
    );
    assert_eq!(
        overlay.session_attach.render_signature().query,
        query_before,
        "filter query must survive menu dismiss"
    );
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before,
        "selection must survive menu dismiss"
    );
}

#[test]
fn navigator_row_menu_esc_preserves_filter_query() {
    let mut overlay = open_navigator(mixed_catalog(), Some("live:12"));
    for character in "pane-a".chars() {
        let _ = overlay.handle_input(OverlayInput::Char(character));
    }
    let query_before = overlay.session_attach.render_signature().query.clone();
    let selected_before = overlay.session_attach.render_signature().selected;
    right_click_navigator_row(&mut overlay, "pane-a", 80, 24);
    assert_menu_over_navigator(&overlay, "precondition");
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    assert_eq!(overlay.render_signature().mode, OverlayMode::SessionAttach);
    assert_eq!(
        overlay.session_attach.render_signature().query,
        query_before,
        "Esc must retain the navigator filter"
    );
    assert_eq!(
        overlay.session_attach.render_signature().selected,
        selected_before
    );
}
