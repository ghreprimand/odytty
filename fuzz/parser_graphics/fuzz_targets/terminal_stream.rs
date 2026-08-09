// SPDX-License-Identifier: GPL-3.0-only
#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use odytty::core::{
    CharsetModes, KeyboardModes, MouseProtocol, Snapshot, SnapshotLayoutState,
    SnapshotTerminalState, Terminal,
};
use odytty::graphics::{GraphicsCommand, ImagePlacement, StoredImage, VisiblePlacement};
use support::{
    assert_graphics_bounds, assert_recovery_sentinel, bounded_terminal, host_output_cap,
};

const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
struct Observable {
    snapshot: Snapshot,
    persistent_state: SnapshotTerminalState,
    layout_state: SnapshotLayoutState,
    title: Option<String>,
    working_directory: Option<String>,
    mouse: MouseProtocol,
    keyboard: KeyboardModes,
    charsets: CharsetModes,
    bracketed_paste: bool,
    alternate_scroll: bool,
    alternate_screen: bool,
    synchronized_output: bool,
    focus_reporting: bool,
    host_output: Vec<u8>,
    visible_graphics: Vec<VisiblePlacement>,
    placements: Vec<ImagePlacement>,
    raw_commands: Vec<GraphicsCommand>,
    stored_images: Vec<StoredImage>,
    sixel_decode_errors: u64,
}

fn capture(terminal: &mut Terminal) -> Observable {
    let mut stored_images = terminal
        .graphics()
        .store()
        .iter_ids()
        .filter_map(|id| terminal.graphics().store().get(id).cloned())
        .collect::<Vec<_>>();
    stored_images.sort_by_key(|image| image.id);

    Observable {
        snapshot: terminal.snapshot(),
        persistent_state: terminal.snapshot_state(256),
        layout_state: terminal.snapshot_layout_state(),
        title: terminal.title().map(str::to_owned),
        working_directory: terminal.current_working_directory().map(str::to_owned),
        mouse: terminal.mouse_protocol(),
        keyboard: terminal.keyboard_modes(),
        charsets: terminal.charset_modes(),
        bracketed_paste: terminal.bracketed_paste_enabled(),
        alternate_scroll: terminal.alternate_scroll_enabled(),
        alternate_screen: terminal.on_alternate_screen(),
        synchronized_output: terminal.synchronized_output_enabled(),
        focus_reporting: terminal.focus_reporting(),
        host_output: terminal.take_host_output(),
        visible_graphics: terminal.visible_graphics(0),
        placements: terminal.graphics().placements().to_vec(),
        raw_commands: terminal.graphics().raw_commands().iter().cloned().collect(),
        stored_images,
        sixel_decode_errors: terminal.screen().sixel_decode_errors(),
    }
}

#[derive(Clone, Copy)]
enum Feed {
    Whole,
    Bytes,
    Chunks,
}

fn run(data: &[u8], feed: Feed) -> (Observable, Observable) {
    let mut terminal = bounded_terminal();
    match feed {
        Feed::Whole => terminal.advance(data),
        Feed::Bytes => {
            for byte in data {
                terminal.advance(std::slice::from_ref(byte));
            }
        }
        Feed::Chunks => {
            const WIDTHS: [usize; 6] = [1, 2, 3, 5, 8, 13];
            let mut offset = 0;
            let mut width = 0;
            while offset < data.len() {
                let end = offset.saturating_add(WIDTHS[width % WIDTHS.len()]);
                let end = end.min(data.len());
                terminal.advance(&data[offset..end]);
                offset = end;
                width += 1;
            }
        }
    }

    assert_graphics_bounds(&terminal);
    let before_reset = capture(&mut terminal);
    assert!(before_reset.host_output.len() <= host_output_cap(data.len()));

    assert_recovery_sentinel(&mut terminal);
    assert_graphics_bounds(&terminal);
    let after_reset = capture(&mut terminal);
    assert!(after_reset.host_output.is_empty());
    (before_reset, after_reset)
}

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let whole = run(input, Feed::Whole);
    let bytes = run(input, Feed::Bytes);
    let chunks = run(input, Feed::Chunks);

    assert_eq!(whole, bytes);
    assert_eq!(whole, chunks);
});
