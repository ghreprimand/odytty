// SPDX-License-Identifier: GPL-3.0-only
//! The notice render-signature fragment identifies everything the banner
//! paints. The frame cache classifies equal content signatures as retained,
//! so equal text raised in another tone, or as the consent prompt, must not
//! compare equal or the old colors and row coverage survive.

use super::*;
use crate::settings::Osc52WritePolicy;

const OSC52_WRITE_HI: &[u8] = b"\x1b]52;c;aGk=\x1b\\";

fn app() -> App {
    let (mut app, _terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.on_window_focus_changed_for_test(true);
    app
}

#[test]
fn the_same_text_in_another_tone_is_a_different_fragment() {
    let mut app = app();
    assert_eq!(app.open_notice_overlay_signature(), OverlayFragment::Inert);

    app.raise_open_notice("Same words".to_owned());
    let failure = app.open_notice_overlay_signature();
    app.raise_neutral_notice("Same words".to_owned());
    let neutral = app.open_notice_overlay_signature();

    assert_ne!(failure, OverlayFragment::Inert);
    assert_ne!(neutral, OverlayFragment::Inert);
    assert_ne!(
        failure, neutral,
        "a tone change repaints different colors, so the cache must see it"
    );

    // Raising the same tone and text again is the same picture.
    app.raise_neutral_notice("Same words".to_owned());
    assert_eq!(app.open_notice_overlay_signature(), neutral);
}

#[test]
fn a_consent_prompt_differs_from_an_ordinary_notice_with_the_same_text() {
    let mut app = app();
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::Ask);
    app.advance_tab_bytes_at_position_for_test(0, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert!(app.osc52_prompt_metadata_for_test().is_some());

    let prompt = app.open_notice_overlay_signature();
    let OverlayFragment::OpenNotice { text, .. } = prompt.clone() else {
        panic!("a pending prompt must contribute a notice fragment: {prompt:?}");
    };

    app.cancel_osc52_prompt();
    assert_eq!(app.open_notice_overlay_signature(), OverlayFragment::Inert);
    app.raise_open_notice(text.clone());
    app.raise_neutral_notice(text);
    let ordinary = app.open_notice_overlay_signature();
    assert_ne!(
        prompt, ordinary,
        "the prompt paints three rows and an ordinary notice one"
    );
}
