// SPDX-License-Identifier: GPL-3.0-only
//! G2.5 fixtures: Kitty file-based transports (t=f, t=t, t=s).
//!
//! Tests exercise the full APC→transport→image pipeline through Terminal,
//! plus security-critical path validation and rejection cases.

use super::*;
// Used by the shm-segment test helpers below.
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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

/// Own an exclusively created directory and all fixture paths inside it.
struct FixtureDir(std::path::PathBuf);

impl FixtureDir {
    fn new() -> Self {
        let fixture = Self(crate::test_dirs::fresh_temp_dir("kt"));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        fixture
    }

    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct OwnedPath {
    path: std::path::PathBuf,
    _directory: FixtureDir,
}

impl OwnedPath {
    fn new(name: &str) -> Self {
        let directory = FixtureDir::new();
        Self {
            path: directory.join(name),
            _directory: directory,
        }
    }
}

impl std::ops::Deref for OwnedPath {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl AsRef<std::path::Path> for OwnedPath {
    fn as_ref(&self) -> &std::path::Path {
        &self.path
    }
}

/// Write a 2×2 RGBA image inside an owned directory.
fn write_test_rgba_file(name: &str) -> OwnedPath {
    let path = OwnedPath::new(name);
    std::fs::write(&path, [0xFF_u8; 16]).unwrap();
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

/// A POSIX shm object owned only after successful exclusive creation.
struct OwnedShmFixture {
    name: CString,
    fd: OwnedFd,
}

impl Drop for OwnedShmFixture {
    fn drop(&mut self) {
        // SAFETY: this name belongs to the successful exclusive creation.
        unsafe {
            libc::shm_unlink(self.name.as_ptr());
        }
    }
}

impl OwnedShmFixture {
    fn try_create_name(name: CString) -> std::io::Result<Self> {
        // macOS shm_open rejects flags beyond its documented access and creation flags.
        // SAFETY: a valid name; O_EXCL refuses every existing object.
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful shm_open transfers this descriptor exactly once.
        Ok(Self {
            name,
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    fn create(data: &[u8]) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = CString::new(format!(
            "/oktt-{:x}-{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        let fixture = Self::try_create_name(name)
            .expect("unavailable-apparatus: exclusive shm fixture creation");
        let length = libc::off_t::try_from(data.len()).expect("bounded fixture length");
        // SAFETY: owned descriptor and checked length. The guard already exists.
        assert_eq!(
            unsafe { libc::ftruncate(fixture.fd.as_raw_fd(), length) },
            0
        );
        if data.is_empty() {
            return fixture;
        }
        // POSIX shm on macOS is mmap-only, so do not write through its fd.
        // SAFETY: the owned object has exactly the required mapping length.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                data.len(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fixture.fd.as_raw_fd(),
                0,
            )
        };
        assert_ne!(addr, libc::MAP_FAILED, "map owned shm fixture");
        // SAFETY: a valid mapping with data.len() bytes and distinct source.
        // No operation between the copy and munmap can panic.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), addr.cast::<u8>(), data.len());
        }
        // SAFETY: release exactly the mapping above.
        assert_eq!(unsafe { libc::munmap(addr, data.len()) }, 0);
        fixture
    }

    fn name(&self) -> &str {
        self.name.to_str().unwrap()
    }
}

#[test]
fn fixture_collision_preserves_existing_shm_bytes() {
    let fixture = OwnedShmFixture::create(&[0xA5; 16]);
    let collision = OwnedShmFixture::try_create_name(fixture.name.clone());
    assert!(matches!(collision, Err(error) if error.raw_os_error() == Some(libc::EEXIST)));
    assert_eq!(
        transport::read_shm_fd(fixture.fd.as_raw_fd(), 32, Some(16)).unwrap(),
        [0xA5; 16]
    );
}

#[test]
fn fixture_panic_unlinks_owned_shm() {
    let mut name = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let fixture = OwnedShmFixture::create(&[0xA5; 16]);
        name = Some(fixture.name.clone());
        panic!("project-authored fixture cleanup probe");
    }));
    assert!(result.is_err());
    let name = name.unwrap();
    // SAFETY: valid name, read-only lookup after the owned fixture was dropped.
    let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
    let error = std::io::Error::last_os_error();
    if fd >= 0 {
        // SAFETY: close only the descriptor returned by this lookup.
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    assert_eq!(fd, -1);
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn fixture_panic_removes_only_its_owned_directory() {
    let neighbor = write_test_rgba_file("neighbor.dat");
    let mut directory = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let path = write_test_rgba_file("panic.dat");
        directory = Some(path._directory.0.clone());
        panic!("project-authored fixture cleanup probe");
    }));
    assert!(result.is_err());
    assert!(!directory.unwrap().exists());
    assert_eq!(std::fs::read(&neighbor).unwrap(), [0xFF; 16]);
}

// ---------------------------------------------------------------------------
// t=f: File transport - success cases
// ---------------------------------------------------------------------------

#[test]
fn named_transports_default_off_rejects_before_host_access() {
    let file = write_test_rgba_file("odytty-g25-default-off-file.dat");
    let marked = write_test_rgba_file("tty-graphics-protocol-odytty-g25-default-off.dat");
    let unmarked = write_test_rgba_file("odytty-g25-default-off-temp.dat");
    let shm = OwnedShmFixture::create(&[0xFF_u8; 16]);
    let shm_name = shm.name();

    let mut terminal = Terminal::new(80, 24);
    for apc in [
        kitty_file_apc(file.to_str().unwrap(), 32, "i=81"),
        kitty_temp_apc(marked.to_str().unwrap(), 32),
        kitty_temp_apc(unmarked.to_str().unwrap(), 32),
        kitty_shm_apc(shm_name, 32, 2, 2),
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
    let c_name = CString::new(shm_name).unwrap();
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
    assert!(fd >= 0, "a denied t=s request must not unlink the name");
    unsafe { libc::close(fd) };

    std::fs::remove_file(file).ok();
    std::fs::remove_file(marked).ok();
    std::fs::remove_file(unmarked).ok();
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
    let dir = FixtureDir::new();
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
// t=f: File transport - security rejections
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
    let ssh_path = "/odytty-fixture-outside-temp/.ssh/id_rsa";
    let mut t = named_transport_terminal();
    let apc = kitty_file_apc(ssh_path, 32, "");
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 0, "~/.ssh rejected");
}

#[test]
fn file_transport_rejects_symlink() {
    let dir = FixtureDir::new();
    let real = dir.join("odytty_g25_real_for_link.dat");
    let link = dir.join("odytty_g25_symlink.dat");
    std::fs::write(&real, [0xFF_u8; 16]).unwrap();
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
    let path = OwnedPath::new("absent.dat");
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

    let path = OwnedPath::new(&format!(
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
        .env("ODYTTY_KITTY_FIFO_TEST_PATH", path.as_os_str())
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
            panic!("FIFO transport subprocess exceeded the bounded rejection window");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
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
    let dir = FixtureDir::new();
    let real = dir.join("odytty_g25_temp_real.dat");
    let link = dir.join("odytty_g25_temp_link.dat");
    std::fs::write(&real, [0xFF_u8; 16]).unwrap();
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
    let shm = OwnedShmFixture::create(&[0xFF_u8; 16]);
    let name = shm.name();

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
        panic!("shm segment should have been unlinked");
    }
}

#[test]
fn shm_reader_rejects_segment_shrunk_after_initial_size_check() {
    let path = OwnedPath::new("shrink.dat");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(16).unwrap();
    let expected = super::kitty_transport::shm_object_size(file.as_raw_fd()).unwrap();
    file.set_len(8).unwrap();

    let result = super::kitty_transport::read_shm_fd_at_size(file.as_raw_fd(), expected, expected);
    assert!(matches!(
        result,
        Err(super::kitty_transport::TransportError::ShmError(_))
    ));
    drop(file);
    std::fs::remove_file(path).ok();
}

#[test]
fn shm_transport_validation_failure_preserves_name() {
    let shm = OwnedShmFixture::create(&[]);
    let name = shm.name();
    let c_name = CString::new(name).unwrap();

    let mut terminal = named_transport_terminal();
    terminal.advance(&kitty_shm_apc(name, 32, 2, 2));
    assert!(terminal.visible_graphics(0).is_empty());

    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
    assert!(fd >= 0, "a rejected t=s object must retain its name");
    unsafe { libc::close(fd) };
}

#[test]
fn shm_transport_without_leading_slash() {
    let shm = OwnedShmFixture::create(&[0xFF_u8; 16]);
    let name_without = shm.name().strip_prefix('/').unwrap();

    let mut t = named_transport_terminal();
    let apc = kitty_shm_apc(name_without, 32, 2, 2);
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "shm without / works");
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
    let shm = OwnedShmFixture::create(&[]);
    let name = shm.name().to_owned();
    drop(shm);
    let apc = kitty_shm_apc(&name, 32, 2, 2);
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
    let dir = FixtureDir::new();
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
    let shm = OwnedShmFixture::create(&png_data);
    let name = shm.name();

    let name_b64 = simple_base64(name.as_bytes());
    let apc = format!("\x1b_Ga=T,t=s,f=100;{name_b64}\x1b\\").into_bytes();
    let mut t = named_transport_terminal();
    t.advance(&apc);
    assert_eq!(t.visible_graphics(0).len(), 1, "PNG via shm placed");
}

#[test]
fn temp_transport_deletes_even_on_decode_failure() {
    let dir = FixtureDir::new();
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

/// A path inside an exclusively owned scratch directory.
fn temp_path(tag: &str) -> OwnedPath {
    OwnedPath::new(tag)
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
    // Walk back to the filesystem root before selecting an existing parent;
    // macOS temp directories are nested more deeply than Linux `/tmp`, and a
    // single `..` can otherwise select a non-existent sibling.
    let mut escape = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let temp_depth = escape
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count();
    for _ in 0..temp_depth {
        escape.push("..");
    }
    escape.push("etc/passwd");
    assert_eq!(
        transport::read_file_transport(&path_bytes(&escape), 4096),
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

    let marked = OwnedPath::new(&format!(
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
    let too_big = OwnedPath::new(&format!(
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
            transport::read_shm_transport(name, 4096, None),
            Err(TransportError::InvalidPath),
            "malformed shm name {name:?} must be refused before shm_open"
        );
    }

    let single = (b'A'..=b'Z')
        .find_map(|letter| {
            OwnedShmFixture::try_create_name(CString::new(vec![b'/', letter]).unwrap()).ok()
        })
        .expect("unavailable-apparatus: no exclusively owned one-character shm name");
    // A one-character name is a legal POSIX shm name. It must reach shm_open
    // and fail there (or succeed), never be refused as malformed.
    assert!(
        !matches!(
            transport::read_shm_transport(single.name.as_bytes(), 4096, None),
            Err(TransportError::InvalidPath)
        ),
        "a single-character shm name is legal and must not be refused as malformed"
    );
}

#[test]
fn shm_reader_reports_the_open_failure_rather_than_a_later_stage() {
    let shm = OwnedShmFixture::create(&[]);
    let name = shm.name().to_owned();
    drop(shm);
    match transport::read_shm_transport(name.as_bytes(), 4096, None) {
        Err(TransportError::ShmError(message)) => assert!(
            message.contains("shm_open"),
            "a failed open must be classified as such, got {message}"
        ),
        other => panic!("a nonexistent segment must fail at open, got {other:?}"),
    }
}

#[test]
fn shm_read_length_admits_the_exact_cap_and_refuses_one_byte_more() {
    assert_eq!(
        transport::shm_read_len(64, None, 64),
        Ok(64),
        "a whole segment exactly at the cap is admitted"
    );
    assert_eq!(
        transport::shm_read_len(64, None, 65),
        Ok(64),
        "a whole segment below the cap is admitted"
    );
    assert_eq!(
        transport::shm_read_len(64, None, 63),
        Err(TransportError::TooLarge),
        "a whole segment one byte over the cap is refused before any mapping"
    );
    assert_eq!(
        transport::shm_read_len(64, Some(64), 63),
        Err(TransportError::TooLarge),
        "a transmitted length one byte over the cap is refused"
    );
    assert!(matches!(
        transport::shm_read_len(64, Some(0), 64),
        Err(TransportError::ShmError(_))
    ));
}

#[test]
fn shm_read_length_is_the_transmitted_length_under_a_page_rounded_object() {
    // macOS reports a shared-memory object's size rounded up to a whole page
    // (16 KiB on Apple silicon). A 22-byte payload in such an object fits a
    // 4 KiB cap: the cap applies to what is read, not to the rounded size.
    assert_eq!(transport::shm_read_len(16_384, Some(22), 4096), Ok(22));
    assert_eq!(
        transport::shm_read_len(16_384, None, 4096),
        Err(TransportError::TooLarge),
        "with no transmitted length the whole object is read and capped"
    );
    assert_eq!(
        transport::shm_read_len(16, Some(64), 4096),
        Ok(16),
        "a transmitted length beyond the object reads only the object"
    );
}

#[test]
fn shm_object_size_refuses_an_empty_segment() {
    let path = temp_path("shm-size");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(64).unwrap();
    let fd = file.as_raw_fd();
    assert_eq!(transport::shm_object_size(fd), Ok(64));

    file.set_len(0).unwrap();
    assert!(
        matches!(
            transport::shm_object_size(fd),
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
    match transport::shm_object_size(-1) {
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
    assert_eq!(transport::shm_object_size(fd), Ok(32));

    match transport::read_shm_fd_at_size(fd, 32, 32) {
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
