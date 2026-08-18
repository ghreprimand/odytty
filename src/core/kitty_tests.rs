// SPDX-License-Identifier: GPL-3.0-only
use crate::graphics::{GraphicsProtocol, ImageStoreLimits};

use super::*;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;

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

fn png_rgba_2x1() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&rgba_2x1()).unwrap();
    }
    out
}

fn png_header_with_large_dimensions(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let _writer = encoder.write_header().unwrap();
    }
    out
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
    assert_eq!(control.image_number, None);
    assert_eq!(control.compression, None);
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
fn named_transport_policy_does_not_change_direct_or_chunked_bytes() {
    fn run(enabled: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut terminal = Terminal::new(20, 4);
        terminal.set_kitty_named_transports_enabled(enabled);
        terminal.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=71", &rgba_2x1()));
        let direct_response = terminal.take_host_output();

        let encoded = b64(&rgba_2x1());
        let split = encoded.len() / 2;
        let first = format!(
            "\x1b_Gf=32,a=t,t=d,s=2,v=1,i=72,m=1;{}\x1b\\",
            &encoded[..split]
        );
        let final_chunk = format!("\x1b_Gm=0;{}\x1b\\", &encoded[split..]);
        terminal.advance(first.as_bytes());
        terminal.advance(final_chunk.as_bytes());
        let chunked_response = terminal.take_host_output();
        let stored = terminal
            .graphics()
            .store()
            .get(crate::graphics::StoredImageId(2))
            .unwrap()
            .rgba
            .clone();
        (direct_response, chunked_response, stored)
    }

    assert_eq!(run(false), run(true));
}

/// C3 regression: real emitters (kitty's own `icat`, timg, term-image) split
/// large images into MANY `m=1` chunks before the final `m=0` — not just one.
/// The old accumulator errored with `malformed-control` on the SECOND `m=1`
/// chunk (`pending.is_some()` was treated as a protocol violation), so any
/// image over two chunks was rejected.
#[test]
fn kitty_chunked_transmission_accepts_many_intermediate_chunks() {
    let mut t = Terminal::new(20, 4);
    let payload = rgba_2x1();
    let encoded = b64(&payload);
    assert!(encoded.len() >= 4, "need at least 4 bytes to split 4 ways");
    let quarter = encoded.len() / 4;

    let first = format!(
        "\x1b_Gf=32,a=T,t=d,s=2,v=1,i=9,m=1;{}\x1b\\",
        &encoded[..quarter]
    );
    t.advance(first.as_bytes());
    assert_eq!(t.take_host_output(), b"\x1b_Gi=9;OK\x1b\\");

    // Two more INTERMEDIATE chunks — the shape the old code rejected.
    for part in [
        &encoded[quarter..2 * quarter],
        &encoded[2 * quarter..3 * quarter],
    ] {
        t.advance(format!("\x1b_Gm=1;{part}\x1b\\").as_bytes());
        assert!(t.visible_graphics(0).is_empty());
        let out = String::from_utf8(t.take_host_output()).unwrap();
        assert!(
            out.contains("OK"),
            "intermediate chunk must be accepted, got: {out:?}"
        );
    }

    t.advance(format!("\x1b_Gm=0;{}\x1b\\", &encoded[3 * quarter..]).as_bytes());
    assert_eq!(t.visible_graphics(0).len(), 1);
    let image = t
        .graphics()
        .store()
        .get(t.visible_graphics(0)[0].image_id)
        .unwrap();
    assert_eq!(image.rgba, rgba_2x1());
    assert_eq!(t.take_host_output(), b"\x1b_Gi=9;OK\x1b\\");
}

/// C3 budget: every intermediate-chunk append enforces the same
/// `MAX_PENDING_ENCODED_BYTES` cap the final-chunk merge does — a client
/// cannot grow the pending accumulation without bound one `m=1` chunk at a
/// time. Uses the private test seam to avoid feeding ~100 MB of base64
/// through the parser.
#[test]
fn kitty_intermediate_chunk_append_enforces_pending_budget() {
    const MAX: usize = 96 * 1024 * 1024; // MAX_PENDING_ENCODED_BYTES

    // Within budget: accepted.
    assert_eq!(
        super::kitty::test_intermediate_chunk_append(1024, 1024),
        None
    );
    // Exactly at the cap: still accepted (inclusive bound, matches the
    // final-merge check).
    assert_eq!(
        super::kitty::test_intermediate_chunk_append(MAX - 8, 8),
        None
    );
    // One byte over: rejected.
    assert_eq!(
        super::kitty::test_intermediate_chunk_append(MAX - 8, 9),
        Some("payload-too-large")
    );
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
fn kitty_huge_declared_dimensions_error_without_overflow() {
    // s=v=4294967295: `pixel_count` fits u64 (u32::MAX² ≈ 1.844e19), but the
    // follow-on `pixels * 4` / `pixels * 3` byte-size multiplies exceed u64.
    // Regression: those were unchecked, panicking in debug builds (and wrapping
    // in release, spoofing the length check). Must map to payload-too-large.
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Gf=32,a=t,t=d,s=4294967295,v=4294967295;AAAA\x1b\\");
    assert!(t.visible_graphics(0).is_empty());
    let out = String::from_utf8(t.take_host_output()).unwrap();
    assert!(out.contains("payload-too-large"), "f=32 response: {out:?}");

    // f=24 hits the `pixels * 3` sibling site the same way.
    t.advance(b"\x1b_Gf=24,a=t,t=d,s=4294967295,v=4294967295;AAAA\x1b\\");
    assert!(t.visible_graphics(0).is_empty());
    let out = String::from_utf8(t.take_host_output()).unwrap();
    assert!(out.contains("payload-too-large"), "f=24 response: {out:?}");
}

#[test]
fn kitty_png_transmit_and_display_decodes_to_rgba8() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=100,a=T,t=d,s=2,v=1,i=11", &png_rgba_2x1()));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].protocol, GraphicsProtocol::Kitty);
    assert_eq!(visible[0].display_columns, 1);
    assert_eq!(visible[0].display_rows, 1);
    let image = t.graphics().store().get(visible[0].image_id).unwrap();
    assert_eq!(image.protocol_id, Some(11));
    assert_eq!(image.width, 2);
    assert_eq!(image.height, 1);
    assert_eq!(image.rgba, rgba_2x1());
    assert_eq!(t.take_host_output(), b"\x1b_Gi=11;OK\x1b\\");
}

#[test]
fn kitty_png_chunked_transmission_accumulates_until_final_chunk() {
    let mut t = Terminal::new(20, 4);
    let encoded = b64(&png_rgba_2x1());
    let split = encoded.len() / 2;
    let first = format!("\x1b_Gf=100,a=T,t=d,i=12,m=1;{}\x1b\\", &encoded[..split]);
    let second = format!("\x1b_Gm=0;{}\x1b\\", &encoded[split..]);

    t.advance(first.as_bytes());
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.take_host_output(), b"\x1b_Gi=12;OK\x1b\\");

    t.advance(second.as_bytes());
    assert_eq!(t.visible_graphics(0).len(), 1);
    let image = t
        .graphics()
        .store()
        .get(t.visible_graphics(0)[0].image_id)
        .unwrap();
    assert_eq!(image.rgba, rgba_2x1());
    assert_eq!(t.take_host_output(), b"\x1b_Gi=12;OK\x1b\\");
}

#[test]
fn kitty_png_dimension_mismatch_returns_explicit_error() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=100,a=T,t=d,s=3,v=1", &png_rgba_2x1()));

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("dimension-mismatch")
    );
}

#[test]
fn kitty_malformed_png_returns_error_and_no_placement() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=100,a=T,t=d", b"not a png"));

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("invalid-payload")
    );
}

#[test]
fn kitty_png_header_rejects_oversized_dimensions_before_frame_decode() {
    let mut t = Terminal::new(20, 4);
    let limits = ImageStoreLimits {
        max_decoded_bytes: 1024,
        max_images: 4,
    };
    *t.graphics_mut() = crate::graphics::ImageScene::new(limits);

    t.advance(&kitty_apc(
        "f=100,a=T,t=d",
        &png_header_with_large_dimensions(10_000, 10_000),
    ));

    assert!(t.visible_graphics(0).is_empty());
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("payload-too-large")
    );
}

#[test]
fn kitty_file_transmission_rejects_invalid_path() {
    // AAAA base64-decodes to three null bytes — rejected as an invalid path.
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Gf=32,a=T,t=f,s=1,v=1;AAAA\x1b\\");

    assert!(t.visible_graphics(0).is_empty());
    let resp = String::from_utf8(t.take_host_output()).unwrap();
    assert!(
        resp.contains("EBADF") || resp.contains("EPERM") || resp.contains("EIO"),
        "transport error in response: {resp}"
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
fn kitty_quiet_two_suppresses_error_response() {
    // C19: q=2 suppresses ERROR responses too (kitty spec), not just OK ones.
    let mut t = Terminal::new(20, 4);
    // invalid-payload error with q=2: no response bytes at all.
    t.advance(b"\x1b_Gf=32,a=T,t=d,s=2,v=1,q=2;!!!!\x1b\\");
    assert!(t.visible_graphics(0).is_empty());
    assert!(
        t.take_host_output().is_empty(),
        "q=2 must suppress the error response"
    );
}

#[test]
fn kitty_quiet_on_first_chunk_suppresses_error_on_later_chunk() {
    // C19: for chunked transmissions the quiet level rides on the FIRST
    // chunk's control data; an error surfaced by a later chunk (bad payload at
    // the final merge) must honor it.
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Gf=32,a=T,t=d,s=2,v=1,q=2,m=1;AA\x1b\\");
    let _ = t.take_host_output(); // chunk ack (if any) is not under test
    t.advance(b"\x1b_Gm=0;!!!!\x1b\\"); // final chunk, undecodable payload
    assert!(t.visible_graphics(0).is_empty());
    assert!(
        t.take_host_output().is_empty(),
        "q=2 from the first chunk must suppress the final-chunk error"
    );
}

#[test]
fn kitty_quiet_one_still_reports_errors() {
    // C19 boundary: q=1 suppresses only OK responses — errors still report.
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Gf=32,a=T,t=d,s=2,v=1,q=1;!!!!\x1b\\");
    let out = String::from_utf8(t.take_host_output()).unwrap();
    assert!(out.contains("invalid-payload"), "q=1 keeps errors: {out:?}");
}

#[test]
fn kitty_quiet_one_suppresses_success_response() {
    // NF9: per the kitty spec q=1 suppresses SUCCESS responses (q=2 both).
    // The old code only suppressed at q=2, so every successful command
    // answered OK even when the client asked for quiet.
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,q=1", &rgba_2x1()));

    assert_eq!(t.visible_graphics(0).len(), 1);
    assert!(
        t.take_host_output().is_empty(),
        "q=1 must suppress the OK response"
    );
}

#[test]
fn kitty_quiet_zero_reports_success_and_errors() {
    // NF9 boundary pin: q=0 (or absent) keeps full response behavior.
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,q=0", &rgba_2x1()));
    let out = String::from_utf8(t.take_host_output()).unwrap();
    assert!(out.contains("OK"), "q=0 keeps OK responses: {out:?}");

    t.advance(b"\x1b_Gf=32,a=T,t=d,s=2,v=1;!!!!\x1b\\");
    let out = String::from_utf8(t.take_host_output()).unwrap();
    assert!(
        out.contains("invalid-payload"),
        "no quiet keeps errors: {out:?}"
    );
}

#[test]
fn kitty_quiet_one_on_first_chunk_suppresses_chunk_acks_and_final_ok() {
    // NF9: chunk acknowledgements are success responses; q=1 riding on the
    // first chunk must silence the intermediate acks AND the final OK, while
    // errors would still report (q=1 < 2).
    let mut t = Terminal::new(20, 4);
    let encoded = b64(&rgba_2x1());
    let (first, rest) = encoded.split_at(4);
    let (mid, last) = rest.split_at(4);
    t.advance(format!("\x1b_Gf=32,a=T,t=d,s=2,v=1,q=1,m=1;{first}\x1b\\").as_bytes());
    assert!(
        t.take_host_output().is_empty(),
        "q=1 must suppress the first-chunk ack"
    );
    t.advance(format!("\x1b_Gm=1;{mid}\x1b\\").as_bytes());
    assert!(
        t.take_host_output().is_empty(),
        "q=1 must suppress intermediate-chunk acks"
    );
    t.advance(format!("\x1b_Gm=0;{last}\x1b\\").as_bytes());
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert!(
        t.take_host_output().is_empty(),
        "q=1 must suppress the final OK"
    );
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

// ---------------------------------------------------------------------------
// K3: placement surface — placement ids, z-index, source crop, cell scaling,
// pixel offset, a=p display, animation/placeholder out-of-scope.
// ---------------------------------------------------------------------------

#[test]
fn parses_extended_placement_keys() {
    let (control, _payload) =
        super::kitty::test_parse_apc(b"Ga=T,f=32,s=2,v=1,x=1,y=2,w=3,h=4,X=5,Y=6,z=-7;QUJD")
            .expect("parse");
    assert_eq!(control.x, Some(1));
    assert_eq!(control.y, Some(2));
    assert_eq!(control.source_w, Some(3));
    assert_eq!(control.source_h, Some(4));
    assert_eq!(control.offset_x, Some(5));
    assert_eq!(control.offset_y, Some(6));
    assert_eq!(control.z_index, Some(-7));
}

#[test]
fn kitty_source_crop_recorded_on_placement() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,x=1,y=0,w=1,h=1,i=3",
        &rgba_2x1(),
    ));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].source.x, 1);
    assert_eq!(visible[0].source.y, 0);
    assert_eq!(visible[0].source.width, 1);
    assert_eq!(visible[0].source.height, 1);
}

#[test]
fn kitty_pixel_offset_recorded_on_placement() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,X=4,Y=7,i=3", &rgba_2x1()));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].pixel_offset_x, 4);
    assert_eq!(visible[0].pixel_offset_y, 7);
}

#[test]
fn kitty_cell_scaling_uses_c_and_r() {
    let mut t = Terminal::new(20, 8);
    // Image is 2x1 px (default cell 8x16 -> 1x1), but c=4,r=2 forces the box.
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,c=4,r=2,i=3", &rgba_2x1()));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].display_columns, 4);
    assert_eq!(visible[0].display_rows, 2);
}

#[test]
fn kitty_z_index_orders_negative_below_and_positive_above() {
    let mut t = Terminal::new(20, 4);
    // Same anchor, overlapping; z=5 transmitted first, z=-1 second.
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=1,z=5", &rgba_2x1()));
    t.advance(b"\x1b[1;1H");
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=2,z=-1", &rgba_2x1()));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 2);
    // visible_placements sorts by (z_index, generation): negative first.
    assert_eq!(visible[0].z_index, -1);
    assert_eq!(visible[1].z_index, 5);
}

#[test]
fn kitty_same_placement_id_replaces_previous() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=7,p=2", &rgba_2x1()));
    t.advance(b"\x1b[3;1H");
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=7,p=2", &rgba_2x1()));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1, "same (i,p) replaces, not appends");
    // The surviving placement is the second one (anchored at row 2).
    assert_eq!(visible[0].row, 2);
}

#[test]
fn kitty_distinct_placement_ids_coexist_for_one_image() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=7,p=1", &rgba_2x1()));
    t.advance(b"\x1b[3;1H");
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=7,p=2", &rgba_2x1()));

    assert_eq!(t.visible_graphics(0).len(), 2, "distinct p= coexist");
}

#[test]
fn kitty_display_existing_image_with_a_p() {
    let mut t = Terminal::new(20, 4);
    // Transmit only (no display), then display by protocol id with a=p.
    t.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=9", &rgba_2x1()));
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 1);

    t.advance(b"\x1b[2;3H");
    t.advance(b"\x1b_Ga=p,i=9,p=1\x1b\\");

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1, "a=p places the stored image");
    assert_eq!(visible[0].row, 1);
    assert_eq!(visible[0].column, 2);
    // No new image transmitted.
    assert_eq!(t.graphics().store().len(), 1);
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("OK")
    );
}

#[test]
fn kitty_unnumbered_a_p_spam_stays_within_placement_cap() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=9", &rgba_2x1()));

    let cap = crate::graphics::placement::MAX_IMAGE_PLACEMENTS_PER_BUFFER;
    for _ in 0..=cap {
        t.advance(b"\x1b_Ga=p,i=9\x1b\\");
    }

    assert_eq!(t.graphics().placements().len(), cap);
}

#[test]
fn kitty_display_unknown_image_with_a_p_errors() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b_Ga=p,i=404,p=1\x1b\\");
    assert!(t.visible_graphics(0).is_empty());
    assert!(!t.take_host_output().is_empty());
}

#[test]
fn kitty_delete_by_image_id_with_placement_id_targets_protocol_id() {
    // Regression: delete d=i,p= must match the Kitty protocol placement id,
    // not the internal auto-increment PlacementId. Use p= values far from the
    // internal ids (which start at 1) so a coincidental match cannot pass.
    let mut t = Terminal::new(20, 6);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=5,p=10", &rgba_2x1()));
    t.advance(b"\x1b[3;1H");
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=5,p=20", &rgba_2x1()));
    assert_eq!(t.visible_graphics(0).len(), 2);

    // Delete only placement p=10 of image i=5.
    t.advance(b"\x1b_Ga=d,d=i,i=5,p=10\x1b\\");

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1, "only the p=10 placement removed");
    assert_eq!(visible[0].row, 2, "p=20 placement (row 2) survives");
}

/// Animation actions (`a=f`, `a=a`, `a=c`) address an existing image, so with no
/// image transmitted they answer `ENOENT` rather than the old
/// "unsupported-action". Animation behavior itself lives in
/// `kitty_animation_tests`; this pins that the actions are dispatched at all,
/// and that an unknown action letter is still rejected as unsupported.
#[test]
fn kitty_animation_actions_are_dispatched_and_unknown_actions_are_not() {
    for action in ["a=f", "a=a", "a=c"] {
        let mut t = Terminal::new(20, 4);
        t.advance(&kitty_apc(
            &format!("f=32,{action},t=d,s=2,v=1,i=1"),
            &rgba_2x1(),
        ));
        assert!(t.visible_graphics(0).is_empty());
        let response = String::from_utf8(t.take_host_output()).unwrap();
        assert!(
            response.contains("ENOENT") || response.contains("malformed-control"),
            "animation action {action} reached its handler, got {response}"
        );
    }

    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=Z,t=d,s=2,v=1,i=1", &rgba_2x1()));
    assert!(
        String::from_utf8(t.take_host_output())
            .unwrap()
            .contains("unsupported-action"),
        "an unknown action letter is still rejected"
    );
}

#[test]
fn kitty_unicode_placeholder_key_creates_a_virtual_placement() {
    // U=1 makes the placement virtual: nothing is drawn at the cursor, and the
    // image waits for placeholder cells to name it. Full coverage of the
    // placeholder path lives in `placeholder_tests`.
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,i=1,U=1,c=2,r=1",
        &rgba_2x1(),
    ));
    assert!(t.visible_graphics(0).is_empty());
    assert!(t.graphics().placements().is_empty());
    assert_eq!(t.graphics().virtual_placements().len(), 1);
}

#[test]
fn kitty_unicode_placeholder_flag_other_than_one_places_normally() {
    // Only U=1 selects the virtual path; U=0 is the documented default and any
    // other value must not silently swallow the placement.
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=1,U=0", &rgba_2x1()));
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert!(t.graphics().virtual_placements().is_empty());
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

#[test]
fn kitty_display_rows_clamp_to_screen_like_columns() {
    // `r=` (and an extreme pixel height) clamps to the rows below the cursor,
    // mirroring the `c=` clamp: an attacker-chosen extent must not dwarf the
    // screen or feed unbounded values into downstream signed row arithmetic.
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[2;1H");
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,c=2,r=9999999999999,i=61",
        &rgba_2x1(),
    ));

    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(
        visible[0].display_rows, 3,
        "rows clamp to the space below the cursor (4 rows, cursor on row 2)"
    );
}

#[test]
fn kitty_extreme_display_params_survive_and_stay_bounded() {
    // Both extents huge at the bottom-right corner: each clamps to 1 (the
    // remaining cell), placement succeeds, and cursor-movement mode c=1 with
    // a huge extent still lands inside the screen.
    let mut t = Terminal::new(6, 3);
    t.advance(b"\x1b[3;6H");
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,c=9999999999999,r=9999999999999,C=0,i=62",
        &rgba_2x1(),
    ));
    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].display_columns, 1);
    assert_eq!(visible[0].display_rows, 1);
    let cursor = t.screen().cursor();
    assert!(cursor.row < 3, "cursor stays on screen: {cursor:?}");
}

fn zlib(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).expect("zlib write");
    encoder.finish().expect("zlib finish")
}

fn zlib_zeros(len: usize) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    let chunk = [0u8; 256];
    let mut remaining = len;
    while remaining > 0 {
        let n = remaining.min(chunk.len());
        encoder.write_all(&chunk[..n]).expect("zlib zeros");
        remaining -= n;
    }
    encoder.finish().expect("zlib zeros finish")
}

fn host_text(terminal: &mut Terminal) -> String {
    String::from_utf8(terminal.take_host_output()).expect("host output is utf-8")
}

/// Payload compression (`o=z`) and image-number addressing (`I=`) are
/// transport-independent: they apply identically on every platform. The
/// Windows-unsupported shared-memory transport (`t=s`) is not exercised here
/// and stays unsupported.
#[test]
fn parses_image_number_and_compression_keys() {
    let (control, _) = super::kitty::test_parse_apc(b"Gf=32,a=T,o=z,I=13,i=9;QQ==").expect("parse");
    assert_eq!(control.compression, Some('z'));
    assert_eq!(control.image_number, Some(13));
    assert_eq!(control.image_id, Some(9));
}

#[test]
fn kitty_zlib_payload_renders_the_same_pixels_as_uncompressed() {
    let mut compressed = Terminal::new(20, 4);
    compressed.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,o=z,i=42",
        &zlib(&rgba_2x1()),
    ));
    let mut plain = Terminal::new(20, 4);
    plain.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=42", &rgba_2x1()));

    assert!(
        host_text(&mut compressed).contains("OK"),
        "valid o=z must render, not be rejected as invalid-payload"
    );
    let placed = compressed.visible_graphics(0);
    assert_eq!(placed.len(), 1);
    let image = compressed
        .graphics()
        .store()
        .get(placed[0].image_id)
        .unwrap();
    assert_eq!(image.rgba, rgba_2x1());
    assert_eq!(
        image.rgba,
        plain
            .graphics()
            .store()
            .get(plain.visible_graphics(0)[0].image_id)
            .unwrap()
            .rgba
    );
}

#[test]
fn kitty_zlib_png_payload_renders() {
    let png = png_rgba_2x1();
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=100,a=T,t=d,o=z,i=7", &zlib(&png)));

    assert!(host_text(&mut t).contains("OK"));
    let placed = t.visible_graphics(0);
    assert_eq!(placed.len(), 1);
    assert_eq!(
        t.graphics().store().get(placed[0].image_id).unwrap().rgba,
        rgba_2x1()
    );
}

#[test]
fn kitty_chunked_zlib_payload_assembles_then_inflates_once() {
    let encoded = b64(&zlib(&rgba_2x1()));
    let (first, second) = encoded.split_at(encoded.len() / 2);
    let mut t = Terminal::new(20, 4);
    t.advance(format!("\x1b_Gf=32,a=T,t=d,s=2,v=1,o=z,i=3,m=1;{first}\x1b\\").as_bytes());
    t.advance(format!("\x1b_Gm=0;{second}\x1b\\").as_bytes());

    assert!(host_text(&mut t).contains("OK"));
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert_eq!(
        t.graphics()
            .store()
            .get(t.visible_graphics(0)[0].image_id)
            .unwrap()
            .rgba,
        rgba_2x1()
    );
}

#[test]
fn kitty_repeated_compression_markers_last_win_and_inflate_once() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,o=z,o=z,i=4",
        &zlib(&rgba_2x1()),
    ));
    assert!(host_text(&mut t).contains("OK"));
    assert_eq!(t.visible_graphics(0).len(), 1);
}

#[test]
fn kitty_double_compressed_payload_is_refused() {
    let nested = zlib(&zlib(&rgba_2x1()));
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,o=z,i=5", &nested));
    let out = host_text(&mut t);
    assert!(
        out.contains("invalid-payload") || out.contains("EINVAL:compressed-payload"),
        "double-zlib must not render: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
}

#[test]
fn kitty_truncated_zlib_stream_is_refused_without_a_placement() {
    let mut truncated = zlib(&rgba_2x1());
    truncated.pop();
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,o=z,i=6", &truncated));
    let out = host_text(&mut t);
    assert!(
        out.contains("EINVAL:compressed-payload"),
        "truncated zlib must refuse at inflate, not allocate pixels: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
}

#[test]
fn kitty_zlib_missing_trailer_is_refused_even_if_pixels_already_inflated() {
    // A cut-short zlib stream can produce every declared pixel before the
    // Adler-32 trailer arrives. Inflating with a reader that treats that as
    // clean EOF would answer OK. StreamEnd (terminating block + Adler-32)
    // is required; missing trailer is EINVAL:compressed-payload, store empty.
    let mut truncated = zlib(&rgba_2x1());
    assert!(
        truncated.len() > 4,
        "fixture must be large enough to drop a 4-byte trailer"
    );
    truncated.truncate(truncated.len() - 4);
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,o=z,i=14", &truncated));
    let out = host_text(&mut t);
    assert!(
        out.contains("EINVAL:compressed-payload"),
        "missing zlib trailer must not render: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
}

#[test]
fn kitty_zlib_header_with_garbage_body_is_refused() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,o=z,i=8",
        &[0x78, 0x9c, 0xff, 0x00, 0x01, 0x02],
    ));
    let out = host_text(&mut t);
    assert!(
        out.contains("EINVAL:compressed-payload"),
        "valid CMF/FLG plus junk must not inflate: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
}

#[test]
fn kitty_empty_zlib_payload_is_refused() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,o=z,i=9", &[]));
    let out = host_text(&mut t);
    assert!(
        out.contains("EINVAL:compressed-payload") || out.contains("invalid-payload"),
        "empty o=z payload must refuse: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
}

#[test]
fn kitty_unknown_compression_scheme_is_refused_not_ignored() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,o=x,i=10", &rgba_2x1()));
    let out = host_text(&mut t);
    assert!(
        out.contains("EINVAL:unsupported-compression"),
        "unknown o= must not fall through to the pixel decoder: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
}

#[test]
fn kitty_hostile_ratio_zlib_is_refused_at_the_store_budget() {
    // Bound under test: decompress_payload caps inflate at
    // ImageStoreLimits.max_decoded_bytes via Read::take(max+1). A tiny store
    // makes the refusal observable without a 64 MiB fixture: 1024 zero bytes
    // compress to a handful of bytes, expand past 64, and must not land an
    // image. Pass is refusal + empty store, not merely no-panic.
    let mut t = Terminal::new(20, 4);
    *t.graphics_mut() = crate::graphics::ImageScene::new(ImageStoreLimits {
        max_decoded_bytes: 64,
        max_images: 4,
    });
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,o=z,i=11",
        &zlib_zeros(1024),
    ));
    let out = host_text(&mut t);
    assert!(
        out.contains("EINVAL:compressed-payload"),
        "hostile-ratio inflate must reject at the store budget: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
    assert_eq!(t.graphics().store().decoded_bytes(), 0);
}

#[test]
fn kitty_zlib_declared_size_mismatch_after_inflate_is_refused() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=3,v=1,o=z,i=12",
        &zlib(&rgba_2x1()),
    ));
    let out = host_text(&mut t);
    assert!(
        out.contains("invalid-payload"),
        "inflated length must still match s*v*bpp: {out:?}"
    );
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 0);
}

// ---------------------------------------------------------------------------
// Compression across the payload shapes that are not a single raw direct blob
// ---------------------------------------------------------------------------

/// A chunked transfer is compressed as one stream spanning every chunk, not as
/// a sequence of independently compressed chunks. Inflating per chunk would
/// find no valid header on the second one, so this pins that decompression
/// happens after reassembly.
#[test]
fn kitty_compressed_payload_survives_a_chunked_transfer() {
    let compressed = zlib(&rgba_2x1());
    let encoded = b64(&compressed);
    // Split the base64 text on a 4-character boundary, as the protocol requires
    // of every chunk but the last.
    let split = (encoded.len() / 2) / 4 * 4;
    let (head, tail) = encoded.split_at(split.max(4).min(encoded.len()));

    let mut t = Terminal::new(20, 4);
    t.advance(format!("\x1b_Gf=32,a=T,t=d,s=2,v=1,o=z,i=40,m=1;{head}\x1b\\").as_bytes());
    t.advance(format!("\x1b_Gm=0;{tail}\x1b\\").as_bytes());

    assert!(host_text(&mut t).contains("OK"));
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert_eq!(
        t.graphics()
            .store()
            .get(t.visible_graphics(0)[0].image_id)
            .unwrap()
            .rgba,
        rgba_2x1()
    );
}

/// `o=z` is orthogonal to `f=`: the protocol allows compressing any format, so
/// a compressed PNG must inflate to the container and then decode normally.
#[test]
fn kitty_compressed_png_inflates_then_decodes() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=100,a=T,t=d,o=z,i=41", &zlib(&png_rgba_2x1())));

    assert!(host_text(&mut t).contains("OK"));
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert_eq!(
        t.graphics()
            .store()
            .get(t.visible_graphics(0)[0].image_id)
            .unwrap()
            .rgba,
        rgba_2x1()
    );
}

/// A query (`a=q`) must validate exactly what a transmission would accept. It
/// decodes on its own path, so it needs its own proof that the path
/// decompresses — and its own proof that validating stored nothing.
#[test]
fn kitty_query_validates_a_compressed_payload_without_storing_it() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc(
        "f=32,a=q,t=d,s=2,v=1,o=z,i=42",
        &zlib(&rgba_2x1()),
    ));

    let out = host_text(&mut t);
    assert!(
        out.contains("OK"),
        "compressed query must validate: {out:?}"
    );
    assert_eq!(t.graphics().store().len(), 0, "a query stores nothing");
}

/// The mirror of the case above: a query over a payload that does not inflate
/// must report the failure rather than answering OK for an image that could
/// never have been stored.
#[test]
fn kitty_query_reports_a_broken_compressed_payload() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc(
        "f=32,a=q,t=d,s=2,v=1,o=z,i=43",
        &[0x78, 0x9c, 0xff, 0xff],
    ));

    let out = host_text(&mut t);
    assert!(
        out.contains("EINVAL:compressed-payload"),
        "a query must not pass a payload a transmission would refuse: {out:?}"
    );
    assert_eq!(t.graphics().store().len(), 0);
}

// ---------------------------------------------------------------------------
// Terminal-assigned image ids for number-addressed transmissions
// ---------------------------------------------------------------------------

/// Extract `i=<id>` from a Kitty response.
fn reply_image_id(text: &str) -> u32 {
    let start = text.find("i=").expect("reply carries an image id") + 2;
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().expect("image id is numeric")
}

/// The point of the id echo is that the client can stop using numbers: the
/// protocol tells clients to switch to `i=` once the terminal reports an id.
/// So the assertion that matters is not that some id appears, but that the
/// reported id addresses the very image the number addressed.
#[test]
fn kitty_number_only_transmission_reports_a_usable_image_id() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,I=13,i=0", &rgba_2x1()));

    let out = host_text(&mut t);
    assert!(out.contains("I=13"), "number must be echoed back: {out:?}");
    let assigned = reply_image_id(&out);
    assert_ne!(assigned, 0, "zero is reserved for `no id`");

    let by_number = t
        .graphics()
        .find_by_image_number(13)
        .expect("image resolves by number");
    let by_id = t
        .graphics()
        .find_by_protocol_id(assigned)
        .expect("image resolves by the reported id");
    assert_eq!(
        by_number, by_id,
        "the reported id must address the same image the number does"
    );
}

/// An assigned id must not silently alias an image the client already owns,
/// because the client would then find its own image replaced by one it never
/// asked the terminal to store there.
#[test]
fn kitty_assigned_ids_avoid_client_chosen_ids() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=1", &rgba_2x1()));
    let _ = host_text(&mut t);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=2", &rgba_2x1()));
    let _ = host_text(&mut t);

    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,I=99", &rgba_2x1()));
    let assigned = reply_image_id(&host_text(&mut t));

    assert!(
        assigned != 1 && assigned != 2,
        "assigned id {assigned} collides with a client-chosen id"
    );
    // The images the client addressed by id must still be the ones it sent.
    for id in [1, 2] {
        assert!(
            t.graphics().find_by_protocol_id(id).is_some(),
            "client image i={id} must survive an assignment"
        );
    }
}

/// Numbers are explicitly not unique and a numbered transmission always creates
/// a new image, so two transmissions sharing a number must produce two distinct
/// images with two distinct ids — and the number must resolve to the newer.
#[test]
fn kitty_repeated_number_creates_a_second_image_and_resolves_newest() {
    let mut t = Terminal::new(20, 4);
    t.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,I=5", &rgba_2x1()));
    let first = reply_image_id(&host_text(&mut t));
    t.advance(&kitty_apc(
        "f=32,a=T,t=d,s=2,v=1,I=5",
        &[1, 2, 3, 4, 5, 6, 7, 8],
    ));
    let second = reply_image_id(&host_text(&mut t));

    assert_ne!(first, second, "a repeated number must not reuse the id");
    let newest = t.graphics().find_by_image_number(5).expect("resolves");
    assert_eq!(
        t.graphics().store().get(newest).unwrap().rgba,
        vec![1, 2, 3, 4, 5, 6, 7, 8],
        "the number must resolve to the most recently transmitted image"
    );
}
