// SPDX-License-Identifier: GPL-3.0-only
//! Cross-platform behavior of the Kitty named graphics transports.
//!
//! The unit fixtures for these transports are Unix-only, so the file-transport
//! allowlist entry for the Windows temporary directory and the non-Unix
//! shared-memory rejection have no assertions on the platforms that use them.
//! These cases drive the same behavior through the public terminal surface, so
//! they run on every shipped platform and are checked by the Windows and macOS
//! CI legs rather than inferred from Linux.
//!
//! Everything here uses the platform's own temporary directory rather than a
//! hard-coded path, so no case assumes a Unix filesystem layout.

use odytty::core::Terminal;

/// Minimal base64 encoder: Kitty carries the path or name as base64.
fn base64(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn opted_in_terminal() -> Terminal {
    let mut terminal = Terminal::new(80, 24);
    terminal.set_kitty_named_transports_enabled(true);
    terminal
}

/// `a=T,t=<medium>` for a 2x2 RGBA image, with a request id so the terminal
/// always answers.
fn transport_apc(medium: char, name: &str, id: u32) -> Vec<u8> {
    let encoded = base64(name.as_bytes());
    format!("\x1b_Ga=T,t={medium},f=32,s=2,v=2,i={id};{encoded}\x1b\\").into_bytes()
}

fn response(terminal: &mut Terminal) -> String {
    String::from_utf8_lossy(&terminal.take_host_output()).into_owned()
}

/// A 2x2 opaque white RGBA payload written into the platform temp directory.
fn write_rgba(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, [0xFF_u8; 16])
        .expect("write test image into the platform temp directory");
    path
}

#[test]
fn file_transport_reads_from_the_platform_temporary_directory() {
    // The allowlist is built from canonical platform temp roots. On Windows
    // that entry is a separate code path from the Unix one and has no other
    // test; this case is what exercises it on the Windows CI leg.
    let path = write_rgba("odytty-named-transport-platform-temp.dat");
    let mut terminal = opted_in_terminal();
    terminal.advance(&transport_apc(
        'f',
        path.to_str().expect("temp path is valid UTF-8"),
        7001,
    ));

    let answer = response(&mut terminal);
    assert!(
        answer.contains(";OK"),
        "a file inside the platform temp directory must be admitted, got {answer:?}"
    );
    assert_eq!(
        terminal.visible_graphics(0).len(),
        1,
        "the image is placed after a successful transport"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_refuses_a_path_outside_the_temporary_roots() {
    // Deterministic path outside the allowlist. Never borrow current_exe():
    // when the test binary lives under /tmp, /dev/shm, or $TMPDIR the premise
    // inverts and the case measures EFBIG instead of EPERM.
    let outside = path_outside_allowed_temp_roots();
    assert_path_outside_allowed_temp_roots(&outside);

    let mut terminal = opted_in_terminal();
    terminal.advance(&transport_apc(
        'f',
        outside.to_str().expect("outside path is valid UTF-8"),
        7002,
    ));

    let answer = response(&mut terminal);
    assert!(
        answer.contains("EPERM:path-not-allowed"),
        "a readable file outside the temp roots must be refused, got {answer:?}"
    );
    assert!(terminal.visible_graphics(0).is_empty());
}

/// A path whose parent is outside every directory `allowed_temp_dirs` admits.
///
/// Unix allowlist: `/tmp`, `/dev/shm`, and `$TMPDIR` when set. Windows
/// allowlist: `std::env::temp_dir()` only. The chosen system path is asserted
/// outside that set before the transport runs, so a future root addition fails
/// as "premise unmet" rather than silently inverting the test.
fn path_outside_allowed_temp_roots() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        std::path::PathBuf::from("/etc/hosts")
    }
    #[cfg(windows)]
    {
        std::path::PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
    }
}

fn assert_path_outside_allowed_temp_roots(path: &std::path::Path) {
    let parent = path
        .parent()
        .expect("outside path must have a parent directory");
    let canonical_parent = std::fs::canonicalize(parent).unwrap_or_else(|err| {
        panic!(
            "premise unmet: cannot canonicalize parent of {path:?}: {err}; \
             the outside-roots fixture must exist on this platform"
        )
    });

    let mut allowed = Vec::new();
    #[cfg(unix)]
    {
        for base in ["/tmp", "/dev/shm"] {
            if let Ok(canonical) = std::fs::canonicalize(base) {
                allowed.push(canonical);
            }
        }
        if let Ok(tmpdir) = std::env::var("TMPDIR")
            && let Ok(canonical) = std::fs::canonicalize(&tmpdir)
            && !allowed.contains(&canonical)
        {
            allowed.push(canonical);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(canonical) = std::fs::canonicalize(std::env::temp_dir()) {
            allowed.push(canonical);
        }
    }

    assert!(
        !allowed.is_empty(),
        "premise unmet: no allowed temp roots could be resolved on this host"
    );
    assert!(
        !allowed
            .iter()
            .any(|prefix| canonical_parent.starts_with(prefix)),
        "premise unmet: {path:?} (parent {canonical_parent:?}) is inside an \
         allowed temp root {allowed:?}; refusing to run a security-boundary \
         case that would not exercise EPERM:path-not-allowed"
    );
}

#[test]
fn temp_transport_requires_the_reference_deletion_marker_on_every_platform() {
    let unmarked = write_rgba("odytty-named-transport-platform-unmarked.dat");
    let mut terminal = opted_in_terminal();
    terminal.advance(&transport_apc(
        't',
        unmarked.to_str().expect("temp path is valid UTF-8"),
        7003,
    ));

    let answer = response(&mut terminal);
    assert!(
        answer.contains("EPERM:missing-temp-marker"),
        "an unmarked temp file must be refused, got {answer:?}"
    );
    assert!(
        unmarked.exists(),
        "a refused temp file is never deleted, on any platform"
    );
    std::fs::remove_file(&unmarked).ok();

    let marked = write_rgba("tty-graphics-protocol-odytty-named-transport-platform.dat");
    terminal.advance(&transport_apc(
        't',
        marked.to_str().expect("temp path is valid UTF-8"),
        7004,
    ));
    let answer = response(&mut terminal);
    assert!(
        answer.contains(";OK"),
        "a marked temp file is read, got {answer:?}"
    );
    assert!(
        !marked.exists(),
        "a marked temp file is deleted by the transport"
    );
}

#[test]
fn shared_memory_transport_failure_is_reported_the_same_way_everywhere() {
    // On Unix the name does not exist, so the open fails. Off Unix the medium
    // itself is unsupported. Both must reach the host as the same protocol
    // failure rather than as a placed image or as silence.
    let name = format!("/odytty-named-transport-absent-{}", std::process::id());
    let mut terminal = opted_in_terminal();
    terminal.advance(&transport_apc('s', &name, 7005));

    let answer = response(&mut terminal);
    assert!(
        answer.contains("EIO:shm-failed"),
        "an unavailable shared memory segment must be reported, got {answer:?}"
    );
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn named_transports_are_refused_before_any_host_access_until_opted_in() {
    let path = write_rgba("odytty-named-transport-platform-default-off.dat");
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(&transport_apc(
        'f',
        path.to_str().expect("temp path is valid UTF-8"),
        7006,
    ));

    let answer = response(&mut terminal);
    assert!(
        answer.contains("EPERM:named-transport-disabled"),
        "named transports are off by default on every platform, got {answer:?}"
    );
    assert!(path.exists(), "the denial happens before any host access");
    assert!(terminal.visible_graphics(0).is_empty());
    std::fs::remove_file(&path).ok();
}

#[cfg(windows)]
#[test]
fn file_transport_rejects_a_windows_final_component_reparse_point() {
    use std::os::windows::fs::symlink_file;

    let fixture = std::env::temp_dir().join(format!(
        "odytty-named-transport-reparse-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&fixture).expect("create reparse-point fixture directory");
    let target = fixture.join("target.dat");
    let link = fixture.join("link.dat");
    std::fs::write(&target, [0xFF_u8; 16]).expect("write reparse-point target");
    symlink_file(&target, &link)
        .expect("the blocking Windows runner must permit a real file symlink fixture");

    let mut terminal = opted_in_terminal();
    terminal.advance(&transport_apc(
        'f',
        link.to_str().expect("temp path is valid UTF-8"),
        7007,
    ));

    let answer = response(&mut terminal);
    assert!(
        answer.contains("EPERM:symlink-rejected"),
        "a final-component reparse point must be refused, got {answer:?}"
    );
    assert!(terminal.visible_graphics(0).is_empty());
    assert_eq!(
        std::fs::read(&target).expect("read unchanged reparse-point target"),
        [0xFF_u8; 16]
    );

    std::fs::remove_file(&link).ok();
    std::fs::remove_file(&target).ok();
    std::fs::remove_dir(&fixture).ok();
}
