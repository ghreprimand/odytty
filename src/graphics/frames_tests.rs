// SPDX-License-Identifier: GPL-3.0-only
//! Frame storage, composition, and playback-timing tests for animated images.

use super::frames::{
    AnimationState, DEFAULT_FRAME_GAP_MS, FrameComposition, FrameError, FrameUpdate, ImageFrames,
    MAX_FRAMES_PER_IMAGE, MIN_PLAYBACK_GAP_MS,
};

/// A 2x2 canvas of a single opaque color.
fn canvas(color: [u8; 4]) -> Vec<u8> {
    color.repeat(4)
}

fn full_update<'a>(data: &'a [u8], gap_ms: Option<i32>) -> FrameUpdate<'a> {
    FrameUpdate {
        data,
        x: 0,
        y: 0,
        width: 2,
        height: 2,
        base_frame: None,
        edit_frame: None,
        gap_ms,
        overwrite: true,
        background: 0,
    }
}

fn frames_with_two_frames() -> ImageFrames {
    let mut frames = ImageFrames::default();
    let base = canvas([10, 20, 30, 255]);
    let second = canvas([40, 50, 60, 255]);
    frames
        .transmit_frame(&base, 2, 2, full_update(&second, Some(50)))
        .expect("second frame");
    frames
}

#[test]
fn first_frame_command_captures_the_transmitted_pixels_as_the_root_frame() {
    let frames = frames_with_two_frames();

    assert_eq!(frames.frame_count(), 2);
    // Frame 1 is the image's own pixels, captured implicitly.
    assert_eq!(frames.gap_ms(1), Some(0));
    assert_eq!(frames.gap_ms(2), Some(50));
    assert_eq!(frames.current_frame(), Some(1));
    assert_eq!(frames.current_rgba(), Some(&canvas([10, 20, 30, 255])[..]));
}

#[test]
fn new_frame_without_gap_takes_the_protocol_default_and_zero_is_ignored() {
    let mut frames = ImageFrames::default();
    let base = canvas([0, 0, 0, 255]);
    frames
        .transmit_frame(&base, 2, 2, full_update(&canvas([1, 1, 1, 255]), None))
        .expect("frame 2");
    frames
        .transmit_frame(&base, 2, 2, full_update(&canvas([2, 2, 2, 255]), Some(0)))
        .expect("frame 3");

    assert_eq!(frames.gap_ms(2), Some(DEFAULT_FRAME_GAP_MS));
    assert_eq!(frames.gap_ms(3), Some(DEFAULT_FRAME_GAP_MS));
}

#[test]
fn partial_frame_composes_over_the_named_base_frame() {
    let mut frames = ImageFrames::default();
    let base = canvas([9, 9, 9, 255]);
    // A 1x1 opaque red pixel at (1, 0), over frame 1's pixels.
    let update = FrameUpdate {
        data: &[255, 0, 0, 255],
        x: 1,
        y: 0,
        width: 1,
        height: 1,
        base_frame: Some(1),
        edit_frame: None,
        gap_ms: Some(20),
        overwrite: true,
        background: 0,
    };
    frames
        .transmit_frame(&base, 2, 2, update)
        .expect("partial frame");

    frames.set_current(2).expect("frame 2 exists");
    let pixels = frames.current_rgba().expect("pixels");
    assert_eq!(&pixels[0..4], &[9, 9, 9, 255], "untouched pixel keeps base");
    assert_eq!(&pixels[4..8], &[255, 0, 0, 255], "rectangle was written");
    assert_eq!(&pixels[8..12], &[9, 9, 9, 255], "second row keeps base");
}

#[test]
fn frame_without_base_frame_starts_from_the_background_color() {
    let mut frames = ImageFrames::default();
    let base = canvas([9, 9, 9, 255]);
    let update = FrameUpdate {
        data: &[1, 2, 3, 255],
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        base_frame: None,
        edit_frame: None,
        gap_ms: None,
        overwrite: true,
        // 0x0000ffff: opaque blue.
        background: 0x0000_ffff,
    };
    frames.transmit_frame(&base, 2, 2, update).expect("frame 2");
    frames.set_current(2).expect("frame 2 exists");

    let pixels = frames.current_rgba().expect("pixels");
    assert_eq!(&pixels[0..4], &[1, 2, 3, 255], "transmitted rectangle");
    assert_eq!(
        &pixels[4..8],
        &[0, 0, 255, 255],
        "unspecified pixels take the background color, not the base image"
    );
}

#[test]
fn alpha_blend_is_the_default_composition_and_overwrite_replaces() {
    let base = canvas([0, 0, 0, 255]);

    let mut blended = ImageFrames::default();
    let mut update = full_update(&[255, 255, 255, 128], None);
    update.width = 1;
    update.height = 1;
    update.base_frame = Some(1);
    update.overwrite = false;
    blended
        .transmit_frame(&base, 2, 2, update)
        .expect("blended frame");
    blended.set_current(2).expect("frame 2");
    let blended_pixel = &blended.current_rgba().expect("pixels")[0..4];
    assert!(
        blended_pixel[0] > 100 && blended_pixel[0] < 155,
        "half-transparent white over black lands near mid grey, got {blended_pixel:?}"
    );
    assert_eq!(blended_pixel[3], 255, "opaque destination stays opaque");

    let mut overwritten = ImageFrames::default();
    let mut update = full_update(&[255, 255, 255, 128], None);
    update.width = 1;
    update.height = 1;
    update.base_frame = Some(1);
    update.overwrite = true;
    overwritten
        .transmit_frame(&base, 2, 2, update)
        .expect("overwritten frame");
    overwritten.set_current(2).expect("frame 2");
    assert_eq!(
        &overwritten.current_rgba().expect("pixels")[0..4],
        &[255, 255, 255, 128],
        "overwrite copies source pixels verbatim, alpha included"
    );
}

#[test]
fn editing_an_existing_frame_keeps_its_pixels_as_the_canvas() {
    let mut frames = frames_with_two_frames();
    let base = canvas([10, 20, 30, 255]);
    let mut update = full_update(&[7, 7, 7, 255], Some(0));
    update.width = 1;
    update.height = 1;
    update.edit_frame = Some(2);
    let edited = frames.transmit_frame(&base, 2, 2, update).expect("edit");

    assert_eq!(edited, 2, "editing returns the frame it wrote");
    assert_eq!(frames.frame_count(), 2, "no frame was appended");
    frames.set_current(2).expect("frame 2");
    let pixels = frames.current_rgba().expect("pixels");
    assert_eq!(&pixels[0..4], &[7, 7, 7, 255], "edited rectangle");
    assert_eq!(
        &pixels[4..8],
        &[40, 50, 60, 255],
        "the rest of the edited frame is unchanged"
    );
    assert_eq!(
        frames.gap_ms(2),
        Some(50),
        "a zero gap leaves the gap alone"
    );
}

#[test]
fn frame_rectangles_outside_the_canvas_are_rejected() {
    let mut frames = ImageFrames::default();
    let base = canvas([0, 0, 0, 255]);

    let mut update = full_update(&[0, 0, 0, 255], None);
    update.x = 2;
    update.width = 1;
    update.height = 1;
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::OutOfBounds),
        "a rectangle starting at the right edge is out of bounds"
    );

    let mut update = full_update(&[0, 0, 0, 255], None);
    update.width = u32::MAX;
    update.height = 1;
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::OutOfBounds),
        "an overflowing width cannot wrap into a valid rectangle"
    );

    // Payload length must match the declared rectangle exactly.
    let update = full_update(&[0, 0, 0, 255], None);
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::OutOfBounds),
        "a 2x2 rectangle needs 16 bytes"
    );
    assert!(frames.is_empty(), "a rejected frame stores nothing");
}

#[test]
fn missing_base_or_edit_frame_is_reported_as_not_found() {
    let mut frames = frames_with_two_frames();
    let base = canvas([10, 20, 30, 255]);
    let data = canvas([1, 1, 1, 255]);

    let mut update = full_update(&data, None);
    update.base_frame = Some(9);
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::FrameNotFound)
    );

    let mut update = full_update(&data, None);
    update.edit_frame = Some(9);
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::FrameNotFound)
    );

    let mut update = full_update(&data, None);
    update.base_frame = Some(0);
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::FrameNotFound),
        "frames are numbered from one, so frame zero never resolves"
    );
}

#[test]
fn missing_frame_rejection_does_not_capture_a_root_frame() {
    let mut frames = ImageFrames::default();
    let base = canvas([10, 20, 30, 255]);
    let data = canvas([1, 1, 1, 255]);

    let mut update = full_update(&data, None);
    update.edit_frame = Some(2);
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::FrameNotFound)
    );
    assert!(frames.is_empty(), "a rejected edit stores no root frame");

    let mut update = full_update(&data, None);
    update.base_frame = Some(2);
    assert_eq!(
        frames.transmit_frame(&base, 2, 2, update),
        Err(FrameError::FrameNotFound)
    );
    assert!(frames.is_empty(), "a rejected base stores no root frame");

    let mut update = full_update(&data, None);
    update.base_frame = Some(1);
    assert_eq!(frames.transmit_frame(&base, 2, 2, update), Ok(2));
    assert_eq!(frames.frame_count(), 2, "c=1 names the captured root");
}

#[test]
fn frame_count_is_capped_per_image() {
    let mut frames = ImageFrames::default();
    let base = canvas([0, 0, 0, 255]);
    for index in 0..MAX_FRAMES_PER_IMAGE * 2 {
        let color = (index % 255) as u8;
        let data = canvas([color, color, color, 255]);
        let full_before = frames.frame_count() >= MAX_FRAMES_PER_IMAGE;
        let result = frames.transmit_frame(&base, 2, 2, full_update(&data, Some(30)));
        if full_before {
            assert_eq!(
                result,
                Err(FrameError::TooManyFrames),
                "the cap holds once the frame list is full"
            );
        } else {
            assert!(result.is_ok(), "frames below the cap are accepted");
        }
    }
    assert_eq!(frames.frame_count(), MAX_FRAMES_PER_IMAGE);
}

#[test]
fn composition_copies_a_rectangle_between_frames() {
    let mut frames = ImageFrames::default();
    let base = canvas([1, 1, 1, 255]);
    frames
        .transmit_frame(&base, 2, 2, full_update(&canvas([2, 2, 2, 255]), Some(30)))
        .expect("frame 2");

    frames
        .compose(
            2,
            2,
            FrameComposition {
                source_frame: 2,
                destination_frame: 1,
                width: 1,
                height: 1,
                destination_x: 0,
                destination_y: 0,
                source_x: 1,
                source_y: 1,
                overwrite: true,
            },
        )
        .expect("compose");

    let pixels = frames.current_rgba().expect("frame 1 pixels");
    assert_eq!(&pixels[0..4], &[2, 2, 2, 255], "source pixel was copied in");
    assert_eq!(&pixels[4..8], &[1, 1, 1, 255], "the rest is untouched");
}

#[test]
fn composition_rejects_overlapping_rectangles_within_one_frame() {
    let mut frames = frames_with_two_frames();
    let overlapping = FrameComposition {
        source_frame: 2,
        destination_frame: 2,
        width: 2,
        height: 2,
        destination_x: 0,
        destination_y: 0,
        source_x: 0,
        source_y: 0,
        overwrite: true,
    };
    assert_eq!(
        frames.compose(2, 2, overlapping),
        Err(FrameError::Overlap),
        "the protocol requires rejecting a self-overlapping composition"
    );

    let mut frames = frames_with_two_frames();
    let disjoint = FrameComposition {
        source_frame: 2,
        destination_frame: 2,
        width: 1,
        height: 1,
        destination_x: 0,
        destination_y: 0,
        source_x: 1,
        source_y: 1,
        overwrite: true,
    };
    assert!(
        frames.compose(2, 2, disjoint).is_ok(),
        "disjoint rectangles in one frame are legal"
    );
}

#[test]
fn composition_rejects_rectangles_outside_the_canvas_and_unknown_frames() {
    let mut frames = frames_with_two_frames();
    let out_of_bounds = FrameComposition {
        source_frame: 1,
        destination_frame: 2,
        width: 2,
        height: 2,
        destination_x: 1,
        destination_y: 0,
        source_x: 0,
        source_y: 0,
        overwrite: true,
    };
    assert_eq!(
        frames.compose(2, 2, out_of_bounds),
        Err(FrameError::OutOfBounds)
    );

    let unknown = FrameComposition {
        source_frame: 7,
        destination_frame: 2,
        width: 1,
        height: 1,
        destination_x: 0,
        destination_y: 0,
        source_x: 0,
        source_y: 0,
        overwrite: true,
    };
    assert_eq!(
        frames.compose(2, 2, unknown),
        Err(FrameError::FrameNotFound)
    );
}

#[test]
fn stopped_animation_schedules_nothing_and_advances_nothing() {
    let mut frames = frames_with_two_frames();

    assert_eq!(frames.next_deadline_ms(), None);
    assert!(!frames.advance(10_000));
    assert_eq!(frames.current_frame(), Some(1));
}

#[test]
fn single_frame_animation_never_schedules_a_wake() {
    let mut frames = ImageFrames::default();
    frames
        .transmit_frame(&canvas([0, 0, 0, 255]), 2, 2, {
            let mut update = full_update(&[1, 1, 1, 255], Some(20));
            update.width = 1;
            update.height = 1;
            update.edit_frame = None;
            update
        })
        .expect("frame 2");
    // Delete frames again: a frame list of one (the root only) cannot animate.
    frames.clear();
    frames
        .transmit_frame(&canvas([0, 0, 0, 255]), 2, 2, {
            let mut update = full_update(&[1, 1, 1, 255], Some(20));
            update.width = 1;
            update.height = 1;
            update.edit_frame = Some(1);
            update
        })
        .expect("edit root");
    frames.set_state(AnimationState::Running);

    assert_eq!(frames.frame_count(), 1);
    assert_eq!(frames.next_deadline_ms(), None);
    assert!(!frames.advance(100_000));
}

#[test]
fn running_animation_advances_one_frame_per_gap_and_loops() {
    let mut frames = frames_with_two_frames();
    frames.set_gap(1, 30).expect("root gap");
    frames.set_state(AnimationState::Running);

    // The first tick only phases the clock; nothing is due yet.
    assert!(!frames.advance(1_000));
    assert_eq!(frames.next_deadline_ms(), Some(1_030));
    assert_eq!(frames.current_frame(), Some(1));

    assert!(frames.advance(1_030), "frame 1's gap elapsed");
    assert_eq!(frames.current_frame(), Some(2));
    assert_eq!(
        frames.next_deadline_ms(),
        Some(1_080),
        "frame 2 gap is 50ms"
    );

    assert!(frames.advance(1_080), "frame 2's gap elapsed");
    assert_eq!(
        frames.current_frame(),
        Some(1),
        "running mode loops back to the first frame"
    );
}

#[test]
fn loading_mode_waits_at_the_last_frame_instead_of_looping() {
    let mut frames = frames_with_two_frames();
    frames.set_gap(1, 30).expect("root gap");
    frames.set_state(AnimationState::RunLoading);
    frames.advance(0);

    assert!(frames.advance(30));
    assert_eq!(frames.current_frame(), Some(2));
    assert_eq!(
        frames.next_deadline_ms(),
        None,
        "loading mode parks on the last frame with no wake scheduled"
    );
    assert!(!frames.advance(10_000), "and stays there");
    assert_eq!(frames.current_frame(), Some(2));
}

#[test]
fn loop_count_is_honored_and_stopping_resets_it() {
    let mut frames = frames_with_two_frames();
    frames.set_gap(1, 10).expect("root gap");
    frames.set_gap(2, 10).expect("frame 2 gap");
    // v=2 plays one further loop, then stops.
    frames.set_loops(2);
    frames.set_state(AnimationState::Running);
    frames.advance(0);

    assert!(frames.advance(10), "1 -> 2");
    assert!(frames.advance(20), "2 -> 1 consumes the one allowed loop");
    assert_eq!(frames.current_frame(), Some(1));
    assert!(frames.advance(30), "1 -> 2 again");
    assert!(!frames.advance(40), "no loops left, playback stops");
    assert_eq!(frames.state(), AnimationState::Stopped);
    assert_eq!(frames.current_frame(), Some(2), "stops on the last frame");
    assert_eq!(frames.next_deadline_ms(), None);

    // Stopping resets the loop budget, so a restart loops freely again.
    frames.set_state(AnimationState::Stopped);
    frames.set_state(AnimationState::Running);
    frames.set_current(1).expect("frame 1");
    frames.advance(100);
    for tick in 1..8 {
        frames.advance(100 + tick * 10);
    }
    assert_eq!(
        frames.state(),
        AnimationState::Running,
        "the loop counter was reset by the stop"
    );
}

#[test]
fn loop_count_of_one_is_infinite_and_zero_is_ignored() {
    let mut frames = frames_with_two_frames();
    frames.set_gap(1, 10).expect("root gap");
    frames.set_gap(2, 10).expect("frame 2 gap");
    frames.set_loops(3);
    frames.set_loops(0);
    frames.set_loops(1);
    frames.set_state(AnimationState::Running);
    frames.advance(0);

    for tick in 1..40 {
        frames.advance(tick * 10);
    }
    assert_eq!(
        frames.state(),
        AnimationState::Running,
        "v=1 loops forever, and v=0 did not clobber it"
    );
}

#[test]
fn gapless_frames_are_skipped_without_being_displayed() {
    let mut frames = ImageFrames::default();
    let base = canvas([1, 1, 1, 255]);
    // Frame 2 is gapless (base data only), frame 3 is displayable.
    frames
        .transmit_frame(&base, 2, 2, full_update(&canvas([2, 2, 2, 255]), Some(-1)))
        .expect("gapless frame");
    frames
        .transmit_frame(&base, 2, 2, full_update(&canvas([3, 3, 3, 255]), Some(40)))
        .expect("frame 3");
    frames.set_gap(1, 10).expect("root gap");
    frames.set_state(AnimationState::Running);
    frames.advance(0);

    assert!(frames.advance(10));
    assert_eq!(
        frames.current_frame(),
        Some(3),
        "the gapless frame was stepped over in one tick"
    );
}

#[test]
fn an_all_gapless_animation_stops_instead_of_spinning() {
    let mut frames = ImageFrames::default();
    let base = canvas([1, 1, 1, 255]);
    frames
        .transmit_frame(&base, 2, 2, full_update(&canvas([2, 2, 2, 255]), Some(-1)))
        .expect("gapless frame");
    frames.set_gap(1, -1).expect("gapless root");
    frames.set_state(AnimationState::Running);
    frames.advance(0);
    frames.advance(1);

    assert_eq!(
        frames.state(),
        AnimationState::Stopped,
        "no displayable frame exists, so playback stops rather than waking forever"
    );
    assert_eq!(frames.next_deadline_ms(), None);
}

#[test]
fn playback_gap_is_floored_so_a_hostile_gap_cannot_spin_the_loop() {
    let mut frames = frames_with_two_frames();
    frames.set_gap(1, 1).expect("root gap");
    frames.set_state(AnimationState::Running);
    frames.advance(1_000);

    assert_eq!(
        frames.next_deadline_ms(),
        Some(1_000 + MIN_PLAYBACK_GAP_MS),
        "a 1ms gap is clamped up to the floor"
    );
}

#[test]
fn deleting_frames_leaves_a_still_image_with_no_animation() {
    let mut frames = frames_with_two_frames();
    frames.set_state(AnimationState::Running);
    frames.clear();

    assert!(frames.is_empty());
    assert_eq!(frames.state(), AnimationState::Stopped);
    assert_eq!(frames.next_deadline_ms(), None);
    assert_eq!(frames.current_rgba(), None);
    assert_eq!(frames.bytes(), 0);
}

#[test]
fn frame_bytes_are_reported_for_the_store_budget() {
    let frames = frames_with_two_frames();
    // Two 2x2 RGBA frames.
    assert_eq!(frames.bytes(), 2 * 2 * 2 * 4);
    assert_eq!(
        frames.added_bytes_for(16),
        16,
        "an image that already has frames pays for one more canvas"
    );
    assert_eq!(
        ImageFrames::default().added_bytes_for(16),
        32,
        "the first frame command also pays for the root-frame copy"
    );
}
