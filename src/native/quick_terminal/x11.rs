// SPDX-License-Identifier: GPL-3.0-only
//! X11 global-shortcut backend (Linux) for the quick terminal (v0.15.0 A).
//!
//! Split out of `quick_terminal` so the X11 client code lives in one platform
//! module. Compiled only on Linux; the whole module is behind the
//! `#[cfg(target_os = "linux")]` on its `mod` declaration. It performs a real
//! root-window `GrabKey` over a dedicated `x11rb` connection and confirms the
//! grab with the server (a CHECKED request) before reporting `Registered`, and
//! reports the honest Wayland/headless limitation otherwise, so it never claims
//! a registration the OS did not confirm.
//!
//! `x11rb` is used deliberately in place of a libX11 (Xlib) backend: its
//! checked requests surface a grab error on this connection alone, so there is
//! no process-global `XSetErrorHandler` racing winit's own X connection and no
//! `XInitThreads` first-call ordering hazard. The grab connection is owned
//! exclusively by this module (the confirming call, then a single poll thread).

use super::wayland::{PortalFailure, WaylandGrab, try_register_portal};
use super::{
    Accelerator, GlobalShortcutAdapter, LinuxDisplayServer, ShortcutRegistration, SummonSink,
    detect_linux_display_server,
};

/// The Linux global-shortcut adapter. It answers honestly per display server:
/// Wayland binds through the `GlobalShortcuts` D-Bus portal (the compositor, not
/// the app, owns global grabs there) and reports `Registered` only once the
/// portal confirms the bind, or an actionable `Unsupported`/`Unavailable`
/// otherwise; X11 performs a real `GrabKey` on the root window and confirms it
/// with the server before returning `Registered`; an Unknown display server or a
/// missing/headless connection returns Unavailable. It never claims a
/// registration the OS/portal did not confirm.
#[cfg(target_os = "linux")]
pub(in crate::native) struct LinuxShortcutAdapter {
    server: LinuxDisplayServer,
    grab: Option<x11_grab::X11Grab>,
    wayland_grab: Option<WaylandGrab>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for LinuxShortcutAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxShortcutAdapter")
            .field("server", &self.server)
            .field("grabbed", &self.grab.is_some())
            .field("wayland_bound", &self.wayland_grab.is_some())
            .finish()
    }
}

#[cfg(target_os = "linux")]
impl LinuxShortcutAdapter {
    /// Detect the display server from the process environment.
    pub(in crate::native) fn from_env() -> Self {
        let wayland = std::env::var("WAYLAND_DISPLAY").ok();
        let x11 = std::env::var("DISPLAY").ok();
        let session = std::env::var("XDG_SESSION_TYPE").ok();
        Self {
            server: detect_linux_display_server(
                wayland.as_deref(),
                x11.as_deref(),
                session.as_deref(),
            ),
            grab: None,
            wayland_grab: None,
        }
    }
}

#[cfg(target_os = "linux")]
impl GlobalShortcutAdapter for LinuxShortcutAdapter {
    fn register(&mut self, accelerator: &Accelerator, sink: SummonSink) -> ShortcutRegistration {
        // Drop any prior grab before re-registering.
        self.grab = None;
        self.wayland_grab = None;
        match self.server {
            LinuxDisplayServer::Wayland => match try_register_portal(accelerator, sink) {
                Ok(grab) => {
                    self.wayland_grab = Some(grab);
                    ShortcutRegistration::Registered {
                        backend: "wayland-globalshortcuts-portal",
                    }
                }
                // No usable portal environment is an honest, actionable
                // limitation; a present-but-refusing portal is a runtime
                // failure. Never Registered without the portal confirming.
                Err(PortalFailure::Unsupported(reason)) => ShortcutRegistration::Unsupported {
                    platform: "linux-wayland",
                    reason,
                },
                Err(PortalFailure::Unavailable(reason)) => {
                    ShortcutRegistration::Unavailable { reason }
                }
            },
            LinuxDisplayServer::X11 | LinuxDisplayServer::Unknown => {
                match x11_grab::try_register(accelerator, sink) {
                    Ok(grab) => {
                        self.grab = Some(grab);
                        ShortcutRegistration::Registered {
                            backend: "x11-grabkey",
                        }
                    }
                    // A headless/unreachable X connection or an unmapped key is
                    // Unavailable; the server refusing the grab (already held)
                    // is a runtime failure, also Unavailable. Never Registered
                    // without the server confirming the grab.
                    Err(reason) => ShortcutRegistration::Unavailable { reason },
                }
            }
        }
    }

    fn unregister(&mut self) {
        // Dropping the grab ungrabs the key / closes the portal session and
        // joins the owning thread.
        self.grab = None;
        self.wayland_grab = None;
    }
}

/// The real X11 global-shortcut backend, built on `x11rb`'s pure-Rust
/// `RustConnection`. A background thread owns a dedicated X connection, receives
/// the grabbed `KeyPress` events, and invokes the summon sink; the grab is
/// confirmed synchronously with a CHECKED `grab_key` request before
/// [`try_register`] returns success, so a shortcut another client already holds
/// is reported as a failure rather than a false `Registered`. Because the error
/// arrives on this connection, there is no process-global error handler and no
/// `XInitThreads` hazard shared with winit's X connection.
#[cfg(target_os = "linux")]
mod x11_grab {
    use super::super::{
        Accelerator, SummonSink, x11_grab_failure_notice, x11_keysym,
        x11_registration_failure_notice,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;
    use x11rb::connection::Connection;
    use x11rb::errors::ReplyError;
    use x11rb::protocol::ErrorKind;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, Keycode, ModMask};
    use x11rb::rust_connection::RustConnection;

    /// A live grab: a stop flag and the poll thread that owns the X connection.
    pub(super) struct X11Grab {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl Drop for X11Grab {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// The X11 modifier mask for an accelerator's modifiers. Super maps to
    /// `Mod4` (`ModMask::M4`), the near-universal convention.
    fn modifier_mask(acc: &Accelerator) -> ModMask {
        let mut mask = ModMask::from(0u8);
        if acc.shift {
            mask |= ModMask::SHIFT;
        }
        if acc.ctrl {
            mask |= ModMask::CONTROL;
        }
        if acc.alt {
            mask |= ModMask::M1;
        }
        if acc.meta {
            mask |= ModMask::M4;
        }
        mask
    }

    /// The lock-key mask variants a grab must cover so CapsLock (`LOCK`) or
    /// NumLock (`M2`) being on does not defeat the shortcut. Built at runtime
    /// because `ModMask`'s `BitOr` is not a `const fn`.
    fn lock_variants() -> [ModMask; 4] {
        [
            ModMask::from(0u8),
            ModMask::LOCK,
            ModMask::M2,
            ModMask::LOCK | ModMask::M2,
        ]
    }

    /// Resolve a keysym to a keycode over the connection's keyboard mapping.
    /// Returns `Ok(None)` when the current layout binds the keysym to no
    /// keycode; `Err` only on a transport/protocol failure.
    fn keycode_for_keysym(conn: &RustConnection, target: u32) -> Result<Option<Keycode>, String> {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        let count = max.saturating_sub(min).saturating_add(1);
        let reply = conn
            .get_keyboard_mapping(min, count)
            .map_err(|e| format!("X11 keyboard-mapping request failed: {e}"))?
            .reply()
            .map_err(|e| format!("X11 keyboard-mapping reply failed: {e}"))?;
        let per = usize::from(reply.keysyms_per_keycode);
        if per == 0 {
            return Ok(None);
        }
        for (row, chunk) in reply.keysyms.chunks(per).enumerate() {
            if chunk.contains(&target)
                && let Ok(keycode) = Keycode::try_from(usize::from(min) + row)
            {
                return Ok(Some(keycode));
            }
        }
        Ok(None)
    }

    /// Attempt a real root-window key grab. On success spawns the poll thread
    /// and returns the live [`X11Grab`]; on any failure returns an actionable
    /// message and leaves nothing grabbed.
    pub(super) fn try_register(acc: &Accelerator, sink: SummonSink) -> Result<X11Grab, String> {
        let keysym = u32::try_from(x11_keysym(&acc.key).ok_or_else(|| {
            tracing::warn!(key = %acc.key, "quick terminal key has no X11 keysym");
            x11_registration_failure_notice(acc)
        })?)
        .map_err(|error| {
            tracing::warn!(key = %acc.key, %error, "quick terminal X11 keysym is out of range");
            x11_registration_failure_notice(acc)
        })?;

        // Headless or no reachable X server (e.g. CI): honest Unavailable.
        let (conn, screen_num) = x11rb::connect(None).map_err(|error| {
            tracing::warn!(%error, "cannot open X11 display for quick terminal shortcut");
            x11_registration_failure_notice(acc)
        })?;
        let root = conn.setup().roots[screen_num].root;
        let keycode = keycode_for_keysym(&conn, keysym)
            .map_err(|error| {
                tracing::warn!(%error, "cannot read X11 keyboard mapping for quick terminal shortcut");
                x11_registration_failure_notice(acc)
            })?
            .ok_or_else(|| {
                tracing::warn!(key = %acc.key, "quick terminal key has no X11 keycode");
                x11_registration_failure_notice(acc)
            })?;
        let base = modifier_mask(acc);
        let variants = lock_variants();

        // Grab each lock-variant with a CHECKED request so a refusal (BadAccess:
        // another client already holds the combo) is reported on THIS connection
        // rather than through a process-global handler. Roll back any grabs that
        // did land if a later variant is refused.
        let mut granted: Vec<ModMask> = Vec::with_capacity(variants.len());
        let mut failure: Option<String> = None;
        for variant in variants {
            let mods = base | variant;
            match conn.grab_key(true, root, mods, keycode, GrabMode::ASYNC, GrabMode::ASYNC) {
                Ok(cookie) => match cookie.check() {
                    Ok(()) => granted.push(mods),
                    Err(error) => {
                        let conflict = matches!(
                            &error,
                            ReplyError::X11Error(error) if error.error_kind == ErrorKind::Access
                        );
                        tracing::warn!(%error, conflict, "X11 rejected quick terminal key grab");
                        failure = Some(x11_grab_failure_notice(acc, conflict));
                        break;
                    }
                },
                Err(error) => {
                    tracing::warn!(%error, "X11 quick terminal grab request failed");
                    failure = Some(x11_registration_failure_notice(acc));
                    break;
                }
            }
        }
        if let Some(reason) = failure {
            for mods in granted {
                let _ = conn.ungrab_key(keycode, root, mods);
            }
            let _ = conn.flush();
            return Err(reason);
        }

        // Confirmed. Hand the connection to a poll thread that delivers the
        // summon and, on stop, ungrabs and drops the connection (closing it).
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("odytty-x11-hotkey".to_owned())
            .spawn(move || {
                while !stop_thread.load(Ordering::SeqCst) {
                    match conn.poll_for_event() {
                        Ok(Some(Event::KeyPress(_))) => sink(),
                        Ok(Some(_)) => {}
                        // No event queued: sleep briefly so a stop flips
                        // promptly without busy-spinning the connection.
                        Ok(None) => std::thread::sleep(Duration::from_millis(30)),
                        // The connection dropped (server gone): stop cleanly.
                        Err(_) => break,
                    }
                }
                for mods in granted {
                    let _ = conn.ungrab_key(keycode, root, mods);
                }
                let _ = conn.flush();
                // `conn` drops here, closing the socket.
            })
            .map_err(|error| {
                tracing::warn!(%error, "cannot spawn X11 quick terminal hotkey thread");
                x11_registration_failure_notice(acc)
            })?;

        Ok(X11Grab {
            stop,
            handle: Some(handle),
        })
    }
}
