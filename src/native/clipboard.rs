// SPDX-License-Identifier: GPL-3.0-only
use std::sync::{Arc, Mutex};
use std::{io, io::Write};

use arboard::Clipboard;
#[cfg(all(
    not(test),
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
use arboard::SetExtLinux;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten")),
    not(test)
))]
use arboard::{GetExtLinux, LinuxClipboardKind};

use crate::core::{ClipboardSelection, Terminal};

use super::pty::{PASTE_CHUNK_SIZE, PtyWriter, write_chunks_blocking};

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ClipboardImagePng {
    Ready(Vec<u8>),
    TooLarge { limit: usize },
}

pub(super) struct ClipboardSlot<T> {
    handle: Option<T>,
}

impl<T> ClipboardSlot<T> {
    pub(super) fn new() -> Self {
        Self { handle: None }
    }

    pub(super) fn get_or_try_init<E>(
        &mut self,
        create: impl FnOnce() -> Result<T, E>,
    ) -> Result<&mut T, E> {
        if self.handle.is_none() {
            self.handle = Some(create()?);
        }

        Ok(self.handle.as_mut().expect("clipboard handle initialized"))
    }

    pub(super) fn clear(&mut self) {
        self.handle = None;
    }

    #[cfg(test)]
    pub(super) fn is_retaining_handle(&self) -> bool {
        self.handle.is_some()
    }
}

impl<T> Default for ClipboardSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
pub(super) struct NativeClipboard {
    // Unused under `cfg(test)`: the real-clipboard I/O that holds it is compiled
    // out of test builds (see `read_clipboard_text` / `write_clipboard_text`).
    #[cfg_attr(test, allow(dead_code))]
    slot: ClipboardSlot<Clipboard>,
    /// Test-only: when set, `write_text` returns `None` without touching the
    /// clipboard. Lets regression tests prove the Cut fail-safe path without
    /// needing a real clipboard to error.
    #[cfg(test)]
    pub(super) force_write_fail: bool,
    /// Test-only: the last text handed to `write_clipboard_text`. The real
    /// clipboard I/O is compiled out under `cfg(test)`, so this records what a
    /// write path *would* have set — letting NF21-5 prove a focused OSC 52 write
    /// reaches the clipboard while a non-focused one is discarded before it does.
    #[cfg(test)]
    pub(super) last_clipboard_write: Option<String>,
    /// Test-only text returned by clipboard reads. Production tests inject it
    /// so OSC 52 read policy can be exercised without touching the live system
    /// clipboard.
    #[cfg(test)]
    pub(super) injected_clipboard_text: Option<String>,
    /// Test-only: PNG bytes `read_image_png` returns instead of touching the real
    /// clipboard. The real image read (`arboard::Clipboard::get_image`) is
    /// compiled out under `cfg(test)` for the same reasons text I/O is — so the
    /// image paste-through (F6-i7) confirm flow can be driven from a synthetic
    /// clipboard image without a live system clipboard.
    #[cfg(test)]
    pub(super) injected_clipboard_image: Option<Vec<u8>>,
    /// Test-only: counts calls to `read_text`. A regression test uses it to prove
    /// that opening the context menu no longer probes the clipboard synchronously
    /// on the winit event-loop thread (the root of the ~12s Wayland right-click
    /// freeze).
    #[cfg(test)]
    pub(super) read_text_calls: usize,
}

pub(super) trait ClipboardSelectionIo {
    fn read_clipboard_text(&mut self) -> Option<String>;
    fn write_clipboard_text(&mut self, text: &str) -> Option<()>;
    fn read_primary_selection_text(&mut self) -> Option<String>;
    fn write_primary_selection_text(&mut self, text: &str) -> Option<()>;
}

impl ClipboardSelectionIo for NativeClipboard {
    fn read_clipboard_text(&mut self) -> Option<String> {
        // Unit tests must never reach the real system clipboard. On macOS the
        // backing NSPasteboard is main-thread-only and SIGSEGVs when the test
        // harness reads it from a worker thread (every test runs on its own
        // thread); on every platform it would also read the developer's live
        // clipboard. Tests that need real contents inject a `MockClipboard`
        // through `ClipboardSelectionIo`. Production (`not(test)`) is unchanged.
        #[cfg(test)]
        {
            self.read_text_calls += 1;
            self.injected_clipboard_text.clone()
        }
        #[cfg(not(test))]
        {
            let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
                Ok(clipboard) => clipboard,
                Err(err) => {
                    tracing::warn!("clipboard unavailable for paste: {err}");
                    return None;
                }
            };

            let result = clipboard.get_text();
            settle_text_read(&mut self.slot, result, "clipboard")
        }
    }

    fn write_clipboard_text(&mut self, text: &str) -> Option<()> {
        // See `read_clipboard_text`: tests never write the real clipboard
        // (NSPasteboard off-main-thread crash + clobbering the developer's
        // clipboard). A no-op success keeps copy paths happy; the `Cut`
        // fail-safe path is exercised separately via `force_write_fail`.
        #[cfg(test)]
        {
            self.last_clipboard_write = Some(text.to_owned());
            Some(())
        }
        #[cfg(not(test))]
        {
            let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
                Ok(clipboard) => clipboard,
                Err(err) => {
                    tracing::warn!("clipboard unavailable for copy: {err}");
                    return None;
                }
            };

            match clipboard.set_text(text.to_owned()) {
                Ok(()) => Some(()),
                Err(err) => {
                    tracing::warn!("clipboard copy failed: {err}");
                    self.slot.clear();
                    None
                }
            }
        }
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    fn read_primary_selection_text(&mut self) -> Option<String> {
        // P8: keep OSC 52 PRIMARY reads hermetic like the regular clipboard read
        // and the PRIMARY write. A unit test must never reach the real primary
        // selection (macOS main-thread NSPasteboard crash + clobbering the
        // developer's selection). Shares the injected clipboard text and read
        // counter, exactly as the PRIMARY write shares `last_clipboard_write`.
        #[cfg(test)]
        {
            self.read_text_calls += 1;
            self.injected_clipboard_text.clone()
        }
        #[cfg(not(test))]
        {
            let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
                Ok(clipboard) => clipboard,
                Err(err) => {
                    tracing::warn!("primary selection unavailable for paste: {err}");
                    return None;
                }
            };

            let result = clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text();
            settle_text_read(&mut self.slot, result, "primary selection")
        }
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    fn write_primary_selection_text(&mut self, text: &str) -> Option<()> {
        #[cfg(test)]
        {
            // Keep OSC 52 PRIMARY regressions hermetic like regular clipboard
            // writes. Production still uses the compositor-specific path below.
            self.last_clipboard_write = Some(text.to_owned());
            Some(())
        }
        #[cfg(not(test))]
        {
            let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
                Ok(clipboard) => clipboard,
                Err(err) => {
                    tracing::warn!("primary selection unavailable for copy: {err}");
                    return None;
                }
            };

            match clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text.to_owned())
            {
                Ok(()) => Some(()),
                Err(err) => {
                    tracing::warn!("primary selection copy failed: {err}");
                    self.slot.clear();
                    None
                }
            }
        }
    }

    // Platforms without an X11/Wayland-style PRIMARY selection (macOS, Android,
    // emscripten, non-unix) have no primary selection to read or write. These
    // are no-ops so selection-driven copy/paste silently falls back to the
    // regular clipboard path.
    #[cfg(not(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    )))]
    fn read_primary_selection_text(&mut self) -> Option<String> {
        None
    }

    #[cfg(not(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    )))]
    fn write_primary_selection_text(&mut self, _text: &str) -> Option<()> {
        None
    }
}

impl NativeClipboard {
    pub(super) fn read_text(&mut self) -> Option<String> {
        self.read_clipboard_text()
    }

    pub(super) fn write_text(&mut self, text: &str) -> Option<()> {
        #[cfg(test)]
        if self.force_write_fail {
            return None;
        }
        self.write_clipboard_text(text)
    }

    /// Read a clipboard image, PNG-encoded (F6-i7 / F6-NF5). Returns `None` when
    /// the clipboard holds no image, the platform has no image support, or
    /// encoding fails — the paste path then falls through to text. The bytes are
    /// re-encoded to PNG (lossless, deterministic, universally handled) from the
    /// backend's RGBA image so the transfer format is fixed regardless of the
    /// source. As with the text paths, unit tests never reach the real clipboard
    /// (off-main-thread NSPasteboard crash + clobbering the developer's live
    /// clipboard); they inject bytes through `injected_clipboard_image`.
    pub(super) fn read_image_png(&mut self) -> Option<ClipboardImagePng> {
        #[cfg(test)]
        {
            self.injected_clipboard_image
                .clone()
                .map(ClipboardImagePng::Ready)
        }
        #[cfg(not(test))]
        {
            let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
                Ok(clipboard) => clipboard,
                Err(err) => {
                    tracing::warn!("clipboard unavailable for image paste: {err}");
                    return None;
                }
            };
            let result = clipboard.get_image();
            let image = settle_image_read(&mut self.slot, result)?;
            encode_rgba_to_png(image.width, image.height, &image.bytes)
        }
    }

    pub(super) fn read_primary_text(&mut self) -> Option<String> {
        self.read_primary_selection_text()
    }

    pub(super) fn write_primary_text(&mut self, text: &str) -> Option<()> {
        self.write_primary_selection_text(text)
    }
}

/// Settle one image read from the clipboard. No image on the clipboard (a
/// text or empty clipboard) is the common case: it keeps the cached handle
/// and logs at debug. Any other backend error invalidates the handle, as a
/// text read does, so the next image paste reconnects instead of reusing a
/// failed backend.
fn settle_image_read<T, V>(
    slot: &mut ClipboardSlot<T>,
    result: Result<V, arboard::Error>,
) -> Option<V> {
    match result {
        Ok(image) => Some(image),
        Err(arboard::Error::ContentNotAvailable) => {
            tracing::debug!("clipboard holds no image");
            None
        }
        Err(err) => {
            tracing::warn!("clipboard image read failed: {err}");
            slot.clear();
            None
        }
    }
}

/// Settle one text read from the clipboard or the PRIMARY selection. An empty
/// selection reports `ContentNotAvailable`, which is routine (middle-click on
/// an empty PRIMARY, paste from an empty clipboard), so it keeps the cached
/// handle and logs at debug: dropping the handle forces a fresh compositor
/// connection on the next read. Only a genuine backend error invalidates the
/// handle and warrants a warning. Both read paths share this rule.
fn settle_text_read<T>(
    slot: &mut ClipboardSlot<T>,
    result: Result<String, arboard::Error>,
    source: &str,
) -> Option<String> {
    match result {
        Ok(text) => Some(text),
        Err(arboard::Error::ContentNotAvailable) => {
            tracing::debug!("{source} empty on paste read");
            None
        }
        Err(err) => {
            tracing::warn!("{source} paste failed: {err}");
            slot.clear();
            None
        }
    }
}

/// Encode a raw RGBA8 image (as `arboard` hands back) to bounded PNG bytes.
/// Malformed buffers and encoder errors degrade to a non-image paste. A valid
/// image over either processing ceiling is reported separately so the caller
/// can explain the refusal without retaining the oversized payload.
#[cfg_attr(test, allow(dead_code))]
fn encode_rgba_to_png(width: usize, height: usize, rgba: &[u8]) -> Option<ClipboardImagePng> {
    encode_rgba_to_png_with_limits(
        width,
        height,
        rgba,
        super::image_decode::MAX_IMAGE_DIM,
        super::image_decode::MAX_IMAGE_ALLOC_BYTES,
        crate::settings::REMOTE_IMAGE_PASTE_MAX_BYTES,
    )
}

fn encode_rgba_to_png_with_limits(
    width: usize,
    height: usize,
    rgba: &[u8],
    max_dimension: u32,
    max_raw_bytes: u64,
    max_png_bytes: usize,
) -> Option<ClipboardImagePng> {
    use image::ImageEncoder;
    use image::codecs::png::PngEncoder;

    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    let expected = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    let rgba_len = u64::try_from(rgba.len()).ok()?;
    if width > max_dimension
        || height > max_dimension
        || expected > max_raw_bytes
        || rgba_len > max_raw_bytes
    {
        return Some(ClipboardImagePng::TooLarge {
            limit: usize::try_from(max_raw_bytes).unwrap_or(usize::MAX),
        });
    }
    if rgba_len != expected {
        return None;
    }

    let mut png = CappedPng::new(max_png_bytes);
    let encode_result =
        PngEncoder::new(&mut png).write_image(rgba, width, height, image::ExtendedColorType::Rgba8);
    if png.overflowed || png.bytes.len() > max_png_bytes {
        return Some(ClipboardImagePng::TooLarge {
            limit: max_png_bytes,
        });
    }
    encode_result.ok()?;
    Some(ClipboardImagePng::Ready(png.bytes))
}

struct CappedPng {
    bytes: Vec<u8>,
    limit: usize,
    overflowed: bool,
}

impl CappedPng {
    fn new(max_png_bytes: usize) -> Self {
        let limit = max_png_bytes.saturating_add(1);
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
            overflowed: false,
        }
    }
}

impl Write for CappedPng {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if remaining == 0 {
            self.overflowed = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "encoded clipboard image exceeds its size limit",
            ));
        }
        let accepted = remaining.min(buffer.len());
        self.bytes.extend_from_slice(&buffer[..accepted]);
        if accepted < buffer.len() {
            self.overflowed = true;
        }
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn write_clipboard_selection(
    clipboard: &mut impl ClipboardSelectionIo,
    selection: ClipboardSelection,
    text: &str,
) -> Option<()> {
    match selection {
        ClipboardSelection::Clipboard => clipboard.write_clipboard_text(text),
        ClipboardSelection::Primary => clipboard.write_primary_selection_text(text),
    }
}

pub(super) fn read_clipboard_selection(
    clipboard: &mut impl ClipboardSelectionIo,
    selection: ClipboardSelection,
) -> Option<String> {
    match selection {
        ClipboardSelection::Clipboard => clipboard.read_clipboard_text(),
        ClipboardSelection::Primary => clipboard.read_primary_selection_text(),
    }
}

/// Hard size ceiling for one bracketed paste (start marker + sanitized body +
/// end marker). A bracketed paste travels as a single indivisible write so its
/// framing can never tear (see [`encode_paste_chunks`]). The ceiling equals the
/// attach protocol's client input limit, so the one policy holds for local and
/// attached sessions alike: a paste the UI accepts always fits one attached
/// input frame, and the host queue delivers that newest frame whole. A paste
/// over the cap is refused whole, since delivering a truncated body would
/// silently corrupt the pasted content.
pub(in crate::native) const MAX_BRACKETED_PASTE_BYTES: usize =
    crate::session_host::protocol::MAX_CLIENT_INPUT_LEN;

/// Why a paste was not delivered.
#[derive(Debug)]
pub(in crate::native) enum PasteError {
    /// Refused before encoding: the framed bracketed paste would exceed
    /// [`MAX_BRACKETED_PASTE_BYTES`]. Nothing was written.
    TooLarge { len: usize, max: usize },
    /// The session writer failed (closed or lost input).
    Write(io::Error),
}

/// Upper bound of the framed bracketed-paste length for `text`, computed with
/// checked arithmetic before any copy. Sanitizing only removes bytes, so the
/// encoded paste is never longer than this. `None` on overflow.
fn bracketed_paste_upper_bound(text: &str) -> Option<usize> {
    text.len()
        .checked_add(BRACKETED_PASTE_START.len())?
        .checked_add(BRACKETED_PASTE_END.len())
}

pub(super) fn write_paste_text(
    terminal: &Arc<Mutex<Terminal>>,
    writer: &PtyWriter,
    text: &str,
) -> Result<(), PasteError> {
    // The child's mode is read through the shared poison-recovery policy, the
    // same reading the paste confirmation used, so a poisoned model never
    // downgrades a bracketed paste to plain bytes.
    let bracketed_paste = crate::native::lock_recover(terminal).bracketed_paste_enabled();
    // The size check runs on the source text BEFORE encoding, so an oversized
    // clipboard payload is never duplicated just to be refused.
    if bracketed_paste {
        let len = bracketed_paste_upper_bound(text).unwrap_or(usize::MAX);
        if len > MAX_BRACKETED_PASTE_BYTES {
            tracing::warn!(
                "bracketed paste refused: {len} bytes exceeds the {MAX_BRACKETED_PASTE_BYTES} byte limit",
            );
            return Err(PasteError::TooLarge {
                len,
                max: MAX_BRACKETED_PASTE_BYTES,
            });
        }
    }
    if bracketed_paste {
        let chunks = encode_paste_chunks(text, true, PASTE_CHUNK_SIZE);
        return write_chunks_blocking(writer, &chunks).map_err(PasteError::Write);
    }
    // Plain paste streams: one normalized chunk is built and written at a
    // time, so a long clipboard text is never copied whole (normalized) and
    // then again (chunked) before it reaches the bounded outbound queue. The
    // chunks are byte-for-byte the ones `encode_paste_chunks` produces.
    let Ok(mut sink) = writer.lock() else {
        return Err(PasteError::Write(io::Error::other(
            "pty writer lock poisoned",
        )));
    };
    for_each_plain_paste_chunk(text, PASTE_CHUNK_SIZE, |chunk| sink.write_all(chunk))
        .and_then(|()| sink.flush())
        .map_err(PasteError::Write)
}

/// Normalize `text` for a plain paste (CRLF and LF become CR) and hand it to
/// `emit` in consecutive chunks of `chunk_size` bytes, the last possibly
/// shorter, holding at most one chunk at a time.
fn for_each_plain_paste_chunk(
    text: &str,
    chunk_size: usize,
    mut emit: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let chunk_size = chunk_size.max(1);
    let mut chunk = Vec::with_capacity(chunk_size.min(text.len()));
    let mut bytes = text.as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        let out = match byte {
            b'\r' => {
                if bytes.peek() == Some(&b'\n') {
                    bytes.next();
                }
                b'\r'
            }
            b'\n' => b'\r',
            _ => byte,
        };
        chunk.push(out);
        if chunk.len() == chunk_size {
            emit(&chunk)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        emit(&chunk)?;
    }
    Ok(())
}

pub(super) fn encode_paste_chunks(
    text: &str,
    bracketed_paste: bool,
    chunk_size: usize,
) -> Vec<Vec<u8>> {
    let chunk_size = chunk_size.max(1);
    if bracketed_paste {
        // The whole framed paste — start marker, sanitized body, end marker —
        // is one indivisible chunk. Every downstream sink treats one write as
        // one atomic unit: the outbound queue's drop-oldest overflow policy
        // drops whole chunks (so a paste is retained or discarded entire,
        // never torn at a marker boundary), and the attached-session input
        // writer sends one write as one protocol frame (so a transient
        // send-timeout drop likewise discards the paste whole). Splitting the
        // markers and body into separate writes let an overflow drop the start
        // marker while keeping the tail: the surviving newlines then arrived
        // OUTSIDE bracketed-paste mode and could execute in the shell.
        let mut paste = Vec::with_capacity(
            BRACKETED_PASTE_START.len() + text.len() + BRACKETED_PASTE_END.len(),
        );
        paste.extend_from_slice(BRACKETED_PASTE_START);
        paste.extend_from_slice(&sanitize_bracketed_paste(text.as_bytes()));
        paste.extend_from_slice(BRACKETED_PASTE_END);
        vec![paste]
    } else {
        // Plain paste has no framing to protect; it stays chunked so the
        // bounded outbound queue can shed oldest chunks under a wedged
        // consumer (documented bounded degradation) without pinning the whole
        // payload.
        let mut chunks = Vec::new();
        let _ = for_each_plain_paste_chunk(text, chunk_size, |chunk| {
            chunks.push(chunk.to_vec());
            Ok(())
        });
        chunks
    }
}

#[cfg(test)]
pub(super) fn flatten_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    chunks.iter().flatten().copied().collect()
}

/// Strip every embedded bracketed-paste end marker from clipboard bytes.
///
/// Twin of [`crate::input::sanitize_paste`]. A naive forward scan does not
/// converge: deleting one marker can splice the surrounding bytes into a new
/// one (`ESC[2` + `ESC[201~` + `01~` collapses back to `ESC[201~`). This scan
/// checks the growing OUTPUT tail after every byte, so a marker reassembled by
/// a prior deletion is removed in the same linear pass, reaching a fixed point
/// with no `BRACKETED_PASTE_END` surviving in the output.
fn sanitize_bracketed_paste(text: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(text.len());
    for &byte in text {
        output.push(byte);
        if output.ends_with(BRACKETED_PASTE_END) {
            output.truncate(output.len() - BRACKETED_PASTE_END.len());
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty PRIMARY or clipboard read keeps the retained handle; only a
    /// backend error drops it. Both production read paths go through
    /// `settle_text_read`, which is the part a unit test can reach.
    #[test]
    fn empty_text_reads_keep_the_clipboard_handle_and_errors_drop_it() {
        let mut slot = ClipboardSlot::<u8>::new();
        slot.get_or_try_init(|| Ok::<u8, ()>(1)).unwrap();
        for source in ["clipboard", "primary selection"] {
            assert_eq!(
                settle_text_read(&mut slot, Err(arboard::Error::ContentNotAvailable), source),
                None
            );
            assert!(
                slot.is_retaining_handle(),
                "{source}: empty keeps the handle"
            );
        }
        assert_eq!(
            settle_text_read(&mut slot, Ok("text".to_owned()), "primary selection"),
            Some("text".to_owned())
        );
        assert!(slot.is_retaining_handle());
        assert_eq!(
            settle_text_read(
                &mut slot,
                Err(arboard::Error::ClipboardOccupied),
                "primary selection"
            ),
            None
        );
        assert!(
            !slot.is_retaining_handle(),
            "a backend error drops the handle"
        );
    }

    /// An image read that finds no image keeps the handle so the text fallback
    /// reuses it; a genuine backend error drops it, and the next read starts a
    /// fresh handle that can succeed.
    #[test]
    fn image_reads_drop_the_handle_only_on_a_backend_error() {
        let mut slot = ClipboardSlot::<u8>::new();
        slot.get_or_try_init(|| Ok::<u8, ()>(1)).unwrap();
        assert_eq!(
            settle_image_read::<u8, u8>(&mut slot, Err(arboard::Error::ContentNotAvailable)),
            None
        );
        assert!(slot.is_retaining_handle(), "no image keeps the handle");
        assert_eq!(
            settle_image_read::<u8, u8>(&mut slot, Err(arboard::Error::ClipboardOccupied)),
            None
        );
        assert!(!slot.is_retaining_handle(), "a backend error drops it");
        let fresh = slot.get_or_try_init(|| Ok::<u8, ()>(2)).copied();
        assert_eq!(fresh, Ok(2), "the next read creates a new handle");
        assert_eq!(settle_image_read::<u8, u8>(&mut slot, Ok(7)), Some(7));
        assert!(slot.is_retaining_handle());
    }

    /// Plain paste written through the streaming path is byte-for-byte and
    /// chunk-for-chunk what the chunk encoder produces, including CRLF pairs
    /// that straddle a chunk boundary.
    #[test]
    fn plain_paste_streams_the_encoder_chunks() {
        for (text, size) in [
            ("a\r\nb\nc\rd", 1),
            ("ab\r\ncd", 2),
            ("abc\r\n", 3),
            ("\r\n\r\n\n\r", 2),
            ("", 4),
            ("x", 4),
        ] {
            let mut streamed: Vec<Vec<u8>> = Vec::new();
            for_each_plain_paste_chunk(text, size, |chunk| {
                assert!(chunk.len() <= size && !chunk.is_empty());
                streamed.push(chunk.to_vec());
                Ok(())
            })
            .unwrap();
            assert_eq!(streamed, encode_paste_chunks(text, false, size), "{text:?}");
            let joined: Vec<u8> = streamed.concat();
            let expected = text.replace("\r\n", "\r").replace('\n', "\r");
            assert_eq!(joined, expected.as_bytes(), "{text:?}");
        }
    }

    /// Twin of `input::sanitize_paste`: deleting a match can reassemble a fresh
    /// end marker from the surrounding bytes.
    #[test]
    fn sanitize_bracketed_paste_rejects_reassembled_end_marker() {
        let input = b"\x1b[2\x1b[201~01~";
        let sanitized = sanitize_bracketed_paste(input);
        assert!(
            !sanitized
                .windows(6)
                .any(|window| window == BRACKETED_PASTE_END),
            "sanitized body must not contain the paste-end marker; got {sanitized:?}"
        );
    }

    #[test]
    fn sanitize_bracketed_paste_realistic_spliced_payload_leaves_no_end_marker() {
        let mut input = Vec::from(b"echo SAFE");
        input.extend_from_slice(b"\x1b[2");
        input.extend_from_slice(BRACKETED_PASTE_END);
        input.extend_from_slice(b"01~");
        input.extend_from_slice(b"id; echo PWNED\n");
        let sanitized = sanitize_bracketed_paste(&input);
        assert!(
            !sanitized
                .windows(6)
                .any(|window| window == BRACKETED_PASTE_END),
            "spliced payload must not retain ESC[201~; got {sanitized:?}"
        );
    }

    #[test]
    fn sanitize_bracketed_paste_reaches_a_fixed_point_over_end_marker_alphabet() {
        const ALPHABET: &[u8] = b"\x1b[201~";
        fn enumerate(prefix: &mut Vec<u8>, max_len: usize, check: &mut dyn FnMut(&[u8])) {
            check(prefix);
            if prefix.len() >= max_len {
                return;
            }
            for &byte in ALPHABET {
                prefix.push(byte);
                enumerate(prefix, max_len, check);
                prefix.pop();
            }
        }
        let mut failures = Vec::new();
        let mut check = |input: &[u8]| {
            let once = sanitize_bracketed_paste(input);
            let twice = sanitize_bracketed_paste(&once);
            if once != twice || once.windows(6).any(|window| window == BRACKETED_PASTE_END) {
                failures.push((input.to_vec(), once, twice));
            }
        };
        check(b"\x1b[2\x1b[201~01~");
        enumerate(&mut Vec::new(), 7, &mut check);
        assert!(
            failures.is_empty(),
            "sanitize_bracketed_paste must be a fixed point with no residual end marker; first failure input={:?} once={:?} twice={:?}",
            failures[0].0,
            failures[0].1,
            failures[0].2
        );
    }

    #[derive(Default)]
    struct MockClipboard {
        clipboard: Option<String>,
        primary: Option<String>,
    }

    impl ClipboardSelectionIo for MockClipboard {
        fn read_clipboard_text(&mut self) -> Option<String> {
            self.clipboard.clone()
        }

        fn write_clipboard_text(&mut self, text: &str) -> Option<()> {
            self.clipboard = Some(text.to_string());
            Some(())
        }

        fn read_primary_selection_text(&mut self) -> Option<String> {
            self.primary.clone()
        }

        fn write_primary_selection_text(&mut self, text: &str) -> Option<()> {
            self.primary = Some(text.to_string());
            Some(())
        }
    }

    #[test]
    fn clipboard_selection_helpers_route_to_mock_slots() {
        let mut clipboard = MockClipboard::default();

        write_clipboard_selection(&mut clipboard, ClipboardSelection::Clipboard, "regular");
        write_clipboard_selection(&mut clipboard, ClipboardSelection::Primary, "primary");

        assert_eq!(
            read_clipboard_selection(&mut clipboard, ClipboardSelection::Clipboard).as_deref(),
            Some("regular")
        );
        assert_eq!(
            read_clipboard_selection(&mut clipboard, ClipboardSelection::Primary).as_deref(),
            Some("primary")
        );
    }

    #[test]
    fn clipboard_image_raw_limit_accepts_exact_and_rejects_plus_one() {
        let exact = encode_rgba_to_png_with_limits(2, 2, &[0x42; 16], 8, 16, 1024);
        assert!(matches!(exact, Some(ClipboardImagePng::Ready(_))));

        let over = encode_rgba_to_png_with_limits(2, 2, &[0x42; 16], 8, 15, 1024);
        assert_eq!(over, Some(ClipboardImagePng::TooLarge { limit: 15 }));
    }

    #[test]
    fn clipboard_image_dimension_and_shape_are_validated_before_encoding() {
        assert_eq!(
            encode_rgba_to_png_with_limits(9, 1, &[0; 36], 8, 64, 1024),
            Some(ClipboardImagePng::TooLarge { limit: 64 })
        );
        assert_eq!(
            encode_rgba_to_png_with_limits(2, 2, &[0; 15], 8, 64, 1024),
            None
        );
    }

    #[test]
    fn clipboard_png_writer_stops_at_one_detection_byte() {
        let over = encode_rgba_to_png_with_limits(2, 2, &[0x42; 16], 8, 64, 1);
        assert_eq!(over, Some(ClipboardImagePng::TooLarge { limit: 1 }));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn native_primary_selection_remains_unsupported() {
        let mut clipboard = NativeClipboard::default();
        assert_eq!(
            write_clipboard_selection(&mut clipboard, ClipboardSelection::Primary, "ignored"),
            None
        );
        assert_eq!(clipboard.last_clipboard_write, None);
    }
}
