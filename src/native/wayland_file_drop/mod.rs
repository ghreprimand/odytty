// SPDX-License-Identifier: GPL-3.0-only
//! Native Wayland external file drop (v0.15.0 C, Linux only).
//!
//! winit 0.30's Wayland backend emits no `WindowEvent::DroppedFile`: it never
//! creates a `wl_data_device`, so a drag from a file manager is invisible to the
//! X11-style event path. This module closes that gap WITHOUT forking winit by
//! opening a second, NON-owning `wl_data_device` on a foreign connection that
//! shares winit's live `wl_display` (winit keeps ownership of the display, the
//! surfaces, and the main event queue). The listener runs on its own thread with
//! its own event queue and never touches winit's queue.
//!
//! Decisions and bookkeeping live in [`state`] (wl-object-free, deterministically
//! tested); this file is the protocol glue that holds the proxies and performs
//! the side effects, plus the wait loop and lifecycle.
//!
//! Safety boundary (shared with the v0.13.0 paste policy and the
//! foreground-group authority in [`crate::native::app::file_drop`]):
//! - Only `text/uri-list` is accepted, and only the Copy action. A drop is
//!   received and `finish`ed ONLY when the compositor confirms Copy AFTER our
//!   preference was sent; Move / Ask / None / an unconfirmed Copy are refused.
//! - URI bytes are bounded, the receive pipe is non-blocking and
//!   deadline-bounded, offers are bounded, and the parser rejects non-`file:`
//!   URIs, non-local authorities, query/fragment, malformed escapes, and NUL.
//! - Parsed paths flow through [`crate::native::app::App::queue_file_drop`]
//!   exactly like a winit drop, so shell-aware quoting, the confirm-first
//!   preview, and the foreground-group insertion authority are reused. Enter is
//!   never appended.
//! - Each drop carries the surface incarnation ([`state::SurfaceIdent`]) captured
//!   at Enter; the host validates it at delivery, so a reused surface address
//!   (ABA) or a torn-down surface cannot misroute or insert.
//!
//! Compositor support is NOT universal. Delivery relies on destination-side
//! action negotiation (Wayland data-device v3). A compositor that ignores
//! `wl_data_offer.set_actions` leaves the action at the source default, so a
//! Copy drop cannot be confirmed. The host does not activate this listener on
//! Hyprland (see `MultiWindowHost::service_wayland_file_drop`), whose data-device
//! also signals completion on offer destruction; that combination has no
//! demonstrated-safe policy and is a tracked limitation, not a universal claim.
//!
//! X11, macOS, and Windows keep their existing winit file-drop event paths.

#![cfg(target_os = "linux")]

mod state;

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wayland_client::backend::ObjectId;
use wayland_client::protocol::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_registry, wl_seat, wl_surface,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, event_created_child,
};
use winit::event_loop::EventLoopProxy;

use super::pty::UserEvent;
use state::{DropAction, DropCore, DropOutcome, SurfaceIdent};

pub(crate) use state::SurfaceIdent as WaylandSurfaceIdent;

/// The only MIME accepted.
const URI_MIME: &str = "text/uri-list";
/// Bounded, shutdown-aware wait for the required globals during init.
const INIT_DEADLINE: Duration = Duration::from_secs(2);
/// Hard cap on tracked seats. A conforming session has one or two; the cap
/// bounds a hostile or buggy compositor that advertises seats without limit,
/// and (since a data device and its transfer are one-per-seat) transitively
/// bounds devices and in-flight transfers too.
const MAX_SEATS: usize = 16;

/// A surface pointer paired with its current incarnation identity. The host
/// maintains a list of these for its live Wayland windows; the listener reads it
/// at Enter to bind a drop to a window incarnation the host can validate at
/// delivery.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SurfaceEntry {
    pub(crate) ptr: u64,
    pub(crate) ident: SurfaceIdent,
}

/// Shared, host-maintained surface incarnation table. Cloned into the listener.
pub(crate) type SurfaceRegistry = Arc<Mutex<Vec<SurfaceEntry>>>;

/// Owns the listener thread and the pipe used to wake and stop it. Dropping the
/// handle wakes the thread and joins it, which MUST happen while winit's
/// `wl_display` is still alive.
pub(crate) struct WaylandDropListener {
    shutdown_write: Option<OwnedFd>,
    thread: Option<JoinHandle<()>>,
}

impl WaylandDropListener {
    /// Start the listener against winit's live `wl_display` pointer.
    ///
    /// # Safety
    ///
    /// `display_ptr` must be a currently live `wl_display` owned by winit, and it
    /// must OUTLIVE this listener. The caller must drop the returned handle
    /// (which wakes and joins the listener thread) BEFORE winit releases the
    /// display. The host upholds this by owning the handle and being dropped
    /// before the event loop that owns the display (see the module docs and
    /// `MultiWindowHost`), and additionally stops it in `exiting`. Passing a
    /// stale or non-Wayland pointer is undefined behavior.
    pub(crate) unsafe fn start(
        display_ptr: u64,
        registry: SurfaceRegistry,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Option<Self> {
        let (shutdown_read, shutdown_write) = match pipe_nonblocking() {
            Ok(pipe) => pipe,
            Err(err) => {
                tracing::warn!(%err, "wayland file-drop listener: shutdown pipe unavailable");
                return None;
            }
        };
        let thread = thread::Builder::new()
            .name("odytty-wayland-drop".to_owned())
            // SAFETY: forwarded to `listener_main`; the caller's contract that
            // the display outlives this listener is upheld by the host.
            .spawn(move || unsafe { listener_main(display_ptr, registry, proxy, shutdown_read) })
            .map_err(|err| {
                tracing::warn!(%err, "wayland file-drop listener: thread spawn failed");
            })
            .ok()?;
        Some(Self {
            shutdown_write: Some(shutdown_write),
            thread: Some(thread),
        })
    }
}

impl Drop for WaylandDropListener {
    fn drop(&mut self) {
        // Closing the sole writer wakes both listener poll loops with POLLHUP.
        // Close before joining; no write or retry is needed, including when the
        // listener already exited and closed its read end.
        drop(self.shutdown_write.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// An in-flight receive on one seat's confirmed Copy drop.
struct Transfer {
    offer: wl_data_offer::WlDataOffer,
    offer_id: u32,
    read: OwnedFd,
    bytes: Vec<u8>,
    started: Instant,
    window: u64,
    generation: u64,
}

struct Listener {
    core: DropCore,
    offer_proxies: HashMap<u32, wl_data_offer::WlDataOffer>,
    manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    /// Seats keyed by their registry global name, so a `global_remove` can drop
    /// the exact seat and its device.
    seats: HashMap<u32, wl_seat::WlSeat>,
    /// Data devices keyed by the seat's global name (one per seat).
    devices: HashMap<u32, wl_data_device::WlDataDevice>,
    /// Seats that already have a device, keyed by seat object id.
    devices_for: HashSet<ObjectId>,
    /// In-flight receives keyed by data-device object id (per seat).
    transfers: HashMap<u32, Transfer>,
    surface_registry: SurfaceRegistry,
    proxy: EventLoopProxy<UserEvent>,
}

impl Listener {
    fn ready(&self) -> bool {
        self.manager.is_some() && !self.devices.is_empty()
    }

    fn destroy_offer(&mut self, offer_id: u32) {
        // Cancel any in-flight transfer bound to this offer FIRST, so an
        // evicted, seat-removed, or superseded offer can never later emit a
        // stale completion. This releases the pipe but does NOT itself destroy
        // the proxy (done just below), so it is reentrancy-free.
        self.cancel_transfer_for_offer(offer_id);
        if let Some(offer) = self.offer_proxies.remove(&offer_id)
            && offer.is_alive()
        {
            offer.destroy();
        }
        self.core.remove_offer(offer_id);
    }

    /// Drop an in-flight transfer whose offer is `offer_id`, if any. Releases the
    /// receive pipe and the offer-proxy clone the transfer held; it does NOT
    /// destroy the offer proxy in `offer_proxies` (the caller does). Returns
    /// whether a transfer was found. Silent: eviction/seat-removal is structural
    /// teardown, not a user-visible drop failure.
    fn cancel_transfer_for_offer(&mut self, offer_id: u32) {
        let device = self
            .transfers
            .iter()
            .find(|(_, t)| t.offer_id == offer_id)
            .map(|(id, _)| *id);
        if let Some(device_id) = device {
            self.transfers.remove(&device_id);
        }
    }

    /// Cancel an in-flight transfer on `device_id` (its seat was removed, or a
    /// second drop superseded a still-receiving one): drop the pipe and destroy
    /// the offer, so no stale completion is emitted. When `notify`, report the
    /// observable failure so the host raises an actionable notice.
    fn cancel_transfer_for_device(&mut self, device_id: u32, notify: bool) {
        if let Some(transfer) = self.transfers.remove(&device_id) {
            let offer_id = transfer.offer_id;
            drop(transfer);
            self.destroy_offer(offer_id);
            if notify {
                let _ = self.proxy.send_event(UserEvent::WaylandFileDropRejected);
            }
        }
    }

    fn create_missing_devices(&mut self, qh: &QueueHandle<Self>) {
        let Some(manager) = self.manager.as_ref() else {
            return;
        };
        let names: Vec<u32> = self.seats.keys().copied().collect();
        for name in names {
            let Some(seat) = self.seats.get(&name) else {
                continue;
            };
            if self.devices_for.insert(seat.id()) {
                let device = manager.get_data_device(seat, qh, ());
                self.devices.insert(name, device);
            }
        }
    }

    fn ident_for_surface(&self, ptr: u64) -> Option<SurfaceIdent> {
        let guard = self
            .surface_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .find(|entry| entry.ptr == ptr)
            .map(|entry| entry.ident)
    }

    /// Refuse a drop: destroy the offer without finishing, and (for a real
    /// `text/uri-list` drag we engaged) report the observable failure so the
    /// host can raise an actionable notice.
    fn refuse(&mut self, offer_id: u32, was_uri: bool) {
        self.destroy_offer(offer_id);
        if was_uri {
            let _ = self.proxy.send_event(UserEvent::WaylandFileDropRejected);
        }
    }
}

/// Map a wire `DndAction` to the testable-core action. Requires an EXACT single
/// action: a compositor that (incorrectly) leaves several bits set, e.g.
/// `Copy | Move`, is classified as [`DropAction::Other`] and REFUSED, never
/// treated as Copy. Only an unambiguous single `Copy` clears the drop gate.
fn map_action(action: wl_data_device_manager::DndAction) -> Option<DropAction> {
    use wl_data_device_manager::DndAction as A;
    if action == A::Copy {
        Some(DropAction::Copy)
    } else if action == A::Move {
        Some(DropAction::Move)
    } else if action == A::Ask {
        Some(DropAction::Ask)
    } else if action.is_empty() {
        None
    } else {
        // Any other single bit or any combination of bits (Copy|Move, ...).
        Some(DropAction::Other)
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Listener {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                match interface.as_str() {
                    // v3 is required for action negotiation (set_actions/action).
                    "wl_data_device_manager" if version >= 3 => {
                        state.manager = Some(registry.bind(name, 3, qh, ()));
                    }
                    "wl_seat" if state.seats.len() < MAX_SEATS => {
                        let seat = registry.bind(name, version.min(7), qh, ());
                        state.seats.insert(name, seat);
                    }
                    _ => {}
                }
                state.create_missing_devices(qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(seat) = state.seats.remove(&name) {
                    state.devices_for.remove(&seat.id());
                    if seat.version() >= 5 {
                        seat.release();
                    }
                    if let Some(device) = state.devices.remove(&name) {
                        let device_id = device.id().protocol_id();
                        // Cancel any in-flight transfer on this device (which
                        // destroys its offer), then destroy any enter-time offer,
                        // so the removed seat leaves neither transfer nor offer
                        // behind and can emit no stale completion.
                        state.cancel_transfer_for_device(device_id, false);
                        for offer in state.core.remove_seat(device_id) {
                            state.destroy_offer(offer);
                        }
                        if device.version() >= 2 {
                            device.release();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(Listener: ignore wl_data_device_manager::WlDataDeviceManager);
delegate_noop!(Listener: ignore wl_seat::WlSeat);
delegate_noop!(Listener: ignore wl_surface::WlSurface);

impl Dispatch<wl_data_offer::WlDataOffer, ()> for Listener {
    fn event(
        state: &mut Self,
        offer: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let offer_id = offer.id().protocol_id();
        match event {
            wl_data_offer::Event::Offer { mime_type } => {
                if mime_type == URI_MIME {
                    state.core.set_supports_uri(offer_id);
                }
            }
            wl_data_offer::Event::SourceActions { source_actions } => {
                let has_copy = source_actions
                    .into_result()
                    .map(|a| a.contains(wl_data_device_manager::DndAction::Copy))
                    .unwrap_or(false);
                state.core.set_source_has_copy(offer_id, has_copy);
            }
            wl_data_offer::Event::Action { dnd_action } => {
                let action = dnd_action.into_result().ok().and_then(map_action);
                state.core.note_action(offer_id, action);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for Listener {
    event_created_child!(Listener, wl_data_device::WlDataDevice, [
        0 => (wl_data_offer::WlDataOffer, ())
    ]);

    fn event(
        state: &mut Self,
        device: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        conn: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let device_id = device.id().protocol_id();
        match event {
            wl_data_device::Event::DataOffer { id } => {
                let offer_id = id.id().protocol_id();
                if let Some(evicted) = state.core.register_offer(offer_id) {
                    state.destroy_offer(evicted);
                }
                state.offer_proxies.insert(offer_id, id);
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                id,
                ..
            } => {
                let Some(offer) = id else {
                    return;
                };
                let offer_id = offer.id().protocol_id();
                let ptr = surface.id().as_ptr() as u64;
                let ident = state.ident_for_surface(ptr);
                let outcome = state.core.enter(device_id, offer_id, ptr, ident);
                if let Some(stale) = outcome.stale_offer {
                    state.destroy_offer(stale);
                }
                if outcome.accept {
                    offer.accept(serial, Some(URI_MIME.to_owned()));
                    offer.set_actions(
                        wl_data_device_manager::DndAction::Copy,
                        wl_data_device_manager::DndAction::Copy,
                    );
                    state.core.mark_preference_sent(offer_id);
                    // Flush so the compositor can round-trip an updated action
                    // BEFORE Drop is dispatched.
                    if let Err(err) = conn.flush() {
                        tracing::debug!(%err, "wayland file-drop: enter flush failed");
                    }
                } else {
                    offer.accept(serial, None);
                }
            }
            wl_data_device::Event::Motion { .. } => {
                // Re-assert the Copy preference so a compositor that renegotiates
                // on modifier changes re-emits an action, and flush it.
                if let Some(offer_id) = state.core.current_enter_offer(device_id)
                    && state.core.supports_uri(offer_id)
                    && let Some(offer) = state.offer_proxies.get(&offer_id)
                {
                    offer.set_actions(
                        wl_data_device_manager::DndAction::Copy,
                        wl_data_device_manager::DndAction::Copy,
                    );
                    state.core.mark_preference_sent(offer_id);
                    if let Err(err) = conn.flush() {
                        tracing::debug!(%err, "wayland file-drop: motion flush failed");
                    }
                }
            }
            wl_data_device::Event::Leave => {
                if let Some(offer_id) = state.core.leave(device_id) {
                    state.destroy_offer(offer_id);
                }
            }
            wl_data_device::Event::Drop => match state.core.drop(device_id) {
                DropOutcome::Idle => {}
                DropOutcome::Refuse { offer, was_uri } => state.refuse(offer, was_uri),
                DropOutcome::Receive { drag } => {
                    let Some(ident) = drag.ident else {
                        // No routable surface incarnation: refuse rather than
                        // receive an undeliverable drop.
                        state.refuse(drag.offer, true);
                        return;
                    };
                    let Some(offer) = state.offer_proxies.get(&drag.offer).cloned() else {
                        state.core.remove_offer(drag.offer);
                        return;
                    };
                    let (read, write) = match pipe_nonblocking() {
                        Ok(pipe) => pipe,
                        Err(err) => {
                            tracing::debug!(%err, "wayland file-drop: receive pipe failed");
                            state.refuse(drag.offer, true);
                            return;
                        }
                    };
                    offer.receive(URI_MIME.to_owned(), write.as_fd());
                    drop(write);
                    if let Err(err) = conn.flush() {
                        tracing::debug!(%err, "wayland file-drop: receive flush failed");
                    }
                    // A prior drop on this same seat may still be receiving;
                    // supersede it (cancel + destroy its offer) so the insert
                    // below never silently discards a live transfer and leaks or
                    // double-completes it.
                    state.cancel_transfer_for_device(device_id, false);
                    state.transfers.insert(
                        device_id,
                        Transfer {
                            offer,
                            offer_id: drag.offer,
                            read,
                            bytes: Vec::new(),
                            started: Instant::now(),
                            window: ident.window,
                            generation: ident.generation,
                        },
                    );
                }
            },
            wl_data_device::Event::Selection { id: Some(offer) } => {
                // Clipboard selection offers are not drops: prune immediately.
                let offer_id = offer.id().protocol_id();
                // The offer proxy usually arrived via data_offer already; ensure
                // it is owned so destroy releases it.
                state.offer_proxies.entry(offer_id).or_insert(offer);
                state.destroy_offer(offer_id);
            }
            _ => {}
        }
    }
}

/// # Safety
///
/// See [`WaylandDropListener::start`]: `display_ptr` must be winit's live
/// `wl_display` and must outlive this call (the caller joins the thread before
/// releasing the display).
unsafe fn listener_main(
    display_ptr: u64,
    surface_registry: SurfaceRegistry,
    proxy: EventLoopProxy<UserEvent>,
    shutdown_read: OwnedFd,
) {
    // SAFETY: forwarded contract - winit owns this live wl_display and the host
    // joins this thread before releasing it.
    let backend = unsafe {
        wayland_client::backend::Backend::from_foreign_display(display_ptr as usize as *mut _)
    };
    let conn = Connection::from_backend(backend);
    let mut event_queue = conn.new_event_queue::<Listener>();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = Listener {
        core: DropCore::default(),
        offer_proxies: HashMap::new(),
        manager: None,
        seats: HashMap::new(),
        devices: HashMap::new(),
        devices_for: HashSet::new(),
        transfers: HashMap::new(),
        surface_registry,
        proxy,
    };

    if !init_globals(&conn, &mut event_queue, &mut state, &shutdown_read) {
        teardown(&conn, &mut state);
        return;
    }
    tracing::debug!(
        devices = state.devices.len(),
        "wayland file-drop listener ready"
    );

    run_poll_loop(&conn, &mut event_queue, &mut state, &shutdown_read);
    teardown(&conn, &mut state);
}

/// Bounded, shutdown-aware wait for `wl_data_device_manager` v3 plus at least
/// one seat/device. Returns `true` when ready, `false` on shutdown, connection
/// loss, or the [`INIT_DEADLINE`] elapsing. Never blocks uncancellably.
fn init_globals(
    conn: &Connection,
    event_queue: &mut wayland_client::EventQueue<Listener>,
    state: &mut Listener,
    shutdown_read: &OwnedFd,
) -> bool {
    let deadline = Instant::now() + INIT_DEADLINE;
    loop {
        if event_queue.dispatch_pending(state).is_err() {
            return false;
        }
        if state.ready() {
            return true;
        }
        if conn.flush().is_err() {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            tracing::info!("wayland file-drop listener: required globals not present");
            // The data-device v3 manager (or any seat) never appeared: the
            // feature cannot run in this session. Report it as unavailable so
            // the host can raise an actionable notice rather than failing
            // silently.
            let _ = state
                .proxy
                .send_event(UserEvent::WaylandFileDropUnavailable);
            return false;
        }
        let Some(guard) = event_queue.prepare_read() else {
            continue;
        };
        let timeout = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut fds = [
            pollfd(conn.as_fd().as_raw_fd(), libc::POLLIN),
            pollfd(shutdown_read.as_raw_fd(), libc::POLLIN),
        ];
        // SAFETY: fds points to two initialized pollfd values for the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            drop(guard);
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if fds[1].revents != 0 {
            drop(guard);
            return false;
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            drop(guard);
            return false;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            if guard.read().is_err() {
                return false;
            }
        } else {
            drop(guard);
        }
    }
}

fn run_poll_loop(
    conn: &Connection,
    event_queue: &mut wayland_client::EventQueue<Listener>,
    state: &mut Listener,
    shutdown_read: &OwnedFd,
) {
    loop {
        if let Err(err) = event_queue.dispatch_pending(state) {
            tracing::warn!(%err, "wayland file-drop listener: dispatch failed");
            break;
        }
        if let Err(err) = conn.flush() {
            tracing::warn!(%err, "wayland file-drop listener: flush failed");
            break;
        }

        let Some(guard) = event_queue.prepare_read() else {
            continue;
        };

        // Build the poll set: connection, shutdown, then one fd per transfer.
        let transfer_ids: Vec<u32> = state.transfers.keys().copied().collect();
        let mut fds = Vec::with_capacity(2 + transfer_ids.len());
        fds.push(pollfd(conn.as_fd().as_raw_fd(), libc::POLLIN));
        fds.push(pollfd(shutdown_read.as_raw_fd(), libc::POLLIN));
        for id in &transfer_ids {
            let raw = state.transfers[id].read.as_raw_fd();
            fds.push(pollfd(raw, libc::POLLIN | libc::POLLHUP | libc::POLLERR));
        }
        let timeout = next_transfer_timeout(state);

        // SAFETY: fds points to `fds.len()` initialized pollfd values.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            drop(guard);
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            tracing::warn!(%err, "wayland file-drop listener: poll failed");
            break;
        }
        if fds[1].revents != 0 {
            drop(guard);
            break;
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            // The shared display connection is gone; stop rather than spin.
            drop(guard);
            tracing::debug!("wayland file-drop listener: connection hangup");
            break;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            if let Err(err) = guard.read() {
                tracing::warn!(%err, "wayland file-drop listener: read failed");
                break;
            }
        } else {
            drop(guard);
        }
        // Process cancellation before draining pipes, including events queued by
        // winit's reader on the shared display while this poll was waiting.
        if let Err(err) = event_queue.dispatch_pending(state) {
            tracing::warn!(%err, "wayland file-drop listener: post-read dispatch failed");
            break;
        }

        for (offset, device_id) in transfer_ids.iter().enumerate() {
            if fds[2 + offset].revents != 0 {
                drain_transfer(state, *device_id);
            }
        }
        expire_transfers(state);
    }
}

fn next_transfer_timeout(state: &Listener) -> i32 {
    let now = Instant::now();
    state
        .transfers
        .values()
        .map(|t| {
            (t.started + state::TRANSFER_TIMEOUT)
                .saturating_duration_since(now)
                .as_millis()
                .min(i32::MAX as u128) as i32
        })
        .min()
        .unwrap_or(-1)
}

fn expire_transfers(state: &mut Listener) {
    let now = Instant::now();
    let expired: Vec<u32> = state
        .transfers
        .iter()
        .filter(|(_, t)| state::transfer_expired(t.started, now))
        .map(|(id, _)| *id)
        .collect();
    for id in expired {
        if let Some(transfer) = state.transfers.remove(&id) {
            // Deadline reached on an engaged drop: report it.
            abort_transfer(state, transfer, true);
        }
    }
}

/// Drain a ready transfer fd. On EOF, finish the offer, parse the payload, and
/// emit the routed drop. On oversize / error, abort without finishing.
fn drain_transfer(state: &mut Listener, device_id: u32) {
    let Some(mut transfer) = state.transfers.remove(&device_id) else {
        return;
    };
    let mut buf = [0_u8; 4096];
    loop {
        // SAFETY: the owned fd is live and buf is valid writable storage.
        let count = unsafe {
            libc::read(
                transfer.read.as_raw_fd(),
                buf.as_mut_ptr().cast::<c_void>(),
                buf.len(),
            )
        };
        if count > 0 {
            let count = count as usize;
            if state::would_exceed_uri_cap(transfer.bytes.len(), count) {
                abort_transfer(state, transfer, true);
                return;
            }
            transfer.bytes.extend_from_slice(&buf[..count]);
            continue;
        }
        if count == 0 {
            complete_transfer(state, transfer);
            return;
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            // Not done: put it back and wait for more.
            state.transfers.insert(device_id, transfer);
            return;
        }
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        abort_transfer(state, transfer, true);
        return;
    }
}

fn complete_transfer(state: &mut Listener, transfer: Transfer) {
    if transfer.offer.is_alive() {
        transfer.offer.finish();
    }
    state.destroy_offer(transfer.offer_id);
    let paths = state::parse_uri_list(&transfer.bytes);
    if paths.is_empty() {
        return;
    }
    let _ = state.proxy.send_event(UserEvent::WaylandFileDrop {
        window: transfer.window,
        generation: transfer.generation,
        paths,
    });
}

/// Abort an in-flight transfer WITHOUT finishing: destroy its offer and release
/// the receive pipe. `notify` reports the observable failure so the host raises
/// an actionable notice. This path is reached only AFTER copy negotiation
/// succeeded (a transfer exists), so a failure here is a transfer failure
/// (deadline, size cap, or pipe error) - it emits `WaylandFileDropFailed`, NOT
/// `WaylandFileDropRejected`, so the notice never misblames copy negotiation.
/// Pass `false` for structural teardown (shutdown) that is not a user-visible
/// drop failure.
fn abort_transfer(state: &mut Listener, transfer: Transfer, notify: bool) {
    let offer_id = transfer.offer_id;
    drop(transfer); // release the receive pipe + offer-proxy clone
    state.destroy_offer(offer_id);
    if notify {
        let _ = state.proxy.send_event(UserEvent::WaylandFileDropFailed);
    }
}

fn teardown(conn: &Connection, state: &mut Listener) {
    let device_ids: Vec<u32> = state.transfers.keys().copied().collect();
    for id in device_ids {
        if let Some(transfer) = state.transfers.remove(&id) {
            abort_transfer(state, transfer, false);
        }
    }
    for (_, offer) in state.offer_proxies.drain() {
        if offer.is_alive() {
            offer.destroy();
        }
    }
    for (_, device) in state.devices.drain() {
        if device.version() >= 2 {
            device.release();
        }
    }
    for (_, seat) in state.seats.drain() {
        if seat.version() >= 5 {
            seat.release();
        }
    }
    let _ = conn.flush();
}

fn pollfd(fd: i32, events: i16) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

fn pipe_nonblocking() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: fds has room for the two descriptors written by pipe2.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pipe2 returned two newly owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(test)]
mod map_action_tests {
    use super::map_action;
    use super::state::DropAction;
    use wayland_client::protocol::wl_data_device_manager::DndAction as A;

    #[test]
    fn exact_single_actions_map() {
        assert_eq!(map_action(A::Copy), Some(DropAction::Copy));
        assert_eq!(map_action(A::Move), Some(DropAction::Move));
        assert_eq!(map_action(A::Ask), Some(DropAction::Ask));
        assert_eq!(map_action(A::empty()), None);
    }

    #[test]
    fn combined_actions_are_other_not_copy() {
        // A compositor that leaves several bits set must NOT be read as Copy;
        // an ambiguous combination is refused via DropAction::Other.
        assert_eq!(map_action(A::Copy | A::Move), Some(DropAction::Other));
        assert_eq!(map_action(A::Copy | A::Ask), Some(DropAction::Other));
        assert_eq!(
            map_action(A::Copy | A::Move | A::Ask),
            Some(DropAction::Other)
        );
        assert_eq!(map_action(A::Move | A::Ask), Some(DropAction::Other));
    }
}
