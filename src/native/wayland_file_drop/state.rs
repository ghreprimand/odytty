// SPDX-License-Identifier: GPL-3.0-only
//! Deterministic, wl-object-free core of the native Wayland file-drop listener.
//!
//! The Dispatch glue in `mod.rs` owns the `wl_data_offer` / `wl_data_device`
//! proxies and performs the protocol side effects (accept, set_actions, receive,
//! finish, destroy). This module owns the DECISIONS and BOOKKEEPING keyed by
//! plain protocol object ids (`u32`), so the offer lifecycle, per-seat drag
//! ownership, the Copy-only gate, bounded growth, and the transfer timeout are
//! all testable without a live compositor.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Hard cap on tracked offers across all seats. A hostile or buggy source that
/// creates offers without ever dropping cannot grow the table without bound; the
/// oldest offer is evicted (and its proxy destroyed by the caller) on overflow.
pub(super) const MAX_OFFERS: usize = 64;
/// Cap on the `text/uri-list` payload read from a source.
pub(super) const MAX_URI_BYTES: usize = 64 * 1024;
/// Upper bound on one transfer once the receive pipe is opened.
pub(super) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3);

/// The DnD action, abstracted from `wl_data_device_manager::DndAction` so the
/// gate is testable without the wayland-client type. `mod.rs` maps the wire
/// value into this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DropAction {
    Copy,
    Move,
    Ask,
    /// Any other single action or an unrecognized bit pattern.
    Other,
}

/// Stable identity of one surface incarnation, captured at Enter and validated
/// at delivery. `generation` is process-monotonic and never reused, so a reused
/// `wl_surface` address (ABA) gets a fresh generation and cannot misroute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceIdent {
    /// `ProcessWindowId` raw value (process-unique, never reused).
    pub(crate) window: u64,
    /// Incarnation counter for that window's current surface.
    pub(crate) generation: u64,
}

/// Per-offer negotiation record. `supports_uri` replaces a growable MIME list:
/// only whether `text/uri-list` was offered matters, so a source cannot grow
/// per-offer memory by advertising many MIME types.
#[derive(Debug, Default, Clone)]
pub(super) struct OfferRecord {
    pub(super) supports_uri: bool,
    pub(super) negotiated: Option<DropAction>,
    pub(super) source_has_copy: bool,
    pub(super) preference_sent: bool,
    pub(super) action_after_preference: bool,
}

/// Per-seat (per `wl_data_device`) drag state. `current_enter_offer` is tracked
/// SEPARATELY from `accepted`: Leave (and a superseding Enter) must destroy the
/// enter-time offer whether or not it was accepted, so a declined Enter never
/// leaks its offer.
#[derive(Debug, Default, Clone)]
struct SeatDrag {
    current_enter_offer: Option<u32>,
    accepted: Option<AcceptedDrag>,
}

/// A drag we accepted (offer advertised `text/uri-list`), pending Drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AcceptedDrag {
    pub(super) offer: u32,
    pub(super) surface_ptr: u64,
    pub(super) ident: Option<SurfaceIdent>,
}

/// Outcome of an Enter for one seat. The caller destroys `stale_offer` (a prior
/// enter-time offer that was never disposed), then performs the protocol action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EnterOutcome {
    /// A prior enter-time offer for this seat that must be destroyed now.
    pub(super) stale_offer: Option<u32>,
    /// `true` when the entered offer advertised `text/uri-list` and was
    /// accepted; `false` means decline (accept null) with no drag retained.
    pub(super) accept: bool,
}

/// Outcome of a Drop for one seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DropOutcome {
    /// No drag was pending (spurious Drop): nothing to do.
    Idle,
    /// Refuse: destroy `offer` without finishing. `was_uri` distinguishes a real
    /// file drop we engaged (worth an actionable notice) from an unrelated one.
    Refuse { offer: u32, was_uri: bool },
    /// Receive + finish: the Copy action was confirmed after our preference.
    Receive { drag: AcceptedDrag },
}

/// The wl-object-free listener core.
#[derive(Debug, Default)]
pub(super) struct DropCore {
    offers: HashMap<u32, OfferRecord>,
    /// Insertion order for bounded eviction.
    order: Vec<u32>,
    seats: HashMap<u32, SeatDrag>,
}

impl DropCore {
    /// Register a freshly created offer. Returns an offer id to destroy when the
    /// table is at capacity (the oldest tracked offer is evicted), else `None`.
    pub(super) fn register_offer(&mut self, offer: u32) -> Option<u32> {
        let mut evict = None;
        if self.offers.len() >= MAX_OFFERS
            && let Some(oldest) = self.order.first().copied()
        {
            self.remove_offer(oldest);
            evict = Some(oldest);
        }
        self.offers.entry(offer).or_default();
        if !self.order.contains(&offer) {
            self.order.push(offer);
        }
        evict
    }

    pub(super) fn set_supports_uri(&mut self, offer: u32) {
        if let Some(record) = self.offers.get_mut(&offer) {
            record.supports_uri = true;
        }
    }

    pub(super) fn set_source_has_copy(&mut self, offer: u32, has_copy: bool) {
        if let Some(record) = self.offers.get_mut(&offer) {
            record.source_has_copy = has_copy;
        }
    }

    pub(super) fn note_action(&mut self, offer: u32, action: Option<DropAction>) {
        if let Some(record) = self.offers.get_mut(&offer) {
            record.negotiated = action;
            if record.preference_sent {
                record.action_after_preference = true;
            }
        }
    }

    pub(super) fn mark_preference_sent(&mut self, offer: u32) {
        if let Some(record) = self.offers.get_mut(&offer) {
            record.preference_sent = true;
        }
    }

    pub(super) fn supports_uri(&self, offer: u32) -> bool {
        self.offers.get(&offer).is_some_and(|r| r.supports_uri)
    }

    /// Adopt an Enter on `seat` for `offer`. Records the enter-time offer,
    /// returns any prior enter-time offer to destroy, and whether to accept.
    pub(super) fn enter(
        &mut self,
        seat: u32,
        offer: u32,
        surface_ptr: u64,
        ident: Option<SurfaceIdent>,
    ) -> EnterOutcome {
        let accept = self.supports_uri(offer);
        let drag = self.seats.entry(seat).or_default();
        let stale_offer = drag
            .current_enter_offer
            .take()
            .filter(|prev| *prev != offer);
        drag.current_enter_offer = Some(offer);
        drag.accepted = accept.then_some(AcceptedDrag {
            offer,
            surface_ptr,
            ident,
        });
        EnterOutcome {
            stale_offer,
            accept,
        }
    }

    /// Leave on `seat`: the enter-time offer must ALWAYS be destroyed (declined
    /// or accepted). Returns the offer id to destroy, if any.
    pub(super) fn leave(&mut self, seat: u32) -> Option<u32> {
        let drag = self.seats.entry(seat).or_default();
        drag.accepted = None;
        drag.current_enter_offer.take()
    }

    /// Drop on `seat`: classify against the Copy-only gate.
    pub(super) fn drop(&mut self, seat: u32) -> DropOutcome {
        let (accepted, enter_offer) = match self.seats.get_mut(&seat) {
            Some(drag) => (drag.accepted.take(), drag.current_enter_offer.take()),
            None => return DropOutcome::Idle,
        };
        let Some(accepted) = accepted else {
            // Declined or no drag: destroy the enter-time offer if present.
            return match enter_offer {
                Some(offer) => DropOutcome::Refuse {
                    offer,
                    was_uri: self.supports_uri(offer),
                },
                None => DropOutcome::Idle,
            };
        };
        let confirmed = self
            .offers
            .get(&accepted.offer)
            .map(|r| copy_drop_confirmed(r.negotiated, r.action_after_preference))
            .unwrap_or(false);
        if confirmed {
            DropOutcome::Receive { drag: accepted }
        } else {
            DropOutcome::Refuse {
                offer: accepted.offer,
                was_uri: true,
            }
        }
    }

    /// Remove an offer's record (called after destroy/finish).
    pub(super) fn remove_offer(&mut self, offer: u32) {
        self.offers.remove(&offer);
        self.order.retain(|id| *id != offer);
        for drag in self.seats.values_mut() {
            if drag.current_enter_offer == Some(offer) {
                drag.current_enter_offer = None;
            }
            if drag.accepted.map(|a| a.offer) == Some(offer) {
                drag.accepted = None;
            }
        }
    }

    /// Drop all per-seat drag state for a removed seat (wl_registry global
    /// remove), returning enter-time offers to destroy.
    pub(super) fn remove_seat(&mut self, seat: u32) -> Vec<u32> {
        let mut to_destroy = Vec::new();
        if let Some(mut drag) = self.seats.remove(&seat)
            && let Some(offer) = drag.current_enter_offer.take()
        {
            to_destroy.push(offer);
        }
        to_destroy
    }

    /// The current enter-time offer for a seat, if any. Used by the Motion
    /// handler to re-assert the Copy preference on the live drag.
    pub(super) fn current_enter_offer(&self, seat: u32) -> Option<u32> {
        self.seats.get(&seat).and_then(|d| d.current_enter_offer)
    }

    #[cfg(test)]
    pub(super) fn offer_count(&self) -> usize {
        self.offers.len()
    }
}

/// Copy-only drop gate. A drop is received and finished ONLY when the compositor
/// confirmed the Copy action AFTER our `set_actions(Copy)` preference. Move,
/// Ask, Other, None, and a same-batch Enter+Drop (no post-preference action)
/// are refused: in every refused case the offer is destroyed WITHOUT calling
/// `finish`, and no bytes are received. This describes only what the listener
/// does; it makes no claim about how any particular compositor's data source
/// accounts for an offer that is destroyed without a `finish` (some signal the
/// source on offer destruction), which is why the known-broken compositor gate
/// exists separately.
pub(super) fn copy_drop_confirmed(
    negotiated: Option<DropAction>,
    action_after_preference: bool,
) -> bool {
    negotiated == Some(DropAction::Copy) && action_after_preference
}

/// Whether adding `incoming` bytes to a `current` buffer would exceed the cap.
pub(super) fn would_exceed_uri_cap(current: usize, incoming: usize) -> bool {
    current.saturating_add(incoming) > MAX_URI_BYTES
}

/// Whether a transfer opened at `started` has passed its deadline by `now`.
pub(super) fn transfer_expired(started: Instant, now: Instant) -> bool {
    now.duration_since(started) >= TRANSFER_TIMEOUT
}

/// Parse a `text/uri-list` (RFC 2483) payload into local file paths, in order.
///
/// Lines are CRLF- or LF-separated; empty and `#` comment lines are skipped.
/// A line yields a path ONLY when it is a `file:` URI with an empty or
/// `localhost` authority and an absolute path. Anything else is rejected and
/// produces no path -- an invalid or foreign URI is never silently reinterpreted
/// as some other path. Rejected: non-`file:` schemes, a non-local authority, a
/// URI carrying a `?` query or `#` fragment (ambiguous), a malformed percent
/// escape (a `%` not followed by two hex digits), and a decoded path containing
/// a NUL byte. Valid percent escapes are decoded at the byte level so non-UTF-8
/// Unix paths survive intact.
pub(super) fn parse_uri_list(bytes: &[u8]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for raw in bytes.split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.is_empty() || line.first() == Some(&b'#') {
            continue;
        }
        if let Some(path) = file_uri_to_path(line) {
            paths.push(path);
        }
    }
    paths
}

fn file_uri_to_path(uri: &[u8]) -> Option<PathBuf> {
    let rest = strip_ascii_ci_prefix(uri, b"file://")?;
    // A file: URI carrying a query or fragment is ambiguous for a filesystem
    // path; reject rather than reinterpret.
    if rest.iter().any(|&b| b == b'?' || b == b'#') {
        return None;
    }
    let slash = rest.iter().position(|&b| b == b'/')?;
    let authority = &rest[..slash];
    if !(authority.is_empty() || authority.eq_ignore_ascii_case(b"localhost")) {
        return None;
    }
    let decoded = percent_decode(&rest[slash..])?;
    if decoded.is_empty() || decoded.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&decoded)))
}

/// Strip an ASCII-case-insensitive `prefix`, returning the rest.
fn strip_ascii_ci_prefix<'a>(input: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if input.len() >= prefix.len() && input[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&input[prefix.len()..])
    } else {
        None
    }
}

/// Decode `%XX` escapes at the byte level. Returns `None` on a malformed escape
/// (a `%` not followed by two hex digits) so the whole URI is rejected rather
/// than silently reinterpreted.
fn percent_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            let hi = input.get(i + 1).copied().and_then(hex_val)?;
            let lo = input.get(i + 2).copied().and_then(hex_val)?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
