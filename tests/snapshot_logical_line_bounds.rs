// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored import fixtures under the project license.
//! Proposed conservative policy: reject oversized imported logical runs.
//! The terminal core contract is shared by Linux, macOS and Windows.

use odytty::core::{SnapshotCaptureLimits, SnapshotEnvelope, Terminal};

#[test]
fn imported_closed_line_over_the_source_ceiling_is_rejected() {
    let terminal = Terminal::new(512, 1);
    let mut envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let mut row = envelope.terminal.visible_rows[0].clone();
    for cell in &mut row.cells {
        cell.ch = 'x';
    }
    row.wrapped = true;
    // 2,049 full rows exceed the 2^20 retained-cell ceiling by 512 cells.
    envelope.terminal.scrollback_rows = vec![row.clone(); 2_049];
    envelope
        .terminal
        .scrollback_rows
        .last_mut()
        .unwrap()
        .wrapped = false;
    // An unrelated closed trailing line keeps the oversized line out of the
    // trailing-line enforcement path after reconstruction.
    row.wrapped = false;
    for cell in &mut row.cells {
        cell.ch = 'y';
    }
    envelope.terminal.scrollback_rows.push(row);
    assert!(Terminal::from_snapshot_envelope(&envelope).is_err());
}
