// SPDX-License-Identifier: GPL-3.0-only
//! macOS global-shortcut backend for the quick terminal (v0.15.0 A).
//!
//! Split out of `quick_terminal` so the Carbon FFI lives in one platform
//! module. Compiled only on macOS (the `#[cfg(target_os = "macos")]` on its
//! `mod` declaration). The macOS CI leg provides its platform build check.

/// The macOS global-shortcut backend. Carbon `RegisterEventHotKey` reserves the
/// combo system-wide and an installed application-event-target handler fires on
/// the main run loop (the same thread winit runs), where it invokes the summon
/// sink. Registration is confirmed by the `RegisterEventHotKey` `OSStatus`
/// before `register` returns, so it never reports a false `Registered`. This
/// module is compiled only on macOS; its CI leg provides the platform build
/// check.
#[cfg(target_os = "macos")]
pub(super) mod macos_grab {
    use super::super::{
        Accelerator, GlobalShortcutAdapter, ShortcutRegistration, SummonSink, macos_keycode,
        macos_registration_confirmed, macos_registration_failure_notice,
    };
    use std::os::raw::c_void;

    type OSStatus = i32;
    type EventTargetRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;
    type EventHandlerRef = *mut c_void;
    type EventHandlerCallRef = *mut c_void;
    type EventRef = *mut c_void;
    type OSType = u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EventHotKeyID {
        signature: OSType,
        id: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EventTypeSpec {
        event_class: OSType,
        event_kind: u32,
    }

    // 'keyb' (kEventClassKeyboard) and kEventHotKeyPressed.
    const EVENT_CLASS_KEYBOARD: OSType = 0x6B65_7962;
    const EVENT_HOTKEY_PRESSED: u32 = 5;
    const NO_ERR: OSStatus = 0;

    // Carbon modifier bits (Events.h): cmd/shift/option/control.
    const CMD_KEY: u32 = 0x0100;
    const SHIFT_KEY: u32 = 0x0200;
    const OPTION_KEY: u32 = 0x0800;
    const CONTROL_KEY: u32 = 0x1000;

    type EventHandlerProcPtr =
        unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> EventTargetRef;
        fn RegisterEventHotKey(
            in_hot_key_code: u32,
            in_hot_key_modifiers: u32,
            in_hot_key_id: EventHotKeyID,
            in_target: EventTargetRef,
            in_options: u32,
            out_ref: *mut EventHotKeyRef,
        ) -> OSStatus;
        fn UnregisterEventHotKey(in_hot_key: EventHotKeyRef) -> OSStatus;
        fn InstallEventHandler(
            in_target: EventTargetRef,
            in_handler: EventHandlerProcPtr,
            in_num_types: u32,
            in_list: *const EventTypeSpec,
            in_user_data: *mut c_void,
            out_ref: *mut EventHandlerRef,
        ) -> OSStatus;
        fn RemoveEventHandler(in_handler_ref: EventHandlerRef) -> OSStatus;
    }

    /// The Carbon C callback. `user` points to the heap-stable boxed sink the
    /// adapter keeps alive; it fires on the main run loop.
    unsafe extern "C" fn hotkey_handler(
        _call: EventHandlerCallRef,
        _event: EventRef,
        user: *mut c_void,
    ) -> OSStatus {
        if !user.is_null() {
            // SAFETY: `user` is the pointer to the adapter's boxed sink, which
            // outlives the handler (removed before the box is dropped).
            let sink = unsafe { &*(user as *const SummonSink) };
            sink();
        }
        NO_ERR
    }

    fn carbon_modifiers(acc: &Accelerator) -> u32 {
        let mut mods = 0;
        if acc.meta {
            mods |= CMD_KEY;
        }
        if acc.shift {
            mods |= SHIFT_KEY;
        }
        if acc.alt {
            mods |= OPTION_KEY;
        }
        if acc.ctrl {
            mods |= CONTROL_KEY;
        }
        mods
    }

    pub(in crate::native) struct MacosShortcutAdapter {
        hotkey: EventHotKeyRef,
        handler: EventHandlerRef,
        // Boxed so its address is stable for the duration it is registered as
        // the handler's user data. Dropped only after the handler is removed.
        sink: Option<Box<SummonSink>>,
    }

    // SAFETY: the raw refs are only used on the main thread (register/unregister
    // and the run-loop callback all run there); the adapter is owned by the
    // main-thread host.
    unsafe impl Send for MacosShortcutAdapter {}

    impl MacosShortcutAdapter {
        pub(in crate::native) fn new() -> Self {
            Self {
                hotkey: std::ptr::null_mut(),
                handler: std::ptr::null_mut(),
                sink: None,
            }
        }

        fn teardown(&mut self) {
            // Remove the handler FIRST so the callback can never fire after the
            // sink box is freed, then unregister the hot key.
            unsafe {
                if !self.handler.is_null() {
                    let _ = RemoveEventHandler(self.handler);
                    self.handler = std::ptr::null_mut();
                }
                if !self.hotkey.is_null() {
                    let _ = UnregisterEventHotKey(self.hotkey);
                    self.hotkey = std::ptr::null_mut();
                }
            }
            self.sink = None;
        }
    }

    impl Drop for MacosShortcutAdapter {
        fn drop(&mut self) {
            self.teardown();
        }
    }

    impl GlobalShortcutAdapter for MacosShortcutAdapter {
        fn register(
            &mut self,
            accelerator: &Accelerator,
            sink: SummonSink,
        ) -> ShortcutRegistration {
            self.teardown();
            let Some(code) = macos_keycode(&accelerator.key) else {
                tracing::warn!(
                    key = %accelerator.key,
                    "quick terminal key has no macOS virtual keycode"
                );
                return ShortcutRegistration::Unavailable {
                    reason: macos_registration_failure_notice(accelerator),
                };
            };
            let mods = carbon_modifiers(accelerator);
            let boxed: Box<SummonSink> = Box::new(sink);
            let user = std::ptr::addr_of!(*boxed) as *mut c_void;

            // SAFETY: standard Carbon registration on the application event
            // target; every out-pointer is checked and torn down on failure.
            unsafe {
                let target = GetApplicationEventTarget();
                let spec = EventTypeSpec {
                    event_class: EVENT_CLASS_KEYBOARD,
                    event_kind: EVENT_HOTKEY_PRESSED,
                };
                let mut handler: EventHandlerRef = std::ptr::null_mut();
                let status =
                    InstallEventHandler(target, hotkey_handler, 1, &spec, user, &mut handler);
                if !macos_registration_confirmed(status) {
                    tracing::warn!(
                        status,
                        "macOS could not install the quick terminal hotkey handler"
                    );
                    return ShortcutRegistration::Unavailable {
                        reason: macos_registration_failure_notice(accelerator),
                    };
                }
                let hotkey_id = EventHotKeyID {
                    signature: EVENT_CLASS_KEYBOARD,
                    id: 1,
                };
                let mut hotkey: EventHotKeyRef = std::ptr::null_mut();
                let status = RegisterEventHotKey(code, mods, hotkey_id, target, 0, &mut hotkey);
                if !macos_registration_confirmed(status) {
                    let _ = RemoveEventHandler(handler);
                    tracing::warn!(
                        status,
                        "macOS RegisterEventHotKey rejected the quick terminal shortcut"
                    );
                    return ShortcutRegistration::Unavailable {
                        reason: macos_registration_failure_notice(accelerator),
                    };
                }
                self.handler = handler;
                self.hotkey = hotkey;
                self.sink = Some(boxed);
            }
            ShortcutRegistration::Registered {
                backend: "macos-registereventhotkey",
            }
        }

        fn unregister(&mut self) {
            self.teardown();
        }
    }
}
