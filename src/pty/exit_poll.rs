// SPDX-License-Identifier: GPL-3.0-only
//! How a Windows child's exit is read without mistaking a live child for one
//! that exited with the code 259.
//!
//! `GetExitCodeProcess` reports a running process as `STILL_ACTIVE` (259), which
//! is also a legitimate exit code, so the code alone cannot say whether the
//! child has exited. The process object can: it is signaled once the child has
//! exited. The two reads are separate calls, so a child can exit between them.
//! The first code is then the stale sentinel while the real code is already
//! final, and it has to be read again once the object is seen signaled. The
//! decision is kept here, free of OS calls, so it is tested on every platform.

use std::io;

/// What the process object says about the child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Liveness {
    Running,
    Exited,
}

/// `Some(code)` once the child has exited, `None` while it runs. `first` is the
/// code from the first read; `liveness` and `reread` are called only when
/// `first` equals `still_active`, and `reread` only after the child is seen
/// exited.
pub(super) fn exit_code_if_exited(
    first: u32,
    still_active: u32,
    liveness: impl FnOnce() -> io::Result<Liveness>,
    reread: impl FnOnce() -> io::Result<u32>,
) -> io::Result<Option<u32>> {
    if first != still_active {
        return Ok(Some(first));
    }
    match liveness()? {
        Liveness::Running => Ok(None),
        Liveness::Exited => reread().map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STILL_ACTIVE: u32 = 259;

    fn never<T>() -> io::Result<T> {
        panic!("not consulted");
    }

    #[test]
    fn an_ordinary_code_is_final_without_consulting_the_process_object() {
        assert_eq!(
            exit_code_if_exited(7, STILL_ACTIVE, never, never).unwrap(),
            Some(7)
        );
    }

    #[test]
    fn the_sentinel_with_a_running_process_is_still_running() {
        assert_eq!(
            exit_code_if_exited(STILL_ACTIVE, STILL_ACTIVE, || Ok(Liveness::Running), never)
                .unwrap(),
            None
        );
    }

    #[test]
    fn a_genuine_259_is_reported_when_the_process_is_signaled() {
        assert_eq!(
            exit_code_if_exited(
                STILL_ACTIVE,
                STILL_ACTIVE,
                || Ok(Liveness::Exited),
                || Ok(259)
            )
            .unwrap(),
            Some(259)
        );
    }

    #[test]
    fn a_child_that_exits_between_the_two_reads_reports_its_real_code() {
        // First read: still running (259). The child then exits with 7 before
        // the liveness check sees it signaled; the stale sentinel is not it.
        assert_eq!(
            exit_code_if_exited(
                STILL_ACTIVE,
                STILL_ACTIVE,
                || Ok(Liveness::Exited),
                || Ok(7)
            )
            .unwrap(),
            Some(7)
        );
    }

    #[test]
    fn failures_of_either_read_surface() {
        let failed = || Err(io::Error::other("boom"));
        assert!(
            exit_code_if_exited(STILL_ACTIVE, STILL_ACTIVE, failed, never).is_err(),
            "liveness failure"
        );
        assert!(
            exit_code_if_exited(
                STILL_ACTIVE,
                STILL_ACTIVE,
                || Ok(Liveness::Exited),
                || { Err(io::Error::other("boom")) }
            )
            .is_err(),
            "reread failure"
        );
    }
}
