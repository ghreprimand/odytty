// SPDX-License-Identifier: GPL-3.0-only
//! Checked read-modify-write on atomics, spelled once.
//!
//! Rust 1.99 renamed `Atomic*::fetch_update` to `try_update` and deprecates
//! the old name. The declared minimum Rust version is 1.96, where only
//! `fetch_update` exists, so the call cannot be renamed yet. Every caller goes
//! through [`AtomicUpdate::fetch_update_compat`], which keeps the 1.96 spelling
//! behind one narrowly scoped `allow(deprecated)` per atomic type and changes
//! no behavior. When `rust-version` and `rust-toolchain.toml` move to 1.99 or
//! later, replace the two method bodies with `try_update` and delete the
//! allows in the same commit; no caller changes.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// An atomic integer that can be updated through a closure that may refuse.
pub(crate) trait AtomicUpdate {
    /// The integer stored in the atomic.
    type Value: Copy;

    /// Apply `update` to the stored value until the store lands or `update`
    /// returns `None`. `Ok` carries the previous value; `Err` carries the
    /// value last seen when `update` refused, exactly like `fetch_update`.
    fn fetch_update_compat(
        &self,
        set_order: Ordering,
        fetch_order: Ordering,
        update: impl FnMut(Self::Value) -> Option<Self::Value>,
    ) -> Result<Self::Value, Self::Value>;
}

macro_rules! impl_atomic_update {
    ($atomic:ty, $value:ty) => {
        impl AtomicUpdate for $atomic {
            type Value = $value;

            #[allow(deprecated)] // MSRV 1.96 has only `fetch_update`; see the module note.
            fn fetch_update_compat(
                &self,
                set_order: Ordering,
                fetch_order: Ordering,
                update: impl FnMut($value) -> Option<$value>,
            ) -> Result<$value, $value> {
                self.fetch_update(set_order, fetch_order, update)
            }
        }
    };
}

impl_atomic_update!(AtomicU64, u64);
impl_atomic_update!(AtomicUsize, usize);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_landed_update_returns_the_previous_value() {
        let counter = AtomicU64::new(7);
        let result = counter.fetch_update_compat(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        });
        assert_eq!(result, Ok(7));
        assert_eq!(counter.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn a_refused_update_returns_the_current_value_and_stores_nothing() {
        let counter = AtomicUsize::new(usize::MAX);
        let result = counter.fetch_update_compat(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        });
        assert_eq!(result, Err(usize::MAX));
        assert_eq!(counter.load(Ordering::Relaxed), usize::MAX);
    }
}
