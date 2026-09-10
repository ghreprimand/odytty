// SPDX-License-Identifier: GPL-3.0-only
//! Wayland global-shortcut protocol seam (Linux) for the quick terminal
//! (v0.15.0 A).
//!
//! Wayland deliberately denies applications a direct global key grab: the
//! compositor owns the keyboard, so `XGrabKey` has no equivalent. The supported
//! route is the `org.freedesktop.portal.GlobalShortcuts` D-Bus portal
//! (xdg-desktop-portal): the application asks the portal to bind a shortcut and
//! the portal delivers an `Activated` signal when the user triggers it.
//!
//! The pure helpers here model the portal's wire contract with no I/O: the
//! bus/interface names, the request `handle_token` generation, the predicted
//! `Request` object path, the `Accelerator` -> portal `preferred_trigger`
//! mapping, and the `Response` code interpretation. Every one is deterministic
//! and unit-tested on the Linux dev host.
//!
//! The live transport ([`try_register_portal`]) is a `zbus` client driven on a
//! dedicated thread through `async_io::block_on`. It is factored behind the
//! [`PortalTransport`] seam so the ordering, cancellation, and cleanup logic is
//! exercised by an injected mock in unit tests, not only against a live portal.
//! The orchestration guarantees, each covered by a mock test:
//!
//! - Every potentially blocking step (session-bus connect, host-application
//!   `Registry.Register`, proxy creation, signal subscription,
//!   `CreateSession`/`BindShortcuts` calls, their `Response` waits, and the
//!   teardown `Close`) is raced against BOTH a
//!   per-step timeout AND the stop flag, so a wedged portal cannot outlive a
//!   disable/reconfigure.
//! - A `CreateSession` that succeeds is closed (bounded, best-effort) on ANY
//!   later setup failure: bind refusal, timeout, cancellation, a call error, or
//!   a bind that confirms no matching shortcut.
//! - `Registered` is reported only after the portal's `BindShortcuts` results
//!   actually list `odytty_quick_terminal_summon`; an empty, unrelated, or
//!   malformed result is `Unavailable`, never a false success.
//! - `Activated` is accepted only when it carries BOTH this session's handle
//!   AND the summon shortcut id, so a stale or concurrent session cannot summon
//!   the current window.
//! - Host-side teardown ([`WaylandGrab::drop`]) is non-blocking: it sets the
//!   stop flag and detaches. The worker performs the bounded `Close` itself, so
//!   the event loop is never joined against portal I/O while it holds the
//!   `quick_live` mutex.
//!
//! No async runtime ever touches the main event loop, and all of this runs off
//! the first-usable-terminal path (the host defers registration until after the
//! first presented frame).
//!
//! Truthful states: a host with no session bus, or a portal that does not
//! implement GlobalShortcuts, degrades to an actionable `Unsupported` (see
//! [`portal_unavailable_reason`]) that names the route and the exact trigger to
//! bind; a portal that is present but refuses, cancels, or times out is
//! `Unavailable` with the specific reason. It never reports `Registered` without
//! the portal confirming the requested bind.
//!
//! Security: the only bus opened is the invoking user's own session bus. There
//! is no network listener and no cross-user endpoint.
//!
//! Cross-platform: this whole module compiles only under
//! `#[cfg(target_os = "linux")]`. It has no Windows or macOS surface.

use super::Accelerator;

/// Well-known bus name of the desktop portal service.
pub(super) const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";

/// Object path of the desktop portal service.
pub(super) const PORTAL_OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// The GlobalShortcuts portal interface (`CreateSession`, `BindShortcuts`,
/// `ListShortcuts`; `Activated` / `Deactivated` / `ShortcutsChanged` signals).
/// Named in the actionable Wayland message, so it is a live consumer.
pub(super) const GLOBAL_SHORTCUTS_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// Host-application registry used by unsandboxed portal clients. Since
/// xdg-desktop-portal 1.18, a host application must register its application id
/// on a connection before using another portal interface on that connection.
pub(super) const REGISTRY_INTERFACE: &str = "org.freedesktop.host.portal.Registry";

/// Packaged Linux desktop identity, shared with the Wayland `app_id`, desktop
/// filename, icon, and `StartupWMClass`.
pub(super) const PORTAL_APP_ID: &str = "io.unfinished_works.odytty";

/// The portal `Request` interface. Every asynchronous portal call replies once
/// on this interface's `Response` signal at a request object path the client
/// predicts up front (see [`request_object_path`]).
pub(super) const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// The portal `Session` interface, used to `Close` a created session on
/// teardown.
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";

/// Root of predicted request object paths, per the portal spec.
const REQUEST_PATH_PREFIX: &str = "/org/freedesktop/portal/desktop/request";

/// The stable shortcut id OdyTTY binds for the quick-terminal summon. The portal
/// keys activations by this id, so it must be stable across sessions.
pub(super) const QUICK_SUMMON_SHORTCUT_ID: &str = "odytty_quick_terminal_summon";

/// Interpretation of a portal `Response` signal's leading `response` code. The
/// portal uses these three codes for every request/response exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PortalResponse {
    /// The request succeeded (code 0).
    Success,
    /// The user cancelled or dismissed the request (code 1).
    Cancelled,
    /// The request ended some other way, e.g. the portal closed it (code 2 or
    /// any other value, treated as a non-success terminal outcome).
    Other(u32),
}

impl PortalResponse {
    /// Interpret a raw portal response code. 0 is success, 1 is user
    /// cancellation, everything else is a non-success terminal outcome carried
    /// verbatim so a caller can log the exact code.
    pub(super) fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Success,
            1 => Self::Cancelled,
            other => Self::Other(other),
        }
    }

    /// Whether the exchange succeeded. Only `Success` may lead to a
    /// `Registered` outcome; every other value is a non-registration.
    pub(super) fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

/// Generate a fresh, D-Bus-valid `handle_token` for one portal request. The
/// token must be a non-empty run of ASCII `[A-Za-z0-9_]`; the portal combines it
/// with the client's unique name to form the request object path. Uniqueness
/// comes from a process-lifetime monotonic counter, so two requests in the same
/// process never collide.
pub(super) fn handle_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("odytty_{n}")
}

/// Sanitize a D-Bus unique connection name (e.g. `:1.42`) into the form the
/// portal uses inside a request object path: the leading `:` is dropped and
/// every `.` becomes `_`. A name without a leading `:` is sanitized the same
/// way (only the `.` -> `_` mapping applies), which keeps the function total.
pub(super) fn sanitize_sender(unique_name: &str) -> String {
    unique_name
        .strip_prefix(':')
        .unwrap_or(unique_name)
        .replace('.', "_")
}

/// Predict the `Request` object path the portal will use to deliver the
/// `Response` for a call made from `unique_name` with `token`. The client must
/// subscribe to `Response` on this exact path BEFORE issuing the method call to
/// avoid a lost-reply race, which is why the path is computed, not discovered.
pub(super) fn request_object_path(unique_name: &str, token: &str) -> String {
    format!(
        "{REQUEST_PATH_PREFIX}/{}/{token}",
        sanitize_sender(unique_name)
    )
}

/// Map an [`Accelerator`] to the portal `preferred_trigger` hint string. The
/// portal treats this as an advisory preference (the compositor and user make
/// the final binding), formatted as uppercase modifier tokens and the key
/// joined by `+`, e.g. `CTRL+SHIFT+F12`. Modifier order is fixed
/// (CTRL, ALT, SHIFT, SUPER) so the output is deterministic. Used on the live
/// Wayland message path.
pub(super) fn accelerator_to_trigger(acc: &Accelerator) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(5);
    if acc.ctrl {
        parts.push("CTRL");
    }
    if acc.alt {
        parts.push("ALT");
    }
    if acc.shift {
        parts.push("SHIFT");
    }
    if acc.meta {
        parts.push("SUPER");
    }
    let key = acc.key.to_ascii_uppercase();
    parts.push(&key);
    parts.join("+")
}

/// A single shortcut binding entry for `BindShortcuts`: the stable id, a
/// human-readable description shown in the portal UI, and the preferred trigger
/// hint. This is the pure payload the transport serializes into the portal's
/// `a(sa{sv})` shortcut array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ShortcutBinding {
    pub(super) id: String,
    pub(super) description: String,
    pub(super) preferred_trigger: String,
}

impl ShortcutBinding {
    /// Build the quick-terminal summon binding for `accelerator`.
    pub(super) fn quick_summon(accelerator: &Accelerator) -> Self {
        Self {
            id: QUICK_SUMMON_SHORTCUT_ID.to_owned(),
            description: "Summon the OdyTTY quick terminal".to_owned(),
            preferred_trigger: accelerator_to_trigger(accelerator),
        }
    }
}

/// The actionable message shown under Wayland when the live GlobalShortcuts
/// portal route is not usable. It names the exact supported route and the
/// precise trigger the user should bind for `accelerator`, so the limitation is
/// explicit and directly actionable, never a silent failure. ASCII-only.
pub(super) fn portal_unavailable_reason(accelerator: &Accelerator) -> String {
    let trigger = accelerator_to_trigger(accelerator);
    format!(
        "Wayland does not let applications grab a global shortcut directly, so \
         {trigger} cannot be registered here. To use the compositor fallback, \
         bind {trigger} in your compositor to run: odytty control quick-terminal \
         toggle, with automation_endpoint = on. You can also use a desktop that \
         provides the {GLOBAL_SHORTCUTS_INTERFACE} portal, run OdyTTY under X11, \
         or use Toggle Quick Terminal from the command palette in an ordinary window."
    )
}

/// Stable notice for a portal that is present but does not grant the requested
/// binding. The response code stays in the message for diagnostics; the next
/// steps work for both a dismissed permission prompt and compositor refusal.
pub(super) fn portal_refused_reason(accelerator: &Accelerator, code: u32) -> String {
    let trigger = accelerator_to_trigger(accelerator);
    format!(
        "The Wayland GlobalShortcuts portal did not grant {trigger} (response code {code}). Allow the shortcut in your desktop's portal prompt or choose a different quick_terminal_shortcut, then restart OdyTTY. Alternatively, bind {trigger} in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on. Toggle Quick Terminal in the command palette also works from an ordinary window."
    )
}

fn portal_unconfirmed_binding_reason(trigger: &str) -> String {
    format!(
        "The Wayland GlobalShortcuts portal reported success without granting {trigger}. Choose a different quick_terminal_shortcut and restart OdyTTY, or bind {trigger} in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on. Toggle Quick Terminal in the command palette also works from an ordinary window."
    )
}

fn portal_registry_registration_failed_reason(trigger: &str) -> String {
    format!(
        "The Wayland portal could not register OdyTTY's application ID with {REGISTRY_INTERFACE}.Register before requesting {trigger}. Restart xdg-desktop-portal and OdyTTY, then try again. Alternatively, bind {trigger} in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on."
    )
}

fn portal_registry_required_reason(trigger: &str) -> String {
    format!(
        "The Wayland GlobalShortcuts portal requires an application ID, but this xdg-desktop-portal does not provide {REGISTRY_INTERFACE}.Register (requires xdg-desktop-portal 1.18 or newer). Update the portal and restart OdyTTY. Until then, bind {trigger} in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on."
    )
}

pub(super) use transport::{PortalFailure, WaylandGrab, try_register_portal};

/// The live `zbus` GlobalShortcuts portal transport. Kept in a submodule so the
/// D-Bus dependency imports stay scoped and the pure seam above has no async
/// surface. Everything here runs on a dedicated thread via `async_io::block_on`;
/// no async runtime touches the main event loop. The portal I/O is factored
/// behind the [`PortalTransport`] seam so the ordering, cancellation, session
/// cleanup, bind verification, and activation filtering are unit-tested with an
/// injected mock (see `transport_tests`), independent of a live portal.
mod transport {
    use super::{
        Accelerator, GLOBAL_SHORTCUTS_INTERFACE, PORTAL_APP_ID, PORTAL_BUS_NAME,
        PORTAL_OBJECT_PATH, PortalResponse, QUICK_SUMMON_SHORTCUT_ID, REGISTRY_INTERFACE,
        REQUEST_INTERFACE, SESSION_INTERFACE, ShortcutBinding, accelerator_to_trigger,
        handle_token, portal_refused_reason, portal_registry_registration_failed_reason,
        portal_registry_required_reason, portal_unavailable_reason,
        portal_unconfirmed_binding_reason, request_object_path,
    };
    use crate::native::quick_terminal::SummonSink;
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use async_io::Timer;
    use futures_lite::{StreamExt, future};
    use zbus::proxy::SignalStream;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
    use zbus::{Connection, Proxy};

    /// Cap on the session-bus connect + proxy + subscribe phase.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Cap on each portal method call plus its `Response` wait.
    const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
    /// Cap on the best-effort teardown `Session.Close`.
    const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
    /// Backstop the registering thread waits for the setup outcome. Larger than
    /// the sum of the per-step timeouts so a normal (slow) portal still
    /// resolves, while a wedged worker cannot block registration forever.
    const REGISTER_BACKSTOP: Duration = Duration::from_secs(40);
    /// How often the activation pump rechecks the stop flag between signals, so a
    /// teardown takes effect promptly without busy-spinning.
    const PUMP_TICK: Duration = Duration::from_millis(250);
    /// How often a bounded step rechecks the stop flag while waiting, so a
    /// cancellation lands promptly without busy-spinning.
    const CANCEL_POLL: Duration = Duration::from_millis(25);

    /// Why the portal route did not produce a live binding. Mapped by the caller
    /// to the honest `Unsupported` (no portal environment) vs `Unavailable`
    /// (portal present but refused/cancelled/timed out) registration outcome.
    /// `Clone` so the mock transport in tests can hand back a programmed failure.
    #[derive(Clone)]
    pub(in crate::native::quick_terminal) enum PortalFailure {
        Unsupported(String),
        Unavailable(String),
    }

    /// A live portal binding: only the stop flag. Dropping it is non-blocking -
    /// it sets the flag and returns; the detached worker thread notices within
    /// [`PUMP_TICK`], performs its own bounded `Close`, and exits. The event
    /// loop therefore never joins against portal I/O while holding `quick_live`.
    pub(in crate::native::quick_terminal) struct WaylandGrab {
        stop: Arc<AtomicBool>,
    }

    impl Drop for WaylandGrab {
        fn drop(&mut self) {
            // Non-blocking: signal stop and return. The worker owns cleanup.
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    /// The injectable portal-transport seam. The orchestration ([`run_setup`],
    /// [`run_with`], [`pump`]) is generic over it so unit tests can drive the
    /// exact failure, cancellation, and cleanup ordering with a mock, while
    /// production uses [`RealTransport`] over `zbus`.
    ///
    /// The trait is crate-private and only ever used with a concrete type on a
    /// single dedicated thread inside `block_on`; its futures are never sent
    /// between threads while suspended, so no `Send` bound is needed and the
    /// `async_fn_in_trait` shape is deliberate.
    #[allow(async_fn_in_trait)]
    trait PortalTransport {
        /// Open the session bus and resolve the unique connection name.
        async fn connect(&mut self) -> Result<(), PortalFailure>;
        /// Register the packaged application id before any GlobalShortcuts
        /// method is used. A missing Registry interface is a fallthrough for
        /// compatibility with older portals; other errors are terminal.
        async fn register_app_id(&mut self) -> Result<RegistryRegistration, PortalFailure>;
        /// Issue `CreateSession` and await its `Response`. On success, retain
        /// the session handle internally (for later `Close`) and return it as a
        /// string for activation filtering.
        async fn create_session(&mut self) -> Result<String, PortalFailure>;
        /// Issue `BindShortcuts` for `binding` and await its `Response`. Return
        /// the shortcut ids the portal reports as actually bound.
        async fn bind_shortcuts(
            &mut self,
            binding: &ShortcutBinding,
        ) -> Result<Vec<String>, PortalFailure>;
        /// Wait up to `tick` for one `Activated` signal.
        async fn next_activation(&mut self, tick: Duration) -> ActivationPoll;
        /// Best-effort `Session.Close` of any created session.
        async fn close_session(&mut self);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RegistryRegistration {
        Registered,
        Missing,
    }

    /// One poll of the activation stream.
    enum ActivationPoll {
        /// A signal arrived carrying the originating session handle and id.
        Fired { session: String, id: String },
        /// The tick elapsed with no signal.
        Idle,
        /// The stream ended.
        Closed,
    }

    /// Per-step timeout budget, injected so tests can run with tiny values.
    struct Timeouts {
        connect: Duration,
        response: Duration,
        close: Duration,
    }

    impl Timeouts {
        fn production() -> Self {
            Self {
                connect: CONNECT_TIMEOUT,
                response: RESPONSE_TIMEOUT,
                close: CLOSE_TIMEOUT,
            }
        }
    }

    /// Outcome of a step raced against a timeout and the stop flag.
    enum Bounded<T> {
        Done(T),
        TimedOut,
        Cancelled,
    }

    /// Race `fut` against BOTH a `timeout` and the `stop` flag. This is what
    /// makes every portal step cancellable: dropping the losing future on
    /// cancel/timeout drops any in-flight D-Bus call it owned.
    async fn bounded<F, T>(fut: F, timeout: Duration, stop: &AtomicBool) -> Bounded<T>
    where
        F: Future<Output = T>,
    {
        future::or(async { Bounded::Done(fut.await) }, async {
            let deadline = Instant::now() + timeout;
            loop {
                if stop.load(Ordering::SeqCst) {
                    return Bounded::Cancelled;
                }
                if Instant::now() >= deadline {
                    return Bounded::TimedOut;
                }
                Timer::after(CANCEL_POLL).await;
            }
        })
        .await
    }

    /// Bounded, best-effort `Close` so teardown can never wedge.
    async fn bounded_close<T: PortalTransport>(t: &mut T, timeout: Duration) {
        future::or(
            async {
                t.close_session().await;
            },
            async {
                Timer::after(timeout).await;
            },
        )
        .await;
    }

    /// A registration cancelled by a disable/reconfigure/teardown.
    fn cancelled_failure() -> PortalFailure {
        PortalFailure::Unavailable(
            "the GlobalShortcuts portal registration was cancelled before it confirmed".to_owned(),
        )
    }

    /// Drive connect -> host-app registration -> create -> bind, returning the
    /// created session handle string on success. Every step is bounded by
    /// timeout and stop; a session created before a later failure is closed
    /// (bounded) before returning.
    async fn run_setup<T: PortalTransport>(
        t: &mut T,
        binding: &ShortcutBinding,
        stop: &AtomicBool,
        to: &Timeouts,
    ) -> Result<String, PortalFailure> {
        if stop.load(Ordering::SeqCst) {
            return Err(cancelled_failure());
        }
        match bounded(t.connect(), to.connect, stop).await {
            Bounded::Done(Ok(())) => {}
            Bounded::Done(Err(e)) => return Err(e),
            Bounded::TimedOut => {
                return Err(PortalFailure::Unavailable(
                    "connecting to the GlobalShortcuts portal timed out".to_owned(),
                ));
            }
            Bounded::Cancelled => return Err(cancelled_failure()),
        }

        if stop.load(Ordering::SeqCst) {
            return Err(cancelled_failure());
        }
        match bounded(t.register_app_id(), to.response, stop).await {
            Bounded::Done(Ok(_)) => {}
            Bounded::Done(Err(e)) => return Err(e),
            Bounded::TimedOut => {
                return Err(PortalFailure::Unavailable(
                    portal_registry_registration_failed_reason(&binding.preferred_trigger),
                ));
            }
            Bounded::Cancelled => return Err(cancelled_failure()),
        }

        if stop.load(Ordering::SeqCst) {
            return Err(cancelled_failure());
        }
        let session = match bounded(t.create_session(), to.response, stop).await {
            Bounded::Done(Ok(s)) => s,
            Bounded::Done(Err(e)) => return Err(e),
            Bounded::TimedOut => {
                return Err(PortalFailure::Unavailable(
                    "the GlobalShortcuts portal did not create a session in time".to_owned(),
                ));
            }
            Bounded::Cancelled => return Err(cancelled_failure()),
        };

        // A session now exists: EVERY subsequent failure path must close it.
        let bound = match bounded(t.bind_shortcuts(binding), to.response, stop).await {
            Bounded::Done(Ok(ids)) => ids,
            Bounded::Done(Err(e)) => {
                bounded_close(t, to.close).await;
                return Err(e);
            }
            Bounded::TimedOut => {
                bounded_close(t, to.close).await;
                return Err(PortalFailure::Unavailable(
                    "the GlobalShortcuts portal did not bind the shortcut in time".to_owned(),
                ));
            }
            Bounded::Cancelled => {
                bounded_close(t, to.close).await;
                return Err(cancelled_failure());
            }
        };

        // Require the portal to have actually bound OUR shortcut. An empty,
        // unrelated, or malformed result set is a non-registration.
        if !bound.iter().any(|id| id == QUICK_SUMMON_SHORTCUT_ID) {
            bounded_close(t, to.close).await;
            return Err(PortalFailure::Unavailable(
                portal_unconfirmed_binding_reason(&binding.preferred_trigger),
            ));
        }

        if stop.load(Ordering::SeqCst) {
            bounded_close(t, to.close).await;
            return Err(cancelled_failure());
        }
        Ok(session)
    }

    /// Thread body: run setup, report the outcome, then pump activations until
    /// stopped and close the session (bounded).
    async fn run_with<T: PortalTransport>(
        mut t: T,
        sink: SummonSink,
        stop: Arc<AtomicBool>,
        tx: mpsc::Sender<Result<(), PortalFailure>>,
        binding: ShortcutBinding,
        to: Timeouts,
    ) {
        match run_setup(&mut t, &binding, &stop, &to).await {
            // On any setup failure a created session was already closed inside
            // run_setup; nothing more to clean up here.
            Err(failure) => {
                let _ = tx.send(Err(failure));
            }
            Ok(session) => {
                let _ = tx.send(Ok(()));
                pump(&mut t, &session, &sink, &stop).await;
                bounded_close(&mut t, to.close).await;
            }
        }
    }

    /// Deliver accepted `Activated` signals into the summon sink until stopped
    /// or the stream ends.
    async fn pump<T: PortalTransport>(
        t: &mut T,
        current_session: &str,
        sink: &SummonSink,
        stop: &AtomicBool,
    ) {
        while !stop.load(Ordering::SeqCst) {
            match t.next_activation(PUMP_TICK).await {
                ActivationPoll::Fired { session, id } => {
                    // Re-check stop so a signal racing teardown cannot summon.
                    if !stop.load(Ordering::SeqCst)
                        && activation_accepted(&session, current_session, &id)
                    {
                        sink();
                    }
                }
                ActivationPoll::Idle => {}
                ActivationPoll::Closed => break,
            }
        }
    }

    /// An `Activated` signal is accepted only when it carries BOTH this
    /// session's handle AND the summon shortcut id. This rejects queued or
    /// concurrent-session signals and ensures a stale detached session cannot
    /// summon the current window.
    fn activation_accepted(msg_session: &str, current_session: &str, id: &str) -> bool {
        msg_session == current_session && id == QUICK_SUMMON_SHORTCUT_ID
    }

    /// Register the quick-terminal summon through the GlobalShortcuts portal.
    /// Spawns the pump thread, waits (bounded) for the CreateSession +
    /// BindShortcuts setup to confirm, and returns the live [`WaylandGrab`] on
    /// success. The thread then delivers `Activated` into `sink` until the grab
    /// is dropped. Never returns success without the portal confirming the bind.
    pub(in crate::native::quick_terminal) fn try_register_portal(
        accelerator: &Accelerator,
        sink: SummonSink,
    ) -> Result<WaylandGrab, PortalFailure> {
        register_with_transport(RealTransport::new(accelerator.clone()), accelerator, sink)
    }

    /// Registration core, generic over the transport so tests can inject a mock.
    fn register_with_transport<T: PortalTransport + Send + 'static>(
        transport: T,
        accelerator: &Accelerator,
        sink: SummonSink,
    ) -> Result<WaylandGrab, PortalFailure> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let binding = ShortcutBinding::quick_summon(accelerator);
        let (tx, rx) = mpsc::channel::<Result<(), PortalFailure>>();

        let handle = std::thread::Builder::new()
            .name("odytty-wayland-hotkey".to_owned())
            .spawn(move || {
                async_io::block_on(run_with(
                    transport,
                    sink,
                    stop_thread,
                    tx,
                    binding,
                    Timeouts::production(),
                ));
            })
            .map_err(|e| {
                PortalFailure::Unavailable(format!("cannot spawn the Wayland portal thread: {e}"))
            })?;

        match rx.recv_timeout(REGISTER_BACKSTOP) {
            Ok(Ok(())) => {
                // Confirmed. Detach the worker: it owns activation delivery and,
                // on stop, its own bounded Close. WaylandGrab::drop stops it.
                drop(handle);
                Ok(WaylandGrab { stop })
            }
            Ok(Err(failure)) => {
                // Setup failed; the worker already closed any created session
                // and is returning. Detach it.
                stop.store(true, Ordering::SeqCst);
                drop(handle);
                Err(failure)
            }
            Err(_) => {
                // The bounded setup did not report in time: signal stop and
                // detach. The worker's steps are all bounded, so it exits on its
                // own; registration is never blocked on a join.
                stop.store(true, Ordering::SeqCst);
                drop(handle);
                Err(PortalFailure::Unavailable(
                    "the GlobalShortcuts portal setup did not complete in time".to_owned(),
                ))
            }
        }
    }

    /// The production `zbus` transport. Holds the session-bus connection, host
    /// registration result, GlobalShortcuts proxy, live `Activated` stream, and
    /// created session handle across the trait's steps.
    struct RealTransport {
        acc: Accelerator,
        conn: Option<Connection>,
        unique_name: String,
        shortcuts: Option<Proxy<'static>>,
        activated: Option<SignalStream<'static>>,
        session_handle: Option<OwnedObjectPath>,
        registry_registration: Option<RegistryRegistration>,
    }

    impl RealTransport {
        fn new(acc: Accelerator) -> Self {
            Self {
                acc,
                conn: None,
                unique_name: String::new(),
                shortcuts: None,
                activated: None,
                session_handle: None,
                registry_registration: None,
            }
        }
    }

    impl PortalTransport for RealTransport {
        async fn connect(&mut self) -> Result<(), PortalFailure> {
            let conn = Connection::session()
                .await
                .map_err(|_| PortalFailure::Unsupported(portal_unavailable_reason(&self.acc)))?;
            let unique = conn
                .unique_name()
                .map(|name| name.as_str().to_owned())
                .ok_or_else(|| PortalFailure::Unsupported(portal_unavailable_reason(&self.acc)))?;
            self.unique_name = unique;
            self.conn = Some(conn);
            Ok(())
        }

        async fn register_app_id(&mut self) -> Result<RegistryRegistration, PortalFailure> {
            let conn = self
                .conn
                .clone()
                .ok_or_else(|| internal_unavailable("no active portal connection"))?;
            let registry = match Proxy::new(
                &conn,
                PORTAL_BUS_NAME,
                PORTAL_OBJECT_PATH,
                REGISTRY_INTERFACE,
            )
            .await
            {
                Ok(proxy) => proxy,
                Err(e) if is_missing_portal(&e) => {
                    self.registry_registration = Some(RegistryRegistration::Missing);
                    return Ok(RegistryRegistration::Missing);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        interface = REGISTRY_INTERFACE,
                        "Wayland portal app-id registry proxy failed"
                    );
                    return Err(PortalFailure::Unavailable(
                        portal_registry_registration_failed_reason(&accelerator_to_trigger(
                            &self.acc,
                        )),
                    ));
                }
            };
            let options: HashMap<&str, Value> = HashMap::new();
            match registry
                .call_method("Register", &(PORTAL_APP_ID, options))
                .await
            {
                Ok(_) => {
                    self.registry_registration = Some(RegistryRegistration::Registered);
                    Ok(RegistryRegistration::Registered)
                }
                Err(e) if is_missing_portal(&e) => {
                    self.registry_registration = Some(RegistryRegistration::Missing);
                    Ok(RegistryRegistration::Missing)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        interface = REGISTRY_INTERFACE,
                        "Wayland portal app-id registration failed"
                    );
                    Err(PortalFailure::Unavailable(
                        portal_registry_registration_failed_reason(&accelerator_to_trigger(
                            &self.acc,
                        )),
                    ))
                }
            }
        }

        async fn create_session(&mut self) -> Result<String, PortalFailure> {
            let conn = self
                .conn
                .clone()
                .ok_or_else(|| internal_unavailable("no active portal connection"))?;
            let shortcuts = Proxy::new(
                &conn,
                PORTAL_BUS_NAME,
                PORTAL_OBJECT_PATH,
                GLOBAL_SHORTCUTS_INTERFACE,
            )
            .await
            .map_err(|_| PortalFailure::Unsupported(portal_unavailable_reason(&self.acc)))?;
            // Subscribe to Activated before CreateSession/BindShortcuts so no
            // activation can race ahead of the subscription. Registry.Register
            // has already run on this connection.
            let activated = shortcuts.receive_signal("Activated").await.map_err(|e| {
                PortalFailure::Unavailable(format!("cannot watch portal activations: {e}"))
            })?;
            let token = handle_token();
            let session_token = handle_token();
            let request_path = request_object_path(&self.unique_name, &token);
            let request = Proxy::new(&conn, PORTAL_BUS_NAME, request_path, REQUEST_INTERFACE)
                .await
                .map_err(|e| {
                    PortalFailure::Unavailable(format!("cannot watch the portal request: {e}"))
                })?;
            let mut responses = request.receive_signal("Response").await.map_err(|e| {
                PortalFailure::Unavailable(format!("cannot watch the portal response: {e}"))
            })?;

            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));
            options.insert("session_handle_token", Value::from(session_token));
            shortcuts
                .call_method("CreateSession", &(options,))
                .await
                .map_err(|e| {
                    if self.registry_registration == Some(RegistryRegistration::Missing)
                        && is_app_id_required_error(&e)
                    {
                        PortalFailure::Unavailable(portal_registry_required_reason(
                            &accelerator_to_trigger(&self.acc),
                        ))
                    } else {
                        classify_call_error("CreateSession", &e, &self.acc)
                    }
                })?;

            let (code, results) = await_response(&mut responses).await?;
            if !PortalResponse::from_code(code).is_success() {
                return Err(PortalFailure::Unavailable(portal_refused_reason(
                    &self.acc, code,
                )));
            }
            let handle = session_handle_from(&results).ok_or_else(|| {
                PortalFailure::Unavailable(
                    "the portal CreateSession response carried no session_handle".to_owned(),
                )
            })?;
            let as_string = handle.as_str().to_owned();
            self.shortcuts = Some(shortcuts);
            self.activated = Some(activated);
            self.session_handle = Some(handle);
            Ok(as_string)
        }

        async fn bind_shortcuts(
            &mut self,
            binding: &ShortcutBinding,
        ) -> Result<Vec<String>, PortalFailure> {
            let conn = self
                .conn
                .clone()
                .ok_or_else(|| internal_unavailable("no active portal connection"))?;
            let shortcuts = self
                .shortcuts
                .clone()
                .ok_or_else(|| internal_unavailable("no GlobalShortcuts proxy"))?;
            let session = self
                .session_handle
                .clone()
                .ok_or_else(|| internal_unavailable("no portal session"))?;
            let token = handle_token();
            let request_path = request_object_path(&self.unique_name, &token);
            let request = Proxy::new(&conn, PORTAL_BUS_NAME, request_path, REQUEST_INTERFACE)
                .await
                .map_err(|e| {
                    PortalFailure::Unavailable(format!("cannot watch the portal request: {e}"))
                })?;
            let mut responses = request.receive_signal("Response").await.map_err(|e| {
                PortalFailure::Unavailable(format!("cannot watch the portal response: {e}"))
            })?;

            let mut props: HashMap<&str, Value> = HashMap::new();
            props.insert("description", Value::from(binding.description.clone()));
            props.insert(
                "preferred_trigger",
                Value::from(binding.preferred_trigger.clone()),
            );
            let shortcut_list: Vec<(String, HashMap<&str, Value>)> =
                vec![(binding.id.clone(), props)];
            let parent_window = String::new();
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));

            shortcuts
                .call_method(
                    "BindShortcuts",
                    &(&session, shortcut_list, parent_window, options),
                )
                .await
                .map_err(|e| classify_call_error("BindShortcuts", &e, &self.acc))?;

            let (code, results) = await_response(&mut responses).await?;
            if !PortalResponse::from_code(code).is_success() {
                return Err(PortalFailure::Unavailable(portal_refused_reason(
                    &self.acc, code,
                )));
            }
            Ok(extract_bound_shortcut_ids(&results))
        }

        async fn next_activation(&mut self, tick: Duration) -> ActivationPoll {
            let Some(activated) = self.activated.as_mut() else {
                return ActivationPoll::Closed;
            };
            let waited = future::or(async { Waited::Signal(activated.next().await) }, async {
                Timer::after(tick).await;
                Waited::Elapsed
            })
            .await;
            match waited {
                Waited::Elapsed => ActivationPoll::Idle,
                Waited::Signal(None) => ActivationPoll::Closed,
                Waited::Signal(Some(message)) => {
                    match message
                        .body()
                        .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>(
                        ) {
                        Ok((session, id, _timestamp, _options)) => ActivationPoll::Fired {
                            session: session.as_str().to_owned(),
                            id,
                        },
                        // An unparseable Activated is ignored, not fatal.
                        Err(_) => ActivationPoll::Idle,
                    }
                }
            }
        }

        async fn close_session(&mut self) {
            let (Some(conn), Some(session)) = (self.conn.clone(), self.session_handle.clone())
            else {
                return;
            };
            if let Ok(proxy) = Proxy::new(&conn, PORTAL_BUS_NAME, session, SESSION_INTERFACE).await
            {
                let _ = proxy.call_method("Close", &()).await;
            }
            self.session_handle = None;
        }
    }

    /// Await one portal `Response` signal. The timeout and cancellation are
    /// supplied by the [`bounded`] wrapper around the whole call, so this only
    /// distinguishes a closed stream from a parse failure.
    async fn await_response(
        responses: &mut SignalStream<'static>,
    ) -> Result<(u32, HashMap<String, OwnedValue>), PortalFailure> {
        match responses.next().await {
            None => Err(PortalFailure::Unavailable(
                "the GlobalShortcuts portal response stream closed unexpectedly".to_owned(),
            )),
            Some(message) => message.body().deserialize().map_err(|e| {
                PortalFailure::Unavailable(format!("could not parse the portal Response: {e}"))
            }),
        }
    }

    /// Result of racing a signal wait against a timer.
    enum Waited {
        Signal(Option<zbus::Message>),
        Elapsed,
    }

    /// An internal-invariant failure (a step was reached without its
    /// prerequisite state). Reported as a specific `Unavailable`.
    fn internal_unavailable(what: &str) -> PortalFailure {
        PortalFailure::Unavailable(format!("internal portal transport error: {what}"))
    }

    /// Extract the shortcut ids the portal reports as actually bound from a
    /// `BindShortcuts` results dict. The `shortcuts` key holds an `a(sa{sv})`:
    /// an array of (id, metadata) pairs. A missing key or a value of an
    /// unexpected shape yields an empty list (a non-registration), never a
    /// panic.
    fn extract_bound_shortcut_ids(results: &HashMap<String, OwnedValue>) -> Vec<String> {
        let Some(value) = results.get("shortcuts") else {
            return Vec::new();
        };
        match <Vec<(String, HashMap<String, OwnedValue>)>>::try_from(value.clone()) {
            Ok(list) => list.into_iter().map(|(id, _)| id).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Extract a `session_handle` from a portal results dict, accepting either an
    /// object-path (`o`) or string (`s`) representation across portal versions.
    fn session_handle_from(results: &HashMap<String, OwnedValue>) -> Option<OwnedObjectPath> {
        let value = results.get("session_handle")?;
        if let Ok(path) = OwnedObjectPath::try_from(value.clone()) {
            return Some(path);
        }
        if let Ok(text) = String::try_from(value.clone())
            && let Ok(path) = OwnedObjectPath::try_from(text)
        {
            return Some(path);
        }
        None
    }

    /// Classify a failed portal method call: a D-Bus "unknown service / method /
    /// interface / object" error means the portal does not implement
    /// GlobalShortcuts here (Unsupported, actionable); anything else is a
    /// present-but-failing portal (Unavailable, with the specific cause).
    fn classify_call_error(method: &str, err: &zbus::Error, acc: &Accelerator) -> PortalFailure {
        if is_missing_portal(err) {
            PortalFailure::Unsupported(portal_unavailable_reason(acc))
        } else {
            PortalFailure::Unavailable(format!("the portal {method} call failed: {err}"))
        }
    }

    /// Whether a zbus error indicates the GlobalShortcuts portal is simply not
    /// present on this bus.
    fn is_missing_portal(err: &zbus::Error) -> bool {
        match err {
            zbus::Error::InterfaceNotFound => true,
            zbus::Error::MethodError(name, _, _) => {
                let name = name.as_str();
                name.ends_with(".ServiceUnknown")
                    || name.ends_with(".NameHasNoOwner")
                    || name.ends_with(".UnknownMethod")
                    || name.ends_with(".UnknownInterface")
                    || name.ends_with(".UnknownObject")
            }
            _ => false,
        }
    }

    /// The old-portal failure seen after Registry was absent: GlobalShortcuts
    /// refuses a host client whose connection has no registered application id.
    fn is_app_id_required_error(err: &zbus::Error) -> bool {
        let zbus::Error::MethodError(name, detail, _) = err else {
            return false;
        };
        detail
            .as_deref()
            .is_some_and(|message| is_app_id_required(name.as_str(), message))
    }

    fn is_app_id_required(error_name: &str, message: &str) -> bool {
        if !error_name.ends_with(".NotAllowed") {
            return false;
        }
        let message = message.to_ascii_lowercase();
        message.contains("app id is required") || message.contains("application id is required")
    }

    #[cfg(test)]
    mod transport_tests {
        use super::*;
        use std::cell::RefCell;
        use std::rc::Rc;

        fn acc() -> Accelerator {
            Accelerator {
                ctrl: false,
                alt: false,
                shift: false,
                meta: false,
                key: "F12".to_owned(),
            }
        }

        fn binding() -> ShortcutBinding {
            ShortcutBinding::quick_summon(&acc())
        }

        fn fast_timeouts() -> Timeouts {
            Timeouts {
                connect: Duration::from_millis(150),
                response: Duration::from_millis(150),
                close: Duration::from_millis(150),
            }
        }

        /// Programmed outcome for one mock step.
        enum Step<T> {
            Ok(T),
            Fail(PortalFailure),
            /// Never resolves within the test window, so a timeout or the stop
            /// flag decides the outcome.
            Block,
        }

        async fn run_step<T: Clone>(step: &Step<T>) -> Result<T, PortalFailure> {
            match step {
                Step::Ok(v) => Ok(v.clone()),
                Step::Fail(f) => Err(f.clone()),
                Step::Block => {
                    Timer::after(Duration::from_secs(3600)).await;
                    unreachable!("blocking step must be cancelled or time out")
                }
            }
        }

        /// When (if ever) the mock flips `stop` mid-setup to simulate concurrent
        /// disable/reconfigure while a step is in flight.
        #[derive(Clone, Copy)]
        enum FlipStop {
            Never,
            /// Flip as soon as `connect` is entered (before its programmed step).
            OnConnect,
            /// Flip as soon as `register_app_id` is entered.
            OnRegister,
            /// Flip as soon as `create_session` is entered.
            OnCreate,
            /// Flip after `create_session` returns Ok (cancels the pending bind).
            AfterCreateOk,
        }

        /// A scripted transport that records call order and can simulate a
        /// concurrent teardown landing at each setup phase.
        struct MockTransport {
            log: Rc<RefCell<Vec<&'static str>>>,
            stop: Arc<AtomicBool>,
            flip_stop: FlipStop,
            connect: Step<()>,
            register: Step<RegistryRegistration>,
            create: Step<String>,
            bind: Step<Vec<String>>,
            close: Step<()>,
            activations: RefCell<Vec<ActivationPoll>>,
        }

        impl PortalTransport for MockTransport {
            async fn connect(&mut self) -> Result<(), PortalFailure> {
                self.log.borrow_mut().push("connect");
                if matches!(self.flip_stop, FlipStop::OnConnect) {
                    self.stop.store(true, Ordering::SeqCst);
                }
                run_step(&self.connect).await
            }

            async fn register_app_id(&mut self) -> Result<RegistryRegistration, PortalFailure> {
                self.log.borrow_mut().push("register");
                if matches!(self.flip_stop, FlipStop::OnRegister) {
                    self.stop.store(true, Ordering::SeqCst);
                }
                run_step(&self.register).await
            }

            async fn create_session(&mut self) -> Result<String, PortalFailure> {
                self.log.borrow_mut().push("create");
                if matches!(self.flip_stop, FlipStop::OnCreate) {
                    self.stop.store(true, Ordering::SeqCst);
                }
                let r = run_step(&self.create).await;
                if r.is_ok() && matches!(self.flip_stop, FlipStop::AfterCreateOk) {
                    self.stop.store(true, Ordering::SeqCst);
                }
                r
            }

            async fn bind_shortcuts(
                &mut self,
                _binding: &ShortcutBinding,
            ) -> Result<Vec<String>, PortalFailure> {
                self.log.borrow_mut().push("bind");
                run_step(&self.bind).await
            }

            async fn next_activation(&mut self, _tick: Duration) -> ActivationPoll {
                let mut q = self.activations.borrow_mut();
                if q.is_empty() {
                    ActivationPoll::Closed
                } else {
                    q.remove(0)
                }
            }

            async fn close_session(&mut self) {
                self.log.borrow_mut().push("close");
                let _ = run_step(&self.close).await;
            }
        }

        struct Fixture {
            log: Rc<RefCell<Vec<&'static str>>>,
            stop: Arc<AtomicBool>,
            transport: MockTransport,
        }

        fn fixture(
            flip_stop_after_create: bool,
            connect: Step<()>,
            create: Step<String>,
            bind: Step<Vec<String>>,
        ) -> Fixture {
            fixture_ex(
                if flip_stop_after_create {
                    FlipStop::AfterCreateOk
                } else {
                    FlipStop::Never
                },
                connect,
                Step::Ok(RegistryRegistration::Registered),
                create,
                bind,
                Step::Ok(()),
                Vec::new(),
            )
        }

        fn fixture_ex(
            flip_stop: FlipStop,
            connect: Step<()>,
            register: Step<RegistryRegistration>,
            create: Step<String>,
            bind: Step<Vec<String>>,
            close: Step<()>,
            activations: Vec<ActivationPoll>,
        ) -> Fixture {
            let log = Rc::new(RefCell::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let transport = MockTransport {
                log: Rc::clone(&log),
                stop: Arc::clone(&stop),
                flip_stop,
                connect,
                register,
                create,
                bind,
                close,
                activations: RefCell::new(activations),
            };
            Fixture {
                log,
                stop,
                transport,
            }
        }

        fn drive(mut fx: Fixture) -> (Result<String, PortalFailure>, Vec<&'static str>) {
            let b = binding();
            let res =
                async_io::block_on(run_setup(&mut fx.transport, &b, &fx.stop, &fast_timeouts()));
            let log = fx.log.borrow().clone();
            (res, log)
        }

        #[test]
        fn bind_confirming_requested_id_succeeds_without_close() {
            let fx = fixture(
                false,
                Step::Ok(()),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Ok(ref s) if s == "/session/1"));
            assert_eq!(log, vec!["connect", "register", "create", "bind"]);
        }

        #[test]
        fn missing_registry_falls_through_to_create_session() {
            let fx = fixture_ex(
                FlipStop::Never,
                Step::Ok(()),
                Step::Ok(RegistryRegistration::Missing),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Ok(()),
                Vec::new(),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Ok(ref s) if s == "/session/1"));
            assert_eq!(log, vec!["connect", "register", "create", "bind"]);
        }

        #[test]
        fn register_failure_stops_before_create_session() {
            let reason = portal_registry_registration_failed_reason("F12");
            let fx = fixture_ex(
                FlipStop::Never,
                Step::Ok(()),
                Step::Fail(PortalFailure::Unavailable(reason.clone())),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Ok(()),
                Vec::new(),
            );
            let (res, log) = drive(fx);
            assert!(matches!(
                res,
                Err(PortalFailure::Unavailable(ref actual)) if actual == &reason
            ));
            assert_eq!(log, vec!["connect", "register"]);
        }

        #[test]
        fn register_timeout_stops_before_create_session() {
            let fx = fixture_ex(
                FlipStop::Never,
                Step::Ok(()),
                Step::Block,
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Ok(()),
                Vec::new(),
            );
            let (res, log) = drive(fx);
            assert!(matches!(
                res,
                Err(PortalFailure::Unavailable(ref reason))
                    if reason == &portal_registry_registration_failed_reason("F12")
            ));
            assert_eq!(log, vec!["connect", "register"]);
        }

        #[test]
        fn bind_without_requested_id_closes_and_fails() {
            // The portal answered success but bound a different (or no) shortcut.
            for bound in [vec![], vec!["someone_elses_shortcut".to_owned()]] {
                let fx = fixture(
                    false,
                    Step::Ok(()),
                    Step::Ok("/session/1".to_owned()),
                    Step::Ok(bound),
                );
                let (res, log) = drive(fx);
                assert!(matches!(
                    res,
                    Err(PortalFailure::Unavailable(ref reason))
                        if reason == "The Wayland GlobalShortcuts portal reported success without granting F12. Choose a different quick_terminal_shortcut and restart OdyTTY, or bind F12 in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on. Toggle Quick Terminal in the command palette also works from an ordinary window."
                ));
                assert_eq!(log, vec!["connect", "register", "create", "bind", "close"]);
            }
        }

        #[test]
        fn bind_failure_after_create_closes_session() {
            let fx = fixture(
                false,
                Step::Ok(()),
                Step::Ok("/session/1".to_owned()),
                Step::Fail(PortalFailure::Unavailable("refused".to_owned())),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect", "register", "create", "bind", "close"]);
        }

        #[test]
        fn bind_timeout_after_create_closes_session() {
            let fx = fixture(
                false,
                Step::Ok(()),
                Step::Ok("/session/1".to_owned()),
                Step::Block,
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect", "register", "create", "bind", "close"]);
        }

        #[test]
        fn cancel_after_create_closes_session() {
            // create_session succeeds and a concurrent teardown flips stop; the
            // pending bind must be cancelled and the session closed.
            let fx = fixture(
                true,
                Step::Ok(()),
                Step::Ok("/session/1".to_owned()),
                Step::Block,
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect", "register", "create", "bind", "close"]);
        }

        #[test]
        fn create_failure_does_not_close() {
            let fx = fixture(
                false,
                Step::Ok(()),
                Step::Fail(PortalFailure::Unavailable("no session".to_owned())),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            // Nothing was created, so nothing is closed.
            assert_eq!(log, vec!["connect", "register", "create"]);
        }

        #[test]
        fn create_timeout_does_not_close() {
            let fx = fixture(
                false,
                Step::Ok(()),
                Step::Block,
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect", "register", "create"]);
        }

        #[test]
        fn connect_unsupported_stops_before_create() {
            let fx = fixture(
                false,
                Step::Fail(PortalFailure::Unsupported("no portal".to_owned())),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unsupported(_))));
            assert_eq!(log, vec!["connect"]);
        }

        #[test]
        fn connect_timeout_stops_before_create() {
            let fx = fixture(
                false,
                Step::Block,
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect"]);
        }

        #[test]
        fn stop_before_start_creates_nothing() {
            let fx = fixture(
                false,
                Step::Ok(()),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
            );
            fx.stop.store(true, Ordering::SeqCst);
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert!(log.is_empty(), "no portal step runs after a pre-set stop");
        }

        #[test]
        fn activation_accepted_requires_session_and_id() {
            // Correct session + correct id -> accepted.
            assert!(activation_accepted(
                "/session/1",
                "/session/1",
                QUICK_SUMMON_SHORTCUT_ID
            ));
            // Wrong session (e.g. a stale detached session) -> rejected.
            assert!(!activation_accepted(
                "/session/OTHER",
                "/session/1",
                QUICK_SUMMON_SHORTCUT_ID
            ));
            // Right session, wrong shortcut id -> rejected.
            assert!(!activation_accepted("/session/1", "/session/1", "other_id"));
        }

        #[test]
        fn extract_bound_ids_is_defensive() {
            // Missing key -> empty (a non-registration).
            let empty: HashMap<String, OwnedValue> = HashMap::new();
            assert!(extract_bound_shortcut_ids(&empty).is_empty());

            // Wrong-typed value -> empty, never a panic.
            let mut malformed: HashMap<String, OwnedValue> = HashMap::new();
            malformed.insert(
                "shortcuts".to_owned(),
                OwnedValue::try_from(Value::from("not an array")).expect("owned value"),
            );
            assert!(extract_bound_shortcut_ids(&malformed).is_empty());
        }

        #[test]
        fn app_id_required_error_needs_not_allowed_name_and_specific_message() {
            assert!(is_app_id_required(
                "org.freedesktop.portal.Error.NotAllowed",
                "An app id is required"
            ));
            assert!(is_app_id_required(
                "org.freedesktop.portal.Error.NotAllowed",
                "An application ID is required"
            ));
            assert!(!is_app_id_required(
                "org.freedesktop.portal.Error.NotAllowed",
                "The shortcut is reserved"
            ));
            assert!(!is_app_id_required(
                "org.freedesktop.DBus.Error.UnknownMethod",
                "An app id is required"
            ));
        }

        #[test]
        fn cancel_during_connect_creates_nothing() {
            let fx = fixture_ex(
                FlipStop::OnConnect,
                Step::Block,
                Step::Ok(RegistryRegistration::Registered),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Ok(()),
                Vec::new(),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect"]);
        }

        #[test]
        fn cancel_during_create_does_not_close() {
            // Stop flips as create is entered and create blocks; no session was
            // retained, so teardown must not Close.
            let fx = fixture_ex(
                FlipStop::OnCreate,
                Step::Ok(()),
                Step::Ok(RegistryRegistration::Registered),
                Step::Block,
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Ok(()),
                Vec::new(),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect", "register", "create"]);
        }

        #[test]
        fn cancel_during_registry_registration_creates_nothing() {
            let fx = fixture_ex(
                FlipStop::OnRegister,
                Step::Ok(()),
                Step::Block,
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Ok(()),
                Vec::new(),
            );
            let (res, log) = drive(fx);
            assert!(matches!(res, Err(PortalFailure::Unavailable(_))));
            assert_eq!(log, vec!["connect", "register"]);
        }

        #[test]
        fn hung_close_is_bounded() {
            let mut fx = fixture_ex(
                FlipStop::Never,
                Step::Ok(()),
                Step::Ok(RegistryRegistration::Registered),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Block,
                Vec::new(),
            );
            let start = Instant::now();
            async_io::block_on(bounded_close(&mut fx.transport, Duration::from_millis(150)));
            let elapsed = start.elapsed();
            assert!(
                elapsed < Duration::from_millis(800),
                "bounded_close must return despite a hung Close; elapsed={elapsed:?}"
            );
            assert_eq!(*fx.log.borrow(), vec!["close"]);
        }

        #[test]
        fn pump_ignores_unrelated_session_and_accepts_matching() {
            let summons = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let sink_count = Arc::clone(&summons);
            let sink: SummonSink = Arc::new(move || {
                sink_count.fetch_add(1, Ordering::SeqCst);
            });
            let mut fx = fixture_ex(
                FlipStop::Never,
                Step::Ok(()),
                Step::Ok(RegistryRegistration::Registered),
                Step::Ok("/session/1".to_owned()),
                Step::Ok(vec![QUICK_SUMMON_SHORTCUT_ID.to_owned()]),
                Step::Ok(()),
                vec![
                    ActivationPoll::Fired {
                        session: "/session/OTHER".to_owned(),
                        id: QUICK_SUMMON_SHORTCUT_ID.to_owned(),
                    },
                    ActivationPoll::Fired {
                        session: "/session/1".to_owned(),
                        id: "other_id".to_owned(),
                    },
                    ActivationPoll::Fired {
                        session: "/session/1".to_owned(),
                        id: QUICK_SUMMON_SHORTCUT_ID.to_owned(),
                    },
                    ActivationPoll::Closed,
                ],
            );
            async_io::block_on(pump(&mut fx.transport, "/session/1", &sink, &fx.stop));
            assert_eq!(
                summons.load(Ordering::SeqCst),
                1,
                "only the matching session+id activation may summon"
            );
        }

        #[test]
        fn wayland_grab_drop_signals_stop_without_joining() {
            let stop = Arc::new(AtomicBool::new(false));
            let grab = WaylandGrab {
                stop: Arc::clone(&stop),
            };
            let start = Instant::now();
            drop(grab);
            assert!(
                start.elapsed() < Duration::from_millis(50),
                "Drop must not join portal I/O"
            );
            assert!(stop.load(Ordering::SeqCst));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(ctrl: bool, alt: bool, shift: bool, meta: bool, key: &str) -> Accelerator {
        Accelerator {
            ctrl,
            alt,
            shift,
            meta,
            key: key.to_owned(),
        }
    }

    #[test]
    fn response_code_interpretation() {
        assert_eq!(PortalResponse::from_code(0), PortalResponse::Success);
        assert!(PortalResponse::from_code(0).is_success());
        assert_eq!(PortalResponse::from_code(1), PortalResponse::Cancelled);
        assert!(!PortalResponse::from_code(1).is_success());
        assert_eq!(PortalResponse::from_code(2), PortalResponse::Other(2));
        assert_eq!(PortalResponse::from_code(99), PortalResponse::Other(99));
        assert!(!PortalResponse::from_code(99).is_success());
    }

    #[test]
    fn handle_tokens_are_unique_and_valid() {
        let a = handle_token();
        let b = handle_token();
        assert_ne!(a, b, "each request gets a fresh token");
        for t in [&a, &b] {
            assert!(!t.is_empty());
            assert!(
                t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "token {t} must be a valid D-Bus handle token"
            );
        }
    }

    #[test]
    fn sender_sanitization_matches_portal_rules() {
        assert_eq!(sanitize_sender(":1.42"), "1_42");
        assert_eq!(sanitize_sender(":1.2.3"), "1_2_3");
        // Total on an already-sanitized or unusual name.
        assert_eq!(sanitize_sender("1_42"), "1_42");
    }

    #[test]
    fn request_path_is_predicted_per_spec() {
        assert_eq!(
            request_object_path(":1.42", "odytty_0"),
            "/org/freedesktop/portal/desktop/request/1_42/odytty_0"
        );
    }

    #[test]
    fn trigger_string_is_ordered_and_uppercase() {
        assert_eq!(
            accelerator_to_trigger(&acc(false, false, false, false, "F12")),
            "F12"
        );
        assert_eq!(
            accelerator_to_trigger(&acc(true, false, true, false, "grave")),
            "CTRL+SHIFT+GRAVE"
        );
        assert_eq!(
            accelerator_to_trigger(&acc(true, true, true, true, "a")),
            "CTRL+ALT+SHIFT+SUPER+A"
        );
    }

    #[test]
    fn quick_summon_binding_uses_stable_id() {
        let b = ShortcutBinding::quick_summon(&acc(false, false, false, false, "F12"));
        assert_eq!(b.id, QUICK_SUMMON_SHORTCUT_ID);
        assert_eq!(b.preferred_trigger, "F12");
        assert!(!b.description.is_empty());
    }

    #[test]
    fn portal_constants_are_wellformed() {
        assert_eq!(PORTAL_BUS_NAME, "org.freedesktop.portal.Desktop");
        assert!(PORTAL_OBJECT_PATH.starts_with('/'));
        assert!(REQUEST_INTERFACE.ends_with(".Request"));
        assert!(GLOBAL_SHORTCUTS_INTERFACE.ends_with(".GlobalShortcuts"));
        assert_eq!(REGISTRY_INTERFACE, "org.freedesktop.host.portal.Registry");
        assert_eq!(PORTAL_APP_ID, "io.unfinished_works.odytty");
        assert!(
            include_str!("../../../dist/linux/io.unfinished_works.odytty.desktop")
                .contains(&format!("StartupWMClass={PORTAL_APP_ID}\n"))
        );
    }

    #[test]
    fn unavailable_reason_is_actionable_ascii() {
        let msg = portal_unavailable_reason(&acc(true, false, true, false, "F12"));
        assert_eq!(
            msg,
            "Wayland does not let applications grab a global shortcut directly, so CTRL+SHIFT+F12 cannot be registered here. To use the compositor fallback, bind CTRL+SHIFT+F12 in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on. You can also use a desktop that provides the org.freedesktop.portal.GlobalShortcuts portal, run OdyTTY under X11, or use Toggle Quick Terminal from the command palette in an ordinary window."
        );
        assert!(msg.is_ascii(), "message must be ASCII (no em-dashes)");
        assert!(!msg.contains('\u{2014}'), "no em-dash");
        assert!(
            msg.contains("GlobalShortcuts"),
            "names the supported portal route"
        );
        assert!(
            msg.contains("CTRL+SHIFT+F12"),
            "names the exact trigger to bind"
        );
    }

    #[test]
    fn refused_reason_is_exact_and_actionable_without_a_portal() {
        let accelerator = acc(true, false, true, false, "F12");
        assert_eq!(
            portal_refused_reason(&accelerator, 1),
            "The Wayland GlobalShortcuts portal did not grant CTRL+SHIFT+F12 (response code 1). Allow the shortcut in your desktop's portal prompt or choose a different quick_terminal_shortcut, then restart OdyTTY. Alternatively, bind CTRL+SHIFT+F12 in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on. Toggle Quick Terminal in the command palette also works from an ordinary window."
        );
    }

    #[test]
    fn unconfirmed_reason_names_the_compositor_fallback() {
        assert_eq!(
            portal_unconfirmed_binding_reason("F12"),
            "The Wayland GlobalShortcuts portal reported success without granting F12. Choose a different quick_terminal_shortcut and restart OdyTTY, or bind F12 in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on. Toggle Quick Terminal in the command palette also works from an ordinary window."
        );
    }

    #[test]
    fn registry_registration_failure_reason_is_exact_and_actionable() {
        assert_eq!(
            portal_registry_registration_failed_reason("F12"),
            "The Wayland portal could not register OdyTTY's application ID with org.freedesktop.host.portal.Registry.Register before requesting F12. Restart xdg-desktop-portal and OdyTTY, then try again. Alternatively, bind F12 in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on."
        );
    }

    #[test]
    fn old_portal_app_id_requirement_names_minimum_version() {
        assert_eq!(
            portal_registry_required_reason("F12"),
            "The Wayland GlobalShortcuts portal requires an application ID, but this xdg-desktop-portal does not provide org.freedesktop.host.portal.Registry.Register (requires xdg-desktop-portal 1.18 or newer). Update the portal and restart OdyTTY. Until then, bind F12 in your compositor to run: odytty control quick-terminal toggle, with automation_endpoint = on."
        );
    }
}
