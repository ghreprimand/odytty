// SPDX-License-Identifier: GPL-3.0-only
use crate::core::Terminal;
use crate::graphics::ImageStoreLimits;

fn b64(bytes: &[u8]) -> String {
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

fn kitty_apc(control: &str, rgba: &[u8]) -> Vec<u8> {
    format!("\x1b_G{control};{}\x1b\\", b64(rgba)).into_bytes()
}

fn solid_rgba(color: [u8; 4]) -> Vec<u8> {
    color.repeat(2)
}

fn terminal_with_limits(limits: ImageStoreLimits) -> Terminal {
    let mut terminal = Terminal::new(20, 4);
    *terminal.graphics_mut() = super::placement::ImageScene::new(limits);
    terminal
}

#[test]
fn same_id_retransmit_replaces_pixels_and_deletes_placements() {
    let mut terminal = terminal_with_limits(ImageStoreLimits {
        max_decoded_bytes: 16,
        max_images: 2,
    });
    let red = solid_rgba([255, 0, 0, 255]);
    let blue = solid_rgba([0, 0, 255, 255]);
    terminal.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=77,p=3", &red));
    let original = terminal
        .visible_graphics(0)
        .into_iter()
        .next()
        .expect("placement");
    assert_eq!(terminal.graphics().store().decoded_bytes(), red.len());

    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=77", &blue));
    assert!(terminal.visible_graphics(0).is_empty());
    assert!(terminal.graphics().placements().is_empty());
    assert!(terminal.graphics().virtual_placements().is_empty());
    assert_eq!(terminal.graphics().store().len(), 1);
    assert_eq!(terminal.graphics().store().decoded_bytes(), blue.len());
    let replacement_id = terminal
        .graphics()
        .find_by_protocol_id(77)
        .expect("new image");
    assert_ne!(replacement_id, original.image_id);
    assert_eq!(
        terminal
            .graphics()
            .store()
            .get(replacement_id)
            .unwrap()
            .rgba,
        blue
    );

    // A separate display command makes the replaced bytes visible again.
    terminal.advance(b"\x1b[2;3H");
    terminal.advance(b"\x1b_Ga=p,i=77,p=4\x1b\\");
    let visible = terminal.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].image_id, replacement_id);
    assert_eq!((visible[0].row, visible[0].column), (1, 2));
}

#[test]
fn same_id_replacement_reuses_quota_without_evicting_other_images() {
    let mut terminal = terminal_with_limits(ImageStoreLimits {
        max_decoded_bytes: 16,
        max_images: 2,
    });
    let red = solid_rgba([255, 0, 0, 255]);
    let green = solid_rgba([0, 255, 0, 255]);
    let blue = solid_rgba([0, 0, 255, 255]);
    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=7", &red));
    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=8", &green));
    terminal.advance(b"\x1b_Ga=p,i=7\x1b\\");

    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=7", &blue));

    assert_eq!(terminal.graphics().store().len(), 2);
    assert_eq!(terminal.graphics().store().decoded_bytes(), 16);
    assert!(terminal.graphics().find_by_protocol_id(8).is_some());
    let replacement = terminal
        .graphics()
        .find_by_protocol_id(7)
        .expect("replacement");
    assert_eq!(
        terminal.graphics().store().get(replacement).unwrap().rgba,
        blue
    );
}

#[test]
fn same_id_retransmit_removes_virtual_placements() {
    let mut terminal = Terminal::new(20, 4);
    let red = solid_rgba([255, 0, 0, 255]);
    let blue = solid_rgba([0, 0, 255, 255]);
    terminal.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=77,U=1,c=2,r=1", &red));
    assert_eq!(terminal.graphics().virtual_placements().len(), 1);

    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=77", &blue));

    assert!(terminal.graphics().virtual_placements().is_empty());
    let replacement = terminal
        .graphics()
        .find_by_protocol_id(77)
        .expect("replacement");
    assert_eq!(
        terminal.graphics().store().get(replacement).unwrap().rgba,
        blue
    );
}

#[test]
fn rejected_same_id_retransmit_preserves_old_image_and_placements() {
    let mut terminal = terminal_with_limits(ImageStoreLimits {
        max_decoded_bytes: 8,
        max_images: 2,
    });
    let red = solid_rgba([255, 0, 0, 255]);
    terminal.advance(&kitty_apc("f=32,a=T,t=d,s=2,v=1,i=77,p=3", &red));
    let original = terminal
        .visible_graphics(0)
        .into_iter()
        .next()
        .expect("placement");

    // Declared 2x1 RGBA requires eight bytes; four bytes must reject before
    // the existing same-ID image or its placement is removed.
    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=77", &[0, 0, 255, 255]));

    let visible = terminal.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].image_id, original.image_id);
    assert_eq!(terminal.graphics().store().len(), 1);
    assert_eq!(terminal.graphics().store().decoded_bytes(), red.len());
    assert_eq!(
        terminal
            .graphics()
            .store()
            .get(original.image_id)
            .unwrap()
            .rgba,
        red
    );
}

#[test]
fn image_number_only_transmit_does_not_replace_client_image_id() {
    let mut terminal = Terminal::new(20, 4);
    let red = solid_rgba([255, 0, 0, 255]);
    let blue = solid_rgba([0, 0, 255, 255]);
    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,i=7", &red));
    terminal.advance(&kitty_apc("f=32,a=t,t=d,s=2,v=1,I=55", &blue));

    let original = terminal
        .graphics()
        .find_by_protocol_id(7)
        .expect("client image");
    assert_eq!(terminal.graphics().store().get(original).unwrap().rgba, red);
    assert_eq!(terminal.graphics().store().len(), 2);
    assert!(terminal.graphics().find_by_image_number(55).is_some());
}
