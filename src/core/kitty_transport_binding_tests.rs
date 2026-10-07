// SPDX-License-Identifier: GPL-3.0-only
//! Object-binding regressions for the opt-in Kitty named transports.
//!
//! The admitted directory, the bytes read, and the name deleted afterwards must
//! all be the same objects. Each test owns an exclusively created fixture
//! directory or shared-memory name, removed by a guard even when an assertion
//! fails, and uses the test-only interleaving hooks to rebind a name or swap a
//! directory at the moment a concurrent process could.

use super::kitty_transport as transport;
use transport::TransportError;
use transport::test_hooks::{self, Stage};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    )
}

/// An exclusively created directory inside the platform temp root.
struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("odytty-binding-{tag}-{}", unique_suffix()));
        std::fs::create_dir(&path).expect("create exclusive fixture directory");
        Self(path)
    }

    #[cfg(unix)]
    fn child_dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir(&path).expect("create fixture child directory");
        path
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_str()
        .expect("fixture paths are UTF-8")
        .as_bytes()
        .to_vec()
}

const ADMITTED: &[u8] = b"admitted fixture bytes";

// ---------------------------------------------------------------------------
// Portable: the binding must keep the ordinary paths working (these run on the
// Windows leg as well, where the opened-handle location check is in force).
// ---------------------------------------------------------------------------

#[test]
fn file_transport_reads_an_admitted_regular_file() {
    let fixture = FixtureDir::new("file");
    let path = fixture.0.join("image.dat");
    std::fs::write(&path, ADMITTED).expect("seed fixture file");
    assert_eq!(
        transport::read_file_transport(&path_bytes(&path), 4096),
        Ok(ADMITTED.to_vec())
    );
    assert!(path.exists(), "t=f never deletes");
}

#[test]
fn temp_transport_reads_then_deletes_the_same_file() {
    let fixture = FixtureDir::new("temp");
    let path = fixture.0.join("tty-graphics-protocol-binding.dat");
    std::fs::write(&path, ADMITTED).expect("seed fixture file");
    assert_eq!(
        transport::read_temp_transport(&path_bytes(&path), 4096),
        Ok(ADMITTED.to_vec())
    );
    assert!(!path.exists(), "the file that was read is deleted");
}

#[test]
fn temp_transport_tolerates_a_name_removed_before_deletion() {
    let fixture = FixtureDir::new("temp-gone");
    let path = fixture.0.join("tty-graphics-protocol-gone.dat");
    std::fs::write(&path, ADMITTED).expect("seed fixture file");
    let removed = path.clone();
    let _hook = test_hooks::install(move |stage| {
        if stage == Stage::BeforeDelete {
            std::fs::remove_file(&removed).expect("remove fixture name");
        }
    });
    assert_eq!(
        transport::read_temp_transport(&path_bytes(&path), 4096),
        Ok(ADMITTED.to_vec()),
        "a name nobody rebound leaves nothing to delete and the read stands"
    );
}

#[test]
fn transport_paths_outside_the_temp_roots_stay_refused() {
    let outside = std::env::current_dir()
        .expect("current directory")
        .join("Cargo.toml");
    assert_eq!(
        transport::read_file_transport(&path_bytes(&outside), 1 << 20),
        Err(TransportError::PathNotAllowed)
    );
}

// ---------------------------------------------------------------------------
// Unix: deterministic rebinding at the interleaving points.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn ancestor_swapped_after_admission_cannot_redirect_the_read() {
    // `/etc/passwd` is a readable regular file outside every admitted root on
    // Linux and macOS. The fixture never writes to it.
    let outside = std::fs::read("/etc/passwd").expect("read the outside control file");
    assert_ne!(outside, ADMITTED);

    let fixture = FixtureDir::new("ancestor");
    let stage = fixture.child_dir("stage");
    std::fs::write(stage.join("passwd"), ADMITTED).expect("seed admitted file");
    let held = fixture.0.join("stage-held");
    let swap_from = stage.clone();
    let _hook = test_hooks::install(move |point| {
        if point == Stage::AfterAdmission {
            std::fs::rename(&swap_from, &held).expect("move admitted directory");
            std::os::unix::fs::symlink("/etc", &swap_from).expect("plant ancestor link");
        }
    });

    let result = transport::read_file_transport(&path_bytes(&stage.join("passwd")), 1 << 20);
    assert_ne!(
        result,
        Ok(outside),
        "a directory swapped after admission must not redirect the read outside the temp roots"
    );
    assert_eq!(
        result,
        Ok(ADMITTED.to_vec()),
        "the read is bound to the directory that was admitted"
    );
}

#[cfg(unix)]
#[test]
fn temp_name_rebound_before_deletion_keeps_the_new_object() {
    let fixture = FixtureDir::new("temp-rebound");
    let path = fixture.0.join("tty-graphics-protocol-rebound.dat");
    std::fs::write(&path, ADMITTED).expect("seed fixture file");
    let moved = fixture.0.join("moved-original.dat");
    let rebound = path.clone();
    let _hook = test_hooks::install(move |point| {
        if point == Stage::BeforeDelete {
            std::fs::rename(&rebound, &moved).expect("move the file that was read");
            std::fs::write(&rebound, b"unrelated object").expect("rebind the name");
        }
    });

    let result = transport::read_temp_transport(&path_bytes(&path), 4096);
    assert_eq!(
        std::fs::read(&path).ok(),
        Some(b"unrelated object".to_vec()),
        "deletion must not remove an object other than the one that was read"
    );
    assert_eq!(result, Err(TransportError::ObjectChanged));
}

#[cfg(unix)]
#[test]
fn temp_directory_swapped_before_deletion_keeps_the_other_directory_intact() {
    let fixture = FixtureDir::new("temp-dir-swap");
    let stage = fixture.child_dir("stage");
    let other = fixture.child_dir("other");
    let name = "tty-graphics-protocol-swap.dat";
    std::fs::write(stage.join(name), ADMITTED).expect("seed admitted file");
    std::fs::write(other.join(name), b"unrelated object").expect("seed other file");
    let held = fixture.0.join("stage-held");
    let swap_from = stage.clone();
    let swap_to = other.clone();
    let _hook = test_hooks::install(move |point| {
        if point == Stage::BeforeDelete {
            std::fs::rename(&swap_from, &held).expect("move admitted directory");
            std::os::unix::fs::symlink(&swap_to, &swap_from).expect("plant directory link");
        }
    });

    let result = transport::read_temp_transport(&path_bytes(&stage.join(name)), 4096);
    assert_eq!(result, Ok(ADMITTED.to_vec()));
    assert_eq!(
        std::fs::read(other.join(name)).ok(),
        Some(b"unrelated object".to_vec()),
        "deletion follows the admitted directory, not a later binding of its path"
    );
    assert!(
        !fixture.0.join("stage-held").join(name).exists(),
        "the file that was read is the one deleted"
    );
}

#[cfg(unix)]
mod shm {
    use super::*;
    use std::ffi::CString;

    /// An exclusively created shared-memory name, unlinked by the guard.
    pub(super) struct ShmFixture(pub(super) CString);

    impl ShmFixture {
        pub(super) fn new(tag: &str, data: &[u8]) -> Self {
            // macOS limits shared-memory names to 31 bytes, so the name is a
            // short tag plus process id and a counter.
            let name = format!(
                "/odb{tag}{}-{}",
                std::process::id() % 100_000,
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            );
            let name = CString::new(name).expect("shm name");
            create_exclusive(&name, data);
            Self(name)
        }

        pub(super) fn exists(&self) -> bool {
            let fd = unsafe { libc::shm_open(self.0.as_ptr(), libc::O_RDONLY, 0) };
            if fd < 0 {
                return false;
            }
            unsafe { libc::close(fd) };
            true
        }
    }

    impl Drop for ShmFixture {
        fn drop(&mut self) {
            unsafe { libc::shm_unlink(self.0.as_ptr()) };
        }
    }

    pub(super) fn create_exclusive(name: &CString, data: &[u8]) {
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        assert!(fd >= 0, "exclusive shm_open for an owned fixture name");
        assert_eq!(
            unsafe { libc::ftruncate(fd, data.len() as libc::off_t) },
            0,
            "size the owned fixture"
        );
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
        assert!(addr != libc::MAP_FAILED, "map the owned fixture");
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), addr.cast::<u8>(), data.len());
            libc::munmap(addr, data.len());
            libc::close(fd);
        }
    }

    #[test]
    fn shm_read_unlinks_the_object_that_was_read() {
        let fixture = ShmFixture::new("r", ADMITTED);
        assert_eq!(
            transport::read_shm_transport(fixture.0.as_bytes(), 4096, Some(ADMITTED.len())),
            Ok(ADMITTED.to_vec())
        );
        assert!(!fixture.exists(), "the object that was read is unlinked");
    }

    #[test]
    fn shm_name_removed_before_unlink_keeps_the_read() {
        let fixture = ShmFixture::new("g", ADMITTED);
        let name = fixture.0.clone();
        let _hook = test_hooks::install(move |point| {
            if point == Stage::BeforeDelete {
                unsafe { libc::shm_unlink(name.as_ptr()) };
            }
        });
        assert_eq!(
            transport::read_shm_transport(fixture.0.as_bytes(), 4096, Some(ADMITTED.len())),
            Ok(ADMITTED.to_vec())
        );
        assert!(!fixture.exists());
    }

    #[test]
    fn shm_name_rebound_before_unlink_keeps_the_new_object() {
        let fixture = ShmFixture::new("b", ADMITTED);
        let name = fixture.0.clone();
        // A size in a different page count keeps the rebinding detectable even
        // where the platform reports no object number for shared memory and
        // rounds the reported size up to a whole page (macOS).
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        let replacement = vec![b'u'; 2 * page + 1];
        let _hook = test_hooks::install(move |point| {
            if point == Stage::BeforeDelete {
                unsafe { libc::shm_unlink(name.as_ptr()) };
                create_exclusive(&name, &replacement);
            }
        });

        let result =
            transport::read_shm_transport(fixture.0.as_bytes(), 4096, Some(ADMITTED.len()));
        assert!(
            fixture.exists(),
            "unlink must not remove an object other than the one that was read"
        );
        assert_eq!(result, Err(TransportError::ObjectChanged));
    }
}
