// SPDX-License-Identifier: GPL-3.0-only
use crate::core::{
    MouseButton as CoreMouseButton, MouseEventKind, MouseModifiers as CoreMouseModifiers,
    MouseProtocol, MouseTracking, Terminal, encode_focus_event, encode_mouse_event,
};
use crate::input::{
    Key, KeyEventType, Modifiers, WIN32_ENHANCED_KEY, WIN32_LEFT_ALT, WIN32_LEFT_CTRL,
    WIN32_RIGHT_ALT, WIN32_RIGHT_CTRL, WIN32_SHIFT, Win32KeyEvent, ctrl_char,
};
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
        // Duplicate Tab / Duplicate Workspace open a fresh shell in the active
        // pane's cwd (F1 cwd inheritance): `Ctrl+Shift+D` duplicates the tab,
        // the `Ctrl+Shift+Alt+D` Alt escalation duplicates the whole workspace
        // one level up -- the same tab->workspace escalation as the
        // `Ctrl+PageDown` (tab) vs `Ctrl+Shift+PageDown` (workspace) pair. `d`
        // was free in the letter space (verified against every other default).
        (
            char_chord('d', true, true, false, false),
            BindableAction::DuplicateTab,
        ),
        (
            char_chord('d', true, true, true, false),
            BindableAction::DuplicateWorkspace,
        ),
        (
            named_chord(KeyBindingNamedKey::PageDown, true, false, false, false),
            BindableAction::NextTab,
        ),
        (
            named_chord(KeyBindingNamedKey::PageUp, true, false, false, false),
            BindableAction::PrevTab,
        ),
        // Workspace cycling. `Ctrl+Shift+PageDown/PageUp` sit mnemonically above
        // the `Ctrl+PageDown/PageUp` tab-cycling chords and are otherwise free
        // (scroll uses Shift+Page*, tab cycling uses Ctrl+Page*).
        (
            named_chord(KeyBindingNamedKey::PageDown, true, true, false, false),
            BindableAction::NextWorkspace,
        ),
        (
            named_chord(KeyBindingNamedKey::PageUp, true, true, false, false),
            BindableAction::PrevWorkspace,
        ),
        // New Workspace and the workspace picker gained default chords once field
        // use showed the create/switch actions were undiscoverable without them.
        // `Ctrl+Shift+Enter` for new-workspace mirrors `Ctrl+Shift+T`'s new-tab
        // feel one level up; plain `Enter` and `Shift+Enter` are untouched (the
        // chord requires Ctrl+Shift, and the search overlay's `Shift+Enter` is
        // modal). `Ctrl+Shift+G` opens the picker — free in the letter space and
        // clear of GTK/ibus's `Ctrl+Shift+U` Unicode entry. Close / Rename
        // Workspace stay unbound (destructive close, Rename-Tab precedent); the
        // rail, context menu, and command palette cover them.
        (
            named_chord(KeyBindingNamedKey::Enter, true, true, false, false),
            BindableAction::NewWorkspace,
        ),
        (
            char_chord('g', true, true, false, false),
            BindableAction::WorkspacePicker,
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
/// does not handle (media keys, F13+, etc.) return `None`.
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
        // F1..F12 reach the PTY through the shared encoder. This arm sits
        // after the chord dispatch in the caller, so user-bound F-key chords
        // still win; only unbound F keys fall through to the shell. F13+ stay
        // chord-only until a PTY encoding is defined for them.
        NamedKey::F1 => Key::F(1),
        NamedKey::F2 => Key::F(2),
        NamedKey::F3 => Key::F(3),
        NamedKey::F4 => Key::F(4),
        NamedKey::F5 => Key::F(5),
        NamedKey::F6 => Key::F(6),
        NamedKey::F7 => Key::F(7),
        NamedKey::F8 => Key::F(8),
        NamedKey::F9 => Key::F(9),
        NamedKey::F10 => Key::F(10),
        NamedKey::F11 => Key::F(11),
        NamedKey::F12 => Key::F(12),
        _ => return None,
    })
}

/// Canonicalize editing keys from their physical identity before application
/// routing. Winit's logical identity is compositor-dependent for Ctrl chords:
/// some Wayland stacks report Ctrl+Backspace as Character(BS), while others
/// report Named(Backspace). The physical code is stable across those delivery
/// shapes, so bindings, modal handling, and PTY encoding all receive the same
/// named key. Numpad Enter is deliberately excluded so the keypad encoder keeps
/// its distinct application-mode identity.
pub(super) fn normalize_winit_editing_key(logical: WinitKey, physical: PhysicalKey) -> WinitKey {
    // A named logical identity is already canonical and must win. Besides
    // preserving synthetic callers that do not carry a meaningful physical
    // code, this avoids rewriting a platform event that explicitly identifies
    // a different named key. The compositor divergence being repaired is the
    // Character(...) delivery shape.
    if !matches!(logical, WinitKey::Character(_)) {
        return logical;
    }
    match physical {
        PhysicalKey::Code(KeyCode::Backspace | KeyCode::NumpadBackspace) => {
            WinitKey::Named(NamedKey::Backspace)
        }
        PhysicalKey::Code(KeyCode::Tab) => WinitKey::Named(NamedKey::Tab),
        PhysicalKey::Code(KeyCode::Enter) => WinitKey::Named(NamedKey::Enter),
        PhysicalKey::Code(KeyCode::Escape) => WinitKey::Named(NamedKey::Escape),
        PhysicalKey::Code(KeyCode::Delete) => WinitKey::Named(NamedKey::Delete),
        _ => logical,
    }
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

/// Map winit's hardware identity onto the Win32 fields ConPTY transports.
///
/// Winit intentionally exposes USB-position-like [`KeyCode`] values rather
/// than native `VK_*` values. This table reconstructs the standard Win32 VK and
/// set-1 scan identities for the keys OdyTTY accepts. The logical keys supply
/// the layout-resolved virtual key and UTF-16 code unit; the physical key
/// supplies the scan code, so non-US layouts retain both pieces of information.
pub(super) fn map_win32_key_event(
    physical: PhysicalKey,
    logical: &WinitKey,
    base_logical: &WinitKey,
    mods: Modifiers,
    event_type: KeyEventType,
) -> Option<Win32KeyEvent> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    let (physical_virtual_key, scan_code, enhanced) = win32_vk_scan(code)?;
    // Windows VK identities follow the active keyboard layout while scan codes
    // follow the hardware position. Winit exposes those halves separately:
    // key_without_modifiers is the layout-resolved base identity and KeyCode is
    // the physical key. Fall back to the physical VK for named/non-ASCII keys.
    // Keypad keys retain their distinct VK_NUMPAD*/VK_DIVIDE identity even
    // when winit exposes their logical value as the corresponding digit or
    // punctuation character. Other keys remain layout-resolved.
    let virtual_key = if is_win32_keypad_key(code) {
        physical_virtual_key
    } else {
        win32_virtual_key_from_logical(base_logical).unwrap_or(physical_virtual_key)
    };
    let mut control_key_state = 0;
    if mods.ctrl {
        control_key_state |= WIN32_LEFT_CTRL;
    }
    if mods.alt {
        control_key_state |= WIN32_LEFT_ALT;
    }
    if mods.shift {
        control_key_state |= WIN32_SHIFT;
    }
    if enhanced {
        control_key_state |= WIN32_ENHANCED_KEY;
    }

    // ModifiersChanged can trail KeyboardInput. Make the modifier's own record
    // self-consistent from its physical identity instead of depending on that
    // event ordering. Aggregate winit state cannot identify the side for other
    // held modifiers, so those deliberately use the left-hand Win32 bit.
    let key_down = event_type != KeyEventType::Release;
    match code {
        KeyCode::ControlLeft => set_control_bit(&mut control_key_state, WIN32_LEFT_CTRL, key_down),
        KeyCode::ControlRight => {
            control_key_state &= !WIN32_LEFT_CTRL;
            set_control_bit(&mut control_key_state, WIN32_RIGHT_CTRL, key_down);
        }
        KeyCode::AltLeft => set_control_bit(&mut control_key_state, WIN32_LEFT_ALT, key_down),
        KeyCode::AltRight => {
            control_key_state &= !WIN32_LEFT_ALT;
            set_control_bit(&mut control_key_state, WIN32_RIGHT_ALT, key_down);
        }
        KeyCode::ShiftLeft | KeyCode::ShiftRight => {
            set_control_bit(&mut control_key_state, WIN32_SHIFT, key_down);
        }
        _ => {}
    }

    Some(Win32KeyEvent {
        virtual_key,
        scan_code,
        unicode_char: win32_unicode_char(logical, mods),
        control_key_state,
    })
}

fn win32_virtual_key_from_logical(logical: &WinitKey) -> Option<u16> {
    let WinitKey::Character(text) = logical else {
        return None;
    };
    let ch = text.chars().next()?;
    match ch.to_ascii_lowercase() {
        'a'..='z' => u16::try_from(u32::from(ch.to_ascii_uppercase())).ok(),
        '0'..='9' => u16::try_from(u32::from(ch)).ok(),
        ';' => Some(0xba),
        '=' => Some(0xbb),
        ',' => Some(0xbc),
        '-' => Some(0xbd),
        '.' => Some(0xbe),
        '/' => Some(0xbf),
        '`' => Some(0xc0),
        '[' => Some(0xdb),
        '\\' => Some(0xdc),
        ']' => Some(0xdd),
        '\'' => Some(0xde),
        _ => None,
    }
}

fn set_control_bit(state: &mut u16, bit: u16, set: bool) {
    if set {
        *state |= bit;
    } else {
        *state &= !bit;
    }
}

fn is_win32_keypad_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Numpad0
            | KeyCode::Numpad1
            | KeyCode::Numpad2
            | KeyCode::Numpad3
            | KeyCode::Numpad4
            | KeyCode::Numpad5
            | KeyCode::Numpad6
            | KeyCode::Numpad7
            | KeyCode::Numpad8
            | KeyCode::Numpad9
            | KeyCode::NumpadDecimal
            | KeyCode::NumpadAdd
            | KeyCode::NumpadSubtract
            | KeyCode::NumpadMultiply
            | KeyCode::NumpadDivide
            | KeyCode::NumpadEnter
    )
}

fn win32_unicode_char(logical: &WinitKey, mods: Modifiers) -> u16 {
    match logical {
        WinitKey::Character(text) => {
            let Some(ch) = text.chars().next() else {
                return 0;
            };
            if mods.ctrl {
                ctrl_char(ch).map_or(0, u16::from)
            } else {
                text.encode_utf16().next().unwrap_or(0)
            }
        }
        WinitKey::Named(NamedKey::Backspace) => 0x08,
        WinitKey::Named(NamedKey::Tab) => 0x09,
        WinitKey::Named(NamedKey::Enter) => 0x0d,
        WinitKey::Named(NamedKey::Escape) => 0x1b,
        WinitKey::Named(NamedKey::Space) if mods.ctrl => 0,
        WinitKey::Named(NamedKey::Space) => 0x20,
        _ => 0,
    }
}

#[allow(clippy::too_many_lines)]
fn win32_vk_scan(code: KeyCode) -> Option<(u16, u16, bool)> {
    Some(match code {
        KeyCode::Escape => (0x1b, 0x01, false),
        KeyCode::Digit1 => (0x31, 0x02, false),
        KeyCode::Digit2 => (0x32, 0x03, false),
        KeyCode::Digit3 => (0x33, 0x04, false),
        KeyCode::Digit4 => (0x34, 0x05, false),
        KeyCode::Digit5 => (0x35, 0x06, false),
        KeyCode::Digit6 => (0x36, 0x07, false),
        KeyCode::Digit7 => (0x37, 0x08, false),
        KeyCode::Digit8 => (0x38, 0x09, false),
        KeyCode::Digit9 => (0x39, 0x0a, false),
        KeyCode::Digit0 => (0x30, 0x0b, false),
        KeyCode::Minus => (0xbd, 0x0c, false),
        KeyCode::Equal => (0xbb, 0x0d, false),
        KeyCode::Backspace => (0x08, 0x0e, false),
        KeyCode::Tab => (0x09, 0x0f, false),
        KeyCode::KeyQ => (0x51, 0x10, false),
        KeyCode::KeyW => (0x57, 0x11, false),
        KeyCode::KeyE => (0x45, 0x12, false),
        KeyCode::KeyR => (0x52, 0x13, false),
        KeyCode::KeyT => (0x54, 0x14, false),
        KeyCode::KeyY => (0x59, 0x15, false),
        KeyCode::KeyU => (0x55, 0x16, false),
        KeyCode::KeyI => (0x49, 0x17, false),
        KeyCode::KeyO => (0x4f, 0x18, false),
        KeyCode::KeyP => (0x50, 0x19, false),
        KeyCode::BracketLeft => (0xdb, 0x1a, false),
        KeyCode::BracketRight => (0xdd, 0x1b, false),
        KeyCode::Enter => (0x0d, 0x1c, false),
        KeyCode::ControlLeft => (0x11, 0x1d, false),
        KeyCode::KeyA => (0x41, 0x1e, false),
        KeyCode::KeyS => (0x53, 0x1f, false),
        KeyCode::KeyD => (0x44, 0x20, false),
        KeyCode::KeyF => (0x46, 0x21, false),
        KeyCode::KeyG => (0x47, 0x22, false),
        KeyCode::KeyH => (0x48, 0x23, false),
        KeyCode::KeyJ => (0x4a, 0x24, false),
        KeyCode::KeyK => (0x4b, 0x25, false),
        KeyCode::KeyL => (0x4c, 0x26, false),
        KeyCode::Semicolon => (0xba, 0x27, false),
        KeyCode::Quote => (0xde, 0x28, false),
        KeyCode::Backquote => (0xc0, 0x29, false),
        KeyCode::ShiftLeft => (0x10, 0x2a, false),
        KeyCode::Backslash | KeyCode::IntlBackslash => (0xdc, 0x2b, false),
        KeyCode::KeyZ => (0x5a, 0x2c, false),
        KeyCode::KeyX => (0x58, 0x2d, false),
        KeyCode::KeyC => (0x43, 0x2e, false),
        KeyCode::KeyV => (0x56, 0x2f, false),
        KeyCode::KeyB => (0x42, 0x30, false),
        KeyCode::KeyN => (0x4e, 0x31, false),
        KeyCode::KeyM => (0x4d, 0x32, false),
        KeyCode::Comma => (0xbc, 0x33, false),
        KeyCode::Period => (0xbe, 0x34, false),
        KeyCode::Slash => (0xbf, 0x35, false),
        KeyCode::ShiftRight => (0x10, 0x36, false),
        KeyCode::NumpadMultiply => (0x6a, 0x37, false),
        KeyCode::AltLeft => (0x12, 0x38, false),
        KeyCode::Space => (0x20, 0x39, false),
        KeyCode::CapsLock => (0x14, 0x3a, false),
        KeyCode::F1 => (0x70, 0x3b, false),
        KeyCode::F2 => (0x71, 0x3c, false),
        KeyCode::F3 => (0x72, 0x3d, false),
        KeyCode::F4 => (0x73, 0x3e, false),
        KeyCode::F5 => (0x74, 0x3f, false),
        KeyCode::F6 => (0x75, 0x40, false),
        KeyCode::F7 => (0x76, 0x41, false),
        KeyCode::F8 => (0x77, 0x42, false),
        KeyCode::F9 => (0x78, 0x43, false),
        KeyCode::F10 => (0x79, 0x44, false),
        KeyCode::NumLock => (0x90, 0x45, true),
        KeyCode::ScrollLock => (0x91, 0x46, false),
        KeyCode::Numpad7 => (0x67, 0x47, false),
        KeyCode::Numpad8 => (0x68, 0x48, false),
        KeyCode::Numpad9 => (0x69, 0x49, false),
        KeyCode::NumpadSubtract => (0x6d, 0x4a, false),
        KeyCode::Numpad4 => (0x64, 0x4b, false),
        KeyCode::Numpad5 => (0x65, 0x4c, false),
        KeyCode::Numpad6 => (0x66, 0x4d, false),
        KeyCode::NumpadAdd => (0x6b, 0x4e, false),
        KeyCode::Numpad1 => (0x61, 0x4f, false),
        KeyCode::Numpad2 => (0x62, 0x50, false),
        KeyCode::Numpad3 => (0x63, 0x51, false),
        KeyCode::Numpad0 => (0x60, 0x52, false),
        KeyCode::NumpadDecimal => (0x6e, 0x53, false),
        KeyCode::F11 => (0x7a, 0x57, false),
        KeyCode::F12 => (0x7b, 0x58, false),
        KeyCode::NumpadEnter => (0x0d, 0x1c, true),
        KeyCode::ControlRight => (0x11, 0x1d, true),
        KeyCode::NumpadDivide => (0x6f, 0x35, true),
        KeyCode::AltRight => (0x12, 0x38, true),
        KeyCode::Home => (0x24, 0x47, true),
        KeyCode::ArrowUp => (0x26, 0x48, true),
        KeyCode::PageUp => (0x21, 0x49, true),
        KeyCode::ArrowLeft => (0x25, 0x4b, true),
        KeyCode::ArrowRight => (0x27, 0x4d, true),
        KeyCode::End => (0x23, 0x4f, true),
        KeyCode::ArrowDown => (0x28, 0x50, true),
        KeyCode::PageDown => (0x22, 0x51, true),
        KeyCode::Insert => (0x2d, 0x52, true),
        KeyCode::Delete => (0x2e, 0x53, true),
        KeyCode::SuperLeft => (0x5b, 0x5b, true),
        KeyCode::SuperRight => (0x5c, 0x5c, true),
        KeyCode::ContextMenu => (0x5d, 0x5d, true),
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
    fn function_keys_map_to_pty_encoder_keys() {
        // F1..F12 reach the PTY through the shared encoder; F13+ stay
        // chord-only. Shift does not perturb the mapping (no BackTab-style
        // rewrite for function keys).
        assert_eq!(map_named_key(NamedKey::F1, false), Some(Key::F(1)));
        assert_eq!(map_named_key(NamedKey::F5, false), Some(Key::F(5)));
        assert_eq!(map_named_key(NamedKey::F12, true), Some(Key::F(12)));
        assert_eq!(map_named_key(NamedKey::F13, false), None);
        assert_eq!(map_named_key(NamedKey::F24, false), None);
    }

    #[test]
    fn plain_function_keys_have_no_default_chords() {
        // No default binding may shadow a plain F key, or htop/mc-style TUIs
        // would lose it; user overrides can still claim them deliberately.
        let bindings = KeyBindings::default();
        for number in 1..=12 {
            assert_eq!(
                bindings.action_for_chord(named_chord(
                    KeyBindingNamedKey::F(number),
                    false,
                    false,
                    false,
                    false
                )),
                None,
                "plain F{number} must reach the PTY"
            );
        }
    }

    #[test]
    fn duplicate_tab_and_workspace_have_default_chords() {
        // Duplicate Tab and Duplicate Workspace gained default chords so both are
        // discoverable (the menu now shows an accelerator). `Ctrl+Shift+D`
        // duplicates the tab; the `Ctrl+Shift+Alt+D` Alt escalation duplicates
        // the whole workspace one level up -- the same tab->workspace escalation
        // shape as `Ctrl+PageDown` vs `Ctrl+Shift+PageDown`. `d` was free.
        let bindings = KeyBindings::default();
        assert_eq!(
            bindings.action_for_chord(char_chord('d', true, true, false, false)),
            Some(BindableAction::DuplicateTab),
            "Ctrl+Shift+D duplicates the tab"
        );
        assert_eq!(
            bindings.action_for_chord(char_chord('d', true, true, true, false)),
            Some(BindableAction::DuplicateWorkspace),
            "Ctrl+Shift+Alt+D duplicates the workspace"
        );
        // The two chords are distinct (Alt is the only differing modifier), so
        // neither shadows the other and `no_duplicate_chords` stays green.
        assert_ne!(
            char_chord('d', true, true, false, false),
            char_chord('d', true, true, true, false),
        );
        // Plain `Ctrl+D` (EOF) is untouched -- the chords require Shift.
        assert_eq!(
            bindings.action_for_chord(char_chord('d', true, false, false, false)),
            None,
            "plain Ctrl+D still reaches the shell as EOF"
        );
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
            chord: char_chord('z', true, true, false, false),
            action: BindableAction::SplitColumns,
        }];
        let bindings = KeyBindings::from_overrides(&overrides);
        // The override was skipped; the default Ctrl+Shift+E still splits.
        assert_eq!(
            bindings.action_for_chord(char_chord('e', true, true, false, false)),
            Some(BindableAction::SplitColumns)
        );
        // The attempted bare override chord did not take. `Ctrl+Shift+Z` is an
        // otherwise-unbound sentinel (the picker took `Ctrl+Shift+G`).
        assert_eq!(
            bindings.action_for_chord(char_chord('z', true, true, false, false)),
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
    fn workspace_create_and_picker_have_default_chords() {
        // Field use showed New Workspace and the picker were undiscoverable
        // without a chord. `Ctrl+Shift+Enter` creates a workspace; `Ctrl+Shift+G`
        // opens the picker. Close / Rename Workspace stay unbound.
        let bindings = KeyBindings::default();
        assert_eq!(
            bindings.action_for_chord(named_chord(
                KeyBindingNamedKey::Enter,
                true,
                true,
                false,
                false
            )),
            Some(BindableAction::NewWorkspace),
            "Ctrl+Shift+Enter creates a new workspace"
        );
        assert_eq!(
            bindings.action_for_chord(char_chord('g', true, true, false, false)),
            Some(BindableAction::WorkspacePicker),
            "Ctrl+Shift+G opens the workspace picker"
        );
        // Plain Enter and Shift+Enter are untouched — they never resolve to a
        // workspace action, so the shell and the modal search overlay keep them.
        assert_eq!(
            bindings.action_for_chord(named_chord(
                KeyBindingNamedKey::Enter,
                false,
                false,
                false,
                false
            )),
            None,
            "plain Enter reaches the shell"
        );
        assert_eq!(
            bindings.action_for_chord(named_chord(
                KeyBindingNamedKey::Enter,
                false,
                true,
                false,
                false
            )),
            None,
            "Shift+Enter stays with the search overlay"
        );
        // Close / Rename Workspace remain unbound by default.
        assert_eq!(
            bindings.chord_for_action(BindableAction::CloseWorkspace),
            None
        );
        assert_eq!(
            bindings.chord_for_action(BindableAction::RenameWorkspace),
            None
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
