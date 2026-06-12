//! MP1 mouse protocol evidence tests.
//!
//! These fixtures exercise the public terminal facade plus the public mouse
//! encoder, so they stay hermetic and do not reach into native/window internals.

use odytty::core::{
    MouseButton, MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, MouseTracking,
    Terminal, encode_focus_event, encode_mouse_event,
};

fn proto(tracking: MouseTracking, encoding: MouseEncoding) -> MouseProtocol {
    MouseProtocol { tracking, encoding }
}

fn all_mods() -> MouseModifiers {
    MouseModifiers {
        shift: true,
        alt: true,
        ctrl: true,
    }
}

fn report(
    protocol: MouseProtocol,
    button: MouseButton,
    kind: MouseEventKind,
    column: usize,
    row: usize,
) -> Option<Vec<u8>> {
    encode_mouse_event(
        protocol,
        button,
        kind,
        column,
        row,
        MouseModifiers::default(),
    )
}

#[test]
fn decrqm_inventory_covers_mouse_tracking_focus_and_encoding_modes() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(
        b"\x1b[?9$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\
          \x1b[?1005$p\x1b[?1006$p\x1b[?1015$p\x1b[?1016$p",
    );

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[?9;2$y\
          \x1b[?1000;2$y\
          \x1b[?1002;2$y\
          \x1b[?1003;2$y\
          \x1b[?1004;2$y\
          \x1b[?1005;2$y\
          \x1b[?1006;2$y\
          \x1b[?1015;2$y\
          \x1b[?1016;4$y"
    );

    terminal.advance(b"\x1b[?1002h\x1b[?1004h\x1b[?1006h");
    terminal.advance(
        b"\x1b[?9$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\
          \x1b[?1005$p\x1b[?1006$p\x1b[?1015$p\x1b[?1016$p",
    );

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b[?9;2$y\
          \x1b[?1000;2$y\
          \x1b[?1002;1$y\
          \x1b[?1003;2$y\
          \x1b[?1004;1$y\
          \x1b[?1005;2$y\
          \x1b[?1006;1$y\
          \x1b[?1015;2$y\
          \x1b[?1016;4$y"
    );
}

#[test]
fn mouse_tracking_and_encoding_priority_are_single_active_axes() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?9h");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::X10);

    terminal.advance(b"\x1b[?1002l");
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Off);

    terminal.advance(b"\x1b[?1005h\x1b[?1006h\x1b[?1015h");
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Urxvt);

    terminal.advance(b"\x1b[?1006l");
    assert_eq!(terminal.mouse_protocol().encoding, MouseEncoding::Default);
}

#[test]
fn ris_and_decrst_cleanup_mouse_state() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[?1003h\x1b[?1004h\x1b[?1006h");
    assert_eq!(
        terminal.mouse_protocol(),
        proto(MouseTracking::AnyEvent, MouseEncoding::Sgr)
    );
    assert!(terminal.focus_reporting());

    terminal.advance(b"\x1b[?1000l\x1b[?1005l\x1b[?1004l");
    assert_eq!(terminal.mouse_protocol(), MouseProtocol::default());
    assert!(!terminal.focus_reporting());

    terminal.advance(b"\x1b[?1002h\x1b[?1004h\x1b[?1015h\x1bc");
    assert_eq!(terminal.mouse_protocol(), MouseProtocol::default());
    assert!(!terminal.focus_reporting());
}

#[test]
fn legacy_encoding_reports_boundary_coords_and_drops_beyond_cap() {
    let protocol = proto(MouseTracking::Normal, MouseEncoding::Default);

    assert_eq!(
        report(protocol, MouseButton::Left, MouseEventKind::Press, 1, 1).as_deref(),
        Some(b"\x1b[M !!".as_slice())
    );
    assert_eq!(
        report(protocol, MouseButton::Left, MouseEventKind::Press, 223, 223),
        Some(vec![0x1b, b'[', b'M', b' ', 0xff, 0xff])
    );
    assert_eq!(
        report(protocol, MouseButton::Left, MouseEventKind::Press, 224, 223),
        None
    );
    assert_eq!(
        report(protocol, MouseButton::Left, MouseEventKind::Press, 223, 224),
        None
    );
}

#[test]
fn utf8_mouse_encoding_extends_legacy_coordinates_with_same_layout() {
    let protocol = proto(MouseTracking::Normal, MouseEncoding::Utf8);

    assert_eq!(
        report(protocol, MouseButton::Left, MouseEventKind::Press, 224, 224),
        Some(vec![0x1b, b'[', b'M', b' ', 0xc4, 0x80, 0xc4, 0x80])
    );
    assert_eq!(
        report(
            protocol,
            MouseButton::Left,
            MouseEventKind::Press,
            2015,
            2015
        )
        .expect("maximum 1005 coordinate should encode")
        .ends_with(&[0xdf, 0xbf, 0xdf, 0xbf]),
        true
    );
    assert_eq!(
        report(
            protocol,
            MouseButton::Left,
            MouseEventKind::Press,
            2016,
            2015
        ),
        None
    );
}

#[test]
fn sgr_and_urxvt_encodings_use_unbounded_decimal_coordinates() {
    let sgr = proto(MouseTracking::Normal, MouseEncoding::Sgr);
    let urxvt = proto(MouseTracking::Normal, MouseEncoding::Urxvt);

    assert_eq!(
        report(sgr, MouseButton::Left, MouseEventKind::Press, 500, 300).as_deref(),
        Some(b"\x1b[<0;500;300M".as_slice())
    );
    assert_eq!(
        report(urxvt, MouseButton::Left, MouseEventKind::Press, 500, 300).as_deref(),
        Some(b"\x1b[32;500;300M".as_slice())
    );
}

#[test]
fn release_encoding_matches_each_protocol_family() {
    assert_eq!(
        report(
            proto(MouseTracking::Normal, MouseEncoding::Default),
            MouseButton::Right,
            MouseEventKind::Release,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[M#*%".as_slice())
    );
    assert_eq!(
        report(
            proto(MouseTracking::Normal, MouseEncoding::Utf8),
            MouseButton::Right,
            MouseEventKind::Release,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[M#*%".as_slice())
    );
    assert_eq!(
        report(
            proto(MouseTracking::Normal, MouseEncoding::Sgr),
            MouseButton::Right,
            MouseEventKind::Release,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[<2;10;5m".as_slice())
    );
    assert_eq!(
        report(
            proto(MouseTracking::Normal, MouseEncoding::Urxvt),
            MouseButton::Right,
            MouseEventKind::Release,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[35;10;5M".as_slice())
    );
}

#[test]
fn wheel_reports_and_modifiers_match_protocol_rules() {
    assert_eq!(
        report(
            proto(MouseTracking::Normal, MouseEncoding::Default),
            MouseButton::WheelDown,
            MouseEventKind::Press,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[Ma*%".as_slice())
    );
    assert_eq!(
        report(
            proto(MouseTracking::Normal, MouseEncoding::Sgr),
            MouseButton::WheelUp,
            MouseEventKind::Press,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[<64;10;5M".as_slice())
    );
    assert_eq!(
        report(
            proto(MouseTracking::Normal, MouseEncoding::Urxvt),
            MouseButton::WheelDown,
            MouseEventKind::Press,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[97;10;5M".as_slice())
    );

    let normal = proto(MouseTracking::Normal, MouseEncoding::Default);
    let x10 = proto(MouseTracking::X10, MouseEncoding::Default);

    assert_eq!(
        encode_mouse_event(
            normal,
            MouseButton::Left,
            MouseEventKind::Press,
            10,
            5,
            all_mods()
        )
        .as_deref(),
        Some(b"\x1b[M<*%".as_slice())
    );
    assert_eq!(
        encode_mouse_event(
            x10,
            MouseButton::Left,
            MouseEventKind::Press,
            10,
            5,
            all_mods()
        )
        .as_deref(),
        Some(b"\x1b[M *%".as_slice())
    );
}

#[test]
fn tracking_modes_gate_motion_and_release_reports() {
    let x10 = proto(MouseTracking::X10, MouseEncoding::Sgr);
    let normal = proto(MouseTracking::Normal, MouseEncoding::Sgr);
    let button_event = proto(MouseTracking::ButtonEvent, MouseEncoding::Sgr);
    let any_event = proto(MouseTracking::AnyEvent, MouseEncoding::Sgr);

    assert_eq!(
        report(x10, MouseButton::Left, MouseEventKind::Release, 10, 5),
        None
    );
    assert_eq!(
        report(x10, MouseButton::Left, MouseEventKind::Motion, 10, 5),
        None
    );
    assert_eq!(
        report(normal, MouseButton::Left, MouseEventKind::Motion, 10, 5),
        None
    );
    assert_eq!(
        report(
            button_event,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            10,
            5
        ),
        None
    );
    assert_eq!(
        report(
            button_event,
            MouseButton::Left,
            MouseEventKind::Motion,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[<32;10;5M".as_slice())
    );
    assert_eq!(
        report(
            any_event,
            MouseButton::NoButton,
            MouseEventKind::Motion,
            10,
            5
        )
        .as_deref(),
        Some(b"\x1b[<35;10;5M".as_slice())
    );
}

#[test]
fn focus_reporting_uses_mode_1004_and_is_not_mouse_tracking() {
    let mut terminal = Terminal::new(10, 4);

    assert_eq!(encode_focus_event(terminal.focus_reporting(), true), None);
    terminal.advance(b"\x1b[?1004h");
    assert_eq!(
        encode_focus_event(terminal.focus_reporting(), true).as_deref(),
        Some(b"\x1b[I".as_slice())
    );
    assert_eq!(
        encode_focus_event(terminal.focus_reporting(), false).as_deref(),
        Some(b"\x1b[O".as_slice())
    );
    assert_eq!(terminal.mouse_protocol().tracking, MouseTracking::Off);

    terminal.advance(b"\x1b[?1004l");
    assert_eq!(encode_focus_event(terminal.focus_reporting(), true), None);
}

#[test]
fn sgr_pixel_mode_1016_is_reported_unsupported_and_does_not_change_protocol() {
    let mut terminal = Terminal::new(10, 4);

    terminal.advance(b"\x1b[?1016h\x1b[?1016$p");

    assert_eq!(terminal.take_host_output(), b"\x1b[?1016;4$y");
    assert_eq!(terminal.mouse_protocol(), MouseProtocol::default());
}
