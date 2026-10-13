// SPDX-License-Identifier: GPL-3.0-only
//! The handle PTY pumps, attach pumps and background workers use to wake the
//! window event loop.
//!
//! Production wraps winit's `EventLoopProxy` and forwards every event
//! unchanged. Test builds can instead hold a discarding waker, because a test
//! that never runs an event loop would leave the events unconsumed anyway.
//! winit allows one event loop per process and macOS refuses to build it off
//! the main thread, so before this handle existed every test that spawned a
//! session needed a real loop and was ignored on macOS.

use winit::event_loop::{EventLoopClosed, EventLoopProxy};

use super::pty::UserEvent;

/// A cloneable wake handle for the window event loop.
///
/// Cloning has exactly the cost of cloning the wrapped proxy (on macOS a clone
/// registers a run-loop source), so the host's clone-counting rules apply to it
/// unchanged.
#[derive(Clone)]
pub(super) struct EventWaker(Inner);

#[derive(Clone)]
enum Inner {
    Winit(EventLoopProxy<UserEvent>),
    /// Test builds only: accepts every event and delivers it nowhere.
    #[cfg(test)]
    Discard,
}

impl EventWaker {
    /// Queue `event` for the loop. Same contract as
    /// [`EventLoopProxy::send_event`]: an error means the loop has shut down.
    /// A discarding waker reports success and drops the event.
    pub(super) fn send_event(&self, event: UserEvent) -> Result<(), EventLoopClosed<UserEvent>> {
        match &self.0 {
            Inner::Winit(proxy) => proxy.send_event(event),
            #[cfg(test)]
            Inner::Discard => {
                drop(event);
                Ok(())
            }
        }
    }

    /// The wrapped winit proxy, for consumers that hand it to a platform
    /// protocol object (Wayland drop, quick terminal, automation). A
    /// discarding waker has none.
    pub(super) fn winit_proxy(&self) -> Option<&EventLoopProxy<UserEvent>> {
        match &self.0 {
            Inner::Winit(proxy) => Some(proxy),
            #[cfg(test)]
            Inner::Discard => None,
        }
    }

    /// A waker that delivers nothing (test builds only).
    #[cfg(test)]
    pub(super) fn discard() -> Self {
        Self(Inner::Discard)
    }
}

impl From<EventLoopProxy<UserEvent>> for EventWaker {
    fn from(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self(Inner::Winit(proxy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::session::SessionToken;

    #[test]
    fn a_discarding_waker_accepts_events_and_has_no_winit_proxy() {
        let waker = EventWaker::discard();
        let clone = waker.clone();
        assert!(
            clone
                .send_event(UserEvent::Redraw {
                    session: SessionToken(7)
                })
                .is_ok()
        );
        assert!(waker.winit_proxy().is_none());
    }
}
