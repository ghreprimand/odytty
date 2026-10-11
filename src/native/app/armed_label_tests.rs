// SPDX-License-Identifier: GPL-3.0-only
//! An armed chrome drag lifts the grabbed label out of its slot and shows it
//! at the drop position. The grabbed slot here is an INACTIVE one among several
//! distinguishable titles, so ordinary active-slot emphasis cannot satisfy the
//! assertions.

use super::*;
use crate::native::test_support::{headless_app_with, headless_writer};

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};

fn terminal() -> Arc<Mutex<Terminal>> {
    Arc::new(Mutex::new(Terminal::new(80, 24)))
}

fn app() -> App {
    headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    )
    .0
}

/// The glyphs of the first run spelling `text` in `row`.
fn run<'a, G>(row: &'a [G], text: &str, ch: impl Fn(&G) -> char) -> Option<&'a [G]> {
    let want: Vec<char> = text.chars().collect();
    row.windows(want.len())
        .find(|window| window.iter().map(&ch).eq(want.iter().copied()))
}

fn arm_top(app: &mut App, origin: usize, drop_idx: usize) {
    let mut drag = TopTabDrag::new(origin, 0.0, 0.0);
    assert!(drag.update_arm(pointer::CHROME_DRAG_THRESHOLD_PX + 1.0, 0.0));
    drag.drop_idx = drop_idx;
    app.top_tab_drag = Some(drag);
}

fn top_row(app: &App) -> Vec<tab_bar::TabBarGlyph> {
    app.render_top_bar_widget(80, 0.0, CELL, WindowPadding::ZERO)
        .glyphs
}

#[test]
fn an_armed_inactive_tab_label_shows_whole_and_lifted_at_the_drop_position() {
    let mut app = app();
    for _ in 0..2 {
        app.push_headless_session_for_test(terminal(), headless_writer(), Dimensions::new(80, 24));
    }
    app.set_session_tab_title_for_test(0, "ActiveTab");
    app.set_session_tab_title_for_test(1, "GrabbedTop");
    app.set_session_tab_title_for_test(2, "LastTab");
    assert_eq!(app.sessions.active_tab(), 0, "the grabbed slot is inactive");

    // At rest the grabbed label is an ordinary inactive label: not bold.
    let rest = top_row(&app);
    let at_rest = run(&rest, "GrabbedTop", |g| g.ch).expect("label at rest");
    assert!(at_rest.iter().all(|g| !g.attrs.bold()), "inactive at rest");
    let rest_col = at_rest[0].col;

    arm_top(&mut app, 1, 3);
    let armed = top_row(&app);
    let label = run(&armed, "GrabbedTop", |g| g.ch).expect("armed label drawn whole");
    assert!(
        label.iter().all(|g| g.attrs.bold()),
        "every glyph of the lifted label carries the emphasis"
    );
    // The label moved to the drop position, past the slot it was dropped on.
    let last = run(&armed, "LastTab", |g| g.ch).expect("shifted neighbour");
    assert!(last[0].col < label[0].col, "dropped after LastTab");
    assert!(label[0].col > rest_col, "the label left its resting slot");
    assert!(
        last.iter().all(|g| !g.attrs.bold()),
        "a neighbour that only shifted is not lifted"
    );
    // The lifted slot wears the active fill, as the active tab does.
    let active = run(&armed, "ActiveTab", |g| g.ch).expect("active label");
    assert_eq!(
        label[0].attrs.background, active[0].attrs.background,
        "lifted slot uses the active slot fill"
    );
}

#[test]
fn an_armed_inactive_workspace_label_shows_whole_and_lifted_at_the_drop_position() {
    let mut app = app();
    for _ in 0..2 {
        app.push_headless_workspace_for_test(
            terminal(),
            headless_writer(),
            Dimensions::new(80, 24),
        );
    }
    app.rename_workspace_for_test(0, "FirstWs");
    app.rename_workspace_for_test(1, "GrabbedRail");
    app.rename_workspace_for_test(2, "ActiveWs");
    assert_eq!(app.sessions.active_workspace_index(), 2);

    let cols = 16;
    let render = |app: &App| {
        app.render_rail_widget(cols, 24, [0.0, 0.0], CELL, RailSide::Left)
            .glyphs
    };
    let rows_of = |glyphs: &[tab_rail::TabRailGlyph], text: &str| -> Option<usize> {
        glyphs
            .chunks(cols)
            .position(|row| run(row, text, |g| g.ch).is_some())
    };

    let rest = render(&app);
    let rest_row = rows_of(&rest, "GrabbedRail").expect("label at rest");
    let at_rest = run(
        rest.chunks(cols).nth(rest_row).unwrap(),
        "GrabbedRail",
        |g| g.ch,
    )
    .expect("run at rest");
    assert!(at_rest.iter().all(|g| !g.attrs.bold()), "inactive at rest");

    let mut drag = RailWorkspaceDrag::new(1, 0.0, 0.0);
    assert!(drag.update_arm(0.0, pointer::CHROME_DRAG_THRESHOLD_PX + 1.0));
    drag.drop_idx = 0;
    app.rail_ws_drag = Some(drag);

    let armed = render(&app);
    let grabbed_row = rows_of(&armed, "GrabbedRail").expect("armed label drawn whole");
    let first_row = rows_of(&armed, "FirstWs").expect("shifted neighbour");
    assert!(grabbed_row < first_row, "dropped before FirstWs");
    assert!(grabbed_row < rest_row, "the label left its resting row");
    let label = run(
        armed.chunks(cols).nth(grabbed_row).unwrap(),
        "GrabbedRail",
        |g| g.ch,
    )
    .expect("lifted run");
    assert!(
        label.iter().all(|g| g.attrs.bold()),
        "every glyph of the lifted label carries the emphasis"
    );
    let first = run(armed.chunks(cols).nth(first_row).unwrap(), "FirstWs", |g| {
        g.ch
    })
    .expect("neighbour run");
    assert!(first.iter().all(|g| !g.attrs.bold()));
}
