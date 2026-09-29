// SPDX-License-Identifier: GPL-3.0-only
//! Single-pane image residency must be scoped to the terminal session that
//! supplied its pixels.

use std::collections::BTreeMap;

use crate::core::{SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal};
use crate::graphics::StoredImageId;

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(first >> 2) as usize] as char);
        out.push(TABLE[(((first & 3) << 4) | (second >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((second & 15) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(third & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn transmit_rgba(terminal: &mut Terminal, rgba: [u8; 4]) {
    let payload = base64(&rgba);
    terminal.advance(format!("\x1b_Ga=T,t=d,f=32,s=1,v=1,i=7;{payload}\x1b\\").as_bytes());
}

fn single_image_terminal(rgba: [u8; 4]) -> Terminal {
    let mut terminal = Terminal::new(12, 3);
    transmit_rgba(&mut terminal, rgba);
    terminal
}

#[test]
fn switching_single_pane_sessions_fetches_each_sessions_own_pixels() {
    let terminal_a = single_image_terminal([255, 0, 0, 255]);
    let terminal_b = single_image_terminal([0, 0, 255, 255]);
    let visible_a = terminal_a.visible_graphics(0);
    let visible_b = terminal_b.visible_graphics(0);
    assert_eq!(visible_a.len(), 1);
    assert_eq!(visible_b.len(), 1);
    assert_eq!(visible_a[0].image_id, visible_b[0].image_id);
    let stored_a = terminal_a
        .graphics()
        .store()
        .get(visible_a[0].image_id)
        .unwrap();
    let stored_b = terminal_b
        .graphics()
        .store()
        .get(visible_b[0].image_id)
        .unwrap();
    assert_eq!(stored_a.generation, stored_b.generation);
    assert_ne!(stored_a.rgba, stored_b.rgba);

    let cached_a = BTreeMap::from([(visible_a[0].image_id, stored_a.generation)]);
    let resident_a = super::image_layer::single_pane_resident_for(Some(10), 10, cached_a.clone());
    assert!(
        super::render_helpers::image_uploads_for_visible(&terminal_a, &visible_a, &resident_a)
            .is_empty(),
        "the owning session keeps its still image resident"
    );

    let resident_for_b =
        super::image_layer::single_pane_resident_for(Some(10), 20, cached_a.clone());
    let uploads_b =
        super::render_helpers::image_uploads_for_visible(&terminal_b, &visible_b, &resident_for_b);
    assert_eq!(
        uploads_b.len(),
        1,
        "session B must not reuse session A's cache entry"
    );
    assert_eq!(
        uploads_b[0].rgba, stored_b.rgba,
        "session B uploads its own pixels"
    );

    let cached_b = BTreeMap::from([(visible_b[0].image_id, stored_b.generation)]);
    let resident_for_a = super::image_layer::single_pane_resident_for(Some(20), 10, cached_b);
    let uploads_a =
        super::render_helpers::image_uploads_for_visible(&terminal_a, &visible_a, &resident_for_a);
    assert_eq!(
        uploads_a.len(),
        1,
        "switching back must not reuse session B's texture"
    );
    assert_eq!(
        uploads_a[0].rgba, stored_a.rgba,
        "session A uploads its own pixels"
    );

    let empty = super::image_layer::single_pane_resident_for(
        None,
        10,
        BTreeMap::from([(StoredImageId(99), 12)]),
    );
    assert!(
        empty.is_empty(),
        "an unowned cache is never attributed to a session"
    );
}

#[test]
fn restoring_a_session_does_not_reissue_its_old_image_cache_identity() {
    let mut terminal = single_image_terminal([255, 0, 0, 255]);
    let original = terminal.visible_graphics(0)[0].image_id;
    let original_generation = terminal
        .graphics()
        .store()
        .get(original)
        .unwrap()
        .generation;
    let encoded = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
        .encode()
        .expect("encode snapshot");
    let decoded = SnapshotEnvelope::decode(&encoded, SnapshotEnvelopeCaps::default())
        .expect("decode snapshot");

    terminal
        .restore_from_envelope(&decoded)
        .expect("restore snapshot");
    transmit_rgba(&mut terminal, [0, 0, 255, 255]);
    let replacement = terminal.visible_graphics(0)[0].image_id;
    let replacement_generation = terminal
        .graphics()
        .store()
        .get(replacement)
        .expect("replacement image")
        .generation;

    assert!(
        replacement > original,
        "restored session must not reuse stored id"
    );
    assert!(
        replacement_generation > original_generation,
        "restored session must not reuse renderer generation"
    );
}
