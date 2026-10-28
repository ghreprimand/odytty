// SPDX-License-Identifier: GPL-3.0-only
//! Test apparatus accounting. A partial or failed reply never counts as complete.
use std::io::{self, Write};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Budget,
    Write(io::ErrorKind),
}
#[derive(Debug, Default)]
pub struct Replies {
    pub generated_bytes: usize,
    pub written_bytes: usize,
    pub completed_replies: usize,
    pub failure: Option<Failure>,
}
impl Replies {
    pub fn complete(&self) -> bool {
        self.failure.is_none() && self.written_bytes == self.generated_bytes
    }
    pub fn submit(&mut self, writer: &mut dyn Write, reply: &[u8]) -> Result<(), Failure> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if reply.is_empty() {
            return Ok(());
        }
        let generated = self.generated_bytes.saturating_add(reply.len());
        self.generated_bytes = generated;
        if generated > 4096 {
            self.failure = Some(Failure::Budget);
            return Err(Failure::Budget);
        }
        let mut position = 0;
        while position < reply.len() {
            let failure = match writer.write(&reply[position..]) {
                Ok(0) => Some(Failure::Write(io::ErrorKind::WriteZero)),
                Ok(count) if count <= reply.len() - position => {
                    position += count;
                    self.written_bytes += count;
                    None
                }
                Ok(_) => Some(Failure::Write(io::ErrorKind::InvalidData)),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => Some(Failure::Write(error.kind())),
            };
            if let Some(error) = failure {
                self.failure = Some(error);
                return Err(error);
            }
        }
        self.completed_replies += 1;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct PartialFailure {
        remaining: usize,
    }
    impl Write for PartialFailure {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let count = self.remaining.min(data.len());
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn only_fully_written_replies_are_counted() {
        let mut state = Replies::default();
        let mut output = Vec::new();
        state.submit(&mut output, b"\x1b[1;1R").unwrap();
        state.submit(&mut output, b"").unwrap();
        assert!(state.complete());
        assert_eq!(state.completed_replies, 1);
        assert_eq!(state.written_bytes, 6);
        assert_eq!(state.generated_bytes, 6);
        assert_eq!(output, b"\x1b[1;1R");
    }
    #[test]
    fn partial_broken_pipe_is_attributed_and_never_complete() {
        let mut state = Replies::default();
        let mut writer = PartialFailure { remaining: 2 };
        assert_eq!(
            state.submit(&mut writer, b"abcdef"),
            Err(Failure::Write(io::ErrorKind::BrokenPipe))
        );
        assert_eq!(state.written_bytes, 2);
        assert_eq!(state.generated_bytes, 6);
        assert_eq!(state.completed_replies, 0);
        assert!(!state.complete());
        assert_eq!(
            state.submit(&mut Vec::new(), b"later"),
            Err(Failure::Write(io::ErrorKind::BrokenPipe))
        );
        assert!(!state.complete());
    }
    #[test]
    fn zero_progress_and_budget_exhaustion_are_failures() {
        let mut state = Replies::default();
        assert_eq!(
            state.submit(&mut io::Cursor::new(&mut [0u8; 0][..]), b"x"),
            Err(Failure::Write(io::ErrorKind::WriteZero))
        );
        assert!(!state.complete());
        let mut state = Replies::default();
        assert_eq!(
            state.submit(&mut Vec::new(), &[0; 4097]),
            Err(Failure::Budget)
        );
        assert_eq!(state.written_bytes, 0);
        assert_eq!(state.completed_replies, 0);
        assert!(!state.complete());
    }
}
