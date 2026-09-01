// SPDX-License-Identifier: GPL-3.0-only
//! Overlay input, outcome, and pointer tests: key and pointer dispatch,
//! click and key parity, dialog routing, and context-menu outcomes.

use super::*;

#[test]
fn input_mapping_covers_settings_panel_navigation() {
    assert_eq!(
        overlay_input_from_winit(&WinitKey::Named(NamedKey::PageDown), Modifiers::default()),
        Some(OverlayInput::PageDown)
    );
    assert_eq!(
        overlay_input_from_winit(&WinitKey::Named(NamedKey::Home), Modifiers::default()),
        Some(OverlayInput::Home)
    );
    assert_eq!(
        overlay_input_from_winit(&WinitKey::Named(NamedKey::End), Modifiers::default()),
        Some(OverlayInput::End)
    );
    assert_eq!(
        overlay_input_from_winit(
            &WinitKey::Character("s".into()),
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            }
        ),
        Some(OverlayInput::Save)
    );
}

#[test]
fn right_click_host_row_opens_connection_row_menu() {
    // A right-click on a saved-host row opens the connection-row context menu
    // over the still-loaded manager; the manager stays open underneath.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    assert_eq!(
        right_click_first_host(&mut overlay),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::ContextMenu);
}

#[test]
fn right_click_prompt_row_is_inert() {
    // A right-click on the query prompt (body row 0) opens no menu.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let _ = overlay
        .connections
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
    assert_eq!(overlay.render_signature().mode, OverlayMode::Connections);
}

#[test]
fn connection_row_menu_open_in_tab_connects() {
    // The first item (Open in New Tab) reuses the manager's connect path.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::Connect(host) => assert_eq!(host.alias, "web1"),
        other => panic!("expected Connect, got {other:?}"),
    }
    assert!(!overlay.is_open(), "the manager closes on connect");
}

#[test]
fn connection_row_menu_open_in_workspace_and_bind_route() {
    // Open in New Workspace (item 1) and Bind Current Workspace (item 2)
    // emit their dedicated outcomes carrying the clicked host.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    overlay.handle_input(OverlayInput::Down); // -> Open in New Workspace
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::ConnectHostInNewWorkspace(host) => assert_eq!(host.alias, "web1"),
        other => panic!("expected ConnectHostInNewWorkspace, got {other:?}"),
    }

    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    overlay.handle_input(OverlayInput::Down);
    overlay.handle_input(OverlayInput::Down); // -> Bind Current Workspace
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::BindWorkspaceToHost("web1".to_owned())
    );
}

#[test]
fn connection_row_menu_edit_opens_form_in_place() {
    // Edit (item 3) opens the P4 Edit form pre-filled; the overlay stays open
    // and switches to the ConnectionForm mode.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    for _ in 0..3 {
        overlay.handle_input(OverlayInput::Down); // -> Edit
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::ConnectionForm);
    assert!(overlay.title().contains("Edit"));
}

#[test]
fn connection_row_menu_remove_opens_confirm_then_confirms() {
    // Remove (item 4) opens the remove-host confirm; confirming emits
    // RemoveConnectionConfirmed with the clicked host.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    for _ in 0..4 {
        overlay.handle_input(OverlayInput::Down); // -> Remove
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_confirm_remove_host());
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::RemoveConnectionConfirmed(host) => assert_eq!(host.alias, "web1"),
        other => panic!("expected RemoveConnectionConfirmed, got {other:?}"),
    }
}

#[test]
fn connection_row_menu_remove_cancel_returns_to_manager() {
    // Cancelling the remove confirm returns to the manager (selection intact),
    // never to the grid.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    for _ in 0..4 {
        overlay.handle_input(OverlayInput::Down);
    }
    overlay.handle_input(OverlayInput::Activate); // -> confirm
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::Connections);
}

#[test]
fn connection_row_menu_dismiss_returns_to_manager() {
    // Esc on the menu itself returns to the manager, not the grid.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::Connections);
}

#[test]
fn ssh_config_row_menu_hides_edit_and_remove() {
    // An ssh-config-imported row's menu offers only the three non-mutating
    // actions; Down cycles through exactly them (Edit/Remove never appear).
    let mut overlay = OverlayUi::default();
    overlay.open_connections(
        vec![connection_host_sourced(
            "remote",
            crate::connection_hosts::ConnectionHostSource::SshConfig,
        )],
        Vec::new(),
    );
    right_click_first_host(&mut overlay);
    // Item 0 = Open in New Tab (connect). Cycling Down three times wraps back
    // to item 0, proving only three items exist.
    for _ in 0..3 {
        overlay.handle_input(OverlayInput::Down);
    }
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::Connect(host) => assert_eq!(host.alias, "remote"),
        other => panic!("wrap to Open in New Tab expected, got {other:?}"),
    }
}

#[test]
fn connection_row_menu_click_open_in_tab_connects() {
    // Click parity: a left-click on the menu's first row (Open in New Tab)
    // connects, mirroring the keyboard Activate.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    right_click_first_host(&mut overlay);
    let rect = overlay_rect(&overlay, 80, 24).expect("menu rect");
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.body_top,
                column: rect.body_left + 1,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    match outcome {
        OverlayOutcome::Connect(host) => assert_eq!(host.alias, "web1"),
        other => panic!("expected Connect from click, got {other:?}"),
    }
}

#[test]
fn connection_overlay_accept_emits_connect_outcome() {
    // The overlay routes an accepted host up as OverlayOutcome::Connect for
    // the App's connect action; presentation stays in the overlay.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    assert!(overlay.is_open());
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::Connect(host) => assert_eq!(host.alias, "web1"),
        other => panic!("expected Connect, got {other:?}"),
    }
    // C12 regression: the overlay must close itself on Connect. If it stays
    // open, keyboard dispatch keeps routing every key (possibly the SSH
    // password) into the type-to-filter box instead of the new SSH tab.
    assert!(
        !overlay.is_open(),
        "connection overlay must close after Connect"
    );
}

#[test]
fn tab_opens_the_add_connection_form() {
    // REMOTE-UX P4: Tab in the connection manager switches this overlay to
    // the Add form (a sibling mode); the overlay stays open.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    assert_eq!(
        overlay.handle_input(OverlayInput::Tab),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::ConnectionForm);
    assert!(overlay.title().contains("Add Connection"));
}

#[test]
fn add_form_save_emits_save_connection_for_append() {
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    overlay.handle_input(OverlayInput::Tab); // -> Add form, focus on Alias
    for ch in "newhost".chars() {
        overlay.handle_input(OverlayInput::Char(ch));
    }
    match overlay.handle_input(OverlayInput::Save) {
        OverlayOutcome::SaveConnection { host, edit_target } => {
            assert_eq!(host.alias, "newhost");
            assert_eq!(edit_target, None, "Add appends");
        }
        other => panic!("expected SaveConnection, got {other:?}"),
    }
    assert!(!overlay.is_open(), "form closes on save");
}

#[test]
fn right_arrow_opens_edit_form_for_an_odytty_row() {
    // The selected OdyTTY-owned row opens pre-filled in the Edit form; Save
    // targets the original block for the byte-splice writer.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    assert_eq!(
        overlay.handle_input(OverlayInput::Right),
        OverlayOutcome::Consumed
    );
    assert_eq!(overlay.render_signature().mode, OverlayMode::ConnectionForm);
    assert!(overlay.title().contains("Edit Connection"));
    match overlay.handle_input(OverlayInput::Save) {
        OverlayOutcome::SaveConnection { host, edit_target } => {
            assert_eq!(host.alias, "web1");
            assert_eq!(
                edit_target.as_deref(),
                Some("web1"),
                "Edit splices the block"
            );
        }
        other => panic!("expected SaveConnection, got {other:?}"),
    }
}

#[test]
fn form_test_button_emits_test_connection_and_stays_open() {
    // The Test action routes up as OverlayOutcome::TestConnection (the App
    // runs the probe); the form must NOT close so the result can render.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    overlay.handle_input(OverlayInput::Tab); // -> Add form, focus Alias
    for ch in "host".chars() {
        overlay.handle_input(OverlayInput::Char(ch));
    }
    // Alias -> HostName -> User -> Port -> Advanced -> Test (5 downs).
    for _ in 0..5 {
        overlay.handle_input(OverlayInput::Down);
    }
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::TestConnection(host) => assert_eq!(host.alias, "host"),
        other => panic!("expected TestConnection, got {other:?}"),
    }
    assert!(overlay.is_open(), "form stays open through a Test");
    assert_eq!(overlay.render_signature().mode, OverlayMode::ConnectionForm);
    // The App feeds the result back; only the form mode consumes it.
    overlay.set_connection_form_test_result(Ok(crate::ssh_connect::ProbeClass::AuthOk));
    let lines = overlay.visible_lines(72, 40);
    assert!(lines.iter().any(|l| l.text.contains("Reachable")));
}

#[test]
fn form_identity_browse_round_trips_and_stays_open() {
    // FORM-UX: Enter on the empty IdentityFile field routes up as
    // BrowseIdentityKeys (the App scans ~/.ssh); the form stays open, and
    // seeding it via open_identity_key_browse fills the field on a pick.
    let mut overlay = OverlayUi::default();
    overlay.open_connections(vec![connection_host("web1")], Vec::new());
    overlay.handle_input(OverlayInput::Tab); // -> Add form, focus Alias
    // Alias -> HostName -> User -> Port -> Advanced, then activate Advanced
    // to reveal IdentityFile, then one Down onto it.
    for _ in 0..4 {
        overlay.handle_input(OverlayInput::Down);
    }
    overlay.handle_input(OverlayInput::Activate); // toggle Advanced open
    overlay.handle_input(OverlayInput::Down); // -> IdentityFile
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::BrowseIdentityKeys
    );
    assert!(overlay.is_open(), "form stays open for the browser");
    assert_eq!(overlay.render_signature().mode, OverlayMode::ConnectionForm);
    // The App seeds the browser; a pick fills the field and closes it.
    overlay.open_identity_key_browse(vec!["/home/u/.ssh/id_ed25519".to_owned()]);
    let lines = overlay.visible_lines(72, 20);
    assert!(lines.iter().any(|l| l.text.contains("id_ed25519")));
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open(), "picking a key returns to the form");
}

#[test]
fn bind_purpose_picker_accept_emits_bind_outcome() {
    // ODP-1B/6B: the shared host picker opened for the BindWorkspace purpose
    // lifts an accepted host into BindWorkspaceToHost(alias) — not Connect —
    // and closes itself before emitting (like Connect).
    let mut overlay = OverlayUi::default();
    overlay.open_connections_for_purpose(
        vec![connection_host("web1")],
        ConnectionPickerPurpose::BindWorkspace,
        Vec::new(),
    );
    assert!(overlay.is_open());
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::BindWorkspaceToHost(alias) => assert_eq!(alias, "web1"),
        other => panic!("expected BindWorkspaceToHost, got {other:?}"),
    }
    assert!(
        !overlay.is_open(),
        "shared picker must close after a bind pick"
    );
}

#[test]
fn connect_tab_after_picker_lifts_to_connect_host_in_tab_after() {
    // ODP-5D: the shared host picker opened for ConnectTabAfter lifts an
    // accepted host into ConnectHostInTabAfter(host, token) — carrying the
    // clicked tab's token — and closes itself before emitting.
    let mut overlay = OverlayUi::default();
    overlay.open_connections_for_purpose(
        vec![connection_host("web1")],
        ConnectionPickerPurpose::ConnectTabAfter(SessionToken(4)),
        Vec::new(),
    );
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::ConnectHostInTabAfter(host, token) => {
            assert_eq!(host.alias, "web1");
            assert_eq!(token, SessionToken(4));
        }
        other => panic!("expected ConnectHostInTabAfter, got {other:?}"),
    }
    assert!(!overlay.is_open(), "picker must close after a connect pick");
}

#[test]
fn replace_tab_picker_lifts_to_replace_tab_with_host_picked() {
    // ODP-5D: the ReplaceTab picker lifts into ReplaceTabWithHostPicked so
    // the App can gate the destructive close behind its running-child check.
    let mut overlay = OverlayUi::default();
    overlay.open_connections_for_purpose(
        vec![connection_host("web1")],
        ConnectionPickerPurpose::ReplaceTab(SessionToken(6)),
        Vec::new(),
    );
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::ReplaceTabWithHostPicked(host, token) => {
            assert_eq!(host.alias, "web1");
            assert_eq!(token, SessionToken(6));
        }
        other => panic!("expected ReplaceTabWithHostPicked, got {other:?}"),
    }
    assert!(!overlay.is_open(), "picker must close after a replace pick");
}

#[test]
fn confirm_replace_tab_dialog_keys_confirm_and_cancel() {
    // ODP-5D: Enter/Y on the replace confirm emits the confirmed outcome
    // carrying the stashed host + token; Esc/N cancels without emitting it.
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_replace_tab(Box::new(connection_host("web1")), SessionToken(2));
    assert!(overlay.is_confirm_replace_tab());
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::ReplaceTabWithHostConfirmed(host, token) => {
            assert_eq!(host.alias, "web1");
            assert_eq!(token, SessionToken(2));
        }
        other => panic!("expected ReplaceTabWithHostConfirmed, got {other:?}"),
    }
    assert!(!overlay.is_open(), "dialog closes on confirm");

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_replace_tab(Box::new(connection_host("web1")), SessionToken(2));
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Close
    );
}

#[test]
fn confirm_replace_tab_click_replace_confirms_cancel_dismisses() {
    // Click→key parity: a click on the "Replace" region confirms, the
    // "Cancel" region cancels, and the prompt text is inert (no destructive
    // click by accident).
    let replace_col = CONFIRM_REPLACE_TAB_ACTION_LINE.find("[Enter").unwrap() + 2;
    let cancel_col = CONFIRM_REPLACE_TAB_ACTION_LINE.find("[Esc").unwrap() + 2;
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_replace_tab(Box::new(connection_host("db")), SessionToken(9));
    assert!(matches!(
        overlay.confirm_replace_tab_click(2, replace_col),
        OverlayOutcome::ReplaceTabWithHostConfirmed(_, SessionToken(9))
    ));

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_replace_tab(Box::new(connection_host("db")), SessionToken(9));
    assert_eq!(
        overlay.confirm_replace_tab_click(2, cancel_col),
        OverlayOutcome::Close
    );
    // Prompt-row click is inert — the dialog stays open, nothing destructive.
    assert_eq!(
        overlay.confirm_replace_tab_click(0, replace_col),
        OverlayOutcome::Consumed
    );
}

#[test]
fn overwrite_layout_confirm_keyboard_three_way() {
    // OVERWRITE-WARN: Enter replaces, R reopens the prompt, Esc cancels.
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_overwrite_layout("work".to_owned(), LayoutSaveKind::WholeApp);
    assert!(overlay.is_confirm_overwrite_layout(), "dialog opened");
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OverwriteLayoutConfirmed {
            name: "work".to_owned(),
            kind: LayoutSaveKind::WholeApp,
        }
    );
    assert!(!overlay.is_open(), "dialog closes on replace");

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_overwrite_layout("dev".to_owned(), LayoutSaveKind::Workspace(3));
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('r')),
        OverlayOutcome::RenameLayoutInstead {
            name: "dev".to_owned(),
            kind: LayoutSaveKind::Workspace(3),
        }
    );

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_overwrite_layout("dev".to_owned(), LayoutSaveKind::WholeApp);
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Close
    );
}

#[test]
fn overwrite_layout_confirm_click_regions() {
    // OVERWRITE-WARN click→key parity: the three bracket regions map, left to
    // right, to Replace / Rename / Cancel; the leading prompt is inert.
    let replace_col = CONFIRM_OVERWRITE_LAYOUT_ACTION_LINE.find("[Enter").unwrap() + 2;
    let rename_col = CONFIRM_OVERWRITE_LAYOUT_ACTION_LINE.find("[R]").unwrap() + 1;
    let cancel_col = CONFIRM_OVERWRITE_LAYOUT_ACTION_LINE.find("[Esc").unwrap() + 2;

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_overwrite_layout("work".to_owned(), LayoutSaveKind::WholeApp);
    assert_eq!(
        overlay.confirm_overwrite_layout_click(2, replace_col),
        OverlayOutcome::OverwriteLayoutConfirmed {
            name: "work".to_owned(),
            kind: LayoutSaveKind::WholeApp,
        }
    );

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_overwrite_layout("work".to_owned(), LayoutSaveKind::Workspace(1));
    assert_eq!(
        overlay.confirm_overwrite_layout_click(2, rename_col),
        OverlayOutcome::RenameLayoutInstead {
            name: "work".to_owned(),
            kind: LayoutSaveKind::Workspace(1),
        }
    );

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_overwrite_layout("work".to_owned(), LayoutSaveKind::WholeApp);
    assert_eq!(
        overlay.confirm_overwrite_layout_click(2, cancel_col),
        OverlayOutcome::Close
    );
    // Prompt-row click is inert — the dialog stays open.
    assert_eq!(
        overlay.confirm_overwrite_layout_click(0, replace_col),
        OverlayOutcome::Consumed
    );
}

#[test]
fn open_layout_mode_keyboard_three_way() {
    // LAYOUT-OPEN-MODE: Enter replaces, A appends, Esc cancels.
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_open_layout("work".to_owned());
    assert!(overlay.is_confirm_open_layout(), "dialog opened");
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenLayoutReplace("work".to_owned())
    );
    assert!(!overlay.is_open(), "dialog closes on replace");

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_open_layout("dev".to_owned());
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('a')),
        OverlayOutcome::OpenLayoutAdd("dev".to_owned())
    );

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_open_layout("dev".to_owned());
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Close
    );
}

#[test]
fn open_layout_mode_click_regions() {
    // LAYOUT-OPEN-MODE click→key parity: the three bracket regions map, left
    // to right, to Replace / Add / Cancel; the leading prompt is inert.
    let replace_col = CONFIRM_OPEN_LAYOUT_ACTION_LINE.find("[Enter").unwrap() + 2;
    let add_col = CONFIRM_OPEN_LAYOUT_ACTION_LINE.find("[A]").unwrap() + 1;
    let cancel_col = CONFIRM_OPEN_LAYOUT_ACTION_LINE.find("[Esc").unwrap() + 2;

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_open_layout("work".to_owned());
    assert_eq!(
        overlay.confirm_open_layout_click(2, replace_col),
        OverlayOutcome::OpenLayoutReplace("work".to_owned())
    );

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_open_layout("work".to_owned());
    assert_eq!(
        overlay.confirm_open_layout_click(2, add_col),
        OverlayOutcome::OpenLayoutAdd("work".to_owned())
    );

    let mut overlay = OverlayUi::default();
    overlay.open_confirm_open_layout("work".to_owned());
    assert_eq!(
        overlay.confirm_open_layout_click(2, cancel_col),
        OverlayOutcome::Close
    );
    // Prompt-row click is inert — the dialog stays open.
    assert_eq!(
        overlay.confirm_open_layout_click(0, replace_col),
        OverlayOutcome::Consumed
    );
}

#[test]
fn session_attach_overlay_accept_emits_attach_outcome() {
    // The overlay routes an accepted session up as
    // OverlayOutcome::AttachSession(id) for the App's new-tab attach;
    // presentation stays in the overlay.
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(vec![listed_session("s-0001-aaaa", "build")]);
    match overlay.handle_input(OverlayInput::Activate) {
        OverlayOutcome::AttachSession(id) => assert_eq!(id, "s-0001-aaaa"),
        other => panic!("expected AttachSession, got {other:?}"),
    }
}

#[test]
fn context_menu_split_items_emit_split_outcomes() {
    // Part B: activating the split items routes up as the split outcomes the
    // App dispatches to `split_active_pane` (the same action the keyboard
    // split chords fire).
    let mut overlay = OverlayUi::default();
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
    // Focus starts at item 0 (Copy); with F7 dropping the content-menu
    // Rename Tab row, Split Right is item index 8.
    for _ in 0..8 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSplitColumns
    );

    // Reopen and walk to Split Down (item index 9).
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
    for _ in 0..9 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSplitRows
    );
}

#[test]
fn tab_slot_duplicate_tab_emits_duplicate_outcome() {
    // Duplicate Tab sits right after New Tab on the tab menu and emits the
    // ContextMenuDuplicateTab outcome (the App spawns a fresh local shell in
    // the active pane's cwd). Focus order (separators skipped): New Tab(0)
    // Duplicate Tab(1) ...
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
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
        false,
        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(SessionToken(3)),
        None,
        std::array::from_fn(|_| None),
    );
    overlay.handle_input(OverlayInput::Down);
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuDuplicateTab
    );
}

#[test]
fn workspace_slot_duplicate_workspace_emits_duplicate_outcome() {
    // Duplicate Workspace sits right after New Workspace on the workspace-slot
    // menu and emits the ContextMenuDuplicateWorkspace outcome (the App opens
    // a fresh workspace whose shell spawns in the active pane's cwd). Focus
    // order (a lone workspace, so the Move rows are hidden): New Workspace(0)
    // Duplicate Workspace(1) Rename Workspace(2) ...
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        false,
        false,
        false,
        false,
        false,
        None,
        false,
        true,
        false,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(0),
        None,
        std::array::from_fn(|_| None),
    );
    overlay.handle_input(OverlayInput::Down);
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuDuplicateWorkspace
    );
}

#[test]
fn tab_slot_close_tab_emits_token_targeted_outcome() {
    // NF-F7-1: closing a tab from a specific tab slot targets THAT tab's
    // token, not the active tab. Body rows: New Tab(0) Duplicate Tab(1)
    // Rename Tab(2) sep(3) Close Tab(4) Close Other Tabs(5) sep(6)
    // New Window(7).
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        false,
        Some(SessionToken(7)),
        false,
        true,
        false,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(SessionToken(7)),
        None,
        std::array::from_fn(|_| None),
    );
    // Focus cycles through items (separators skipped): New Tab(0) Duplicate
    // Tab(1) Rename Tab(2) Close Tab(3) Close Other Tabs(4) New Window(5).
    for _ in 0..3 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuCloseTabToken(SessionToken(7))
    );
}

#[test]
fn tab_slot_close_other_tabs_emits_token_outcome() {
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        false,
        Some(SessionToken(2)),
        false,
        true,
        false,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(SessionToken(2)),
        None,
        std::array::from_fn(|_| None),
    );
    // Close Other Tabs is item index 4 (New Tab / Duplicate Tab / Rename
    // Tab / Close Tab / Close Other Tabs / New Window).
    for _ in 0..4 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuCloseOtherTabs(SessionToken(2))
    );
}

#[test]
fn tab_slot_move_to_next_workspace_emits_token_outcome() {
    // ODP-7: with multiple workspaces, the move row targets the clicked
    // tab's token.
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        false,
        Some(SessionToken(5)),
        false,
        true,
        true,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(SessionToken(5)),
        None,
        std::array::from_fn(|_| None),
    );
    // Items: New Tab(0) Duplicate Tab(1) Rename Tab(2) Close Tab(3)
    // Close Other Tabs(4) Connect to Host(5) Replace with Host(6)
    // Move to Workspace(7) New Window(8).
    for _ in 0..7 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuMoveToWorkspace(SessionToken(5))
    );
}

#[test]
fn tab_slot_connect_and_replace_host_emit_token_outcomes() {
    // ODP-5D: the tab-menu host actions carry the CLICKED tab's token so the
    // App seeds the picker for the right tab. Single-workspace tab menu:
    // New Tab(0) Duplicate Tab(1) Rename Tab(2) Close Tab(3)
    // Close Other Tabs(4) Connect to Host(5) Replace with Host(6)
    // New Window(7).
    let open = || {
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu_with_prompt_editing_hint(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            false,
            Some(SessionToken(8)),
            false,
            true,
            false,
            false,
            crate::native::context_menu_ui::ContextMenuSurface::TabSlot(SessionToken(8)),
            None,
            std::array::from_fn(|_| None),
        );
        overlay
    };
    let mut connect = open();
    for _ in 0..5 {
        connect.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        connect.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuConnectToHost(SessionToken(8))
    );
    let mut replace = open();
    for _ in 0..6 {
        replace.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        replace.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuReplaceTabWithHost(SessionToken(8))
    );
}

#[test]
fn content_close_tab_emits_active_close_outcome() {
    // The content surface (no TabSlot token) keeps the active-tab close.
    let mut overlay = OverlayUi::default();
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
    // With a selection: Copy(0) Cut(1) Paste(2) Delete(3) Select All(4) sep
    // New Tab(5) New Window(6) Close Tab(7).
    for _ in 0..7 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuCloseTab
    );
}

#[test]
fn context_menu_new_window_emits_new_window_outcome() {
    // F1: activating the New Window item (single-pane visible index 6, right
    // after New Tab at 5) routes up as the ContextMenuNewWindow outcome the
    // App dispatches to `handle_new_window` — the same handler the
    // Ctrl+Shift+N chord fires.
    let mut overlay = OverlayUi::default();
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
    for _ in 0..6 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuNewWindow
    );
    // The menu closes itself on activation (the App relies on this before
    // dispatching the outcome).
    assert!(!overlay.is_open(), "context menu closes on New Window");
}

#[test]
fn context_menu_close_pane_emits_close_pane_outcome_only_multi_pane() {
    // Multi-pane, with selection: Close Pane is visible item index 10 (right
    // after Split Down at 9; F7 dropped the content-menu Rename Tab row);
    // activating it routes up as the Close Pane outcome the App dispatches to
    // `apply_pane_action(ClosePane)`.
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        None,
        true,
        None,
        std::array::from_fn(|_| None),
    );
    for _ in 0..10 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuClosePane
    );

    // Single-pane: Close Pane is hidden, so item index 10 is New Workspace
    // (the first workspace-section item) — the Close Pane outcome is
    // unreachable.
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
    for _ in 0..10 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuNewWorkspace
    );
}

#[test]
fn content_menu_workspace_actions_target_the_active_workspace() {
    // The content-grid workspace section has no per-workspace click target,
    // so Rename/Close route up as the active-workspace outcomes (the App
    // resolves the active index); New Workspace is unconditional.
    let open = || {
        let mut overlay = OverlayUi::default();
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
        overlay
    };
    // Single-pane with selection: New/Rename/Close Workspace are visible
    // indices 10/11/12 (right after the two splits at 8/9).
    let step_to = |idx: usize| {
        let mut overlay = open();
        for _ in 0..idx {
            overlay.handle_input(OverlayInput::Down);
        }
        overlay.handle_input(OverlayInput::Activate)
    };
    assert_eq!(step_to(10), OverlayOutcome::ContextMenuNewWorkspace);
    assert_eq!(
        step_to(11),
        OverlayOutcome::ContextMenuRenameActiveWorkspace
    );
    assert_eq!(step_to(12), OverlayOutcome::ContextMenuCloseActiveWorkspace);
    // ODP-6B: an unbound workspace shows Bind to Host at index 13, which
    // lifts to the "open the shared host picker" outcome.
    assert_eq!(step_to(13), OverlayOutcome::ContextMenuBindWorkspace);
}

#[test]
fn content_menu_bound_workspace_offers_unbind() {
    // ODP-6B: when the active workspace is bound, the workspace section's
    // conditional row is Unbind (index 13), lifting to the direct-unbind
    // outcome (no host picker needed).
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
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
        crate::native::context_menu_ui::ContextMenuSurface::Content,
        None,
        std::array::from_fn(|_| None),
    );
    for _ in 0..13 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuUnbindWorkspace
    );
}

#[test]
fn workspace_slot_menu_bind_unbind_target_the_clicked_slot() {
    // RAIL-BIND: the rail slot menu's host action targets the CLICKED slot
    // index. Unbound slot 2 -> "Bind to Host" (index 3) lifts to the picker
    // outcome carrying idx 2; a bound slot -> "Unbind" lifts to the direct
    // unbind outcome carrying idx 2.
    let open = |bound: bool| {
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu_with_prompt_editing_hint(
            CellPoint { row: 0, column: 0 },
            false,
            false,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
            bound,
            crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(2),
            None,
            std::array::from_fn(|_| None),
        );
        for _ in 0..4 {
            overlay.handle_input(OverlayInput::Down);
        }
        overlay.handle_input(OverlayInput::Activate)
    };
    assert_eq!(open(false), OverlayOutcome::ContextMenuBindWorkspaceAt(2));
    assert_eq!(open(true), OverlayOutcome::ContextMenuUnbindWorkspaceAt(2));
}

#[test]
fn workspace_slot_menu_save_as_layout_targets_the_clicked_slot() {
    // LAYOUT-SURFACE + RAIL-SAVE-ALL: the rail slot menu offers BOTH the
    // whole-app "Save as Layout…" (surface-independent → ContextMenuSaveAllLayout)
    // and the single-workspace "Save Workspace as Layout…" (targeting the
    // CLICKED slot → ContextMenuSaveLayoutAt). Unbound slot 2 menu:
    // New(0) Duplicate(1) Rename(2) Close(3) Bind(4) SaveAll(5) SaveWorkspace(6).
    let open = |surface| {
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu_with_prompt_editing_hint(
            CellPoint { row: 0, column: 0 },
            false,
            false,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
            false,
            surface,
            None,
            std::array::from_fn(|_| None),
        );
        overlay
    };
    let surface = crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(2);

    // Index 5 = whole-app save.
    let mut overlay = open(surface);
    for _ in 0..5 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSaveAllLayout
    );

    // Index 6 = single-workspace save of the clicked slot.
    let mut overlay = open(surface);
    for _ in 0..6 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSaveLayoutAt(2)
    );
}

#[test]
fn rail_empty_menu_open_layout_emits_picker_outcome() {
    // LAYOUT-SURFACE + SAVE-ALL-LAYOUT: the empty rail offers New Workspace(0),
    // the whole-app Save as Layout(1), then Open Layout(2); activating Open
    // Layout lifts to the picker outcome, and Save as Layout lifts to the
    // whole-app save outcome.
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        false,
        false,
        false,
        false,
        false,
        None,
        false,
        true,
        false,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceRailEmpty,
        None,
        std::array::from_fn(|_| None),
    );
    overlay.handle_input(OverlayInput::Down);
    overlay.handle_input(OverlayInput::Down);
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuOpenLayoutPicker
    );
}

#[test]
fn rail_empty_menu_save_all_layout_emits_whole_app_outcome() {
    // SAVE-ALL-LAYOUT: the empty rail's Save as Layout (index 1) lifts to the
    // surface-independent whole-app save outcome.
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        false,
        false,
        false,
        false,
        false,
        None,
        false,
        true,
        false,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceRailEmpty,
        None,
        std::array::from_fn(|_| None),
    );
    overlay.handle_input(OverlayInput::Down);
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSaveAllLayout
    );
}

#[test]
fn content_menu_save_as_layout_targets_the_active_workspace() {
    // LAYOUT-SURFACE: Save Workspace as Layout on the content surface (no slot
    // target) lifts to the active-workspace save outcome. With a selection the
    // workspace section is New(10) Rename(11) Close(12) Bind(13)
    // SaveAll(14) SaveWorkspace(15) Open(16).
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        false,
        None,
        false,
        true,
        false,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::Content,
        None,
        std::array::from_fn(|_| None),
    );
    for _ in 0..15 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSaveActiveLayout
    );
}

#[test]
fn content_menu_save_all_layout_emits_whole_app_outcome() {
    // SAVE-ALL-LAYOUT: the whole-app Save as Layout on the content surface
    // (index 14, right after Bind) lifts to the surface-independent whole-app
    // save outcome, ahead of the single-workspace Save Workspace as Layout.
    let mut overlay = OverlayUi::default();
    overlay.open_context_menu_with_prompt_editing_hint(
        CellPoint { row: 0, column: 0 },
        true,
        true,
        true,
        true,
        false,
        None,
        false,
        true,
        false,
        false,
        crate::native::context_menu_ui::ContextMenuSurface::Content,
        None,
        std::array::from_fn(|_| None),
    );
    for _ in 0..14 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSaveAllLayout
    );
}

#[test]
fn context_menu_file_items_emit_path_outcomes() {
    // C3: with a resolved path target, the file section's four items lift
    // into the matching path outcomes carrying the resolved data. Synthetic
    // path only — no real filesystem.
    let resolved = crate::paths::Resolved {
        abs: "/proj/src/main.rs".to_owned(),
        kind: crate::paths::FsKind::File,
        line: Some(42),
        col: Some(7),
    };
    // Path-scoped visible order: Open(0) / Open With…(1) / CopyPath(2) /
    // CopyFile(3) / Reveal(4) for a non-image file, then pinned text
    // Copy/Paste after a separator.
    let open_menu = |steps: usize| {
        let mut overlay = OverlayUi::default();
        overlay.open_context_menu(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            None,
            false,
            Some(resolved.clone()),
            std::array::from_fn(|_| None),
        );
        for _ in 0..steps {
            overlay.handle_input(OverlayInput::Down);
        }
        overlay.handle_input(OverlayInput::Activate)
    };

    assert_eq!(
        open_menu(0),
        OverlayOutcome::ContextMenuOpenPath(Box::new(resolved.clone()))
    );
    assert_eq!(
        open_menu(1),
        OverlayOutcome::ContextMenuOpenWith(Box::new(resolved.clone()))
    );
    assert_eq!(
        open_menu(2),
        OverlayOutcome::ContextMenuCopyPath("/proj/src/main.rs".to_owned())
    );
    assert_eq!(
        open_menu(3),
        OverlayOutcome::ContextMenuCopyFile("file:///proj/src/main.rs".to_owned())
    );
    assert_eq!(
        open_menu(4),
        OverlayOutcome::ContextMenuRevealPath(Box::new(resolved.clone()))
    );
}

#[test]
fn context_menu_without_path_has_no_file_outcomes() {
    // With no path target the file items are not visible; walking to where
    // they would be lands on the launcher section instead (byte-identical
    // to the pre-C3 menu).
    let mut overlay = OverlayUi::default();
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
    // 24 visible items single-pane with a selection (F7 dropped the Rename
    // Tab row; the workspace section adds New/Rename/Close + Bind to Host +
    // Save as Layout + Save Workspace as Layout + Open Layout); Manage
    // Sessions is index 22 (Detach & switch is last at 23).
    for _ in 0..22 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ContextMenuSessionAttach,
        "walking to the launcher section lands on Manage Sessions, not a file item"
    );
}

#[test]
fn context_menu_keyboard_shortcuts_opens_key_bindings() {
    // F3: the "Keyboard Shortcuts" launcher item (first after Settings,
    // visible index 18 single-pane with a selection — the workspace section
    // plus Bind to Host + Save as Layout + Save Workspace as Layout + Open
    // Layout shift the launcher block down by seven) activates the key-remap
    // editor via the same OpenKeyBindings outcome the settings "keybinds"
    // row emits.
    let mut overlay = OverlayUi::default();
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
    for _ in 0..18 {
        overlay.handle_input(OverlayInput::Down);
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenKeyBindings,
        "activating Keyboard Shortcuts opens the key-remap editor"
    );
}

#[test]
fn confirm_close_dialog_opens_renders_and_routes_keys() {
    // CLOSE-CONFIRM: the dialog opens in its own mode, paints its title and
    // copy, and routes keys per the keyboard contract.
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_close();
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::ConfirmClose);

    // The dialog paints its title and a non-empty body.
    let mut rendered = snapshot(70, 18);
    apply_overlay(&mut rendered, &mut overlay);
    let painted: String = rendered.cells.iter().map(|cell| cell.ch).collect();
    assert!(painted.contains("Close?"));
    assert!(painted.contains("Close anyway?"));

    // Enter confirms: emits ForceClose AND closes the dialog so the UI is
    // clean before the App exits (TRAP-4).
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::ForceClose
    );
    assert!(!overlay.is_open());

    // 'y' / 'Y' also confirm.
    for ch in ['y', 'Y'] {
        overlay.open_confirm_close();
        assert_eq!(
            overlay.handle_input(OverlayInput::Char(ch)),
            OverlayOutcome::ForceClose
        );
        assert!(!overlay.is_open());
    }

    // Esc and 'n' / 'N' cancel: they emit Close (NOT ForceClose), so the
    // window never exits on a dismiss (TRAP-2).
    for input in [
        OverlayInput::Close,
        OverlayInput::Char('n'),
        OverlayInput::Char('N'),
    ] {
        overlay.open_confirm_close();
        assert_eq!(overlay.handle_input(input), OverlayOutcome::Close);
    }

    // Any other key is swallowed (no PTY leak behind the modal).
    overlay.open_confirm_close();
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('x')),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
}

#[test]
fn risky_paste_dialog_shows_bounded_metadata_and_routes_explicit_actions() {
    let dialog = RiskyPasteDialog {
        line_count: 2,
        byte_count: 12,
        escaped_preview: "first\\nsecond".to_owned(),
        preview_truncated: false,
        one_line_available: true,
    };
    let mut overlay = OverlayUi::default();
    overlay.open_risky_paste(dialog.clone());
    assert!(overlay.is_risky_paste());

    let mut rendered = snapshot(100, 24);
    apply_overlay(&mut rendered, &mut overlay);
    let painted: String = rendered.cells.iter().map(|cell| cell.ch).collect();
    assert!(painted.contains("Confirm paste"));
    assert!(painted.contains("2 lines, 12 bytes"));
    assert!(painted.contains("first\\nsecond"));
    assert!(painted.contains("Paste as One Line"));

    assert_eq!(
        overlay.handle_input(OverlayInput::Char('o')),
        OverlayOutcome::RiskyPasteOneLine
    );
    assert!(!overlay.is_open());

    overlay.open_risky_paste(dialog);
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::RiskyPaste
    );

    overlay.open_risky_paste(RiskyPasteDialog {
        one_line_available: false,
        ..RiskyPasteDialog::default()
    });
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('o')),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::RiskyPasteCancel
    );
}

#[test]
fn pointer_press_outside_the_panel_dismisses_settings() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Top-left corner is well outside the centered panel.
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint { row: 0, column: 0 },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::Close);
}

#[test]
fn pointer_press_outside_in_theme_picker_restores_and_closes() {
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // A move within the picker previews a different theme...
    assert!(matches!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::ApplySettings(_)
    ));
    // ...and a click outside dismisses exactly like Esc: restore + close.
    let OverlayOutcome::ApplySettings(restored) = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint { row: 0, column: 0 },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    ) else {
        panic!("expected restoration settings on click-away");
    };
    assert_eq!(restored.theme, crate::theme::Theme::ODYSSEY);
    assert!(!overlay.is_open());
}

#[test]
fn pointer_click_on_theme_value_opens_picker() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Drill into Themes section first (Enter on the focused first section).
    overlay.handle_input(OverlayInput::Activate);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // body_top + 1 = theme value row (after "Theme" group header at row 0).
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: theme_value_cell(rect),
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::OpenThemePicker);
}

#[test]
fn pointer_press_on_title_row_is_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // The top border row sits above body_top but inside the panel box.
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::Consumed);
}

#[test]
fn pointer_press_on_settings_back_arrow_returns_to_sections() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    overlay.handle_input(OverlayInput::Down); // Fonts
    overlay.handle_input(OverlayInput::Activate); // drill in
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Clicking the TITLE ROW (rect.top) where the ← arrow is actually drawn.
    // This is the correct click target; the previous test used rect.top + 1
    // (the row below the title) which was wrong — the arrow is at rect.top.
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_eq!(
        overlay.render_signature().panel.level,
        SettingsLevel::SectionList,
        "clicking the title row ← arrow returns to section list"
    );

    // The row below the title (rect.top + 1) is also accepted as a forgiving
    // click target for the back affordance.
    overlay.handle_input(OverlayInput::Activate); // re-enter detail
    let rect2 = overlay_rect(&overlay, 80, 24).expect("rect");
    let outcome2 = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect2.top + 1,
                column: rect2.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect2,
    );
    assert_eq!(outcome2, OverlayOutcome::Consumed);
    assert_eq!(
        overlay.render_signature().panel.level,
        SettingsLevel::SectionList,
        "clicking one row below title also navigates back"
    );
}

#[test]
fn every_back_titled_mode_has_a_live_title_arrow() {
    // BACK-ARROW class guard (NF15 recurrence): every mode whose title
    // starts with `←` MUST have a title-row hit-test that accepts the arrow,
    // or the affordance is click-dead (Esc masks it). This iterates ALL
    // modes so a future `←`-titled mode without coverage fails here rather
    // than shipping a dead arrow (as Connections, then About, once did).
    const ALL_MODES: [OverlayMode; 19] = [
        OverlayMode::Settings,
        OverlayMode::ThemePicker,
        OverlayMode::ThemeBuilder,
        OverlayMode::FontPicker,
        OverlayMode::KeyBindings,
        OverlayMode::Onboarding,
        OverlayMode::ContextMenu,
        OverlayMode::CommandPalette,
        OverlayMode::Replay,
        OverlayMode::Connections,
        OverlayMode::ConnectionForm,
        OverlayMode::SessionAttach,
        OverlayMode::OpenWith,
        OverlayMode::WorkspacePicker,
        OverlayMode::ImageView,
        OverlayMode::ConfirmClose,
        OverlayMode::AttachChoice,
        OverlayMode::ConfirmKillSession,
        OverlayMode::DetachSwitchChoice,
    ];
    let mut back_titled = 0;
    for mode in ALL_MODES {
        let overlay = OverlayUi {
            mode,
            open: true,
            ..Default::default()
        };
        let title = overlay.title();
        if !title.starts_with('\u{2190}') {
            continue;
        }
        back_titled += 1;
        let rect = overlay_rect(&overlay, 80, 24).expect("rect");
        // The arrow sits at rect.top / rect.body_left; a click there must be
        // claimed by exactly one of the two title back-hit tests.
        let cell = CellPoint {
            row: rect.top,
            column: rect.body_left,
        };
        assert!(
            overlay.settings_title_back_hit(cell, rect)
                || overlay.picker_title_back_hit(cell, rect),
            "mode {mode:?} draws a ← title but no title hit-test claims the arrow"
        );
    }
    // Sanity: the non-Settings ←-titled pickers are all covered (guards
    // against the loop silently matching zero modes if `title` regressed).
    assert!(
        back_titled >= 8,
        "expected the picker-style ← modes to be counted, saw {back_titled}"
    );
}

#[test]
fn pointer_press_on_about_back_arrow_returns_to_sections() {
    // NF15: the About view draws the same `← … (Esc = back)` title as a
    // SectionDetail, so clicking its arrow must navigate back too. Before
    // the fix, settings_title_back_hit matched only SectionDetail, so this
    // click fell through and left the panel stranded at About (Esc worked
    // via the panel's own input path, masking the dead click).
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    overlay.handle_input(OverlayInput::End); // select the synthetic About row
    overlay.handle_input(OverlayInput::Activate); // drill into About
    assert_eq!(
        overlay.render_signature().panel.level,
        SettingsLevel::About,
        "precondition: at the About level"
    );
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Click the title row (rect.top) where the ← arrow is drawn.
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert_eq!(
        overlay.render_signature().panel.level,
        SettingsLevel::SectionList,
        "clicking the About title ← arrow returns to the section list"
    );
}

#[test]
fn pointer_wheel_scrolls_settings_without_changing_selection() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Drill into Rendering (many entries) so wheel scrolls the Level-2
    // entry list (self.scroll), not the Level-1 section_scroll.
    overlay.handle_input(OverlayInput::Down); // Fonts
    overlay.handle_input(OverlayInput::Down); // Rendering
    overlay.handle_input(OverlayInput::Activate); // drill in
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let before = overlay.render_signature().panel;
    let outcome = overlay.handle_pointer(OverlayPointer::Wheel { lines: 4 }, rect);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    let after = overlay.render_signature().panel;
    assert!(after.scroll > before.scroll, "wheel scrolled the list");
    assert_eq!(after.selected, before.selected, "selection did not move");
}

#[test]
fn pointer_click_session_row_attaches_like_enter() {
    let sessions = vec![
        ListedSession {
            id: "s-1".to_owned(),
            name: "build".to_owned(),
            state: "running",
            age_ms: 1,
            pane_count: 1,
        },
        ListedSession {
            id: "s-2".to_owned(),
            name: "web".to_owned(),
            state: "running",
            age_ms: 1,
            pane_count: 1,
        },
    ];
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(sessions);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Prime the scroll window with a render, then click body row 2 (the
    // second session; row 0 is the `> query` prompt, row 1 the first).
    let _ = overlay.visible_lines(rect.body_width, rect.body_height);
    let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
    assert_eq!(outcome, OverlayOutcome::AttachSession("s-2".to_owned()));
}

#[test]
fn pointer_click_keybind_row_selects_and_arms_capture() {
    let mut overlay = OverlayUi::default();
    overlay.open_key_bindings(&overlay.settings.clone());
    let rect = overlay_rect(&overlay, 80, 30).expect("rect");
    let _ = overlay.visible_lines(rect.body_width, rect.body_height);
    // Row 0 is the help message; row 1 is the first action row.
    let outcome = overlay.handle_pointer(body_press(rect, 1, 0), rect);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert!(
        overlay.is_capturing_chord(),
        "a click on an action row selects + arms chord capture (parity with Enter)"
    );
}

#[test]
fn pointer_click_yes_in_confirm_close_forces_close() {
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_close();
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Action line is the 3rd body row; click inside the "[Enter / Y] Yes" span.
    let yes_col = CONFIRM_CLOSE_ACTION_LINE.find("[Enter").unwrap() + 2;
    let outcome = overlay.handle_pointer(body_press(rect, 2, yes_col), rect);
    assert_eq!(outcome, OverlayOutcome::ForceClose);
    assert!(!overlay.is_open(), "Yes confirms and closes the dialog");
}

#[test]
fn pointer_click_no_in_confirm_close_cancels() {
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_close();
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let no_col = CONFIRM_CLOSE_ACTION_LINE.find("[Esc").unwrap() + 2;
    let outcome = overlay.handle_pointer(body_press(rect, 2, no_col), rect);
    assert_eq!(outcome, OverlayOutcome::Close);
}

#[test]
fn pointer_click_confirm_close_prompt_text_is_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_close();
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Column 0 of the action line is the "Close anyway?" prompt — never a
    // button, so a stray click cannot destroy a running job (TRAP-2).
    let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert!(overlay.is_open(), "a prompt-text click never force-closes");
}

#[test]
fn attach_choice_key_n_and_enter_emit_new_tab() {
    for input in [OverlayInput::Char('n'), OverlayInput::Activate] {
        let mut overlay = OverlayUi::default();
        overlay.open_attach_choice("s-0001-aaaa".to_owned());
        let outcome = overlay.handle_input(input);
        assert_eq!(
            outcome,
            OverlayOutcome::AttachChoiceNewTab("s-0001-aaaa".to_owned())
        );
        assert!(!overlay.is_open(), "the dialog closes after choosing");
    }
}

#[test]
fn attach_choice_key_r_emits_replace() {
    let mut overlay = OverlayUi::default();
    overlay.open_attach_choice("s-0001-aaaa".to_owned());
    let outcome = overlay.handle_input(OverlayInput::Char('R'));
    assert_eq!(
        outcome,
        OverlayOutcome::AttachChoiceReplace("s-0001-aaaa".to_owned())
    );
    assert!(!overlay.is_open());
}

#[test]
fn attach_choice_esc_cancels_without_attaching() {
    let mut overlay = OverlayUi::default();
    overlay.open_attach_choice("s-0001-aaaa".to_owned());
    let outcome = overlay.handle_input(OverlayInput::Close);
    assert_eq!(outcome, OverlayOutcome::Close);
}

#[test]
fn pointer_click_new_tab_in_attach_choice_emits_new_tab() {
    let mut overlay = OverlayUi::default();
    overlay.open_attach_choice("s-0001-aaaa".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let new_col = ATTACH_CHOICE_ACTION_LINE.find("[N").unwrap() + 1;
    let outcome = overlay.handle_pointer(body_press(rect, 2, new_col), rect);
    assert_eq!(
        outcome,
        OverlayOutcome::AttachChoiceNewTab("s-0001-aaaa".to_owned())
    );
    assert!(!overlay.is_open());
}

#[test]
fn pointer_click_replace_in_attach_choice_emits_replace() {
    let mut overlay = OverlayUi::default();
    overlay.open_attach_choice("s-0001-aaaa".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let replace_col = ATTACH_CHOICE_ACTION_LINE.find("[R").unwrap() + 1;
    let outcome = overlay.handle_pointer(body_press(rect, 2, replace_col), rect);
    assert_eq!(
        outcome,
        OverlayOutcome::AttachChoiceReplace("s-0001-aaaa".to_owned())
    );
}

#[test]
fn pointer_click_attach_choice_prompt_text_is_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_attach_choice("s-0001-aaaa".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Col 0 of the action line is the "Open where?" prompt — never a button,
    // so a stray click cannot attach (parity with the ConfirmClose guard).
    let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert!(overlay.is_open(), "a prompt-text click never attaches");
    // A click on the prompt body row (row 0) is also inert.
    let outcome_row0 = overlay.handle_pointer(body_press(rect, 0, 5), rect);
    assert_eq!(outcome_row0, OverlayOutcome::Consumed);
    assert!(overlay.is_open());
}

#[test]
fn confirm_kill_session_key_y_and_enter_emit_confirmed() {
    for input in [OverlayInput::Char('y'), OverlayInput::Activate] {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
        let outcome = overlay.handle_input(input);
        assert_eq!(
            outcome,
            OverlayOutcome::KillSessionConfirmed("s-0001-aaaa".to_owned())
        );
        assert!(!overlay.is_open(), "the dialog closes after confirming");
    }
}

#[test]
fn confirm_kill_session_esc_and_n_cancel_without_killing() {
    for input in [OverlayInput::Close, OverlayInput::Char('n')] {
        let mut overlay = OverlayUi::default();
        overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
        let outcome = overlay.handle_input(input);
        assert_eq!(outcome, OverlayOutcome::Close, "cancel never kills");
    }
}

#[test]
fn pointer_click_kill_in_confirm_kill_session_confirms() {
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let kill_col = CONFIRM_KILL_SESSION_ACTION_LINE.find("[Enter").unwrap() + 2;
    let outcome = overlay.handle_pointer(body_press(rect, 2, kill_col), rect);
    assert_eq!(
        outcome,
        OverlayOutcome::KillSessionConfirmed("s-0001-aaaa".to_owned())
    );
    assert!(!overlay.is_open(), "Kill confirms and closes the dialog");
}

#[test]
fn pointer_click_cancel_in_confirm_kill_session_cancels() {
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let cancel_col = CONFIRM_KILL_SESSION_ACTION_LINE.find("[Esc").unwrap() + 2;
    let outcome = overlay.handle_pointer(body_press(rect, 2, cancel_col), rect);
    assert_eq!(outcome, OverlayOutcome::Close);
}

#[test]
fn pointer_click_confirm_kill_session_prompt_text_is_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Col 0 of the action line is the "Kill it?" prompt — never a button, so
    // a stray click cannot kill (parity with the ConfirmClose guard).
    let outcome = overlay.handle_pointer(body_press(rect, 2, 0), rect);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert!(overlay.is_open(), "a prompt-text click never kills");
}

#[test]
fn right_click_session_row_requests_kill_left_click_still_attaches() {
    let sessions = vec![
        ListedSession {
            id: "s-1".to_owned(),
            name: "build".to_owned(),
            state: "running",
            age_ms: 1,
            pane_count: 1,
        },
        ListedSession {
            id: "s-2".to_owned(),
            name: "web".to_owned(),
            state: "running",
            age_ms: 1,
            pane_count: 1,
        },
    ];
    // Right-click body row 2 (the second session) requests a kill for its id
    // and leaves the manager open (the App opens the confirm dialog).
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(sessions.clone());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let _ = overlay.visible_lines(rect.body_width, rect.body_height);
    let right_press = OverlayPointer::Press {
        cell: CellPoint {
            row: rect.body_top + 2,
            column: rect.body_left,
        },
        button: PointerButton::Right,
        x_in_body: None,
    };
    assert_eq!(
        overlay.handle_pointer(right_press, rect),
        OverlayOutcome::KillSessionRequest("s-2".to_owned())
    );

    // Left-click the same row still attaches (Phase 5 path unchanged).
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(sessions);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let _ = overlay.visible_lines(rect.body_width, rect.body_height);
    assert_eq!(
        overlay.handle_pointer(body_press(rect, 2, 0), rect),
        OverlayOutcome::AttachSession("s-2".to_owned())
    );
}

#[test]
fn right_click_off_a_session_row_is_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_session_attach(Vec::new());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let _ = overlay.visible_lines(rect.body_width, rect.body_height);
    // Row 0 is the `> query` prompt — never a session row.
    let right_press = OverlayPointer::Press {
        cell: CellPoint {
            row: rect.body_top,
            column: rect.body_left,
        },
        button: PointerButton::Right,
        x_in_body: None,
    };
    assert_eq!(
        overlay.handle_pointer(right_press, rect),
        OverlayOutcome::Consumed
    );
}

#[test]
fn detach_switch_key_s_emits_swap() {
    for input in [OverlayInput::Char('s'), OverlayInput::Char('S')] {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        let outcome = overlay.handle_input(input);
        assert_eq!(
            outcome,
            OverlayOutcome::DetachSwitchSwap("/home/user/proj".to_owned())
        );
        assert!(!overlay.is_open(), "the dialog closes after choosing");
    }
}

#[test]
fn detach_switch_key_k_emits_keep_both() {
    for input in [OverlayInput::Char('k'), OverlayInput::Char('K')] {
        let mut overlay = OverlayUi::default();
        overlay.open_detach_switch_choice("/home/user/proj".to_owned());
        let outcome = overlay.handle_input(input);
        assert_eq!(
            outcome,
            OverlayOutcome::DetachSwitchKeepBoth("/home/user/proj".to_owned())
        );
        assert!(!overlay.is_open());
    }
}

#[test]
fn detach_switch_esc_cancels_and_enter_is_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_detach_switch_choice("/home/user/proj".to_owned());
    // Enter has NO default here (Swap is destructive) — it is swallowed.
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed,
        "Enter must not trigger the destructive Swap"
    );
    assert!(overlay.is_open(), "Enter leaves the dialog open");
    // Esc cancels, spawning/closing nothing.
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Close
    );
}

#[test]
fn pointer_click_swap_in_detach_switch_emits_swap() {
    let mut overlay = OverlayUi::default();
    overlay.open_detach_switch_choice("/home/user/proj".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Action line is the 4th body row (index 3).
    let swap_col = DETACH_SWITCH_ACTION_LINE.find("[S").unwrap() + 1;
    let outcome = overlay.handle_pointer(body_press(rect, 3, swap_col), rect);
    assert_eq!(
        outcome,
        OverlayOutcome::DetachSwitchSwap("/home/user/proj".to_owned())
    );
    assert!(!overlay.is_open());
}

#[test]
fn pointer_click_keep_both_in_detach_switch_emits_keep_both() {
    let mut overlay = OverlayUi::default();
    overlay.open_detach_switch_choice("/home/user/proj".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let keep_col = DETACH_SWITCH_ACTION_LINE.find("[K").unwrap() + 1;
    let outcome = overlay.handle_pointer(body_press(rect, 3, keep_col), rect);
    assert_eq!(
        outcome,
        OverlayOutcome::DetachSwitchKeepBoth("/home/user/proj".to_owned())
    );
}

#[test]
fn pointer_click_cancel_in_detach_switch_cancels() {
    let mut overlay = OverlayUi::default();
    overlay.open_detach_switch_choice("/home/user/proj".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let cancel_col = DETACH_SWITCH_ACTION_LINE.find("[Esc").unwrap() + 1;
    let outcome = overlay.handle_pointer(body_press(rect, 3, cancel_col), rect);
    assert_eq!(outcome, OverlayOutcome::Close);
}

#[test]
fn pointer_click_detach_switch_prompt_text_is_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_detach_switch_choice("/home/user/proj".to_owned());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // Col 0 of the action line is the "Swap closes this." prompt — never a
    // button, so a stray click cannot spawn or close a pane.
    let outcome = overlay.handle_pointer(body_press(rect, 3, 0), rect);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert!(overlay.is_open(), "a prompt-text click never acts");
}

// --- Settings numeric steppers: no live drag capture ---

#[test]
fn pointer_press_steps_numeric_once_and_move_release_are_inert() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Drill into Fonts section (contains font_size, a stepper row).
    overlay.handle_input(OverlayInput::Down); // section_selected = 1 (Fonts)
    overlay.handle_input(OverlayInput::Activate); // drill in
    let (down, up) = overlay
        .first_stepper_button_cells(80, 24)
        .expect("a stepper row is visible in Fonts section");
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Press the up button -> applies once without arming drag.
    assert!(matches!(
        overlay.handle_pointer(
            OverlayPointer::Press {
                cell: up,
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        ),
        OverlayOutcome::ApplySettings(_)
    ));
    assert!(
        !overlay.is_settings_dragging(),
        "settings stepper click does not arm a drag"
    );

    // Move to the down button -> inert, because settings steppers do not
    // capture pointer motion.
    assert_eq!(
        overlay.handle_pointer(
            OverlayPointer::Move {
                cell: down,
                x_in_body: None
            },
            rect
        ),
        OverlayOutcome::Consumed
    );

    // Release and later move stay inert.
    assert_eq!(
        overlay.handle_pointer(
            OverlayPointer::Release {
                cell: down,
                button: PointerButton::Left,
            },
            rect,
        ),
        OverlayOutcome::Consumed
    );
    assert!(!overlay.is_settings_dragging());
    assert_eq!(
        overlay.handle_pointer(
            OverlayPointer::Move {
                cell: up,
                x_in_body: None
            },
            rect
        ),
        OverlayOutcome::Consumed,
        "no drag after release"
    );
}

// --- U2 Step 2/3: builder pointer routing through handle_pointer ---

#[test]
fn builder_slider_press_routes_through_handle_pointer_and_arms_a_drag() {
    let mut overlay = OverlayUi::default();
    let settings = overlay.settings.clone();
    overlay.open_theme_builder(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Row 2 of the body is the focused-channel slider (name line,
    // channel-picker, slider). A left press on it applies a theme and arms a
    // drag the App's Move gate (`is_settings_dragging`) now reports.
    let cell = CellPoint {
        row: rect.body_top + 2,
        column: rect.body_left + rect.body_width.saturating_sub(1),
    };
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell,
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "slider press previews a theme"
    );
    assert!(
        overlay.is_settings_dragging(),
        "builder slider press arms a drag routed via the shared gate"
    );

    // Release ends the drag through the same gate.
    overlay.handle_pointer(
        OverlayPointer::Release {
            cell,
            button: PointerButton::Left,
        },
        rect,
    );
    assert!(
        !overlay.is_settings_dragging(),
        "release ends the builder drag"
    );
}

#[test]
fn builder_press_outside_restores_and_closes() {
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_builder(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let OverlayOutcome::ApplySettings(restored) = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint { row: 0, column: 0 },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    ) else {
        panic!("expected restoration settings on click-away");
    };
    assert_eq!(restored.theme, crate::theme::Theme::ODYSSEY);
    assert!(!overlay.is_open(), "click-away closes the builder");
}

// --- Picker back-button mouse click ---

#[test]
fn theme_picker_title_back_arrow_click_closes_standalone() {
    // Standalone ThemePicker (no picker_return): clicking the ← title area
    // restores the original theme and closes the overlay.
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);
    // Navigate away from the original so cancel is visible.
    let _ = overlay.handle_input(OverlayInput::Down);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    // Restores theme (ApplySettings) and closes.
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "theme picker ← click should restore theme"
    );
    assert!(
        !overlay.is_open(),
        "standalone theme picker ← click closes the overlay"
    );
}

#[test]
fn theme_picker_title_back_arrow_click_returns_to_settings_when_from_settings() {
    // ThemePicker opened from settings: clicking ← should return to settings.
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Drill into Themes section then activate the theme row to set picker_return.
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenThemePicker
    );
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Click the ← area in the title row.
    let _outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert!(
        overlay.is_open(),
        "overlay stays open after returning to settings"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "theme picker ← click returns to settings panel"
    );
}

#[test]
fn font_picker_title_back_arrow_click_closes_standalone() {
    // Standalone FontPicker (no picker_return): clicking the ← title area
    // closes the overlay.
    let mut overlay = OverlayUi::default();
    overlay.open_font_picker(&overlay.settings.clone());
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(
        outcome,
        OverlayOutcome::Close,
        "standalone font picker ← click emits Close"
    );
    assert!(
        !overlay.is_open(),
        "standalone font picker ← click closes the overlay"
    );
}

#[test]
fn font_picker_title_back_arrow_click_returns_to_settings_when_from_settings() {
    // FontPicker opened from settings: clicking ← should return to settings.
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Navigate: Down → Fonts section, Activate → drill in, Down → font_family
    // row, Activate → OpenFontPicker (sets picker_return).
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenFontPicker
    );
    let settings = overlay.settings.clone();
    overlay.open_font_picker(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Click the ← area in the title row.
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(
        outcome,
        OverlayOutcome::Consumed,
        "font picker ← click from settings emits Consumed"
    );
    assert!(
        overlay.is_open(),
        "overlay stays open after returning to settings"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "font picker ← click returns to settings panel"
    );
}

// --- Theme picker mouse wheel scrolling ---

#[test]
fn theme_picker_wheel_scrolls_selection() {
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let before = overlay.render_signature().theme_picker.selected;

    // Wheel down moves selection forward.
    let outcome = overlay.handle_pointer(OverlayPointer::Wheel { lines: 1 }, rect);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    let after = overlay.render_signature().theme_picker.selected;
    assert!(
        after > before,
        "wheel down advances selection in theme picker"
    );
}

#[test]
fn theme_picker_wheel_up_moves_selection_back() {
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    // First advance a few entries so there's room to scroll back.
    for _ in 0..3 {
        overlay.handle_pointer(OverlayPointer::Wheel { lines: 1 }, rect);
    }
    let mid = overlay.render_signature().theme_picker.selected;
    assert!(mid >= 3, "should have advanced at least 3 entries");

    // Wheel up moves selection backward.
    overlay.handle_pointer(OverlayPointer::Wheel { lines: -1 }, rect);
    let after = overlay.render_signature().theme_picker.selected;
    assert!(after < mid, "wheel up moves selection back in theme picker");
}

// --- Picker title back-arrow: non-back-area title click is inert ---

#[test]
fn theme_picker_title_click_far_right_is_inert() {
    // Clicking outside the ← area in the title row (far right) is inert.
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left + 20, // far right of title, outside ← zone
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(
        outcome,
        OverlayOutcome::Consumed,
        "title click outside ← zone is inert"
    );
    assert!(
        overlay.is_open(),
        "inert title click does not close the picker"
    );
}

// --- KeyBindings back-button ---

#[test]
fn key_bindings_title_back_arrow_click_returns_to_settings() {
    // KeyBindings is always opened from Settings. Clicking ← in the title
    // area must return to Settings (not close the overlay entirely).
    let mut overlay = OverlayUi::default();
    let settings = overlay.settings.clone();
    overlay.open_settings();
    overlay.open_key_bindings(&settings);
    assert_eq!(overlay.render_signature().mode, OverlayMode::KeyBindings);

    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    // Returns ApplySettings (restores undone key-binding changes) and stays open.
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "key bindings ← click should emit ApplySettings"
    );
    assert!(
        overlay.is_open(),
        "overlay stays open after returning to settings"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "key bindings ← click returns to Settings panel"
    );
}

#[test]
fn key_bindings_title_back_zone_not_matched_outside_arrow_area() {
    // Clicking outside the ← zone in the KeyBindings title row is inert.
    let mut overlay = OverlayUi::default();
    let settings = overlay.settings.clone();
    overlay.open_key_bindings(&settings);
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left + 20, // far right, outside ← zone
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert_eq!(
        outcome,
        OverlayOutcome::Consumed,
        "title click outside ← zone in key bindings is inert"
    );
    assert!(
        overlay.is_open(),
        "inert title click does not close key bindings"
    );
}

// --- ThemeBuilder back-button ---

#[test]
fn theme_builder_title_back_arrow_click_from_picker_returns_to_picker() {
    // ThemeBuilder opened from ThemePicker: clicking ← in the title area
    // must return to ThemePicker (not close the overlay).
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);
    // Simulate opening the builder from the picker (sets builder_from_picker).
    let _ = overlay.handle_input(OverlayInput::Activate); // OpenBuilder for focused theme
    // If OpenBuilder wasn't triggered (no customizable theme focused), open manually.
    if overlay.render_signature().mode != OverlayMode::ThemeBuilder {
        // Force into ThemeBuilder via the picker outcome path.
        overlay.open_theme_picker(&settings);
        // Directly transition as the picker would.
        overlay.theme_builder.open(&settings);
        overlay.mode = OverlayMode::ThemeBuilder;
        overlay.builder_from_picker = true;
    }
    assert_eq!(overlay.render_signature().mode, OverlayMode::ThemeBuilder);

    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let outcome = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: CellPoint {
                row: rect.top,
                column: rect.body_left,
            },
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "theme builder ← click should emit ApplySettings (restore theme)"
    );
    assert!(
        overlay.is_open(),
        "overlay stays open after returning to theme picker"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::ThemePicker,
        "theme builder ← click returns to ThemePicker"
    );
}
