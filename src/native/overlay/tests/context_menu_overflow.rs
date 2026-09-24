// SPDX-License-Identifier: GPL-3.0-only
//! Overflow behavior for the right-click context menu. These tests drive the
//! headless overlay and App paths with synthetic events.

use super::*;
use crate::native::test_support::headless_app_with;
use crate::native::{App, NativeOptions};
use crate::settings::Settings;
use crate::text::CellSize;
use winit::dpi::PhysicalPosition;
use winit::event::{MouseButton as WinitMouseButton, MouseScrollDelta};

const SHORT_COLUMNS: usize = 44;
const SHORT_ROWS: usize = 8;
const CELL_HEIGHT: u32 = 16;
const PIXEL_HALF_ROW: f64 = CELL_HEIGHT as f64 * 1.5;

fn open_context_menu(overlay: &mut OverlayUi) {
    overlay.open_context_menu(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        None,
        false,
        None,
        std::array::from_fn(|_| None),
    );
}

fn context_menu() -> OverlayUi {
    let mut overlay = OverlayUi::default();
    open_context_menu(&mut overlay);
    overlay
}

fn app_with_context_menu() -> App {
    let dimensions = Dimensions::new(SHORT_COLUMNS, SHORT_ROWS);
    let (mut app, _) = headless_app_with(NativeOptions::default(), dimensions, Settings::default());
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: CELL_HEIGHT,
        baseline: 0,
    });
    app.set_pointer_cell_for_test(0, 0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test(), "context menu opened");
    app
}

fn focus(app: &App) -> usize {
    usize::from(app.overlay_signature_for_test().context_menu.focused)
}

fn overlay_focus(overlay: &OverlayUi) -> usize {
    usize::from(overlay.render_signature().context_menu.focused)
}

fn focused_body_row(overlay: &OverlayUi) -> usize {
    overlay
        .context_menu
        .rows()
        .iter()
        .position(|row| {
            matches!(
                row,
                crate::native::context_menu_ui::ContextMenuRow::Item { focused: true, .. }
            )
        })
        .expect("one context-menu item is focused")
}

#[test]
fn overflow_arrows_scroll_the_window_and_clamp_focus() {
    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);
    assert!(rect.body_height > 1, "short menu has multiple visible rows");

    let top_mark = CellPoint {
        row: rect.top,
        column: rect.left + rect.width / 2,
    };
    let bottom_mark = CellPoint {
        row: rect.top + rect.height - 1,
        column: rect.left + rect.width / 2,
    };
    let initial_focus = overlay_focus(&overlay);
    overlay.handle_pointer(
        OverlayPointer::Press {
            cell: top_mark,
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(overlay_focus(&overlay), initial_focus);
    assert_eq!(overlay.context_menu.scroll_offset(rect.body_height), 0);

    // Focus is at the top. The bottom mark moves the window and clamps that
    // focus into the new visible range rather than advancing it with the view.
    for expected_offset in 1..=2 {
        overlay.handle_pointer(
            OverlayPointer::Press {
                cell: bottom_mark,
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(
            overlay.context_menu.scroll_offset(rect.body_height),
            expected_offset,
            "each bottom-mark click scrolls exactly one row"
        );
        let focus_row = focused_body_row(&overlay);
        assert!(
            expected_offset <= focus_row && focus_row < expected_offset + rect.body_height,
            "focus remains visible after arrow scroll (focus={focus_row}, offset={expected_offset})"
        );
    }

    overlay.handle_pointer(
        OverlayPointer::Press {
            cell: top_mark,
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(overlay.context_menu.scroll_offset(rect.body_height), 1);

    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(
        outcome,
        OverlayOutcome::ContextMenuCopy,
        "body-row presses still activate their menu item"
    );
}

#[test]
fn hover_on_a_visible_non_bottom_row_does_not_move_scrolled_window() {
    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);

    // Reach a scrolled state with keyboard focus at the visible bottom row.
    for _ in 0..64 {
        overlay.handle_input(OverlayInput::Down);
        let offset = overlay.context_menu.scroll_offset(rect.body_height);
        if offset > 0 && focused_body_row(&overlay) == offset + rect.body_height - 1 {
            break;
        }
    }
    let before = overlay.context_menu.scroll_offset(rect.body_height);
    assert!(before > 0, "test starts with the menu scrolled");

    overlay.handle_pointer(
        OverlayPointer::Move {
            cell: CellPoint {
                row: rect.body_top + 1,
                column: rect.body_left,
            },
            x_in_body: None,
        },
        rect,
    );

    assert_eq!(
        overlay.context_menu.scroll_offset(rect.body_height),
        before,
        "hover changes focus within the visible window without moving it"
    );
}

#[test]
fn keyboard_up_keeps_window_until_focus_reaches_its_top_row() {
    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);

    for _ in 0..64 {
        overlay.commit_context_menu_scroll(rect.body_height);
        overlay.handle_input(OverlayInput::Down);
        let offset = overlay.context_menu.scroll_offset(rect.body_height);
        if offset > 0 && focused_body_row(&overlay) == offset + rect.body_height - 1 {
            break;
        }
    }
    let initial_offset = overlay.context_menu.scroll_offset(rect.body_height);
    assert!(initial_offset > 0, "test starts with a scrolled window");

    while focused_body_row(&overlay) > overlay.context_menu.scroll_offset(rect.body_height) {
        let before = overlay.context_menu.scroll_offset(rect.body_height);
        overlay.commit_context_menu_scroll(rect.body_height);
        overlay.handle_input(OverlayInput::Up);
        assert_eq!(
            overlay.context_menu.scroll_offset(rect.body_height),
            before,
            "Up moves focus inside the window without moving the window"
        );
    }

    let before_top = overlay.context_menu.scroll_offset(rect.body_height);
    assert_eq!(focused_body_row(&overlay), before_top);
    overlay.commit_context_menu_scroll(rect.body_height);
    overlay.handle_input(OverlayInput::Up);
    assert_eq!(
        overlay.context_menu.scroll_offset(rect.body_height),
        before_top - 1,
        "Up scrolls the window only after focus passes its top row"
    );
}

#[test]
fn reopening_context_menu_resets_its_scroll_window() {
    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);
    let bottom_mark = CellPoint {
        row: rect.top + rect.height - 1,
        column: rect.left + rect.width / 2,
    };
    for expected_offset in 1..=3 {
        overlay.handle_pointer(
            OverlayPointer::Press {
                cell: bottom_mark,
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(
            overlay.context_menu.scroll_offset(rect.body_height),
            expected_offset
        );
    }

    overlay.handle_input(OverlayInput::Close);
    open_context_menu(&mut overlay);
    let reopened_rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);
    assert_eq!(
        overlay
            .context_menu
            .scroll_offset(reopened_rect.body_height),
        0
    );
    assert_eq!(overlay_focus(&overlay), 0);
}

#[test]
fn pixel_scroll_accumulates_rows_and_preserves_the_post_emit_remainder() {
    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);
    let pointer = None;

    assert_eq!(
        overlay.context_menu_wheel(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -PIXEL_HALF_ROW)),
            CELL_HEIGHT,
            pointer,
            rect,
        ),
        0,
        "half a row emits no step"
    );
    assert_eq!(
        overlay.context_menu_wheel(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -PIXEL_HALF_ROW)),
            CELL_HEIGHT,
            pointer,
            rect,
        ),
        1,
        "two halves emit one step"
    );
    assert_eq!(overlay.context_menu.scroll_offset(rect.body_height), 1);

    // A 1.5-row event emits one row and retains the remaining half. The next
    // half-row completes that retained remainder and emits the following step.
    assert_eq!(
        overlay.context_menu_wheel(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -(PIXEL_HALF_ROW * 3.0))),
            CELL_HEIGHT,
            pointer,
            rect,
        ),
        1
    );
    assert_eq!(overlay.context_menu.scroll_offset(rect.body_height), 2);
    assert_eq!(
        overlay.context_menu_wheel(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -PIXEL_HALF_ROW)),
            CELL_HEIGHT,
            pointer,
            rect,
        ),
        1
    );
    assert_eq!(overlay.context_menu.scroll_offset(rect.body_height), 3);
}

#[test]
fn one_line_notch_scrolls_context_menu_by_one_row() {
    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);

    assert_eq!(
        overlay.context_menu_wheel(
            MouseScrollDelta::LineDelta(0.0, -1.0),
            CELL_HEIGHT,
            None,
            rect,
        ),
        1,
        "one physical wheel notch moves one row, not three"
    );
    assert_eq!(overlay.context_menu.scroll_offset(rect.body_height), 1);
}

#[test]
fn wheel_focus_follows_pointer_and_repeated_same_cell_moves_are_stable() {
    let mut overlay = context_menu();
    let rect = overlay.context_menu.rect(SHORT_COLUMNS, SHORT_ROWS);
    let pointer_cell = CellPoint {
        row: rect.body_top,
        column: rect.body_left,
    };

    assert_eq!(
        overlay.context_menu_wheel(
            MouseScrollDelta::LineDelta(0.0, -1.0),
            CELL_HEIGHT,
            Some(pointer_cell),
            rect,
        ),
        1,
        "wheel scrolls one row"
    );
    let wheeled_offset = overlay.context_menu.scroll_offset(rect.body_height);
    let wheeled_focus = overlay_focus(&overlay);
    assert_eq!(wheeled_offset, 1);
    assert_eq!(
        focused_body_row(&overlay),
        wheeled_offset,
        "focus follows the item under the pointer after scrolling"
    );

    for _ in 0..2 {
        overlay.handle_pointer(
            OverlayPointer::Move {
                cell: pointer_cell,
                x_in_body: None,
            },
            rect,
        );
        assert_eq!(
            overlay.context_menu.scroll_offset(rect.body_height),
            wheeled_offset,
            "same-cell hover leaves the window unchanged"
        );
        assert_eq!(
            overlay_focus(&overlay),
            wheeled_focus,
            "same-cell hover leaves focus unchanged"
        );
    }
}

#[test]
fn app_line_notch_moves_context_menu_window_by_one_row() {
    let mut app = app_with_context_menu();
    app.handle_overlay_pointer_wheel(MouseScrollDelta::LineDelta(0.0, -1.0));
    assert!(app.context_menu_open_for_test());
    assert_ne!(focus(&app), 0, "wheel leaves focus on a visible menu item");
}
