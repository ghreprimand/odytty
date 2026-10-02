// SPDX-License-Identifier: GPL-3.0-only
//! Opt-in, privacy-safe diagnostics for compositor keyboard delivery.
//!
//! `ODYTTY_KEY_EVENT_DIAGNOSTICS=on` records the identities and state attached
//! to winit keyboard and IME events. Printable text is never recorded: text
//! fields are reduced to character/byte counts, while a single control code is
//! identified numerically so editing-key delivery can be diagnosed.

use std::ffi::OsStr;
use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use winit::event::{Ime, KeyEvent};
use winit::keyboard::{Key as WinitKey, NativeKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::input::{KeyEventType, KeyModes, Modifiers};

pub(super) const KEY_EVENT_DIAGNOSTICS_ENV: &str = "ODYTTY_KEY_EVENT_DIAGNOSTICS";

static ENABLED: OnceLock<bool> = OnceLock::new();
static EMPTY_PREEDIT_EVENTS: AtomicU64 = AtomicU64::new(0);

fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        diagnostics_enabled_from(std::env::var_os(KEY_EVENT_DIAGNOSTICS_ENV).as_deref())
    })
}

fn diagnostics_enabled_from(value: Option<&OsStr>) -> bool {
    let Some(value) = value.and_then(OsStr::to_str).map(str::trim) else {
        return false;
    };
    value == "1" || value.eq_ignore_ascii_case("on") || value.eq_ignore_ascii_case("true")
}

pub(super) fn log_keyboard_event(
    event: &KeyEvent,
    key_without_modifiers: &WinitKey,
    modifiers: Modifiers,
    super_key: bool,
) {
    if !enabled() {
        return;
    }

    tracing::warn!(
        "key-event diagnostic: logical={} key_without_modifiers={} physical={:?} location={:?} text={} text_with_all_modifiers={} ctrl={} alt={} shift={} super={} state={:?} repeat={}",
        SafeKey(&event.logical_key),
        SafeKey(key_without_modifiers),
        event.physical_key,
        event.location,
        OptionalText(event.text.as_deref()),
        OptionalText(event.text_with_all_modifiers()),
        modifiers.ctrl,
        modifiers.alt,
        modifiers.shift,
        super_key,
        event.state,
        event.repeat,
    );
}

pub(super) fn log_modifiers_changed(modifiers: Modifiers, super_key: bool) {
    if !enabled() {
        return;
    }

    tracing::warn!(
        "key-event diagnostic: modifiers-changed ctrl={} alt={} shift={} super={}",
        modifiers.ctrl,
        modifiers.alt,
        modifiers.shift,
        super_key,
    );
}

pub(super) fn log_ime_event(ime: &Ime) {
    if !enabled() {
        return;
    }

    if matches!(ime, Ime::Preedit(text, _) if text.is_empty()) {
        let occurrence = EMPTY_PREEDIT_EVENTS.fetch_add(1, Ordering::Relaxed) + 1;
        if !should_log_empty_preedit(occurrence) {
            return;
        }
        tracing::warn!(
            "key-event diagnostic: ime={} occurrence={occurrence}",
            SafeIme(ime)
        );
        return;
    }

    tracing::warn!("key-event diagnostic: ime={}", SafeIme(ime));
}

fn should_log_empty_preedit(occurrence: u64) -> bool {
    occurrence.is_power_of_two()
}

pub(super) fn is_backspace_target(key: &WinitKey) -> bool {
    match key {
        WinitKey::Named(winit::keyboard::NamedKey::Backspace) => true,
        WinitKey::Character(text) => matches!(text.as_str(), "\u{8}" | "\u{7f}"),
        _ => false,
    }
}

pub(super) fn log_backspace_stage(key: &WinitKey, stage: &'static str) {
    if !enabled() || !is_backspace_target(key) {
        return;
    }
    tracing::warn!(
        "key-event diagnostic: backspace-route stage={stage} logical={}",
        SafeKey(key)
    );
}

pub(super) fn log_backspace_modes(key: &WinitKey, modes: KeyModes, event_type: KeyEventType) {
    if !enabled() || !is_backspace_target(key) {
        return;
    }
    tracing::warn!(
        "key-event diagnostic: backspace-route stage=encoder-enter logical={} win32_input={} kitty_flags={} modify_other_keys={} event_type={event_type:?}",
        SafeKey(key),
        modes.win32_input,
        modes.kitty_keyboard_flags,
        modes.modify_other_keys,
    );
}

pub(super) fn log_backspace_encoding(key: &WinitKey, bytes: &[u8]) {
    if !enabled() || !is_backspace_target(key) {
        return;
    }
    tracing::warn!(
        "key-event diagnostic: backspace-route stage=encoder-output logical={} bytes={} hex={}",
        SafeKey(key),
        bytes.len(),
        HexBytes(bytes),
    );
}

pub(super) fn log_backspace_write(key: &WinitKey, write_ok: bool, flush_ok: bool) {
    if !enabled() || !is_backspace_target(key) {
        return;
    }
    tracing::warn!(
        "key-event diagnostic: backspace-route stage=pty-write logical={} write_ok={write_ok} flush_ok={flush_ok}",
        SafeKey(key),
    );
}

pub(super) fn log_backspace_writer_lock_failed(key: &WinitKey) {
    if !enabled() || !is_backspace_target(key) {
        return;
    }
    tracing::warn!(
        "key-event diagnostic: backspace-route stage=pty-writer-lock-failed logical={}",
        SafeKey(key),
    );
}

/// What one broadcast fan-out did for one receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FanoutOutcome {
    Delivered,
    /// The focused pane: it keeps its own write, so fan-out skips it.
    Focused,
    /// Another window owns the pane; the payload is queued for that window.
    QueuedOtherWindow,
    /// No window resolves the token on delivery.
    Unresolved,
    ReadOnly,
    WriteFailed,
    /// A paste too large for bracketed paste; refused with a notice.
    TooLarge,
    /// The pane no longer exists, so the receiver was dropped before delivery.
    Pruned,
}

impl FanoutOutcome {
    fn label(self) -> &'static str {
        match self {
            FanoutOutcome::Delivered => "delivered",
            FanoutOutcome::Focused => "skipped-focused",
            FanoutOutcome::QueuedOtherWindow => "queued-other-window",
            FanoutOutcome::Unresolved => "skipped-unresolved",
            FanoutOutcome::ReadOnly => "skipped-read-only",
            FanoutOutcome::WriteFailed => "write-failed",
            FanoutOutcome::TooLarge => "refused-too-large",
            FanoutOutcome::Pruned => "pruned",
        }
    }
}

/// Whether the diagnostics flag is on, so callers can skip building a trace.
pub(super) fn broadcast_trace_enabled() -> bool {
    enabled()
}

/// One fan-out record: the payload kind, its size in bytes, the focused pane's
/// token, and each receiver's token with its outcome. Counts and numeric
/// tokens only: never the typed bytes, text, titles, or paths.
fn broadcast_fanout_line(
    kind: &str,
    payload_bytes: usize,
    focused: u64,
    receivers: &[(u64, FanoutOutcome)],
) -> String {
    let mut line = format!(
        "key-event diagnostic: broadcast-fanout kind={kind} bytes={payload_bytes} focused={focused} receivers={}",
        receivers.len()
    );
    for (token, outcome) in receivers {
        line.push_str(&format!(" [{token}:{}]", outcome.label()));
    }
    line
}

fn emit_broadcast_fanout(
    on: bool,
    emit: impl FnOnce(String),
    kind: &str,
    payload_bytes: usize,
    focused: u64,
    receivers: &[(u64, FanoutOutcome)],
) {
    if on {
        emit(broadcast_fanout_line(
            kind,
            payload_bytes,
            focused,
            receivers,
        ));
    }
}

pub(super) fn log_broadcast_fanout(
    kind: &str,
    payload_bytes: usize,
    focused: u64,
    receivers: &[(u64, FanoutOutcome)],
) {
    emit_broadcast_fanout(
        enabled(),
        |line| tracing::warn!("{line}"),
        kind,
        payload_bytes,
        focused,
        receivers,
    );
}

struct SafeKey<'a>(&'a WinitKey);

impl fmt::Display for SafeKey<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            WinitKey::Named(named) => write!(formatter, "Named({named:?})"),
            WinitKey::Character(text) => write!(formatter, "Character({})", Text(text)),
            WinitKey::Unidentified(native) => {
                write!(formatter, "Unidentified({})", SafeNativeKey(native))
            }
            WinitKey::Dead(character) => {
                write!(formatter, "Dead(character_present={})", character.is_some())
            }
        }
    }
}

struct SafeNativeKey<'a>(&'a NativeKey);

impl fmt::Display for SafeNativeKey<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            NativeKey::Unidentified => formatter.write_str("native=unidentified"),
            NativeKey::Android(code) => write!(formatter, "native=android code=0x{code:04X}"),
            NativeKey::MacOS(code) => write!(formatter, "native=macos code=0x{code:04X}"),
            NativeKey::Windows(code) => write!(formatter, "native=windows code=0x{code:04X}"),
            NativeKey::Xkb(code) => write!(formatter, "native=xkb code=0x{code:04X}"),
            NativeKey::Web(value) => write!(
                formatter,
                "native=web chars={} utf8_bytes={}",
                value.chars().count(),
                value.len()
            ),
        }
    }
}

struct OptionalText<'a>(Option<&'a str>);

impl fmt::Display for OptionalText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(text) => write!(formatter, "Some({})", Text(text)),
            None => formatter.write_str("None"),
        }
    }
}

struct Text<'a>(&'a str);

impl fmt::Display for Text<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut characters = self.0.chars();
        let first = characters.next();
        if let Some(character) = first
            && characters.next().is_none()
            && character.is_control()
        {
            return write!(formatter, "control=U+{:04X}", u32::from(character));
        }

        write!(
            formatter,
            "redacted chars={} utf8_bytes={}",
            self.0.chars().count(),
            self.0.len()
        )
    }
}

struct HexBytes<'a>(&'a [u8]);

impl fmt::Display for HexBytes<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.iter().take(32) {
            write!(formatter, "{byte:02X}")?;
        }
        if self.0.len() > 32 {
            formatter.write_str("...")?;
        }
        Ok(())
    }
}

struct SafeIme<'a>(&'a Ime);

impl fmt::Display for SafeIme<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Ime::Enabled => formatter.write_str("Enabled"),
            Ime::Disabled => formatter.write_str("Disabled"),
            Ime::Preedit(text, cursor) => {
                write!(formatter, "Preedit(text={}, cursor={cursor:?})", Text(text))
            }
            Ime::Commit(text) => write!(formatter, "Commit(text={})", Text(text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::Key;

    #[test]
    fn broadcast_fanout_trace_is_silent_when_off() {
        let mut lines = Vec::new();
        emit_broadcast_fanout(
            false,
            |line| lines.push(line),
            "bytes",
            1,
            3,
            &[(1, FanoutOutcome::Delivered)],
        );
        assert!(lines.is_empty());
    }

    #[test]
    fn broadcast_fanout_trace_names_every_outcome_without_payload() {
        let all = [
            (1, FanoutOutcome::Delivered),
            (2, FanoutOutcome::Focused),
            (3, FanoutOutcome::QueuedOtherWindow),
            (4, FanoutOutcome::Unresolved),
            (5, FanoutOutcome::ReadOnly),
            (6, FanoutOutcome::WriteFailed),
            (7, FanoutOutcome::TooLarge),
            (8, FanoutOutcome::Pruned),
        ];
        let mut lines = Vec::new();
        emit_broadcast_fanout(true, |line| lines.push(line), "paste", 42, 9, &all);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            "key-event diagnostic: broadcast-fanout kind=paste bytes=42 focused=9 receivers=8 \
             [1:delivered] [2:skipped-focused] [3:queued-other-window] [4:skipped-unresolved] \
             [5:skipped-read-only] [6:write-failed] [7:refused-too-large] [8:pruned]"
        );
    }

    #[test]
    fn diagnostics_are_opt_in() {
        assert!(!diagnostics_enabled_from(None));
        assert!(!diagnostics_enabled_from(Some(OsStr::new(""))));
        assert!(!diagnostics_enabled_from(Some(OsStr::new("off"))));
        assert!(diagnostics_enabled_from(Some(OsStr::new("1"))));
        assert!(diagnostics_enabled_from(Some(OsStr::new("ON"))));
        assert!(diagnostics_enabled_from(Some(OsStr::new(" true "))));
    }

    #[test]
    fn printable_key_and_ime_text_are_redacted() {
        let private = "private command text";
        let key = Key::Character(private.into());
        let key_record = SafeKey(&key).to_string();
        let ime_record = SafeIme(&Ime::Commit(private.into())).to_string();

        assert_eq!(key_record, "Character(redacted chars=20 utf8_bytes=20)");
        assert_eq!(ime_record, "Commit(text=redacted chars=20 utf8_bytes=20)");
        assert!(!key_record.contains(private));
        assert!(!ime_record.contains(private));
    }

    #[test]
    fn single_control_text_keeps_numeric_identity() {
        let backspace = Key::Character("\u{8}".into());
        assert_eq!(SafeKey(&backspace).to_string(), "Character(control=U+0008)");
        assert_eq!(
            OptionalText(Some("\u{7f}")).to_string(),
            "Some(control=U+007F)"
        );
    }

    #[test]
    fn dead_and_web_keys_do_not_expose_their_text() {
        let dead = Key::Dead(Some('\u{e9}'));
        let web = Key::Unidentified(NativeKey::Web("private-web-key".into()));

        assert_eq!(SafeKey(&dead).to_string(), "Dead(character_present=true)");
        assert_eq!(
            SafeKey(&web).to_string(),
            "Unidentified(native=web chars=15 utf8_bytes=15)"
        );
    }

    #[test]
    fn empty_preedit_logging_is_exponentially_bounded() {
        let logged = (1..=20)
            .filter(|occurrence| should_log_empty_preedit(*occurrence))
            .collect::<Vec<_>>();
        assert_eq!(logged, [1, 2, 4, 8, 16]);
    }

    #[test]
    fn backspace_output_hex_is_bounded_and_contains_no_text() {
        assert_eq!(HexBytes(b"\x1b[127;5u").to_string(), "1B5B3132373B3575");
        assert_eq!(HexBytes(&[0x7f]).to_string(), "7F");
    }
}
