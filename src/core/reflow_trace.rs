// SPDX-License-Identifier: GPL-3.0-only
//! Env-gated, event-driven resize trace for diagnosing cursor-placement
//! behavior across `Screen::resize` (notably the Windows/ConPTY multi-cycle
//! cursor-ratchet investigation).
//!
//! This is a passive diagnostic, OFF by default and zero-cost when off: a single
//! atomic load on the resize path and nothing else (no allocation, no
//! formatting, no file handle) unless `ODYTTY_REFLOW_TRACE` is set to `1` or
//! `true`. It is purely event-driven — one line appended per `Screen::resize`
//! call — never a poll or background loop.
//!
//! When enabled it appends one line per resize to
//! `std::env::temp_dir()/odytty-reflow-trace.log`. A file (not stderr) because
//! the Windows build is a GUI-subsystem app with no visible stderr; the log is
//! retrieved from the temp dir afterward.
//!
//! # Privacy / public-repo safety
//! The trace records ONLY geometry and cursor coordinates, booleans, and a
//! sequence counter — never cell contents, user-typed text, working-directory
//! or other paths, or environment values. The log destination is the OS temp
//! dir (no developer path is baked into the source) and the env var name is
//! generic. Nothing user-identifying can enter a trace line.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// One atomic load when off. Parsed once from `ODYTTY_REFLOW_TRACE`
/// (`"1"`/`"true"` = on, case-insensitive); any other value or absence = off.
static ENABLED: OnceLock<bool> = OnceLock::new();
/// Monotonic per-process resize counter, included in every trace line so the
/// captured cycles are orderable.
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Set once after the header line is written, so repeated resizes in one process
/// append data lines without re-emitting the legend.
static HEADER_WRITTEN: OnceLock<()> = OnceLock::new();

/// Whether resize tracing is enabled. Reads the env var exactly once; every
/// subsequent call is a single relaxed atomic load.
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
/// cursor coordinates and booleans — no user content.
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
    /// live but the cursor moved for another reason" — the exact ambiguity the
    /// Windows cursor-translation investigation needs settled per-resize.
    pub shell_owns_cursor_on_resize: bool,
    pub cursor_in_row: usize,
    pub cursor_in_col: usize,
    pub pending_wrap_in: bool,
    pub cursor_out_row: usize,
    pub cursor_out_col: usize,
    pub pending_wrap_out: bool,
}

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

/// Append one resize trace line to the temp-dir log when tracing is enabled.
/// No-op (single atomic load) when off. Any I/O error is silently ignored — a
/// diagnostic must never perturb terminal behavior.
pub(in crate::core) fn trace_resize(t: &ResizeTrace) {
    if !enabled() {
        return;
    }
    use std::io::Write;

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join("odytty-reflow-trace.log");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
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
