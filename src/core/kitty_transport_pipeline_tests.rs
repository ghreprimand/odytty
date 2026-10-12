// SPDX-License-Identifier: GPL-3.0-only
//! Escape-sequence level regressions for the opt-in Kitty file transports that
//! run on every platform, Windows included. The Unix-only suite next to it also
//! covers shared memory, FIFOs, descriptors and permission modes; this one pins
//! what a program in the terminal sees from `t=f` and `t=t`: the default-off
//! denial, placement, deletion and the error replies, through the same
//! admission and opened-handle checks the Windows build uses.

use super::*;
use crate::test_dirs::ScratchDir;

const RGBA_2X2: [u8; 16] = [0xFF; 16];

fn apc(kind: char, path: &std::path::Path, extra: &str) -> Vec<u8> {
    let name =
        super::kitty::encode_base64_bytes(path.to_str().expect("UTF-8 fixture path").as_bytes());
    format!("\x1b_Ga=T,t={kind},f=32,s=2,v=2,{extra};{name}\x1b\\").into_bytes()
}

fn enabled() -> Terminal {
    let mut terminal = Terminal::new(80, 24);
    terminal.set_kitty_named_transports_enabled(true);
    terminal
}

fn reply(terminal: &mut Terminal) -> String {
    String::from_utf8(terminal.take_host_output()).expect("ASCII reply")
}

#[test]
fn the_default_denies_file_and_temp_requests_and_touches_nothing() {
    let dir = ScratchDir::new("ktp-off-");
    let plain = dir.path().join("image.dat");
    let marked = dir.path().join("tty-graphics-protocol-off.dat");
    std::fs::write(&plain, RGBA_2X2).expect("seed");
    std::fs::write(&marked, RGBA_2X2).expect("seed");
    let mut terminal = Terminal::new(80, 24);
    for sequence in [apc('f', &plain, "i=81"), apc('t', &marked, "i=82")] {
        terminal.advance(&sequence);
        assert!(reply(&mut terminal).contains("EPERM:named-transport-disabled"));
    }
    assert!(plain.exists() && marked.exists(), "denial never deletes");
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn a_file_transport_places_the_image_and_keeps_the_file() {
    let dir = ScratchDir::new("ktp-file-");
    let path = dir.path().join("image.dat");
    std::fs::write(&path, RGBA_2X2).expect("seed");
    let mut terminal = enabled();
    terminal.advance(&apc('f', &path, "i=83"));
    assert!(reply(&mut terminal).contains(";OK"));
    assert_eq!(terminal.visible_graphics(0).len(), 1);
    assert!(path.exists(), "t=f never deletes");
}

#[test]
fn a_temp_transport_deletes_a_marked_file_and_leaves_an_unmarked_one() {
    let dir = ScratchDir::new("ktp-temp-");
    let marked = dir.path().join("tty-graphics-protocol-temp.dat");
    let unmarked = dir.path().join("image.dat");
    std::fs::write(&marked, RGBA_2X2).expect("seed");
    std::fs::write(&unmarked, RGBA_2X2).expect("seed");
    let mut terminal = enabled();
    terminal.advance(&apc('t', &marked, "i=84"));
    assert!(reply(&mut terminal).contains(";OK"));
    assert_eq!(terminal.visible_graphics(0).len(), 1);
    assert!(
        !marked.exists(),
        "the marked temp file is deleted after the read"
    );

    terminal.advance(&apc('t', &unmarked, "i=85"));
    assert!(reply(&mut terminal).contains("EPERM:missing-temp-marker"));
    assert!(unmarked.exists(), "an unmarked path is never deleted");
    assert_eq!(terminal.visible_graphics(0).len(), 1, "no second placement");
}

#[test]
fn a_missing_file_gets_an_error_reply_and_no_placement() {
    let dir = ScratchDir::new("ktp-missing-");
    let mut terminal = enabled();
    terminal.advance(&apc('f', &dir.path().join("absent.dat"), "i=86"));
    let reply = reply(&mut terminal);
    // A file that is not there passed admission (the directory is allowed) and
    // failed to read: `EIO:read-failed`, not a permission, path or marker
    // refusal, which would mean the request was rejected before it was tried.
    // The reply names the request it answers (`i=86`).
    assert_eq!(reply, "\x1b_Gi=86;EIO:read-failed\x1b\\", "{reply:?}");
    assert!(terminal.visible_graphics(0).is_empty());
}

#[cfg(not(unix))]
#[test]
fn shared_memory_is_reported_unsupported_off_unix() {
    let mut terminal = enabled();
    let name = super::kitty::encode_base64_bytes(b"/odytty-ktp-shm");
    terminal.advance(format!("\x1b_Ga=T,t=s,f=32,s=2,v=2,i=87;{name}\x1b\\").as_bytes());
    assert!(reply(&mut terminal).contains("EIO:shm-failed"));
    assert!(terminal.visible_graphics(0).is_empty());
}
