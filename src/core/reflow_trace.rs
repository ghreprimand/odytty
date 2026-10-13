// SPDX-License-Identifier: GPL-3.0-only
//! Opt-in resize geometry diagnostics. `ODYTTY_REFLOW_TRACE=1` or `true`
//! appends one line per `Screen::resize`; when off the cost is one atomic
//! load.
//!
//! When enabled it appends one line per resize to `odytty-reflow-trace.log`
//! in the state directory the runtime log uses. On Unix that directory is
//! owner-private and the file is opened through the no-follow, non-blocking,
//! owner-checked append helper, which refuses a planted symlink, FIFO, or
//! foreign-owned file; on Windows it is an ordinary append open under the
//! inherited ACLs. A file (not stderr) because the Windows build is a
//! GUI-subsystem app with no visible stderr; the log is retrieved afterward.
//!
//! # Privacy
//! A line records only geometry, cursor coordinates, booleans, and a sequence
//! counter: no terminal text, paths, or environment values.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Tracing enablement is cached in a `OnceLock` after the first environment
/// lookup. Parsed once from `ODYTTY_REFLOW_TRACE`
/// (`"1"`/`"true"` = on, case-insensitive); any other value or absence = off.
static ENABLED: OnceLock<bool> = OnceLock::new();
/// Monotonic per-process resize counter, included in every trace line so the
/// captured cycles are orderable.
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Set once after the header line is written, so repeated resizes in one process
/// append data lines without re-emitting the legend.
static HEADER_WRITTEN: OnceLock<()> = OnceLock::new();

/// Whether resize tracing is enabled. Reads the env var exactly once; later
/// calls read the cached `OnceLock` value.
fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("ODYTTY_REFLOW_TRACE")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false)
    })
}

/// The fields captured for one `Screen::resize` call. All are plain geometry /
/// cursor coordinates and booleans - no user content.
#[derive(Clone, Copy, Debug)]
pub(in crate::core) struct ResizeTrace {
    pub old_cols: usize,
    pub old_rows: usize,
    pub new_cols: usize,
    pub new_rows: usize,
    pub width_unchanged: bool,
    /// THE load-bearing field: whether the shell applied output since the last
    /// resize (so the `preserve_cursor_physical_line` override fired).
    pub output_since_last_resize: bool,
    pub alt_screen_active: bool,
    /// Whether the resized screen had the backend "shell owns cursor on resize"
    /// capability live at this resize (ConPTY ⇒ true). Captured so a trace can
    /// distinguish "the flag was never wired/was clobbered" from "the flag was
    /// live but the cursor moved for another reason" - the exact ambiguity the
    /// Windows cursor-translation investigation needs settled per-resize.
    pub shell_owns_cursor_on_resize: bool,
    pub cursor_in_row: usize,
    pub cursor_in_col: usize,
    pub pending_wrap_in: bool,
    pub cursor_out_row: usize,
    pub cursor_out_col: usize,
    pub pending_wrap_out: bool,
}

/// Trace file name inside the owner-private state directory.
const TRACE_FILE: &str = "odytty-reflow-trace.log";

/// Column legend for the data lines, written once per process as a header.
const LEGEND: &str = "seq old_dims->new_dims width_unchanged output_since_last_resize alt_screen shell_owns_cursor cursor_in pending_in cursor_out pending_out";

/// Format one trace data line. Pure (no env, no I/O, no global state beyond the
/// provided `seq`), so it is unit-testable without races.
pub(in crate::core) fn format_trace_line(seq: u64, t: &ResizeTrace) -> String {
    format!(
        "seq={seq} {old_cols}x{old_rows}->{new_cols}x{new_rows} width_unchanged={width_unchanged} output_since_last_resize={out_since} alt_screen={alt} shell_owns_cursor={shell_owns} cursor_in=({in_row},{in_col}) pending_in={pin} cursor_out=({out_row},{out_col}) pending_out={pout}",
        old_cols = t.old_cols,
        old_rows = t.old_rows,
        new_cols = t.new_cols,
        new_rows = t.new_rows,
        width_unchanged = t.width_unchanged,
        out_since = t.output_since_last_resize,
        alt = t.alt_screen_active,
        shell_owns = t.shell_owns_cursor_on_resize,
        in_row = t.cursor_in_row,
        in_col = t.cursor_in_col,
        pin = t.pending_wrap_in,
        out_row = t.cursor_out_row,
        out_col = t.cursor_out_col,
        pout = t.pending_wrap_out,
    )
}

/// Append one resize trace line to the state-directory log when tracing is
/// enabled. No-op (single atomic load) when off. Any I/O error, including (on
/// Unix) a rejected symlink, FIFO, or foreign-owned file, or an unusable state
/// directory, skips the line without affecting terminal behavior.
pub(in crate::core) fn trace_resize(t: &ResizeTrace) {
    if !enabled() {
        return;
    }
    use std::io::Write;

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let Ok(dir) = crate::logging::prepare_state_log_dir() else {
        return;
    };
    let path = dir.join(TRACE_FILE);
    let Ok(mut file) = crate::state_dir::open_append_sensitive(&path) else {
        return;
    };

    if HEADER_WRITTEN.set(()).is_ok() {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(
            file,
            "# odytty-reflow-trace v{} start_epoch_ms={millis}\n# {LEGEND}",
            env!("CARGO_PKG_VERSION"),
        );
    }

    let _ = writeln!(file, "{}", format_trace_line(seq, t));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_trace_line_contains_all_fields() {
        let t = ResizeTrace {
            old_cols: 80,
            old_rows: 24,
            new_cols: 40,
            new_rows: 24,
            width_unchanged: false,
            output_since_last_resize: true,
            alt_screen_active: false,
            shell_owns_cursor_on_resize: true,
            cursor_in_row: 0,
            cursor_in_col: 16,
            pending_wrap_in: false,
            cursor_out_row: 1,
            cursor_out_col: 7,
            pending_wrap_out: true,
        };
        let line = format_trace_line(7, &t);
        assert_eq!(
            line,
            "seq=7 80x24->40x24 width_unchanged=false output_since_last_resize=true alt_screen=false shell_owns_cursor=true cursor_in=(0,16) pending_in=false cursor_out=(1,7) pending_out=true"
        );
    }
}
