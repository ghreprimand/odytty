// SPDX-License-Identifier: GPL-3.0-only
use crate::core::{
    MouseButton as CoreMouseButton, MouseEventKind, MouseModifiers as CoreMouseModifiers,
    MouseProtocol, MouseTracking, Terminal, encode_focus_event, encode_mouse_event,
};
use crate::input::{Key, Modifiers};
use crate::selection::CellPoint;
use crate::settings::{
    BindableAction, KeyBindingKey, KeyBindingModifiers, KeyBindingNamedKey, KeyBindingOverride,
    KeyChord,
};

use winit::event::{MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey, PhysicalKey};

use std::time::{Duration, Instant};

use super::viewport::wheel_lines;

/// Default time a pressed prefix waits for its second key before cancelling
/// (§7). Matches tmux's default `escape-time`-adjacent feel; long enough for a
/// deliberate two-key sequence, short enough that a stray prefix clears fast.
pub(super) const PREFIX_TIMEOUT: Duration = Duration::from_secs(2);

/// Outcome of feeding a keychord to the multiplexer prefix engine (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrefixOutcome {
    /// No prefix configured, or not pending and this chord is not the prefix —
    /// the caller processes the chord through the normal path. This is the
    /// byte-identical case: when not pending, every non-prefix chord is
    /// `Inactive` and nothing about the existing input path changes.
    Inactive,
    /// This chord was the prefix; the engine is now prefix-pending. The caller
    /// swallows the chord (sends nothing to the PTY) and waits for the next key.
    Entered,
    /// Doubled prefix while pending (`Ctrl-b Ctrl-b`) — the caller sends the
    /// literal prefix bytes ([`PrefixEngine::passthrough_bytes`]) to the focused
    /// pane's PTY so a nested multiplexer still gets its prefix (K3). Back to
    /// normal afterward.
    Passthrough,
    /// A prefix-table pane action resolved while pending. The caller dispatches
    /// it (K2) and the engine returns to normal.
    Action(BindableAction),
    /// Prefix was pending but this chord matched nothing in the table (or the
    /// pending state had already timed out) — the caller swallows the chord,
    /// fires no action, and returns to normal (tmux swallows unknown prefix
    /// keys).
    Cancelled,
}

/// The multiplexer prefix-sequence engine (§7, K1). Holds the configurable
/// prefix chord, the pane-action table resolved while pending, and the transient
/// pending state. Additive by construction: when `prefix` is `None` or the
/// engine is not pending, [`Self::on_chord`] returns [`PrefixOutcome::Inactive`]
/// for every chord, so the existing input path is byte-identical.
#[derive(Debug, Clone)]
pub(super) struct PrefixEngine {
    /// The configured prefix chord; `None` disables the whole feature.
    prefix: Option<KeyChord>,
    /// Pane-action bindings resolved while prefix-pending (tmux defaults plus
    /// any pane-action overrides). Chords are stored lookup-normalized.
    table: Vec<(KeyChord, BindableAction)>,
    /// When the prefix was pressed; `None` when not pending.
    pending_since: Option<Instant>,
    /// How long a pending prefix waits for its second key before cancelling.
    timeout: Duration,
}

impl PrefixEngine {
    /// Build the engine from the configured prefix chord, the user's key-binding
    /// overrides (pane-action entries rebind the prefix table), and a timeout.
    pub(super) fn new(
        prefix: Option<KeyChord>,
        overrides: &[KeyBindingOverride],
        timeout: Duration,
    ) -> Self {
        let mut table: Vec<(KeyChord, BindableAction)> = default_prefix_bindings()
            .into_iter()
            .map(|(chord, action)| (normalize_lookup_chord(chord), action))
            .collect();
        for override_ in overrides {
            if !override_.action.is_pane_action() {
                continue;
            }
            table.retain(|(_, action)| *action != override_.action);
            table.push((normalize_lookup_chord(override_.chord), override_.action));
        }
        Self {
            prefix,
            table,
            pending_since: None,
            timeout,
        }
    }

    /// Build the engine from settings (the production constructor).
    pub(super) fn from_settings(settings: &crate::settings::Settings) -> Self {
        Self::new(settings.pane_prefix, &settings.key_bindings, PREFIX_TIMEOUT)
    }

    /// Whether a prefix is currently pending (the next key resolves against the
    /// prefix table). Used by the K1 state-machine tests and reserved for the
    /// pending-state visual affordance; not yet read on the production frame
    /// path.
    #[allow(dead_code)]
    pub(super) fn is_pending(&self) -> bool {
        self.pending_since.is_some()
    }

    /// The instant at which a pending prefix cancels, for the event loop's
    /// wait deadline. `None` when not pending.
    pub(super) fn pending_deadline(&self) -> Option<Instant> {
        self.pending_since.map(|since| since + self.timeout)
    }

    /// Clear any pending prefix (e.g. on focus loss). Idempotent.
    pub(super) fn cancel(&mut self) {
        self.pending_since = None;
    }

    /// Drop a pending prefix whose timeout has elapsed by `now`, returning `true`
    /// when one was cleared. Called from the event loop's about-to-wait
    /// maintenance so a prefix that times out with no follow-up key is forgotten
    /// on the timer instead of lingering until the next keypress.
    ///
    /// Without this, `pending_deadline()` keeps reporting a boundary that has
    /// already passed: the loop schedules `WaitUntil(that boundary)`, wakes to
    /// find it in the past, re-reads the same still-pending deadline, and
    /// re-arms `WaitUntil(past)` — a 0-timeout poll that returns immediately
    /// every iteration, busy-spinning a core (frozen `voluntary_ctxt_switches`)
    /// until the next key or focus loss clears the prefix. Mirrors the timeout
    /// check in [`Self::on_chord`] so the timer and the next-key paths agree on
    /// when a prefix is stale (using `>=` here so the boundary the loop was woken
    /// at is treated as expired in that same pass, never re-armed).
    pub(super) fn expire_pending(&mut self, now: Instant) -> bool {
        if let Some(since) = self.pending_since
            && now.duration_since(since) >= self.timeout
        {
            self.pending_since = None;
            return true;
        }
        false
    }

    /// The literal bytes to forward to the PTY for the doubled-prefix passthrough
    /// (K3). For a `Ctrl+<letter>` prefix this is the corresponding C0 control
    /// byte (`Ctrl-b` → `0x02`); empty for prefixes with no single-byte literal.
    pub(super) fn passthrough_bytes(&self) -> Vec<u8> {
        let Some(chord) = self.prefix else {
            return Vec::new();
        };
        if let KeyBindingKey::Character(ch) = chord.key {
            let m = chord.modifiers;
            if m.ctrl && !m.alt && !m.super_key {
                let upper = ch.to_ascii_uppercase();
                if upper.is_ascii_uppercase() {
                    return vec![(upper as u8) - 0x40];
                }
                // Ctrl with a few non-letter keys also has C0 encodings.
                match ch {
                    ' ' => return vec![0x00],
                    '@' => return vec![0x00],
                    '[' => return vec![0x1b],
                    '\\' => return vec![0x1c],
                    ']' => return vec![0x1d],
                    '^' => return vec![0x1e],
                    '_' => return vec![0x1f],
                    _ => {}
                }
            }
        }
        Vec::new()
    }

    /// Feed a keychord to the engine, advancing the prefix state machine and
    /// returning what the caller should do. `now` drives the pending timeout so
    /// the state machine is deterministically unit-testable.
    pub(super) fn on_chord(&mut self, chord: KeyChord, now: Instant) -> PrefixOutcome {
        // Resolve any pending state first.
        if let Some(since) = self.pending_since {
            if now.duration_since(since) > self.timeout {
                // The pending prefix expired; forget it and treat this chord as
                // fresh input (it may itself be the prefix, re-entering).
                self.pending_since = None;
            } else {
                // Still pending: doubled prefix → passthrough; table hit →
                // action; anything else → clean cancel. Either way, no longer
                // pending afterward.
                self.pending_since = None;
                if self.is_prefix(chord) {
                    return PrefixOutcome::Passthrough;
                }
                if let Some(action) = self.lookup(chord) {
                    return PrefixOutcome::Action(action);
                }
                return PrefixOutcome::Cancelled;
            }
        }
        // Not pending: only the prefix chord does anything new.
        if self.is_prefix(chord) {
            self.pending_since = Some(now);
            return PrefixOutcome::Entered;
        }
        PrefixOutcome::Inactive
    }

    /// The configured prefix chord (`None` when the multiplexer prefix is
    /// disabled, e.g. `ODYTTY_PANE_PREFIX=off`). Used to label the prefix-only
    /// pane actions (e.g. Close Pane) in the context menu.
    pub(super) fn prefix(&self) -> Option<KeyChord> {
        self.prefix
    }

    /// The *second* chord bound to `action` in the prefix table (the key pressed
    /// after the prefix), or `None` when the action has no prefix binding. Scans
    /// newest-first so an override wins over the default, mirroring
    /// [`KeyBindings::chord_for_action`]. Returns the lookup-normalized chord
    /// (shift folded for character keys), which is what gets displayed.
    pub(super) fn chord_for_action(&self, action: BindableAction) -> Option<KeyChord> {
        self.table
            .iter()
            .rev()
            .find_map(|(chord, candidate)| (*candidate == action).then_some(*chord))
    }

    fn is_prefix(&self, chord: KeyChord) -> bool {
        self.prefix == Some(chord)
    }

    fn lookup(&self, chord: KeyChord) -> Option<BindableAction> {
        let normalized = normalize_lookup_chord(chord);
        self.table
            .iter()
            .rev()
            .find_map(|(candidate, action)| (*candidate == normalized).then_some(*action))
    }
}

/// Normalize a chord for prefix-table lookup: for a character key, drop the
/// shift modifier, because the produced character already encodes shift (`%`
/// arrives as Shift+`%` on a US layout but should match a stored `%`). Named
/// keys (arrows, Space) keep their modifiers exactly.
fn normalize_lookup_chord(chord: KeyChord) -> KeyChord {
    match chord.key {
        KeyBindingKey::Character(_) => KeyChord {
            modifiers: KeyBindingModifiers {
                shift: false,
                ..chord.modifiers
            },
            key: chord.key,
        },
        KeyBindingKey::Named(_) => chord,
    }
}

/// The default tmux-matching prefix bindings (§7, K2). Chords are the *second*
/// key after the prefix; the prefix itself is configured separately.
fn default_prefix_bindings() -> Vec<(KeyChord, BindableAction)> {
    vec![
        (
            char_chord('%', false, false, false, false),
            BindableAction::SplitColumns,
        ),
        (
            char_chord('"', false, false, false, false),
            BindableAction::SplitRows,
        ),
        (
            named_chord(KeyBindingNamedKey::ArrowLeft, false, false, false, false),
            BindableAction::FocusPaneLeft,
        ),
        (
            named_chord(KeyBindingNamedKey::ArrowRight, false, false, false, false),
            BindableAction::FocusPaneRight,
        ),
        (
            named_chord(KeyBindingNamedKey::ArrowUp, false, false, false, false),
            BindableAction::FocusPaneUp,
        ),
        (
            named_chord(KeyBindingNamedKey::ArrowDown, false, false, false, false),
            BindableAction::FocusPaneDown,
        ),
        (
            char_chord('o', false, false, false, false),
            BindableAction::FocusPaneNext,
        ),
        (
            char_chord('x', false, false, false, false),
            BindableAction::ClosePane,
        ),
        (
            char_chord('z', false, false, false, false),
            BindableAction::ZoomPane,
        ),
        (
            named_chord(KeyBindingNamedKey::Space, false, false, false, false),
            BindableAction::EqualizePanes,
        ),
        (
            char_chord('=', false, false, false, false),
            BindableAction::EqualizePanes,
        ),
    ]
}

/// A read-only view of the pane-action PREFIX table (tmux-prefix second keys)
/// for the KB-REMAP modal (C8).
///
/// Pane actions live ONLY in the multiplexer prefix space ([`PrefixEngine`]),
/// never in the flat [`KeyBindings`] table (`KeyBindings::from_overrides` skips
/// pane-action overrides). So the remap UI must resolve a pane action's display
/// chord and its conflicts here rather than through the flat table — which
/// always returned `None` for them, rendering a bound pane action as the
/// self-contradictory "(unbound) *". The two spaces are disjoint: a prefix
/// second-key never collides with a bare global chord at runtime, so a
/// pane-action chord only conflicts with ANOTHER pane action's chord.
///
/// Chords are kept RAW (as authored), matching how [`KeyBindings`] stores
/// override chords, so `format_key_chord` renders exactly what the user pressed
/// and conflict comparison is raw-to-raw on both sides.
pub(in crate::native) struct PanePrefixBindings {
    bindings: Vec<(KeyChord, BindableAction)>,
}

impl PanePrefixBindings {
    /// Build from the working overrides: the tmux defaults plus any pane-action
    /// overrides (later entries win), mirroring [`PrefixEngine::new`]'s table
    /// build minus the lookup normalization (the UI wants raw chords).
    pub(in crate::native) fn from_overrides(overrides: &[KeyBindingOverride]) -> Self {
        let mut bindings = default_prefix_bindings();
        for override_ in overrides {
            if !override_.action.is_pane_action() {
                continue;
            }
            bindings.retain(|(_, action)| *action != override_.action);
            bindings.push((override_.chord, override_.action));
        }
        Self { bindings }
    }

    /// The effective prefix second-key currently bound to `action`, newest-first
    /// so an override wins over a default.
    pub(in crate::native) fn chord_for_action(&self, action: BindableAction) -> Option<KeyChord> {
        self.bindings
            .iter()
            .rev()
            .find_map(|(chord, candidate)| (*candidate == action).then_some(*chord))
    }

    /// The pane action a prefix second-key resolves to, newest-first.
    pub(in crate::native) fn action_for_chord(&self, chord: KeyChord) -> Option<BindableAction> {
        self.bindings
            .iter()
            .rev()
            .find_map(|(candidate, action)| (*candidate == chord).then_some(*action))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyBindings {
    bindings: Vec<(KeyChord, BindableAction)>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            bindings: default_key_bindings(),
        }
    }
}

impl KeyBindings {
    pub(super) fn from_overrides(overrides: &[KeyBindingOverride]) -> Self {
        let mut bindings = default_key_bindings();
        for override_ in overrides {
            // Pane-management actions (§7) live only in the multiplexer prefix
            // table (`PrefixEngine`), never in the flat global table — so a
            // pane-action override cannot bind a bare global chord and perturb
            // the no-prefix input path.
            if override_.action.is_pane_action() {
                continue;
            }
            bindings.retain(|(_, action)| *action != override_.action);
            bindings.push((override_.chord, override_.action));
        }
        Self { bindings }
    }

    pub(super) fn action_for(
        &self,
        logical: &WinitKey,
        mods: Modifiers,
        super_key: bool,
    ) -> Option<BindableAction> {
        let chord = chord_from_winit(logical, mods, super_key)?;
        self.action_for_chord(chord)
    }

    /// Resolve the action a `KeyChord` triggers under these bindings (KB-REMAP
    /// conflict detection). Scans newest-first so a working override shadows a
    /// default, matching [`Self::action_for`]'s precedence exactly.
    pub(in crate::native) fn action_for_chord(&self, chord: KeyChord) -> Option<BindableAction> {
        self.bindings
            .iter()
            .rev()
            .find_map(|(candidate, action)| (*candidate == chord).then_some(*action))
    }

    /// The effective chord currently bound to `action` (KB-REMAP row display),
    /// or `None` if every default/override for it was removed. Scans newest-first
    /// so the active override wins over a default of the same action.
    pub(in crate::native) fn chord_for_action(&self, action: BindableAction) -> Option<KeyChord> {
        self.bindings
            .iter()
            .rev()
            .find_map(|(chord, candidate)| (*candidate == action).then_some(*chord))
    }
}

fn default_key_bindings() -> Vec<(KeyChord, BindableAction)> {
    vec![
        // Direct split chords (GUI). Unlike the other pane-management actions
        // (focus/close/zoom/equalize), which resolve only on the tmux prefix,
        // the two *creation* splits have direct chords so the first split on a
        // single-pane tab is reachable without the prefix (the prefix engine is
        // gated off at single-pane for byte-identity). Matches Ghostty's Linux
        // defaults: Ctrl+Shift+E splits columns (new pane right), Ctrl+Shift+O
        // splits rows (new pane down). They work at single-pane and multi-pane;
        // the prefix `%`/`"` keep working once a tab is split.
        (
            char_chord('e', true, true, false, false),
            BindableAction::SplitColumns,
        ),
        (
            char_chord('o', true, true, false, false),
            BindableAction::SplitRows,
        ),
        (
            char_chord('f', true, true, false, false),
            BindableAction::Search,
        ),
        (
            char_chord(',', true, true, false, false),
            BindableAction::SettingsPanel,
        ),
        (
            char_chord('h', true, true, false, false),
            BindableAction::ThemePicker,
        ),
        (
            char_chord('c', true, true, false, false),
            BindableAction::Copy,
        ),
        (
            char_chord('v', true, true, false, false),
            BindableAction::Paste,
        ),
        (
            named_chord(KeyBindingNamedKey::PageUp, false, true, false, false),
            BindableAction::ScrollPageUp,
        ),
        (
            named_chord(KeyBindingNamedKey::PageDown, false, true, false, false),
            BindableAction::ScrollPageDown,
        ),
        // Prompt navigation (OSC 133): the `Ctrl+Shift+Up/Down` arrow chords are
        // the primary (and now sole) default bindings. The `Ctrl+Shift+P` /
        // `Ctrl+Shift+N` letter fallbacks were dropped in v0.3.1 so the
        // industry-standard `Ctrl+Shift+P` can open the command palette; prompt
        // jump still fully works via the arrows (and users can re-add letter
        // chords with `keybinds` if a terminal intercepts the arrow chords).
        (
            named_chord(KeyBindingNamedKey::ArrowUp, true, true, false, false),
            BindableAction::JumpPromptPrev,
        ),
        (
            named_chord(KeyBindingNamedKey::ArrowDown, true, true, false, false),
            BindableAction::JumpPromptNext,
        ),
        // Discoverability defaults (v0.3.1): each of these overlays previously
        // had no default chord. They are all `Ctrl+Shift+<letter>`, which a TUI
        // cannot receive as input, so no PTY input path is perturbed.
        (
            char_chord('p', true, true, false, false),
            BindableAction::CommandPalette,
        ),
        (
            char_chord('s', true, true, false, false),
            BindableAction::ConnectionManager,
        ),
        (
            char_chord('r', true, true, false, false),
            BindableAction::SessionReplay,
        ),
        (
            char_chord('b', true, true, false, false),
            BindableAction::ThemeBuilder,
        ),
        (
            char_chord('a', true, true, false, false),
            BindableAction::SessionAttach,
        ),
        (
            named_chord(KeyBindingNamedKey::Space, true, true, false, false),
            BindableAction::CopyMode,
        ),
        (
            char_chord('l', true, true, false, false),
            BindableAction::Hints,
        ),
        (
            char_chord('k', true, true, false, false),
            BindableAction::ClearInput,
        ),
        (
            char_chord('t', true, true, false, false),
            BindableAction::NewTab,
        ),
        // New Window (F1): launch another OdyTTY instance. `Ctrl+Shift+N` is the
        // gnome-terminal / kitty convention; it was freed in v0.3.1 (the old
        // prompt-jump letter fallback was dropped) and is reclaimed here.
        (
            char_chord('n', true, true, false, false),
            BindableAction::NewWindow,
        ),
        (
            char_chord('w', true, true, false, false),
            BindableAction::CloseTab,
        ),
        (
            named_chord(KeyBindingNamedKey::PageDown, true, false, false, false),
            BindableAction::NextTab,
        ),
        (
            named_chord(KeyBindingNamedKey::PageUp, true, false, false, false),
            BindableAction::PrevTab,
        ),
    ]
}

fn char_chord(ch: char, ctrl: bool, shift: bool, alt: bool, super_key: bool) -> KeyChord {
    KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl,
            shift,
            alt,
            super_key,
        },
        key: KeyBindingKey::Character(ch.to_ascii_lowercase()),
    }
}

fn named_chord(
    named: KeyBindingNamedKey,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_key: bool,
) -> KeyChord {
    KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl,
            shift,
            alt,
            super_key,
        },
        key: KeyBindingKey::Named(named),
    }
}

// Visibility widened to `pub(in crate::native)` (D-KBR-7) so the chord-capture
// bypass in `app/mod.rs::handle_overlay_key` can build a raw `KeyChord` from a
// live keypress without the lossy `overlay_input_from_winit` mapper. Stays
// internal to the native module — no new public crate API surface.
pub(in crate::native) fn chord_from_winit(
    logical: &WinitKey,
    mods: Modifiers,
    super_key: bool,
) -> Option<KeyChord> {
    let key = match logical {
        WinitKey::Character(text) => {
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || !ch.is_ascii_graphic() {
                return None;
            }
            KeyBindingKey::Character(ch.to_ascii_lowercase())
        }
        WinitKey::Named(named) => KeyBindingKey::Named(binding_named_key(*named)?),
        _ => return None,
    };
    Some(KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl: mods.ctrl,
            shift: mods.shift,
            alt: mods.alt,
            super_key,
        },
        key,
    })
}

/// Build the multiplexer prefix-engine chord from a key event, resolving the
/// shifted-character ambiguity that `key_without_modifiers()` introduces.
///
/// tmux's prefix table matches the *produced* character for printable second
/// keys (`%`, `"`, `o`, …) but a modifier-qualified base key for control chords
/// (e.g. `<prefix> Ctrl+f`). winit gives two views of a key: `logical` carries
/// the shifted character (`%` for Shift+5, `"` for Shift+'), while `binding_key`
/// (`key_without_modifiers()`) carries the unshifted base key (`5`, `'`). Using
/// `binding_key` alone — the previous behaviour — meant `%`/`"` never matched
/// their stored chords (`5` != `%`), so the tmux split chords silently did
/// nothing on hardware.
///
/// Resolve by preferring the logical character when it yields a printable
/// (ascii-graphic, single-char) chord, falling back to the base key otherwise.
/// This fires `%`/`"` correctly, while a `Ctrl+<letter>` second key (whose
/// `logical` is a non-printable control char on platforms that fold Ctrl into
/// `logical_key`) and the `Ctrl-b` prefix itself both fall through to the base
/// key unchanged. When `logical` and `binding_key` agree (every unshifted key:
/// `o`, `x`, arrows) the two paths are identical, so this is a strict superset
/// of the old behaviour.
pub(in crate::native) fn prefix_chord_from_winit(
    logical: &WinitKey,
    binding_key: &WinitKey,
    mods: Modifiers,
    super_key: bool,
) -> Option<KeyChord> {
    chord_from_winit(logical, mods, super_key)
        .or_else(|| chord_from_winit(binding_key, mods, super_key))
}

/// Native settings-panel chord. Kept as a named helper for tests and callers
/// that need the UX1/UX2 default without duplicating binding-table details.
#[cfg(test)]
pub(super) fn is_overlay_shortcut(logical: &WinitKey, mods: Modifiers, super_key: bool) -> bool {
    KeyBindings::default().action_for(logical, mods, super_key)
        == Some(BindableAction::SettingsPanel)
}

#[cfg(test)]
pub(super) fn is_theme_picker_shortcut(
    logical: &WinitKey,
    mods: Modifiers,
    super_key: bool,
) -> bool {
    KeyBindings::default().action_for(logical, mods, super_key) == Some(BindableAction::ThemePicker)
}

fn binding_named_key(named: NamedKey) -> Option<KeyBindingNamedKey> {
    Some(match named {
        NamedKey::Enter => KeyBindingNamedKey::Enter,
        NamedKey::Backspace => KeyBindingNamedKey::Backspace,
        NamedKey::Escape => KeyBindingNamedKey::Escape,
        NamedKey::Tab => KeyBindingNamedKey::Tab,
        NamedKey::Space => KeyBindingNamedKey::Space,
        NamedKey::PageUp => KeyBindingNamedKey::PageUp,
        NamedKey::PageDown => KeyBindingNamedKey::PageDown,
        NamedKey::Home => KeyBindingNamedKey::Home,
        NamedKey::End => KeyBindingNamedKey::End,
        NamedKey::Delete => KeyBindingNamedKey::Delete,
        NamedKey::Insert => KeyBindingNamedKey::Insert,
        NamedKey::ArrowUp => KeyBindingNamedKey::ArrowUp,
        NamedKey::ArrowDown => KeyBindingNamedKey::ArrowDown,
        NamedKey::ArrowLeft => KeyBindingNamedKey::ArrowLeft,
        NamedKey::ArrowRight => KeyBindingNamedKey::ArrowRight,
        NamedKey::F1 => KeyBindingNamedKey::F(1),
        NamedKey::F2 => KeyBindingNamedKey::F(2),
        NamedKey::F3 => KeyBindingNamedKey::F(3),
        NamedKey::F4 => KeyBindingNamedKey::F(4),
        NamedKey::F5 => KeyBindingNamedKey::F(5),
        NamedKey::F6 => KeyBindingNamedKey::F(6),
        NamedKey::F7 => KeyBindingNamedKey::F(7),
        NamedKey::F8 => KeyBindingNamedKey::F(8),
        NamedKey::F9 => KeyBindingNamedKey::F(9),
        NamedKey::F10 => KeyBindingNamedKey::F(10),
        NamedKey::F11 => KeyBindingNamedKey::F(11),
        NamedKey::F12 => KeyBindingNamedKey::F(12),
        NamedKey::F13 => KeyBindingNamedKey::F(13),
        NamedKey::F14 => KeyBindingNamedKey::F(14),
        NamedKey::F15 => KeyBindingNamedKey::F(15),
        NamedKey::F16 => KeyBindingNamedKey::F(16),
        NamedKey::F17 => KeyBindingNamedKey::F(17),
        NamedKey::F18 => KeyBindingNamedKey::F(18),
        NamedKey::F19 => KeyBindingNamedKey::F(19),
        NamedKey::F20 => KeyBindingNamedKey::F(20),
        NamedKey::F21 => KeyBindingNamedKey::F(21),
        NamedKey::F22 => KeyBindingNamedKey::F(22),
        NamedKey::F23 => KeyBindingNamedKey::F(23),
        NamedKey::F24 => KeyBindingNamedKey::F(24),
        _ => return None,
    })
}

pub(super) fn changed_window_title(terminal: &mut Terminal, default_title: &str) -> Option<String> {
    terminal
        .take_title_changed()
        .then(|| terminal.title().unwrap_or(default_title).to_owned())
}

fn core_mouse_modifiers(mods: Modifiers) -> CoreMouseModifiers {
    CoreMouseModifiers {
        // Shift is reserved for local selection when mouse reporting is active.
        shift: false,
        alt: mods.alt,
        ctrl: mods.ctrl,
    }
}

pub(super) fn encode_native_mouse_report(
    protocol: MouseProtocol,
    point: CellPoint,
    button: CoreMouseButton,
    kind: MouseEventKind,
    mods: Modifiers,
) -> Option<Vec<u8>> {
    encode_mouse_event(
        protocol,
        button,
        kind,
        point.column + 1,
        point.row + 1,
        core_mouse_modifiers(mods),
    )
}

pub(super) fn motion_report_button(
    protocol: MouseProtocol,
    held_button: Option<CoreMouseButton>,
) -> Option<CoreMouseButton> {
    held_button.or_else(|| {
        (protocol.tracking == MouseTracking::AnyEvent).then_some(CoreMouseButton::NoButton)
    })
}

pub(super) fn encode_native_focus_report(terminal: &Terminal, focused: bool) -> Option<Vec<u8>> {
    encode_focus_event(terminal.focus_reporting(), focused)
}

pub(super) fn wheel_report_button(delta: MouseScrollDelta) -> Option<CoreMouseButton> {
    match wheel_lines(delta, 1).cmp(&0) {
        std::cmp::Ordering::Greater => Some(CoreMouseButton::WheelUp),
        std::cmp::Ordering::Less => Some(CoreMouseButton::WheelDown),
        std::cmp::Ordering::Equal => None,
    }
}

#[cfg(test)]
pub(super) fn is_copy_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::Copy)
}

#[cfg(test)]
pub(super) fn is_paste_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::Paste)
}

/// Shift+PageUp pages the scrollback viewport upward. Shift only (no Ctrl/Alt)
/// so plain PageUp still reaches the PTY.
#[cfg(test)]
pub(super) fn is_scroll_up_key(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::ScrollPageUp)
}

/// Shift+PageDown pages the scrollback viewport toward the live bottom.
#[cfg(test)]
pub(super) fn is_scroll_down_key(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::ScrollPageDown)
}

/// Translate a `winit` [`NamedKey`] into the neutral [`Key`] model.
///
/// `shift` is consulted only to turn Tab into [`Key::BackTab`] (Shift-Tab),
/// while `Space` is mapped to [`Key::Char(' ')`] rather than a named key so
/// Ctrl-Space encodes to `NUL` via the shared encoder. Named keys the prototype
/// does not handle (function keys, media keys, etc.) return `None`.
pub(super) fn map_named_key(named: NamedKey, shift: bool) -> Option<Key> {
    Some(match named {
        NamedKey::Enter => Key::Enter,
        NamedKey::Backspace => Key::Backspace,
        NamedKey::ArrowLeft => Key::Left,
        NamedKey::ArrowRight => Key::Right,
        NamedKey::ArrowUp => Key::Up,
        NamedKey::ArrowDown => Key::Down,
        NamedKey::Home => Key::Home,
        NamedKey::End => Key::End,
        NamedKey::PageUp => Key::PageUp,
        NamedKey::PageDown => Key::PageDown,
        NamedKey::Tab if shift => Key::BackTab,
        NamedKey::Tab => Key::Tab,
        NamedKey::Delete => Key::Delete,
        NamedKey::Insert => Key::Insert,
        NamedKey::Escape => Key::Esc,
        NamedKey::Space => Key::Char(' '),
        _ => return None,
    })
}

/// Translate a `winit` physical key into the neutral keypad identities that
/// logical keys cannot preserve. Returns `None` for non-keypad keys so callers
/// can fall back to logical-key mapping.
pub(super) fn map_keypad_physical_key(physical: PhysicalKey) -> Option<Key> {
    Some(match physical {
        PhysicalKey::Code(KeyCode::Numpad0) => Key::KeypadDigit(0),
        PhysicalKey::Code(KeyCode::Numpad1) => Key::KeypadDigit(1),
        PhysicalKey::Code(KeyCode::Numpad2) => Key::KeypadDigit(2),
        PhysicalKey::Code(KeyCode::Numpad3) => Key::KeypadDigit(3),
        PhysicalKey::Code(KeyCode::Numpad4) => Key::KeypadDigit(4),
        PhysicalKey::Code(KeyCode::Numpad5) => Key::KeypadDigit(5),
        PhysicalKey::Code(KeyCode::Numpad6) => Key::KeypadDigit(6),
        PhysicalKey::Code(KeyCode::Numpad7) => Key::KeypadDigit(7),
        PhysicalKey::Code(KeyCode::Numpad8) => Key::KeypadDigit(8),
        PhysicalKey::Code(KeyCode::Numpad9) => Key::KeypadDigit(9),
        PhysicalKey::Code(KeyCode::NumpadDecimal) => Key::KeypadDecimal,
        PhysicalKey::Code(KeyCode::NumpadAdd) => Key::KeypadAdd,
        PhysicalKey::Code(KeyCode::NumpadSubtract) => Key::KeypadSubtract,
        PhysicalKey::Code(KeyCode::NumpadMultiply) => Key::KeypadMultiply,
        PhysicalKey::Code(KeyCode::NumpadDivide) => Key::KeypadDivide,
        PhysicalKey::Code(KeyCode::NumpadEnter) => Key::KeypadEnter,
        _ => return None,
    })
}

pub(super) fn map_winit_mouse_button(button: WinitMouseButton) -> Option<CoreMouseButton> {
    Some(match button {
        WinitMouseButton::Left => CoreMouseButton::Left,
        WinitMouseButton::Middle => CoreMouseButton::Middle,
        WinitMouseButton::Right => CoreMouseButton::Right,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_key_bindings_have_no_duplicate_chords() {
        let bindings = default_key_bindings();
        for (i, (chord, action)) in bindings.iter().enumerate() {
            for (other_chord, other_action) in bindings.iter().skip(i + 1) {
                assert_ne!(
                    chord, other_chord,
                    "duplicate default chord {:?} for {:?} and {:?}",
                    chord, action, other_action
                );
            }
        }
    }

    #[test]
    fn direct_chord_ctrl_shift_e_splits_columns() {
        // The first split on a single-pane tab is reachable via the direct GUI
        // chord (the prefix engine is gated off at single-pane). The binding
        // table is pane-count-agnostic, so this resolves regardless of panes.
        let bindings = KeyBindings::default();
        let chord = char_chord('e', true, true, false, false);
        assert_eq!(
            bindings.action_for_chord(chord),
            Some(BindableAction::SplitColumns)
        );
    }

    #[test]
    fn direct_chord_ctrl_shift_o_splits_rows() {
        let bindings = KeyBindings::default();
        let chord = char_chord('o', true, true, false, false);
        assert_eq!(
            bindings.action_for_chord(chord),
            Some(BindableAction::SplitRows)
        );
    }

    #[test]
    fn direct_split_chords_survive_pane_action_override_skip() {
        // A pane-action override cannot bind a bare global chord (it is skipped
        // in `from_overrides`), but the *default* direct split chords must
        // survive that skip so the GUI split remains reachable.
        let overrides = vec![KeyBindingOverride {
            chord: char_chord('g', true, true, false, false),
            action: BindableAction::SplitColumns,
        }];
        let bindings = KeyBindings::from_overrides(&overrides);
        // The override was skipped; the default Ctrl+Shift+E still splits.
        assert_eq!(
            bindings.action_for_chord(char_chord('e', true, true, false, false)),
            Some(BindableAction::SplitColumns)
        );
        // The attempted bare override chord did not take.
        assert_eq!(
            bindings.action_for_chord(char_chord('g', true, true, false, false)),
            None
        );
    }

    #[test]
    fn discoverability_chords_resolve_to_their_overlays() {
        // v0.3.1: the four previously-unbound overlays gained default
        // `Ctrl+Shift+<letter>` chords (a TUI cannot receive these, so no PTY
        // input path is perturbed).
        let bindings = KeyBindings::default();
        assert_eq!(
            bindings.action_for_chord(char_chord('p', true, true, false, false)),
            Some(BindableAction::CommandPalette),
            "Ctrl+Shift+P opens the command palette (industry standard)"
        );
        assert_eq!(
            bindings.action_for_chord(char_chord('s', true, true, false, false)),
            Some(BindableAction::ConnectionManager),
            "Ctrl+Shift+S opens the connection manager"
        );
        assert_eq!(
            bindings.action_for_chord(char_chord('r', true, true, false, false)),
            Some(BindableAction::SessionReplay),
            "Ctrl+Shift+R opens session replay"
        );
        assert_eq!(
            bindings.action_for_chord(char_chord('b', true, true, false, false)),
            Some(BindableAction::ThemeBuilder),
            "Ctrl+Shift+B opens the theme builder"
        );
    }

    #[test]
    fn prompt_jump_keeps_arrow_chords_and_drops_letter_fallbacks() {
        // The arrow chords remain the primary (and now sole default) prompt-jump
        // bindings; the `Ctrl+Shift+P` / `Ctrl+Shift+N` letter fallbacks were
        // reclaimed in v0.3.1 (P → command palette; N was freed then, and is now
        // bound to New Window in F1).
        let bindings = KeyBindings::default();
        assert_eq!(
            bindings.action_for_chord(named_chord(
                KeyBindingNamedKey::ArrowUp,
                true,
                true,
                false,
                false
            )),
            Some(BindableAction::JumpPromptPrev),
            "Ctrl+Shift+Up still jumps to the previous prompt"
        );
        assert_eq!(
            bindings.action_for_chord(named_chord(
                KeyBindingNamedKey::ArrowDown,
                true,
                true,
                false,
                false
            )),
            Some(BindableAction::JumpPromptNext),
            "Ctrl+Shift+Down still jumps to the next prompt"
        );
        // The reclaimed P no longer maps to prompt-jump; N now opens a new window
        // (F1) rather than being unbound.
        assert_ne!(
            bindings.action_for_chord(char_chord('p', true, true, false, false)),
            Some(BindableAction::JumpPromptPrev),
            "Ctrl+Shift+P no longer jumps prompts (reclaimed by the palette)"
        );
        assert_eq!(
            bindings.action_for_chord(char_chord('n', true, true, false, false)),
            Some(BindableAction::NewWindow),
            "Ctrl+Shift+N opens a new window (F1)"
        );
    }

    // ----- §7 K1 prefix-engine state machine -----

    fn ctrl_b() -> KeyChord {
        char_chord('b', true, false, false, false)
    }

    fn engine_ctrl_b() -> PrefixEngine {
        PrefixEngine::new(Some(ctrl_b()), &[], PREFIX_TIMEOUT)
    }

    #[test]
    fn no_prefix_pending_is_inactive_for_every_non_prefix_chord() {
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        // Bare keys, including the very keys that are pane chords once pending,
        // are all Inactive when nothing is pending — the byte-identical path.
        for chord in [
            char_chord('a', false, false, false, false),
            char_chord('%', false, true, false, false),
            char_chord('x', false, false, false, false),
            named_chord(KeyBindingNamedKey::ArrowLeft, false, false, false, false),
        ] {
            assert_eq!(engine.on_chord(chord, now), PrefixOutcome::Inactive);
            assert!(!engine.is_pending());
        }
    }

    #[test]
    fn prefix_then_action_resolves_and_returns_to_normal() {
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        assert_eq!(engine.on_chord(ctrl_b(), now), PrefixOutcome::Entered);
        assert!(engine.is_pending());
        // `%` arrives as Shift+`%`; the table lookup normalizes shift away.
        let pct = char_chord('%', false, true, false, false);
        assert_eq!(
            engine.on_chord(pct, now),
            PrefixOutcome::Action(BindableAction::SplitColumns)
        );
        assert!(!engine.is_pending());
        // After resolving, the same `%` is plain input again.
        assert_eq!(engine.on_chord(pct, now), PrefixOutcome::Inactive);
    }

    #[test]
    fn prefix_then_shifted_punctuation_resolves_through_the_real_wiring() {
        // REGRESSION (v0.3.0): the live winit path feeds the prefix engine a
        // chord built from BOTH `logical` (the shifted character) and
        // `binding_key` (`key_without_modifiers()`), via `prefix_chord_from_winit`
        // — exactly as `handle_key_event` does. On a US layout Shift+5 yields
        // `logical = '%'` but `binding_key = '5'`, and Shift+' yields
        // `logical = '"'` but `binding_key = '''`. The earlier test injected a
        // pre-shifted `%` straight into the engine, so it passed while the real
        // wiring (which only had `5`) silently no-op'd. This drives the genuine
        // two-key construction.
        let shift = Modifiers {
            ctrl: false,
            shift: true,
            alt: false,
        };

        // `<prefix> %` → SplitColumns. binding_key is the unshifted '5'.
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        assert_eq!(engine.on_chord(ctrl_b(), now), PrefixOutcome::Entered);
        let pct = prefix_chord_from_winit(
            &WinitKey::Character("%".into()),
            &WinitKey::Character("5".into()),
            shift,
            false,
        )
        .expect("shifted punctuation builds a chord");
        assert_eq!(
            engine.on_chord(pct, now),
            PrefixOutcome::Action(BindableAction::SplitColumns),
            "Shift+5 (logical '%', base '5') must split columns"
        );

        // `<prefix> \"` → SplitRows. binding_key is the unshifted '\''.
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        assert_eq!(engine.on_chord(ctrl_b(), now), PrefixOutcome::Entered);
        let quote = prefix_chord_from_winit(
            &WinitKey::Character("\"".into()),
            &WinitKey::Character("'".into()),
            shift,
            false,
        )
        .expect("shifted quote builds a chord");
        assert_eq!(
            engine.on_chord(quote, now),
            PrefixOutcome::Action(BindableAction::SplitRows),
            "Shift+' (logical '\"', base ''') must split rows"
        );
    }

    #[test]
    fn prefix_chord_from_winit_falls_back_to_base_key_for_unshifted_keys() {
        // For an unshifted second key the two winit views agree, so the helper
        // is identical to the old `binding_key`-only path: `o`/`x`/`z` still
        // resolve. Guards against the fix perturbing the non-punctuation chords.
        let none = Modifiers {
            ctrl: false,
            shift: false,
            alt: false,
        };
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        engine.on_chord(ctrl_b(), now);
        let o = prefix_chord_from_winit(
            &WinitKey::Character("o".into()),
            &WinitKey::Character("o".into()),
            none,
            false,
        )
        .expect("letter builds a chord");
        assert_eq!(
            engine.on_chord(o, now),
            PrefixOutcome::Action(BindableAction::FocusPaneNext)
        );
    }

    #[test]
    fn prefix_then_named_action_matches_exactly() {
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        engine.on_chord(ctrl_b(), now);
        let left = named_chord(KeyBindingNamedKey::ArrowLeft, false, false, false, false);
        assert_eq!(
            engine.on_chord(left, now),
            PrefixOutcome::Action(BindableAction::FocusPaneLeft)
        );
    }

    #[test]
    fn prefix_then_unknown_key_cancels_clean() {
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        engine.on_chord(ctrl_b(), now);
        // A key not in the prefix table swallows + cancels, firing no action.
        let q = char_chord('q', false, false, false, false);
        assert_eq!(engine.on_chord(q, now), PrefixOutcome::Cancelled);
        assert!(!engine.is_pending());
        // Input is normal again immediately.
        assert_eq!(engine.on_chord(q, now), PrefixOutcome::Inactive);
    }

    #[test]
    fn prefix_then_timeout_cancels_and_reprocesses_fresh() {
        let mut engine = engine_ctrl_b();
        let start = Instant::now();
        assert_eq!(engine.on_chord(ctrl_b(), start), PrefixOutcome::Entered);
        // A pane chord arriving after the timeout does NOT fire the action; the
        // expired prefix is forgotten and the key is fresh input.
        let later = start + PREFIX_TIMEOUT + Duration::from_millis(1);
        let x = char_chord('x', false, false, false, false);
        assert_eq!(engine.on_chord(x, later), PrefixOutcome::Inactive);
        assert!(!engine.is_pending());
    }

    #[test]
    fn prefix_then_timeout_then_prefix_reenters() {
        let mut engine = engine_ctrl_b();
        let start = Instant::now();
        engine.on_chord(ctrl_b(), start);
        // The prefix itself, arriving after timeout, re-enters pending rather
        // than passing through.
        let later = start + PREFIX_TIMEOUT + Duration::from_millis(1);
        assert_eq!(engine.on_chord(ctrl_b(), later), PrefixOutcome::Entered);
        assert!(engine.is_pending());
    }

    #[test]
    fn expire_pending_clears_a_timed_out_prefix_on_the_timer() {
        let mut engine = engine_ctrl_b();
        let start = Instant::now();
        assert_eq!(engine.on_chord(ctrl_b(), start), PrefixOutcome::Entered);
        assert!(engine.is_pending());
        // Before the timeout: nothing to expire, still pending.
        let mid = start + PREFIX_TIMEOUT / 2;
        assert!(!engine.expire_pending(mid), "not yet timed out");
        assert!(engine.is_pending());
        // At the timeout boundary — the instant the loop is woken at — it clears,
        // so the recomputed wait deadline is never a stale past instant.
        let at = start + PREFIX_TIMEOUT;
        assert!(engine.expire_pending(at), "cleared at the boundary");
        assert!(!engine.is_pending());
        assert_eq!(engine.pending_deadline(), None);
        // Idempotent: a second pass with nothing pending is a no-op.
        assert!(!engine.expire_pending(at + Duration::from_secs(1)));
    }

    #[test]
    fn expire_pending_is_a_noop_when_not_pending() {
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        assert!(!engine.expire_pending(now));
        assert!(!engine.is_pending());
    }

    #[test]
    fn doubled_prefix_is_passthrough_with_the_literal_byte() {
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        engine.on_chord(ctrl_b(), now);
        assert_eq!(engine.on_chord(ctrl_b(), now), PrefixOutcome::Passthrough);
        assert!(!engine.is_pending());
        // Ctrl-b passes through as the literal 0x02.
        assert_eq!(engine.passthrough_bytes(), vec![0x02]);
    }

    #[test]
    fn passthrough_byte_tracks_a_reconfigured_prefix() {
        // Ctrl-a → 0x01.
        let engine = PrefixEngine::new(
            Some(char_chord('a', true, false, false, false)),
            &[],
            PREFIX_TIMEOUT,
        );
        assert_eq!(engine.passthrough_bytes(), vec![0x01]);
    }

    #[test]
    fn disabled_prefix_is_always_inactive() {
        let mut engine = PrefixEngine::new(None, &[], PREFIX_TIMEOUT);
        let now = Instant::now();
        assert_eq!(engine.on_chord(ctrl_b(), now), PrefixOutcome::Inactive);
        assert!(!engine.is_pending());
        assert!(engine.passthrough_bytes().is_empty());
    }

    #[test]
    fn pane_action_override_rebinds_the_prefix_table() {
        // Rebind zoom-pane from `z` to `f`.
        let overrides = vec![KeyBindingOverride {
            chord: char_chord('f', false, false, false, false),
            action: BindableAction::ZoomPane,
        }];
        let mut engine = PrefixEngine::new(Some(ctrl_b()), &overrides, PREFIX_TIMEOUT);
        let now = Instant::now();
        // `f` now zooms.
        engine.on_chord(ctrl_b(), now);
        let f = char_chord('f', false, false, false, false);
        assert_eq!(
            engine.on_chord(f, now),
            PrefixOutcome::Action(BindableAction::ZoomPane)
        );
        // The old `z` no longer resolves to an action (cancels).
        engine.on_chord(ctrl_b(), now);
        let z = char_chord('z', false, false, false, false);
        assert_eq!(engine.on_chord(z, now), PrefixOutcome::Cancelled);
    }

    #[test]
    fn equalize_has_two_default_chords() {
        let mut engine = engine_ctrl_b();
        let now = Instant::now();
        // Space equalizes.
        engine.on_chord(ctrl_b(), now);
        let space = named_chord(KeyBindingNamedKey::Space, false, false, false, false);
        assert_eq!(
            engine.on_chord(space, now),
            PrefixOutcome::Action(BindableAction::EqualizePanes)
        );
        // `=` equalizes too.
        engine.on_chord(ctrl_b(), now);
        let eq = char_chord('=', false, false, false, false);
        assert_eq!(
            engine.on_chord(eq, now),
            PrefixOutcome::Action(BindableAction::EqualizePanes)
        );
    }
}
