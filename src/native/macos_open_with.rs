// SPDX-License-Identifier: GPL-3.0-only
//! macOS-native "Open With…" enumeration (Phase 17).
//!
//! The Linux enumeration in `crate::desktop::enumerate_open_with` is 100%
//! freedesktop (`.desktop` / `mimeapps.list` / `mimeinfo.cache`); none of those
//! exist on macOS, so on a Mac that path is structurally always empty and the
//! picker reported "No applications found" even though Ctrl+click open worked.
//!
//! This module is the macOS arm: it asks `NSWorkspace` for the applications
//! that can open the file's URL (already UTI-appropriate, so no separate MIME
//! query is needed) and hands the resulting bundle paths to the pure
//! [`crate::desktop::map_macos_app_paths`] mapper, which turns them into the
//! same `DesktopApp { id, name, argv }` rows the Linux path produces.
//!
//! All FFI is confined here behind `#[cfg(target_os = "macos")]` (set in
//! `native/mod.rs`); every bit of testable logic lives in the pure mapper that
//! compiles and unit-tests on Linux.

use crate::desktop::{DesktopApp, map_macos_app_paths};

use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

/// Enumerate the applications that can open `file_abs` via `NSWorkspace`,
/// returning picker rows. Never panics: an empty LaunchServices result yields
/// an empty vec (the overlay shows its empty-state hint).
pub(in crate::native) fn enumerate(file_abs: &str) -> Vec<DesktopApp> {
    map_macos_app_paths(collect_app_paths(file_abs), file_abs)
}

/// Collect the bundle paths (`/Applications/Foo.app`) of every application
/// LaunchServices reports as able to open `file_abs`, in preference order.
///
/// `URLsForApplicationsToOpenURL:` is macOS 12+ (fine for macOS 26). Each
/// returned `NSURL`'s filesystem path is collected; URLs without a path are
/// skipped. objc2's `Retained<T>` handles the Objective-C memory management.
fn collect_app_paths(file_abs: &str) -> Vec<String> {
    let ns_path = NSString::from_str(file_abs);
    // SAFETY: standard Cocoa calls with valid, owned arguments. `fileURLWithPath`
    // takes an NSString and returns an owned NSURL; `sharedWorkspace` is the
    // process-wide singleton; `URLsForApplicationsToOpenURL` returns an owned
    // array; `path` returns an optional owned NSString. No raw pointers escape.
    unsafe {
        let url = NSURL::fileURLWithPath(&ns_path);
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.URLsForApplicationsToOpenURL(&url);
        let mut paths = Vec::new();
        for app_url in apps.iter() {
            if let Some(path) = app_url.path() {
                paths.push(path.to_string());
            }
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke: enumeration runs against a real path on the macOS CI runner
    /// without panicking. The specific app set is runner-dependent, so this
    /// asserts only that a `Vec` comes back (it may be empty or non-empty).
    #[test]
    fn enumerate_does_not_panic_on_real_path() {
        let apps = enumerate("/etc/hosts");
        // Touch the result so the call is not optimized away; no count assert.
        let _ = apps.len();
    }
}
