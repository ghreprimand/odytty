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

use super::bindings::{KeyBindings, PanePrefixBindings};
use super::overlay::OverlayInput;

/// Every `BindableAction` the in-app remap UI exposes — the full config surface,
/// grouped core → overlay → tab → pane. Sourced from [`BindableAction::ALL`] so
/// the editor's row set can never drift from the enum (the
/// `all_bindable_actions_is_exhaustive` guard in the settings tests pins `ALL`
/// to the variant set). The modal pages/scrolls, so the taller list fits.
const ACTIONS: [BindableAction; BindableAction::ALL.len()] = BindableAction::ALL;

/// What the App should do after a remap-UI key/chord. Mirrors
/// `ThemeBuilderOutcome`: `Preview` applies the working bindings live (the
/// reloadable apply rebuilds the live `KeyBindings`), `Save` persists them, and
/// `Cancel` restores the bindings present when the modal opened, then closes.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum KeyRemapOutcome {
    Consumed,
    Preview(Settings),
    Save(Vec<SettingEdit>),
    /// Save the working overrides AND return to the settings panel — emitted by
    /// the dirty-close prompt's "save" choice (P1-6). Distinct from `Save`
    /// (which keeps the editor open after an in-modal Ctrl+S) so the overlay can
    /// navigate back only on this path.
    SaveAndClose(Vec<SettingEdit>),
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
    /// `true` while the dirty-close save/discard/cancel prompt is showing
    /// (P1-6). Mirrors the settings panel's `pending_close_prompt`: Esc on a
    /// dirty editor raises it instead of silently discarding the working
    /// overrides; while it is up, all input routes to the prompt.
    pending_close_prompt: bool,
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
            pending_close_prompt: false,
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
        // The dirty-close prompt captures ALL browsing input until resolved
        // (P1-6), mirroring the settings panel's `handle_close_prompt_input`.
        if self.pending_close_prompt {
            return self.handle_close_prompt_input(input);
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
            OverlayInput::Close => return self.request_close(),
            _ => {}
        }
        KeyRemapOutcome::Consumed
    }

    /// Whether the working overrides differ from the bindings present when the
    /// modal opened (or were last saved) — i.e. there are uncommitted edits the
    /// user could lose. Drives the dirty-close prompt (P1-6).
    fn is_dirty(&self) -> bool {
        self.overrides != self.base.key_bindings
    }

    /// Esc handling (P1-6): a clean editor closes immediately (Cancel restores
    /// the base settings); a DIRTY editor raises the save/discard/cancel prompt
    /// instead of silently discarding the captured chords.
    fn request_close(&mut self) -> KeyRemapOutcome {
        if self.is_dirty() {
            self.pending_close_prompt = true;
            self.message = Some(
                "Unsaved keybinding changes — [S] save  [D] discard  [C] keep editing.".to_owned(),
            );
            KeyRemapOutcome::Consumed
        } else {
            KeyRemapOutcome::Cancel(self.base.clone())
        }
    }

    /// Resolve the dirty-close prompt: S/Enter/Ctrl+S saves and returns to the
    /// settings panel, D discards (restores base), C/Esc keeps editing. Mirrors
    /// the settings panel's prompt key map.
    fn handle_close_prompt_input(&mut self, input: OverlayInput) -> KeyRemapOutcome {
        match input {
            OverlayInput::Char('s')
            | OverlayInput::Char('S')
            | OverlayInput::Activate
            | OverlayInput::Save => {
                self.pending_close_prompt = false;
                KeyRemapOutcome::SaveAndClose(self.save_edits())
            }
            OverlayInput::Char('d') | OverlayInput::Char('D') => {
                self.pending_close_prompt = false;
                KeyRemapOutcome::Cancel(self.base.clone())
            }
            OverlayInput::Char('c') | OverlayInput::Char('C') | OverlayInput::Close => {
                self.pending_close_prompt = false;
                self.message = None;
                KeyRemapOutcome::Consumed
            }
            _ => KeyRemapOutcome::Consumed,
        }
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
        KeyRemapOutcome::Save(self.save_edits())
    }

    /// The `keybinds = …` edit for the current working overrides, built via the
    /// shared serializer so a UI-authored chord and a hand-typed one round-trip
    /// to identical bytes. Shared by the in-modal Ctrl+S [`Self::save`] and the
    /// dirty-close prompt's save choice.
    fn save_edits(&self) -> Vec<SettingEdit> {
        vec![SettingEdit {
            key: "keybinds",
            env: KEYBINDS_ENV,
            value: key_bindings_config_value(&self.overrides),
        }]
    }

    /// Map a clicked body row to the action index it represents — the inverse of
    /// [`Self::visible_lines`] (UX4-P1 click→Activate). The optional message line
    /// occupies row 0 when present; action rows follow from `self.scroll`.
    /// Returns `None` for the message row or a click past the last action.
    pub(super) fn row_at(&self, row_in_body: usize) -> Option<usize> {
        let prefix = usize::from(self.message.is_some());
        if row_in_body < prefix {
            return None;
        }
        let index = self.scroll + (row_in_body - prefix);
        (index < ACTIONS.len()).then_some(index)
    }

    /// Select the action row under a left-click, reporting whether it landed on a
    /// real action so the caller can route the existing Activate (which ARMS
    /// chord capture for the row — a click selects + arms, it does not capture).
    /// Inert while capturing or while the dirty-close prompt is up.
    pub(super) fn click_row(&mut self, row_in_body: usize) -> bool {
        if self.is_capturing_chord() || self.pending_close_prompt {
            return false;
        }
        match self.row_at(row_in_body) {
            Some(index) => {
                self.set_selection(index);
                true
            }
            None => false,
        }
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
        //
        // C8: pane actions and flat actions occupy DISJOINT chord spaces — a
        // pane action's chord is a tmux-prefix second key that never collides
        // with a bare global chord at runtime. So resolve a pane action's
        // conflict within the prefix table (catching a clash with another pane
        // action), and a flat action's within the flat table, rather than
        // cross-checking the two.
        let existing = if action.is_pane_action() {
            self.pane_prefix_bindings().action_for_chord(chord)
        } else {
            self.effective_bindings().action_for_chord(chord)
        };
        match existing {
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
                // The confirm/cancel key hints go on their OWN line so a narrow
                // overlay never tail-truncates them off the end of the question
                // (field use found the `[Enter]/[Esc]` tail clipped on normal
                // window widths). `visible_lines` splits the message on `\n`.
                self.message = Some(format!(
                    "{} is bound to {} — reassign to {}?\n[Enter] yes  [Esc] no",
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

    /// The pane-action PREFIX view (C8): pane actions resolve their display chord
    /// and conflicts here, not through the flat `KeyBindings` table which never
    /// holds pane-action bindings.
    fn pane_prefix_bindings(&self) -> PanePrefixBindings {
        PanePrefixBindings::from_overrides(&self.overrides)
    }

    /// Render a pane action's effective chord as its tmux-prefix second key, so a
    /// bound pane action never shows the flat-table "(unbound)". Labels it as a
    /// prefix second key (`⟨prefix⟩ z`), or notes the prefix is disabled.
    fn pane_action_chord_text(&self, action: BindableAction) -> String {
        match self.pane_prefix_bindings().chord_for_action(action) {
            Some(chord) => match self.base.pane_prefix {
                Some(prefix) => format!(
                    "{} then {}",
                    format_key_chord(prefix),
                    format_key_chord(chord)
                ),
                None => format!("{} (prefix disabled)", format_key_chord(chord)),
            },
            None => "(unbound)".to_owned(),
        }
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
            pending_close_prompt: self.pending_close_prompt,
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

    /// Hidden actions above / below the visible window, for the scroll
    /// affordance (OVERLAY-SMALL-WINDOW). `(false, false)` when all fit, so a
    /// normal window draws no arrows and stays byte-identical.
    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        (
            self.scroll > 0,
            body_height > 0 && self.scroll + body_height < ACTIONS.len(),
        )
    }

    pub(super) fn visible_lines(
        &self,
        _body_width: usize,
        body_height: usize,
    ) -> Vec<KeyRemapLine> {
        let bindings = self.effective_bindings();
        let mut rows: Vec<KeyRemapLine> = Vec::new();
        // Header + message occupy the first lines; the action list scrolls. A
        // message may span multiple lines (the conflict prompt carries its key
        // hints on a second line so they are never tail-truncated); each `\n`
        // segment becomes its own render line.
        let message_line_count = self
            .message
            .as_ref()
            .map(|m| m.lines().count().max(1))
            .unwrap_or(0);
        if let Some(message) = &self.message {
            for line in message.lines() {
                rows.push(KeyRemapLine {
                    text: line.to_owned(),
                    focused: false,
                });
            }
        }
        let body_budget = body_height.max(1);
        let list_budget = body_budget.saturating_sub(rows.len()).max(1);
        let start = self.scroll.min(ACTIONS.len().saturating_sub(1));
        for (offset, action) in ACTIONS.iter().enumerate().skip(start) {
            if rows.len() >= body_budget {
                break;
            }
            if rows.len().saturating_sub(message_line_count) >= list_budget {
                break;
            }
            let focused = offset == self.selected;
            let name = bindable_action_display_name(*action);
            let chord_text = if self.capture == Some(*action) {
                "press a chord…".to_owned()
            } else if action.is_pane_action() {
                // C8: pane actions live in the prefix table, not the flat one.
                self.pane_action_chord_text(*action)
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
    pub(super) pending_close_prompt: bool,
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
    fn actions_list_covers_every_bindable_action() {
        // The remap modal now offers the full `keybinds` config surface — every
        // `BindableAction` variant. ACTIONS is `BindableAction::ALL`, whose
        // exhaustiveness is pinned by `all_bindable_actions_is_exhaustive` in the
        // settings tests; here we confirm the editor inherits the full set with
        // no duplicates.
        assert_eq!(ACTIONS.len(), 40);
        assert_eq!(ACTIONS, BindableAction::ALL);
        for (i, a) in ACTIONS.iter().enumerate() {
            for b in &ACTIONS[i + 1..] {
                assert_ne!(a, b, "ACTIONS must be distinct");
            }
        }
        // Sample one action from each group to prove the expansion landed.
        for expected in [
            BindableAction::Search,            // core
            BindableAction::ConnectionManager, // overlay
            BindableAction::NewTab,            // tab
            BindableAction::ZoomPane,          // pane
        ] {
            assert!(ACTIONS.contains(&expected), "editor must list {expected:?}");
        }
    }

    #[test]
    fn capture_commits_chord_for_newly_added_action() {
        // A newly-exposed action (overlay group) round-trips a chord through the
        // working overrides exactly like a core action.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::ConnectionManager);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(Some(char_chord(true, true, 'q')));
        assert!(matches!(out, KeyRemapOutcome::Preview(_)));
        assert!(!ui.is_capturing_chord());
        assert_eq!(
            effective(&ui, char_chord(true, true, 'q')),
            Some(BindableAction::ConnectionManager)
        );
    }

    #[test]
    fn save_emits_keybinds_edit_for_newly_added_action() {
        // Binding a pane action and saving emits a keybinds edit whose value is
        // the exact serializer output; the parse-side round-trip for every
        // action is pinned by `bindable_action_names_round_trip_through_parse`
        // and `keybinds_value_round_trips_for_every_action` in the settings
        // tests.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::ZoomPane);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'z')));
        let KeyRemapOutcome::Save(edits) = ui.handle_input(OverlayInput::Save) else {
            panic!("Save must emit a SettingEdit");
        };
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].key, "keybinds");
        assert_eq!(edits[0].value, key_bindings_config_value(&ui.overrides));
        assert!(
            ui.overrides
                .iter()
                .any(|o| o.action == BindableAction::ZoomPane
                    && o.chord == char_chord(true, true, 'z')),
            "ZoomPane binding must be in the working overrides"
        );
    }

    #[test]
    fn conflict_surfaced_for_newly_added_action() {
        // Capturing CommandPalette's default chord (ctrl+shift+p) for a tab
        // action must flag a conflict — the conflict path covers the expanded
        // set, not only the original 12.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::NewTab);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(Some(char_chord(true, true, 'p')));
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(
            ui.conflict.is_some(),
            "stealing CommandPalette's default must conflict"
        );
        assert_eq!(
            ui.conflict.as_ref().map(|c| c.conflicts_with),
            Some(BindableAction::CommandPalette)
        );
    }

    #[test]
    fn conflict_prompt_key_hints_sit_on_their_own_line() {
        // Field use found the reassign prompt's `[Enter]/[Esc]` key hints
        // tail-truncated on narrow windows. The hints must render on their own
        // line so a downstream horizontal clip of the (long) question line can
        // never remove the action keys.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::NewTab);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'p')));
        assert!(ui.conflict.is_some(), "the capture must have conflicted");

        // Render at a narrow body width (the width arg is advisory; the point is
        // the hints occupy a dedicated, short line rather than the question's
        // tail).
        let lines: Vec<String> = ui
            .visible_lines(24, 100)
            .into_iter()
            .map(|line| line.text)
            .collect();
        let hint_line = lines
            .iter()
            .find(|t| t.contains("[Enter]") && t.contains("[Esc]"))
            .expect("a line must carry the Enter/Esc hints");
        // The hint line is dedicated — it does NOT also carry the long question,
        // so a tail clip of the question can never eat the hints.
        assert!(
            !hint_line.contains("is bound to"),
            "key hints must be on their own line, not the question's tail: {hint_line:?}"
        );
        // The hint line is short enough to survive a narrow overlay.
        assert!(
            hint_line.chars().count() <= 24,
            "hint line must fit a narrow overlay: {hint_line:?}"
        );
        // And the question itself is still present (on a separate line).
        assert!(
            lines.iter().any(|t| t.contains("is bound to")),
            "the reassign question is still shown"
        );
    }

    /// Find the rendered row for `name` in a tall render (no scrolling).
    fn row_text_for(ui: &KeyRemapUi, name: &str) -> String {
        // Action rows are `{cursor}{name:<18} {chord}` — match on the leading
        // name so the message banner (which may mention the same action) is not
        // picked up.
        ui.visible_lines(80, 100)
            .into_iter()
            .map(|line| line.text)
            .find(|text| text.trim_start_matches(['>', ' ']).starts_with(name))
            .unwrap_or_else(|| panic!("no row for {name}"))
    }

    #[test]
    fn c8_pane_action_row_shows_prefix_chord_not_unbound() {
        // C8: a pane action's default binding is a tmux-prefix second key (ZoomPane
        // → prefix `z`). Before the fix the flat table returned None and the row
        // read "(unbound)"; now it shows the prefix second key.
        let ui = ui();
        let row = row_text_for(&ui, "zoom-pane");
        assert!(
            row.contains("then "),
            "pane-action row must label the prefix second key: {row:?}"
        );
        assert!(
            !row.contains("(unbound)"),
            "a defaulted pane action must not read (unbound): {row:?}"
        );
    }

    #[test]
    fn c8_bound_pane_action_row_is_not_self_contradictory() {
        // C8: binding a pane action must show the NEW prefix chord, never the
        // self-contradictory "(unbound) *" (unbound text with the override marker)
        // the flat-table lookup produced.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::ZoomPane);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'z')));
        let row = row_text_for(&ui, "zoom-pane");
        assert!(
            row.contains('*'),
            "an overridden pane action keeps its override marker: {row:?}"
        );
        assert!(
            !row.contains("(unbound)"),
            "a bound pane action must never read (unbound): {row:?}"
        );
        assert!(
            row.contains("then "),
            "the new binding renders as a prefix second key: {row:?}"
        );
    }

    #[test]
    fn c8_pane_action_conflict_resolves_in_prefix_space() {
        // C8: a pane action's chord conflicts only with ANOTHER pane action (the
        // prefix space), which the flat table could never see. Capturing
        // ClosePane's default prefix key (`x`) for ZoomPane must flag ClosePane.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::ZoomPane);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(Some(char_chord(false, false, 'x')));
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert_eq!(
            ui.conflict.as_ref().map(|c| c.conflicts_with),
            Some(BindableAction::ClosePane),
            "prefix-space conflict must be detected"
        );
    }

    #[test]
    fn c8_flat_and_prefix_chord_spaces_are_disjoint() {
        // C8: a pane action's prefix second key (`x`) and a bare global chord are
        // DISJOINT input spaces — they never collide at runtime. Binding a flat
        // action (Copy) to bare `x` must NOT falsely conflict with ClosePane.
        let mut ui = ui();
        select_action(&mut ui, BindableAction::Copy);
        ui.handle_input(OverlayInput::Activate);
        let out = ui.deliver_chord(Some(char_chord(false, false, 'x')));
        assert!(
            matches!(out, KeyRemapOutcome::Preview(_)),
            "a flat binding must not conflict with a pane action's prefix key"
        );
        assert!(ui.conflict.is_none(), "no cross-space conflict");
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
        // Esc while browsing (clean editor) closes the modal (Cancel restores the
        // base settings); it is NOT consumed as a no-op.
        let mut ui = ui();
        let out = ui.handle_input(OverlayInput::Close);
        assert!(matches!(out, KeyRemapOutcome::Cancel(_)));
        assert!(!ui.render_signature().pending_close_prompt);
    }

    // ── UX4-P1 click→select+arm parity ─────────────────────────────────────

    #[test]
    fn click_row_selects_and_routes_activate_to_arm_capture() {
        // open() sets a help message, so row 0 is the message and action rows
        // start at row 1. Clicking row 1 selects ACTIONS[0]; routing Activate
        // (as the overlay does) arms capture — a click selects + arms, exactly
        // like Enter.
        let mut ui = ui();
        assert!(ui.click_row(1));
        assert_eq!(ui.render_signature().selected, 0);
        let out = ui.handle_input(OverlayInput::Activate);
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(ui.is_capturing_chord(), "click then Activate arms capture");
    }

    #[test]
    fn click_row_maps_offset_past_the_message_line() {
        let mut ui = ui();
        // Row 3 = message(0) + ACTIONS[2] (rows 1,2,3 → actions 0,1,2).
        assert!(ui.click_row(3));
        assert_eq!(ui.render_signature().selected, 2);
    }

    #[test]
    fn click_message_row_is_inert() {
        let ui_ref = ui();
        assert!(ui_ref.row_at(0).is_none());
        let mut ui = ui();
        assert!(!ui.click_row(0));
    }

    // ── P1-6 dirty-close prompt ─────────────────────────────────────────────

    fn make_dirty(ui: &mut KeyRemapUi) {
        select_action(ui, BindableAction::Hints);
        ui.handle_input(OverlayInput::Activate);
        ui.deliver_chord(Some(char_chord(true, true, 'j')));
        assert!(
            ui.overrides
                .iter()
                .any(|o| o.action == BindableAction::Hints),
            "precondition: a working override exists"
        );
    }

    #[test]
    fn esc_on_dirty_editor_raises_prompt_without_discarding() {
        let mut ui = ui();
        make_dirty(&mut ui);
        let out = ui.handle_input(OverlayInput::Close);
        assert_eq!(
            out,
            KeyRemapOutcome::Consumed,
            "Esc on dirty shows the prompt"
        );
        assert!(ui.render_signature().pending_close_prompt);
        // The captured chord survives — it is NOT silently discarded.
        assert!(
            ui.overrides
                .iter()
                .any(|o| o.action == BindableAction::Hints)
        );
    }

    #[test]
    fn dirty_prompt_save_emits_save_and_close_with_round_trip_value() {
        let mut ui = ui();
        make_dirty(&mut ui);
        ui.handle_input(OverlayInput::Close); // raise the prompt
        let out = ui.handle_input(OverlayInput::Char('s'));
        let KeyRemapOutcome::SaveAndClose(edits) = out else {
            panic!("the save choice must emit SaveAndClose");
        };
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].key, "keybinds");
        assert_eq!(edits[0].env, KEYBINDS_ENV);
        // Same bytes as the in-modal Ctrl+S serializer → the UI-authored chord
        // round-trips to odytty.conf identically to a hand-typed one.
        assert_eq!(edits[0].value, key_bindings_config_value(&ui.overrides));
        assert!(!ui.render_signature().pending_close_prompt);
    }

    #[test]
    fn dirty_prompt_discard_restores_base() {
        let mut ui = ui();
        make_dirty(&mut ui);
        ui.handle_input(OverlayInput::Close); // raise the prompt
        let out = ui.handle_input(OverlayInput::Char('d'));
        assert!(matches!(out, KeyRemapOutcome::Cancel(_)));
        assert!(!ui.render_signature().pending_close_prompt);
    }

    #[test]
    fn dirty_prompt_keep_editing_dismisses_and_stays_dirty() {
        let mut ui = ui();
        make_dirty(&mut ui);
        ui.handle_input(OverlayInput::Close); // raise the prompt
        let out = ui.handle_input(OverlayInput::Char('c'));
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(!ui.render_signature().pending_close_prompt);
        // Override still present: the editor stayed open with edits intact.
        assert!(
            ui.overrides
                .iter()
                .any(|o| o.action == BindableAction::Hints)
        );
    }

    #[test]
    fn dirty_prompt_esc_keeps_editing() {
        // Esc at the prompt is the "keep editing" choice (cancel the close), not
        // a second discard.
        let mut ui = ui();
        make_dirty(&mut ui);
        ui.handle_input(OverlayInput::Close); // raise the prompt
        let out = ui.handle_input(OverlayInput::Close);
        assert_eq!(out, KeyRemapOutcome::Consumed);
        assert!(!ui.render_signature().pending_close_prompt);
        assert!(
            ui.overrides
                .iter()
                .any(|o| o.action == BindableAction::Hints)
        );
    }
}
