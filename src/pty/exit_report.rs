// SPDX-License-Identifier: GPL-3.0-only
//! Order of the steps the Windows child-waiter takes when a child has exited.
//!
//! The pump learns that a session ended from end-of-file on the pseudoconsole
//! output, which closing the pseudoconsole produces, and it reads the one-shot
//! startup-failure line from a shared slot at that moment. The line therefore
//! has to be in the slot before the pseudoconsole closes, or the pump can see
//! the end-of-file first and leave the pane blank. The line is also written
//! to standard error, which can block (a full pipe, a stalled console), so
//! that write comes last and runs off the waiter's thread: session teardown
//! joins the waiter, and a blocked write must never hold it up. The order is
//! kept here, free of OS calls, so it is tested on every platform.

/// Run the three steps in their required order: `publish` the line for the
/// pane, `close` the pseudoconsole, then hand the line to `log`. `log` is
/// expected to return at once (the Windows backend starts a detached thread
/// for the write). With no line, only `close` runs.
pub(super) fn finish_child_exit(
    line: Option<String>,
    publish: impl FnOnce(String),
    close: impl FnOnce(),
    log: impl FnOnce(String),
) {
    match line {
        Some(line) => {
            publish(line.clone());
            close();
            log(line);
        }
        None => close(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn run(line: Option<&str>) -> Vec<String> {
        let events = RefCell::new(Vec::new());
        finish_child_exit(
            line.map(str::to_owned),
            |line| events.borrow_mut().push(format!("publish {line}")),
            || events.borrow_mut().push("close".to_owned()),
            |line| events.borrow_mut().push(format!("log {line}")),
        );
        events.into_inner()
    }

    #[test]
    fn the_line_is_published_before_the_pseudoconsole_closes() {
        assert_eq!(run(Some("boom")), ["publish boom", "close", "log boom"]);
    }

    #[test]
    fn a_normal_exit_only_closes() {
        assert_eq!(run(None), ["close"]);
    }
}
