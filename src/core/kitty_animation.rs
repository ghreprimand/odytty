// SPDX-License-Identifier: GPL-3.0-only
//! Kitty graphics animation commands: frame transmission (`a=f`), animation
//! control (`a=a`), and frame composition (`a=c`).
//!
//! This module is the protocol boundary only: it reads control data, resolves
//! the addressed image, decodes a frame payload through the same transport and
//! format code the still-image path uses, and hands the result to
//! [`crate::graphics::ImageScene`], which owns frame storage, composition, and
//! playback. Nothing here keeps state of its own and nothing here reads a
//! clock - playback timing is driven entirely by the render loop.
//!
//! **Key overloading.** The animation commands reuse control keys that mean
//! something else on a transmit/display command, exactly as `x`/`y` already do
//! for deletes. The mapping this module applies:
//!
//! | key | `a=f`                       | `a=a`                    | `a=c`                     |
//! |-----|-----------------------------|--------------------------|---------------------------|
//! | `c` | base frame for new frame    | frame to make current    | destination frame         |
//! | `r` | frame being edited          | frame whose gap is set   | source frame              |
//! | `z` | gap of this frame (ms)      | gap value                | unused                    |
//! | `s` | frame rectangle width       | playback state            | unused                    |
//! | `v` | frame rectangle height      | loop count                | unused                    |
//! | `x` | rectangle left in canvas    | unused                   | destination left          |
//! | `y` | rectangle top in canvas     | unused                   | destination top           |
//! | `w` | unused                      | unused                   | rectangle width           |
//! | `h` | unused                      | unused                   | rectangle height          |
//! | `X` | composition mode            | unused                   | source left               |
//! | `Y` | canvas background color     | unused                   | source top                |
//! | `C` | unused                      | unused                   | composition mode          |
//!
//! For `a=c` the protocol specification is internally inconsistent in two
//! places. Its prose and worked example make `r` the source frame and `c` the
//! destination, while the key table reverses them. One prose sentence also
//! reverses the rectangle offsets, while the worked example and key table make
//! `X`/`Y` the source origin and `x`/`y` the destination origin. This module
//! follows the worked example for both mappings.
//!
//! Windows: frame handling is platform-neutral. Frame payloads ride the same
//! transports as still images, so the shared-memory transport (`t=s`) remains
//! Unix-only for frames exactly as it is for still transmission, and every
//! other transport behaves identically on all platforms.

use crate::graphics::StoredImageId;
use crate::graphics::{
    AnimationControl, AnimationState, FrameComposition, FrameError, FrameUpdate, ImageScene,
};

use super::kitty::{ControlData, KittyError};

/// Map a frame-layer rejection onto the protocol error the client sees.
fn frame_error(error: FrameError) -> KittyError {
    match error {
        FrameError::FrameNotFound => KittyError::FrameNotFound,
        FrameError::OutOfBounds => KittyError::FrameOutOfBounds,
        FrameError::Overlap => KittyError::FrameOverlap,
        FrameError::TooManyFrames => KittyError::FrameLimit,
        FrameError::Quota => KittyError::FrameQuota,
    }
}

/// Resolve the image an animation command addresses. Every animation command
/// must name an image (`i=`), since a frame has no meaning without one.
fn resolve_image(
    graphics: &ImageScene,
    control: &ControlData,
) -> Result<StoredImageId, KittyError> {
    let protocol_id = control
        .image_id
        .filter(|id| *id != 0)
        .ok_or(KittyError::MalformedControl)?;
    graphics
        .find_by_protocol_id(protocol_id)
        .ok_or(KittyError::FrameNotFound)
}

/// `a=f` - create or edit an animation frame from transmitted pixel data.
///
/// `already_decoded` carries the RGBA pixels and their true dimensions as
/// resolved by the still-image transport/format path, so raw and PNG frames,
/// direct and chunked transfers, and file/temp/shm transports all reach frame
/// storage through one code path.
pub(super) fn process_frame(
    graphics: &mut ImageScene,
    control: &ControlData,
    rgba: Vec<u8>,
    data_width: u32,
    data_height: u32,
) -> Result<(u32, bool), KittyError> {
    let image_id = resolve_image(graphics, control)?;
    let update = FrameUpdate {
        data: &rgba,
        x: control.x.unwrap_or(0),
        y: control.y.unwrap_or(0),
        width: data_width,
        height: data_height,
        base_frame: control.frame_base.filter(|frame| *frame != 0),
        edit_frame: control.frame_target.filter(|frame| *frame != 0),
        gap_ms: control.z_index,
        overwrite: control.upper_x == Some(1),
        background: control.upper_y.unwrap_or(0),
    };
    let (frame, changed) = graphics
        .animation_transmit_frame(image_id, update)
        .map_err(frame_error)?;
    // A new background frame changes nothing on screen. Editing the current
    // frame republishes its pixels and must invalidate the terminal immediately.
    Ok((frame, changed))
}

/// `a=a` - animation control. Returns whether displayed pixels changed.
pub(super) fn process_control(
    graphics: &mut ImageScene,
    control: &ControlData,
) -> Result<bool, KittyError> {
    let image_id = resolve_image(graphics, control)?;
    let state = match control.width {
        Some(1) => Some(AnimationState::Stopped),
        Some(2) => Some(AnimationState::RunLoading),
        Some(3) => Some(AnimationState::Running),
        // `s=0` and unknown states are ignored rather than rejected: the
        // protocol treats zero as "unspecified" throughout.
        _ => None,
    };
    let request = AnimationControl {
        state,
        current_frame: control.frame_base.filter(|frame| *frame != 0),
        gap_frame: control.frame_target.filter(|frame| *frame != 0),
        gap_ms: control.z_index,
        loops: control.height,
    };
    graphics
        .animation_control(image_id, request)
        .map_err(frame_error)
}

/// `a=c` - compose a rectangle of one frame onto another.
pub(super) fn process_compose(
    graphics: &mut ImageScene,
    control: &ControlData,
) -> Result<bool, KittyError> {
    let image_id = resolve_image(graphics, control)?;
    let source_frame = control
        .frame_target
        .filter(|frame| *frame != 0)
        .ok_or(KittyError::MalformedControl)?;
    let destination_frame = control
        .frame_base
        .filter(|frame| *frame != 0)
        .ok_or(KittyError::MalformedControl)?;
    let composition = FrameComposition {
        source_frame,
        destination_frame,
        width: control.source_w.unwrap_or(0),
        height: control.source_h.unwrap_or(0),
        destination_x: control.x.unwrap_or(0),
        destination_y: control.y.unwrap_or(0),
        source_x: control.upper_x.unwrap_or(0),
        source_y: control.upper_y.unwrap_or(0),
        overwrite: control.cursor_movement == Some(1),
    };
    let changed = graphics
        .animation_compose(image_id, composition)
        .map_err(frame_error)?;
    Ok(changed)
}

/// `d=f` / `d=F` - delete one selected frame. The image id is mandatory and
/// `r=` defaults to the root. Uppercase removes the whole image only when it
/// has no extra frame data left.
pub(super) fn delete_frame(
    graphics: &mut ImageScene,
    control: &ControlData,
    free_when_exhausted: bool,
) -> Result<bool, KittyError> {
    let image_id = resolve_image(graphics, control)?;
    graphics
        .animation_delete_frame(
            image_id,
            control.frame_target.unwrap_or(1),
            free_when_exhausted,
        )
        .map_err(frame_error)
}
