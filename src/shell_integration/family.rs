// SPDX-License-Identifier: GPL-3.0-only
//! The shell families the Shell Integration readout describes, and how the
//! master switch actually reaches each one.
//!
//! Deliberately a superset of the injectable [`ShellKind`]: a family OdyTTY
//! only detects still needs an honest row in the readout, and keeping the two
//! vocabularies apart keeps the injection path total without an
//! empty-snippet arm.

use super::detect::ShellKind;

/// How OdyTTY delivers shell integration to a shell family, for the Shell
/// Integration settings readout. The section renders one row per family from
/// this so the master switch never over-promises: it says exactly what the
/// switch does for each shell rather than implying a uniform capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationPosture {
    /// OdyTTY injects its OSC 133 snippet at spawn (bash/zsh/fish).
    Injected,
    /// Injected only on Windows. `pwsh` on Unix has no injection surface (the
    /// Unix injector has no PowerShell arm by design), so the readout hides the
    /// PowerShell row off-Windows.
    InjectedWindowsOnly,
    /// The shell ships its own integration config; OdyTTY detects it and points
    /// at the native setting but never injects (nushell).
    ConfigureNatively,
}

/// A shell family the Shell Integration section can describe. This is the
/// readout vocabulary and a deliberate superset of the injection-capable
/// [`ShellKind`]: it also names nushell, which OdyTTY recognizes and documents
/// but never injects into (nushell loads its own `$env.config.shell_integration`
/// and there is no clean spawn-time override). Keeping this separate from
/// [`ShellKind`] keeps the injection path total without an empty-snippet arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFamily {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Nushell,
}

impl ShellFamily {
    /// Every family the readout enumerates, in display order.
    pub const ALL: [ShellFamily; 5] = [
        ShellFamily::Bash,
        ShellFamily::Zsh,
        ShellFamily::Fish,
        ShellFamily::PowerShell,
        ShellFamily::Nushell,
    ];

    /// Classify a shell from a user-supplied name or program basename token.
    /// Recognizes the four injected families plus `nu`/`nushell` (detection
    /// only). Leading `-` (login-shell argv) is stripped by the caller side for
    /// [`ShellKind`]; this readout classifier accepts the bare token.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw
            .trim()
            .trim_start_matches('-')
            .to_ascii_lowercase()
            .as_str()
        {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "powershell" | "pwsh" => Some(Self::PowerShell),
            "nu" | "nushell" => Some(Self::Nushell),
            _ => None,
        }
    }

    /// Human-readable name for the readout row.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::PowerShell => "PowerShell",
            Self::Nushell => "nushell",
        }
    }

    /// How the switch reaches this family.
    pub fn posture(self) -> IntegrationPosture {
        match self {
            Self::Bash | Self::Zsh | Self::Fish => IntegrationPosture::Injected,
            Self::PowerShell => IntegrationPosture::InjectedWindowsOnly,
            Self::Nushell => IntegrationPosture::ConfigureNatively,
        }
    }

    /// The injection-capable [`ShellKind`] for this family, or `None` for
    /// nushell (detected and documented, never injected).
    pub fn injectable(self) -> Option<ShellKind> {
        match self {
            Self::Bash => Some(ShellKind::Bash),
            Self::Zsh => Some(ShellKind::Zsh),
            Self::Fish => Some(ShellKind::Fish),
            Self::PowerShell => Some(ShellKind::PowerShell),
            Self::Nushell => None,
        }
    }

    /// One-line readout summary shown in the Shell Integration section / docs.
    /// Honest about what the switch does for this family; nushell points at the
    /// native config rather than promising injection.
    pub fn readout(self) -> &'static str {
        match self {
            Self::Bash => {
                "OSC 133 prompt marks, OSC 7 cwd, click-to-position, button emitters; \
                 optional prompt-scoped key enhancement"
            }
            Self::Zsh => {
                "OSC 133 prompt marks, OSC 7 cwd, per-keystroke edit region, \
                 click-to-position, button emitters; optional prompt-scoped key enhancement"
            }
            Self::Fish => {
                "OSC 133 prompt marks, OSC 7 cwd, edit region, click-to-position, \
                 button emitters; fish 4+ manages the keyboard protocol itself"
            }
            Self::PowerShell => {
                "Windows only: OSC 133 prompt marks, OSC 7 cwd, button emitters; \
                 key bindings use the PSReadLine/Console API, not a VT protocol"
            }
            Self::Nushell => {
                "Configure natively: set $env.config.shell_integration.osc133/osc7/osc2 \
                 and use_kitty_protocol in your nushell config"
            }
        }
    }
}
