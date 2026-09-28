// SPDX-License-Identifier: GPL-3.0-only
//! Snapshot restore must not let stale terminal references alias later content.

use super::*;
use crate::core::{SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal};

#[test]
fn restored_hyperlink_is_inert_or_keeps_its_original_target_after_id_reuse() {
    let mut original = Terminal::new(24, 3);
    original.advance(b"\x1b]8;;https://first.example.invalid\x07old link\x1b]8;;\x07");
    let old_id = original
        .screen()
        .cell(0, 0)
        .expect("old link cell")
        .attrs
        .hyperlink
        .expect("old cell has a link id");

    let encoded = SnapshotEnvelope::from_terminal(&original, SnapshotCaptureLimits::default())
        .encode()
        .expect("encode synthetic snapshot");
    let decoded = SnapshotEnvelope::decode(&encoded, SnapshotEnvelopeCaps::default())
        .expect("decode synthetic snapshot");
    let mut restored = Terminal::from_snapshot_envelope(&decoded).expect("restore snapshot");
    restored.advance(b"\x1b]8;;https://different.example.invalid\x07new\x1b]8;;\x07");

    let resolved = restored.hyperlink(old_id).map(|link| link.uri.as_str());
    assert_ne!(
        resolved,
        Some("https://different.example.invalid"),
        "a visible old link must never resolve to a later URL"
    );
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() >= 2 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() == 3 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn transmit_rgba(terminal: &mut Terminal, rgba: [u8; 4]) {
    let payload = base64(&rgba);
    terminal.advance(format!("\x1b_Ga=t,t=d,f=32,s=1,v=1,i=7,q=2;{payload}\x1b\\").as_bytes());
}

#[test]
fn restored_kitty_placeholder_does_not_resolve_to_a_new_image_with_reused_id() {
    let mut original = Terminal::new(12, 3);
    transmit_rgba(&mut original, [255, 0, 0, 255]);
    original.advance(b"\x1b_Ga=p,U=1,i=7,c=1,r=1,q=2\x1b\\");
    original.advance("\x1b[38;2;0;0;7m\u{10EEEE}\u{0305}\u{0305}".as_bytes());
    let placeholder = original.visible_graphics(0);
    assert_eq!(placeholder.len(), 1, "fixture contains a live placeholder");
    assert_ne!(
        placeholder[0].id.0 & (1_u64 << 63),
        0,
        "fixture placement uses the synthetic placeholder namespace"
    );

    let encoded = SnapshotEnvelope::from_terminal(&original, SnapshotCaptureLimits::default())
        .encode()
        .expect("encode synthetic snapshot");
    let decoded = SnapshotEnvelope::decode(&encoded, SnapshotEnvelopeCaps::default())
        .expect("decode synthetic snapshot");
    let mut restored = Terminal::from_snapshot_envelope(&decoded).expect("restore snapshot");
    assert!(
        restored.visible_graphics(0).is_empty(),
        "snapshot omits graphics scene"
    );

    transmit_rgba(&mut restored, [0, 0, 255, 255]);
    restored.advance(b"\x1b_Ga=p,U=1,i=7,c=1,r=1,q=2\x1b\\");

    assert!(
        restored.visible_graphics(0).is_empty(),
        "restored placeholder cells must not bind to a later image reusing protocol id 7"
    );
}

#[test]
fn stationary_pointer_does_not_keep_a_stale_url_after_terminal_text_changes() {
    let dimensions = Dimensions::new(80, 24);
    let (mut app, terminal) =
        headless_app_with(NativeOptions::default(), dimensions, Settings::default());
    terminal
        .lock()
        .expect("terminal")
        .advance(b"https://first.example.invalid");
    app.set_interactive_urls_for_test(true);
    app.set_test_cell_for_test(cell(8, 10));
    app.pointer_move_for_test(8.0 * 10.5, 10.0 * 0.5);
    assert_eq!(
        app.hovered_url_for_test(),
        Some("https://first.example.invalid"),
        "precondition: pointer resolves the original URL"
    );

    app.advance_primary_terminal_for_test(b"\rhttps://second.example.invalid");

    assert_ne!(
        app.hovered_url_for_test(),
        Some("https://first.example.invalid"),
        "content changes under a stationary pointer must invalidate the old URL target"
    );
}
