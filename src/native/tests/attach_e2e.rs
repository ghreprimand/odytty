// SPDX-License-Identifier: GPL-3.0-only
//! Real-process end-to-end daemon-survival test for resumable sessions.
//!
//! The unit tests cover each side against a stand-in (the attach client vs. an
//! in-process fake host; the host vs. a synthetic client). This test is the
//! missing integration seam: it glues the **real** detached session-host
//! *process* to the **real** native [`AttachClient`] across a true client
//! disconnect, exercising the actual wire format, socket-path resolution, and
//! cross-process daemon survival where a mismatch would otherwise hide.
//!
//! Flow (each step bounded so a stuck host fails the test rather than hanging):
//! 1. spawn a real `odytty session-host` subprocess owning a real PTY child;
//! 2. attach with the real `AttachClient`, restore the snapshot (full
//!    pre-attach scrollback), and drive input the child echoes;
//! 3. drop the client (clean detach — the window-close path) and assert the
//!    host process SURVIVES with its PTY + scrollback intact;
//! 4. reattach by id with a fresh client and assert the restored snapshot
//!    carries the scrollback produced before + during the first attach, plus
//!    the mid-attach output (proving it landed in the host's terminal model);
//! 5. end the child and assert the host process reaps cleanly — no orphaned
//!    daemon and no stale socket.
//!
//! Hermetic: a synthetic runtime base under the OS temp dir (the host creates
//! and `0700`-locks its own `odytty/` runtime dir inside it), a trivial
//! controlled `/bin/sh` child, and bounded timeouts. No real user data.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::core::Terminal;
use crate::native::attach::{AttachClient, resolve_session_socket};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

/// Locate the built `odytty` binary next to the test executable
/// (`target/<profile>/odytty`). `cargo test` builds the bin automatically, so it
/// is present under the standard test gate; a clear panic guides a bare
/// `--lib`-without-build run.
fn odytty_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("resolve test executable");
    let deps = exe.parent().expect("test exe has a parent dir");
    let profile = deps.parent().expect("deps has a parent dir");
    for candidate in [profile.join("odytty"), deps.join("odytty")] {
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "odytty binary not found near {} — run `cargo build` first (cargo test builds it automatically)",
        profile.display()
    );
}

/// A unique synthetic runtime base under the OS temp dir. The host creates and
/// `0700`-locks `<base>/odytty/` itself; we only own the outer dir for cleanup.
fn unique_base() -> PathBuf {
    // Keep this base SHORT: the host binds `<base>/odytty/session-<id>.sock`
    // (plus a `.sock.lock`), and on macOS the temp base is a long
    // `/var/folders/.../T/` path, so a verbose unique dir overflows the 104-byte
    // `AF_UNIX` sun_path limit and `bind()` fails.
    let base = std::env::temp_dir().join(format!(
        "ode_{}_{}",
        std::process::id(),
        UNIQUE.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&base).expect("create runtime base");
    base
}

/// Spawns the real session-host subprocess and reaps it on drop so a failed
/// assertion never leaks a daemon or a temp dir.
struct HostGuard {
    child: Child,
    base: PathBuf,
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Spawn `odytty session-host` as a genuine separate process with a controlled
/// `/bin/sh` child, a synthetic runtime base, and a long idle timeout (so a
/// detach never idle-kills it mid-test). `--exec` must be last (it consumes the
/// remaining args as the child argv).
fn spawn_real_host(bin: &Path, base: &Path, id: &str, child_command: &str) -> Child {
    Command::new(bin)
        .arg("session-host")
        .arg("--session-id")
        .arg(id)
        .arg("--runtime-dir")
        .arg(base)
        .arg("--idle-timeout-ms")
        .arg("60000")
        .arg("--max-scrollback-rows")
        .arg("2000")
        .arg("--cols")
        .arg("80")
        .arg("--rows")
        .arg("24")
        .arg("--exec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(child_command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real session-host subprocess")
}

/// Poll until the host has bound its per-user socket (resolved exactly as the
/// production attach path resolves it), or panic at the deadline.
fn wait_for_socket(base: &Path, id: &str, timeout: Duration) -> PathBuf {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(socket) = resolve_session_socket(Some(base), id) {
            return socket;
        }
        if Instant::now() >= deadline {
            panic!("session-host socket for {id} not ready within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Poll `cond` until true or the deadline; returns whether it became true.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Attach once, restore the snapshot, and return the mirror terminal if `accept`
/// passes; otherwise cleanly detach and return `None`. A connect/snapshot error
/// (e.g. the previous client's detach not yet processed, or a transient
/// snapshot read hiccup on a slow runner) also maps to `None` so the poll
/// retries rather than failing — the read is repeated against fresh host state
/// instead of trusting a single timing-sensitive attempt.
fn try_attach_matching(
    socket: &Path,
    id: &str,
    accept: impl Fn(&Terminal) -> bool,
) -> Option<Terminal> {
    let (client, reader, terminal) = AttachClient::connect(socket, id).ok()?;
    let matched = accept(&terminal);
    // Clean detach: the host keeps the session alive for the next attach.
    drop(client);
    drop(reader);
    matched.then_some(terminal)
}

/// Poll [`try_attach_matching`] until the restored snapshot satisfies `accept`
/// or the deadline elapses. This is the deterministic replacement for "sleep a
/// fixed amount, then attach once and hope the host's terminal model already
/// reflects the expected state" — host-side work (the child printing, or an echo
/// of mid-attach input folding into the model) is observed by re-reading the
/// snapshot until it appears, not by a fixed wait. Sequential attaches only
/// (each fully detached before the next), so the single-client host invariant
/// holds.
fn poll_attach_until(
    socket: &Path,
    id: &str,
    timeout: Duration,
    accept: impl Fn(&Terminal) -> bool,
) -> Terminal {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(terminal) = try_attach_matching(socket, id, &accept) {
            return terminal;
        }
        if Instant::now() >= deadline {
            panic!("attach snapshot never satisfied the expected condition within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn real_host_survives_detach_and_reattach_restores_scrollback() {
    let bin = odytty_bin();
    let base = unique_base();
    let id = "esurv";
    // 40 numbered lines push early output into scrollback (80x24 grid); `cat`
    // keeps the child alive across detach until we send EOF (Ctrl-D).
    let child_command = "i=1; while [ $i -le 40 ]; do echo \"L$i\"; i=$((i+1)); done; cat";
    let mut guard = HostGuard {
        child: spawn_real_host(&bin, &base, id, child_command),
        base: base.clone(),
    };

    let socket = wait_for_socket(&base, id, Duration::from_secs(30));

    // --- Attach #1: poll until the child has printed all 40 lines, so the
    //     snapshot deterministically carries the pre-attach scrollback (no fixed
    //     "sleep 400ms and hope the child finished"). ---
    let term1 = poll_attach_until(&socket, id, Duration::from_secs(15), |t| {
        t.screen().scrollback_len() > 0 && t.screen().plain_text().contains("L40")
    });
    let scrollback1 = term1.screen().scrollback_len();
    assert!(
        scrollback1 > 0,
        "early lines must have scrolled into history: scrollback_len={scrollback1}"
    );
    assert!(
        term1.screen().plain_text().contains("L40"),
        "the latest printed line must be on screen:\n{}",
        term1.screen().plain_text()
    );
    drop(term1);

    // --- Produce output INTO the host PTY during a real attach. It must fold
    //     into the host's own terminal model (proven by the reattach snapshot).
    //     The Input frame is ordered before the Detach on the same stream, so
    //     the host receives the keystrokes before the detach; the echo folding
    //     into the model is async and is awaited by the reattach poll below. ---
    let (mut client1, reader1, _term1live) =
        AttachClient::connect(&socket, id).expect("second attach connects for mid-attach input");
    client1
        .send_input(b"MIDLINE\n")
        .expect("send mid-attach input");

    // --- Detach #1 == window close: drop the client (clean Detach frame). ---
    drop(client1);
    drop(reader1);

    // The host process MUST survive a client disconnect (daemon survival). Give
    // it a moment to process the detach, then assert it is still running. (A
    // negative "stays alive" assertion has no condition to poll toward, so a
    // short bounded settle is appropriate here.)
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        guard.child.try_wait().expect("poll host process").is_none(),
        "host process must survive the client detach"
    );
    assert!(socket.exists(), "socket must remain after a clean detach");

    // --- Reattach #2 by id: poll until the mid-attach output has folded into
    //     the host model AND scrollback survived, instead of a single
    //     timing-sensitive snapshot read. The id must resolve to the same
    //     per-user socket. ---
    let socket2 = resolve_session_socket(Some(&base), id).expect("resolve session by id");
    assert_eq!(
        socket2, socket,
        "the id resolves to the same per-user socket"
    );
    let term2 = poll_attach_until(&socket2, id, Duration::from_secs(15), |t| {
        let text = t.screen().plain_text();
        t.screen().scrollback_len() >= scrollback1
            && text.contains("L40")
            && text.contains("MIDLINE")
    });
    assert!(
        term2.screen().scrollback_len() >= scrollback1,
        "scrollback must survive the detach (>= pre-detach length): before={scrollback1} after={}",
        term2.screen().scrollback_len()
    );
    assert!(
        term2.screen().plain_text().contains("L40"),
        "the recent screen content must repaint on reattach:\n{}",
        term2.screen().plain_text()
    );
    assert!(
        term2.screen().plain_text().contains("MIDLINE"),
        "mid-attach output must have landed in the host model and repaint on reattach:\n{}",
        term2.screen().plain_text()
    );
    drop(term2);

    // --- End the child: EOF to `cat` -> child exits -> host reaps + exits. A
    //     fresh client sends the EOF (the poll above already detached). ---
    let (mut client3, reader3, _term3) =
        AttachClient::connect(&socket, id).expect("attach connects to send EOF");
    client3
        .send_input(&[0x04])
        .expect("send EOF to the hosted child");
    drop(client3);
    drop(reader3);

    // Clean reaping: the host process exits (no orphaned daemon)...
    let exited = wait_until(Duration::from_secs(5), || {
        guard.child.try_wait().expect("poll host process").is_some()
    });
    assert!(
        exited,
        "host must exit once its child shell exits (no orphaned daemon)"
    );
    // ...and removes its own socket on the way out (no stale socket).
    assert!(
        wait_until(Duration::from_secs(2), || !socket.exists()),
        "host must remove its socket on exit (no stale socket left behind)"
    );
}
