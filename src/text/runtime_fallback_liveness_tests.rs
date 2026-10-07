// SPDX-License-Identifier: GPL-3.0-only
//! Runtime font resolution must settle without blocking a glyph request.

use super::super::symbols::FontconfigStalled;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};

const WATCHDOG: Duration = Duration::from_secs(5);
static RESOLVER_GATE: Mutex<Option<Receiver<()>>> = Mutex::new(None);
static PANIC_WAKES: AtomicUsize = AtomicUsize::new(0);

fn gated_none(_ch: char) -> Result<Option<Arc<FontHandle>>, FontconfigStalled> {
    let gate = RESOLVER_GATE.lock().unwrap().take();
    if let Some(receiver) = gate {
        receiver
            .recv_timeout(WATCHDOG)
            .expect("release resolver gate");
    }
    Ok(None)
}

fn stalled(_ch: char) -> Result<Option<Arc<FontHandle>>, FontconfigStalled> {
    Err(FontconfigStalled)
}

fn panicking(_ch: char) -> Result<Option<Arc<FontHandle>>, FontconfigStalled> {
    panic!("project-authored resolver panic");
}

fn wake_none() {}
fn wake_panic() {
    PANIC_WAKES.fetch_add(1, Ordering::SeqCst);
}

fn wait_idle(state: &Mutex<State>) -> bool {
    let until = Instant::now() + WATCHDOG;
    while Instant::now() < until {
        let idle = {
            let state = state.lock().unwrap_or_else(PoisonError::into_inner);
            !state.worker_running && state.queue.is_empty()
        };
        if idle {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

/// Each test owns separate state. Cleanup releases a gated resolver on unwind
/// and drains its worker before removing the override.
struct Fixture {
    state: &'static Mutex<State>,
    release: Option<SyncSender<()>>,
    gated: bool,
}

impl Fixture {
    fn new(state: &'static Mutex<State>, resolver: Resolver) -> Self {
        *state.lock().unwrap_or_else(PoisonError::into_inner) = State {
            resolver: Some(resolver),
            ..State::default()
        };
        Self {
            state,
            release: None,
            gated: false,
        }
    }

    fn request(&self, ch: char, wake: fn()) -> RuntimeSymbol {
        request_with_state(ch, self.state, wake)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(sender) = self.release.take() {
            let _ = sender.send(());
        }
        if wait_idle(self.state) {
            *self.state.lock().unwrap_or_else(PoisonError::into_inner) = State::default();
            if self.gated {
                *RESOLVER_GATE.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
        }
    }
}

#[test]
fn glyph_fallback_returns_pending_while_the_resolver_is_gated() {
    static LOCAL: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
    let mut fixture = Fixture::new(&LOCAL, gated_none);
    let (release, receiver) = mpsc::sync_channel(1);
    *RESOLVER_GATE.lock().unwrap() = Some(receiver);
    fixture.release = Some(release);
    fixture.gated = true;
    let (returned, result) = mpsc::sync_channel(1);
    let caller = std::thread::spawn(move || {
        let mut answers = vec![request_with_state('\u{0378}', &LOCAL, wake_none)];
        for offset in 0..200_u32 {
            let ch = char::from_u32(0x4000 + offset).expect("synthetic codepoint");
            answers.push(request_with_state(ch, &LOCAL, wake_none));
        }
        returned.send(answers).expect("return request results");
    });
    // The resolver cannot finish until the test releases it. The deadline is
    // only a deadlock watchdog, not a performance assertion.
    let answers = result
        .recv_timeout(WATCHDOG)
        .expect("requests return before resolver release");
    assert_eq!(answers.len(), 201);
    assert!(
        answers
            .iter()
            .all(|answer| matches!(answer, RuntimeSymbol::Pending))
    );
    assert!(matches!(
        fixture.request('\u{0378}', wake_none),
        RuntimeSymbol::Pending
    ));
    fixture
        .release
        .take()
        .unwrap()
        .send(())
        .expect("release resolver");
    caller.join().expect("join request caller");
    assert!(wait_idle(&LOCAL), "worker drains released request");
    assert!(matches!(
        fixture.request('\u{0378}', wake_none),
        RuntimeSymbol::Ready(None)
    ));
    *RESOLVER_GATE.lock().unwrap() = None;
}

#[test]
fn glyph_fallback_disables_after_a_helper_stall() {
    static LOCAL: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
    let fixture = Fixture::new(&LOCAL, stalled);
    assert!(matches!(
        fixture.request('\u{0379}', wake_none),
        RuntimeSymbol::Pending
    ));
    assert!(wait_idle(&LOCAL), "worker settles after helper stall");
    assert!(LOCAL.lock().unwrap().disabled);
    assert!(matches!(
        fixture.request('\u{037A}', wake_none),
        RuntimeSymbol::Ready(None)
    ));
}

#[test]
fn glyph_fallback_disables_and_wakes_after_a_resolver_panic() {
    static LOCAL: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
    let fixture = Fixture::new(&LOCAL, panicking);
    PANIC_WAKES.store(0, Ordering::SeqCst);
    assert!(matches!(
        fixture.request('\u{0379}', wake_panic),
        RuntimeSymbol::Pending
    ));
    assert!(wait_idle(&LOCAL), "worker settles after resolver panic");
    let state = LOCAL.lock().unwrap();
    assert!(state.disabled);
    assert!(state.queue.is_empty());
    assert!(state.queued.is_empty());
    drop(state);
    assert!(matches!(
        fixture.request('\u{037A}', wake_panic),
        RuntimeSymbol::Ready(None)
    ));
    let until = Instant::now() + WATCHDOG;
    while PANIC_WAKES.load(Ordering::SeqCst) == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        PANIC_WAKES.load(Ordering::SeqCst) > 0,
        "wake windows after panic"
    );
}
