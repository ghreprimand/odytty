// SPDX-License-Identifier: GPL-3.0-only
//! G2.5 fixtures: Kitty file-based transports (t=f, t=t, t=s).
//!
//! Tests exercise the full APC→transport→image pipeline through Terminal,
//! plus security-critical path validation and rejection cases.

use super::*;
// Used by the shm-segment test helpers below.
use std::ffi::CString;
use std::os::fd::AsRawFd;

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
fn file_transport_rgba_2x2() {
    let path = write_test_rgba_file("odytty_g25_file_rgba.dat");
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "PNG via t=f placed");
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_transmit_only() {
    let path = write_test_rgba_file("odytty_g25_file_tonly.dat");
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
    let apc = kitty_file_apc("/etc/passwd", 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "file outside /tmp rejected");
}

#[test]
fn file_transport_rejects_home_ssh() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let ssh_path = format!("{home}/.ssh/id_rsa");
    let mut t = Terminal::new(80, 24);
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

    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
    let apc = kitty_file_apc(path.to_str().unwrap(), 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "missing file rejected");
}

#[test]
fn file_transport_rejects_empty_path() {
    let mut t = Terminal::new(80, 24);
    // Empty path = empty base64 payload.
    let apc = b"\x1b_Ga=T,t=f,f=32,s=2,v=2;\x1b\\".to_vec();
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "empty path rejected");
}

// ---------------------------------------------------------------------------
// t=t: Temp file transport
// ---------------------------------------------------------------------------

#[test]
fn temp_transport_reads_and_deletes() {
    let path = write_test_rgba_file("odytty_g25_temp.dat");
    let mut t = Terminal::new(80, 24);
    let apc = kitty_temp_apc(path.to_str().unwrap(), 32);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "temp file placed");
    assert!(!path.exists(), "temp file deleted after read");
}

#[test]
fn temp_transport_rejects_outside_tmp() {
    let mut t = Terminal::new(80, 24);
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

    let mut t = Terminal::new(80, 24);
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

    let mut t = Terminal::new(80, 24);
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
fn shm_transport_without_leading_slash() {
    let name_with = "/odytty_g25_shm_noslash";
    let name_without = "odytty_g25_shm_noslash";
    let rgba = [0xFF_u8; 16];
    create_shm(name_with, &rgba);

    let mut t = Terminal::new(80, 24);
    let apc = kitty_shm_apc(name_without, 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "shm without / works");
    cleanup_shm(name_with);
}

#[test]
fn shm_transport_rejects_path_traversal() {
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
    let apc = kitty_shm_apc("/foo/bar", 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "shm nested slash rejected");
}

#[test]
fn shm_transport_nonexistent() {
    let mut t = Terminal::new(80, 24);
    let apc = kitty_shm_apc("/odytty_g25_nonexistent_shm", 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "nonexistent shm rejected");
}

#[test]
fn shm_transport_empty_name() {
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
    let apc = kitty_file_apc(path.to_str().unwrap(), 32, "i=55");
    t.advance(&apc);
    let resp = t.take_host_output();
    let resp_str = String::from_utf8_lossy(&resp);
    assert!(resp_str.contains(";OK"), "success response: {resp_str}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn file_transport_error_response_contains_reason() {
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
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
    let mut t = Terminal::new(80, 24);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "PNG via shm placed");
    cleanup_shm(name);
}

#[test]
fn temp_transport_deletes_even_on_decode_failure() {
    let dir = std::env::temp_dir();
    let path = dir.join("odytty_g25_temp_bad.dat");
    // Write garbage that won't decode as 2×2 RGBA.
    std::fs::write(&path, b"not an image").unwrap();

    let mut t = Terminal::new(80, 24);
    let apc = kitty_temp_apc(path.to_str().unwrap(), 32);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "bad data = no placement");
    // The temp file must still be deleted (read succeeded, decode failed).
    assert!(!path.exists(), "temp file deleted even on decode failure");
}
