// SPDX-License-Identifier: GPL-3.0-only
//! Clipboard slot, paste-chunk encoding, and PTY-writer tests. (M6 mechanical split from native/tests.rs).

use super::*;

#[test]
fn clipboard_slot_retains_initialized_handle() {
    let mut slot = ClipboardSlot::default();
    let mut created = 0;

    *slot
        .get_or_try_init(|| {
            created += 1;
            Ok::<_, ()>(41)
        })
        .expect("first handle") += 1;
    let retained = *slot
        .get_or_try_init(|| {
            created += 1;
            Ok::<_, ()>(0)
        })
        .expect("retained handle");

    assert_eq!(created, 1);
    assert_eq!(retained, 42);
    assert!(slot.is_retaining_handle());
}

#[test]
fn clipboard_slot_can_drop_failed_or_stale_handle() {
    let mut slot = ClipboardSlot::default();

    let _ = slot
        .get_or_try_init(|| Ok::<_, ()>("first"))
        .expect("first handle");
    assert!(slot.is_retaining_handle());

    slot.clear();
    assert!(!slot.is_retaining_handle());

    let retained = *slot
        .get_or_try_init(|| Ok::<_, ()>("replacement"))
        .expect("replacement handle");
    assert_eq!(retained, "replacement");
}

#[test]
fn selected_text_extracts_plain_terminal_text() {
    let snapshot = snapshot(&["copy me   ", "not image "], 10);
    let range = selection::SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 6 },
    };

    assert_eq!(selection::selected_text(&snapshot, range), "copy me");
}

#[test]
fn selected_text_trims_an_all_blank_selection_to_empty() {
    let snapshot = snapshot(&["          "], 10);
    let range = selection::SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 9 },
    };

    assert!(selection::selected_text(&snapshot, range).is_empty());
}

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    flushes: Arc<Mutex<usize>>,
}

type RecordingWriterParts = (PtyWriter, Arc<Mutex<Vec<u8>>>, Arc<Mutex<usize>>);

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        *self.flushes.lock().expect("flushes") += 1;
        Ok(())
    }
}

fn recording_writer() -> RecordingWriterParts {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let flushes = recorder.flushes.clone();
    (Arc::new(Mutex::new(Box::new(recorder))), bytes, flushes)
}

#[test]
fn plain_paste_chunks_normalize_line_endings_to_carriage_return() {
    let chunks = encode_paste_chunks("one\ntwo\r\nthree\rfour", false, PASTE_CHUNK_SIZE);

    assert_eq!(&flatten_chunks(&chunks), b"one\rtwo\rthree\rfour");
}

#[test]
fn paste_chunks_split_large_plain_payload_without_data_loss() {
    let chunks = encode_paste_chunks("abcdefghi", false, 3);

    assert_eq!(
        chunks,
        vec![b"abc".to_vec(), b"def".to_vec(), b"ghi".to_vec()]
    );
    assert_eq!(&flatten_chunks(&chunks), b"abcdefghi");
}

#[test]
fn bracketed_paste_is_one_indivisible_framed_chunk() {
    // Framing atomicity: the start marker, body, and end marker travel as ONE
    // chunk regardless of the chunk-size hint, so no downstream drop-whole-
    // chunk policy (outbound queue overflow, attached-session frame drop) can
    // ever separate a marker from the body.
    let chunks = encode_paste_chunks("abcdefghi", true, 3);

    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks.first().map(Vec::as_slice),
        Some(b"\x1b[200~abcdefghi\x1b[201~".as_slice())
    );
}

#[test]
fn bracketed_paste_chunks_strip_embedded_end_marker_only_from_payload() {
    let chunks = encode_paste_chunks("safe\x1b[201~tail\r\nkept", true, 4);

    assert_eq!(chunks.len(), 1);
    assert_eq!(
        &flatten_chunks(&chunks),
        b"\x1b[200~safetail\r\nkept\x1b[201~"
    );
}

#[test]
fn write_chunks_blocking_writes_all_chunks_and_flushes_once() {
    let (writer, bytes, flushes) = recording_writer();
    let chunks = vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()];

    write_chunks_blocking(&writer, &chunks).expect("chunk write");

    assert_eq!(&*bytes.lock().expect("bytes"), b"onetwothree");
    assert_eq!(*flushes.lock().expect("flushes"), 1);
}

/// End-to-end PTY → core check: spawn a one-shot command on a real PTY,
/// pump its bytes into a `Terminal` exactly as the native pump thread does,
/// and assert the rendered snapshot contains the command's output.
///
/// `#[ignore]`d like the other live-PTY smoke test: it needs a real shell
/// and a PTY, so it is opt-in (`cargo test -- --ignored`).
#[test]
#[cfg(unix)]
#[ignore = "spawns a real shell on a PTY"]
fn pty_output_pumps_into_terminal_snapshot() {
    use std::io::Read;

    let dims = Dimensions::new(40, 10);
    let session = PtySession::spawn_shell_command(dims, "printf 'HELLO_ODYTTY'")
        .expect("spawn one-shot pty command");
    let mut reader = session.try_clone_reader().expect("clone reader");
    let mut terminal = Terminal::new(dims.columns, dims.rows);

    // Pump to EOF, mirroring the pump thread's read/advance loop.
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(len) => terminal.advance(&buffer[..len]),
            Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    assert!(
        terminal.screen().plain_text().contains("HELLO_ODYTTY"),
        "snapshot should contain the command output"
    );
}
