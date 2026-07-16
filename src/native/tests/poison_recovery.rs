// SPDX-License-Identifier: GPL-3.0-only
//! P0-3 hardening: mutex poison recovery on the hot terminal-lock paths.
//!
//! A panic *while holding* the shared `Arc<Mutex<Terminal>>` poisons it; every
//! subsequent `.lock().expect()` on a per-event / per-frame path would then turn
//! the next mouse-move / copy / paint / OSC-title event into a SECOND abort that
//! unwinds across the AppKit→Rust FFI boundary (the original crash class). The
//! `lock_recover` helper takes the inner guard instead, keeping the event loop
//! alive. These pin both the helper itself (PTY-free, always runs) and the
//! higher-level `current_selection_text` choke point (PTY-gated, skipped in CI
//! sandboxes without a PTY).

use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Run `f` with the panic hook silenced, restoring it afterward. The poison
/// tests deliberately panic to poison a lock; without this the default hook
/// prints a scary (but expected) backtrace to the test log.
fn with_silent_panic_hook<R>(f: impl FnOnce() -> R) -> R {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = f();
    std::panic::set_hook(prev);
    out
}

#[test]
fn lock_recover_yields_usable_guard_after_poison() {
    // The helper's core contract, independent of any GPU/PTY: a poisoned lock is
    // still usable through `lock_recover`, and the protected value is intact.
    let m = std::sync::Mutex::new(7_i32);
    with_silent_panic_hook(|| {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut g = m.lock().expect("first lock");
            *g = 11;
            panic!("poison while holding the guard");
        }));
    });
    // The lock is now poisoned: a plain lock would surface an Err.
    assert!(m.lock().is_err(), "the panic poisoned the mutex");
    // lock_recover hands back the inner guard regardless, and the write the
    // panicking holder made before unwinding is preserved (no half-state here).
    let mut g = crate::native::lock_recover(&m);
    assert_eq!(*g, 11);
    *g = 42;
    drop(g);
    assert_eq!(*crate::native::lock_recover(&m), 42);
}

/// Build an `App` over a one-shot PTY, returning the App plus a *clone* of the
/// shared terminal handle so a test can poison the lock out-of-band. `None` when
/// no PTY is available (CI sandboxes), so callers skip cleanly.
fn app_with_terminal_handle(content: &[u8]) -> Option<(App, Arc<Mutex<Terminal>>)> {
    let dims = Dimensions::new(80, 24);
    let (app, terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    let handle = Arc::clone(&terminal);
    Some((app, handle))
}

#[test]
fn current_selection_text_survives_poisoned_terminal_lock() {
    let Some((mut app, handle)) = app_with_terminal_handle(b"hello world there") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Select the first word so `current_selection_text` actually reaches the
    // terminal lock (it early-returns None with no active selection range). A
    // double-click snaps to the word; drag-extend off finalizes it cleanly (the
    // deterministic pattern from selection_extend.rs).
    app.set_selection_drag_extend_for_test(false);
    for _ in 0..2 {
        app.set_pointer_cell_for_test(0, 2);
        app.begin_selection_for_test();
        app.finish_selection_for_test();
    }
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("hello"),
        "precondition: a selection exists before poisoning"
    );

    // Poison the shared terminal lock out-of-band (as a hot-path panic holding
    // the guard would).
    with_silent_panic_hook(|| {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _g = handle.lock().expect("poisoning lock");
            panic!("poison the terminal lock");
        }));
    });
    assert!(handle.lock().is_err(), "the terminal lock is now poisoned");

    // The copy / PRIMARY choke point must RETURN through poison recovery, not
    // abort. catch_unwind proves no panic crosses the boundary.
    let result = catch_unwind(AssertUnwindSafe(|| app.selection_text_for_test()));
    assert!(
        result.is_ok(),
        "current_selection_text must not panic on a poisoned lock"
    );
    assert_eq!(
        result.unwrap().as_deref(),
        Some("hello"),
        "poison-recovered read still returns the selected text"
    );
}
