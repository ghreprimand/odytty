use std::sync::{Arc, Mutex};

use arboard::Clipboard;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};

use crate::core::{ClipboardSelection, Snapshot, Terminal};
use crate::selection;

use super::pty::{PASTE_CHUNK_SIZE, PtyWriter, spawn_chunked_pty_write};

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
    slot: ClipboardSlot<Clipboard>,
}

pub(super) trait ClipboardSelectionIo {
    fn read_clipboard_text(&mut self) -> Option<String>;
    fn write_clipboard_text(&mut self, text: &str) -> Option<()>;
    fn read_primary_selection_text(&mut self) -> Option<String>;
    fn write_primary_selection_text(&mut self, text: &str) -> Option<()>;
}

impl ClipboardSelectionIo for NativeClipboard {
    fn read_clipboard_text(&mut self) -> Option<String> {
        let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
            Ok(clipboard) => clipboard,
            Err(err) => {
                eprintln!("odytty: clipboard unavailable for paste: {err}");
                return None;
            }
        };

        match clipboard.get_text() {
            Ok(text) => Some(text),
            Err(err) => {
                eprintln!("odytty: clipboard paste failed: {err}");
                self.slot.clear();
                None
            }
        }
    }

    fn write_clipboard_text(&mut self, text: &str) -> Option<()> {
        let clipboard = match self.slot.get_or_try_init(Clipboard::new) {
            Ok(clipboard) => clipboard,
            Err(err) => {
                eprintln!("odytty: clipboard unavailable for copy: {err}");
                return None;
            }
        };

        match clipboard.set_text(text.to_owned()) {
            Ok(()) => Some(()),
            Err(err) => {
                eprintln!("odytty: clipboard copy failed: {err}");
                self.slot.clear();
                None
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
                eprintln!("odytty: primary selection unavailable for paste: {err}");
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
                eprintln!("odytty: primary selection paste failed: {err}");
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
                eprintln!("odytty: primary selection unavailable for copy: {err}");
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
                eprintln!("odytty: primary selection copy failed: {err}");
                self.slot.clear();
                None
            }
        }
    }
}

impl NativeClipboard {
    pub(super) fn read_text(&mut self) -> Option<String> {
        self.read_clipboard_text()
    }

    pub(super) fn write_text(&mut self, text: &str) -> Option<()> {
        self.write_clipboard_text(text)
    }

    pub(super) fn read_primary_text(&mut self) -> Option<String> {
        self.read_primary_selection_text()
    }

    pub(super) fn write_primary_text(&mut self, text: &str) -> Option<()> {
        self.write_primary_selection_text(text)
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

pub(super) fn selected_clipboard_text(
    snapshot: &Snapshot,
    range: selection::SelectionRange,
) -> Option<String> {
    let text = selection::selected_text(snapshot, range);
    (!text.is_empty()).then_some(text)
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
    spawn_chunked_pty_write(writer.clone(), chunks, "paste")
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
