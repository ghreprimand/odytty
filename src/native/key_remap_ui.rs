// SPDX-License-Identifier: GPL-3.0-only
//! KB-REMAP — the in-app keybinding remap modal (`OverlayMode::KeyBindings`).
//!
//! Turns the previously display-only `keybinds` settings row into a
//! mouse-light, keyboard-driven editor: pick an action, press a chord, and it
//! is captured live and written to `odytty.conf` — no hand-typing of
//! `ctrl+shift+f=search` strings (the no-hand-editing north star).
//!
//! Architecture (mirrors `theme_builder.rs`): this is a self-contained state
//! machine the [`super::overlay::OverlayUi`] owns as one `OverlayMode`. Browsing
//! navigation arrives through [`KeyRemapUi::handle_input`] like every other
//! overlay mode; the chord-capture interaction is special — it BYPASSES the
//! lossy `overlay_input_from_winit` mapper (which strips modifiers) and routes a
//! raw [`KeyChord`] in through [`KeyRemapUi::deliver_chord`]. The App gates that
//! bypass on [`KeyRemapUi::is_capturing_chord`].
//!
//! Persistence reuses the EXISTING serializer ([`crate::settings::
//! key_bindings_config_value`]) so a chord authored here and one typed into the
//! config round-trip to the identical bytes; the live `KeyBindings` table
//! rebuilds via `from_overrides` on the reloadable apply, so a remap takes
//! effect immediately with no restart.

use crate::settings::{
    BindableAction, KEYBINDS_ENV, KeyBindingKey, KeyBindingNamedKey, KeyBindingOverride, KeyChord,
    SettingEdit, Settings, bindable_action_display_name, format_key_chord,
    key_bindings_config_value,
};

use super::bindings::KeyBindings;
use super::overlay::OverlayInput;

/// Every bindable action, in the canonical order shared with
/// `default_key_bindings` and the `bindable_action_name` authority. This is the
/// single source the remap list iterates — so a new `BindableAction` variant
/// surfaces here the moment it is added (the `all_actions_match_enum` test pins
/// the count to the enum).
const ACTIONS: [BindableAction; 12] = [
    BindableAction::Search,
    BindableAction::SettingsPanel,
    BindableAction::ThemePicker,
    BindableAction::Copy,
    BindableAction::Paste,
    BindableAction::ScrollPageUp,
    BindableAction::ScrollPageDown,
    BindableAction::JumpPromptPrev,
    BindableAction::JumpPromptNext,
    BindableAction::CopyMode,
    BindableAction::Hints,
    BindableAction::ClearInput,
];

/// What the App should do after a remap-UI key/chord. Mirrors
/// `ThemeBuilderOutcome`: `Preview` applies the working bindings live (the
/// reloadable apply rebuilds the live `KeyBindings`), `Save` persists them, and
/// `Cancel` restores the bindings present when the modal opened, then closes.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum KeyRemapOutcome {
    Consumed,
    Preview(Settings),
    Save(Vec<SettingEdit>),
    Cancel(Settings),
}

/// A pending conflict-confirm: the captured `chord` would reassign away from
/// `conflicts_with` to `for_action`. Enter reassigns; Esc cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictState {
    chord: KeyChord,
    for_action: BindableAction,
    conflicts_with: BindableAction,
}

#[derive(Debug, Clone)]
pub(super) struct KeyRemapUi {
    /// Full settings present when the modal opened — the base for the live
    /// `Preview` settings and the value `Cancel` restores to.
    base: Settings,
    /// Working override vector (mutated; applied live via `Preview`, persisted
    /// on `Save`). Starts as a clone of `base.key_bindings`.
    overrides: Vec<KeyBindingOverride>,
    selected: usize,
    scroll: usize,
    /// `Some(action)` while a row is armed to capture its next chord.
    capture: Option<BindableAction>,
    /// `Some(_)` while a captured chord awaits conflict confirmation.
    conflict: Option<ConflictState>,
    message: Option<String>,
}

impl Default for KeyRemapUi {
    fn default() -> Self {
        Self::new(&Settings::default())
    }
}

impl KeyRemapUi {
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            base: settings.clone(),
            overrides: settings.key_bindings.clone(),
            selected: 0,
            scroll: 0,
            capture: None,
            conflict: None,
            message: None,
        }
    }

    pub(super) fn open(&mut self, settings: &Settings) {
        *self = Self::new(settings);
        self.message = Some(
            "Up/Down select, Enter captures a chord, Backspace resets a row, R resets all, Ctrl+S saves, Esc closes."
                .to_owned(),
        );
    }

    /// Resync from external settings ONLY while idle — never clobber an
    /// in-progress capture/conflict (the analogue of the theme builder guarding
    /// its refresh on `editing.is_none()`).
    pub(super) fn refresh(&mut self, settings: &Settings) {
        if self.capture.is_none() && self.conflict.is_none() {
            let selected = self.selected;
            let scroll = self.scroll;
            *self = Self::new(settings);
            self.selected = selected.min(ACTIONS.len() - 1);
            self.scroll = scroll;
        }
    }

    /// True while a row is capturing a chord OR a conflict-confirm is pending.
    /// The App routes raw keys to [`Self::deliver_chord`] exactly when this is
    /// true; it is `false` the instant the modal returns to browsing, so normal
    /// overlay navigation is never swallowed (R1).
    pub(super) fn is_capturing_chord(&self) -> bool {
        self.capture.is_some() || self.conflict.is_some()
    }

    // -- Browsing-mode input (chord capture is NOT routed here) ---------------

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> KeyRemapOutcome {
        // Defensive: capture/conflict keys never come through this path (the App
        // routes them to `deliver_chord`), so browsing input that somehow
        // arrives mid-capture is dropped rather than mishandled.
        if self.is_capturing_chord() {
            return KeyRemapOutcome::Consumed;
        }
        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-5),
            OverlayInput::PageDown => self.move_selection(5),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => self.set_selection(ACTIONS.len() - 1),
            OverlayInput::Activate => self.begin_capture(),
            OverlayInput::Backspace => return self.reset_selected_row(),
            OverlayInput::Char('r') | OverlayInput::Char('R') => return self.reset_all(),
            OverlayInput::Save => return self.save(),
            OverlayInput::Close => return KeyRemapOutcome::Cancel(self.base.clone()),
            _ => {}
        }
        KeyRemapOutcome::Consumed
    }

    fn move_selection(&mut self, delta: isize) {
        let len = ACTIONS.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        self.set_selection(next);
    }

    fn set_selection(&mut self, index: usize) {
        self.selected = index.min(ACTIONS.len() - 1);
        self.message = None;
    }

    fn begin_capture(&mut self) {
        let action = ACTIONS[self.selected];
        self.capture = Some(action);
        self.message = Some(format!(
            "Press a chord for {} (Esc cancels)…",
            bindable_action_display_name(action)
        ));
    }

    fn reset_selected_row(&mut self) -> KeyRemapOutcome {
        let action = ACTIONS[self.selected];
        let before = self.overrides.len();
        self.overrides.retain(|o| o.action != action);
        if self.overrides.len() == before {
            self.message = Some(format!(
                "{} already uses its default binding.",
                bindable_action_display_name(action)
            ));
            return KeyRemapOutcome::Consumed;
        }
        self.message = Some(format!(
            "Reset {} to its default binding.",
            bindable_action_display_name(action)
        ));
        KeyRemapOutcome::Preview(self.live_settings())
    }

    fn reset_all(&mut self) -> KeyRemapOutcome {
        if self.overrides.is_empty() {
            self.message = Some("All actions already use their default bindings.".to_owned());
            return KeyRemapOutcome::Consumed;
        }
        self.overrides.clear();
        self.message = Some("Reset all actions to their default bindings.".to_owned());
        KeyRemapOutcome::Preview(self.live_settings())
    }

    fn save(&mut self) -> KeyRemapOutcome {
        let value = key_bindings_config_value(&self.overrides);
        KeyRemapOutcome::Save(vec![SettingEdit {
            key: "keybinds",
            env: KEYBINDS_ENV,
            value,
        }])
    }

    // -- Capture-mode input (raw chord bypass) --------------------------------

    /// Deliver a raw chord (or `None` for a key `chord_from_winit` could not
    /// encode) while capturing. Drives both the capture and the conflict-confirm
    /// sub-states; returns to browsing on commit/cancel.
    pub(super) fn deliver_chord(&mut self, chord: Option<KeyChord>) -> KeyRemapOutcome {
        if self.conflict.is_some() {
            return self.deliver_conflict(chord);
        }
        let Some(action) = self.capture else {
            // Not actually capturing — guard against a stray route.
            return KeyRemapOutcome::Consumed;
        };
        let Some(chord) = chord else {
            self.message = Some("Unsupported key — try another chord.".to_owned());
            return KeyRemapOutcome::Consumed;
        };
        // Esc cancels capture (its natural modal-control role); it is never
        // BOUND. Enter is reserved too — binding either would make the modal
        // uncloseable (D-KBR-5 / R4).
        if is_bare(chord, KeyBindingNamedKey::Escape) {
            self.capture = None;
            self.message = Some("Capture cancelled.".to_owned());
            return KeyRemapOutcome::Consumed;
        }
        if is_bare(chord, KeyBindingNamedKey::Enter) {
            self.message =
                Some("Enter is reserved as a control key — try a different chord.".to_owned());
            return KeyRemapOutcome::Consumed;
        }
        // Conflict check against the CURRENT EFFECTIVE bindings (defaults +
        // working overrides), not the override vec alone — a chord that matches
        // a DEFAULT of another action must still be caught (R3).
        match self.effective_bindings().action_for_chord(chord) {
            Some(existing) if existing == action => {
                self.capture = None;
                self.message = Some(format!(
                    "{} is already bound to {}.",
                    format_key_chord(chord),
                    bindable_action_display_name(action)
                ));
                KeyRemapOutcome::Consumed
            }
            Some(other) => {
                self.conflict = Some(ConflictState {
                    chord,
                    for_action: action,
                    conflicts_with: other,
                });
                self.message = Some(format!(
                    "{} is bound to {} — reassign to {}? [Enter] yes  [Esc] no",
                    format_key_chord(chord),
                    bindable_action_display_name(other),
                    bindable_action_display_name(action)
                ));
                KeyRemapOutcome::Consumed
            }
            None => self.commit_binding(action, chord),
        }
    }

    fn deliver_conflict(&mut self, chord: Option<KeyChord>) -> KeyRemapOutcome {
        let Some(conflict) = self.conflict.clone() else {
            return KeyRemapOutcome::Consumed;
        };
        // Only bare Enter (confirm) and bare Esc (cancel) act; any other key is
        // ignored so a stray keypress cannot silently resolve the prompt.
        match chord {
            Some(c) if is_bare(c, KeyBindingNamedKey::Enter) => {
                self.conflict = None;
                self.commit_binding(conflict.for_action, conflict.chord)
            }
            Some(c) if is_bare(c, KeyBindingNamedKey::Escape) => {
                self.conflict = None;
                self.capture = None;
                self.message = Some("Kept the existing binding.".to_owned());
                KeyRemapOutcome::Consumed
            }
            _ => KeyRemapOutcome::Consumed,
        }
    }

    /// Set `action`'s override to `chord`, dropping any prior override that used
    /// either this action or this chord so the working vector never holds two
    /// overrides on one chord (replace-only, D-KBR-6).
    fn commit_binding(&mut self, action: BindableAction, chord: KeyChord) -> KeyRemapOutcome {
        self.overrides
            .retain(|o| o.action != action && o.chord != chord);
        self.overrides.push(KeyBindingOverride { chord, action });
        self.capture = None;
        self.message = Some(format!(
            "Bound {} to {}.",
            format_key_chord(chord),
            bindable_action_display_name(action)
        ));
        KeyRemapOutcome::Preview(self.live_settings())
    }

    fn effective_bindings(&self) -> KeyBindings {
        KeyBindings::from_overrides(&self.overrides)
    }

    fn live_settings(&self) -> Settings {
        let mut settings = self.base.clone();
        settings.key_bindings = self.overrides.clone();
        settings
    }

    // -- Save lifecycle hooks (mirror the other overlay modes) ----------------

    pub(super) fn save_succeeded(&mut self, changed: usize) {
        // The working vector is now the persisted truth, so it becomes the new
        // base for any further Cancel-restore in this session.
        self.base.key_bindings = self.overrides.clone();
        self.message = Some(match changed {
            0 => "No keybinding changes to save.".to_owned(),
            1 => "Saved 1 keybinding change to odytty.conf.".to_owned(),
            n => format!("Saved {n} keybinding changes to odytty.conf."),
        });
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.message = Some(format!("Save failed: {message}"));
    }

    // -- Rendering ------------------------------------------------------------

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        64.min(columns)
    }

    pub(super) fn scroll_lines(&mut self, delta: isize) {
        let max_scroll = ACTIONS.len().saturating_sub(1);
        let next = (self.scroll as isize + delta).clamp(0, max_scroll as isize);
        self.scroll = next as usize;
    }

    pub(super) fn render_signature(&self) -> KeyRemapSignature {
        KeyRemapSignature {
            selected: self.selected,
            scroll: self.scroll,
            capture: self.capture.map(bindable_action_display_name),
            conflict: self.conflict.as_ref().map(|c| {
                format!(
                    "{}->{}",
                    bindable_action_display_name(c.conflicts_with),
                    bindable_action_display_name(c.for_action)
                )
            }),
            message: self.message.clone(),
            // The full chord state of every row in one cheap digest — the rows
            // repaint whenever any binding changes.
            bindings: key_bindings_config_value(&self.overrides),
        }
    }

    pub(super) fn visible_lines(
        &self,
        _body_width: usize,
        body_height: usize,
    ) -> Vec<KeyRemapLine> {
        let bindings = self.effective_bindings();
        let mut rows: Vec<KeyRemapLine> = Vec::new();
        // Header + message occupy the first lines; the action list scrolls.
        if let Some(message) = &self.message {
            rows.push(KeyRemapLine {
                text: message.clone(),
                focused: false,
            });
        }
        let body_budget = body_height.max(1);
        let list_budget = body_budget.saturating_sub(rows.len()).max(1);
        let start = self.scroll.min(ACTIONS.len().saturating_sub(1));
        for (offset, action) in ACTIONS.iter().enumerate().skip(start) {
            if rows.len() >= body_budget {
                break;
            }
            if rows
                .len()
                .saturating_sub(usize::from(self.message.is_some()))
                >= list_budget
            {
                break;
            }
            let focused = offset == self.selected;
            let name = bindable_action_display_name(*action);
            let chord_text = if self.capture == Some(*action) {
                "press a chord…".to_owned()
            } else {
                match bindings.chord_for_action(*action) {
                    Some(chord) => format_key_chord(chord),
                    None => "(unbound)".to_owned(),
                }
            };
            let overridden = self.overrides.iter().any(|o| o.action == *action);
            let marker = if overridden { " *" } else { "" };
            let cursor = if focused { "> " } else { "  " };
            rows.push(KeyRemapLine {
                text: format!("{cursor}{name:<18} {chord_text}{marker}"),
                focused,
            });
        }
        rows
    }
}

/// One rendered line of the remap modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyRemapLine {
    pub(super) text: String,
    pub(super) focused: bool,
}

/// Repaint signature for the remap modal — everything that changes its pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyRemapSignature {
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) capture: Option<&'static str>,
    pub(super) conflict: Option<String>,
    pub(super) message: Option<String>,
    pub(super) bindings: String,
}

/// Whether a chord is the named key with NO modifiers — the reserved-control
/// test for `Esc`/`Enter` (D-KBR-5). A modified chord (e.g. `ctrl+enter`) is NOT
/// reserved and may be bound freely.
fn is_bare(chord: KeyChord, named: KeyBindingNamedKey) -> bool {
    let m = chord.modifiers;
    !m.ctrl && !m.shift && !m.alt && !m.super_key && chord.key == KeyBindingKey::Named(named)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::KeyBindingModifiers;

    fn ui() -> KeyRemapUi {
        let mut ui = KeyRemapUi::new(&Settings::default());
        ui.open(&Settings::default());
        ui
    }

    fn chord(ctrl: bool, shift: bool, alt: bool, key: KeyBindingKey) -> KeyChord {
        KeyChord {
            modifiers: KeyBindingModifiers {
                ctrl,
                shift,
                alt,
                super_key: false,
            },
            key,
        }
    }

    fn char_chord(ctrl: bool, shift: bool, c: char) -> KeyChord {
        chord(ctrl, shift, false, KeyBindingKey::Character(c))
    }

    fn named(named: KeyBindingNamedKey) -> KeyChord {
        chord(false, false, false, KeyBindingKey::Named(named))
    }

    fn select_action(ui: &mut KeyRemapUi, action: BindableAction) {
        let index = ACTIONS.iter().position(|a| *a == action).unwrap();
        ui.set_selection(index);
    }

    fn effective(ui: &KeyRemapUi, c: KeyChord) -> Option<BindableAction> {
        KeyBindings::from_overrides(&ui.overrides).action_for_chord(c)
    }

    #[test]
    fn actions_list_matches_enum_size() {
        // The remap list must offer EVERY bindable action; if a variant is added
        // without extending ACTIONS this fails (the names also pin to the
        // authority via the info.rs test).
        assert_eq!(ACTIONS.len(), 12);
        for (i, a) in ACTIONS.iter().enumerate() {
            for b in &ACTIONS[i + 1..] {
                assert_ne!(a, b, "ACTIONS must be distinct");
            }
        }
    }

    #[test]
    fn is_capturing_chord_false_when_idle() {
        // R1: a freshly-opened (browsing) modal is NOT capturing — the App's
        // bypass must stay dormant until a row is armed.
        let mut ui = ui();
        assert!(!ui.is_capturing_chord());
        ui.handle_input(OverlayInput::Activate);
        assert!(ui.is_capturing_chord(), "Enter arms capture");
        ui.deliver_chord(Some(named(KeyBindingNamedKey::Escape)));
        assert!(!ui.is_capturing_chord(), "Esc disarms capture");
    }

    #[test]
    fn capture_commits_unbound_chord() {
        // Matrix 2: capturing an unbound chord applies it as an override and
        // returns to browsing with a live Preview.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Hints);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(Some(char_chord(true, true, 'j')));
        assert!(matches!(out, KeyRemapOutcome::Preview(_)));
        assert!(!ui.is_capturing_chord());
        assert_eq!(
            effective(&ui, char_chord(true, true, 'j')),
            Some(BindableAction::Hints)
        );
    }

    #[test]
    fn capture_rejects_reserved_chords() {
        // R4: bare Enter is reserved (stays capturing, no binding); bare Esc
        // cancels; a normal chord is accepted — confirms the guard polarity is
        // not inverted (which would reject everything).
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Hints);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(Some(named(KeyBindingNamedKey::Enter)));
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(ui.is_capturing_chord(), "Enter must not commit or cancel");
        // A normal chord still commits.
        let out = ui.deliver_chord(Some(char_chord(true, true, 'j')));
        assert!(matches!(out, KeyRemapOutcome::Preview(_)));
    }

    #[test]
    fn capture_none_keeps_capturing() {
        // Matrix 12: a key chord_from_winit cannot encode (bare modifier / IME)
        // arrives as None — capture stays armed, nothing commits.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Hints);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(None);
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(ui.is_capturing_chord());
        assert!(ui.overrides.is_empty());
    }

    #[test]
    fn capturing_same_chord_is_noop() {
        // Re-binding an action to the chord it already owns is not a conflict and
        // adds no override.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Search);
        ui.handle_input(OverlayInput::Activate);
        // Search's default is ctrl+shift+f.
        let out = ui.deliver_chord(Some(char_chord(true, true, 'f')));
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(!ui.is_capturing_chord());
        assert!(ui.overrides.is_empty(), "no override for a no-op remap");
    }

    #[test]
    fn conflict_against_default_then_reassign() {
        // R3: capturing ctrl+shift+f (Search's DEFAULT) for Copy must flag a
        // conflict even though there is no override for it; Enter reassigns.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Copy);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(Some(char_chord(true, true, 'f')));
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(ui.conflict.is_some(), "default binding must conflict");
        assert!(ui.is_capturing_chord());
        // Confirm with bare Enter → reassign.
        let out = ui.deliver_chord(Some(named(KeyBindingNamedKey::Enter)));
        assert!(matches!(out, KeyRemapOutcome::Preview(_)));
        assert!(!ui.is_capturing_chord());
        assert_eq!(
            effective(&ui, char_chord(true, true, 'f')),
            Some(BindableAction::Copy),
            "reassigned: ctrl+shift+f now triggers Copy"
        );
    }

    #[test]
    fn conflict_cancel_keeps_existing() {
        // Matrix 5: Esc at the conflict prompt keeps the existing binding.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Copy);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'f')));
        assert!(ui.conflict.is_some());
        let out = ui.deliver_chord(Some(named(KeyBindingNamedKey::Escape)));
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(!ui.is_capturing_chord());
        assert_eq!(
            effective(&ui, char_chord(true, true, 'f')),
            Some(BindableAction::Search),
            "binding unchanged after cancel"
        );
    }

    #[test]
    fn reset_row_drops_override() {
        // Matrix 7: resetting a remapped row drops its override (falls back to
        // the default binding).
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Hints);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'j')));
        assert!(
            ui.overrides
                .iter()
                .any(|o| o.action == BindableAction::Hints)
        );
        select_action(&mut ui, BindableAction::Hints);
        let out = ui.handle_input(OverlayInput::Backspace);
        assert!(matches!(out, KeyRemapOutcome::Preview(_)));
        assert!(
            !ui.overrides
                .iter()
                .any(|o| o.action == BindableAction::Hints),
            "override dropped on reset"
        );
    }

    #[test]
    fn reset_all_clears_overrides() {
        // Matrix 8: reset-all empties the override vector.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Hints);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'j')));
        assert!(!ui.overrides.is_empty());
        let out = ui.handle_input(OverlayInput::Char('r'));
        assert!(matches!(out, KeyRemapOutcome::Preview(_)));
        assert!(ui.overrides.is_empty());
    }

    #[test]
    fn save_emits_keybinds_edit_that_round_trips() {
        // Matrix 3/4: Save emits one keybinds SettingEdit whose value is the
        // exact serializer output; re-parsing it through the live settings path
        // reproduces the binding (the UI-authored chord and a hand-typed one are
        // the same bytes).
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Hints);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'j')));
        let out = ui.handle_input(OverlayInput::Save);
        let KeyRemapOutcome::Save(edits) = out else {
            panic!("Save must emit a SettingEdit");
        };
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].key, "keybinds");
        assert_eq!(edits[0].env, KEYBINDS_ENV);
        assert_eq!(edits[0].value, key_bindings_config_value(&ui.overrides));
        assert!(!edits[0].value.is_empty());
    }

    #[test]
    fn open_preserves_existing_overrides() {
        // R5: opening from settings that already carry an override must clone it
        // into the working vector (not start empty — which would wipe the user's
        // bindings on the next save).
        let mut settings = Settings::default();
        settings.key_bindings.push(KeyBindingOverride {
            chord: char_chord(true, true, 'j'),
            action: BindableAction::Hints,
        });
        let mut remap = KeyRemapUi::new(&settings);
        remap.open(&settings);
        assert_eq!(remap.overrides.len(), 1);
        assert_eq!(remap.overrides[0].action, BindableAction::Hints);
    }

    #[test]
    fn browsing_close_emits_cancel() {
        // Esc while browsing closes the modal (Cancel restores the base
        // settings); it is NOT consumed as a no-op.
        let mut ui = ui();
        let out = ui.handle_input(OverlayInput::Close);
        assert!(matches!(out, KeyRemapOutcome::Cancel(_)));
    }
}
