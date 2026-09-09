// SPDX-License-Identifier: GPL-3.0-only
//! Windows global-shortcut backend for the quick terminal (v0.15.0 A).
//!
//! Split out of `quick_terminal` so the Win32 FFI lives in one platform module.
//! Compiled only on Windows (the `#[cfg(target_os = "windows")]` on its `mod`
//! declaration). The Windows CI leg provides its platform build check.

/// The Windows global-shortcut backend. `RegisterHotKey` with a null window
/// posts `WM_HOTKEY` to a dedicated thread's message queue; that thread owns the
/// registration and its message loop, and invokes the summon sink on each press.
/// Registration is confirmed by the `RegisterHotKey` return value relayed back
/// over a channel before `register` returns, so a combo another app already owns
/// is reported as a failure rather than a false `Registered`. This module is
/// compiled only on Windows; its CI leg provides the platform build check.
#[cfg(target_os = "windows")]
pub(super) mod windows_grab {
    use super::super::{
        Accelerator, GlobalShortcutAdapter, ShortcutRegistration, SummonSink,
        windows_conflict_notice, windows_hotkey_already_registered,
        windows_registration_failure_notice, windows_vk,
    };
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
        UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
    };

    /// A per-thread hot-key id. Unique within the dedicated thread's queue.
    const HOTKEY_ID: i32 = 0x0DB1;

    pub(in crate::native) struct WindowsShortcutAdapter {
        thread: Option<(u32, JoinHandle<()>)>,
    }

    impl WindowsShortcutAdapter {
        pub(in crate::native) fn new() -> Self {
            Self { thread: None }
        }

        fn stop(&mut self) {
            if let Some((tid, handle)) = self.thread.take() {
                // Break the message loop; the thread unregisters and exits.
                // SAFETY: posting WM_QUIT to a live thread id we started. The
                // thread forced its message queue into existence before it
                // confirmed registration, so this post always lands (a lost
                // WM_QUIT would hang the join below). If it still fails the
                // thread is already tearing down, so the join completes anyway.
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                let _ = handle.join();
            }
        }
    }

    impl Drop for WindowsShortcutAdapter {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn modifiers(acc: &Accelerator) -> HOT_KEY_MODIFIERS {
        let mut mods = MOD_NOREPEAT;
        if acc.alt {
            mods |= MOD_ALT;
        }
        if acc.ctrl {
            mods |= MOD_CONTROL;
        }
        if acc.shift {
            mods |= MOD_SHIFT;
        }
        if acc.meta {
            mods |= MOD_WIN;
        }
        mods
    }

    impl GlobalShortcutAdapter for WindowsShortcutAdapter {
        fn register(
            &mut self,
            accelerator: &Accelerator,
            sink: SummonSink,
        ) -> ShortcutRegistration {
            self.stop();
            let Some(vk) = windows_vk(&accelerator.key) else {
                tracing::warn!(
                    key = %accelerator.key,
                    "quick terminal key has no Windows virtual keycode"
                );
                return ShortcutRegistration::Unavailable {
                    reason: windows_registration_failure_notice(accelerator),
                };
            };
            let mods = modifiers(accelerator);
            let vk = u32::from(vk);
            let conflict_notice = windows_conflict_notice(accelerator);
            let failure_notice = windows_registration_failure_notice(accelerator);
            let thread_failure_notice = failure_notice.clone();
            let (tx, rx) = mpsc::channel::<Result<u32, String>>();
            let spawned = std::thread::Builder::new()
                .name("odytty-win-hotkey".to_owned())
                .spawn(move || {
                    // SAFETY: RegisterHotKey with a null HWND posts WM_HOTKEY to
                    // THIS thread's queue; GetMessageW pumps the same thread.
                    unsafe {
                        let tid = GetCurrentThreadId();
                        // Force this thread's message queue into existence
                        // BEFORE confirming registration. A thread has no queue
                        // until it first calls a message function; a teardown
                        // PostThreadMessageW(WM_QUIT) sent before the queue
                        // exists is silently dropped, which would hang the join.
                        // PeekMessageW with PM_NOREMOVE creates the queue
                        // without consuming or dispatching anything.
                        let mut probe = MSG::default();
                        let _ = PeekMessageW(&mut probe, None, 0, 0, PM_NOREMOVE);
                        if let Err(err) = RegisterHotKey(None, HOTKEY_ID, mods, vk) {
                            let conflict = windows_hotkey_already_registered(err.code().0);
                            tracing::warn!(
                                error = %err,
                                conflict,
                                "Windows RegisterHotKey rejected the quick terminal shortcut"
                            );
                            let reason = if conflict {
                                conflict_notice.clone()
                            } else {
                                thread_failure_notice.clone()
                            };
                            let _ = tx.send(Err(reason));
                            return;
                        }
                        if tx.send(Ok(tid)).is_err() {
                            let _ = UnregisterHotKey(None, HOTKEY_ID);
                            return;
                        }
                        let mut msg = MSG::default();
                        loop {
                            // GetMessageW returns >0 for a delivered message, 0
                            // for WM_QUIT, and -1 for an error. `.as_bool()`
                            // treats -1 as `true`, which would spin forever on
                            // error; inspect the raw BOOL and handle the three
                            // cases distinctly.
                            let ret = GetMessageW(&mut msg, None, 0, 0).0;
                            if ret == -1 || ret == 0 {
                                // -1: message-queue error; 0: WM_QUIT (teardown).
                                break;
                            }
                            if msg.message == WM_HOTKEY {
                                sink();
                            }
                        }
                        let _ = UnregisterHotKey(None, HOTKEY_ID);
                    }
                });
            let handle = match spawned {
                Ok(handle) => handle,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "cannot spawn Windows quick terminal hotkey thread"
                    );
                    return ShortcutRegistration::Unavailable {
                        reason: failure_notice,
                    };
                }
            };
            match rx.recv() {
                Ok(Ok(tid)) => {
                    self.thread = Some((tid, handle));
                    ShortcutRegistration::Registered {
                        backend: "windows-registerhotkey",
                    }
                }
                Ok(Err(reason)) => {
                    let _ = handle.join();
                    ShortcutRegistration::Unavailable { reason }
                }
                Err(_) => {
                    let _ = handle.join();
                    ShortcutRegistration::Unavailable {
                        reason: failure_notice,
                    }
                }
            }
        }

        fn unregister(&mut self) {
            self.stop();
        }
    }
}
