// SPDX-License-Identifier: GPL-3.0-only
// Project-authored file bytes. These checks drive the production APC path on
// Linux, macOS and Windows; no default-off permission gate masks host access.
use odytty::core::Terminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..1024 {
            let path = std::env::temp_dir().join(format!(
                "odytty-nt-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create exclusive fixture directory: {error}"),
            }
        }
        panic!("unavailable-apparatus: no exclusive fixture directory");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn b64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for part in data.chunks(3) {
        let value = (u32::from(part[0]) << 16)
            | (u32::from(*part.get(1).unwrap_or(&0)) << 8)
            | u32::from(*part.get(2).unwrap_or(&0));
        encoded.push(TABLE[((value >> 18) & 63) as usize] as char);
        encoded.push(TABLE[((value >> 12) & 63) as usize] as char);
        encoded.push(if part.len() > 1 {
            TABLE[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if part.len() > 2 {
            TABLE[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}
fn read(terminal: &mut Terminal, path: &std::path::Path, medium: char) -> Vec<u8> {
    let name = b64(path.to_str().unwrap().as_bytes());
    terminal.advance(format!("\x1b_Ga=T,t={medium},f=32,s=1,v=1,i=7;{name}\x1b\\").as_bytes());
    terminal.take_host_output()
}
fn enabled() -> Terminal {
    let mut terminal = Terminal::new(8, 3);
    terminal.set_kitty_named_transports_enabled(true);
    terminal
}
#[test]
fn opted_in_file_transport_reads_owned_regular_file_without_deleting_it() {
    let fixture = Fixture::new();
    let path = fixture.0.join("image.dat");
    std::fs::write(&path, [12, 34, 56, 255]).unwrap();
    let mut terminal = enabled();
    assert_eq!(read(&mut terminal, &path, 'f'), b"\x1b_Gi=7;OK\x1b\\");
    assert!(path.exists());
    let placement = &terminal.graphics().placements()[0];
    assert_eq!(
        terminal
            .graphics()
            .store()
            .get(placement.image_id)
            .unwrap()
            .rgba,
        [12, 34, 56, 255]
    );
}
#[test]
fn opted_in_temp_transport_reads_and_deletes_owned_marked_file() {
    let fixture = Fixture::new();
    let path = fixture.0.join("tty-graphics-protocol-image.dat");
    std::fs::write(&path, [12, 34, 56, 255]).unwrap();
    let mut terminal = enabled();
    assert_eq!(read(&mut terminal, &path, 't'), b"\x1b_Gi=7;OK\x1b\\");
    assert!(!path.exists());
    assert_eq!(terminal.graphics().placements().len(), 1);
}
#[test]
fn opted_in_temp_transport_refuses_an_unmarked_owned_file() {
    let fixture = Fixture::new();
    let path = fixture.0.join("unmarked.dat");
    std::fs::write(&path, [12, 34, 56, 255]).unwrap();
    let mut terminal = enabled();
    let reply = read(&mut terminal, &path, 't');
    assert!(String::from_utf8_lossy(&reply).contains("missing-temp-marker"));
    assert_eq!(std::fs::read(&path).unwrap(), [12, 34, 56, 255]);
    assert!(terminal.graphics().store().is_empty());
}
#[cfg(windows)]
#[test]
#[ignore = "requires Windows symlink creation privilege or Developer Mode; run explicitly with that apparatus"]
fn opted_in_windows_transport_rejects_a_final_component_reparse_point() {
    let fixture = Fixture::new();
    let target = fixture.0.join("image.dat");
    let link = fixture.0.join("tty-graphics-protocol-link.dat");
    std::fs::write(&target, [12, 34, 56, 255]).unwrap();
    std::os::windows::fs::symlink_file(&target, &link)
        .expect("unavailable-apparatus: Windows symlink privilege");
    let mut terminal = enabled();
    let reply = read(&mut terminal, &link, 't');
    assert!(String::from_utf8_lossy(&reply).contains("symlink"));
    assert!(std::fs::symlink_metadata(&link).is_ok());
    assert_eq!(std::fs::read(&target).unwrap(), [12, 34, 56, 255]);
    assert!(terminal.graphics().store().is_empty());
}
