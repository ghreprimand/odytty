// SPDX-License-Identifier: GPL-3.0-only
//! G2.5 fixtures: Kitty file-based transports (t=f, t=t, t=s).
//!
//! Tests exercise the full APC→transport→image pipeline through Terminal,
//! plus security-critical path validation and rejection cases.

use super::*;
// Used by the shm-segment test helpers below.
use std::ffi::CString;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Minimal base64 encoder for test payloads.
fn simple_base64(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Write a 2×2 RGBA image (16 bytes) to a temp file, return path.
fn write_test_rgba_file(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    let path = dir.join(name);
    let rgba = [0xFF_u8; 16]; // 2×2 white opaque
    std::fs::write(&path, rgba).unwrap();
    path
}

fn named_transport_terminal() -> Terminal {
    let mut terminal = Terminal::new(80, 24);
    terminal.set_kitty_named_transports_enabled(true);
    terminal
}

/// Create a minimal valid PNG in memory for a 2×2 RGBA image.
fn make_2x2_png() -> Vec<u8> {
    let mut buf = Vec::new();
    let mut encoder = png::Encoder::new(&mut buf, 2, 2);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    let data = [0xFF_u8; 16]; // 2×2 white opaque
    writer.write_image_data(&data).unwrap();
    writer.finish().unwrap();
    buf
}

/// Build a Kitty APC for file transport.
fn kitty_file_apc(path: &str, format: u32, extra: &str) -> Vec<u8> {
    let path_b64 = simple_base64(path.as_bytes());
    if extra.is_empty() {
        format!("\x1b_Ga=T,t=f,f={format},s=2,v=2;{path_b64}\x1b\\").into_bytes()
    } else {
        format!("\x1b_Ga=T,t=f,f={format},s=2,v=2,{extra};{path_b64}\x1b\\").into_bytes()
    }
}

/// Build a Kitty APC for temp file transport.
fn kitty_temp_apc(path: &str, format: u32) -> Vec<u8> {
    let path_b64 = simple_base64(path.as_bytes());
    format!("\x1b_Ga=T,t=t,f={format},s=2,v=2;{path_b64}\x1b\\").into_bytes()
}

/// Build a Kitty APC for shared memory transport.
fn kitty_shm_apc(name: &str, format: u32, width: u32, height: u32) -> Vec<u8> {
    let name_b64 = simple_base64(name.as_bytes());
    format!("\x1b_Ga=T,t=s,f={format},s={width},v={height};{name_b64}\x1b\\").into_bytes()
}

/// Build a Kitty APC for transmit-only (a=t) via file.
fn kitty_file_transmit_only(path: &str, format: u32, id: u32) -> Vec<u8> {
    let path_b64 = simple_base64(path.as_bytes());
    format!("\x1b_Ga=t,t=f,f={format},s=2,v=2,i={id};{path_b64}\x1b\\").into_bytes()
}

/// Create a POSIX shm segment with the given data.
///
/// Populated via `mmap` (after `ftruncate` sizes the object), which is the only
/// portable way: `read()`/`write()` on a POSIX shm fd return ENXIO ("Device not
/// configured") on macOS, where shm objects are mmap-only. The production `t=s`
/// reader (`read_shm_transport`) mirrors this with an `mmap` read, so both ends
/// are cross-platform and these tests run on Linux and macOS alike.
fn create_shm(name: &str, data: &[u8]) {
    let c_name = CString::new(name).unwrap();
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600) };
    assert!(fd >= 0, "shm_open failed for test setup: {name}");
    let rc = unsafe { libc::ftruncate(fd, data.len() as libc::off_t) };
    assert!(rc == 0, "ftruncate failed for test setup: {name}");
    let addr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            data.len(),
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    assert!(
        addr != libc::MAP_FAILED,
        "mmap failed for test setup: {name}"
    );
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len());
        libc::munmap(addr, data.len());
        libc::close(fd);
    }
}

/// Cleanup a shm segment (best-effort).
fn cleanup_shm(name: &str) {
    let c_name = CString::new(name).unwrap();
    unsafe {
        libc::shm_unlink(c_name.as_ptr());
    }
}

// ---------------------------------------------------------------------------
// t=f: File transport — success cases
// ---------------------------------------------------------------------------

#[test]
fn named_transports_default_off_rejects_before_host_access() {
    let file = write_test_rgba_file("odytty-g25-default-off-file.dat");
    let marked = write_test_rgba_file("tty-graphics-protocol-odytty-g25-default-off.dat");
    let unmarked = write_test_rgba_file("odytty-g25-default-off-temp.dat");
    let shm_name = format!("/odytty_g25_default_off_{}", std::process::id());
    create_shm(&shm_name, &[0xFF_u8; 16]);

    let mut terminal = Terminal::new(80, 24);
    for apc in [
        kitty_file_apc(file.to_str().unwrap(), 32, "i=81"),
        kitty_temp_apc(marked.to_str().unwrap(), 32),
        kitty_temp_apc(unmarked.to_str().unwrap(), 32),
        kitty_shm_apc(&shm_name, 32, 2, 2),
    ] {
        terminal.advance(&apc);
        let response = String::from_utf8(terminal.take_host_output()).unwrap();
        assert!(
            response.contains("EPERM:named-transport-disabled"),
            "normal denial response: {response}"
        );
    }

    assert!(file.exists());
    assert!(marked.exists());
    assert!(unmarked.exists());
    let c_name = CString::new(shm_name.as_str()).unwrap();
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
    assert!(fd >= 0, "a denied t=s request must not unlink the name");
    unsafe { libc::close(fd) };

    std::fs::remove_file(file).ok();
    std::fs::remove_file(marked).ok();
    std::fs::remove_file(unmarked).ok();
    cleanup_shm(&shm_name);
}

#[test]
fn named_transport_default_denial_honors_quiet_response() {
    let file = write_test_rgba_file("odytty-g25-default-off-quiet.dat");
    let mut terminal = Terminal::new(80, 24);
    let apc = kitty_file_apc(file.to_str().unwrap(), 32, "q=2");
    terminal.advance(&apc);
    assert!(terminal.take_host_output().is_empty());
    assert!(file.exists());
    std::fs::remove_file(file).ok();
}

#[test]
fn file_transport_rgba_2x2() {
    let path = write_test_rgba_file("odytty_g25_file_rgba.dat");
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(path.to_str().unwrap(), 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "image placed via t=f");
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_png() {
    let png_data = make_2x2_png();
    let dir = std::env::temp_dir();
    let path = dir.join("odytty_g25_file_png.png");
    std::fs::write(&path, &png_data).unwrap();

    let path_b64 = simple_base64(path.to_str().unwrap().as_bytes());
    let apc = format!("\x1b_Ga=T,t=f,f=100;{path_b64}\x1b\\").into_bytes();
    let mut t = named_transport_terminal();
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "PNG via t=f placed");
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_transmit_only() {
    let path = write_test_rgba_file("odytty_g25_file_tonly.dat");
    let mut t = named_transport_terminal();
    let apc = kitty_file_transmit_only(path.to_str().unwrap(), 32, 42);
    t.advance(&apc);
    // a=t stores image but does NOT place.
    assert_eq!(
        t.visible_graphics(0).len(),
        0,
        "transmit-only: no placement"
    );
    assert!(!t.graphics().store().is_empty(), "image stored");
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_with_image_id() {
    let path = write_test_rgba_file("odytty_g25_file_id.dat");
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(path.to_str().unwrap(), 32, "i=77");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1);
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// t=f: File transport — security rejections
// ---------------------------------------------------------------------------

#[test]
fn file_transport_rejects_outside_tmp() {
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc("/etc/passwd", 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "file outside /tmp rejected");
}

#[test]
fn file_transport_rejects_home_ssh() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let ssh_path = format!("{home}/.ssh/id_rsa");
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(&ssh_path, 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "~/.ssh rejected");
}

#[test]
fn file_transport_rejects_symlink() {
    let dir = std::env::temp_dir();
    let real = dir.join("odytty_g25_real_for_link.dat");
    let link = dir.join("odytty_g25_symlink.dat");
    std::fs::write(&real, [0xFF_u8; 16]).unwrap();
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(link.to_str().unwrap(), 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "symlink rejected");

    std::fs::remove_file(&real).ok();
    std::fs::remove_file(&link).ok();
}

#[test]
fn file_transport_rejects_nonexistent() {
    let path = std::env::temp_dir().join("odytty_g25_nonexistent_9f3c.dat");
    let _ = std::fs::remove_file(&path);
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(path.to_str().unwrap(), 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "missing file rejected");
}

#[test]
fn file_transport_rejects_empty_path() {
    let mut t = named_transport_terminal();
    // Empty path = empty base64 payload.
    let apc = b"\x1b_Ga=T,t=f,f=32,s=2,v=2;\x1b\\".to_vec();
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "empty path rejected");
}

#[cfg(unix)]
#[test]
fn fifo_transport_child_rejects_without_deleting() {
    let Ok(path) = std::env::var("ODYTTY_KITTY_FIFO_TEST_PATH") else {
        return;
    };
    let path = std::path::PathBuf::from(path);

    let file = super::kitty_transport::read_file_transport(path.as_os_str().as_bytes(), 16);
    assert_eq!(
        file,
        Err(super::kitty_transport::TransportError::NonRegularFile)
    );

    let temp = super::kitty_transport::read_temp_transport(path.as_os_str().as_bytes(), 16);
    assert_eq!(
        temp,
        Err(super::kitty_transport::TransportError::NonRegularFile)
    );
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_fifo(),
        "a rejected t=t FIFO must not be deleted"
    );
}

#[cfg(unix)]
#[test]
fn file_and_temp_transports_reject_fifo_without_blocking() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let path = std::env::temp_dir().join(format!(
        "tty-graphics-protocol-odytty-kitty-fifo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("core::kitty_transport_tests::fifo_transport_child_rejects_without_deleting")
        .arg("--nocapture")
        .env("ODYTTY_KITTY_FIFO_TEST_PATH", &path)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            let _ = std::fs::remove_file(&path);
            panic!("FIFO transport subprocess exceeded the bounded rejection window");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = std::fs::remove_file(&path);
    assert!(
        status.success(),
        "FIFO transport subprocess failed: {status}"
    );
}

// ---------------------------------------------------------------------------
// t=t: Temp file transport
// ---------------------------------------------------------------------------

#[test]
fn temp_transport_reads_and_deletes() {
    let path = write_test_rgba_file("tty-graphics-protocol-odytty-g25-temp.dat");
    let mut t = named_transport_terminal();
    let apc = kitty_temp_apc(path.to_str().unwrap(), 32);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "temp file placed");
    assert!(!path.exists(), "temp file deleted after read");
}

#[test]
fn temp_transport_opt_in_requires_reference_deletion_marker() {
    let path = write_test_rgba_file("odytty-g25-unmarked-temp.dat");
    let mut terminal = named_transport_terminal();
    terminal.advance(&kitty_temp_apc(path.to_str().unwrap(), 32));
    let response = String::from_utf8(terminal.take_host_output()).unwrap();
    assert!(response.contains("EPERM:missing-temp-marker"));
    assert!(path.exists(), "an unmarked t=t path must remain untouched");
    assert!(terminal.visible_graphics(0).is_empty());
    std::fs::remove_file(path).ok();
}

#[test]
fn temp_transport_rejects_outside_tmp() {
    let mut t = named_transport_terminal();
    let apc = kitty_temp_apc("/etc/hostname", 32);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "t=t outside /tmp rejected");
}

#[test]
fn temp_transport_rejects_symlink() {
    let dir = std::env::temp_dir();
    let real = dir.join("odytty_g25_temp_real.dat");
    let link = dir.join("odytty_g25_temp_link.dat");
    std::fs::write(&real, [0xFF_u8; 16]).unwrap();
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut t = named_transport_terminal();
    let apc = kitty_temp_apc(link.to_str().unwrap(), 32);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "t=t symlink rejected");

    std::fs::remove_file(&real).ok();
    std::fs::remove_file(&link).ok();
}

// ---------------------------------------------------------------------------
// t=s: Shared memory transport
// ---------------------------------------------------------------------------

#[test]
fn shm_transport_rgba_2x2() {
    let name = "/odytty_g25_shm_rgba";
    let rgba = [0xFF_u8; 16]; // 2×2 white
    create_shm(name, &rgba);

    let mut t = named_transport_terminal();
    let apc = kitty_shm_apc(name, 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "shm image placed");

    // Segment should already be unlinked by the transport.
    let c_name = CString::new(name).unwrap();
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
    if fd >= 0 {
        unsafe {
            libc::close(fd);
        }
        cleanup_shm(name);
        panic!("shm segment should have been unlinked");
    }
}

#[test]
fn shm_reader_rejects_segment_shrunk_after_initial_size_check() {
    let path = std::env::temp_dir().join("odytty_shm_shrink_regression.dat");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(16).unwrap();
    let expected = super::kitty_transport::checked_shm_size(file.as_raw_fd(), 32).unwrap();
    file.set_len(8).unwrap();

    let result = super::kitty_transport::read_shm_fd_at_size(file.as_raw_fd(), expected, 32);
    assert!(matches!(
        result,
        Err(super::kitty_transport::TransportError::ShmError(_))
    ));
    drop(file);
    std::fs::remove_file(path).ok();
}

#[test]
fn shm_transport_validation_failure_preserves_name() {
    let name = format!("/odytty_g25_invalid_shm_{}", std::process::id());
    let c_name = CString::new(name.as_str()).unwrap();
    let created = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600) };
    assert!(created >= 0);
    unsafe { libc::close(created) };

    let mut terminal = named_transport_terminal();
    terminal.advance(&kitty_shm_apc(&name, 32, 2, 2));
    assert!(terminal.visible_graphics(0).is_empty());

    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
    assert!(fd >= 0, "a rejected t=s object must retain its name");
    unsafe { libc::close(fd) };
    cleanup_shm(&name);
}

#[test]
fn shm_transport_without_leading_slash() {
    let name_with = "/odytty_g25_shm_noslash";
    let name_without = "odytty_g25_shm_noslash";
    let rgba = [0xFF_u8; 16];
    create_shm(name_with, &rgba);

    let mut t = named_transport_terminal();
    let apc = kitty_shm_apc(name_without, 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "shm without / works");
    cleanup_shm(name_with);
}

#[test]
fn shm_transport_rejects_path_traversal() {
    let mut t = named_transport_terminal();
    let apc = kitty_shm_apc("../etc/passwd", 32, 2, 2);
    t.advance(&apc);
    assert_eq!(
        t.visible_graphics(0).len(),
        0,
        "shm path traversal rejected"
    );
}

#[test]
fn shm_transport_rejects_nested_slash() {
    let mut t = named_transport_terminal();
    let apc = kitty_shm_apc("/foo/bar", 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "shm nested slash rejected");
}

#[test]
fn shm_transport_nonexistent() {
    let mut t = named_transport_terminal();
    let apc = kitty_shm_apc("/odytty_g25_nonexistent_shm", 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "nonexistent shm rejected");
}

#[test]
fn shm_transport_empty_name() {
    let mut t = named_transport_terminal();
    // Empty shm name = empty base64 payload.
    let apc = b"\x1b_Ga=T,t=s,f=32,s=2,v=2;\x1b\\".to_vec();
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "empty shm name rejected");
}

// ---------------------------------------------------------------------------
// Response verification
// ---------------------------------------------------------------------------

#[test]
fn file_transport_ok_response() {
    let path = write_test_rgba_file("odytty_g25_resp.dat");
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(path.to_str().unwrap(), 32, "i=55");
    t.advance(&apc);
    let resp = t.take_host_output();
    let resp_str = String::from_utf8_lossy(&resp);
    assert!(resp_str.contains(";OK"), "success response: {resp_str}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_error_response_contains_reason() {
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc("/etc/passwd", 32, "i=56");
    t.advance(&apc);
    let resp = t.take_host_output();
    let resp_str = String::from_utf8_lossy(&resp);
    assert!(
        resp_str.contains("EPERM") || resp_str.contains("EIO") || resp_str.contains("EBADF"),
        "error response should contain error code: {resp_str}"
    );
}

#[test]
fn file_transport_quiet_suppresses_response() {
    let path = write_test_rgba_file("odytty_g25_quiet.dat");
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(path.to_str().unwrap(), 32, "q=2");
    t.advance(&apc);
    let resp = t.take_host_output();
    assert!(resp.is_empty(), "q=2 suppresses response");
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn file_transport_rgb_format() {
    let dir = std::env::temp_dir();
    let path = dir.join("odytty_g25_rgb.dat");
    let rgb = [0xFF_u8; 12]; // 2×2 RGB (3 bytes per pixel)
    std::fs::write(&path, rgb).unwrap();

    let path_b64 = simple_base64(path.to_str().unwrap().as_bytes());
    let apc = format!("\x1b_Ga=T,t=f,f=24,s=2,v=2;{path_b64}\x1b\\").into_bytes();
    let mut t = named_transport_terminal();
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "RGB via t=f placed");
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_dimension_mismatch() {
    // File has 16 bytes (2×2 RGBA) but we claim 3×3.
    let path = write_test_rgba_file("odytty_g25_dim_mismatch.dat");
    let path_b64 = simple_base64(path.to_str().unwrap().as_bytes());
    let apc = format!("\x1b_Ga=T,t=f,f=32,s=3,v=3;{path_b64}\x1b\\").into_bytes();
    let mut t = named_transport_terminal();
    t.advance(&apc);
    assert_eq!(
        t.visible_graphics(0).len(),
        0,
        "dimension mismatch rejected"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn shm_transport_png() {
    let png_data = make_2x2_png();
    let name = "/odytty_g25_shm_png";
    create_shm(name, &png_data);

    let name_b64 = simple_base64(name.as_bytes());
    let apc = format!("\x1b_Ga=T,t=s,f=100;{name_b64}\x1b\\").into_bytes();
    let mut t = named_transport_terminal();
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "PNG via shm placed");
    cleanup_shm(name);
}

#[test]
fn temp_transport_deletes_even_on_decode_failure() {
    let dir = std::env::temp_dir();
    let path = dir.join("tty-graphics-protocol-odytty-g25-temp-bad.dat");
    // Write garbage that won't decode as 2×2 RGBA.
    std::fs::write(&path, b"not an image").unwrap();

    let mut t = named_transport_terminal();
    let apc = kitty_temp_apc(path.to_str().unwrap(), 32);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "bad data = no placement");
    // The temp file must still be deleted (read succeeded, decode failed).
    assert!(!path.exists(), "temp file deleted even on decode failure");
}

// ---------------------------------------------------------------------------
// Reader boundaries, path admission, and failure classification
//
// The tests above drive the transports through the APC pipeline, where every
// rejection collapses into "no image was placed". These call the readers
// directly so each boundary and each error classification is asserted on its
// own, one byte either side of the cap where a boundary exists.
// ---------------------------------------------------------------------------

use super::kitty_transport as transport;
use transport::TransportError;

/// A unique path inside the platform temp directory for this test process.
fn temp_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("odytty-transport-{tag}-{}.dat", std::process::id()))
}

fn path_bytes(path: &std::path::Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

#[test]
fn file_reader_admits_exactly_the_cap_and_refuses_one_byte_more() {
    let path = temp_path("cap");
    let cap = 4096_usize;
    let exact: Vec<u8> = (0..cap).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &exact).unwrap();
    let raw = path_bytes(&path);

    // Exactly at the cap: admitted, and the whole file comes back. A reader
    // that stopped one byte short would still return Ok here.
    let read = transport::read_file_transport(&raw, cap).expect("a file of exactly cap bytes");
    assert_eq!(read.len(), cap, "the complete file is returned");
    assert_eq!(read, exact, "returned bytes match the file byte for byte");

    // The same file against a cap one byte smaller is refused.
    assert_eq!(
        transport::read_file_transport(&raw, cap - 1),
        Err(TransportError::TooLarge),
        "a file one byte over the cap is refused"
    );

    // Cap plus one byte on disk, refused before any decode.
    let mut over = exact;
    over.push(0);
    std::fs::write(&path, &over).unwrap();
    assert_eq!(
        transport::read_file_transport(&raw, cap),
        Err(TransportError::TooLarge)
    );

    // Zero-length files remain readable; the cap is an upper bound only.
    std::fs::write(&path, b"").unwrap();
    assert_eq!(transport::read_file_transport(&raw, cap), Ok(Vec::new()));

    std::fs::remove_file(&path).ok();
}

#[test]
fn file_reader_cap_constant_admits_a_multi_megabyte_file() {
    // With no caller-imposed limit the module constant is the only bound. Two
    // MiB is far below it and must be admitted; the constant is stated as a
    // product of three factors, and every arithmetic corruption of that
    // product lands below this size.
    let path = temp_path("constant");
    let size = 2 * 1024 * 1024;
    std::fs::write(&path, vec![0x5A_u8; size]).unwrap();
    let raw = path_bytes(&path);

    let read = transport::read_file_transport(&raw, usize::MAX)
        .expect("2 MiB is well inside the transport read cap");
    assert_eq!(read.len(), size);
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_reader_rejects_empty_non_utf8_and_interior_nul_paths() {
    assert_eq!(
        transport::read_file_transport(b"", 4096),
        Err(TransportError::InvalidPath),
        "an empty path never reaches the filesystem"
    );
    assert_eq!(
        transport::read_file_transport(&[0x2F, 0xFF, 0xFE], 4096),
        Err(TransportError::InvalidPath),
        "a non-UTF-8 path is refused rather than reinterpreted"
    );

    let mut interior_nul = path_bytes(&std::env::temp_dir());
    interior_nul.extend_from_slice(b"/odytty\0truncated.dat");
    assert_eq!(
        transport::read_file_transport(&interior_nul, 4096),
        Err(TransportError::InvalidPath),
        "an interior NUL is refused at the path boundary, not passed to open()"
    );

    // A NUL as the final byte is the same rejection, not a trailing-byte trim.
    let mut trailing_nul = path_bytes(&temp_path("nul"));
    trailing_nul.push(0);
    assert_eq!(
        transport::read_file_transport(&trailing_nul, 4096),
        Err(TransportError::InvalidPath)
    );
}

#[test]
fn file_reader_admission_is_limited_to_the_allowlisted_roots() {
    assert_eq!(
        transport::read_file_transport(b"/etc/passwd", 4096),
        Err(TransportError::PathNotAllowed),
        "a readable system file outside the temp roots is refused"
    );
    assert_eq!(
        transport::read_file_transport(b"/etc/./passwd", 4096),
        Err(TransportError::PathNotAllowed),
        "a dot component does not change the admitted directory"
    );

    // A traversal that starts inside the temp root still resolves outside it.
    let mut escape = path_bytes(&std::env::temp_dir());
    escape.extend_from_slice(b"/../etc/passwd");
    assert_eq!(
        transport::read_file_transport(&escape, 4096),
        Err(TransportError::PathNotAllowed),
        "the parent directory is canonicalized before containment is checked"
    );

    // A relative path has no admitted parent.
    assert!(matches!(
        transport::read_file_transport(b"relative.dat", 4096),
        Err(TransportError::PathNotAllowed | TransportError::IoError(_))
    ));
}

#[test]
fn file_reader_distinguishes_symlink_rejection_from_other_open_failures() {
    let missing = temp_path("absent");
    std::fs::remove_file(&missing).ok();
    assert!(
        matches!(
            transport::read_file_transport(&path_bytes(&missing), 4096),
            Err(TransportError::IoError(_))
        ),
        "a missing file is an I/O error, never a symlink rejection"
    );

    let target = temp_path("symlink-target");
    std::fs::write(&target, [0xFF_u8; 16]).unwrap();
    let link = temp_path("symlink");
    std::fs::remove_file(&link).ok();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(
        transport::read_file_transport(&path_bytes(&link), 4096),
        Err(TransportError::SymlinkRejected),
        "the final path component is opened with O_NOFOLLOW"
    );
    // The target itself is still readable, so the rejection is about the link.
    assert_eq!(
        transport::read_file_transport(&path_bytes(&target), 4096).map(|b| b.len()),
        Ok(16)
    );
    std::fs::remove_file(&link).ok();
    std::fs::remove_file(&target).ok();
}

#[test]
fn temp_reader_requires_the_deletion_marker_before_reading_or_deleting() {
    let unmarked = temp_path("unmarked");
    std::fs::write(&unmarked, [0xFF_u8; 16]).unwrap();
    assert_eq!(
        transport::read_temp_transport(&path_bytes(&unmarked), 4096),
        Err(TransportError::MissingTempMarker)
    );
    assert!(
        unmarked.exists(),
        "a file rejected for a missing marker is never deleted"
    );
    std::fs::remove_file(&unmarked).ok();

    let marked = std::env::temp_dir().join(format!(
        "tty-graphics-protocol-odytty-{}.dat",
        std::process::id()
    ));
    std::fs::write(&marked, [0xFF_u8; 16]).unwrap();
    assert_eq!(
        transport::read_temp_transport(&path_bytes(&marked), 4096).map(|b| b.len()),
        Ok(16)
    );
    assert!(!marked.exists(), "a marked file is deleted after the read");

    // The cap applies to the temp reader too, and a refused read leaves the
    // file in place.
    let too_big = std::env::temp_dir().join(format!(
        "tty-graphics-protocol-odytty-big-{}.dat",
        std::process::id()
    ));
    std::fs::write(&too_big, vec![0_u8; 64]).unwrap();
    assert_eq!(
        transport::read_temp_transport(&path_bytes(&too_big), 32),
        Err(TransportError::TooLarge)
    );
    assert!(too_big.exists(), "an oversized temp file is not deleted");
    std::fs::remove_file(&too_big).ok();
}

// ---------------------------------------------------------------------------
// Shared-memory name admission and failure classification
// ---------------------------------------------------------------------------

#[test]
fn shm_name_admission_rejects_only_malformed_names() {
    for name in [
        &b""[..],
        b"/",
        b"/foo/bar",
        b"foo/bar",
        b"../etc/passwd",
        &[0x2F, 0xFF, 0xFE][..],
    ] {
        assert_eq!(
            transport::read_shm_transport(name, 4096),
            Err(TransportError::InvalidPath),
            "malformed shm name {name:?} must be refused before shm_open"
        );
    }

    // A one-character name is a legal POSIX shm name. It must reach shm_open
    // and fail there (or succeed), never be refused as malformed.
    assert!(
        !matches!(
            transport::read_shm_transport(b"/Z", 4096),
            Err(TransportError::InvalidPath)
        ),
        "a single-character shm name is legal and must not be refused as malformed"
    );
}

#[test]
fn shm_reader_reports_the_open_failure_rather_than_a_later_stage() {
    let name = format!("/odytty-transport-absent-{}", std::process::id());
    cleanup_shm(&name);
    match transport::read_shm_transport(name.as_bytes(), 4096) {
        Err(TransportError::ShmError(message)) => assert!(
            message.contains("shm_open"),
            "a failed open must be classified as such, got {message}"
        ),
        other => panic!("a nonexistent segment must fail at open, got {other:?}"),
    }
}

#[test]
fn shm_size_check_admits_the_exact_cap_and_refuses_one_byte_more() {
    let path = temp_path("shm-size");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(64).unwrap();
    let fd = file.as_raw_fd();

    assert_eq!(
        transport::checked_shm_size(fd, 64),
        Ok(64),
        "a segment exactly at the cap is admitted"
    );
    assert_eq!(
        transport::checked_shm_size(fd, 65),
        Ok(64),
        "a segment below the cap is admitted"
    );
    assert_eq!(
        transport::checked_shm_size(fd, 63),
        Err(TransportError::TooLarge),
        "a segment one byte over the cap is refused before any mapping"
    );

    file.set_len(0).unwrap();
    assert!(
        matches!(
            transport::checked_shm_size(fd, 64),
            Err(TransportError::ShmError(_))
        ),
        "an empty segment is refused"
    );

    drop(file);
    std::fs::remove_file(&path).ok();
}

#[test]
fn shm_size_check_reports_a_failed_fstat() {
    // -1 is never a valid descriptor. The failure must be reported as an fstat
    // failure rather than being read out of an uninitialized stat buffer.
    match transport::checked_shm_size(-1, 4096) {
        Err(TransportError::ShmError(message)) => assert!(
            message.contains("fstat"),
            "a failed fstat must be classified as such, got {message}"
        ),
        other => panic!("an invalid descriptor must fail, got {other:?}"),
    }
}

#[test]
fn shm_reader_reports_a_failed_copy_from_an_unreadable_descriptor() {
    // A write-only descriptor passes both size checks and fails in the copy.
    // The failure must be reported as a read failure, not as a segment shrink.
    let path = temp_path("shm-writeonly");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.set_len(32).unwrap();
    let fd = file.as_raw_fd();
    assert_eq!(transport::checked_shm_size(fd, 64), Ok(32));

    match transport::read_shm_fd_at_size(fd, 32, 64) {
        Err(TransportError::ShmError(message)) => {
            #[cfg(not(target_os = "macos"))]
            assert!(
                message.contains("pread"),
                "a failed positional read must be classified as such, got {message}"
            );
            #[cfg(target_os = "macos")]
            assert!(
                message.contains("mmap") || message.contains("isolated copy"),
                "macOS maps the segment, so an unreadable descriptor fails there, got {message}"
            );
        }
        other => panic!("an unreadable descriptor must fail the copy, got {other:?}"),
    }

    drop(file);
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// $TMPDIR admission and metadata that understates a file's readable size
//
// Both behaviors need a process environment this test process cannot safely
// mutate in place, so the assertions run in a re-executed child of this same
// test binary. The child prints a completion marker: a filter that matched no
// test would otherwise exit successfully and read as a pass.
//
// Linux only. `/proc` is used because it is the one directory that reliably
// serves regular files whose stat size is zero while a read returns content,
// which is the only deterministic stand-in for a file that grows between the
// size check and the read. macOS and Windows have no equivalent, and on macOS
// the $TMPDIR branch is already exercised by every test in this file, because
// there the platform temp directory is $TMPDIR rather than /tmp.
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
#[test]
fn tmpdir_root_is_admitted_and_understated_metadata_size_still_caps_the_read() {
    const CHILD_MARKER: &str = "ODYTTY_TRANSPORT_TMPDIR_CHILD";
    const COMPLETED: &str = "odytty-transport-tmpdir-child-completed";
    const TEST_PATH: &str = concat!(
        "core::kitty_transport_tests::",
        "tmpdir_root_is_admitted_and_understated_metadata_size_still_caps_the_read"
    );

    if std::env::var_os(CHILD_MARKER).is_some() {
        // $TMPDIR is /proc here, so /proc/version is inside an admitted root
        // only if the $TMPDIR entry was added to the allowlist.
        let content = transport::read_file_transport(b"/proc/version", 4096)
            .expect("a file directly inside $TMPDIR must be admitted");
        assert!(
            !content.is_empty(),
            "content is returned even though the metadata size is zero"
        );

        // The same file with a cap below its real length: the reader must not
        // trust the understated metadata size and hand back a silently
        // truncated payload.
        assert_eq!(
            transport::read_file_transport(b"/proc/version", 8),
            Err(TransportError::TooLarge),
            "content past the cap is refused even when metadata reports zero"
        );

        // A sibling of the admitted root is still outside it.
        assert_eq!(
            transport::read_file_transport(b"/etc/passwd", 4096),
            Err(TransportError::PathNotAllowed),
            "adding $TMPDIR does not widen admission beyond that directory"
        );

        println!("{COMPLETED}");
        return;
    }

    let exe = std::env::current_exe().expect("path to this test binary");
    let output = std::process::Command::new(exe)
        .args(["--exact", "--nocapture", TEST_PATH])
        .env(CHILD_MARKER, "1")
        .env("TMPDIR", "/proc")
        .output()
        .expect("re-run this test binary as a child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child run failed ({}):\n{stdout}{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(COMPLETED),
        "the child never reached its assertions, so this test proved nothing; stdout was:\n{stdout}"
    );
}
