// SPDX-License-Identifier: GPL-3.0-only
use std::sync::{Arc, Mutex};

use arboard::Clipboard;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};

use crate::core::{ClipboardSelection, Terminal};

use super::pty::{PASTE_CHUNK_SIZE, PtyWriter, write_chunks_blocking};

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

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
    /// Test-only: PNG bytes `read_image_png` returns instead of touching the real
    /// clipboard. The real image read (`arboard::Clipboard::get_image`) is
    /// compiled out under `cfg(test)` for the same reasons text I/O is — so the
    /// image paste-through (F6-i7) confirm flow can be driven from a synthetic
    /// clipboard image without a live system clipboard.
    #[cfg(test)]
    pub(super) injected_clipboard_image: Option<Vec<u8>>,
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
            None
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

            match clipboard.get_text() {
                Ok(text) => Some(text),
                Err(err) => {
                    tracing::warn!("clipboard paste failed: {err}");
                    self.slot.clear();
                    None
                }
            }
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
        let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
            Ok(clipboard) => clipboard,
            Err(err) => {
                tracing::warn!("primary selection unavailable for paste: {err}");
                return None;
            }
        };

        match clipboard
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
        {
            Ok(text) => Some(text),
            Err(err) => {
                tracing::warn!("primary selection paste failed: {err}");
                self.slot.clear();
                None
            }
        }
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    fn write_primary_selection_text(&mut self, text: &str) -> Option<()> {
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
    pub(super) fn read_image_png(&mut self) -> Option<Vec<u8>> {
        #[cfg(test)]
        {
            self.injected_clipboard_image.clone()
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
            let image = match clipboard.get_image() {
                Ok(image) => image,
                Err(err) => {
                    // No image on the clipboard is the common case (a text or
                    // empty clipboard), so this stays at debug — not a warning.
                    tracing::debug!("clipboard image read failed: {err}");
                    return None;
                }
            };
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

/// Encode a raw RGBA8 image (as `arboard` hands back) to PNG bytes. Returns
/// `None` on a malformed buffer or an encoder error so the caller degrades to a
/// non-image paste. Compiled out under `cfg(test)` — the image paste-through
/// tests inject already-encoded bytes and never touch a real clipboard image.
#[cfg(not(test))]
fn encode_rgba_to_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    use image::codecs::png::PngEncoder;

    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba.len() != expected {
        return None;
    }
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(png)
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

pub(super) fn write_paste_text(
    terminal: &Arc<Mutex<Terminal>>,
    writer: &PtyWriter,
    text: &str,
) -> std::io::Result<()> {
    let bracketed_paste = terminal
        .lock()
        .map(|terminal| terminal.bracketed_paste_enabled())
        .unwrap_or(false);
    let chunks = encode_paste_chunks(text, bracketed_paste, PASTE_CHUNK_SIZE);
    write_chunks_blocking(writer, &chunks)
}

pub(super) fn encode_paste_chunks(
    text: &str,
    bracketed_paste: bool,
    chunk_size: usize,
) -> Vec<Vec<u8>> {
    let chunk_size = chunk_size.max(1);
    if bracketed_paste {
        let mut chunks = Vec::new();
        chunks.push(BRACKETED_PASTE_START.to_vec());
        push_chunked(
            &mut chunks,
            &sanitize_bracketed_paste(text.as_bytes()),
            chunk_size,
        );
        chunks.push(BRACKETED_PASTE_END.to_vec());
        chunks
    } else {
        let normalized = normalize_plain_paste(text);
        let mut chunks = Vec::new();
        push_chunked(&mut chunks, &normalized, chunk_size);
        chunks
    }
}

#[cfg(test)]
pub(super) fn flatten_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    chunks.iter().flatten().copied().collect()
}

fn push_chunked(chunks: &mut Vec<Vec<u8>>, bytes: &[u8], chunk_size: usize) {
    chunks.extend(bytes.chunks(chunk_size).map(<[u8]>::to_vec));
}

fn sanitize_bracketed_paste(text: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text[index..].starts_with(BRACKETED_PASTE_END) {
            index += BRACKETED_PASTE_END.len();
        } else {
            output.push(text[index]);
            index += 1;
        }
    }
    output
}

fn normalize_plain_paste(text: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(text.len());
    let mut bytes = text.as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'\r' => {
                if bytes.peek() == Some(&b'\n') {
                    bytes.next();
                }
                output.push(b'\r');
            }
            b'\n' => output.push(b'\r'),
            _ => output.push(byte),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
