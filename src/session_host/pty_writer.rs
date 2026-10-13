// SPDX-License-Identifier: GPL-3.0-only
//! Bounded, non-blocking PTY-master writer for the session host. Blocking writes
//! run on a dedicated thread; producers enqueue under a short-held lock. The
//! byte-bounded queue drops whole oldest chunks when full, preserves the newest
//! accepted frame, and counts discarded bytes in rate-limited log warnings.
//!
//! A bracketed paste must be one input frame so overflow cannot retain a tail
//! without its opening marker. Shutdown does not join a potentially blocked
//! writer thread.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Maximum bytes buffered before drop-oldest engages. Generous enough that a
/// legitimate large paste into a healthy shell drains far faster than it fills;
/// bounded so a fully wedged slave cannot grow host memory without limit.
const QUEUE_BYTE_CAP: usize = 4 * 1024 * 1024;

struct Queue {
    chunks: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    /// Set once the handle is dropped or an fd write fails: the writer thread
    /// drains what it can and exits, and further enqueues are discarded.
    closed: bool,
    /// Bytes discarded by drop-oldest over this writer's lifetime.
    dropped_bytes: u64,
    /// Discarded bytes not yet reported in the host log.
    unlogged_dropped_bytes: u64,
    /// When the last overflow warning was logged, for rate limiting.
    last_overflow_log: Option<Instant>,
}

/// Minimum spacing between host overflow warnings. Each warning carries every
/// byte discarded since the previous one, so rate limiting loses no count.
const OVERFLOW_LOG_INTERVAL: Duration = Duration::from_secs(5);

struct Shared {
    byte_cap: usize,
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enqueue a copy of `bytes`, applying the byte-cap drop-oldest policy. Never
    /// blocks on the fd - only the briefly-held queue lock is taken.
    fn enqueue(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let report = {
            let mut queue = self.lock();
            if queue.closed {
                return;
            }
            queue.chunks.push_back(bytes.to_vec());
            queue.queued_bytes += bytes.len();
            // Never drop the newest chunk: a single frame larger than the cap
            // is still delivered whole rather than discarded outright.
            while queue.queued_bytes > self.byte_cap && queue.chunks.len() > 1 {
                if let Some(dropped) = queue.chunks.pop_front() {
                    queue.queued_bytes -= dropped.len();
                    let dropped = u64::try_from(dropped.len()).unwrap_or(u64::MAX);
                    queue.dropped_bytes = queue.dropped_bytes.saturating_add(dropped);
                    queue.unlogged_dropped_bytes =
                        queue.unlogged_dropped_bytes.saturating_add(dropped);
                }
            }
            take_overflow_report(&mut queue, Instant::now())
        };
        if let Some(dropped) = report {
            tracing::warn!(
                "session-host: PTY input queue overflow discarded {dropped} bytes of input \
                 while the foreground process was not reading"
            );
        }
        self.ready.notify_one();
    }
}

/// The unreported discarded byte count when an overflow warning is due, and
/// mark it reported. `None` when nothing is pending or the last warning was
/// too recent (the count carries into the next warning).
fn take_overflow_report(queue: &mut Queue, now: Instant) -> Option<u64> {
    if queue.unlogged_dropped_bytes == 0 {
        return None;
    }
    if let Some(last) = queue.last_overflow_log
        && now.saturating_duration_since(last) < OVERFLOW_LOG_INTERVAL
    {
        return None;
    }
    queue.last_overflow_log = Some(now);
    Some(std::mem::take(&mut queue.unlogged_dropped_bytes))
}

/// Handle held by the host loop. Producers call [`Self::write`] to enqueue; the
/// dedicated writer thread performs the blocking fd write off the loop.
pub(crate) struct HostPtyWriter {
    shared: Arc<Shared>,
}

impl HostPtyWriter {
    /// Spawn the writer thread that owns `writer` (the raw blocking master fd).
    ///
    /// Fallible: OS thread creation can fail under resource exhaustion, and a
    /// panic here would unwind host startup instead of reporting a spawn
    /// error. Mirrors the native writer shim's fallible spawn.
    pub(crate) fn spawn(writer: Box<dyn Write + Send>) -> std::io::Result<Self> {
        Self::spawn_with_cap(writer, QUEUE_BYTE_CAP)
    }

    fn spawn_with_cap(mut writer: Box<dyn Write + Send>, byte_cap: usize) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            byte_cap,
            queue: Mutex::new(Queue {
                chunks: VecDeque::new(),
                queued_bytes: 0,
                closed: false,
                dropped_bytes: 0,
                unlogged_dropped_bytes: 0,
                last_overflow_log: None,
            }),
            ready: Condvar::new(),
        });
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("odytty-host-pty-writer".to_owned())
            .spawn(move || writer_loop(&thread_shared, &mut writer))?;
        Ok(Self { shared })
    }

    /// Enqueue `bytes` for the hosted PTY. Non-blocking: returns as soon as the
    /// bytes are queued (or dropped under the cap), never waiting on the fd.
    pub(crate) fn write(&self, bytes: &[u8]) {
        self.shared.enqueue(bytes);
    }

    /// Total bytes discarded by drop-oldest so far. Test/telemetry accessor.
    #[cfg(test)]
    pub(crate) fn dropped_bytes(&self) -> u64 {
        self.shared.lock().dropped_bytes
    }
}

impl Drop for HostPtyWriter {
    fn drop(&mut self) {
        // Signal close and detach, without joining: the writer thread may be
        // parked in a blocking fd write on a wedged PTY slave, and a join
        // here would transfer that unbounded wait onto whoever drops the
        // handle (the host teardown path). The thread exits on its own when
        // the write returns or errors and it observes `closed` - the same
        // non-joining teardown the app-side writer shim uses.
        {
            let mut queue = self.shared.lock();
            queue.closed = true;
        }
        self.shared.ready.notify_all();
    }
}

fn writer_loop(shared: &Shared, writer: &mut Box<dyn Write + Send>) {
    loop {
        let chunk = {
            let mut queue = shared.lock();
            loop {
                if let Some(chunk) = queue.chunks.pop_front() {
                    queue.queued_bytes -= chunk.len();
                    break chunk;
                }
                if queue.closed {
                    return;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        // Blocking fd write happens here, OFF the host loop. A wedged slave parks
        // this thread, not the loop; the host stays responsive and the queue cap
        // bounds memory. A write error (e.g. EIO once the child is gone) closes
        // the writer: the host loop detects child exit independently via its
        // `try_wait` fallback and the PTY-EOF path, so nothing here needs to
        // surface the error.
        if writer
            .write_all(&chunk)
            .and_then(|()| writer.flush())
            .is_err()
        {
            let mut queue = shared.lock();
            queue.closed = true;
            queue.chunks.clear();
            queue.queued_bytes = 0;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::mpsc;

    /// A writer whose `write` blocks forever on the first call, modelling a slave
    /// that has stopped reading its input queue.
    struct WedgedWriter {
        started: mpsc::Sender<()>,
        block: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Write for WedgedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let _ = self.started.send(());
            let (lock, cvar) = &*self.block;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn enqueue_never_blocks_when_the_slave_stops_reading() {
        // The writer thread parks in the very first fd write; a producer that
        // enqueues afterwards must still return promptly (a
        // wedged slave cannot stall the host loop).
        let (started_tx, started_rx) = mpsc::channel();
        let block = Arc::new((Mutex::new(false), Condvar::new()));
        let writer = WedgedWriter {
            started: started_tx,
            block: Arc::clone(&block),
        };
        let pty = HostPtyWriter::spawn_with_cap(Box::new(writer), 64 * 1024).expect("spawn writer");

        // Prime the wedge: the first chunk enters the fd write and parks.
        pty.write(b"first");
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("writer thread should reach the blocking fd write");

        // Every subsequent enqueue must return without waiting on the parked fd.
        let start = Instant::now();
        for _ in 0..1000 {
            pty.write(b"more input while the slave is wedged");
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "enqueue blocked behind the wedged fd write"
        );

        // Release the wedge so the writer thread can exit.
        {
            let (lock, cvar) = &*block;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        drop(pty);
    }

    #[test]
    fn drop_oldest_bounds_memory_when_the_consumer_is_wedged() {
        let (started_tx, started_rx) = mpsc::channel();
        let block = Arc::new((Mutex::new(false), Condvar::new()));
        let writer = WedgedWriter {
            started: started_tx,
            block: Arc::clone(&block),
        };
        let cap = 4096;
        let pty = HostPtyWriter::spawn_with_cap(Box::new(writer), cap).expect("spawn writer");

        pty.write(b"prime the wedge");
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("writer thread should reach the blocking fd write");

        // Flood well past the cap; drop-oldest must discard rather than grow or
        // block.
        let chunk = vec![b'x'; 1024];
        for _ in 0..64 {
            pty.write(&chunk);
        }
        assert!(
            pty.dropped_bytes() > 0,
            "drop-oldest never engaged despite exceeding the byte cap"
        );

        {
            let (lock, cvar) = &*block;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        drop(pty);
    }

    #[test]
    fn a_sole_chunk_over_the_cap_is_delivered_whole() {
        let (started_tx, started_rx) = mpsc::channel();
        let block = Arc::new((Mutex::new(false), Condvar::new()));
        let writer = WedgedWriter {
            started: started_tx,
            block: Arc::clone(&block),
        };
        let pty = HostPtyWriter::spawn_with_cap(Box::new(writer), 4096).expect("spawn writer");
        pty.write(b"prime the wedge");
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("writer thread should reach the blocking fd write");

        // One accepted input frame larger than the cap is kept, not discarded.
        pty.write(&vec![b'p'; 3 * 4096]);
        assert_eq!(pty.dropped_bytes(), 0);
        // A newer chunk displaces it as the oldest over-cap chunk.
        pty.write(b"k");
        assert_eq!(pty.dropped_bytes(), 3 * 4096);

        {
            let (lock, cvar) = &*block;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        drop(pty);
    }

    #[test]
    fn overflow_reports_are_rate_limited_without_losing_the_count() {
        let mut queue = Queue {
            chunks: VecDeque::new(),
            queued_bytes: 0,
            closed: false,
            dropped_bytes: 0,
            unlogged_dropped_bytes: 0,
            last_overflow_log: None,
        };
        let start = Instant::now();
        assert_eq!(take_overflow_report(&mut queue, start), None);
        queue.unlogged_dropped_bytes = 10;
        assert_eq!(take_overflow_report(&mut queue, start), Some(10));
        queue.unlogged_dropped_bytes = 5;
        assert_eq!(
            take_overflow_report(&mut queue, start + Duration::from_secs(1)),
            None,
            "a second warning inside the interval is deferred"
        );
        queue.unlogged_dropped_bytes += 7;
        assert_eq!(
            take_overflow_report(&mut queue, start + OVERFLOW_LOG_INTERVAL),
            Some(12),
            "the deferred count carries into the next warning"
        );
    }

    /// Records every byte delivered after the first, wedged write is released.
    struct RecordingWedge {
        started: mpsc::Sender<()>,
        block: Arc<(Mutex<bool>, Condvar)>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for RecordingWedge {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let _ = self.started.send(());
            let (lock, cvar) = &*self.block;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// A bracketed paste sent as one input frame survives overflow whole or is
    /// dropped whole: the shell never sees an end marker without its start.
    #[test]
    fn a_bracketed_paste_frame_is_delivered_or_dropped_whole_under_overflow() {
        const PASTE: &[u8] = b"\x1b[200~rm -rf /tmp/x\necho done\n\x1b[201~";
        for flood_chunks in [0usize, 8] {
            let (started_tx, started_rx) = mpsc::channel();
            let block = Arc::new((Mutex::new(false), Condvar::new()));
            let written = Arc::new(Mutex::new(Vec::new()));
            let writer = RecordingWedge {
                started: started_tx,
                block: Arc::clone(&block),
                written: Arc::clone(&written),
            };
            let pty = HostPtyWriter::spawn_with_cap(Box::new(writer), 64).expect("spawn writer");
            pty.write(b"prime");
            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("writer thread should reach the blocking fd write");

            pty.write(PASTE);
            for _ in 0..flood_chunks {
                pty.write(&[b'k'; 32]);
            }
            let expect_dropped = flood_chunks > 0;
            let last = if expect_dropped { b'k' } else { b'~' };
            {
                let (lock, cvar) = &*block;
                *lock.lock().unwrap() = true;
                cvar.notify_all();
            }
            // Wait for the writer thread to drain what survived.
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if written.lock().unwrap().last() == Some(&last) {
                    break;
                }
                assert!(Instant::now() < deadline, "writer never drained the queue");
                std::thread::yield_now();
            }
            let sink = written.lock().unwrap().clone();
            let has_start = contains(&sink, b"\x1b[200~");
            let has_end = contains(&sink, b"\x1b[201~");
            assert_eq!(has_start, has_end, "paste markers stay paired: {sink:?}");
            assert_eq!(
                contains(&sink, PASTE),
                !expect_dropped,
                "the paste is kept whole or dropped whole (flood={flood_chunks})"
            );
            assert_eq!(contains(&sink, b"rm -rf"), !expect_dropped);
            drop(pty);
        }
    }
}
