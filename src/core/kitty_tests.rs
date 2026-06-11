use crate::graphics::{GraphicsProtocol, ImageStoreLimits};

use super::*;

fn b64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() >= 2 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() == 3 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn kitty_apc(control: &str, payload: &[u8]) -> Vec<u8> {
    format!("\x1b_G{control};{}\x1b\\", b64(payload)).into_bytes()
}

fn rgba_2x1() -> Vec<u8> {
    vec![255, 0, 0, 255, 0, 255, 0, 128]
}

#[test]
fn parses_control_and_payload_without_new_dependencies() {
    let (control, payload) =
        super::kitty::test_parse_apc(b"Gf=32,a=T,t=d,s=2,v=1,c=3,r=1,i=9,p=4;QUJD").expect("parse");

    assert_eq!(control.format, Some(32));
    assert_eq!(control.action, Some('T'));
    assert_eq!(control.transmission, Some('d'));
    assert_eq!(control.width, Some(2));
    assert_eq!(control.height, Some(1));
    assert_eq!(control.display_columns, Some(3));
    assert_eq!(control.display_rows, Some(1));
    assert_eq!(control.image_id, Some(9));
    assert_eq!(control.placement_id, Some(4));
    assert_eq!(payload, b"QUJD");
}

#[test]
fn base64_decoder_accepts_padded_and_unpadded_payloads() {
    assert_eq!(
        super::kitty::test_decode_base64(b"TWFu", 16).unwrap(),
        b"Man"
    );
    assert_eq!(
        super::kitty::test_decode_base64(b"TWE=", 16).unwrap(),
        b"Ma"
    );
    assert_eq!(super::kitty::test_decode_base64(b"TQ==", 16).unwrap(), b"M");
}

#[test]
fn kitty_rgba_transmit_and_display_places_at_cursor_without_moving_by_default() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[2;4H");
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,c=2,r=1,i=42", &rgba_2x1()));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].protocol, GraphicsProtocol::Kitty);
    assert_eq!(visible[0].row, 1);
    assert_eq!(visible[0].column, 3);
    assert_eq!(visible[0].display_columns, 2);
    assert_eq!(visible[0].display_rows, 1);
    assert_eq!(
        t.screen().cursor(),
        crate::core::Position { row: 1, column: 3 }
    );
    assert_eq!(t.take_host_output(), b"\x1b_Gi=42;OK\x1b\\");

    let image = t.graphics().store().get(visible[0].image_id).unwrap();
    assert_eq!(image.protocol_id, Some(42));
    assert_eq!(image.rgba, rgba_2x1());
}

#[test]
fn kitty_rgb_transmit_expands_to_opaque_rgba_without_display() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=24,a=t,t=d,s=2,v=1,i=7", &[1, 2, 3, 4, 5, 6]));

    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 1);
    let image = t
        .graphics()
        .store()
        .get(crate::graphics::StoredImageId(1))
        .unwrap();
    assert_eq!(image.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    assert_eq!(t.take_host_output(), b"\x1b_Gi=7;OK\x1b\\");
}

#[test]
fn kitty_chunked_transmission_accumulates_until_final_chunk() {
    let mut t = Terminal::new(20, 4);
    let payload = rgba_2x1();
    let encoded = b64(&payload);
    let split = encoded.len() / 2;

    let first = format!(
        "\x1b_Gf=32,a=T,t=d,s=2,v=1,i=5,m=1;{}\x1b\\",
        &encoded[..split]
    );
    let second = format!("\x1b_Gm=0;{}\x1b\\", &encoded[split..]);
    t.advance(first.as_bytes());
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.take_host_output(), b"\x1b_Gi=5;OK\x1b\\");

    t.advance(second.as_bytes());
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert_eq!(t.take_host_output(), b"\x1b_Gi=5;OK\x1b\\");
}

#[test]
fn kitty_cursor_moves_only_when_c_flag_requests_it() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,r=2,C=1", &rgba_2x1()));

    assert_eq!(
        t.screen().cursor(),
        crate::core::Position { row: 2, column: 0 }
    );
}

#[test]
fn kitty_invalid_payload_returns_error_and_no_placement() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Gf=32,a=T,t=d,s=2,v=1;!!!!\x1b\\");

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("invalid-payload")
    );
}

#[test]
fn kitty_png_is_deferred_with_explicit_error() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Gf=100,a=T,t=d,s=1,v=1;AAAA\x1b\\");

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("unsupported-format")
    );
}

#[test]
fn kitty_file_transmission_is_deferred_with_explicit_error() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Gf=32,a=T,t=f,s=1,v=1;AAAA\x1b\\");

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("unsupported-transmission")
    );
}

#[test]
fn kitty_truncated_payload_returns_error() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1", &[1, 2, 3, 4]));

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("invalid-payload")
    );
}

#[test]
fn kitty_oversized_payload_is_rejected_by_store_cap() {
    let mut t = Terminal::new(20, 4);
    let limits = ImageStoreLimits {
        max_decoded_bytes: 4,
        max_images: 4,
    };
    *t.graphics_mut() = crate::graphics::ImageScene::new(limits);

    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1", &rgba_2x1()));

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("payload-too-large")
    );
}

#[test]
fn kitty_malformed_control_inputs_never_panic() {
    let cases: &[&[u8]] = &[
        b"\x1b_Gf,a=T;AAAA\x1b\\",
        b"\x1b_Gf=32,a=TT,s=1,v=1;AAAA\x1b\\",
        b"\x1b_Gf=32,a=T,s=x,v=1;AAAA\x1b\\",
        b"\x1b_Gf=32,a=T,s=1,v=1,m=1;AAAA\x1b\\",
        b"\x1b_Gm=0;AAAA\x1b\\",
        b"\x1b_Gf=32,a=T,s=1,v=1;A===\x1b\\",
    ];

    for case in cases {
        let mut t = Terminal::new(20, 4);
        t.advance(case);
        let _ = t.take_host_output();
        assert!(t.visible_graphics(0).is_empty());
    }
}

#[test]
fn kitty_quiet_two_suppresses_success_response() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,q=2", &rgba_2x1()));

    assert_eq!(t.visible_graphics(0).len(), 1);
    assert!(t.take_host_output().is_empty());
}

#[test]
fn ris_clears_pending_kitty_chunks() {
    let mut t = Terminal::new(20, 4);
    let encoded = b64(&rgba_2x1());
    let first = format!("\x1b_Gf=32,a=T,t=d,s=2,v=1,m=1;{}\x1b\\", &encoded[..4]);
    t.advance(first.as_bytes());
    let _ = t.take_host_output();

    t.advance(b"\x1bc");
    let second = format!("\x1b_Gm=0;{}\x1b\\", &encoded[4..]);
    t.advance(second.as_bytes());

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("unsupported-format")
    );
}

#[test]
fn kitty_alt_screen_placements_are_isolated() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1", &rgba_2x1()));
    assert_eq!(t.visible_graphics(0).len(), 1);

    t.advance(b"\x1b[?1049h");
    assert!(t.visible_graphics(0).is_empty());
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1", &rgba_2x1()));
    assert_eq!(t.visible_graphics(0).len(), 1);

    t.advance(b"\x1b[?1049l");
    assert_eq!(t.visible_graphics(0).len(), 1);
}

#[test]
fn kitty_eviction_removes_old_visible_placements() {
    let mut t = Terminal::new(20, 4);
    let limits = ImageStoreLimits {
        max_decoded_bytes: 1024,
        max_images: 1,
    };
    *t.graphics_mut() = crate::graphics::ImageScene::new(limits);

    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=1", &rgba_2x1()));
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=2", &rgba_2x1()));

    assert_eq!(t.graphics().store().len(), 1);
    assert_eq!(t.visible_graphics(0).len(), 1);
    let image = t
        .graphics()
        .store()
        .get(t.visible_graphics(0)[0].image_id)
        .unwrap();
    assert_eq!(image.protocol_id, Some(2));
}
