// SPDX-License-Identifier: GPL-3.0-only
//! Off-render-path runtime glyph fallback (Linux and other fontconfig hosts).
//!
//! The glyph atlas asks for a covering face while it prepares a frame. Asking
//! fontconfig means two helper processes and font loads per codepoint, which
//! must not stall a frame: a page of distinct missing characters would cost
//! one helper round trip each, and a stalled helper would freeze the window.
//! [`request`] therefore only consults and fills a process-wide table. A
//! codepoint without an answer is queued for one worker thread and reported
//! [`RuntimeSymbol::Pending`]; the atlas draws the fallback box for it that
//! frame without caching. When the worker finishes a batch it calls the
//! registered waker, and each window rebuilds, asking again.
//!
//! Bounds: at most [`MAX_QUEUED`] codepoints wait at once (a codepoint that
//! does not fit stays pending and is queued again on a later rebuild), the
//! answer table holds at most [`MAX_ANSWERS`] entries, and each helper run is
//! bounded in time and output. A resolver panic or a helper stall or flood
//! switches runtime fallback off for the rest of the run and every later request answers
//! "no face" at once, so a broken fontconfig costs one deadline, not one per
//! glyph.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::FontHandle;
use crate::atlas::RuntimeSymbol;

/// Codepoints waiting for the worker at once.
pub(super) const MAX_QUEUED: usize = 512;
/// Final answers kept for later requests (other windows, rebuilt atlases).
pub(super) const MAX_ANSWERS: usize = 8192;
/// Longest the worker runs before waking windows with a partial batch, so a
/// long queue shows glyphs progressively instead of all at the end.
const WAKE_BATCH_INTERVAL: Duration = Duration::from_millis(100);

type Waker = Box<dyn Fn() + Send + Sync>;
#[cfg(test)]
type Resolver = fn(char) -> Result<Option<Arc<FontHandle>>, super::symbols::FontconfigStalled>;

static WAKER: OnceLock<Waker> = OnceLock::new();
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

#[derive(Default)]
struct State {
    answers: HashMap<char, Option<Arc<FontHandle>>>,
    queue: VecDeque<char>,
    queued: HashSet<char>,
    worker_running: bool,
    disabled: bool,
    /// Test override for the blocking resolver.
    #[cfg(test)]
    resolver: Option<Resolver>,
}

/// Register the callback the worker uses to wake windows after resolving
/// codepoints. The first registration wins (one event loop per process).
pub fn set_runtime_symbol_waker(waker: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(waker));
}

/// The atlas-facing lookup. Never blocks on fontconfig.
pub(super) fn request(ch: char) -> RuntimeSymbol {
    request_with_state(ch, &STATE, wake)
}

fn request_with_state(ch: char, shared: &'static Mutex<State>, wake: fn()) -> RuntimeSymbol {
    let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
    if state.disabled {
        return RuntimeSymbol::Ready(None);
    }
    if let Some(answer) = state.answers.get(&ch) {
        return RuntimeSymbol::Ready(answer.clone());
    }
    if state.queued.contains(&ch) || state.queue.len() >= MAX_QUEUED {
        return RuntimeSymbol::Pending;
    }
    state.queue.push_back(ch);
    state.queued.insert(ch);
    if !state.worker_running {
        let spawned = std::thread::Builder::new()
            .name("odytty-glyph-fallback".to_owned())
            .spawn(move || run_worker(shared, wake));
        if spawned.is_err() {
            // No worker: answer every queued codepoint with "no face" rather
            // than leaving them pending forever.
            tracing::warn!("glyph fallback: worker thread unavailable; runtime fallback disabled");
            disable(&mut state);
            return RuntimeSymbol::Ready(None);
        }
        state.worker_running = true;
    }
    RuntimeSymbol::Pending
}

fn disable(state: &mut State) {
    state.disabled = true;
    state.queue.clear();
    state.queued.clear();
}

fn run_worker(shared: &Mutex<State>, wake: fn()) {
    let mut batch_started = Instant::now();
    loop {
        let (ch, resolver) = {
            let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(ch) = state.queue.pop_front() else {
                state.worker_running = false;
                break;
            };
            let resolver = super::symbols::resolve_symbol_font_blocking;
            #[cfg(test)]
            let resolver = state.resolver.unwrap_or(resolver);
            (ch, resolver)
        };
        let result = std::panic::catch_unwind(|| resolver(ch));
        {
            let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
            state.queued.remove(&ch);
            match result {
                Ok(Ok(answer)) => {
                    if state.answers.len() >= MAX_ANSWERS {
                        // Atlases keep their own per-codepoint cache; this
                        // table only spares repeat helper runs, so starting
                        // over is cheaper than tracking recency.
                        state.answers.clear();
                    }
                    state.answers.insert(ch, answer);
                }
                Ok(Err(_)) => {
                    tracing::warn!(
                        "glyph fallback: fontconfig helper exceeded its deadline or output cap; runtime fallback disabled for this run"
                    );
                    disable(&mut state);
                }
                Err(_) => {
                    tracing::warn!(
                        "glyph fallback: font resolver panicked; runtime fallback disabled for this run"
                    );
                    disable(&mut state);
                }
            }
        }
        if batch_started.elapsed() >= WAKE_BATCH_INTERVAL {
            wake();
            batch_started = Instant::now();
        }
    }
    wake();
}

fn wake() {
    if let Some(waker) = WAKER.get() {
        waker();
    }
}

#[cfg(test)]
#[path = "runtime_fallback_liveness_tests.rs"]
mod liveness_tests;
