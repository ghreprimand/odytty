// SPDX-License-Identifier: GPL-3.0-only
//! Read the global AppleLocale preference without spawning another process.
use std::ffi::{CStr, c_char, c_void};

const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFPreferencesAnyApplication: *const c_void;
    static kCFPreferencesCurrentUser: *const c_void;
    static kCFPreferencesAnyHost: *const c_void;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        string: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFPreferencesCopyValue(
        key: *const c_void,
        application: *const c_void,
        user: *const c_void,
        host: *const c_void,
    ) -> *const c_void;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(value: *const c_void);
}

struct Owned(*const c_void);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Owned holds only a Create/Copy result, released once.
            unsafe { CFRelease(self.0) };
        }
    }
}

pub(super) fn apple_locale() -> Option<String> {
    // SAFETY: the key is a static NUL-terminated C string, all preference
    // domain constants are supplied by CoreFoundation, and Copy/Create values
    // stay alive through their uses. The preference is checked for CFString
    // type before conversion into a fixed, correctly sized byte buffer.
    unsafe {
        let key = Owned(CFStringCreateWithCString(
            std::ptr::null(),
            c"AppleLocale".as_ptr(),
            UTF8,
        ));
        if key.0.is_null() {
            return None;
        }
        let value = Owned(CFPreferencesCopyValue(
            key.0,
            kCFPreferencesAnyApplication,
            kCFPreferencesCurrentUser,
            kCFPreferencesAnyHost,
        ));
        if value.0.is_null() || CFGetTypeID(value.0) != CFStringGetTypeID() {
            return None;
        }
        let mut bytes = [0_u8; 129];
        if CFStringGetCString(
            value.0,
            bytes.as_mut_ptr().cast(),
            bytes.len() as isize,
            UTF8,
        ) == 0
        {
            return None;
        }
        CStr::from_bytes_until_nul(&bytes)
            .ok()?
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}
