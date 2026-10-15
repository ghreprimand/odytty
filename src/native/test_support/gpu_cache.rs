// SPDX-License-Identifier: GPL-3.0-only
//! Keeps GPU driver shader caches out of the user's real cache directories.
//!
//! A driver chooses its cache location when it initialises, which for the
//! headless fixtures is the first `wgpu::Instance` of the process, and it keeps
//! writing there for the rest of the run. Redirecting a platform directory only
//! while a fixture holds the environment lock (as the config fixture does)
//! would therefore miss later writes, so the redirect is set once, before the
//! first instance, and stays for the process.
//!
//! Only the driver variables are redirected. `XDG_CACHE_HOME` is left alone
//! because fontconfig and other readers also use it.
//!
//! Linux honours these variables: Mesa (llvmpipe, lavapipe, radv, anv, and the
//! other Mesa drivers) reads `MESA_SHADER_CACHE_DIR`, and the NVIDIA driver
//! (OpenGL and Vulkan) reads `__GL_SHADER_DISK_CACHE_PATH`, which must name an
//! existing directory. Windows and macOS drivers keep their caches in places the
//! environment does not control: `%LOCALAPPDATA%\D3DSCache` and the NVIDIA
//! `DXCache` and `GLCache` folders on Windows, and the Metal compiler cache
//! under the per-user cache directory (`getconf DARWIN_USER_CACHE_DIR`) on
//! macOS. The NVIDIA variable still applies to NVIDIA's OpenGL cache on Windows.
//! `tests/check_temp_dir_leaks.py` reports writes to those locations.

use std::sync::Once;

/// Driver variables, each pointed at the one directory this process owns.
const REDIRECTED: [&str; 2] = ["MESA_SHADER_CACHE_DIR", "__GL_SHADER_DISK_CACHE_PATH"];

/// Point the driver cache variables at a directory inside this test process's
/// own base. Idempotent; call it before creating any `wgpu::Instance`.
pub(in crate::native) fn isolate_driver_caches() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = crate::settings::test_process_base().join("gpu-cache");
        std::fs::create_dir_all(&dir).expect("create the test GPU cache directory");
        // A thread that already holds the (non-reentrant) environment lock
        // must not take it again.
        let _env = (!crate::test_lock::current_thread_holds_env_lock())
            .then(crate::test_lock::test_env_lock);
        for key in REDIRECTED {
            // SAFETY: the environment lock is held, or the caller already
            // holds it; this runs once, before any driver has initialised.
            unsafe { std::env::set_var(key, &dir) };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_cache_variables_name_a_directory_inside_the_process_base() {
        isolate_driver_caches();
        for key in REDIRECTED {
            let value = std::env::var_os(key).expect("redirected variable is set");
            let path = std::path::Path::new(&value);
            assert!(path.is_dir(), "{key} names an existing directory");
            assert!(
                path.starts_with(crate::settings::test_process_base()),
                "{key} stays inside the test process base"
            );
        }
    }
}
