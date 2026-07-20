// SPDX-License-Identifier: GPL-3.0-only
//! Non-blocking PTY write path (FREEZE-REMOTE-FIX).
//!
//! Field diagnosis of an unresponsive remote tab found every PTY write sharing
//! one `Arc<Mutex<Box<dyn Write>>>` over a *blocking* master fd: when an `ssh`
//! child stopped draining its stdin PTY (a full session-channel flow-control
//! window), the thread holding the writer lock parked in `write_all` forever and
//! the main-thread input path deadlocked acquiring that same lock — a frozen UI
//! on a fully healthy connection.
//!
//! The write path now runs the blocking fd write on a dedicated per-session
//! writer thread fed by a bounded in-memory queue. Producers — the main input
//! path, the paste path, the output pump's host replies — only ever *enqueue*:
//! an O(1) push under a briefly-held queue lock that never spans an fd write, so
//! a flow-controlled or wedged remote can never stall a producer again. The
//! existing `PtyWriter` (`Arc<Mutex<Box<dyn Write>>>`) is retained unchanged; the
//! boxed writer is now an [`OutboundShim`] whose `write` enqueues, so every
//! existing `writer.lock().write_all(..)` call site keeps working while the
//! retained mutex only ever guards an enqueue — never fd I/O.
//!
//! Overflow policy: the queue is byte-bounded ([`QUEUE_BYTE_CAP`]). When a
//! stalled consumer lets it exceed the cap the OLDEST buffered chunks are
//! dropped — never a blocking enqueue, because a wedged remote must not
//! propagate backpressure into the UI thread — and the discarded byte count is
//! surfaced by the monitor. Dropping terminal input corrupts a stalled stream,
//! but that only happens once a remote is already wedged; a live UI is the
//! higher priority.
//!
//! The chunk is the atomicity unit of that policy: overflow drops whole
//! chunks, never partial ones, and the newest chunk is never dropped. Anything
//! whose framing must not tear therefore travels as ONE chunk — a bracketed
//! paste (start marker + body + end marker) is enqueued as a single write, so
//! an overflow either delivers the paste intact or discards it entirely. It
//! can never drop the start marker while keeping the tail, which would let the
//! surviving newlines execute outside bracketed-paste mode in the shell.
//!
//! Telemetry (default-on, privacy-safe — counters and a numeric session id only,
//! never PTY bytes): the writer thread stamps [`OutboundShared::write_started_ms`]
//! before each fd write and clears it after. One detached monitor thread polls
//! every registered session and emits a single `pty_write_stall` line for a
//! write in flight past [`WRITE_STALL_AFTER`], plus a `pty_write_overflow` line
//! when drop-oldest has discarded data. This is independent of the presented-
//! frame freeze watchdog, which did not classify the field freeze.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use super::session::SessionToken;

/// Maximum bytes buffered in one session's outbound queue before drop-oldest
/// engages. Generous enough that a legitimate multi-megabyte paste into a
/// healthy shell never drops (it drains far faster than it fills); bounded so a
/// fully wedged remote cannot grow unbounded memory.
const QUEUE_BYTE_CAP: usize = 4 * 1024 * 1024;
/// A single fd write in flight longer than this is treated as a stall and
/// logged once by the monitor.
const WRITE_STALL_AFTER: Duration = Duration::from_secs(3);
/// Monitor poll cadence. Coarse: near-zero idle cost, seconds-scale detection.
const MONITOR_POLL: Duration = Duration::from_secs(1);

/// Queue state guarded by [`OutboundShared::queue`].
struct OutboundQueue {
    chunks: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    /// Set once the writer handle is dropped or an fd write fails; the writer
    /// thread drains what it can and exits, and further enqueues are discarded.
    closed: bool,
    /// Bytes discarded by drop-oldest since the monitor last reported them.
    dropped_bytes: u64,
}

/// Shared state between the producing threads (via [`OutboundShim`]), the
/// dedicated writer thread, and the stall monitor.
struct OutboundShared {
    session: SessionToken,
    epoch: Instant,
    byte_cap: usize,
    queue: Mutex<OutboundQueue>,
    ready: Condvar,
    /// ms-offset from `epoch` when the current fd write began; 0 = idle. Read by
    /// the monitor to detect a stalled write without touching the writer thread.
    write_started_ms: AtomicU64,
    /// A stall has already been logged for the current in-flight write.
    stall_logged: AtomicBool,
}

impl OutboundShared {
    fn new(session: SessionToken) -> Self {
        Self::with_cap(session, QUEUE_BYTE_CAP)
    }

    fn with_cap(session: SessionToken, byte_cap: usize) -> Self {
        Self {
            session,
            epoch: Instant::now(),
            byte_cap,
            queue: Mutex::new(OutboundQueue {
                chunks: VecDeque::new(),
                queued_bytes: 0,
                closed: false,
                dropped_bytes: 0,
            }),
            ready: Condvar::new(),
            write_started_ms: AtomicU64::new(0),
            stall_logged: AtomicBool::new(false),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn lock_queue(&self) -> MutexGuard<'_, OutboundQueue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enqueue a copy of `bytes`, applying the byte-cap drop-oldest policy. Never
    /// blocks on the fd — only the briefly-held queue lock is taken.
    fn enqueue(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        {
            let mut queue = self.lock_queue();
            if queue.closed {
                return;
            }
            queue.chunks.push_back(bytes.to_vec());
            queue.queued_bytes += bytes.len();
            drop_oldest_over_cap(&mut queue, self.byte_cap);
        }
        self.ready.notify_one();
    }

    /// Signal the writer thread to drain and exit. Non-blocking: it never joins.
    fn close(&self) {
        self.lock_queue().closed = true;
        self.ready.notify_all();
    }

    fn queued_bytes(&self) -> usize {
        self.lock_queue().queued_bytes
    }

    /// The stall decision + record for the monitor. `None` unless a write has
    /// been in flight past the threshold and has not already been logged for the
    /// current episode.
    fn stall_record(&self, now_ms: u64) -> Option<String> {
        let started = self.write_started_ms.load(Ordering::Relaxed);
        let stalled_secs = evaluate_write_stall(now_ms, started, WRITE_STALL_AFTER)?;
        if self.stall_logged.swap(true, Ordering::Relaxed) {
            return None;
        }
        Some(format!(
            "pty_write_stall session={} stalled={}s queued_bytes={}",
            self.session.0,
            stalled_secs,
            self.queued_bytes(),
        ))
    }

    /// A drop-oldest overflow record, consuming the accrued counter so each
    /// discarded episode is reported once. `None` when nothing was dropped.
    fn overflow_record(&self) -> Option<String> {
        let mut queue = self.lock_queue();
        if queue.dropped_bytes == 0 {
            return None;
        }
        let dropped = queue.dropped_bytes;
        queue.dropped_bytes = 0;
        Some(format!(
            "pty_write_overflow session={} dropped_bytes={dropped}",
            self.session.0,
        ))
    }
}

/// Drop whole chunks from the FRONT until the queue is within `byte_cap`, but
/// never drop the most recently enqueued chunk (so a single over-cap chunk is
/// still delivered). Discarded bytes accrue into `dropped_bytes` for the
/// monitor.
fn drop_oldest_over_cap(queue: &mut OutboundQueue, byte_cap: usize) {
    while queue.queued_bytes > byte_cap && queue.chunks.len() > 1 {
        if let Some(dropped) = queue.chunks.pop_front() {
            queue.queued_bytes = queue.queued_bytes.saturating_sub(dropped.len());
            let dropped_len = u64::try_from(dropped.len()).unwrap_or(u64::MAX);
            queue.dropped_bytes = queue.dropped_bytes.saturating_add(dropped_len);
        }
    }
}

/// Whether a write started at `started_ms` (0 = idle) has been in flight at
/// `now_ms` for at least `threshold`, and if so for how many whole seconds.
fn evaluate_write_stall(now_ms: u64, started_ms: u64, threshold: Duration) -> Option<u64> {
    if started_ms == 0 {
        return None;
    }
    let elapsed = now_ms.saturating_sub(started_ms);
    let threshold_ms = u64::try_from(threshold.as_millis()).unwrap_or(u64::MAX);
    if elapsed < threshold_ms {
        return None;
    }
    Some(elapsed / 1000)
}

/// The boxed writer handed to every producer through `PtyWriter`. `write`
/// enqueues (non-blocking); the real fd lives on the writer thread.
struct OutboundShim {
    shared: Arc<OutboundShared>,
}

impl Write for OutboundShim {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.shared.enqueue(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // The writer thread flushes the real fd after every dequeued chunk, so a
        // producer-side flush is a no-op: the bytes are already committed to the
        // queue and are flushed to the fd as they are written.
        Ok(())
    }
}

impl Drop for OutboundShim {
    fn drop(&mut self) {
        // Signal the writer thread to drain and exit. Never joins here — dropping
        // a session writer must not block. The thread releases its fd clone as it
        // exits (bounded because session teardown closes/kills the fd, erroring
        // any in-flight write).
        self.shared.close();
    }
}

/// The dedicated writer loop: owns the real `fd`, drains the queue, and performs
/// the only blocking `write_all`/`flush`. The queue lock is released before each
/// fd write, so producers never contend with a blocked write. Exits when the
/// queue is closed and empty, or when an fd write fails (teardown).
fn run_writer(shared: Arc<OutboundShared>, mut fd: Box<dyn Write + Send>) {
    loop {
        let chunk = {
            let mut queue = shared.lock_queue();
            loop {
                if let Some(chunk) = queue.chunks.pop_front() {
                    queue.queued_bytes = queue.queued_bytes.saturating_sub(chunk.len());
                    break Some(chunk);
                }
                if queue.closed {
                    break None;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(chunk) = chunk else {
            return;
        };
        shared
            .write_started_ms
            .store(shared.now_ms().max(1), Ordering::Relaxed);
        shared.stall_logged.store(false, Ordering::Relaxed);
        let result = fd.write_all(&chunk).and_then(|()| fd.flush());
        shared.write_started_ms.store(0, Ordering::Relaxed);
        if result.is_err() {
            // The fd is gone (child reaped / link torn down). Mark closed so
            // further enqueues are discarded, then stop and release the fd.
            shared.lock_queue().closed = true;
            return;
        }
    }
}

/// Spawn the dedicated writer thread for `fd` and return the producer-side
/// [`OutboundShim`] boxed as the inner writer for a `PtyWriter`. Registers the
/// session with the stall monitor (lazily spawning it on first use).
///
/// Platform-neutral: the shim, queue, writer thread, and monitor are pure `std`
/// and share the Unix PTY and Windows ConPTY write paths alike. The writer
/// thread owns the sole fd/handle clone and releases it on exit, so no fd or
/// ConPTY handle leaks past session teardown.
pub(super) fn writer_shim(
    fd: Box<dyn Write + Send>,
    session: SessionToken,
) -> io::Result<Box<dyn Write + Send>> {
    let shared = Arc::new(OutboundShared::new(session));
    register(&shared);
    let thread_shared = shared.clone();
    // A writer-thread spawn only fails under resource exhaustion (the thread
    // ceiling / address-space limits), which is a per-session condition. Return
    // it as a recoverable error so the caller reports one failed session rather
    // than aborting the whole process through the panic hook. The registry
    // `Weak` for `shared` expires on its own when `shared` drops here.
    std::thread::Builder::new()
        .name(format!("odytty-pty-writer-{}", session.0))
        .spawn(move || run_writer(thread_shared, fd))?;
    Ok(Box::new(OutboundShim { shared }))
}

/// Registry of live per-session outbound handles, polled by the single monitor
/// thread. `Weak` so a closed session's entry expires on its own.
struct Registry {
    entries: Vec<Weak<OutboundShared>>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static MONITOR_STARTED: AtomicBool = AtomicBool::new(false);

fn register(shared: &Arc<OutboundShared>) {
    let registry = REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            entries: Vec::new(),
        })
    });
    {
        let mut guard = registry.lock().unwrap_or_else(PoisonError::into_inner);
        guard.entries.retain(|weak| weak.strong_count() > 0);
        guard.entries.push(Arc::downgrade(shared));
    }
    if !MONITOR_STARTED.swap(true, Ordering::SeqCst) {
        spawn_monitor(registry);
    }
}

fn spawn_monitor(registry: &'static Mutex<Registry>) {
    let _ = std::thread::Builder::new()
        .name("odytty-pty-write-monitor".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(MONITOR_POLL);
                let live: Vec<Arc<OutboundShared>> = {
                    let mut guard = registry.lock().unwrap_or_else(PoisonError::into_inner);
                    guard.entries.retain(|weak| weak.strong_count() > 0);
                    guard.entries.iter().filter_map(Weak::upgrade).collect()
                };
                for shared in live {
                    if let Some(record) = shared.stall_record(shared.now_ms()) {
                        tracing::warn!("{record}");
                    }
                    if let Some(record) = shared.overflow_record() {
                        tracing::warn!("{record}");
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    /// A `Write` that blocks in `write` until a gate is opened, recording each
    /// completed write. Models a stalled (flow-controlled) fd deterministically.
    struct BlockingWriter {
        gate: Arc<(Mutex<bool>, Condvar)>,
        started: Arc<AtomicUsize>,
        written: Arc<Mutex<Vec<u8>>>,
        error_on_release: bool,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.started.fetch_add(1, Ordering::SeqCst);
            let (lock, cvar) = &*self.gate;
            let mut open = lock.lock().unwrap_or_else(PoisonError::into_inner);
            while !*open {
                open = cvar.wait(open).unwrap_or_else(PoisonError::into_inner);
            }
            if self.error_on_release {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "stalled fd released as error",
                ));
            }
            self.written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn new_gate() -> Arc<(Mutex<bool>, Condvar)> {
        Arc::new((Mutex::new(false), Condvar::new()))
    }

    fn open_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, cvar) = &**gate;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cvar.notify_all();
    }

    fn wait_until(mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cond() {
            assert!(
                Instant::now() < deadline,
                "condition not met before timeout"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn run_with_timeout(timeout: Duration, task: impl FnOnce() + Send + 'static) -> bool {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            task();
            let _ = tx.send(());
        });
        rx.recv_timeout(timeout).is_ok()
    }

    fn join_with_timeout(handle: std::thread::JoinHandle<()>, timeout: Duration) -> bool {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });
        rx.recv_timeout(timeout).is_ok()
    }

    #[test]
    fn evaluate_write_stall_reports_only_a_long_in_flight_write() {
        let threshold = Duration::from_secs(3);
        // Idle (0) is never a stall.
        assert_eq!(evaluate_write_stall(10_000, 0, threshold), None);
        // In flight but under the threshold.
        assert_eq!(evaluate_write_stall(2_500, 1, threshold), None);
        // In flight past the threshold: whole seconds elapsed.
        assert_eq!(evaluate_write_stall(5_000, 1, threshold), Some(4));
    }

    #[test]
    fn enqueue_drops_oldest_chunks_when_over_the_byte_cap() {
        let shared = OutboundShared::with_cap(SessionToken(3), 8);
        shared.enqueue(b"aaaa"); // 4 bytes
        shared.enqueue(b"bbbb"); // 8 bytes, at cap
        shared.enqueue(b"cccc"); // 12 > 8: drop oldest "aaaa" back to 8
        let queue = shared.lock_queue();
        assert_eq!(queue.queued_bytes, 8);
        assert_eq!(queue.dropped_bytes, 4);
        let remaining: Vec<&[u8]> = queue.chunks.iter().map(Vec::as_slice).collect();
        assert_eq!(remaining, vec![b"bbbb".as_slice(), b"cccc".as_slice()]);
    }

    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn bracketed_paste_survives_overflow_intact_when_it_is_the_newest_chunk() {
        // Stalled-writer framing test, delivery half. The writer thread is
        // parked in a blocked fd write; the queue is pre-filled past the cap
        // and then a multiline bracketed paste (ONE chunk, see
        // `clipboard::encode_paste_chunks`) is enqueued over it. Drop-oldest
        // sheds the pre-fill but never the newest chunk, so once the fd
        // releases the sink must see the COMPLETE framed paste, contiguous,
        // with both markers.
        let paste_chunks =
            crate::native::clipboard::encode_paste_chunks("line one\nline two\n", true, 4);
        assert_eq!(paste_chunks.len(), 1, "bracketed paste must be one chunk");
        let paste = &paste_chunks[0];

        let shared = Arc::new(OutboundShared::with_cap(SessionToken(11), 16));
        let gate = new_gate();
        let started = Arc::new(AtomicUsize::new(0));
        let written = Arc::new(Mutex::new(Vec::new()));
        let fd = BlockingWriter {
            gate: gate.clone(),
            started: started.clone(),
            written: written.clone(),
            error_on_release: false,
        };
        let thread_shared = shared.clone();
        let handle = std::thread::spawn(move || run_writer(thread_shared, Box::new(fd)));

        // Park the writer thread in a blocked fd write; the queue is now empty.
        shared.enqueue(b"first");
        wait_until(|| started.load(Ordering::SeqCst) >= 1);

        // Pre-fill past the cap, then enqueue the paste over it.
        shared.enqueue(b"fill-fill-fill-fill");
        shared.enqueue(paste);

        shared.close();
        open_gate(&gate);
        handle.join().expect("writer thread joins");

        let sink = written.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            contains_subslice(&sink, paste),
            "sink must contain the complete framed paste contiguously"
        );
    }

    #[test]
    fn bracketed_paste_dropped_by_overflow_leaves_no_fragment() {
        // Stalled-writer framing test, discard half. The paste is enqueued
        // first and NEWER traffic overflows the queue past the cap: the paste
        // is dropped as a whole chunk. The sink must then contain no paste
        // bytes at all — no start marker, no end marker, no body — because a
        // surviving tail without its start marker would execute outside
        // bracketed-paste mode.
        let paste_chunks =
            crate::native::clipboard::encode_paste_chunks("rm -rf /tmp/x\necho done\n", true, 4);
        assert_eq!(paste_chunks.len(), 1, "bracketed paste must be one chunk");
        let paste = &paste_chunks[0];

        let shared = Arc::new(OutboundShared::with_cap(SessionToken(12), 16));
        let gate = new_gate();
        let started = Arc::new(AtomicUsize::new(0));
        let written = Arc::new(Mutex::new(Vec::new()));
        let fd = BlockingWriter {
            gate: gate.clone(),
            started: started.clone(),
            written: written.clone(),
            error_on_release: false,
        };
        let thread_shared = shared.clone();
        let handle = std::thread::spawn(move || run_writer(thread_shared, Box::new(fd)));

        // Park the writer thread in a blocked fd write; the queue is now empty.
        shared.enqueue(b"first");
        wait_until(|| started.load(Ordering::SeqCst) >= 1);

        // Enqueue the paste, then enough newer input to push far past the cap
        // so drop-oldest discards the paste chunk whole.
        shared.enqueue(paste);
        shared.enqueue(b"newer-key-input-1");
        shared.enqueue(b"newer-key-input-2");

        shared.close();
        open_gate(&gate);
        handle.join().expect("writer thread joins");

        let sink = written.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            !contains_subslice(&sink, b"\x1b[200~"),
            "no orphaned start marker in the sink"
        );
        assert!(
            !contains_subslice(&sink, b"\x1b[201~"),
            "no orphaned end marker in the sink"
        );
        assert!(
            !contains_subslice(&sink, b"rm -rf"),
            "no paste body fragment in the sink"
        );
    }

    #[test]
    fn stall_and_overflow_records_are_state_only() {
        let shared = OutboundShared::with_cap(SessionToken(9), QUEUE_BYTE_CAP);
        // No in-flight write: no stall record.
        assert!(shared.stall_record(shared.now_ms()).is_none());
        // Simulate a write in flight since t=1ms; at t=5000ms it is a 4s stall.
        shared.write_started_ms.store(1, Ordering::Relaxed);
        assert_eq!(
            shared.stall_record(5_000).as_deref(),
            Some("pty_write_stall session=9 stalled=4s queued_bytes=0"),
        );
        // Once-only per episode.
        assert!(shared.stall_record(6_000).is_none());

        let overflow = OutboundShared::with_cap(SessionToken(4), 4);
        overflow.enqueue(b"aaaa"); // at cap
        overflow.enqueue(b"bbbb"); // drop oldest "aaaa"
        assert_eq!(
            overflow.overflow_record().as_deref(),
            Some("pty_write_overflow session=4 dropped_bytes=4"),
        );
        // Consumed: reported once.
        assert!(overflow.overflow_record().is_none());
    }

    #[test]
    fn enqueue_never_blocks_while_the_fd_write_is_stalled() {
        // The fix's core contract: with the writer thread parked in a blocked fd
        // write, producers still enqueue without blocking.
        let shared = Arc::new(OutboundShared::new(SessionToken(1)));
        let gate = new_gate();
        let started = Arc::new(AtomicUsize::new(0));
        let written = Arc::new(Mutex::new(Vec::new()));
        let fd = BlockingWriter {
            gate: gate.clone(),
            started: started.clone(),
            written: written.clone(),
            error_on_release: false,
        };
        let thread_shared = shared.clone();
        let handle = std::thread::spawn(move || run_writer(thread_shared, Box::new(fd)));

        // The first chunk is dequeued and the fd write blocks.
        shared.enqueue(b"first");
        wait_until(|| started.load(Ordering::SeqCst) >= 1);

        // Further enqueues must return promptly (buffered, not blocked on the fd).
        let enqueued = run_with_timeout(Duration::from_secs(2), {
            let shared = shared.clone();
            move || {
                shared.enqueue(b"second");
                shared.enqueue(b"third");
            }
        });
        assert!(
            enqueued,
            "enqueue blocked while the writer thread was stalled on the fd"
        );
        assert!(shared.queued_bytes() >= b"second".len() + b"third".len());

        // Release the fd: the thread drains the whole FIFO in order, then exits
        // cleanly once closed.
        shared.close();
        open_gate(&gate);
        handle.join().expect("writer thread joins");
        assert_eq!(
            &*written.lock().unwrap_or_else(PoisonError::into_inner),
            b"firstsecondthird",
        );
    }

    #[test]
    fn old_shared_mutex_shape_deadlocks_a_second_writer() {
        // Fail-before contrast: the pre-fix shape (`Arc<Mutex<Box<dyn Write>>>`
        // over a blocking fd) parks the first writer in `write_all` while holding
        // the lock, so a second writer — the main-thread input path in the field
        // incident — deadlocks acquiring it. Asserted via a bounded wait that must
        // TIME OUT, then released so both complete. The new path (test above)
        // never blocks the second producer.
        let gate = new_gate();
        let started = Arc::new(AtomicUsize::new(0));
        let fd: Box<dyn Write + Send> = Box::new(BlockingWriter {
            gate: gate.clone(),
            started: started.clone(),
            written: Arc::new(Mutex::new(Vec::new())),
            error_on_release: false,
        });
        let old: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(fd));

        let first = old.clone();
        let h1 = std::thread::spawn(move || {
            let mut guard = first.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = guard.write_all(b"a");
        });
        wait_until(|| started.load(Ordering::SeqCst) >= 1);

        let (tx, rx) = mpsc::channel();
        let second = old.clone();
        let h2 = std::thread::spawn(move || {
            let mut guard = second.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = guard.write_all(b"b");
            let _ = tx.send(());
        });

        // While the first writer holds the lock across the blocked fd write, the
        // second cannot progress.
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "old shared-mutex shape did not deadlock the second writer",
        );

        open_gate(&gate);
        assert!(
            rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "second writer never completed after the fd released",
        );
        h1.join().expect("first joins");
        h2.join().expect("second joins");
    }

    #[test]
    fn writer_thread_exits_cleanly_on_close_during_a_stall() {
        // Session close while a remote is wedged: close is signalled, the fd
        // releases as an error (link torn down), and the writer thread exits
        // without lingering (no fd/handle leak).
        let shared = Arc::new(OutboundShared::new(SessionToken(2)));
        let gate = new_gate();
        let started = Arc::new(AtomicUsize::new(0));
        let fd = BlockingWriter {
            gate: gate.clone(),
            started: started.clone(),
            written: Arc::new(Mutex::new(Vec::new())),
            error_on_release: true,
        };
        let thread_shared = shared.clone();
        let handle = std::thread::spawn(move || run_writer(thread_shared, Box::new(fd)));

        shared.enqueue(b"x");
        wait_until(|| started.load(Ordering::SeqCst) >= 1);
        shared.close();
        open_gate(&gate);
        assert!(
            join_with_timeout(handle, Duration::from_secs(2)),
            "writer thread did not exit after close during a stall",
        );
    }

    #[test]
    fn writer_shim_is_fallible_and_the_happy_path_enqueues_through_the_thread() {
        // F18 regression: `writer_shim` returns a `Result` instead of aborting
        // the whole process when its writer thread cannot spawn. On the normal
        // path it returns `Ok`, and bytes written to the returned shim reach the
        // fd via the spawned writer thread. (A real spawn failure only occurs
        // under thread-ceiling exhaustion, which is not reproduced here.)
        let written = Arc::new(Mutex::new(Vec::new()));
        let gate = new_gate();
        open_gate(&gate); // fd never blocks
        let fd = BlockingWriter {
            gate: gate.clone(),
            started: Arc::new(AtomicUsize::new(0)),
            written: written.clone(),
            error_on_release: false,
        };

        let mut shim = writer_shim(Box::new(fd), SessionToken(42)).expect("writer thread spawns");
        shim.write_all(b"hello").expect("enqueue");
        shim.flush().expect("flush");
        // Dropping the shim signals the writer thread to drain and exit.
        drop(shim);

        wait_until(|| {
            written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_slice()
                == b"hello"
        });
    }

    #[test]
    fn writer_drains_remaining_queue_on_clean_close() {
        // A clean close (healthy fd) drains everything already queued before the
        // thread exits.
        let shared = Arc::new(OutboundShared::new(SessionToken(5)));
        let gate = new_gate();
        open_gate(&gate); // fd never blocks
        let written = Arc::new(Mutex::new(Vec::new()));
        let fd = BlockingWriter {
            gate: gate.clone(),
            started: Arc::new(AtomicUsize::new(0)),
            written: written.clone(),
            error_on_release: false,
        };
        let thread_shared = shared.clone();
        let handle = std::thread::spawn(move || run_writer(thread_shared, Box::new(fd)));

        shared.enqueue(b"one");
        shared.enqueue(b"two");
        shared.close();
        handle.join().expect("writer thread joins");
        assert_eq!(
            &*written.lock().unwrap_or_else(PoisonError::into_inner),
            b"onetwo",
        );
    }
}
