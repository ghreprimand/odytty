//! Tests for the pure mouse-/focus-event encoders in `super::encoding`,
//! exercised through the re-exported `encode_mouse_event` / `encode_focus_event`.

use super::*;

// === Pure mouse-event encoders ===

fn proto(tracking: MouseTracking, encoding: MouseEncoding) -> MouseProtocol {
    MouseProtocol { tracking, encoding }
}

#[test]
fn encode_mouse_off_reports_nothing() {
    let p = proto(MouseTracking::Off, MouseEncoding::Default);
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Press,
            1,
            1,
            MouseModifiers::default()
        ),
        None
    );
}

#[test]
fn encode_mouse_legacy_press_release() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Default);
    // Left press at col 1, row 1: Cb=0+32=32(' '), Cx=33('!'), Cy=33('!').
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Press,
            1,
            1,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[M !!"
    );
    // Release collapses the button to bits 3: Cb=3+32=35('#').
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Release,
            1,
            1,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[M#!!"
    );
}

#[test]
fn encode_mouse_legacy_buttons_and_wheel() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Default);
    let press = |btn| {
        encode_mouse_event(
            p,
            btn,
            MouseEventKind::Press,
            1,
            1,
            MouseModifiers::default(),
        )
        .unwrap()
    };
    // Middle=1 -> 33('!'), Right=2 -> 34('"').
    assert_eq!(press(MouseButton::Middle), b"\x1b[M!!!");
    assert_eq!(press(MouseButton::Right), b"\x1b[M\"!!");
    // WheelUp=64 -> 96('`'), WheelDown=65 -> 97('a').
    assert_eq!(press(MouseButton::WheelUp), b"\x1b[M`!!");
    assert_eq!(press(MouseButton::WheelDown), b"\x1b[Ma!!");
}

#[test]
fn encode_mouse_legacy_modifiers_and_coordinates() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Default);
    // Ctrl(16)+Shift(4)=20 on a left press: Cb=0+20+32=52('4').
    let mods = MouseModifiers {
        shift: true,
        ctrl: true,
        alt: false,
    };
    let bytes =
        encode_mouse_event(p, MouseButton::Left, MouseEventKind::Press, 10, 5, mods).unwrap();
    // Cx=10+32=42('*'), Cy=5+32=37('%').
    assert_eq!(bytes, b"\x1b[M4*%");
}

#[test]
fn encode_mouse_legacy_drops_out_of_range_coordinate() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Default);
    // Column 223 is the last representable (223+32=255); 224 overflows.
    assert!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Press,
            223,
            1,
            MouseModifiers::default()
        )
        .is_some()
    );
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Press,
            224,
            1,
            MouseModifiers::default()
        ),
        None
    );
}

#[test]
fn encode_mouse_x10_press_only_no_modifiers() {
    let p = proto(MouseTracking::X10, MouseEncoding::Default);
    let mods = MouseModifiers {
        shift: true,
        ctrl: true,
        alt: true,
    };
    // X10 ignores modifiers: left press is plain Cb=32(' ').
    assert_eq!(
        encode_mouse_event(p, MouseButton::Left, MouseEventKind::Press, 1, 1, mods).unwrap(),
        b"\x1b[M !!"
    );
    // X10 does not report release or motion.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Release,
            1,
            1,
            MouseModifiers::default()
        ),
        None
    );
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Motion,
            1,
            1,
            MouseModifiers::default()
        ),
        None
    );
}

#[test]
fn encode_mouse_normal_drops_motion_button_event_keeps_it() {
    let normal = proto(MouseTracking::Normal, MouseEncoding::Sgr);
    assert_eq!(
        encode_mouse_event(
            normal,
            MouseButton::Left,
            MouseEventKind::Motion,
            3,
            4,
            MouseModifiers::default()
        ),
        None
    );

    let button_event = proto(MouseTracking::ButtonEvent, MouseEncoding::Sgr);
    // Motion adds 32 to Cb: left drag -> Cb=0+32=32.
    assert_eq!(
        encode_mouse_event(
            button_event,
            MouseButton::Left,
            MouseEventKind::Motion,
            3,
            4,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[<32;3;4M"
    );
}

#[test]
fn encode_mouse_sgr_press_release_carry_button() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Sgr);
    // Left press: Cb=0, terminator M.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Press,
            12,
            34,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[<0;12;34M"
    );
    // Right release: Cb=2 preserved, terminator m (SGR reports the button).
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Right,
            MouseEventKind::Release,
            12,
            34,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[<2;12;34m"
    );
}

#[test]
fn encode_mouse_sgr_handles_large_coordinates_and_modifiers() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Sgr);
    // SGR has no 223 limit.
    let mods = MouseModifiers {
        shift: true,
        alt: false,
        ctrl: false,
    };
    // Left press + Shift(4): Cb=4.
    assert_eq!(
        encode_mouse_event(p, MouseButton::Left, MouseEventKind::Press, 500, 300, mods).unwrap(),
        b"\x1b[<4;500;300M"
    );
}

#[test]
fn encode_mouse_utf8_extends_range() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Utf8);
    // Small coordinates match the legacy single-byte form.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Press,
            1,
            1,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[M !!"
    );
    // A coordinate past 223 is encoded as 2-byte UTF-8 rather than dropped.
    // Column 300 -> value 332 (0x14C) -> UTF-8 0xC5 0x8C.
    let bytes = encode_mouse_event(
        p,
        MouseButton::Left,
        MouseEventKind::Press,
        300,
        1,
        MouseModifiers::default(),
    )
    .unwrap();
    assert_eq!(bytes, b"\x1b[M \xc5\x8c!");
}

#[test]
fn encode_mouse_urxvt_decimal_form() {
    let p = proto(MouseTracking::Normal, MouseEncoding::Urxvt);
    // Left press at (200,100): Cb=0+32=32, decimal params, terminator M.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Press,
            200,
            100,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[32;200;100M"
    );
    // Release collapses to button bits 3: Cb=3+32=35.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::Left,
            MouseEventKind::Release,
            200,
            100,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[35;200;100M"
    );
}

// --- C3: any-event hover motion (no button) ---

#[test]
fn encode_hover_motion_legacy_any_event() {
    let p = proto(MouseTracking::AnyEvent, MouseEncoding::Default);
    // No-button motion: legacy Cb = 3 (no button) + 32 (motion) = 35, then
    // +32 offset = 67 ('C'). At (3,4): cx=35 ('#'), cy=36 ('$').
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            3,
            4,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[MC#$"
    );
}

#[test]
fn encode_hover_motion_sgr_any_event() {
    let p = proto(MouseTracking::AnyEvent, MouseEncoding::Sgr);
    // SGR no-button motion: Cb = 3 + 32 = 35, terminator M.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            10,
            20,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[<35;10;20M"
    );
}

#[test]
fn encode_hover_motion_urxvt_any_event() {
    let p = proto(MouseTracking::AnyEvent, MouseEncoding::Urxvt);
    // urxvt no-button motion: Cb = 32 + 35 = 67, decimal.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            5,
            6,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[67;5;6M"
    );
}

#[test]
fn encode_hover_motion_utf8_matches_legacy_in_ascii_range() {
    let p = proto(MouseTracking::AnyEvent, MouseEncoding::Utf8);
    // UTF-8 (1005) hover in the ASCII range is byte-identical to legacy.
    assert_eq!(
        encode_mouse_event(
            p,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            3,
            4,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[MC#$"
    );
}

#[test]
fn button_event_drops_no_button_motion_any_event_keeps_it() {
    // 1002 reports motion only while a button is held: no-button hover drops.
    let button_event = proto(MouseTracking::ButtonEvent, MouseEncoding::Sgr);
    assert_eq!(
        encode_mouse_event(
            button_event,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            3,
            4,
            MouseModifiers::default()
        ),
        None
    );
    // A real button drag is still reported under 1002 (Cb=0+32=32).
    assert_eq!(
        encode_mouse_event(
            button_event,
            MouseButton::Left,
            MouseEventKind::Motion,
            3,
            4,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[<32;3;4M"
    );
    // 1003 reports the same no-button hover that 1002 dropped.
    let any_event = proto(MouseTracking::AnyEvent, MouseEncoding::Sgr);
    assert_eq!(
        encode_mouse_event(
            any_event,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            3,
            4,
            MouseModifiers::default()
        )
        .unwrap(),
        b"\x1b[<35;3;4M"
    );
}

// --- C3: focus reporting (DECSET/DECRST 1004) ---

#[test]
fn focus_reporting_mode_set_and_reset() {
    let mut terminal = Terminal::new(4, 1);
    assert!(!terminal.focus_reporting());

    terminal.advance(b"\x1b[?1004h");
    assert!(terminal.focus_reporting());

    terminal.advance(b"\x1b[?1004l");
    assert!(!terminal.focus_reporting());
}

#[test]
fn ris_resets_focus_reporting() {
    let mut terminal = Terminal::new(4, 1);
    terminal.advance(b"\x1b[?1004h");
    assert!(terminal.focus_reporting());

    terminal.advance(b"\x1bc"); // RIS
    assert!(!terminal.focus_reporting());
}

#[test]
fn encode_focus_event_gated_and_directional() {
    // Disabled: nothing is emitted regardless of direction.
    assert_eq!(encode_focus_event(false, true), None);
    assert_eq!(encode_focus_event(false, false), None);
    // Enabled: focus-in is ESC [ I, focus-out is ESC [ O.
    assert_eq!(encode_focus_event(true, true).unwrap(), b"\x1b[I");
    assert_eq!(encode_focus_event(true, false).unwrap(), b"\x1b[O");
}
