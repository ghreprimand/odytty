// SPDX-License-Identifier: GPL-3.0-only
//! Native OSC 52 clipboard-write policy and consent state.
//!
//! Terminal parsing remains model-only. This module owns the final native
//! authority check: the emitting PTY must still be active, the OS window must
//! still be focused, and the live write policy must permit the request.

use std::time::{Duration, Instant};

use winit::keyboard::{Key as WinitKey, PhysicalKey};

use crate::core::{Attrs, Cell, ClipboardSelection, Color, Snapshot};
use crate::input::{KeyEventType, Modifiers};
use crate::settings::Osc52WritePolicy;

use super::super::clipboard::write_clipboard_selection;
use super::{App, OverlayFragment, SessionToken};

const NOTICE_RATE_LIMIT: Duration = Duration::from_secs(1);
const PROMPT_ROWS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionConsent {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteDisposition {
    Discard,
    Apply,
    Prompt,
}

#[derive(Debug)]
struct PendingWrite {
    session: SessionToken,
    selection: ClipboardSelection,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum PromptDecision {
    AllowOnce,
    AllowSession,
    DenySession,
    Cancel,
}

#[derive(Debug, Default)]
pub(in crate::native) struct Osc52WriteState {
    pending: Option<PendingWrite>,
    session_consents: Vec<(SessionToken, SessionConsent)>,
    captured_key: Option<PhysicalKey>,
    last_notice_at: Option<Instant>,
    focus_observed: bool,
}

impl Osc52WriteState {
    /// Whether a real OS focus event has been observed since launch. The
    /// OSC 52 read authority (C41) consults this alongside `App::focused`,
    /// mirroring the write path, so a read is denied until focus is confirmed.
    pub(super) fn focus_observed(&self) -> bool {
        self.focus_observed
    }

    fn disposition(
        &self,
        policy: Osc52WritePolicy,
        window_focused: bool,
        active_session: SessionToken,
        emitting_session: SessionToken,
    ) -> WriteDisposition {
        if !window_focused || active_session != emitting_session {
            return WriteDisposition::Discard;
        }
        match policy {
            Osc52WritePolicy::Off => WriteDisposition::Discard,
            Osc52WritePolicy::On => WriteDisposition::Apply,
            Osc52WritePolicy::Ask => match self.consent_for(emitting_session) {
                Some(SessionConsent::Allow) => WriteDisposition::Apply,
                Some(SessionConsent::Deny) => WriteDisposition::Discard,
                None => WriteDisposition::Prompt,
            },
        }
    }

    fn consent_for(&self, session: SessionToken) -> Option<SessionConsent> {
        self.session_consents
            .iter()
            .find_map(|(candidate, consent)| (*candidate == session).then_some(*consent))
    }

    fn remember(&mut self, session: SessionToken, consent: SessionConsent) {
        if let Some((_, saved)) = self
            .session_consents
            .iter_mut()
            .find(|(candidate, _)| *candidate == session)
        {
            *saved = consent;
        } else {
            self.session_consents.push((session, consent));
        }
    }

    fn retain_live_sessions(&mut self, live: &[SessionToken]) {
        self.session_consents
            .retain(|(session, _)| live.contains(session));
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !live.contains(&pending.session))
        {
            self.pending = None;
        }
    }

    fn queue(&mut self, session: SessionToken, selection: ClipboardSelection, text: String) {
        // Exactly one pending request is retained. A newer request from the
        // still-active PTY replaces the older one, so output bursts cannot grow
        // an authorization queue or resurrect stale clipboard contents later.
        self.pending = Some(PendingWrite {
            session,
            selection,
            text,
        });
    }

    fn should_raise_notice(&mut self, now: Instant) -> bool {
        if self
            .last_notice_at
            .is_some_and(|last| now.saturating_duration_since(last) < NOTICE_RATE_LIMIT)
        {
            return false;
        }
        self.last_notice_at = Some(now);
        true
    }

    fn resolve(&mut self, decision: PromptDecision) -> Option<PendingWrite> {
        let pending = self.pending.take()?;
        match decision {
            PromptDecision::Cancel => None,
            PromptDecision::DenySession => {
                self.remember(pending.session, SessionConsent::Deny);
                None
            }
            PromptDecision::AllowOnce => Some(pending),
            PromptDecision::AllowSession => {
                self.remember(pending.session, SessionConsent::Allow);
                Some(pending)
            }
        }
    }

    fn prompt_text(&self) -> Option<String> {
        self.pending.as_ref().map(|pending| {
            format!(
                "Clipboard write request | {} | {} bytes",
                selection_label(pending.selection),
                pending.text.len()
            )
        })
    }
}

impl App {
    pub(in crate::native) fn cancel_osc52_prompt(&mut self) {
        let prompt_was_visible = self.osc52_write.pending.take().is_some();
        self.osc52_write.captured_key = None;
        if prompt_was_visible {
            self.request_selection_redraw();
        }
    }

    pub(in crate::native) fn observe_osc52_window_focus(&mut self) {
        self.osc52_write.focus_observed = true;
    }

    fn apply_osc52_write(&mut self, selection: ClipboardSelection, text: &str, now: Instant) {
        if write_clipboard_selection(&mut self.clipboard, selection, text).is_none() {
            return;
        }
        if self.osc52_write.should_raise_notice(now) {
            self.raise_neutral_notice(format!(
                "Clipboard updated by terminal output | {} | {} bytes",
                selection_label(selection),
                text.len()
            ));
        }
    }

    pub(in crate::native) fn handle_osc52_write(
        &mut self,
        session: SessionToken,
        selection: ClipboardSelection,
        text: String,
        now: Instant,
    ) {
        match self.osc52_write.disposition(
            self.settings.osc52_write,
            self.focused && self.osc52_write.focus_observed,
            self.sessions.active_id(),
            session,
        ) {
            WriteDisposition::Discard => {
                tracing::debug!("discarded OSC 52 clipboard write without native authority");
            }
            WriteDisposition::Apply => self.apply_osc52_write(selection, &text, now),
            WriteDisposition::Prompt => {
                self.osc52_write.queue(session, selection, text);
                self.request_selection_redraw();
            }
        }
    }

    pub(in crate::native) fn prune_osc52_session_state(&mut self, live: &[SessionToken]) {
        self.osc52_write.retain_live_sessions(live);
    }

    pub(in crate::native) fn resolve_osc52_prompt(&mut self, decision: PromptDecision) {
        let Some(pending) = self.osc52_write.resolve(decision) else {
            self.request_selection_redraw();
            return;
        };

        // Re-check authority at the moment of approval. Focus loss, activation
        // changes, and reloads also cancel proactively, but this guard prevents
        // any future input route from applying a stale prompt.
        if self.focused
            && self.osc52_write.focus_observed
            && pending.session == self.sessions.active_id()
            && self.settings.osc52_write == Osc52WritePolicy::Ask
        {
            self.apply_osc52_write(pending.selection, &pending.text, Instant::now());
        }
        self.request_selection_redraw();
    }

    pub(in crate::native) fn handle_osc52_prompt_key(
        &mut self,
        binding_key: &WinitKey,
        physical: PhysicalKey,
        event_type: KeyEventType,
    ) -> bool {
        if self.osc52_write.captured_key == Some(physical) {
            if event_type == KeyEventType::Release {
                self.osc52_write.captured_key = None;
            }
            return true;
        }
        if self.osc52_write.pending.is_none() {
            return false;
        }
        if event_type == KeyEventType::Release {
            return true;
        }
        if event_type == KeyEventType::Press {
            self.osc52_write.captured_key = Some(physical);
        }

        if let Some(decision) = prompt_decision(binding_key, self.modifiers) {
            self.resolve_osc52_prompt(decision);
        }
        true
    }

    pub(in crate::native) fn osc52_prompt_overlay_signature(&self) -> Option<OverlayFragment> {
        self.osc52_write
            .prompt_text()
            .map(|text| OverlayFragment::OpenNotice { text })
    }

    pub(in crate::native) fn paint_osc52_prompt_cells(&self, snapshot: &mut Snapshot) -> bool {
        let Some(pending) = self.osc52_write.pending.as_ref() else {
            return false;
        };
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows.min(PROMPT_ROWS);
        if columns == 0 || rows == 0 {
            return true;
        }

        let attrs = prompt_attrs();
        for row in 0..rows {
            let start = row * columns;
            for column in 0..columns {
                snapshot.cells[start + column] = Cell::new(' ', attrs);
            }
        }
        paint_prompt_row(
            snapshot,
            0,
            &format!(
                "Clipboard write request | {} | {} bytes",
                selection_label(pending.selection),
                pending.text.len()
            ),
            attrs,
        );
        paint_prompt_row(
            snapshot,
            1,
            "Ctrl+Shift+1 allow once | Ctrl+Shift+S allow session",
            attrs,
        );
        paint_prompt_row(
            snapshot,
            2,
            "Ctrl+Shift+D deny session | Escape cancel",
            attrs,
        );
        true
    }

    #[cfg(test)]
    pub(in crate::native) fn osc52_prompt_metadata_for_test(
        &self,
    ) -> Option<(&'static str, usize)> {
        self.osc52_write
            .pending
            .as_ref()
            .map(|pending| (selection_label(pending.selection), pending.text.len()))
    }
}

fn selection_label(selection: ClipboardSelection) -> &'static str {
    match selection {
        ClipboardSelection::Clipboard => "Clipboard",
        ClipboardSelection::Primary => "Primary",
    }
}

fn prompt_decision(key: &WinitKey, modifiers: Modifiers) -> Option<PromptDecision> {
    if matches!(key, WinitKey::Named(winit::keyboard::NamedKey::Escape)) {
        return Some(PromptDecision::Cancel);
    }
    if !modifiers.ctrl || !modifiers.shift || modifiers.alt {
        return None;
    }
    match key {
        WinitKey::Character(text) if text == "1" => Some(PromptDecision::AllowOnce),
        WinitKey::Character(text) if text.eq_ignore_ascii_case("s") => {
            Some(PromptDecision::AllowSession)
        }
        WinitKey::Character(text) if text.eq_ignore_ascii_case("d") => {
            Some(PromptDecision::DenySession)
        }
        _ => None,
    }
}

fn paint_prompt_row(snapshot: &mut Snapshot, row: usize, text: &str, attrs: Attrs) {
    if row >= snapshot.dimensions.rows {
        return;
    }
    let columns = snapshot.dimensions.columns;
    let start = row * columns;
    for (column, ch) in text.chars().filter(|ch| !ch.is_control()).enumerate() {
        let column = column + 1;
        if column >= columns {
            break;
        }
        snapshot.cells[start + column] = Cell::new(ch, attrs);
    }
}

fn prompt_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(15);
    attrs.background = Color::Indexed(4);
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_shortcuts_require_explicit_chords() {
        let chord = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert_eq!(
            prompt_decision(&WinitKey::Character("1".into()), chord),
            Some(PromptDecision::AllowOnce)
        );
        assert_eq!(
            prompt_decision(&WinitKey::Character("s".into()), chord),
            Some(PromptDecision::AllowSession)
        );
        assert_eq!(
            prompt_decision(&WinitKey::Character("d".into()), chord),
            Some(PromptDecision::DenySession)
        );
        assert_eq!(
            prompt_decision(&WinitKey::Character("s".into()), Modifiers::NONE),
            None
        );
    }

    #[test]
    fn pending_write_is_latest_only_and_content_never_enters_prompt_text() {
        let mut state = Osc52WriteState::default();
        state.queue(
            SessionToken(7),
            ClipboardSelection::Clipboard,
            "first private value".to_owned(),
        );
        state.queue(
            SessionToken(7),
            ClipboardSelection::Primary,
            "new private value".to_owned(),
        );
        let prompt = state.prompt_text().expect("pending prompt");
        assert!(prompt.contains("Primary"));
        assert!(prompt.contains("17 bytes"));
        assert!(!prompt.contains("private"));
    }

    #[test]
    fn notices_are_rate_limited_without_a_repeating_wake() {
        let mut state = Osc52WriteState::default();
        let start = Instant::now();
        assert!(state.should_raise_notice(start));
        assert!(!state.should_raise_notice(start + Duration::from_millis(999)));
        assert!(state.should_raise_notice(start + NOTICE_RATE_LIMIT));
    }

    #[test]
    fn authority_matrix_requires_focus_active_session_and_policy() {
        let state = Osc52WriteState::default();
        assert!(!state.focus_observed, "startup authority is fail-closed");
        let active = SessionToken(1);
        let background = SessionToken(2);
        assert_eq!(
            state.disposition(Osc52WritePolicy::On, true, active, active),
            WriteDisposition::Apply
        );
        assert_eq!(
            state.disposition(Osc52WritePolicy::On, false, active, active),
            WriteDisposition::Discard
        );
        assert_eq!(
            state.disposition(Osc52WritePolicy::On, true, active, background),
            WriteDisposition::Discard
        );
        assert_eq!(
            state.disposition(Osc52WritePolicy::Off, true, active, active),
            WriteDisposition::Discard
        );
        assert_eq!(
            state.disposition(Osc52WritePolicy::Ask, true, active, active),
            WriteDisposition::Prompt
        );
    }

    #[test]
    fn consent_is_per_session_and_allow_once_does_not_persist() {
        let session = SessionToken(3);
        let other = SessionToken(4);
        let mut state = Osc52WriteState::default();

        state.queue(session, ClipboardSelection::Clipboard, "once".to_owned());
        assert!(state.resolve(PromptDecision::AllowOnce).is_some());
        assert_eq!(state.consent_for(session), None);

        state.queue(session, ClipboardSelection::Clipboard, "session".to_owned());
        assert!(state.resolve(PromptDecision::AllowSession).is_some());
        assert_eq!(state.consent_for(session), Some(SessionConsent::Allow));
        assert_eq!(state.consent_for(other), None);

        state.queue(other, ClipboardSelection::Clipboard, "deny".to_owned());
        assert!(state.resolve(PromptDecision::DenySession).is_none());
        assert_eq!(state.consent_for(other), Some(SessionConsent::Deny));

        state.retain_live_sessions(&[other]);
        assert_eq!(state.consent_for(session), None);
        assert_eq!(state.consent_for(other), Some(SessionConsent::Deny));
    }
}
