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

use winit::event::{Ime, KeyEvent};
use winit::keyboard::{Key as WinitKey, NativeKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::input::Modifiers;

pub(super) const KEY_EVENT_DIAGNOSTICS_ENV: &str = "ODYTTY_KEY_EVENT_DIAGNOSTICS";

static ENABLED: OnceLock<bool> = OnceLock::new();

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

    tracing::warn!("key-event diagnostic: ime={}", SafeIme(ime));
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
}
