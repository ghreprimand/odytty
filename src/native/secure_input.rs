// SPDX-License-Identifier: GPL-3.0-only
//! Secure keyboard entry.
//!
//! The mode exists only where the OS has a real primitive. It is never
//! simulated. On macOS the primitive is `EnableSecureEventInput` /
//! `DisableSecureEventInput` (HIToolbox), process-wide, called on the main
//! thread. A process counter pairs the calls: the enable runs when the first
//! focused window that wants the mode acquires it, and the disable runs when
//! the last such hold is released. A disable at zero does nothing. Windows
//! and Linux have no equivalent, so `set_secure_input` does not flip the
//! test-visible OS flag there.
//!
//! The setting is the process-wide wish. The primitive is enabled only while
//! that wish is on and at least one OdyTTY window has keyboard focus. While
//! it is enabled, keyboard-intercept tools (event taps such as text
//! expanders, and some accessibility and automation tools) do not receive
//! keystrokes. Other apps still receive keys. A crash while it is enabled
//! can leave secure input reported as active, and those tools blocked, until
//! logout. Normal typing in other apps is unaffected. The mode never turns
//! on because a program printed a password prompt, and it does not change
//! bytes written to the PTY.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
use std::cell::Cell;

/// Label painted on the focused window while this process holds secure input.
pub(in crate::native) const SECURE_INPUT_LABEL: &str = "SECURE INPUT";

#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput();
    fn DisableSecureEventInput();
}

/// Set only by the macOS production primitive. Stays false when that primitive
/// is not called, including every Windows and Linux call.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
static OS_ENABLED: AtomicBool = AtomicBool::new(false);

struct SecureInputState {
    holders: usize,
}

static STATE: Mutex<SecureInputState> = Mutex::new(SecureInputState { holders: 0 });

/// The user's wish, shared by every window. Distinct from the OS hold: a
/// window applies the primitive only when this is set and that window has
/// keyboard focus.
static WISH: AtomicBool = AtomicBool::new(false);

/// Set by the first window in the process, from its settings. Later windows
/// adopt [`WISH`] and must not overwrite it with a freshly loaded config.
static WISH_ESTABLISHED: AtomicBool = AtomicBool::new(false);

/// Serializes every write of [`WISH`] and [`WISH_ESTABLISHED`], so the first
/// binder publishes its wish before any later binder can read it, and a
/// reload write never interleaves with a first bind.
static WISH_WRITE: Mutex<()> = Mutex::new(());

fn lock_wish_write() -> std::sync::MutexGuard<'static, ()> {
    WISH_WRITE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
thread_local! {
    static TEST_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Holds the process-wide secure-input test lock. Re-entrant on the holding
/// thread so a test can keep it across `App` construction. Other threads
/// block, which keeps parallel tests from sharing the wish, the holder
/// counter, and the test primitive.
#[cfg(test)]
pub(crate) struct SecureInputTestGuard {
    locked: Option<std::sync::MutexGuard<'static, ()>>,
}

#[cfg(test)]
pub(crate) fn secure_input_test_guard() -> SecureInputTestGuard {
    TEST_LOCK_DEPTH.with(|depth| {
        let current = depth.get();
        if current == 0 {
            let locked = TEST_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            depth.set(1);
            SecureInputTestGuard {
                locked: Some(locked),
            }
        } else {
            depth.set(current.saturating_add(1));
            SecureInputTestGuard { locked: None }
        }
    })
}

#[cfg(test)]
impl Drop for SecureInputTestGuard {
    fn drop(&mut self) {
        TEST_LOCK_DEPTH.with(|depth| {
            depth.set(depth.get().saturating_sub(1));
        });
        // The field exists to hold the mutex until the outermost guard
        // drops. Take it so the unlock is explicit and the field is read.
        drop(self.locked.take());
    }
}

/// Linux and Windows tests opt in to the hold decision so the focus contract
/// runs without a Mac. Production builds on those platforms leave this false,
/// and the macOS build ignores it.
#[cfg(all(test, not(target_os = "macos")))]
static TEST_APPLY: AtomicBool = AtomicBool::new(false);

/// Whether this process should acquire holds. Always true on macOS. Elsewhere
/// only a test that opts in, so production Windows and Linux stay inert.
pub(crate) fn secure_input_applies() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(all(test, not(target_os = "macos")))]
    {
        TEST_APPLY.load(Ordering::SeqCst)
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        false
    }
}

static FFI: Mutex<fn(bool)> = Mutex::new(production_ffi);

fn production_ffi(enable: bool) {
    #[cfg(target_os = "macos")]
    {
        // Main-thread contract: callers are the winit event loop, window
        // close, and process drop. The HIToolbox calls are not synchronized.
        unsafe {
            if enable {
                EnableSecureEventInput();
            } else {
                DisableSecureEventInput();
            }
        }
        OS_ENABLED.store(enable, Ordering::SeqCst);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = enable;
    }
}

fn lock_state() -> std::sync::MutexGuard<'static, SecureInputState> {
    STATE.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn current_ffi() -> fn(bool) {
    *FFI.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// One window acquires (`enabled`) or releases the process-wide hold.
///
/// The OS primitive runs only on the 0->1 and 1->0 transitions. A release
/// when nothing is held does not call it.
pub fn set_secure_input(enabled: bool) {
    #[cfg(test)]
    let _lock = secure_input_test_guard();
    let ffi = current_ffi();
    // The primitive runs under the state lock, so concurrent acquire and
    // release calls reach the OS in the same order as their transitions.
    let mut state = lock_state();
    let transition = if enabled {
        let was_zero = state.holders == 0;
        state.holders = state.holders.saturating_add(1);
        was_zero
    } else if state.holders == 0 {
        false
    } else {
        state.holders -= 1;
        state.holders == 0
    };
    if transition {
        ffi(enabled);
    }
    drop(state);
}

/// Whether the macOS primitive is currently enabled. False on Windows and
/// Linux, and false in tests that replace the primitive.
#[cfg_attr(not(test), allow(dead_code))]
pub fn secure_input_os_enabled() -> bool {
    OS_ENABLED.load(Ordering::SeqCst)
}

/// The process-wide wish. Every window reads this; a palette toggle or a
/// config reload writes it once.
pub(crate) fn secure_keyboard_wish() -> bool {
    WISH.load(Ordering::SeqCst)
}

pub(crate) fn set_secure_keyboard_wish(enabled: bool) {
    #[cfg(test)]
    let _lock = secure_input_test_guard();
    let _write = lock_wish_write();
    WISH.store(enabled, Ordering::SeqCst);
    WISH_ESTABLISHED.store(true, Ordering::SeqCst);
}

/// Bind one window to the process wish.
///
/// The first window in the process establishes the wish from its own
/// settings (process start, including a config that is off). A window
/// created after that adopts the wish already in force and returns it, so
/// a sibling built from the on-disk config cannot clear a wish the user
/// turned on. Config reload writes the wish through
/// [`set_secure_keyboard_wish`], not through this function.
pub(crate) fn bind_window_secure_keyboard_wish(from_settings: bool) -> bool {
    #[cfg(test)]
    let _lock = secure_input_test_guard();
    // Establishment and the wish it publishes happen under one lock, so a
    // concurrent second binder cannot see the flag set before the wish is.
    let _write = lock_wish_write();
    if WISH_ESTABLISHED.load(Ordering::SeqCst) {
        WISH.load(Ordering::SeqCst)
    } else {
        WISH.store(from_settings, Ordering::SeqCst);
        WISH_ESTABLISHED.store(true, Ordering::SeqCst);
        from_settings
    }
}

#[cfg(test)]
pub(crate) fn secure_input_holders_for_test() -> usize {
    lock_state().holders
}

#[cfg(test)]
pub(crate) fn reset_secure_input_for_test() {
    let _lock = secure_input_test_guard();
    lock_state().holders = 0;
    OS_ENABLED.store(false, Ordering::SeqCst);
    WISH.store(false, Ordering::SeqCst);
    WISH_ESTABLISHED.store(false, Ordering::SeqCst);
    #[cfg(not(target_os = "macos"))]
    TEST_APPLY.store(false, Ordering::SeqCst);
    *FFI.lock().unwrap_or_else(|poison| poison.into_inner()) = production_ffi;
}

#[cfg(test)]
pub(crate) fn force_secure_input_apply_for_test(apply: bool) {
    let _lock = secure_input_test_guard();
    #[cfg(not(target_os = "macos"))]
    TEST_APPLY.store(apply, Ordering::SeqCst);
    #[cfg(target_os = "macos")]
    let _ = apply;
}

#[cfg(test)]
pub(crate) fn install_secure_input_ffi_for_test(ffi: fn(bool)) {
    let _lock = secure_input_test_guard();
    *FFI.lock().unwrap_or_else(|poison| poison.into_inner()) = ffi;
}
