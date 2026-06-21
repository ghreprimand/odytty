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
    let base = std::env::temp_dir().join(format!(
        "odytty_e2e_{}_{}",
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

#[test]
fn real_host_survives_detach_and_reattach_restores_scrollback() {
    let bin = odytty_bin();
    let base = unique_base();
    let id = "e2e-survive";
    // 40 numbered lines push early output into scrollback (80x24 grid); `cat`
    // keeps the child alive across detach until we send EOF (Ctrl-D).
    let child_command = "i=1; while [ $i -le 40 ]; do echo \"L$i\"; i=$((i+1)); done; cat";
    let mut guard = HostGuard {
        child: spawn_real_host(&bin, &base, id, child_command),
        base: base.clone(),
    };

    let socket = wait_for_socket(&base, id, Duration::from_secs(5));
    // Let the child finish printing its 40 lines before the first snapshot.
    std::thread::sleep(Duration::from_millis(400));

    // --- Attach #1: the snapshot carries the pre-attach scrollback. ---
    let (mut client1, reader1, term1) =
        AttachClient::connect(&socket, id).expect("first attach connects + restores snapshot");
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

    // --- Produce output INTO the host PTY during the attach. It must fold into
    //     the host's own terminal model (proven by the reattach snapshot). ---
    client1
        .send_input(b"MIDLINE\n")
        .expect("send mid-attach input");
    // Bounded settle so the host reads + echoes the input before we detach.
    std::thread::sleep(Duration::from_millis(300));

    // --- Detach #1 == window close: drop the client (clean Detach frame). ---
    drop(client1);
    drop(reader1);

    // The host process MUST survive a client disconnect (daemon survival). Give
    // it a moment to process the detach, then assert it is still running.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        guard.child.try_wait().expect("poll host process").is_none(),
        "host process must survive the client detach"
    );
    assert!(socket.exists(), "socket must remain after a clean detach");

    // --- Reattach #2 by id with a fresh client: scrollback restored. ---
    let socket2 = resolve_session_socket(Some(&base), id).expect("resolve session by id");
    assert_eq!(
        socket2, socket,
        "the id resolves to the same per-user socket"
    );
    let (mut client2, reader2, term2) =
        AttachClient::connect(&socket2, id).expect("reattach connects + restores snapshot");
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

    // --- End the child: EOF to `cat` -> child exits -> host reaps + exits. ---
    client2
        .send_input(&[0x04])
        .expect("send EOF to the hosted child");
    drop(client2);
    drop(reader2);

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
