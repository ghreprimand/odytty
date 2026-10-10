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

/// Longest a profile picker waits for `wsl.exe --list`.
#[cfg(windows)]
const WSL_LIST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
/// UTF-16 distro names, one per line; the parser keeps at most 32 of them.
#[cfg(windows)]
const WSL_LIST_MAX_OUTPUT: usize = 64 * 1024;
/// How long a successful answer (including "no distros") is reused before
/// `wsl.exe` runs again, so reopening a picker does not repeat the wait.
#[cfg(any(windows, test))]
const WSL_LIST_REUSE: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a failed run (timeout, flood, error exit) is reused: long enough
/// that a burst of picker opens does not rerun a broken helper, short enough
/// that a transient failure does not hide every distro for a minute.
#[cfg(any(windows, test))]
const WSL_FAILED_REUSE: std::time::Duration = std::time::Duration::from_secs(5);

/// The last `wsl.exe --list` answer: when it was taken, the names, and whether
/// the run succeeded.
#[cfg(any(windows, test))]
type WslListCache = std::sync::Mutex<Option<(std::time::Instant, Vec<String>, bool)>>;

#[cfg(windows)]
static WSL_LIST_CACHE: WslListCache = std::sync::Mutex::new(None);

/// WSL distro names, from a bounded `wsl.exe --list --quiet` run whose answer
/// is reused for [`WSL_LIST_REUSE`] ([`WSL_FAILED_REUSE`] after a failure). A
/// helper that stalls past [`WSL_LIST_DEADLINE`] or floods its output is killed
/// and yields no distros.
#[cfg(windows)]
fn read_wsl_distro_names() -> Vec<String> {
    cached_wsl_distro_names(
        &WSL_LIST_CACHE,
        std::time::Instant::now,
        query_wsl_distro_names,
    )
}

/// Serve a fresh cached answer, or run `query` with the cache unlocked and
/// store its answer. `query` returns `None` for a failed run. Releasing the
/// lock means a caller never waits on the cache itself; the enumeration still
/// runs synchronously, so it blocks the calling event loop (and every window
/// it serves) for up to the two-second listing deadline, and a second caller
/// that misses the cache meanwhile runs its own query.
#[cfg(any(windows, test))]
fn cached_wsl_distro_names(
    cache: &WslListCache,
    now: impl Fn() -> std::time::Instant,
    query: impl FnOnce() -> Option<Vec<String>>,
) -> Vec<String> {
    let lock = || {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((at, names, succeeded)) = lock().as_ref() {
        let reuse = if *succeeded {
            WSL_LIST_REUSE
        } else {
            WSL_FAILED_REUSE
        };
        if now().saturating_duration_since(*at) < reuse {
            return names.clone();
        }
    }
    let answer = query();
    let succeeded = answer.is_some();
    let names = answer.unwrap_or_default();
    *lock() = Some((now(), names.clone(), succeeded));
    names
}

#[cfg(windows)]
fn query_wsl_distro_names() -> Option<Vec<String>> {
    use std::process::Command;

    let mut command = Command::new("wsl.exe");
    command.args(["--list", "--quiet"]);
    super::app::win_spawn::apply_no_console_window(&mut command);
    match crate::bounded_io::run_bounded(&mut command, WSL_LIST_DEADLINE, WSL_LIST_MAX_OUTPUT) {
        Ok(output) if output.status.success() => Some(parse_wsl_distro_list(&output.stdout)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The query runs with the cache unlocked: another caller can read the
    /// cache while it is in progress. A success is reused for the full
    /// window, a failure only briefly.
    #[test]
    fn wsl_list_cache_runs_the_query_unlocked_and_reuses_failures_briefly() {
        use std::time::{Duration, Instant};
        let cache: WslListCache = std::sync::Mutex::new(None);
        let start = Instant::now();
        let clock = std::cell::Cell::new(start);
        let now = || clock.get();

        let names = cached_wsl_distro_names(&cache, now, || {
            assert!(
                cache.try_lock().is_ok(),
                "the cache is not held during the query"
            );
            None
        });
        assert!(names.is_empty());
        clock.set(start + Duration::from_secs(1));
        let reused = cached_wsl_distro_names(&cache, now, || panic!("a fresh failure is reused"));
        assert!(reused.is_empty());
        clock.set(start + WSL_FAILED_REUSE + Duration::from_secs(1));
        let names = cached_wsl_distro_names(&cache, now, || Some(vec!["Ubuntu".to_owned()]));
        assert_eq!(
            names,
            ["Ubuntu"],
            "a failure is retried after the short window"
        );
        clock.set(start + WSL_FAILED_REUSE + Duration::from_secs(30));
        let reused = cached_wsl_distro_names(&cache, now, || panic!("a success is reused"));
        assert_eq!(reused, ["Ubuntu"]);
    }

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
