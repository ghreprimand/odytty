// SPDX-License-Identifier: GPL-3.0-only
//! Retry only Windows sharing/access races after an owned helper has exited.
use std::{
    io,
    time::{Duration, Instant},
};
const LIMIT: Duration = Duration::from_secs(5);
const PAUSE: Duration = Duration::from_millis(20);
fn retry_with<T>(
    mut operation: impl FnMut() -> io::Result<T>,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> io::Result<T> {
    let mut result = operation();
    while result
        .as_ref()
        .is_err_and(|error| matches!(error.raw_os_error(), Some(5 | 32 | 33)))
    {
        let remaining = LIMIT.saturating_sub(elapsed());
        if remaining.is_zero() {
            return result;
        }
        sleep(PAUSE.min(remaining));
        if elapsed() >= LIMIT {
            return result;
        }
        result = operation();
    }
    result
}
pub fn retry<T>(operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let started = Instant::now();
    retry_with(operation, || started.elapsed(), std::thread::sleep)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn transient_sharing_and_access_errors_retry_then_succeed() {
        let elapsed = Cell::new(Duration::ZERO);
        let attempts = Cell::new(0);
        let result = retry_with(
            || {
                let n = attempts.get();
                attempts.set(n + 1);
                match n {
                    0 => Err(io::Error::from_raw_os_error(32)),
                    1 => Err(io::Error::from_raw_os_error(5)),
                    2 => Err(io::Error::from_raw_os_error(33)),
                    _ => Ok(7),
                }
            },
            || elapsed.get(),
            |duration| elapsed.set(elapsed.get() + duration),
        );
        assert_eq!(result.unwrap(), 7);
        assert_eq!(attempts.get(), 4);
        assert_eq!(elapsed.get(), PAUSE * 3);
    }
    #[test]
    fn five_second_deadline_retains_failure_without_an_extra_attempt() {
        let elapsed = Cell::new(Duration::ZERO);
        let attempts = Cell::new(0);
        let result: io::Result<()> = retry_with(
            || {
                attempts.set(attempts.get() + 1);
                Err(io::Error::from_raw_os_error(32))
            },
            || elapsed.get(),
            |duration| elapsed.set(elapsed.get() + duration),
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
        assert_eq!(elapsed.get(), LIMIT);
        assert_eq!(attempts.get(), 250);
    }
    #[test]
    fn unrelated_failures_and_success_do_not_sleep_or_retry() {
        for code in [2, 3, 87] {
            let attempts = Cell::new(0);
            let result: io::Result<()> = retry_with(
                || {
                    attempts.set(attempts.get() + 1);
                    Err(io::Error::from_raw_os_error(code))
                },
                || Duration::ZERO,
                |_| panic!("unrelated failure retried"),
            );
            assert_eq!(result.unwrap_err().raw_os_error(), Some(code));
            assert_eq!(attempts.get(), 1);
        }
        assert_eq!(retry(|| Ok(11)).unwrap(), 11);
    }
}
