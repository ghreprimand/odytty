// SPDX-License-Identifier: GPL-3.0-only
//! Kitty graphics animation at the protocol boundary: frame transmission
//! (`a=f`), animation control (`a=a`), frame composition (`a=c`), frame
//! deletion (`d=f`), quota enforcement, and the interaction with still
//! placements and Unicode-placeholder (`U=1`) display.

use crate::core::Terminal;
use crate::graphics::{AnimationState, ImageStoreLimits, MAX_FRAMES_PER_IMAGE, StoredImageId};

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

fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
    format!("\x1b_G{control};{}\x1b\\", b64(payload)).into_bytes()
}

fn apc_no_payload(control: &str) -> Vec<u8> {
    format!("\x1b_G{control}\x1b\\").into_bytes()
}

/// A 2x2 RGBA canvas of one color.
fn canvas(color: [u8; 4]) -> Vec<u8> {
    color.repeat(4)
}

/// Transmit and display a 2x2 image with protocol id 1, then drain the
/// acknowledgement so later assertions see only their own responses.
fn terminal_with_animated_image() -> Terminal {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,i=1,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    let _ = terminal.take_host_output();
    terminal
}

fn stored_id(terminal: &Terminal, protocol_id: u32) -> StoredImageId {
    terminal
        .graphics()
        .find_by_protocol_id(protocol_id)
        .expect("image is stored")
}

fn displayed_pixels(terminal: &Terminal, protocol_id: u32) -> Vec<u8> {
    let id = stored_id(terminal, protocol_id);
    terminal
        .graphics()
        .store()
        .get(id)
        .expect("image")
        .rgba
        .clone()
}

fn frame_count(terminal: &Terminal, protocol_id: u32) -> usize {
    let id = stored_id(terminal, protocol_id);
    terminal
        .graphics()
        .store()
        .get(id)
        .expect("image")
        .frames
        .frame_count()
}

#[test]
fn frame_transmission_creates_frames_and_acknowledges() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=60",
        &canvas([40, 50, 60, 255]),
    ));

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_Gi=1,r=2;OK\x1b\\",
        "the acknowledgement reports the assigned frame number"
    );
    assert_eq!(
        frame_count(&terminal, 1),
        2,
        "the root frame is captured implicitly alongside the new frame"
    );
    assert_eq!(
        displayed_pixels(&terminal, 1),
        canvas([10, 20, 30, 255]),
        "transmitting a frame does not change what is displayed"
    );
}

#[test]
fn frame_rectangle_defaults_to_the_full_image_and_partial_rectangles_compose() {
    let mut terminal = terminal_with_animated_image();
    // No s=/v=: the rectangle defaults to the image's own dimensions.
    terminal.advance(&apc("a=f,i=1,f=32,z=40", &canvas([1, 2, 3, 255])));
    // A 1x1 rectangle at (1,1) over frame 1.
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=1,v=1,x=1,y=1,c=1,X=1,z=40",
        &[9, 9, 9, 255],
    ));
    let _ = terminal.take_host_output();

    assert_eq!(frame_count(&terminal, 1), 3);

    // Make frame 3 current and confirm it is base-frame pixels plus the patch.
    terminal.advance(&apc_no_payload("a=a,i=1,c=3"));
    let pixels = displayed_pixels(&terminal, 1);
    assert_eq!(&pixels[0..4], &[10, 20, 30, 255], "base frame pixels");
    assert_eq!(&pixels[12..16], &[9, 9, 9, 255], "patched pixel");
}

#[test]
fn editing_the_displayed_frame_invalidates_the_terminal() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc("a=f,i=1,f=32,s=2,v=2,z=40", &canvas([1, 2, 3, 255])));
    let _ = terminal.take_host_output();

    let revision = terminal.render_revision();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=1,v=1,x=1,y=1,r=1,X=1,z=40",
        &[9, 9, 9, 255],
    ));

    assert!(
        terminal.render_revision() > revision,
        "editing the displayed root frame must invalidate the terminal"
    );
    assert_eq!(&displayed_pixels(&terminal, 1)[12..16], &[9, 9, 9, 255]);
}

#[test]
fn chunked_frame_transmission_assembles_one_frame() {
    let mut terminal = terminal_with_animated_image();
    let payload = b64(&canvas([7, 7, 7, 255]));
    let (first, second) = payload.split_at(8);
    terminal.advance(format!("\x1b_Ga=f,i=1,f=32,s=2,v=2,z=40,m=1;{first}\x1b\\").as_bytes());
    terminal.advance(format!("\x1b_Ga=f,m=0;{second}\x1b\\").as_bytes());
    let _ = terminal.take_host_output();

    assert_eq!(
        frame_count(&terminal, 1),
        2,
        "the chunks assembled into a single frame"
    );
    terminal.advance(&apc_no_payload("a=a,i=1,c=2"));
    assert_eq!(displayed_pixels(&terminal, 1), canvas([7, 7, 7, 255]));
}

#[test]
fn frame_chunks_require_a_f_and_intervening_commands_abort_the_upload() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc("a=f,i=1,f=32,s=2,v=2,z=40", &canvas([7, 7, 7, 255])));
    let _ = terminal.take_host_output();
    assert_eq!(frame_count(&terminal, 1), 2);

    let payload = b64(&canvas([8, 8, 8, 255]));
    let (first, second) = payload.split_at(8);
    terminal.advance(format!("\x1b_Ga=f,i=1,f=32,s=2,v=2,m=1;{first}\x1b\\").as_bytes());
    let _ = terminal.take_host_output();
    terminal.advance(&apc_no_payload("a=d,d=f,i=1,r=2"));
    assert_eq!(frame_count(&terminal, 1), 1, "delete was not swallowed");
    assert_eq!(terminal.take_host_output(), b"\x1b_Gi=1;OK\x1b\\");

    terminal.advance(format!("\x1b_Ga=f,i=1,f=32,s=2,v=2,m=1;{first}\x1b\\").as_bytes());
    let _ = terminal.take_host_output();
    terminal.advance(format!("\x1b_Gm=0;{second}\x1b\\").as_bytes());
    assert_eq!(frame_count(&terminal, 1), 1);
    assert_ne!(
        terminal.take_host_output(),
        b"\x1b_Gi=1,r=2;OK\x1b\\",
        "a continuation without a=f must not finish the frame"
    );
}

#[test]
fn frame_command_for_an_unknown_image_reports_enoent() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc("a=f,i=77,f=32,s=2,v=2", &canvas([1, 1, 1, 255])));

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;ENOENT:frame-not-found\x1b\\"
    );
}

#[test]
fn frame_rectangle_outside_the_canvas_reports_einval() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,x=1,y=0",
        &canvas([1, 1, 1, 255]),
    ));

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;EINVAL:frame-bounds\x1b\\"
    );
    assert_eq!(frame_count(&terminal, 1), 0, "nothing was stored");
}

#[test]
fn animation_control_states_and_current_frame_are_applied() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=50",
        &canvas([40, 50, 60, 255]),
    ));
    let _ = terminal.take_host_output();

    terminal.advance(&apc_no_payload("a=a,i=1,s=3,v=4,r=1,z=30"));
    assert_eq!(terminal.take_host_output(), b"\x1b_Gi=1;OK\x1b\\");
    {
        let id = stored_id(&terminal, 1);
        let frames = &terminal.graphics().store().get(id).expect("image").frames;
        assert_eq!(frames.state(), AnimationState::Running);
        assert_eq!(frames.gap_ms(1), Some(30), "r=/z= set the root frame's gap");
    }

    // c= makes a frame current, which publishes its pixels immediately.
    terminal.advance(&apc_no_payload("a=a,i=1,c=2"));
    assert_eq!(displayed_pixels(&terminal, 1), canvas([40, 50, 60, 255]));

    terminal.advance(&apc_no_payload("a=a,i=1,s=1"));
    let id = stored_id(&terminal, 1);
    assert_eq!(
        terminal
            .graphics()
            .store()
            .get(id)
            .expect("image")
            .frames
            .state(),
        AnimationState::Stopped
    );
}

#[test]
fn animation_control_for_an_image_without_frames_reports_enoent() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc_no_payload("a=a,i=1,s=3"));

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;ENOENT:frame-not-found\x1b\\",
        "an image with no frames has no animation to control"
    );
}

#[test]
fn rejected_compound_control_does_not_change_playback_state() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([40, 50, 60, 255]),
    ));
    let _ = terminal.take_host_output();

    terminal.advance(&apc_no_payload("a=a,i=1,s=3,v=4,c=99"));
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;ENOENT:frame-not-found\x1b\\"
    );
    let id = stored_id(&terminal, 1);
    assert_eq!(
        terminal
            .graphics()
            .store()
            .get(id)
            .expect("image")
            .frames
            .state(),
        AnimationState::Stopped,
        "a rejected control command is atomic"
    );
}

#[test]
fn frame_composition_copies_between_frames() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([90, 90, 90, 255]),
    ));
    let _ = terminal.take_host_output();

    // Compose a 1x1 rectangle from frame 2 at (1,1) onto frame 1 at (0,0).
    let revision = terminal.render_revision();
    terminal.advance(&apc_no_payload(
        "a=c,i=1,r=2,c=1,w=1,h=1,X=1,Y=1,x=0,y=0,C=1",
    ));
    assert_eq!(terminal.take_host_output(), b"\x1b_Gi=1;OK\x1b\\");
    assert!(
        terminal.render_revision() > revision,
        "composing into the displayed frame must invalidate the terminal"
    );

    let pixels = displayed_pixels(&terminal, 1);
    assert_eq!(
        &pixels[0..4],
        &[90, 90, 90, 255],
        "frame 1 is the displayed frame, so the composition is visible at once"
    );
    assert_eq!(&pixels[4..8], &[10, 20, 30, 255], "the rest is unchanged");
}

#[test]
fn self_overlapping_composition_reports_einval() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([90, 90, 90, 255]),
    ));
    let _ = terminal.take_host_output();

    terminal.advance(&apc_no_payload("a=c,i=1,r=2,c=2,w=2,h=2,x=0,y=0,X=0,Y=0"));
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;EINVAL:frame-overlap\x1b\\"
    );
}

#[test]
fn composition_without_both_frame_numbers_is_malformed() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([90, 90, 90, 255]),
    ));
    let _ = terminal.take_host_output();

    terminal.advance(&apc_no_payload("a=c,i=1,r=2"));
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;malformed-control\x1b\\"
    );
}

#[test]
fn frame_delete_removes_one_frame_and_promotes_the_root() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([40, 50, 60, 255]),
    ));
    terminal.advance(&apc_no_payload("a=a,i=1,s=3"));
    terminal.advance(&apc_no_payload("a=a,i=1,c=2"));
    let _ = terminal.take_host_output();
    assert_eq!(frame_count(&terminal, 1), 2);

    terminal.advance(&apc_no_payload("a=d,d=f,i=1"));
    assert_eq!(terminal.take_host_output(), b"\x1b_Gi=1;OK\x1b\\");

    assert_eq!(frame_count(&terminal, 1), 1, "only the root was deleted");
    assert!(
        terminal.graphics().has_animations(),
        "the promoted root remains stored as frame data"
    );
    assert_eq!(
        terminal.visible_graphics(0).len(),
        1,
        "the placement survives lowercase frame deletion"
    );
    assert_eq!(
        displayed_pixels(&terminal, 1),
        canvas([40, 50, 60, 255]),
        "frame 2 is promoted when the root is deleted"
    );
}

#[test]
fn frame_delete_requires_an_image_id_and_does_not_clear_other_animations() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,i=2,c=2,r=2",
        &canvas([1, 1, 1, 255]),
    ));
    terminal.advance(&apc("a=f,i=1,f=32,s=2,v=2,z=40", &canvas([2, 2, 2, 255])));
    terminal.advance(&apc("a=f,i=2,f=32,s=2,v=2,z=40", &canvas([3, 3, 3, 255])));
    let _ = terminal.take_host_output();
    assert_eq!(terminal.graphics().store().animated_ids().len(), 2);

    terminal.advance(&apc_no_payload("a=d,d=F"));

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;malformed-control\x1b\\"
    );
    assert!(terminal.graphics().has_animations());
    assert_eq!(frame_count(&terminal, 1), 2);
    assert_eq!(frame_count(&terminal, 2), 2);
}

#[test]
fn uppercase_frame_delete_removes_an_image_after_extra_frames_are_exhausted() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([40, 50, 60, 255]),
    ));
    let _ = terminal.take_host_output();

    terminal.advance(&apc_no_payload("a=d,d=F,i=1,r=2"));
    assert_eq!(frame_count(&terminal, 1), 1);
    assert_eq!(terminal.visible_graphics(0).len(), 1);

    terminal.advance(&apc_no_payload("a=d,d=F,i=1"));
    assert!(terminal.graphics().store().iter_ids().next().is_none());
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn image_delete_that_frees_data_also_drops_its_frames() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc("a=f,i=1,f=32,s=2,v=2,z=40", &canvas([2, 2, 2, 255])));
    let _ = terminal.take_host_output();
    let bytes_with_frames = terminal.graphics().store().decoded_bytes();

    terminal.advance(&apc_no_payload("a=d,d=I,i=1"));

    assert!(
        !terminal.graphics().has_animations(),
        "freeing the image frees its animation with it"
    );
    assert!(
        terminal.graphics().store().decoded_bytes() < bytes_with_frames,
        "and the frame bytes leave the budget"
    );
}

#[test]
fn a_frame_flood_cannot_exceed_the_store_byte_budget() {
    // Budget for the image plus a couple of frames only.
    let mut terminal = Terminal::new(20, 6);
    *terminal.graphics_mut() = crate::graphics::ImageScene::new(ImageStoreLimits {
        max_decoded_bytes: 4 * 16,
        max_images: 8,
    });
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,i=1,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    let _ = terminal.take_host_output();

    let mut rejections = 0;
    for index in 0..MAX_FRAMES_PER_IMAGE {
        let color = (index % 255) as u8;
        terminal.advance(&apc(
            "a=f,i=1,f=32,s=2,v=2,z=40",
            &canvas([color, color, color, 255]),
        ));
        let response = terminal.take_host_output();
        if response == b"\x1b_G;ENOSPC:frame-quota\x1b\\" {
            rejections += 1;
        }
        assert!(
            terminal.graphics().store().decoded_bytes()
                <= terminal.graphics().store().limits().max_decoded_bytes,
            "frames never push the store past its byte budget"
        );
    }
    assert!(rejections > 0, "the flood was refused, not absorbed");
    assert!(
        terminal.graphics().find_by_protocol_id(1).is_some(),
        "and the image being animated was not evicted to make room"
    );
}

#[test]
fn initial_root_edit_respects_the_shared_store_budget() {
    let mut terminal = Terminal::new(20, 6);
    *terminal.graphics_mut() = crate::graphics::ImageScene::new(ImageStoreLimits {
        max_decoded_bytes: 16,
        max_images: 8,
    });
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,i=1,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    let _ = terminal.take_host_output();

    terminal.advance(&apc("a=f,i=1,f=32,s=2,v=2,r=1", &canvas([40, 50, 60, 255])));

    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;ENOSPC:frame-quota\x1b\\"
    );
    assert_eq!(frame_count(&terminal, 1), 0);
    assert_eq!(terminal.graphics().store().decoded_bytes(), 16);
}

#[test]
fn animation_playback_advances_visible_placements_and_schedules_bounded_wakes() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=50",
        &canvas([40, 50, 60, 255]),
    ));
    terminal.advance(&apc_no_payload("a=a,i=1,s=3,r=1,z=30"));
    let _ = terminal.take_host_output();

    // First tick phases the clock; the deadline is one gap out.
    assert!(!terminal.advance_graphics_animations(1_000, 0));
    assert_eq!(
        terminal.graphics_animation_deadline_ms(0),
        Some(1_030),
        "the wake is the current frame's gap after the phase point"
    );

    let revision_before = terminal.render_revision();
    assert!(
        terminal.advance_graphics_animations(1_030, 0),
        "the frame flip is reported so the frame gate repaints"
    );
    assert_eq!(displayed_pixels(&terminal, 1), canvas([40, 50, 60, 255]));
    assert!(
        terminal.render_revision() != revision_before,
        "and the terminal is marked dirty"
    );
}

#[test]
fn no_animation_means_no_deadline_and_no_work() {
    let mut terminal = terminal_with_animated_image();

    assert!(!terminal.graphics().has_animations());
    assert_eq!(
        terminal.graphics_animation_deadline_ms(0),
        None,
        "a still image schedules no animation wake"
    );
    assert!(!terminal.advance_graphics_animations(10_000, 0));
}

#[test]
fn an_animation_with_no_visible_placement_schedules_no_deadline() {
    // Transmit without displaying (a=t), then add frames and run.
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc("a=t,f=32,t=d,s=2,v=2,i=1", &canvas([10, 20, 30, 255])));
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([40, 50, 60, 255]),
    ));
    terminal.advance(&apc_no_payload("a=a,i=1,s=3,r=1,z=30"));
    let _ = terminal.take_host_output();

    assert!(terminal.graphics().has_animations());
    assert!(
        terminal.visible_graphics(0).is_empty(),
        "nothing is placed on screen"
    );
    assert_eq!(
        terminal.graphics_animation_deadline_ms(0),
        None,
        "an animation nobody can see schedules no wake"
    );
    assert!(!terminal.advance_graphics_animations(10_000, 0));
    assert_eq!(
        displayed_pixels(&terminal, 1),
        canvas([10, 20, 30, 255]),
        "and it holds its frame instead of burning frames off-screen"
    );

    // Once displayed, the same animation does schedule a wake.
    terminal.advance(&apc_no_payload("a=p,i=1,c=2,r=2"));
    let _ = terminal.take_host_output();
    assert!(!terminal.advance_graphics_animations(1_000, 0));
    assert_eq!(terminal.graphics_animation_deadline_ms(0), Some(1_030));
}

#[test]
fn an_animation_shown_through_a_unicode_placeholder_animates_there() {
    let mut terminal = Terminal::new(20, 6);
    // Virtual placement for image 1 (Unicode-placeholder prototype).
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,i=1,U=1,c=2,r=1",
        &canvas([10, 20, 30, 255]),
    ));
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=50",
        &canvas([40, 50, 60, 255]),
    ));
    terminal.advance(&apc_no_payload("a=a,i=1,s=3,r=1,z=30"));
    let _ = terminal.take_host_output();

    // Two placeholder cells naming image 1 through the foreground color.
    terminal.advance("\x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}".as_bytes());
    assert!(
        !terminal.visible_graphics(0).is_empty(),
        "the placeholder cells resolve to a visible placement"
    );

    assert!(!terminal.advance_graphics_animations(1_000, 0));
    assert_eq!(
        terminal.graphics_animation_deadline_ms(0),
        Some(1_030),
        "placeholder-displayed images are visible for animation purposes"
    );
    assert!(terminal.advance_graphics_animations(1_030, 0));
    assert_eq!(displayed_pixels(&terminal, 1), canvas([40, 50, 60, 255]));
}

#[test]
fn still_placements_of_an_animated_image_show_the_current_frame() {
    let mut terminal = terminal_with_animated_image();
    // A second placement of the same image.
    terminal.advance(b"\x1b[4;1H");
    terminal.advance(&apc_no_payload("a=p,i=1,c=2,r=2"));
    terminal.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=50",
        &canvas([40, 50, 60, 255]),
    ));
    terminal.advance(&apc_no_payload("a=a,i=1,s=3,r=1,z=30"));
    let _ = terminal.take_host_output();

    let visible = terminal.visible_graphics(0);
    assert_eq!(visible.len(), 2, "two placements of one image");
    let generation_before = terminal
        .graphics()
        .store()
        .get(stored_id(&terminal, 1))
        .expect("image")
        .generation;

    terminal.advance_graphics_animations(1_000, 0);
    assert!(terminal.advance_graphics_animations(1_030, 0));

    let generation_after = terminal
        .graphics()
        .store()
        .get(stored_id(&terminal, 1))
        .expect("image")
        .generation;
    assert_ne!(
        generation_before, generation_after,
        "the frame flip moves the image generation once, for every placement of it"
    );
}

#[test]
fn frame_commands_do_not_disturb_a_session_without_animation() {
    let mut plain = Terminal::new(20, 6);
    plain.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,i=1,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    let plain_visible = plain.visible_graphics(0);
    let plain_revision = plain.render_revision();

    let mut animated = terminal_with_animated_image();
    animated.advance(&apc(
        "a=f,i=1,f=32,s=2,v=2,z=40",
        &canvas([40, 50, 60, 255]),
    ));
    let _ = animated.take_host_output();

    assert_eq!(
        animated.visible_graphics(0),
        plain_visible,
        "transmitting frames changes no placement geometry"
    );
    assert_eq!(
        animated.render_revision(),
        plain_revision,
        "and does not dirty the screen on its own"
    );
}

/// Image-number (`I=`) animation addressing is transport-independent and
/// applies identically on every platform. Shared-memory transmit (`t=s`) is
/// Windows-unsupported and is not used here.
fn assert_ok_echoes_number(output: &[u8], number: u32) {
    let text = String::from_utf8_lossy(output);
    assert!(
        text.contains(&format!("I={number}")),
        "reply must echo the client number: {text}"
    );
    assert!(text.contains("OK"), "expected OK, got {text}");
    assert!(
        !text.contains("EINVAL:id-and-number"),
        "number-only commands must not trip the both-present rejection: {text}"
    );
}

fn newest_image(terminal: &Terminal) -> &crate::graphics::StoredImage {
    terminal
        .graphics()
        .store()
        .iter_ids()
        .filter_map(|id| terminal.graphics().store().get(id))
        .max_by_key(|image| image.generation)
        .expect("store holds an image")
}

#[test]
fn animation_commands_address_an_image_by_number() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,I=7,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    assert_ok_echoes_number(&terminal.take_host_output(), 7);

    terminal.advance(&apc(
        "a=f,I=7,f=32,s=2,v=2,z=60",
        &canvas([40, 50, 60, 255]),
    ));
    let out = terminal.take_host_output();
    assert!(
        String::from_utf8_lossy(&out).contains("OK"),
        "a=f must resolve I=7 to the transmitted image: {}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(newest_image(&terminal).frames.frame_count(), 2);
}

#[test]
fn animation_number_reused_across_images_resolves_the_newest() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,I=7,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,I=7,c=2,r=2",
        &canvas([1, 1, 1, 255]),
    ));
    let _ = terminal.take_host_output();
    assert_eq!(terminal.graphics().store().len(), 2);

    terminal.advance(&apc("a=f,I=7,f=32,s=2,v=2,z=40", &canvas([9, 9, 9, 255])));
    assert!(String::from_utf8_lossy(&terminal.take_host_output()).contains("OK"));

    let newest = newest_image(&terminal);
    assert_eq!(newest.frames.frame_count(), 2, "newest I=7 gained a frame");
    let older_frames = terminal
        .graphics()
        .store()
        .iter_ids()
        .filter_map(|id| terminal.graphics().store().get(id))
        .filter(|image| image.generation != newest.generation)
        .map(|image| image.frames.frame_count())
        .sum::<usize>();
    assert_eq!(older_frames, 0, "the older I=7 image must be left still");
}

#[test]
fn animation_number_of_a_deleted_image_reports_enoent() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,I=7,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    let _ = terminal.take_host_output();
    terminal.advance(&apc_no_payload("a=d,d=A"));
    assert_eq!(terminal.graphics().store().len(), 0);
    // The delete acknowledges with its own `OK`; drain it so the assertion
    // below reads the frame command's reply rather than the pair concatenated.
    assert_eq!(terminal.take_host_output(), b"\x1b_G;OK\x1b\\");

    terminal.advance(&apc("a=f,I=7,f=32,s=2,v=2", &canvas([1, 1, 1, 255])));
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;ENOENT:frame-not-found\x1b\\"
    );
}

#[test]
fn animation_number_matching_nothing_reports_enoent() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc("a=f,I=99,f=32,s=2,v=2", &canvas([1, 1, 1, 255])));
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;ENOENT:frame-not-found\x1b\\"
    );
}

#[test]
fn animation_id_and_number_together_are_rejected() {
    let mut terminal = terminal_with_animated_image();
    terminal.advance(&apc("a=f,i=1,I=7,f=32,s=2,v=2", &canvas([1, 1, 1, 255])));
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;EINVAL:id-and-number\x1b\\"
    );
    assert_eq!(frame_count(&terminal, 1), 0, "the image must be untouched");
}

#[test]
fn animation_zero_id_with_a_number_addresses_by_number() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,I=7,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    let _ = terminal.take_host_output();
    // `i=0` is "absent", so this is number-only, not both-present.
    terminal.advance(&apc(
        "a=f,i=0,I=7,f=32,s=2,v=2,z=40",
        &canvas([40, 50, 60, 255]),
    ));
    let out = terminal.take_host_output();
    assert!(
        String::from_utf8_lossy(&out).contains("OK"),
        "i=0 must not shadow I=7: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn still_transmit_rejects_id_and_number_together() {
    let mut terminal = Terminal::new(20, 6);
    terminal.advance(&apc(
        "a=T,f=32,t=d,s=2,v=2,i=1,I=7,c=2,r=2",
        &canvas([10, 20, 30, 255]),
    ));
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b_G;EINVAL:id-and-number\x1b\\"
    );
    assert_eq!(terminal.graphics().store().len(), 0);
}
