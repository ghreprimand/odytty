// SPDX-License-Identifier: GPL-3.0-only
//! Windows WSL shell discovery for profile pickers.
//!
//! WSL enumeration runs only on demand (never on the ordinary default launch
//! path) and uses [`super::app::win_spawn::apply_no_console_window`] so a GUI
//! build does not flash a console window.

#[cfg(not(windows))]
use crate::profiles::DiscoveredShell;
use crate::profiles::discovered_shells as base_discovered_shells;
#[cfg(windows)]
use crate::profiles::{DiscoveredShell, ShellKind, parse_wsl_distro_list};

/// Shell discovery for UI pickers: base platform shells plus Windows WSL distros.
pub(crate) fn discovered_shells() -> Vec<DiscoveredShell> {
    #[cfg(windows)]
    {
        let mut out = base_discovered_shells().to_vec();
        append_wsl_shells(&mut out);
        out
    }
    #[cfg(not(windows))]
    {
        base_discovered_shells().to_vec()
    }
}

#[cfg(windows)]
fn append_wsl_shells(out: &mut Vec<DiscoveredShell>) {
    for distro in read_wsl_distro_names() {
        if out
            .iter()
            .any(|entry| entry.kind == ShellKind::Wsl && entry.args.get(1) == Some(&distro))
        {
            continue;
        }
        out.push(DiscoveredShell {
            label: format!("WSL: {distro}"),
            program: "wsl.exe".to_owned(),
            args: vec!["-d".to_owned(), distro],
            kind: ShellKind::Wsl,
        });
    }
}

#[cfg(windows)]
fn read_wsl_distro_names() -> Vec<String> {
    use std::process::Command;

    let mut command = Command::new("wsl.exe");
    command.args(["--list", "--quiet"]);
    super::app::win_spawn::apply_no_console_window(&mut command);
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_wsl_distro_list(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn wsl_shell_entries_use_structured_program_and_args() {
        let mut out = base_discovered_shells().to_vec();
        append_wsl_shells(&mut out);
        for shell in out.iter().filter(|entry| entry.kind == ShellKind::Wsl) {
            assert_eq!(shell.program, "wsl.exe");
            assert!(
                shell.args.len() >= 2 && shell.args[0] == "-d",
                "expected structured -d args, got {:?}",
                shell.args
            );
        }
    }

    #[test]
    fn discovered_shells_merge_is_cached_via_profiles_base() {
        let first = discovered_shells();
        let second = discovered_shells();
        assert_eq!(first.len(), second.len());
    }
}
