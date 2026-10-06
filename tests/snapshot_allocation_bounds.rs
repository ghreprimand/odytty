// SPDX-License-Identifier: GPL-3.0-only
// Project-authored short wire inputs. Allocation observation is confined to
// the calling test thread; no corpus, elapsed-time or performance claim.
use odytty::core::{SnapshotEnvelope, SnapshotEnvelopeCaps};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct ObservedAllocator;
thread_local! {
    static OBSERVE: Cell<bool> = const { Cell::new(false) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}
fn record(size: usize) {
    let _ = OBSERVE.try_with(|active| {
        if active.get() {
            let _ = PEAK.try_with(|peak| peak.set(peak.get().max(size)));
        }
    });
}
// SAFETY: every allocation operation is forwarded unchanged to System.
unsafe impl GlobalAlloc for ObservedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

struct Observation;
impl Drop for Observation {
    fn drop(&mut self) {
        OBSERVE.with(|active| active.set(false));
    }
}
fn decode_peak(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> usize {
    PEAK.with(|peak| peak.set(0));
    OBSERVE.with(|active| active.set(true));
    let guard = Observation;
    let result = SnapshotEnvelope::decode(bytes, caps);
    drop(guard);
    assert!(result.is_err(), "hostile truncated input must be refused");
    PEAK.with(Cell::get)
}
fn header(sections: u16) -> Vec<u8> {
    let mut bytes = b"ODYTTY-SNAPSHOT".to_vec();
    bytes.extend_from_slice(&5u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes()); // empty producer version
    bytes.extend_from_slice(&sections.to_le_bytes());
    bytes
}
fn one_terminal_section(payload: &[u8]) -> Vec<u8> {
    let mut bytes = header(1);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}
fn short_row_payload(columns: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&columns.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&[0; 8]); // cursor at origin
    payload.extend_from_slice(&[1, 0, 0]); // visible block cursor
    payload.extend_from_slice(&[0; 12]); // basic modes and charset
    payload.extend_from_slice(&0u32.to_le_bytes()); // no history
    payload.extend_from_slice(&1u32.to_le_bytes()); // one visible row
    payload.push(0); // hard row
    payload.extend_from_slice(&columns.to_le_bytes());
    payload
}

#[test]
fn truncated_section_table_reserve_tracks_remaining_wire_bytes() {
    let bytes = header(4096);
    let caps = SnapshotEnvelopeCaps {
        max_sections: 4096,
        ..SnapshotEnvelopeCaps::default()
    };
    assert!(
        decode_peak(&bytes, caps) <= 1024,
        "absent table entries must not reserve a declared table"
    );
}

#[test]
fn truncated_row_reserve_tracks_remaining_wire_bytes() {
    let bytes = one_terminal_section(&short_row_payload(65_536));
    let caps = SnapshotEnvelopeCaps {
        max_columns: 65_536,
        ..SnapshotEnvelopeCaps::default()
    };
    assert!(
        decode_peak(&bytes, caps) <= 1024,
        "absent cells must not reserve a declared row"
    );
}

#[test]
fn small_cell_budget_is_applied_before_reserving_a_row() {
    let bytes = one_terminal_section(&short_row_payload(65_536));
    let caps = SnapshotEnvelopeCaps {
        max_columns: 65_536,
        max_cells: 1,
        ..SnapshotEnvelopeCaps::default()
    };
    assert!(
        decode_peak(&bytes, caps) <= 1024,
        "a one-cell budget must bound allocation before row decoding"
    );
}
