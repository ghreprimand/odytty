// SPDX-License-Identifier: GPL-3.0-only
//! Replace-tab confirmation painting through the App action and overlay painter.

use super::*;
use crate::native::overlay::OverlayOutcome;

fn painted_prompt(alias: &str, width: usize) -> String {
    let (mut app, _) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(width + 4, 14),
        Settings::default(),
    );
    app.set_active_foreground_job_for_test(crate::pty::ForegroundJob::Running);
    assert!(app.foreground_job_running());
    let token = app.active_session_token_for_test();
    let mut hosts = crate::connection_hosts::parse_odytty_hosts_bytes(
        format!("Host {alias}\n  HostName replacement.example.invalid\n").as_bytes(),
    );
    let host = hosts.pop().expect("project-authored host alias parses");
    assert_eq!(host.alias, alias);
    app.apply_overlay_outcome_for_test(OverlayOutcome::ReplaceTabWithHostPicked(
        Box::new(host),
        token,
    ));
    assert!(app.overlay_open_for_test());
    let rows = app.render_overlay_rows_for_test(width + 4, 14);
    let prompt = rows
        .into_iter()
        .find(|row| row.contains("A program is running here."))
        .expect("confirmation prompt painted");
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    assert!(!app.overlay_open_for_test());
    assert_eq!(app.active_session_token_for_test(), token);
    prompt
}

#[test]
fn replace_prompt_painted_ascii_boundary_keeps_the_question() {
    let alias = "production-database-replica-07";
    let narrow = painted_prompt(alias, 44);
    assert!(narrow.contains("Replace it with p"), "{narrow:?}");
    for (width, suffix) in [(45, "with \u{2026}?"), (46, "with p\u{2026}?")] {
        let painted = painted_prompt(alias, width);
        assert!(painted.contains(suffix), "width {width}: {painted:?}");
    }
}

#[test]
fn replace_prompt_painted_wide_alias_keeps_the_question() {
    let painted = painted_prompt(&"\u{754c}".repeat(10), 54);
    assert!(painted.contains("\u{2026}?"), "{painted:?}");
    assert_eq!(painted.matches('\u{754c}').count(), 4);
}

#[test]
fn replace_prompt_painted_combining_alias_is_not_shortened_when_it_fits() {
    let alias = "e\u{301}".repeat(10);
    let painted = painted_prompt(&alias, 54);
    assert!(painted.contains(&format!("with {alias}?")), "{painted:?}");
    assert!(!painted.contains('\u{2026}'));
}
