// SPDX-License-Identifier: GPL-3.0-only
//! Exercise std framing retries without an operating-system transport.

use super::{cancelled_io_error, protocol};
use std::io::{self, Read, Write};

#[derive(Default)]
struct CancelledIo {
    calls: usize,
}

impl CancelledIo {
    fn attempt(&mut self) -> io::Result<usize> {
        self.calls += 1;
        if self.calls == 1 {
            Err(cancelled_io_error())
        } else {
            // Bound the regression case instead of recreating an endless retry.
            Err(io::Error::from(io::ErrorKind::UnexpectedEof))
        }
    }
}

impl Read for CancelledIo {
    fn read(&mut self, _bytes: &mut [u8]) -> io::Result<usize> {
        self.attempt()
    }
}

impl Write for CancelledIo {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        self.attempt()
    }

    fn flush(&mut self) -> io::Result<()> {
        panic!("a cancelled frame must not flush")
    }
}

fn assert_cancelled(io: &CancelledIo, error: io::Error) {
    assert_eq!(io.calls, 1, "framing must not retry transport cancellation");
    assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    assert_eq!(
        error
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<protocol::ErrorCode>()),
        Some(&protocol::ErrorCode::Cancelled),
    );
}

#[test]
fn cancelled_request_read_is_not_retried() {
    let mut io = CancelledIo::default();
    let error = protocol::read_request(&mut io).expect_err("cancelled request");
    assert_cancelled(&io, error);
}

#[test]
fn cancelled_response_write_is_not_retried() {
    let mut io = CancelledIo::default();
    let error = protocol::write_response(
        &mut io,
        &protocol::Response {
            request_id: 1,
            reply: protocol::Reply::Error(protocol::ErrorCode::Cancelled),
        },
    )
    .expect_err("cancelled response");
    assert_cancelled(&io, error);
}
