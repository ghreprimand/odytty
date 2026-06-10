use std::io::Write;
use std::sync::{Arc, Mutex};

use arboard::Clipboard;

use crate::core::{Snapshot, Terminal};
use crate::input;
use crate::selection;

use super::pty::PtyWriter;

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

impl NativeClipboard {
    pub(super) fn read_text(&mut self) -> Option<String> {
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

    pub(super) fn write_text(&mut self, text: &str) -> Option<()> {
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
    let bytes = input::encode_paste(text, bracketed_paste);

    if let Ok(mut writer) = writer.lock() {
        writer.write_all(&bytes)?;
        writer.flush()?;
    }
    Ok(())
}
