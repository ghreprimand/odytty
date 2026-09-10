// SPDX-License-Identifier: GPL-3.0-only
//! Structural-control protocol. Transport and GUI routing are separate owners.
//!
//! No request can carry terminal input, content reads, argv, environment,
//! profile writes, or file transfer. Endpoints require explicit construction;
//! only the invoked quick-terminal CLI verb performs bounded, fail-closed Unix
//! discovery. Importing this module performs no discovery or startup work.

pub mod cli;
pub mod dispatch;
pub mod protocol;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod unix;
#[cfg(windows)]
pub mod windows;
