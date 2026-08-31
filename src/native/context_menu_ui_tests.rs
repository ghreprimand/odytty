// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for the context-menu UI. Kept as a child `mod tests` of
//! `context_menu_ui` via `#[path]`, so `use super::*` still reaches the
//! module's private items -- this is a pure file move for module-size
//! relief, not a behavior or visibility change.

use super::*;

fn menu(copy: bool, paste: bool) -> ContextMenuUi {
    let mut m = ContextMenuUi::new();
    m.open(
        CellPoint { row: 4, column: 7 },
        copy,
        copy,
        paste,
        copy,
        None,
        false,
        None,
    );
    m
}

/// A multi-pane menu (Close Pane visible), no selection / clipboard.
fn multipane_menu() -> ContextMenuUi {
    let mut m = ContextMenuUi::new();
    m.open(
        CellPoint { row: 4, column: 7 },
        false,
        false,
        false,
        false,
        None,
        true,
        None,
    );
    m
}

/// A synthetic resolved file with a line/col, for the path-present file
/// section variants. No real filesystem — a fixed `Resolved`.
fn resolved_file() -> Resolved {
    Resolved {
        abs: "/proj/src/main.rs".to_owned(),
        kind: crate::paths::FsKind::File,
        line: Some(42),
        col: Some(7),
    }
}

/// A single-pane menu with a resolved path under the click (file section
/// visible).
fn menu_with_path() -> ContextMenuUi {
    let mut m = ContextMenuUi::new();
    m.open(
        CellPoint { row: 4, column: 7 },
        false,
        false,
        false,
        false,
        None,
        false,
        Some(resolved_file()),
    );
    m
}

/// A synthetic resolved IMAGE file (C4): drives the "Open in OdyTTY" item.
fn resolved_image() -> Resolved {
    Resolved {
        abs: "/proj/assets/diagram.png".to_owned(),
        kind: crate::paths::FsKind::File,
        line: None,
        col: None,
    }
}

/// A single-pane menu with a resolved image path under the click (the full
/// file section, including "Open in OdyTTY", is visible).
fn menu_with_image() -> ContextMenuUi {
    let mut m = ContextMenuUi::new();
    m.open(
        CellPoint { row: 4, column: 7 },
        false,
        false,
        false,
        false,
        None,
        false,
        Some(resolved_image()),
    );
    m
}

#[test]
fn rect_clamps_to_fit_grid() {
    let mut m = ContextMenuUi::new();
    // Spawn far past the right/bottom edge; the box must shift to fit.
    m.open(
        CellPoint {
            row: 100,
            column: 100,
        },
        true,
        true,
        true,
        true,
        None,
        false,
        None,
    );
    let rect = m.rect(40, 20);
    assert!(rect.left + rect.width <= 40);
    assert!(rect.top + rect.height <= 20);
}

#[test]
fn rect_clears_reserved_left_band() {
    // MENU-Z-ORDER: a left-rail clearance keeps the whole box right of the
    // reserved band even when the spawn cell lands inside it (a right-click
    // on the floating rail).
    let mut m = menu(true, true);
    m.spawn = CellPoint { row: 2, column: 1 };
    m.set_rail_clearance(16, 0);
    let rect = m.rect(80, 34);
    assert!(
        rect.left >= 16,
        "box left ({}) must clear the reserved 16-col rail band",
        rect.left
    );
    assert!(rect.left + rect.width <= 80, "box stays on the grid");
}

#[test]
fn rect_clears_reserved_right_band() {
    // MENU-Z-ORDER: a right-rail clearance keeps the box's right edge left of
    // the reserved band, even when the spawn cell is near the right edge.
    let mut m = menu(true, true);
    m.spawn = CellPoint { row: 2, column: 78 };
    m.set_rail_clearance(0, 16);
    let rect = m.rect(80, 34);
    assert!(
        rect.left + rect.width <= 80 - 16,
        "box right edge ({}) must clear the reserved right band (<= 64)",
        rect.left + rect.width
    );
}

#[test]
fn rect_no_clearance_is_byte_identical() {
    // A zero reserve must not move the box: the default menu geometry is
    // unchanged by the clearance machinery.
    let mut m = menu(true, true);
    m.spawn = CellPoint { row: 4, column: 7 };
    let baseline = m.rect(80, 34);
    m.set_rail_clearance(0, 0);
    assert_eq!(m.rect(80, 34), baseline, "zero reserve is a no-op");
}

#[test]
fn rect_tracks_spawn_when_it_fits() {
    let m = menu(true, true);
    // The with-selection single-pane menu (CONTEXT_MENU_BODY_ROWS body rows
    // plus two borders) needs a grid tall enough to host it at the spawn row
    // without clamping upward.
    let rect = m.rect(80, CONTEXT_MENU_BODY_ROWS + 6);
    assert_eq!(rect.left, 7);
    assert_eq!(rect.top, 4);
    assert_eq!(rect.body_top, 5);
    assert_eq!(rect.body_height, CONTEXT_MENU_BODY_ROWS);
}

#[test]
fn tall_window_has_no_scroll_and_full_body() {
    // The fits-on-screen case: the whole body is visible, the scroll offset
    // is zero, and the layout is byte-identical to the pre-scroll menu.
    let m = menu(true, true);
    let rect = m.rect(80, 34);
    assert_eq!(
        rect.body_height,
        m.body_row_count(),
        "full body visible when it fits"
    );
    assert_eq!(
        m.scroll_offset(rect.body_height),
        0,
        "no scroll when the menu fits"
    );
}

#[test]
fn rect_clamps_body_window_to_short_window() {
    // A window too short for the 14-row body: the body window is clamped to
    // the box interior and never overflows past the bottom border / window.
    let m = menu(true, true);
    let rect = m.rect(40, 8);
    assert!(rect.top + rect.height <= 8, "box stays on screen");
    assert!(
        rect.body_height <= rect.height.saturating_sub(2),
        "body window fits inside the top/bottom borders"
    );
    assert!(
        rect.body_top + rect.body_height < rect.top + rect.height,
        "body window must not overflow into/below the bottom border"
    );
    assert!(
        rect.body_height < m.body_row_count(),
        "the window really is shorter than the menu"
    );
}

#[test]
fn focus_down_scrolls_last_item_into_view() {
    // Walking focus down to the last item must scroll the window so the last
    // item becomes visible and reachable by both keyboard and pointer.
    let mut m = menu(true, true);
    let rect = m.rect(40, 8);
    let body_height = rect.body_height;
    assert_eq!(
        m.scroll_offset(body_height),
        0,
        "starts unscrolled at item 0"
    );

    for _ in 0..(m.item_count() - 1) {
        m.handle_input(OverlayInput::Down);
    }
    assert_eq!(m.focused, m.item_count() - 1, "focus reached the last item");

    let scroll = m.scroll_offset(body_height);
    let last_body_row = m.focused_body_row();
    assert!(
        scroll <= last_body_row && last_body_row < scroll + body_height,
        "focused last item must sit inside the visible window \
         (scroll={scroll}, last_body_row={last_body_row}, body_height={body_height})"
    );

    // A press on the visible row holding the last item activates it. The
    // last visible single-pane item is now Detach & switch (appended to the
    // launcher section after Manage Sessions).
    let visible_row = last_body_row - scroll;
    assert_eq!(
        m.handle_press(visible_row, body_height, PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::DetachSwitch),
        "last item reachable via the scrolled visible window"
    );
}

#[test]
fn focus_cycles_with_wrap() {
    let mut m = menu(true, true);
    assert_eq!(m.focused, 0);
    m.handle_input(OverlayInput::Up);
    // Wraps from 0 to the last *visible* item (Detach & switch). With a
    // selection the single-pane content menu shows 24 items: Copy/Cut/Paste/
    // Delete/Select All, New Tab/New Window/Close Tab (Rename Tab dropped),
    // the two splits, New/Rename/Close Workspace + Bind to Host (unbound) +
    // Save as Layout + Save Workspace as Layout + Open Layout, Settings, and
    // the six launcher items (Close Pane hidden single-pane).
    assert_eq!(m.focused, m.item_count() - 1);
    assert_eq!(m.item_count(), 24);
    m.handle_input(OverlayInput::Down);
    assert_eq!(m.focused, 0);
    m.handle_input(OverlayInput::Down);
    assert_eq!(m.focused, 1);
}

#[test]
fn copy_cut_delete_hidden_without_selection() {
    // ODP-5: with no selection the content menu omits Copy/Cut/Delete
    // entirely (rather than showing them dim); Paste/Select All remain the
    // editing anchors, so Paste leads the menu at body row 0.
    let m = menu(false, true);
    let rows = m.rows();
    for label in ["Copy Text", "Cut", "Delete"] {
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item { label: l, .. } if *l == label
            )),
            "{label} must be hidden with no selection"
        );
    }
    assert_eq!(rows[0], item("Paste Text", true, true));
    assert_eq!(rows[1], item("Select All", false, true));
}

#[test]
fn copy_present_and_hoisted_with_selection() {
    // ODP-8: a selection surfaces Copy at the very top of the content menu.
    let m = menu(true, false);
    let rows = m.rows();
    assert_eq!(rows[0], item("Copy Text", true, true));
    assert_eq!(rows[1], item("Cut", false, true));
}

#[test]
fn paste_disabled_swallows_activation() {
    let mut m = menu(true, false);
    // Paste is at item index 2 → body row 2.
    assert_eq!(
        m.handle_press(2, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Consumed
    );
}

#[test]
fn select_all_always_activates() {
    let mut m = menu(false, false);
    // With no selection Copy/Cut/Delete are hidden, so Select All sits at
    // item index 1 → body row 1 (right after Paste).
    assert_eq!(
        m.handle_press(1, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::SelectAll)
    );
}

#[test]
fn content_menu_tab_actions_and_no_rename_row() {
    // F7: the content menu drops the always-disabled Rename Tab row (it only
    // has a target on a tab right-click). With no selection the tab-actions
    // section sits at body rows 3-5: New Tab / New Window / Close Tab.
    let mut m = menu(false, false);
    let rows = m.rows();
    assert!(
        !rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Rename Tab",
                ..
            }
        )),
        "Rename Tab must not appear on the content menu"
    );
    assert_eq!(rows[3], item("New Tab", false, true));
    assert_eq!(rows[4], item("New Window", false, true));
    assert_eq!(rows[5], item("Close Tab", false, true));
    assert_eq!(
        m.handle_press(3, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::NewTab)
    );
    assert_eq!(
        m.handle_press(4, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::NewWindow)
    );
    assert_eq!(
        m.handle_press(5, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::CloseTab)
    );
}

#[test]
fn split_items_always_activate() {
    let mut m = menu(false, false);
    // With no selection the splits sit at body rows 7 (Split Right) and 8
    // (Split Down): Paste/Select All · New Tab/New Window/Close Tab · splits.
    assert_eq!(
        m.handle_press(7, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::SplitColumns)
    );
    assert_eq!(
        m.handle_press(8, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::SplitRows)
    );
}

#[test]
fn settings_always_activates() {
    let mut m = menu(false, false);
    // With no selection Settings sits at body row 18 (2 editing anchors +
    // sep + 3 tab actions + sep + 2 splits + sep + 7 workspace (incl. Bind
    // to Host + Save as Layout + Save Workspace as Layout + Open Layout) +
    // sep = 18).
    assert_eq!(
        m.handle_press(18, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::Settings)
    );
}

#[test]
fn single_pane_menu_hides_close_pane() {
    // Single-pane, no selection: Close Pane is absent; Copy/Cut/Delete are
    // hidden (no selection) and Rename Tab is dropped, so the content menu is
    // 21 items / 26 body rows — Paste/Select All · New Tab/New Window/Close
    // Tab · the two splits · New/Rename/Close Workspace + Bind to Host
    // (unbound) + Save as Layout + Save Workspace as Layout + Open Layout ·
    // Settings · the six launcher items.
    let m = menu(false, false);
    assert_eq!(m.item_count(), 21);
    let rows = m.rows();
    assert_eq!(rows.len(), 26);
    assert!(
        !rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Close Pane",
                ..
            }
        )),
        "Close Pane must not appear in a single-pane menu"
    );
    assert_eq!(rows[3], item("New Tab", false, true));
    assert_eq!(rows[4], item("New Window", false, true));
    assert_eq!(rows[5], item("Close Tab", false, true));
    assert_eq!(rows[10], item("New Workspace", false, true));
    assert_eq!(rows[11], item("Rename Workspace", false, true));
    assert_eq!(rows[12], item("Close Workspace", false, true));
    assert_eq!(
        rows[13],
        item("Bind to Host\u{2026}", false, true),
        "an unbound workspace shows Bind to Host in the workspace section"
    );
    assert_eq!(rows[14], item("Save as Layout\u{2026}", false, true));
    assert_eq!(
        rows[15],
        item("Save Workspace as Layout\u{2026}", false, true)
    );
    assert_eq!(rows[16], item("Open Layout\u{2026}", false, true));
    assert_eq!(rows[17], ContextMenuRow::Separator);
    assert_eq!(
        rows[18],
        item("Settings", false, true),
        "Settings sits at body row 18 (no selection, workspace + bind + layout)"
    );
    assert_eq!(rows[19], ContextMenuRow::Separator);
    assert_eq!(rows[20], item("Keyboard Shortcuts", false, true));
    assert_eq!(rows[21], item("Connection Manager", false, true));
    assert_eq!(rows[22], item("Command Palette", false, true));
    assert_eq!(rows[23], item("Session Replay", false, true));
    assert_eq!(rows[24], item("Manage Sessions", false, true));
    assert_eq!(rows[25], item("Detach & switch", false, true));
}

#[test]
fn no_path_menu_hides_the_file_section() {
    // C3: with no resolved path under the click, the four file items are
    // absent and the layout is the 26-row single-pane content menu (no
    // selection). This is the no-file-section guarantee.
    let m = menu(false, false);
    assert_eq!(m.item_count(), 21);
    let rows = m.rows();
    assert_eq!(rows.len(), 26);
    for label in ["Open", "Copy Path", "Copy File", "Reveal in File Manager"] {
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item { label: l, .. } if *l == label
            )),
            "file item {label} must be absent with no path target"
        );
    }
}

/// A single-pane content menu with the active workspace bound to a host
/// (ODP-6B): the workspace section shows Unbind from Host instead of Bind.
fn menu_bound_workspace() -> ContextMenuUi {
    let mut m = ContextMenuUi::new();
    m.open_with_prompt_editing_hint(
        CellPoint { row: 4, column: 7 },
        true,
        true,
        true,
        true,
        false,
        None,
        false,
        false,
        false,
        true, // bound_workspace
        ContextMenuSurface::Content,
        None,
    );
    m
}

#[test]
fn workspace_bind_toggle_is_conditional_on_bind_state() {
    // ODP-6B: exactly one of the bind/unbind pair shows on the content menu,
    // keyed to whether the active workspace is bound.
    let unbound = menu(true, true);
    let unbound_rows = unbound.rows();
    assert!(
        unbound_rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Bind to Host\u{2026}",
                ..
            }
        )),
        "an unbound workspace shows Bind to Host"
    );
    assert!(
        !unbound_rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Unbind from Host",
                ..
            }
        )),
        "an unbound workspace hides Unbind"
    );

    let bound = menu_bound_workspace();
    let bound_rows = bound.rows();
    assert!(
        bound_rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Unbind from Host",
                ..
            }
        )),
        "a bound workspace shows Unbind from Host"
    );
    assert!(
        !bound_rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Bind to Host\u{2026}",
                ..
            }
        )),
        "a bound workspace hides Bind to Host"
    );
    // The pair swaps 1-for-1, so the item count is unchanged either way.
    assert_eq!(unbound.item_count(), bound.item_count());
}

#[test]
fn path_present_menu_appends_the_file_section() {
    // UX-B: a resolved file shows only path actions plus pinned terminal
    // Copy/Paste text actions. Global tab/split/settings rows are filtered
    // out; a separator reflows between the file section and text actions.
    let m = menu_with_path();
    assert_eq!(m.item_count(), 7, "5 file items + 2 pinned text items");
    let rows = m.rows();
    assert_eq!(rows.len(), 8, "5 file rows + separator + 2 text rows");
    assert_eq!(rows[0], item("Open", true, true));
    assert_eq!(rows[1], item("Open With\u{2026}", false, true));
    assert_eq!(rows[2], item("Copy Path", false, true));
    assert_eq!(rows[3], item("Copy File", false, true));
    assert_eq!(rows[4], item("Reveal in File Manager", false, true));
    assert_eq!(rows[5], ContextMenuRow::Separator);
    assert_eq!(rows[6], item("Copy Text", false, false));
    assert_eq!(rows[7], item("Paste Text", false, false));
    for label in [
        "Cut",
        "Delete",
        "Select All",
        "New Tab",
        "Rename Tab",
        "Close Tab",
        "Split Right",
        "Split Down",
        "Settings",
        "Connection Manager",
        "Command Palette",
        "Session Replay",
        "Manage Sessions",
    ] {
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item { label: l, .. } if *l == label
            )),
            "global item {label} must be absent in a path-scoped menu"
        );
    }
}

#[test]
fn non_image_path_hides_open_in_odytty() {
    // C4: a resolved *non-image* file shows the path-scoped file section
    // but NOT "Open in OdyTTY".
    let m = menu_with_path();
    assert_eq!(
        m.item_count(),
        7,
        "5 file/text items (incl. Open With…), no C4 item"
    );
    let rows = m.rows();
    assert_eq!(rows.len(), 8);
    assert!(
        !rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Open in OdyTTY",
                ..
            }
        )),
        "Open in OdyTTY must be absent on a non-image path"
    );
}

#[test]
fn image_path_shows_open_in_odytty_after_open() {
    // C4: a resolved image span adds "Open in OdyTTY" right after "Open" in
    // the path-scoped file section. "Open With…" follows "Open in OdyTTY".
    let m = menu_with_image();
    assert_eq!(
        m.item_count(),
        8,
        "6 file items (incl. C4 + C3b) + 2 pinned text items"
    );
    let rows = m.rows();
    assert_eq!(rows.len(), 9, "6 file rows + separator + 2 text rows");
    assert_eq!(rows[0], item("Open", true, true));
    assert_eq!(rows[1], item("Open in OdyTTY", false, true));
    assert_eq!(rows[2], item("Open With\u{2026}", false, true));
    assert_eq!(rows[3], item("Copy Path", false, true));
    assert_eq!(rows[4], item("Copy File", false, true));
    assert_eq!(rows[5], item("Reveal in File Manager", false, true));
    assert_eq!(rows[6], ContextMenuRow::Separator);
    assert_eq!(rows[7], item("Copy Text", false, false));
    assert_eq!(rows[8], item("Paste Text", false, false));
}

#[test]
fn open_in_odytty_activates_on_press_for_an_image() {
    let mut m = menu_with_image();
    // "Open in OdyTTY" sits at body row 1.
    assert_eq!(
        m.handle_press(1, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::OpenInOdytty)
    );
}

#[test]
fn open_with_shows_for_a_file_activates_and_carries_no_accelerator() {
    // C3b: "Open With…" is a file affordance; it activates on press and is
    // pointer-only (no chord).
    let mut m = menu_with_path();
    assert_eq!(
        m.handle_press(1, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::OpenWith)
    );
    assert_eq!(ContextMenuItem::OpenWith.bindable_action(), None);
}

#[test]
fn open_with_hidden_for_a_directory_target() {
    // C3b: a resolved *directory* keeps the C3 copy/reveal items but NOT
    // "Open With…" (a file-only affordance).
    let mut m = ContextMenuUi::new();
    m.open(
        CellPoint { row: 4, column: 7 },
        false,
        false,
        false,
        false,
        None,
        false,
        Some(Resolved {
            abs: "/proj/src".to_owned(),
            kind: crate::paths::FsKind::Dir,
            line: None,
            col: None,
        }),
    );
    let rows = m.rows();
    assert_eq!(
        rows,
        vec![
            item("Open", true, true),
            item("Copy Path", false, true),
            item("Copy File", false, true),
            item("Reveal in File Manager", false, true),
            ContextMenuRow::Separator,
            item("Copy Text", false, false),
            item("Paste Text", false, false),
        ]
    );
    assert!(
        !rows.iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Open With\u{2026}",
                ..
            }
        )),
        "Open With… must be absent on a directory target"
    );
    // The signature distinguishes file from directory targets.
    assert!(!m.render_signature().is_file_target);
    assert!(menu_with_path().render_signature().is_file_target);
}

#[test]
fn image_target_changes_the_signature() {
    // The C4 item changes the rendered rows, so its presence must alter the
    // render-cache signature (repaint when the item appears).
    let image_sig = menu_with_image().render_signature();
    let file_sig = menu_with_path().render_signature();
    assert_ne!(image_sig, file_sig);
    assert!(image_sig.is_image_target, "image span sets the flag");
    assert!(!file_sig.is_image_target, "a .rs file does not");
    assert!(!menu(false, false).render_signature().is_image_target);
}

#[test]
fn open_in_odytty_carries_no_accelerator() {
    // The C4 item is pointer-only.
    assert_eq!(ContextMenuItem::OpenInOdytty.bindable_action(), None);
}

#[test]
fn file_items_carry_no_accelerator() {
    // The file items are pointer-only; even with accelerators populated for
    // other items, the file rows render a blank accelerator.
    let mut m = menu_with_path();
    let mut accels: [Option<String>; CONTEXT_MENU_ITEMS] = std::array::from_fn(|_| None);
    accels[0] = Some("Ctrl+Shift+C".to_owned());
    m.set_accelerators(accels);
    let rows = m.rows();
    for (body_row, row) in rows.iter().enumerate().take(5) {
        assert!(
            matches!(
                row,
                ContextMenuRow::Item {
                    accelerator: None,
                    ..
                }
            ),
            "file item at body row {body_row} must have no accelerator"
        );
    }
}

#[test]
fn file_items_activate_on_press() {
    let mut m = menu_with_path();
    // Open is at body row 0, then Open With… / Copy Path / Copy File /
    // Reveal (non-image file, so no "Open in OdyTTY").
    assert_eq!(
        m.handle_press(0, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::OpenPath)
    );
    assert_eq!(
        m.handle_press(1, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::OpenWith)
    );
    assert_eq!(
        m.handle_press(2, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::CopyPath)
    );
    assert_eq!(
        m.handle_press(3, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::CopyFile)
    );
    assert_eq!(
        m.handle_press(4, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::RevealPath)
    );
}

#[test]
fn path_target_presence_changes_the_signature() {
    // The file section changes the rendered rows, so its presence must alter
    // the render-cache signature (repaint when the section appears).
    assert_ne!(
        menu(false, false).render_signature(),
        menu_with_path().render_signature()
    );
    assert!(menu_with_path().render_signature().has_path_target);
    assert!(!menu(false, false).render_signature().has_path_target);
}

#[test]
fn path_target_accessor_returns_the_resolved() {
    let m = menu_with_path();
    let target = m.path_target().expect("path target present");
    assert_eq!(target.abs, "/proj/src/main.rs");
    assert_eq!(target.line, Some(42));
    assert!(menu(false, false).path_target().is_none());
}

#[test]
fn multi_pane_menu_shows_close_pane_in_the_split_section() {
    // Multi-pane: Close Pane appears after Split Down in the split/pane
    // section; the workspace section, Settings, and the v0.3.1 launcher
    // section follow below.
    // Multi-pane, no selection: 22 items / 27 body rows. Paste/Select All ·
    // New Tab/New Window/Close Tab · Split Right/Split Down/Close Pane ·
    // New/Rename/Close Workspace + Bind to Host + Save as Layout + Save
    // Workspace as Layout + Open Layout · Settings · six launchers.
    let m = multipane_menu();
    assert_eq!(m.item_count(), 22);
    let rows = m.rows();
    assert_eq!(
        rows.len(),
        27,
        "one more row than the single-pane content menu"
    );
    assert_eq!(rows[7], item("Split Right", false, true));
    assert_eq!(rows[8], item("Split Down", false, true));
    assert_eq!(
        rows[9],
        item("Close Pane", false, true),
        "Close Pane sits at body row 9, alongside the splits"
    );
    assert_eq!(rows[10], ContextMenuRow::Separator);
    assert_eq!(rows[11], item("New Workspace", false, true));
    assert_eq!(rows[12], item("Rename Workspace", false, true));
    assert_eq!(rows[13], item("Close Workspace", false, true));
    assert_eq!(rows[14], item("Bind to Host\u{2026}", false, true));
    assert_eq!(rows[15], item("Save as Layout\u{2026}", false, true));
    assert_eq!(
        rows[16],
        item("Save Workspace as Layout\u{2026}", false, true)
    );
    assert_eq!(rows[17], item("Open Layout\u{2026}", false, true));
    assert_eq!(rows[18], ContextMenuRow::Separator);
    assert_eq!(
        rows[19],
        item("Settings", false, true),
        "Settings sits at body row 19 in the multi-pane content menu"
    );
    assert_eq!(rows[20], ContextMenuRow::Separator);
    assert_eq!(rows[21], item("Keyboard Shortcuts", false, true));
    assert_eq!(rows[22], item("Connection Manager", false, true));
    assert_eq!(rows[23], item("Command Palette", false, true));
    assert_eq!(rows[24], item("Session Replay", false, true));
    assert_eq!(rows[25], item("Manage Sessions", false, true));
    assert_eq!(rows[26], item("Detach & switch", false, true));
}

#[test]
fn multi_pane_close_pane_activates_on_press() {
    // Pressing the Close Pane body row (9) activates the item.
    let mut m = multipane_menu();
    assert_eq!(
        m.handle_press(9, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::ClosePane)
    );
}

#[test]
fn multi_pane_focus_wraps_through_all_items() {
    // Up from item 0 wraps to the last visible item (Detach & switch, index
    // 21), proving Close Pane is in the focus cycle only when multi-pane and
    // the workspace + launcher items extend the cycle.
    let mut m = multipane_menu();
    assert_eq!(m.focused, 0);
    m.handle_input(OverlayInput::Up);
    assert_eq!(m.focused, 21);
    assert_eq!(m.item_count(), 22);
}

#[test]
fn separator_row_is_inert() {
    let mut m = menu(true, true);
    let before = m.focused;
    for sep in [
        CONTEXT_MENU_SEPARATOR_ROW,
        CONTEXT_MENU_SECOND_SEPARATOR_ROW,
        CONTEXT_MENU_THIRD_SEPARATOR_ROW,
    ] {
        assert_eq!(
            m.handle_press(sep, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
        assert_eq!(m.focused, before, "separator press does not move focus");
    }
}

#[test]
fn hover_skips_separator() {
    let mut m = menu(true, true);
    m.handle_hover(Some(2), m.body_row_count()); // Paste
    assert_eq!(m.focused, 2);
    // Hovering each separator leaves focus unchanged.
    for sep in [
        CONTEXT_MENU_SEPARATOR_ROW,
        CONTEXT_MENU_SECOND_SEPARATOR_ROW,
        CONTEXT_MENU_THIRD_SEPARATOR_ROW,
    ] {
        m.handle_hover(Some(sep), m.body_row_count());
        assert_eq!(m.focused, 2, "separator hover is inert");
    }
    // Hovering Settings (body row 21, item index 17 in the with-selection
    // reference — the workspace section + Bind + Save/Save Workspace/Open
    // Layout rows now sit above Settings) focuses it.
    m.handle_hover(Some(21), m.body_row_count());
    assert_eq!(m.focused, 17, "hover Settings focuses it");
}

#[test]
fn enabled_items_activate_on_press() {
    let mut m = menu(true, true);
    assert_eq!(
        m.handle_press(0, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::Copy)
    );
    assert_eq!(
        m.handle_press(2, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::Paste)
    );
}

#[test]
fn cut_and_delete_disabled_swallow_activation() {
    let mut m = ContextMenuUi::new();
    m.open(
        CellPoint { row: 4, column: 7 },
        true,
        false,
        true,
        false,
        None,
        false,
        None,
    );
    assert_eq!(
        m.handle_press(1, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Consumed
    );
    assert_eq!(
        m.handle_press(3, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Consumed
    );
}

#[test]
fn cut_and_delete_disabled_can_carry_shell_integration_hint() {
    let mut m = ContextMenuUi::new();
    m.open_with_prompt_editing_hint(
        CellPoint { row: 4, column: 7 },
        true,
        false,
        true,
        false,
        true,
        None,
        false,
        false,
        false,
        false,
        ContextMenuSurface::Content,
        None,
    );
    let rows = m.rows();
    assert_eq!(
        rows[1],
        ContextMenuRow::Item {
            label: "Cut",
            accelerator: Some(SHELL_INTEGRATION_DISABLED_HINT.to_owned()),
            focused: false,
            enabled: false,
        }
    );
    assert_eq!(
        rows[3],
        ContextMenuRow::Item {
            label: "Delete",
            accelerator: Some(SHELL_INTEGRATION_DISABLED_HINT.to_owned()),
            focused: false,
            enabled: false,
        }
    );
    assert!(m.render_signature().prompt_editing_hint);
}

#[test]
fn press_on_border_row_is_inert() {
    let mut m = menu(true, true);
    // row_in_body == CONTEXT_MENU_BODY_ROWS is past the bottom border.
    assert_eq!(
        m.handle_press(
            CONTEXT_MENU_BODY_ROWS,
            m.body_row_count(),
            PointerButton::Left
        ),
        ContextMenuOutcome::Consumed
    );
}

#[test]
fn escape_closes() {
    let mut m = menu(true, true);
    assert_eq!(
        m.handle_input(OverlayInput::Close),
        ContextMenuOutcome::Close
    );
}

#[test]
fn hover_moves_focus_only_over_items() {
    let mut m = menu(true, true);
    m.handle_hover(Some(2), m.body_row_count());
    assert_eq!(m.focused, 2);
    // Off-item hover leaves focus unchanged.
    m.handle_hover(None, m.body_row_count());
    assert_eq!(m.focused, 2);
    // Row past all body rows is inert.
    m.handle_hover(Some(CONTEXT_MENU_BODY_ROWS + 5), m.body_row_count());
    assert_eq!(m.focused, 2);
}

#[test]
fn signature_tracks_state() {
    let plain = menu(false, false).render_signature();
    let with_copy = menu(true, false).render_signature();
    assert_ne!(plain, with_copy);
    let mut m = menu(true, true);
    let before = m.render_signature();
    m.handle_input(OverlayInput::Down);
    assert_ne!(before, m.render_signature());
}

/// An `Item` row with the given label/focus/enabled and no accelerator (the
/// bare-`open` / unit-test layout).
fn item(label: &'static str, focused: bool, enabled: bool) -> ContextMenuRow {
    ContextMenuRow::Item {
        label,
        accelerator: None,
        focused,
        enabled,
    }
}

#[test]
fn rows_report_label_focus_enabled() {
    let m = menu(true, false);
    let rows = m.rows();
    assert_eq!(rows.len(), CONTEXT_MENU_BODY_ROWS);
    assert_eq!(rows[0], item("Copy Text", true, true));
    assert_eq!(rows[1], item("Cut", false, true));
    assert_eq!(rows[2], item("Paste Text", false, false));
    assert_eq!(rows[3], item("Delete", false, true));
    assert_eq!(rows[4], item("Select All", false, true));
    assert_eq!(rows[5], ContextMenuRow::Separator);
    assert_eq!(rows[6], item("New Tab", false, true));
    assert_eq!(rows[7], item("New Window", false, true));
    // F7: Rename Tab is no longer on the content menu; Close Tab follows
    // New Window directly.
    assert_eq!(rows[8], item("Close Tab", false, true));
    assert_eq!(rows[9], ContextMenuRow::Separator);
    assert_eq!(rows[10], item("Split Right", false, true));
    assert_eq!(rows[11], item("Split Down", false, true));
    assert_eq!(rows[12], ContextMenuRow::Separator);
    // Workspace section sits between Split and Settings; an unbound
    // workspace shows Bind to Host as its last row (ODP-6B).
    assert_eq!(rows[13], item("New Workspace", false, true));
    assert_eq!(rows[14], item("Rename Workspace", false, true));
    assert_eq!(rows[15], item("Close Workspace", false, true));
    assert_eq!(rows[16], item("Bind to Host\u{2026}", false, true));
    assert_eq!(rows[17], item("Save as Layout\u{2026}", false, true));
    assert_eq!(
        rows[18],
        item("Save Workspace as Layout\u{2026}", false, true)
    );
    assert_eq!(rows[19], item("Open Layout\u{2026}", false, true));
    assert_eq!(rows[20], ContextMenuRow::Separator);
    assert_eq!(rows[21], item("Settings", false, true));
}

#[test]
fn copy_and_paste_are_labeled_as_text_actions() {
    assert_eq!(ContextMenuItem::Copy.label(), "Copy Text");
    assert_eq!(ContextMenuItem::Paste.label(), "Paste Text");
}

#[test]
fn accelerators_render_and_widen_the_menu() {
    let mut m = menu(true, true);
    let narrow = m.menu_width();
    // Populate a couple of accelerators in ALL order: Copy and Split Right.
    let mut accels: [Option<String>; CONTEXT_MENU_ITEMS] = std::array::from_fn(|_| None);
    let copy_index = ContextMenuItem::ALL
        .iter()
        .position(|item| *item == ContextMenuItem::Copy)
        .expect("Copy belongs to the complete item set");
    let split_index = ContextMenuItem::ALL
        .iter()
        .position(|item| *item == ContextMenuItem::SplitColumns)
        .expect("Split Right belongs to the complete item set");
    accels[copy_index] = Some("Ctrl+Shift+C".to_owned());
    accels[split_index] = Some("Ctrl+Shift+E".to_owned());
    m.set_accelerators(accels);

    let rows = m.rows();
    assert_eq!(
        rows[0],
        ContextMenuRow::Item {
            label: "Copy Text",
            accelerator: Some("Ctrl+Shift+C".to_owned()),
            focused: true,
            enabled: true,
        }
    );
    // Split Right carries its accelerator regardless of additions elsewhere in
    // the complete item set.
    let split_row = rows
        .iter()
        .find(|row| {
            matches!(
                row,
                ContextMenuRow::Item {
                    label: "Split Right",
                    ..
                }
            )
        })
        .expect("Split Right is visible in the content menu");
    assert_eq!(
        split_row,
        &ContextMenuRow::Item {
            label: "Split Right",
            accelerator: Some("Ctrl+Shift+E".to_owned()),
            focused: false,
            enabled: true,
        }
    );
    // An item with no chord (Cut, item 1) renders a blank accelerator.
    assert_eq!(rows[1], item("Cut", false, true));
    // The menu grew to fit "longest label + gap + longest accelerator".
    assert!(
        m.menu_width() > narrow,
        "accelerators widen the menu: {} !> {narrow}",
        m.menu_width()
    );
}

#[test]
fn duplicate_actions_render_their_default_accelerators() {
    // Both Duplicate rows carry a bound chord now, so each renders its
    // accelerator beside the label (keyed by ALL order, exactly as the App
    // fills the array). Duplicate Tab -> Ctrl+Shift+D on the tab slot;
    // Duplicate Workspace -> Ctrl+Shift+Alt+D on the workspace slot.
    let accel_for = |item: ContextMenuItem, chord: &str| {
        let idx = ContextMenuItem::ALL
            .iter()
            .position(|it| *it == item)
            .expect("item is in ALL");
        let mut accels: [Option<String>; CONTEXT_MENU_ITEMS] = std::array::from_fn(|_| None);
        accels[idx] = Some(chord.to_owned());
        accels
    };
    let rendered = |m: &ContextMenuUi, label: &str| -> Option<Option<String>> {
        m.rows().into_iter().find_map(|r| match r {
            ContextMenuRow::Item {
                label: l,
                accelerator,
                ..
            } if l == label => Some(accelerator),
            _ => None,
        })
    };

    let mut tab = open_surface(ContextMenuSurface::TabSlot(SessionToken(1)), true);
    tab.set_accelerators(accel_for(ContextMenuItem::DuplicateTab, "Ctrl+Shift+D"));
    assert_eq!(
        rendered(&tab, "Duplicate Tab"),
        Some(Some("Ctrl+Shift+D".to_owned())),
        "Duplicate Tab renders its accelerator"
    );

    let mut ws = open_surface(ContextMenuSurface::WorkspaceSlot(0), true);
    ws.set_accelerators(accel_for(
        ContextMenuItem::DuplicateWorkspace,
        "Ctrl+Shift+Alt+D",
    ));
    assert_eq!(
        rendered(&ws, "Duplicate Workspace"),
        Some(Some("Ctrl+Shift+Alt+D".to_owned())),
        "Duplicate Workspace renders its accelerator"
    );
}

/// Open a menu on a specific surface (F7) with the given tab-count state.
/// Single-workspace (no `Move to Workspace` row); use
/// [`open_surface_ws`] to exercise the multi-workspace composition.
fn open_surface(surface: ContextMenuSurface, multi_tab: bool) -> ContextMenuUi {
    open_surface_ws(surface, multi_tab, false)
}

/// Open a menu on a specific surface with explicit tab- and workspace-count
/// state (W4). `multi_workspace` drives the `Move to Workspace` row.
fn open_surface_ws(
    surface: ContextMenuSurface,
    multi_tab: bool,
    multi_workspace: bool,
) -> ContextMenuUi {
    let mut m = ContextMenuUi::new();
    m.open_with_prompt_editing_hint(
        CellPoint { row: 4, column: 7 },
        true,
        true,
        true,
        true,
        false,
        match surface {
            ContextMenuSurface::TabSlot(token) => Some(token),
            _ => None,
        },
        false,
        multi_tab,
        multi_workspace,
        false,
        surface,
        None,
    );
    m
}

#[test]
fn tab_slot_menu_is_tab_scoped() {
    // F7: a tab right-click opens a tight, tab-scoped menu — no Copy / Split /
    // Settings / launcher tail. New Tab / Rename Tab · Close Tab / Close Other
    // Tabs · Connect to Host / Replace with Host (ODP-5D) · New Window, split
    // by three separators.
    let m = open_surface(ContextMenuSurface::TabSlot(SessionToken(3)), true);
    let rows = m.rows();
    assert_eq!(
        rows,
        vec![
            item("New Tab", true, true),
            item("Duplicate Tab", false, true),
            item("Rename Tab", false, true),
            ContextMenuRow::Separator,
            item("Close Tab", false, true),
            item("Close Other Tabs", false, true),
            ContextMenuRow::Separator,
            item("Connect to Host\u{2026}", false, true),
            item("Replace with Host\u{2026}", false, true),
            ContextMenuRow::Separator,
            item("New Window", false, true),
        ]
    );
    for label in [
        "Copy Text",
        "Paste Text",
        "Select All",
        "Split Right",
        "Split Down",
        "Settings",
        "Command Palette",
    ] {
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item { label: l, .. } if *l == label
            )),
            "tab menu must not contain {label}"
        );
    }
}

/// F6-W5: on a workspace bound to a host, the tab menu gains a "New Local
/// Tab" escape row beside New Tab; an unbound workspace's tab menu does not.
#[test]
fn tab_slot_new_local_tab_appears_only_when_workspace_bound() {
    let mut bound = ContextMenuUi::new();
    bound.open_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        false,
        Some(SessionToken(3)),
        false,
        true,
        false,
        true,
        ContextMenuSurface::TabSlot(SessionToken(3)),
        None,
    );
    let has_local = |m: &ContextMenuUi| {
        m.rows().iter().any(|r| {
            matches!(
                r,
                ContextMenuRow::Item { label, .. } if *label == "New Local Tab"
            )
        })
    };
    assert!(
        has_local(&bound),
        "bound-workspace tab menu offers New Local Tab"
    );

    let unbound = open_surface(ContextMenuSurface::TabSlot(SessionToken(3)), true);
    assert!(
        !has_local(&unbound),
        "unbound-workspace tab menu has no New Local Tab row"
    );
}

#[test]
fn tab_slot_rename_is_enabled_and_targets_the_token() {
    let m = open_surface(ContextMenuSurface::TabSlot(SessionToken(9)), true);
    assert_eq!(m.surface(), ContextMenuSurface::TabSlot(SessionToken(9)));
    assert_eq!(m.rename_target(), Some(SessionToken(9)));
}

#[test]
fn move_to_workspace_row_hidden_with_one_workspace() {
    // ODP-7: no destination workspace ⇒ no move row (single-workspace tab
    // menu is byte-identical to before W4).
    let m = open_surface_ws(ContextMenuSurface::TabSlot(SessionToken(3)), true, false);
    assert!(
        !m.rows().iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Move to Workspace\u{2026}",
                ..
            }
        )),
        "single-workspace tab menu must not offer the move row"
    );
}

#[test]
fn move_to_workspace_row_shown_and_bracketed_with_multiple_workspaces() {
    // ODP-7: with >1 workspace the move row appears between the host-actions
    // group (ODP-5D) and New Window, each in its own section (separators
    // bracket it).
    let m = open_surface_ws(ContextMenuSurface::TabSlot(SessionToken(3)), true, true);
    let rows = m.rows();
    assert_eq!(
        rows,
        vec![
            item("New Tab", true, true),
            item("Duplicate Tab", false, true),
            item("Rename Tab", false, true),
            ContextMenuRow::Separator,
            item("Close Tab", false, true),
            item("Close Other Tabs", false, true),
            ContextMenuRow::Separator,
            item("Connect to Host\u{2026}", false, true),
            item("Replace with Host\u{2026}", false, true),
            ContextMenuRow::Separator,
            item("Move to Workspace\u{2026}", false, true),
            ContextMenuRow::Separator,
            item("New Window", false, true),
        ]
    );
}

#[test]
fn move_to_workspace_row_never_on_the_content_menu() {
    // The move item is tab-scoped: even with multiple workspaces, the grid
    // menu never shows it.
    let m = open_surface_ws(ContextMenuSurface::Content, true, true);
    assert!(
        !m.rows().iter().any(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Move to Workspace\u{2026}",
                ..
            }
        )),
        "content menu must not offer the move row"
    );
}

#[test]
fn tab_slot_close_other_tabs_disabled_at_one_tab() {
    // "Close Other Tabs" is inert when only one tab is open.
    let lone = open_surface(ContextMenuSurface::TabSlot(SessionToken(1)), false);
    let row = lone
        .rows()
        .into_iter()
        .find(|r| {
            matches!(
                r,
                ContextMenuRow::Item {
                    label: "Close Other Tabs",
                    ..
                }
            )
        })
        .expect("Close Other Tabs present");
    assert_eq!(row, item("Close Other Tabs", false, false));
    // With a second tab it activates.
    let many = open_surface(ContextMenuSurface::TabSlot(SessionToken(1)), true);
    assert!(matches!(
        many.rows().into_iter().find(|r| matches!(
            r,
            ContextMenuRow::Item {
                label: "Close Other Tabs",
                ..
            }
        )),
        Some(ContextMenuRow::Item { enabled: true, .. })
    ));
}

#[test]
fn tab_slot_close_other_tabs_activates_on_press() {
    let mut m = open_surface(ContextMenuSurface::TabSlot(SessionToken(4)), true);
    // Body rows: New Tab(0) Duplicate Tab(1) Rename Tab(2) sep(3) Close
    // Tab(4) Close Other Tabs(5) sep(6) New Window(7).
    assert_eq!(
        m.handle_press(4, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::CloseTab)
    );
    let mut m = open_surface(ContextMenuSurface::TabSlot(SessionToken(4)), true);
    assert_eq!(
        m.handle_press(5, m.body_row_count(), PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::CloseOtherTabs)
    );
}

#[test]
fn tab_strip_empty_menu_is_minimal() {
    // F7 + LAYOUT-SURFACE: an empty-strip right-click opens New Tab · New
    // Workspace · Open Layout · Command Palette · Settings, with one
    // separator between the creation/restore group and the two launchers.
    let m = open_surface(ContextMenuSurface::TabStripEmpty, true);
    assert_eq!(
        m.rows(),
        vec![
            item("New Tab", true, true),
            item("New Workspace", false, true),
            item("Open Layout\u{2026}", false, true),
            ContextMenuRow::Separator,
            item("Command Palette", false, true),
            item("Settings", false, true),
        ]
    );
}

#[test]
fn workspace_slot_menu_offers_workspace_actions() {
    // §3.5 / §7.4 + RAIL-BIND + LAYOUT-SURFACE + RAIL-SAVE-ALL: a
    // workspace-slot right-click opens New / Rename / Close Workspace, the
    // host bind action in its own group, then the layout group below a
    // separator — the whole-app "Save as Layout…" leading the single-
    // workspace "Save Workspace as Layout…" (targeting the CLICKED slot). An
    // unbound slot shows "Bind to Host…".
    let m = open_surface(ContextMenuSurface::WorkspaceSlot(0), true);
    assert_eq!(
        m.rows(),
        vec![
            item("New Workspace", true, true),
            item("Duplicate Workspace", false, true),
            item("Rename Workspace", false, true),
            ContextMenuRow::Separator,
            item("Close Workspace", false, true),
            ContextMenuRow::Separator,
            item("Bind to Host\u{2026}", false, true),
            ContextMenuRow::Separator,
            item("Save as Layout\u{2026}", false, true),
            item("Save Workspace as Layout\u{2026}", false, true),
            ContextMenuRow::Separator,
            item("Settings", false, true),
        ]
    );
}

#[test]
fn workspace_slot_menu_gates_move_rows_by_position() {
    // RAIL-REORDER: the Move Up / Move Down rows appear between Rename and
    // the Close separator, gated by the clicked slot's position within the
    // total workspace count. First slot: Down only. Middle slot: both. Last
    // slot: Up only. The count is snapshotted via `set_workspace_count`
    // exactly as the App does on a rail right-click.
    let has = |m: &ContextMenuUi, label: &str| {
        m.rows()
            .iter()
            .any(|r| matches!(r, ContextMenuRow::Item { label: l, .. } if *l == label))
    };

    let mut first = open_surface(ContextMenuSurface::WorkspaceSlot(0), true);
    first.set_workspace_count(3);
    assert!(!has(&first, "Move Up"), "first slot cannot move up");
    assert!(has(&first, "Move Down"), "first slot can move down");

    let mut middle = open_surface(ContextMenuSurface::WorkspaceSlot(1), true);
    middle.set_workspace_count(3);
    assert!(has(&middle, "Move Up"), "middle slot can move up");
    assert!(has(&middle, "Move Down"), "middle slot can move down");
    // The move rows sit in the New/Rename group, above the first separator.
    let rows = middle.rows();
    let first_sep = rows
        .iter()
        .position(|r| matches!(r, ContextMenuRow::Separator))
        .expect("a separator exists");
    let up_at = rows
        .iter()
        .position(|r| {
            matches!(
                r,
                ContextMenuRow::Item {
                    label: "Move Up",
                    ..
                }
            )
        })
        .expect("Move Up present");
    assert!(
        up_at < first_sep,
        "Move rows group above the Close separator"
    );

    let mut last = open_surface(ContextMenuSurface::WorkspaceSlot(2), true);
    last.set_workspace_count(3);
    assert!(has(&last, "Move Up"), "last slot can move up");
    assert!(!has(&last, "Move Down"), "last slot cannot move down");

    // A lone workspace (count 1) shows neither.
    let mut lone = open_surface(ContextMenuSurface::WorkspaceSlot(0), true);
    lone.set_workspace_count(1);
    assert!(!has(&lone, "Move Up"));
    assert!(!has(&lone, "Move Down"));
}

#[test]
fn workspace_slot_menu_shows_unbind_when_bound() {
    // RAIL-BIND: a slot whose workspace is bound offers "Unbind from Host"
    // in place of "Bind to Host…" — the same conditional pair as the
    // content-grid menu, keyed to the CLICKED slot's binding.
    let mut m = ContextMenuUi::new();
    m.open_with_prompt_editing_hint(
        CellPoint { row: 4, column: 7 },
        false,
        false,
        false,
        false,
        false,
        None,
        false,
        true,
        false,
        true, // bound_workspace = true (clicked slot is bound)
        ContextMenuSurface::WorkspaceSlot(1),
        None,
    );
    let labels: Vec<&str> = m
        .rows()
        .iter()
        .filter_map(|row| match row {
            ContextMenuRow::Item { label, .. } => Some(*label),
            _ => None,
        })
        .collect();
    assert!(
        labels.contains(&"Unbind from Host"),
        "bound slot shows Unbind: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.starts_with("Bind to Host")),
        "bound slot hides Bind: {labels:?}"
    );
}

#[test]
fn workspace_rail_empty_menu_offers_new_workspace_and_open_layout() {
    // §3.5 + LAYOUT-SURFACE + SAVE-ALL-LAYOUT: the empty rail area offers New
    // Workspace, then the whole-app Save as Layout and Open Layout below a
    // separator (save/restore a full session from a bare rail).
    let m = open_surface(ContextMenuSurface::WorkspaceRailEmpty, true);
    assert_eq!(
        m.rows(),
        vec![
            item("New Workspace", true, true),
            ContextMenuRow::Separator,
            item("Save as Layout\u{2026}", false, true),
            item("Open Layout\u{2026}", false, true),
            ContextMenuRow::Separator,
            item("Settings", false, true),
        ]
    );
}

#[test]
fn workspace_actions_appear_on_the_content_menu() {
    // The content menu carries a workspace section (New / Rename / Close
    // Workspace) after the split section, before Settings. Rename/Close
    // target the active workspace (the content surface has no per-workspace
    // click target).
    let m = open_surface(ContextMenuSurface::Content, true);
    let labels: Vec<&str> = m
        .rows()
        .into_iter()
        .filter_map(|row| match row {
            ContextMenuRow::Item { label, .. } => Some(label),
            ContextMenuRow::Separator => None,
        })
        .collect();
    for expected in ["New Workspace", "Rename Workspace", "Close Workspace"] {
        assert!(
            labels.contains(&expected),
            "content menu must contain {expected:?}"
        );
    }
    // They sit after the split section and before Settings.
    let split = labels.iter().position(|l| *l == "Split Right").unwrap();
    let new_ws = labels.iter().position(|l| *l == "New Workspace").unwrap();
    let settings = labels.iter().position(|l| *l == "Settings").unwrap();
    assert!(
        split < new_ws && new_ws < settings,
        "workspace section sits between Split and Settings"
    );
}

#[test]
fn surface_change_repaints_the_menu() {
    // Different surfaces swap the whole composition, so their render-cache
    // signatures must differ (a surface change repaints).
    let content = open_surface(ContextMenuSurface::Content, true).render_signature();
    let tab = open_surface(ContextMenuSurface::TabSlot(SessionToken(1)), true).render_signature();
    let empty = open_surface(ContextMenuSurface::TabStripEmpty, true).render_signature();
    assert_ne!(content, tab);
    assert_ne!(content, empty);
    assert_ne!(tab, empty);
}

#[test]
fn humanize_chord_title_cases_tokens() {
    assert_eq!(humanize_chord("ctrl+shift+e".to_owned()), "Ctrl+Shift+E");
    assert_eq!(
        humanize_chord("ctrl+shift+comma".to_owned()),
        "Ctrl+Shift+Comma"
    );
    assert_eq!(humanize_chord("c".to_owned()), "C");
}

#[test]
fn split_items_map_to_pane_actions() {
    assert_eq!(
        ContextMenuItem::SplitColumns.bindable_action(),
        Some(BindableAction::SplitColumns)
    );
    assert_eq!(
        ContextMenuItem::SplitRows.bindable_action(),
        Some(BindableAction::SplitRows)
    );
    // Items with no keyboard binding map to None.
    assert_eq!(ContextMenuItem::Cut.bindable_action(), None);
    assert_eq!(ContextMenuItem::SelectAll.bindable_action(), None);
}

#[test]
fn body_row_mapping_is_consistent() {
    // Items 0-4 map 1:1 to body rows 0-4.
    for i in 0..CONTEXT_MENU_SEPARATOR_ROW {
        assert_eq!(item_to_body_row(i), i);
        assert_eq!(body_row_to_item(i), Some(i));
    }
    // The separator rows map to no item.
    assert_eq!(body_row_to_item(CONTEXT_MENU_SEPARATOR_ROW), None);
    assert_eq!(body_row_to_item(CONTEXT_MENU_SECOND_SEPARATOR_ROW), None);
    assert_eq!(body_row_to_item(CONTEXT_MENU_THIRD_SEPARATOR_ROW), None);
    assert_eq!(body_row_to_item(CONTEXT_MENU_FOURTH_SEPARATOR_ROW), None);
    assert_eq!(body_row_to_item(CONTEXT_MENU_FIFTH_SEPARATOR_ROW), None);
    // Tab actions (5-7: New Tab / New Window / Close Tab — Rename Tab dropped)
    // shift past the first separator.
    assert_eq!(item_to_body_row(5), 6);
    assert_eq!(body_row_to_item(6), Some(5));
    assert_eq!(item_to_body_row(6), 7);
    assert_eq!(body_row_to_item(7), Some(6));
    assert_eq!(item_to_body_row(7), 8);
    assert_eq!(body_row_to_item(8), Some(7));
    // Split actions (8-9) shift past the first two separators.
    assert_eq!(item_to_body_row(8), 10);
    assert_eq!(body_row_to_item(10), Some(8));
    assert_eq!(item_to_body_row(9), 11);
    assert_eq!(body_row_to_item(11), Some(9));
    // Workspace actions (10-16: New / Rename / Close + Bind to Host + Save as
    // Layout + Save Workspace as Layout + Open Layout) shift past the first
    // three separators.
    assert_eq!(item_to_body_row(10), 13);
    assert_eq!(body_row_to_item(13), Some(10));
    assert_eq!(item_to_body_row(16), 19);
    assert_eq!(body_row_to_item(19), Some(16));
    // Settings (17) shifts past the first four separators.
    assert_eq!(item_to_body_row(17), 21);
    assert_eq!(body_row_to_item(21), Some(17));
    // A launcher item (18) shifts past all five separators.
    assert_eq!(item_to_body_row(18), 23);
    assert_eq!(body_row_to_item(23), Some(18));
}

// ── ODP-2C connection-row surface composition ──────────────────────────

fn conn_host(alias: &str, source: ConnectionHostSource) -> ConnectionHost {
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
        source,
    }
}

#[test]
fn connection_row_odytty_shows_all_five_actions() {
    // An OdyTTY-owned row offers Open in New Tab / Open in New Workspace /
    // Bind Current Workspace / Edit / Remove, in that order.
    let mut m = ContextMenuUi::new();
    m.open_connection_row(
        CellPoint { row: 3, column: 5 },
        0,
        conn_host("web1", ConnectionHostSource::Odytty),
    );
    assert_eq!(
        m.visible_items(),
        vec![
            ContextMenuItem::ConnRowOpenInTab,
            ContextMenuItem::ConnRowOpenInWorkspace,
            ContextMenuItem::ConnRowBindWorkspace,
            ContextMenuItem::ConnRowEdit,
            ContextMenuItem::ConnRowRemove,
        ]
    );
    assert_eq!(
        m.connection_target().map(|host| host.alias.as_str()),
        Some("web1")
    );
    assert!(m.render_signature().connection_is_odytty);
    // A separator falls between the open/bind group and the mutating group.
    assert!(matches!(m.rows().get(3), Some(ContextMenuRow::Separator)));
}

#[test]
fn connection_row_ssh_config_hides_edit_and_remove() {
    // An ssh-config-imported row is read-only (OdyTTY never writes
    // ~/.ssh/config), so Edit and Remove are hidden — only the three
    // non-mutating actions show.
    let mut m = ContextMenuUi::new();
    m.open_connection_row(
        CellPoint { row: 3, column: 5 },
        0,
        conn_host("remote", ConnectionHostSource::SshConfig),
    );
    assert_eq!(
        m.visible_items(),
        vec![
            ContextMenuItem::ConnRowOpenInTab,
            ContextMenuItem::ConnRowOpenInWorkspace,
            ContextMenuItem::ConnRowBindWorkspace,
        ]
    );
    assert!(!m.render_signature().connection_is_odytty);
    // No separator: a single tight group of three.
    assert!(
        !m.rows()
            .iter()
            .any(|r| matches!(r, ContextMenuRow::Separator))
    );
}

#[test]
fn content_surface_never_shows_connection_row_actions() {
    // The five ODP-2C items are ConnectionRow-only; the content menu (with a
    // selection) must never surface them.
    let m = menu(true, true);
    for item in m.visible_items() {
        assert!(
            !matches!(
                item,
                ContextMenuItem::ConnRowOpenInTab
                    | ContextMenuItem::ConnRowOpenInWorkspace
                    | ContextMenuItem::ConnRowBindWorkspace
                    | ContextMenuItem::ConnRowEdit
                    | ContextMenuItem::ConnRowRemove
            ),
            "content menu leaked a connection-row item: {item:?}"
        );
    }
}

#[test]
fn opening_another_surface_clears_the_connection_target() {
    // The snapshotted host must not leak across a later non-ConnectionRow
    // open, or a content menu could carry a stale target.
    let mut m = ContextMenuUi::new();
    m.open_connection_row(
        CellPoint { row: 3, column: 5 },
        0,
        conn_host("web1", ConnectionHostSource::Odytty),
    );
    assert!(m.connection_target().is_some());
    m.open(
        CellPoint { row: 0, column: 0 },
        false,
        false,
        false,
        false,
        None,
        false,
        None,
    );
    assert!(m.connection_target().is_none());
}
