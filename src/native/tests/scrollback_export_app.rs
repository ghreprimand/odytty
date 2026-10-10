// SPDX-License-Identifier: GPL-3.0-only
//! App-level scrollback export: the focused pane's history and screen are read
//! in order through the production extractor, private terminal metadata never
//! reaches either format, inline images become `[image]`, and the palette
//! entries reach the export path. Document and sanitizer details are unit
//! tested beside the builder.

use super::super::scrollback_export::ScrollbackFormat;
use super::*;

fn app_with(columns: usize, rows: usize) -> (App, Arc<Mutex<Terminal>>) {
    headless_app_with(
        NativeOptions::default(),
        Dimensions::new(columns, rows),
        Settings::default(),
    )
}

fn feed(terminal: &Arc<Mutex<Terminal>>, bytes: &[u8]) {
    let mut guard = crate::native::lock_recover(terminal);
    guard.advance(bytes);
    let _ = guard.take_host_output();
}

#[test]
fn text_export_reads_scrollback_then_screen_in_order() {
    let (app, terminal) = app_with(20, 4);
    let mut input = String::new();
    for line in 0..30 {
        input.push_str(&format!("line {line:02}\r\n"));
    }
    input.push_str("prompt$");
    feed(&terminal, input.as_bytes());
    let text = app
        .capture_scrollback_export(ScrollbackFormat::PlainText)
        .expect("under the cap");
    let mut expected = String::new();
    for line in 0..30 {
        expected.push_str(&format!("line {line:02}\n"));
    }
    expected.push_str("prompt$\n");
    assert_eq!(text, expected);
}

#[test]
fn soft_wrapped_output_exports_as_one_logical_line() {
    let (app, terminal) = app_with(10, 4);
    feed(&terminal, b"abcdefghijklmnopqrstuvwxyz\r\nnext");
    let text = app
        .capture_scrollback_export(ScrollbackFormat::PlainText)
        .expect("under the cap");
    assert_eq!(text, "abcdefghijklmnopqrstuvwxyz\nnext\n");
}

#[test]
fn exports_carry_no_cwd_title_or_link_target_metadata() {
    let (app, terminal) = app_with(40, 6);
    feed(
        &terminal,
        b"\x1b]7;file://private-host/home/secret-user/secret-dir\x07\
          \x1b]2;secret-title\x07\
          \x1b]8;;file:///home/secret-user/notes\x07local\x1b]8;;\x07 \
          \x1b]8;;https://example.com/docs\x07docs\x1b]8;;\x07\r\nvisible",
    );
    let text = app
        .capture_scrollback_export(ScrollbackFormat::PlainText)
        .expect("under the cap");
    assert_eq!(text, "local docs\nvisible\n");
    let html = app
        .capture_scrollback_export(ScrollbackFormat::Html)
        .expect("under the cap");
    for private in ["secret", "private-host", "file:"] {
        assert!(!text.contains(private), "text leaks {private}");
        assert!(!html.contains(private), "html leaks {private}");
    }
    assert!(html.contains("<a href=\"https://example.com/docs\""));
    assert!(html.contains("<title>OdyTTY scrollback</title>"));
}

#[test]
fn kitty_image_placement_exports_as_an_image_line() {
    let (app, terminal) = app_with(40, 8);
    feed(&terminal, b"before\r\n");
    feed(
        &terminal,
        b"\x1b_Ga=T,f=32,t=d,s=2,v=2,i=1,c=2,r=2;AAAA/wAAAP8AAAD/AAAA/w==\x1b\\",
    );
    feed(&terminal, b"\r\nafter");
    let text = app
        .capture_scrollback_export(ScrollbackFormat::PlainText)
        .expect("under the cap");
    assert!(text.starts_with("before\n[image]\n"), "{text:?}");
    assert!(text.trim_end().ends_with("after"), "{text:?}");
    assert_eq!(text.matches("[image]").count(), 1);
    let html = app
        .capture_scrollback_export(ScrollbackFormat::Html)
        .expect("under the cap");
    assert!(!html.to_ascii_lowercase().contains("<img"));
    assert!(!html.contains("AAAA/w"), "image bytes are never embedded");
}

#[test]
fn palette_entries_reach_the_export_path() {
    // A headless App has no event-loop proxy, so the dialog cannot open and
    // the action reports that instead of writing anything.
    for id in ["export-scrollback-text", "export-scrollback-html"] {
        let (mut app, terminal) = app_with(20, 4);
        feed(&terminal, b"hello");
        app.handle_palette_action_for_test(id);
        assert_eq!(
            app.open_notice_message_for_test().as_deref(),
            Some("Native scrollback export is unavailable."),
            "{id}"
        );
    }
}

#[test]
fn app_capture_accepts_exact_final_byte_limit_and_refuses_one_byte_less() {
    let (app, terminal) = app_with(24, 4);
    feed(&terminal, b"capture-boundary");

    for format in [ScrollbackFormat::PlainText, ScrollbackFormat::Html] {
        let expected = app
            .capture_scrollback_export(format)
            .expect("default cap accepts short fixture");
        assert_eq!(
            app.capture_scrollback_export_with_limit(format, expected.len()),
            Ok(expected.clone()),
            "exact encoded length is accepted for {format:?}"
        );
        assert_eq!(
            app.capture_scrollback_export_with_limit(format, expected.len() - 1),
            Err(crate::native::command_export::CommandExportError::TooLarge),
            "one byte below encoded length refuses the whole {format:?} document"
        );
    }
}

#[test]
fn app_html_cap_counts_escaped_text_and_safe_link_markup() {
    let (app, terminal) = app_with(40, 4);
    feed(
        &terminal,
        b"\x1b]8;;https://example.com/long/path\x07<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<<\x1b]8;;\x07",
    );
    let plain = app
        .capture_scrollback_export(ScrollbackFormat::PlainText)
        .expect("short plain text");
    let html = app
        .capture_scrollback_export(ScrollbackFormat::Html)
        .expect("html fits the default cap");
    assert!(html.len() > plain.len());
    assert!(html.contains("&lt;&lt;"));
    assert!(html.contains("<a href=\"https://example.com/long/path\""));

    assert_eq!(
        app.capture_scrollback_export_with_limit(ScrollbackFormat::Html, plain.len()),
        Err(crate::native::command_export::CommandExportError::TooLarge),
        "the actual HTML byte count includes escapes and hyperlink markup"
    );
}

#[test]
fn busy_dialog_is_rejected_before_scrollback_capture() {
    let (mut app, terminal) = app_with(20, 4);
    feed(&terminal, b"ordinary text");
    app.occupy_scrollback_export_dialog_for_test();
    let captures_before = App::scrollback_capture_count();

    app.begin_scrollback_export(ScrollbackFormat::PlainText);

    assert_eq!(
        App::scrollback_capture_count(),
        captures_before,
        "the busy branch must not project or encode the terminal"
    );
    assert_eq!(app.pending_scrollback_export_count(), 1);
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some("A save dialog is already open.")
    );
}

/// HTML export of a profile pane uses that pane's presented theme, matching
/// an App whose global theme is the profile theme; a plain pane keeps the
/// global theme.
#[test]
fn html_export_of_a_profile_pane_uses_the_profile_theme() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let global = Theme::ODYSSEY;
    let profile = Theme::PLAIN;
    let render = |theme: Theme, profile_theme: Option<Theme>| {
        let (mut app, terminal) = headless_app_with(
            NativeOptions::default(),
            Dimensions::new(20, 4),
            Settings {
                theme,
                ..Settings::default()
            },
        );
        if let Some(profile_theme) = profile_theme {
            app.set_active_profile_theme_for_test(Some(profile_theme));
        }
        feed(&terminal, b"\x1b[31mred\x1b[0m plain");
        app.capture_scrollback_export(ScrollbackFormat::Html)
            .expect("under the cap")
    };
    let plain = render(global, None);
    let expected = render(profile, None);
    assert_ne!(plain, expected, "the two themes export different colors");
    assert_eq!(
        render(global, Some(profile)),
        expected,
        "a profile pane exports its own theme"
    );
}
