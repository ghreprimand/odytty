// SPDX-License-Identifier: GPL-3.0-only
//! Small front-end helper for acquiring the local hostname once and injecting it
//! into the terminal core. The core remains syscall-free and deterministic.

#[cfg(unix)]
const HOSTNAME_BUF_LEN: usize = 256;

/// Return the local hostname reported by the operating system, or `None` when
/// it cannot be read as a complete non-empty UTF-8 string.
pub(crate) fn get() -> Option<String> {
    get_impl()
}

#[cfg(unix)]
fn get_impl() -> Option<String> {
    let mut buf = [0_u8; HOSTNAME_BUF_LEN];
    let rc =
        unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), HOSTNAME_BUF_LEN) };
    if rc != 0 {
        return None;
    }

    let len = buf.iter().position(|&byte| byte == 0)?;
    if len == 0 {
        return None;
    }
    std::str::from_utf8(&buf[..len]).ok().map(str::to_owned)
}

/// Windows: read the primary hostname via `GetComputerNameExW`. Returning the
/// name here lets [`crate::core::screen::osc`] accept an OSC 7 `file://<host>/…`
/// URL whose authority is this machine's name (the form oh-my-posh / starship
/// and other third-party prompts emit) instead of dropping it, so cwd tracking
/// keeps working under those prompts. `osc7_host_is_local` compares leading
/// labels case-insensitively, so either the DNS hostname or the NetBIOS
/// `%COMPUTERNAME%` matches.
#[cfg(windows)]
fn get_impl() -> Option<String> {
    use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    use windows::core::PWSTR;

    // Documented two-call pattern: the first call with a null buffer reports
    // the required length (in WCHARs, including the terminating NUL) via
    // `size`.
    let mut size: u32 = 0;
    // SAFETY: a null buffer with `size` = 0 asks only for the needed length;
    // the API writes it into `size` and returns an error we ignore here.
    unsafe {
        let _ = GetComputerNameExW(ComputerNameDnsHostname, None, &mut size);
    }
    if size == 0 {
        return None;
    }
    let mut buf = vec![0_u16; size as usize];
    // SAFETY: `buf` holds `size` WCHARs; on success `size` is updated to the
    // count actually written (excluding the NUL).
    let ok = unsafe {
        GetComputerNameExW(
            ComputerNameDnsHostname,
            Some(PWSTR(buf.as_mut_ptr())),
            &mut size,
        )
        .is_ok()
    };
    if !ok {
        return None;
    }
    let name = String::from_utf16_lossy(&buf[..size as usize]);
    if name.trim().is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(not(any(unix, windows)))]
fn get_impl() -> Option<String> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn hostname_helper_returns_non_empty_host() {
        let host = get().expect("local hostname");
        assert!(!host.trim().is_empty());
        assert!(!host.as_bytes().contains(&0));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn hostname_helper_returns_non_empty_host_on_windows() {
        // D-11: the Windows arm must resolve a real, non-empty, NUL-free host
        // name so OSC 7 URLs whose authority is `%COMPUTERNAME%` are accepted
        // as local. Runs on the windows-latest CI leg.
        let host = get().expect("local hostname");
        assert!(!host.trim().is_empty());
        assert!(!host.as_bytes().contains(&0));
    }
}
