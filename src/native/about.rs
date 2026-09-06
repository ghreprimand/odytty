// SPDX-License-Identifier: GPL-3.0-only
//! ABOUT: read-only application/build/renderer facts for the in-app About view.
//!
//! Aggregates compile-time constants (version, license, links), build-time
//! provenance emitted by `build.rs` (git SHA, build date, target triple, rustc
//! version), the runtime display server, and — once the GPU is up — the active
//! adapter diagnostics. Presentation-only: the settings panel renders these
//! fields; this module owns no UI.
//!
//! The `diagnostics_block()` clipboard text deliberately OMITS filesystem paths
//! (config/log dirs contain `$HOME`/username) so a user can paste it into a bug
//! report without leaking their account name. No filesystem path is displayed
//! anywhere in the About view either -- neither `info_lines()` nor
//! `diagnostics_block()` formats one -- so there is no on-screen path surface to
//! guard.

use super::gpu::AdapterDiagnostics;

/// A clickable project link shown in the About view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AboutLink {
    pub(super) label: &'static str,
    pub(super) url: &'static str,
}

/// The three canonical project links (hardcoded; the only openable set).
pub(super) const ABOUT_LINKS: &[AboutLink] = &[
    AboutLink {
        label: "Homepage",
        url: "https://odytty.unfinished-works.com",
    },
    AboutLink {
        label: "Repository",
        url: "https://github.com/ghreprimand/odytty",
    },
    AboutLink {
        label: "Issues",
        url: "https://github.com/ghreprimand/odytty/issues",
    },
];

/// Read-only snapshot of application + build + renderer facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AboutInfo {
    pub(super) name: &'static str,
    pub(super) version: &'static str,
    pub(super) license: &'static str,
    pub(super) git_sha: &'static str,
    pub(super) build_date: &'static str,
    pub(super) target: &'static str,
    pub(super) rustc_version: &'static str,
    /// "Wayland", "X11", or "unknown" — detected from the environment.
    pub(super) display_server: &'static str,
    /// Active GPU adapter. `None` until the renderer is up (headless/early).
    pub(super) adapter: Option<AdapterDiagnostics>,
}

impl AboutInfo {
    /// Build from compile-time constants + `build.rs` env + the (optional)
    /// active adapter. `adapter` is `None` when the GPU is not yet initialized.
    pub(super) fn collect(adapter: Option<AdapterDiagnostics>) -> Self {
        Self {
            name: "OdyTTY",
            version: env!("CARGO_PKG_VERSION"),
            license: "GPL-3.0-only",
            git_sha: env!("ODYTTY_GIT_SHA"),
            build_date: env!("ODYTTY_BUILD_DATE"),
            target: env!("ODYTTY_TARGET"),
            rustc_version: env!("ODYTTY_RUSTC_VERSION"),
            display_server: detect_display_server(),
            adapter,
        }
    }

    /// Plaintext block for the "Copy diagnostics" button: version, build, and
    /// renderer facts a bug report needs. Excludes filesystem paths by design
    /// (no `$HOME`/username leakage). Stable, greppable `key: value` lines.
    pub(super) fn diagnostics_block(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("{} {}\n", self.name, self.version));
        out.push_str(&format!("commit:  {}\n", self.git_sha));
        out.push_str(&format!("built:   {}\n", self.build_date));
        out.push_str(&format!("target:  {}\n", self.target));
        out.push_str(&format!("rustc:   {}\n", self.rustc_version));
        out.push_str(&format!("display: {}\n", self.display_server));
        match &self.adapter {
            Some(a) => {
                out.push_str(&format!("gpu:     {}\n", a.name));
                out.push_str(&format!("backend: {} ({})\n", a.backend, a.device_type));
                let driver = match (a.driver.is_empty(), a.driver_info.is_empty()) {
                    (true, true) => "unknown".to_string(),
                    (false, true) => a.driver.clone(),
                    (true, false) => a.driver_info.clone(),
                    (false, false) => format!("{} {}", a.driver, a.driver_info),
                };
                out.push_str(&format!("driver:  {}\n", driver));
            }
            None => out.push_str("gpu:     (renderer not initialized)\n"),
        }
        out
    }

    /// Informational lines for the About view body, grouped with blank-line
    /// separators. These are the inert (non-actionable) rows; the panel appends
    /// the clickable links and the Copy-diagnostics row after them. A driver
    /// string combines `driver`/`driver_info`, falling back to "unknown".
    pub(super) fn info_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        // Identity block.
        lines.push(format!("{} {}", self.name, self.version));
        lines.push(format!("License: {}", self.license));
        lines.push(String::new());
        // Build block.
        lines.push(format!("Commit:  {}", self.git_sha));
        lines.push(format!("Built:   {}", self.build_date));
        lines.push(format!("Target:  {}", self.target));
        lines.push(format!("Rust:    {}", self.rustc_version));
        lines.push(String::new());
        // Renderer block.
        match &self.adapter {
            Some(a) => {
                lines.push(format!("GPU:     {}", a.name));
                lines.push(format!("Backend: {} ({})", a.backend, a.device_type));
                lines.push(format!("Driver:  {}", self.driver_string()));
            }
            None => lines.push("GPU:     (renderer not initialized)".to_string()),
        }
        lines.push(format!("Display: {}", self.display_server));
        lines
    }

    /// Combined driver name + detail, or "unknown" when both are empty.
    fn driver_string(&self) -> String {
        match &self.adapter {
            None => "unknown".to_string(),
            Some(a) => match (a.driver.is_empty(), a.driver_info.is_empty()) {
                (true, true) => "unknown".to_string(),
                (false, true) => a.driver.clone(),
                (true, false) => a.driver_info.clone(),
                (false, false) => format!("{} {}", a.driver, a.driver_info),
            },
        }
    }
}

/// Best-effort display-server detection from the environment. Wayland is checked
/// first because a session can advertise both; OdyTTY prefers the Wayland
/// backend when `WAYLAND_DISPLAY` is set.
fn detect_display_server() -> &'static str {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "Wayland"
    } else if std::env::var_os("DISPLAY").is_some() {
        "X11"
    } else {
        "unknown"
    }
}
