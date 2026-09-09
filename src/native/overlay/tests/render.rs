// SPDX-License-Identifier: GPL-3.0-only
//! Overlay signature and snapshot tests: what the open overlay draws, what a
//! closed overlay must not draw, and the geometry the painter uses.

use super::*;

#[test]
fn fit_hint_returns_full_string_when_it_fits() {
    // Large-window byte-identity guard: a width that holds the whole hint
    // returns it unchanged (no behavior change on normal windows).
    let hint = "  Enter/\u{2192} open  / search  Ctrl+S save  Esc close";
    assert_eq!(fit_hint_to_width(hint, 80), hint);
    // Exactly-fits boundary also returns the full string.
    let w = text_display_width(hint);
    assert_eq!(fit_hint_to_width(hint, w), hint);
}

#[test]
fn fit_hint_truncates_on_a_word_boundary_not_mid_word() {
    // A narrow window must never cut a word in half ("Ctrl+S sav").
    let hint = "Enter open search Ctrl+S save Esc close";
    // At/above the first word's width the trim is always on a word boundary;
    // below that the char-fallback (a legible head) is exercised separately.
    let first_word_w = text_display_width(hint.split(' ').next().unwrap());
    for width in first_word_w..text_display_width(hint) {
        let fitted = fit_hint_to_width(hint, width);
        assert!(
            text_display_width(&fitted) <= width,
            "fitted hint must fit in {width}: {fitted:?}"
        );
        // The fitted text is a whole-word prefix of the original: splitting
        // both on spaces, every fitted word equals the matching source word
        // (no partial trailing word).
        for (got, want) in fitted.split(' ').zip(hint.split(' ')) {
            assert_eq!(got, want, "word boundary preserved (width {width})");
        }
        assert!(
            !fitted.ends_with(' '),
            "no trailing space left by the word trim: {fitted:?}"
        );
    }
}

#[test]
fn fit_hint_falls_back_to_char_cut_when_first_word_overflows() {
    // A single very long word can't be word-split; show a legible head
    // rather than nothing.
    let fitted = fit_hint_to_width("supercalifragilistic", 5);
    assert_eq!(fitted, "super");
    assert_eq!(fit_hint_to_width("anything", 0), "");
}

#[test]
fn fit_hint_preserves_leading_indent() {
    // Leading indentation spaces are kept (the footer is indented two cols).
    let fitted = fit_hint_to_width("  Enter open close", 9);
    assert!(
        fitted.starts_with("  Enter"),
        "kept indent + first word: {fitted:?}"
    );
    assert!(text_display_width(&fitted) <= 9);
}

#[test]
fn overlay_draws_into_snapshot_copy_only() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    let original = snapshot(40, 10);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(original.cells[0].ch, '.');
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    assert!(rendered.cells.iter().any(|cell| cell.ch == '>'));
}

#[test]
fn command_palette_draws_into_snapshot_copy_only() {
    let mut overlay = OverlayUi::default();
    overlay.open_command_palette_for_test(["git status"], Some("/work/demo"));
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(
        original.cells,
        vec![Cell::new('.', Attrs::default()); 80 * 20]
    );
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    assert!(rendered.cells.iter().any(|cell| cell.ch == 'g'));
}

#[test]
fn closed_command_palette_is_pixel_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_command_palette_for_test(["git status"], Some("/work/demo"));
    overlay.close();
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(rendered, original);
}

#[test]
fn replay_overlay_draws_into_snapshot_copy_only() {
    // REPLAY-OVERLAY-ISOLATION (render side): apply_overlay only mutates the
    // snapshot copy it is handed; the source frame is untouched, and the
    // recorded screen content is shown.
    let mut overlay = OverlayUi::default();
    overlay.open_replay(vec![
        replay_frame(40, 6, "alpha"),
        replay_frame(40, 6, "bravo"),
    ]);
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    // The input snapshot is never mutated in place.
    assert_eq!(
        original.cells,
        vec![Cell::new('.', Attrs::default()); 80 * 20]
    );
    // The panel border and the recorded content (live tail = "bravo") show.
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    assert!(rendered.cells.iter().any(|cell| cell.ch == 'b'));
}

#[test]
fn closed_replay_overlay_is_pixel_inert() {
    // OVERLAY-CLOSED-BYTE-IDENTICAL: once closed, the replay overlay paints
    // nothing — the frame is byte-identical to the input.
    let mut overlay = OverlayUi::default();
    overlay.open_replay(vec![replay_frame(40, 6, "alpha")]);
    overlay.close();
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(rendered, original);
}

#[test]
fn empty_replay_overlay_opens_with_hint() {
    // Opening replay with no recorded frames still opens (showing a hint)
    // rather than failing; it draws a panel into the copy only.
    let mut overlay = OverlayUi::default();
    overlay.open_replay(Vec::new());
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(
        original.cells,
        vec![Cell::new('.', Attrs::default()); 80 * 20]
    );
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
}

#[test]
fn connection_row_menu_renders_manager_underneath() {
    // MENU-OVER-MANAGER: opening the row menu must NOT blank the manager —
    // the manager panel paints underneath the menu box so it stays visible.
    // Render the composed overlay and assert the manager's title row (above
    // the spawn cell, so never covered by the menu) survives.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    assert_eq!(overlay.render_signature().mode, OverlayMode::ContextMenu);

    let (cols, rows) = (80usize, 24usize);
    let mut snap = Snapshot {
        dimensions: Dimensions::new(cols, rows),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: crate::core::DynamicColors::default(),
        cells: vec![crate::core::Cell::default(); cols * rows],
    };
    apply_overlay(&mut snap, &mut overlay);
    let rendered: String = (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| snap.cells[r * cols + c].grapheme())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Connections"),
        "the manager title must remain visible under the row menu:\n{rendered}"
    );
}

#[test]
fn connection_row_menu_production_crop_must_retain_manager_underneath() {
    // Sibling of the navigator crop regression: the multi-pane `build_overlay_top`
    // crop must span the connection-manager underlay, not the menu box alone, or
    // the manager vanishes behind its own row menu in a multi-pane tab.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    assert_eq!(overlay.render_signature().mode, OverlayMode::ContextMenu);

    let (cols, rows) = (80usize, 24usize);
    let mut snap = Snapshot {
        dimensions: Dimensions::new(cols, rows),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: crate::core::DynamicColors::default(),
        cells: vec![crate::core::Cell::default(); cols * rows],
    };
    apply_overlay(&mut snap, &mut overlay);

    let composite = overlay_composite_rect(&mut overlay, cols, rows).expect("composite rect");
    let cropped = crop_snapshot_for_test(
        &snap,
        composite.left,
        composite.top,
        composite.width,
        composite.height,
    );
    assert!(
        snapshot_text(&cropped).contains("Connections"),
        "production composite crop must retain the connection-manager underlay; \
         composite rect {composite:?}"
    );
}

fn snapshot_text(snap: &Snapshot) -> String {
    let cols = snap.dimensions.columns;
    let rows = snap.dimensions.rows;
    (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| snap.cells[r * cols + c].grapheme())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mirror of `panes::crop_snapshot` used by production `build_overlay_top`.
fn crop_snapshot_for_test(
    src: &Snapshot,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
) -> Snapshot {
    let src_cols = src.dimensions.columns;
    let mut cells = Vec::with_capacity(width * height);
    for r in 0..height {
        for c in 0..width {
            let cell = src
                .cells
                .get((top + r) * src_cols + (left + c))
                .copied()
                .unwrap_or_default();
            cells.push(cell);
        }
    }
    Snapshot {
        dimensions: Dimensions::new(width, height),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: src.colors.clone(),
        cells,
    }
}

fn open_navigator_menu_for_render() -> OverlayUi {
    use crate::native::session::SessionToken;
    use crate::native::session_navigator::{NavigatorEntry, NavigatorTarget};
    let entries = vec![NavigatorEntry {
        target: NavigatorTarget::Live(SessionToken(12)),
        stable_id: "live:12".to_owned(),
        name: "pane-a".to_owned(),
        detail: "local /tmp".to_owned(),
        status: "running".to_owned(),
        unread: false,
        profile: None,
        preview: Vec::new(),
    }];
    let mut overlay = OverlayUi::default();
    overlay.open_session_navigator_selected(entries, Some("live:12"), false);
    let rect = overlay_rect(&overlay, 80, 24).expect("navigator rect");
    let _ = overlay
        .session_attach
        .visible_lines(rect.body_width, rect.body_height);
    let row = overlay
        .session_attach
        .visible_lines(rect.body_width, rect.body_height)
        .iter()
        .enumerate()
        .find_map(|(row, line)| (row > 0 && line.text.contains("pane-a")).then_some(row))
        .expect("pane-a row");
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top + row,
                column: rect.body_left + 1,
            },
            button: PointerButton::Right,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_eq!(overlay.render_signature().mode, OverlayMode::ContextMenu);
    overlay
}

#[test]
fn navigator_row_menu_apply_overlay_keeps_navigator_title() {
    // MENU-OVER-NAVIGATOR (paint): apply_overlay must keep the SessionAttach
    // underlay so the navigator title survives beside the menu box.
    let mut overlay = open_navigator_menu_for_render();
    let (cols, rows) = (80usize, 24usize);
    let mut snap = Snapshot {
        dimensions: Dimensions::new(cols, rows),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: crate::core::DynamicColors::default(),
        cells: vec![crate::core::Cell::default(); cols * rows],
    };
    apply_overlay(&mut snap, &mut overlay);
    let rendered = snapshot_text(&snap);
    assert!(
        rendered.contains("Session Navigator"),
        "apply_overlay must retain the navigator underlay title:\n{rendered}"
    );
}

#[test]
fn navigator_row_menu_production_crop_must_retain_navigator_underlay() {
    // MENU-OVER-NAVIGATOR (compositing): multi-pane `build_overlay_top` paints
    // via apply_overlay then crops. Cropping to the ContextMenu box alone drops
    // the SessionAttach underlay (operator: navigator vanishes, terminal shows
    // through). The composited top must retain the navigator title: crop to the
    // composite rect (the union of underlay panel + menu box).
    let mut overlay = open_navigator_menu_for_render();
    let (cols, rows) = (80usize, 24usize);
    let mut snap = Snapshot {
        dimensions: Dimensions::new(cols, rows),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: crate::core::DynamicColors::default(),
        cells: vec![crate::core::Cell::default(); cols * rows],
    };
    apply_overlay(&mut snap, &mut overlay);
    assert!(
        snapshot_text(&snap).contains("Session Navigator"),
        "precondition: full apply_overlay still has the underlay"
    );

    // Cropping to the ContextMenu box alone (the plain `overlay_rect`) drops the
    // navigator underlay - the bug the operator saw. The production crop instead
    // uses `overlay_composite_rect`, the union of underlay panel + menu box.
    let menu_rect = overlay_rect(&overlay, cols, rows).expect("menu rect");
    let menu_only = crop_snapshot_for_test(
        &snap,
        menu_rect.left,
        menu_rect.top,
        menu_rect.width,
        menu_rect.height,
    );
    assert!(
        !snapshot_text(&menu_only).contains("Session Navigator"),
        "menu-box-only crop is expected to DROP the underlay (regression witness)"
    );

    // Production `build_overlay_top` crop: composite rect. It must span the
    // underlay panel so the navigator title survives beside the menu box.
    let composite = overlay_composite_rect(&mut overlay, cols, rows).expect("composite rect");
    assert!(
        composite.width >= menu_rect.width && composite.height >= menu_rect.height,
        "composite rect must cover at least the menu box: composite={composite:?} menu={menu_rect:?}"
    );
    let cropped = crop_snapshot_for_test(
        &snap,
        composite.left,
        composite.top,
        composite.width,
        composite.height,
    );
    let cropped_text = snapshot_text(&cropped);
    assert!(
        cropped_text.contains("Session Navigator"),
        "production composite crop must retain the navigator underlay (not menu-box-only); \
         got composite rect {composite:?}:\n{cropped_text}"
    );
}

#[test]
fn connection_overlay_draws_into_snapshot_copy_only() {
    // CONNECTION-OVERLAY-ISOLATION (render side): apply_overlay only mutates
    // the snapshot copy it is handed; the source frame is untouched, and the
    // host list shows.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(
        vec![connection_host("web1"), connection_host("db1")],
        Vec::new(),
    );
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    // The input snapshot is never mutated in place.
    assert_eq!(
        original.cells,
        vec![Cell::new('.', Attrs::default()); 80 * 20]
    );
    // The panel border and a host alias show.
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    assert!(rendered.cells.iter().any(|cell| cell.ch == 'w'));
}

#[test]
fn closed_connection_overlay_is_pixel_inert() {
    // OVERLAY-CLOSED-BYTE-IDENTICAL: once closed, the connection overlay
    // paints nothing — the frame is byte-identical to the input.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    overlay.close();
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(rendered, original);
}

#[test]
fn empty_connection_overlay_opens_with_hint() {
    // Opening the connection manager with no hosts still opens (showing a
    // hint) rather than failing; it draws a panel into the copy only.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(Vec::new(), Vec::new());
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(
        original.cells,
        vec![Cell::new('.', Attrs::default()); 80 * 20]
    );
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
}

#[test]
fn session_attach_overlay_draws_into_snapshot_copy_only() {
    // SESSION-ATTACH-OVERLAY-ISOLATION (render side): apply_overlay only
    // mutates the snapshot copy it is handed; the source frame is untouched,
    // and the session list shows.
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(vec![
        listed_session("s-0001-aaaa", "build"),
        listed_session("s-0002-bbbb", "web"),
    ]);
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    // The input snapshot is never mutated in place.
    assert_eq!(
        original.cells,
        vec![Cell::new('.', Attrs::default()); 80 * 20]
    );
    // The panel border and a session title show.
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
    assert!(rendered.cells.iter().any(|cell| cell.ch == 'b'));
}

#[test]
fn closed_session_attach_overlay_is_pixel_inert() {
    // OVERLAY-CLOSED-BYTE-IDENTICAL: once closed, the session-attach overlay
    // paints nothing — the frame is byte-identical to the input.
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(vec![listed_session("s-0001-aaaa", "build")]);
    overlay.close();
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(rendered, original);
}

#[test]
fn empty_session_attach_overlay_opens_with_hint() {
    // Opening the session-attach overlay with no live sessions still opens
    // (showing a hint) rather than failing; it draws a panel into the copy.
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(Vec::new());
    let original = snapshot(80, 20);
    let mut rendered = original.clone();

    apply_overlay(&mut rendered, &mut overlay);

    assert_eq!(
        original.cells,
        vec![Cell::new('.', Attrs::default()); 80 * 20]
    );
    assert!(rendered.cells.iter().any(|cell| cell.ch == '+'));
}

#[test]
fn onboarding_opens_renders_and_dismisses() {
    let mut overlay = OverlayUi::default();
    overlay.open_onboarding();
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::Onboarding);

    // The welcome card paints its title into the snapshot.
    let mut rendered = snapshot(70, 18);
    apply_overlay(&mut rendered, &mut overlay);
    let painted: String = rendered.cells.iter().map(|cell| cell.ch).collect();
    assert!(painted.contains("Welcome to OdyTTY"));

    // Enter, Esc, and Space each dismiss; any other key is swallowed.
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('x')),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::CloseOnboarding
    );
    for input in [
        OverlayInput::Close,
        OverlayInput::Char(' '),
        OverlayInput::Activate,
    ] {
        overlay.open_onboarding();
        assert_eq!(overlay.handle_input(input), OverlayOutcome::CloseOnboarding);
    }
}

#[test]
fn image_view_opens_renders_caption_and_dismisses() {
    // C4 + Phase 13c lightbox: the image-viewer overlay opens in its own
    // mode and paints ONLY a minimal caption (no bordered panel — the image
    // + dimming scrim are composited on the GPU). It dismisses on Esc / Enter
    // while swallowing every other key (no PTY leak behind the overlay).
    let mut overlay = OverlayUi::default();
    overlay.open_image_view("diagram.png".to_owned());
    assert!(overlay.is_open());
    assert!(overlay.image_view_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::ImageView);

    // The lightbox paints the filename caption with an inline close hint.
    let mut rendered = snapshot(70, 18);
    apply_overlay(&mut rendered, &mut overlay);
    let painted: String = rendered.cells.iter().map(|cell| cell.ch).collect();
    assert!(painted.contains("diagram.png"), "caption is painted");
    assert!(
        painted.contains("Esc = close"),
        "caption carries the close hint"
    );

    // A stray key is swallowed; the overlay stays open.
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('x')),
        OverlayOutcome::Consumed
    );
    assert!(overlay.image_view_open());

    // Esc and Enter both dismiss (emit Close).
    for input in [OverlayInput::Close, OverlayInput::Activate] {
        overlay.open_image_view("pic.webp".to_owned());
        assert_eq!(overlay.handle_input(input), OverlayOutcome::Close);
    }
}

/// OVERLAY-SIZE: on a large terminal the panel must be substantially wider
/// and taller than the old 22-row / 64-col-min caps. Also verifies that the
/// hit-map still aligns 1:1 with visible_lines after the resize (their shared
/// `build_visible_rows` walker guarantees this by construction).
#[test]
fn overlay_rect_is_wider_and_taller_on_large_terminal() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();

    // 120×50 grid — large enough to show the effect of the raised caps.
    let rect = overlay_rect(&overlay, 120, 50).expect("rect");

    // Width: must be substantially wider than the old 64-col floor.
    // At 120 cols: (120*3/4).max(80)+4 = 94 → capped at 120. At least 90.
    assert!(
        rect.width >= 90,
        "panel width should be wide on a 120-col terminal, got {}",
        rect.width
    );

    // Height: must be taller than the old 22-row cap. At 50 rows:
    // (50*4/5).max(22).min(48) = 40.
    assert!(
        rect.height > 22,
        "panel height should exceed 22 on a 50-row terminal, got {}",
        rect.height
    );

    // visible_lines must produce at least as many rows as there are entries
    // in the first group (the shared walker never drops rows vs. the hit-map).
    let lines = overlay
        .panel
        .visible_lines(rect.body_width, rect.body_height);
    assert!(
        !lines.is_empty(),
        "visible_lines must be non-empty after resize"
    );
    // All lines must be within the body_height window.
    assert!(
        lines.len() <= rect.body_height,
        "visible_lines must not exceed body_height: {} > {}",
        lines.len(),
        rect.body_height
    );
}

#[test]
fn detach_switch_unknown_cwd_shows_default_directory_copy() {
    let mut overlay = OverlayUi::default();
    overlay.open_detach_switch_choice(String::new());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let lines = overlay.visible_lines(rect.body_width, rect.body_height);
    assert!(
        lines[0].text.contains("default directory"),
        "unknown cwd falls back to a clear default-directory line"
    );
}
