//! Pure-action vocabulary emitted by the OdyTTY parser control state machine.
//!
//! The control state machine's `step(byte) -> Action` (in [`super::machine`])
//! returns one of these values per byte. The **driver** (in [`super::driver`])
//! is the only place an action becomes a [`super::VtDispatch`] call. Splitting
//! along the action boundary keeps the state machine sink-agnostic — ideal for
//! component-level tests, fuzzers, and golden fixtures — and confines the I/O
//! contract to one module.
//!
//! ## Originality note
//!
//! This is OdyTTY's chosen action vocabulary, designed from the dispatch
//! callbacks the canonical DEC ANSI parser surfaces (`print`, `execute`,
//! `csi_dispatch`, `esc_dispatch`, OSC start/put/end, DCS hook/put/unhook, APC
//! string). The vocabulary differs from the inline-callback designs that wire
//! the sink directly into the state machine: we keep the machine sink-agnostic
//! and the adapter thin. The compound variants ([`Action::DcsUnhookExecute`],
//! [`Action::OscEndExecute`]) encode the canonical "two-effect" transitions —
//! DCS/OSC terminated by a control byte that still itself executes — as one
//! action so `step` returns exactly one [`Action`] per byte.
//!
//! Layer 1 (the [`super::segmenter`]) does not emit actions: Ground-state
//! printable text and C1-via-UTF-8 executes go directly to the driver's
//! [`super::VtDispatch::print`] / [`super::VtDispatch::execute`] adapter
//! without round-tripping through this enum.

/// One outcome of processing a control byte in the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// No observable effect — internal bookkeeping (param digit collected,
    /// intermediate stashed, ignore flag set) or a deliberately ignored byte.
    None,
    /// Execute a control byte (C0 typically; in Ground via the segmenter for C1
    /// scalars decoded from UTF-8).
    Execute(u8),
    /// A CSI final byte arrived; the driver invokes
    /// [`super::VtDispatch::csi_dispatch`] with the machine's accumulated
    /// params + intermediates + ignore flag and this byte as the action char.
    CsiDispatch(u8),
    /// A non-CSI ESC final byte arrived; the driver invokes
    /// [`super::VtDispatch::esc_dispatch`].
    EscDispatch(u8),
    /// A DCS final byte arrived; the driver invokes [`super::VtDispatch::hook`]
    /// and prepares for passthrough.
    DcsHook(u8),
    /// A DCS passthrough payload byte → [`super::VtDispatch::put`].
    DcsPut(u8),
    /// DCS terminated cleanly → [`super::VtDispatch::unhook`].
    DcsUnhook,
    /// DCS terminated by a cancel byte (CAN/SUB) inside passthrough: unhook,
    /// then execute the cancel byte itself.
    DcsUnhookExecute(u8),
    /// OSC payload byte (driver appends to its OSC buffer).
    OscPut(u8),
    /// OSC `;` separator: driver snapshots the current buffer position as the
    /// end of the current OSC parameter and opens the next.
    OscParamBoundary,
    /// OSC terminated; driver dispatches the accumulated params via
    /// [`super::VtDispatch::osc_dispatch`]. `bell` distinguishes BEL (`0x07`)
    /// from ST (`ESC \` or `0x9C`).
    OscEnd { bell: bool },
    /// OSC terminated by a cancel byte: dispatch then execute the cancel byte.
    /// `bell` is always `false` here (CAN/SUB are not BEL).
    OscEndExecute { bell: bool, byte: u8 },
    /// APC payload byte (driver appends to its APC buffer, dropping past cap).
    ApcPut(u8),
    /// APC terminated; driver flushes the buffer via
    /// [`super::VtDispatch::apc_dispatch`] unless it overflowed the cap.
    ApcEnd,
}
