// SPDX-License-Identifier: GPL-3.0-only
//! WIN-SPAWN — shared child-process creation-flag helper (C13).
//!
//! OdyTTY ships as a GUI-subsystem binary on Windows (no attached console).
//! When such a process spawns a *console* child (`ssh.exe`, an old-style opener,
//! a console editor) with no creation flags, Windows allocates a fresh console
//! for the child and flashes a black console window on screen — brief for a
//! one-shot probe, several seconds for an upload that streams over `ssh`.
//!
//! [`apply_no_console_window`] centralises the fix: it sets `CREATE_NO_WINDOW`
//! (`0x0800_0000`) on the [`Command`] so a console child runs windowless. The
//! flag only affects console applications; for a GUI app (`explorer.exe`) it is
//! a harmless no-op, so the same helper can guard every spawn site uniformly
//! without special-casing the GUI launcher. On non-Windows targets the helper
//! compiles to nothing and the `Command` is returned unchanged.

use std::process::Command;

/// The `CREATE_NO_WINDOW` process creation flag: run a console child without
/// allocating a console window. Mirrors `windows_sys`/winbase; hard-coded here
/// so the helper needs no extra dependency (`std`'s `creation_flags` takes the
/// raw `u32`).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Suppress the console window a console child would otherwise flash.
///
/// Windows: sets `CREATE_NO_WINDOW` on `cmd` via
/// [`std::os::windows::process::CommandExt::creation_flags`]. Applied at every
/// site where the GUI-subsystem binary spawns a child that might be a console
/// app (`ssh.exe` probes/uploads, the default-open launcher). A GUI child
/// ignores the flag, so callers need not distinguish console from GUI targets.
///
/// Non-Windows: a no-op — there is no console-window concept, and
/// `creation_flags` is a Windows-only extension trait.
#[cfg(windows)]
pub(in crate::native) fn apply_no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Non-Windows no-op form. Present so call sites stay `cfg`-free.
#[cfg(not(windows))]
pub(in crate::native) fn apply_no_console_window(_cmd: &mut Command) {}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn create_no_window_constant_matches_winbase() {
        // Guard against a typo in the hard-coded flag: CREATE_NO_WINDOW is
        // 0x08000000 in winbase.h.
        assert_eq!(super::CREATE_NO_WINDOW, 0x0800_0000);
    }
}
