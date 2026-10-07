// SPDX-License-Identifier: GPL-3.0-only
//! Pure context-menu tests for activation buttons, empty and shrinking
//! compositions, geometry bounds, the render signature, and the production
//! body-row mapping.

use super::*;

fn open_on(surface: ContextMenuSurface, copy: bool, multi_pane: bool) -> ContextMenuUi {
    let mut m = ContextMenuUi::new();
    m.open_with_prompt_editing_hint(
        CellPoint { row: 4, column: 7 },
        copy,
        copy,
        true,
        copy,
        false,
        None,
        multi_pane,
        true,
        true,
        false,
        surface,
        None,
    );
    m
}

fn content(copy: bool) -> ContextMenuUi {
    open_on(ContextMenuSurface::Content, copy, false)
}

fn focused_item(m: &ContextMenuUi) -> Option<ContextMenuItem> {
    m.visible_items().get(m.focused).copied()
}

fn all_index(item: ContextMenuItem) -> usize {
    ContextMenuItem::ALL
        .iter()
        .position(|it| *it == item)
        .expect("item is in ALL")
}

fn accelerators_with(entries: &[(ContextMenuItem, &str)]) -> [Option<String>; CONTEXT_MENU_ITEMS] {
    let mut accels: [Option<String>; CONTEXT_MENU_ITEMS] = std::array::from_fn(|_| None);
    for (item, text) in entries {
        accels[all_index(*item)] = Some((*text).to_owned());
    }
    accels
}

#[test]
fn right_press_on_an_enabled_row_is_inert_and_keeps_focus() {
    let mut m = content(true);
    let height = m.body_row_count();
    let close_tab_row = m
        .body_layout()
        .iter()
        .position(|row| *row == Some(ContextMenuItem::CloseTab))
        .expect("Close Tab is on the content menu");
    let before = m.focused;
    assert_eq!(
        m.handle_press(close_tab_row, height, PointerButton::Right),
        ContextMenuOutcome::Consumed,
        "a right press never activates a row"
    );
    assert_eq!(m.focused, before, "a right press does not move focus");
    assert_eq!(
        m.handle_press(close_tab_row, height, PointerButton::Left),
        ContextMenuOutcome::Activate(ContextMenuItem::CloseTab),
        "the left press on the same row activates it"
    );
}

#[test]
fn an_empty_navigator_composition_tolerates_keys_focus_and_geometry() {
    let mut m = open_on(ContextMenuSurface::NavigatorRow, false, false);
    assert_eq!(m.item_count(), 0, "no target composes no rows");
    assert_eq!(
        m.handle_input(OverlayInput::Up),
        ContextMenuOutcome::Consumed
    );
    assert_eq!(
        m.handle_input(OverlayInput::Down),
        ContextMenuOutcome::Consumed
    );
    assert_eq!(
        m.handle_input(OverlayInput::Activate),
        ContextMenuOutcome::Consumed,
        "nothing activates in an empty menu"
    );
    assert_eq!(
        m.handle_input(OverlayInput::Char(' ')),
        ContextMenuOutcome::Consumed
    );
    assert_eq!(m.focused_body_row(), 0);
    assert_eq!(m.scroll_offset(4), 0);
    assert!(m.rows().is_empty());
    assert_eq!(
        m.handle_press(0, 1, PointerButton::Left),
        ContextMenuOutcome::Consumed
    );
    m.handle_hover(Some(0), 1);
    let rect = m.rect(80, 24);
    assert_eq!(rect.body_height, 0);
}

#[test]
fn a_shrinking_composition_keeps_focus_inside_the_list() {
    let mut m = content(true);
    m.set_command_actions_enabled(true);
    // Focus the last row (wrapping Up from the first), then drop the eight
    // command rows: focus must still name a visible item.
    m.handle_input(OverlayInput::Up);
    let last = m.item_count() - 1;
    assert_eq!(m.focused, last);
    m.set_command_actions_enabled(false);
    assert!(
        m.focused < m.item_count(),
        "focus {} is past the {}-item list",
        m.focused,
        m.item_count()
    );
    assert!(matches!(
        m.handle_input(OverlayInput::Activate),
        ContextMenuOutcome::Activate(_)
    ));
    let _ = m.focused_body_row();
}

#[test]
fn a_composition_change_keeps_focus_on_the_same_visible_item() {
    let mut m = content(true);
    m.set_command_actions_enabled(true);
    while focused_item(&m) != Some(ContextMenuItem::Settings) {
        m.handle_input(OverlayInput::Down);
    }
    m.set_command_actions_enabled(false);
    assert_eq!(focused_item(&m), Some(ContextMenuItem::Settings));
    m.set_broadcast(false, true);
    assert_eq!(
        focused_item(&m),
        Some(ContextMenuItem::Settings),
        "adding Stop Broadcast above keeps focus on Settings"
    );
}

#[test]
fn hidden_items_do_not_widen_the_menu() {
    let mut m = open_on(ContextMenuSurface::TabStripEmpty, false, false);
    m.set_accelerators(accelerators_with(&[(ContextMenuItem::NewTab, "Ctrl+T")]));
    let width = m.menu_width();
    // Split Right is not on the empty-strip menu; neither its long
    // accelerator nor the long hidden labels may widen the box.
    m.set_accelerators(accelerators_with(&[
        (ContextMenuItem::NewTab, "Ctrl+T"),
        (
            ContextMenuItem::SplitColumns,
            "Ctrl+Shift+Alt+Super+PageDown",
        ),
    ]));
    assert_eq!(m.menu_width(), width, "a hidden accelerator is ignored");
    let longest_visible = m
        .visible_items()
        .iter()
        .map(|item| item.label().chars().count())
        .max()
        .expect("items");
    assert_eq!(
        width,
        longest_visible + ACCELERATOR_GAP + "Ctrl+T".len() + 4,
        "the width is the longest visible label plus the visible accelerator"
    );
}

#[test]
fn exhausted_rail_clearance_falls_back_to_a_usable_whole_grid_box() {
    for (columns, left, right) in [
        (20, 20, 0),
        (20, 0, 20),
        (20, 18, 0),
        (20, 9, 9),
        (20, 40, 40),
    ] {
        let mut m = content(true);
        m.set_rail_clearance(left, right);
        let rect = m.rect(columns, 40);
        assert!(
            rect.body_width > 0 && rect.body_left + rect.body_width <= columns,
            "{columns} cols, reserves {left}/{right}: body {}+{} outside the grid",
            rect.body_left,
            rect.body_width
        );
        assert!(rect.left + rect.width <= columns);
    }
    // A reserve that leaves room keeps the box beside the rail.
    let mut m = content(true);
    m.set_rail_clearance(16, 0);
    let rect = m.rect(80, 40);
    assert!(rect.left >= 16, "box stays clear of a 16-column rail");
}

#[test]
fn tiny_grids_keep_an_empty_body_inside_the_box() {
    for (columns, rows) in [(0, 0), (1, 1), (3, 2), (4, 10), (40, 2)] {
        let m = content(true);
        let rect = m.rect(columns, rows);
        if rect.body_width == 0 || rect.body_height == 0 {
            assert_eq!(
                (rect.body_width, rect.body_height),
                (0, 0),
                "{columns}x{rows}: a body with no width or height is empty"
            );
        }
        assert!(
            rect.body_left < rect.left + rect.width.max(1)
                && rect.body_top < rect.top + rect.height.max(1),
            "{columns}x{rows}: body origin ({}, {}) outside the box {rect:?}",
            rect.body_top,
            rect.body_left
        );
    }
}

#[test]
fn signature_changes_with_command_rows_accelerators_clearance_and_slot() {
    let plain = content(true);
    let mut commands = content(true);
    commands.set_command_actions_enabled(true);
    assert_ne!(plain.rows(), commands.rows());
    assert_ne!(plain.render_signature(), commands.render_signature());

    let mut accel = content(true);
    accel.set_accelerators(accelerators_with(&[(ContextMenuItem::NewTab, "Ctrl+T")]));
    let mut remapped = content(true);
    remapped.set_accelerators(accelerators_with(&[(ContextMenuItem::NewTab, "Alt+T")]));
    assert_ne!(accel.rows(), remapped.rows());
    assert_ne!(accel.render_signature(), remapped.render_signature());

    let mut clear = content(true);
    clear.set_rail_clearance(16, 0);
    assert_ne!(plain.rect(80, 40), clear.rect(80, 40));
    assert_ne!(plain.render_signature(), clear.render_signature());

    let mut first = open_on(ContextMenuSurface::WorkspaceSlot(0), false, false);
    first.set_workspace_count(3);
    let mut second = open_on(ContextMenuSurface::WorkspaceSlot(1), false, false);
    second.set_workspace_count(3);
    assert_ne!(first.rows(), second.rows(), "Move Up shows only on slot 1");
    assert_ne!(first.render_signature(), second.render_signature());
}

#[test]
fn equal_presentations_keep_equal_signatures() {
    let mut a = content(true);
    let mut b = content(true);
    for m in [&mut a, &mut b] {
        m.set_command_actions_enabled(true);
        m.set_accelerators(accelerators_with(&[(ContextMenuItem::NewTab, "Ctrl+T")]));
        m.set_rail_clearance(4, 0);
    }
    assert_eq!(a.render_signature(), b.render_signature());
    assert_eq!(
        ContextMenuUi::new().render_signature(),
        ContextMenuSignature::default(),
        "a never-opened menu keeps the default signature"
    );
}

#[test]
fn production_body_rows_match_the_reference_and_invert_on_every_surface() {
    // The single-pane with-selection reference against the production layout.
    let reference = content(true);
    for row in 0..CONTEXT_MENU_BODY_ROWS {
        assert_eq!(
            reference.body_row_to_item_index(row),
            body_row_to_item(row),
            "body row {row}"
        );
    }
    for index in 0..reference.item_count() {
        let mut m = reference.clone();
        m.focused = index;
        assert_eq!(
            m.focused_body_row(),
            item_to_body_row(index),
            "item {index}"
        );
    }
    // Every surface: each item row maps to its item, focusing that item puts
    // it back on the same row, and separators map to nothing.
    let surfaces = [
        ContextMenuSurface::Content,
        ContextMenuSurface::TabSlot(SessionToken(1)),
        ContextMenuSurface::TabStripEmpty,
        ContextMenuSurface::WorkspaceSlot(1),
        ContextMenuSurface::WorkspaceRailEmpty,
        ContextMenuSurface::PaneDivider,
    ];
    for surface in surfaces {
        for (copy, multi_pane) in [(false, false), (true, false), (true, true)] {
            let mut m = open_on(surface, copy, multi_pane);
            m.set_workspace_count(3);
            let layout = m.body_layout();
            assert_eq!(layout.len(), m.body_row_count());
            assert_eq!(m.rows().len(), layout.len(), "{surface:?}: rows and layout");
            for (row, slot) in layout.iter().enumerate() {
                let index = m.body_row_to_item_index(row);
                match slot {
                    None => assert_eq!(index, None, "{surface:?} separator row {row}"),
                    Some(item) => {
                        let index = index.expect("an item row maps to an item");
                        assert_eq!(m.visible_items()[index], *item);
                        m.focused = index;
                        assert_eq!(m.focused_body_row(), row, "{surface:?} item row {row}");
                    }
                }
            }
        }
    }
}
