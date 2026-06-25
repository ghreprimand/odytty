// SPDX-License-Identifier: GPL-3.0-only
//! Small front-end helper for acquiring the local hostname once and injecting it
//! into the terminal core. The core remains syscall-free and deterministic.

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

#[cfg(not(unix))]
fn get_impl() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn hostname_helper_returns_non_empty_host() {
        let host = get().expect("local hostname");
        assert!(!host.trim().is_empty());
        assert!(!host.as_bytes().contains(&0));
    }
}
