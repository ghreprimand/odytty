// SPDX-License-Identifier: GPL-3.0-only
//! Stable settings-group ordering and derived numeric range labels.

use super::NumericSpec;

pub(super) fn setting_group_rank(group: &str) -> usize {
    match group {
        "Theme" => 0,
        "Font" => 1,
        "Rendering" => 2,
        // The four "Layout" groups sort right after `Rendering`, matching that
        // section's position (4th in `SECTIONS`): Tabs, then Workspace rail, then
        // Panel, then Panes, so the rows read top-to-bottom in that order.
        "Tabs" => 3,
        "Workspace rail" => 4,
        "Panel" => 5,
        "Panes" => 6,
        "Post-process" => 7,
        "Cursor" => 8,
        "Input" => 9,
        "Shell Integration" => 10,
        "Connections" => 11,
        "Sessions" => 12,
        "Clipboard" => 13,
        "Accessibility" => 14,
        "Development" => 15,
        _ => 99,
    }
}

/// Derive the human-readable range hint for a numeric row from its
/// [`NumericSpec`] (UX4-P2, Q4), keeping the optional unit suffix so the
/// display string can never drift from the clamp bounds.
pub(super) fn numeric_range_label(spec: NumericSpec) -> String {
    let lo = format_bound(spec.min);
    let hi = format_bound(spec.max);
    if spec.unit.is_empty() {
        format!("{lo}..={hi}")
    } else {
        format!("{lo}..={hi} {}", spec.unit)
    }
}

/// Format a numeric bound for the range hint: two decimals, then trailing
/// zeros trimmed while always keeping at least one decimal place (so `6.0`
/// stays `6.0` and `0.18` stays `0.18`).
fn format_bound(value: f32) -> String {
    let mut s = format!("{value:.2}");
    while s.ends_with('0') && !s.ends_with(".0") {
        s.pop();
    }
    s
}
