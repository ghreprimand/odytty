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
    // The admitted root set is injected, so the outside file is outside it by
    // construction. A path derived from the working directory would sit inside
    // an admitted root whenever the checkout itself lives under a temp root.
    let fixture = FixtureDir::new("outside-root");
    let allowed = fixture.child_dir("allowed");
    let outside = fixture.child_dir("outside");
    let file = outside.join("control.bin");
    std::fs::write(&file, ADMITTED).expect("seed outside control file");
    let _roots = test_hooks::restrict_roots(&[&allowed]);
    assert_eq!(
        transport::read_file_transport(&path_bytes(&file), 1 << 20),
        Err(TransportError::PathNotAllowed)
    );
    assert!(file.exists(), "a refused read leaves the file in place");
}

// ---------------------------------------------------------------------------
// Unix: deterministic rebinding at the interleaving points.
// ---------------------------------------------------------------------------

/// Project-authored bytes placed outside the injected admitted root. A read
/// redirected through a planted link would return exactly these bytes.
#[cfg(unix)]
const OUTSIDE: &[u8] = b"outside control bytes";

/// A fixture whose only admitted root is its `allowed` child, with an
/// `outside` sibling holding the outside control file `passwd`. The injected
/// root keeps the control project-authored and inside the fixture.
#[cfg(unix)]
struct Confined {
    allowed: PathBuf,
    outside: PathBuf,
    _roots: test_hooks::RootsGuard,
    _fixture: FixtureDir,
}

#[cfg(unix)]
impl Confined {
    fn new(tag: &str) -> Self {
        let fixture = FixtureDir::new(tag);
        let allowed = fixture.child_dir("allowed");
        let outside = fixture.child_dir("outside");
        std::fs::write(outside.join("passwd"), OUTSIDE).expect("seed outside control file");
        let roots = test_hooks::restrict_roots(&[&allowed]);
        Self {
            allowed,
            outside,
            _roots: roots,
            _fixture: fixture,
        }
    }
}

#[cfg(unix)]
#[test]
fn ancestor_swapped_after_admission_cannot_redirect_the_read() {
    let confined = Confined::new("ancestor");
    let stage = confined.allowed.join("stage");
    std::fs::create_dir(&stage).expect("create admitted directory");
    std::fs::write(stage.join("passwd"), ADMITTED).expect("seed admitted file");
    let held = confined.allowed.join("stage-held");
    let (swap_from, target) = (stage.clone(), confined.outside.clone());
    let _hook = test_hooks::install(move |point| {
        if point == Stage::AfterAdmission {
            std::fs::rename(&swap_from, &held).expect("move admitted directory");
            std::os::unix::fs::symlink(&target, &swap_from).expect("plant ancestor link");
        }
    });

    let result = transport::read_file_transport(&path_bytes(&stage.join("passwd")), 1 << 20);
    assert_ne!(
        result,
        Ok(OUTSIDE.to_vec()),
        "a directory swapped after admission must not redirect the read outside the admitted root"
    );
    assert_eq!(
        result,
        Ok(ADMITTED.to_vec()),
        "the read is bound to the directory that was admitted"
    );
}

/// Swap `link` between a symlink to `target` and the real directory parked at
/// `held`, once per admission interleaving point.
#[cfg(unix)]
fn toggle_link(link: &Path, held: &Path, target: &Path) {
    if std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(link).expect("remove planted link");
        std::fs::rename(held, link).expect("restore admitted directory");
    } else {
        std::fs::rename(link, held).expect("park admitted directory");
        std::os::unix::fs::symlink(target, link).expect("plant ancestor link");
    }
}

#[cfg(unix)]
#[test]
fn ancestor_flipped_during_admission_cannot_redirect_the_read() {
    let confined = Confined::new("admit-flip");
    let held = confined.allowed.join("stage-held");
    std::fs::create_dir(&held).expect("create admitted directory");
    std::fs::write(held.join("passwd"), ADMITTED).expect("seed admitted file");
    let stage = confined.allowed.join("stage");
    std::os::unix::fs::symlink(&confined.outside, &stage).expect("plant ancestor link");
    // An adversary flips the ancestor at every admission interleaving point:
    // a link when the directory is resolved or opened, the real directory when
    // its containment is judged.
    let (link, park, target) = (stage.clone(), held.clone(), confined.outside.clone());
    let _hook = test_hooks::install(move |point| {
        if point == Stage::DuringAdmission {
            toggle_link(&link, &park, &target);
        }
    });

    let result = transport::read_file_transport(&path_bytes(&stage.join("passwd")), 1 << 20);
    assert_ne!(
        result,
        Ok(OUTSIDE.to_vec()),
        "admission must not hand out a directory outside the admitted root"
    );
}

#[cfg(unix)]
#[test]
fn ancestor_swapped_during_admission_refuses_the_path() {
    let confined = Confined::new("admit-swap");
    let stage = confined.allowed.join("stage");
    std::fs::create_dir(&stage).expect("create admitted directory");
    std::fs::write(stage.join("passwd"), ADMITTED).expect("seed admitted file");
    let held = confined.allowed.join("stage-held");
    let (link, park, target) = (stage.clone(), held.clone(), confined.outside.clone());
    let _hook = test_hooks::install(move |point| {
        if point == Stage::DuringAdmission {
            toggle_link(&link, &park, &target);
        }
    });

    let result = transport::read_file_transport(&path_bytes(&stage.join("passwd")), 1 << 20);
    assert_eq!(
        result,
        Err(TransportError::PathNotAllowed),
        "a link met while walking the admitted path refuses admission"
    );
}

#[cfg(unix)]
#[test]
fn injected_root_admits_its_own_files_and_refuses_the_outside_control() {
    let confined = Confined::new("roots");
    std::fs::write(confined.allowed.join("image.dat"), ADMITTED).expect("seed admitted file");
    assert_eq!(
        transport::read_file_transport(&path_bytes(&confined.allowed.join("image.dat")), 4096),
        Ok(ADMITTED.to_vec())
    );
    assert_eq!(
        transport::read_file_transport(&path_bytes(&confined.outside.join("passwd")), 4096),
        Err(TransportError::PathNotAllowed),
        "the outside control lies outside the only admitted root"
    );
}

#[cfg(unix)]
#[test]
fn temp_name_rebound_with_equal_metadata_keeps_the_new_object() {
    let fixture = FixtureDir::new("temp-equal");
    let path = fixture.0.join("tty-graphics-protocol-equal.dat");
    std::fs::write(&path, ADMITTED).expect("seed fixture file");
    let moved = fixture.0.join("moved-original.dat");
    let rebound = path.clone();
    let replacement = vec![b'u'; ADMITTED.len()];
    let expected = replacement.clone();
    let _hook = test_hooks::install(move |point| {
        if point == Stage::BeforeDelete {
            std::fs::rename(&rebound, &moved).expect("move the file that was read");
            std::fs::write(&rebound, &replacement).expect("rebind the name");
        }
    });

    let result = transport::read_temp_transport(&path_bytes(&path), 4096);
    assert_eq!(
        std::fs::read(&path).ok(),
        Some(expected),
        "an equal-size replacement must not be deleted"
    );
    assert_eq!(result, Err(TransportError::ObjectChanged));
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
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

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
            let fd = open_exclusive(&name);
            // The guard owns the name before any sizing or mapping can fail.
            let fixture = Self(name);
            fill(fd, data);
            fixture
        }

        pub(super) fn exists(&self) -> bool {
            let fd = unsafe { libc::shm_open(self.0.as_ptr(), libc::O_RDONLY, 0) };
            if fd < 0 {
                return false;
            }
            unsafe { libc::close(fd) };
            true
        }

        /// Whether the platform reports an object number for this object, the
        /// condition under which the transport unlinks a name after a read.
        pub(super) fn identity_proves_object(&self) -> bool {
            let fd = unsafe { libc::shm_open(self.0.as_ptr(), libc::O_RDONLY, 0) };
            assert!(fd >= 0, "reopen the owned fixture");
            // SAFETY: successful shm_open transfers this descriptor to OwnedFd.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            transport::shm_identity(fd.as_raw_fd())
                .is_some_and(|id| transport::shm_identity_proves_object(&id))
        }
    }

    impl Drop for ShmFixture {
        fn drop(&mut self) {
            unsafe { libc::shm_unlink(self.0.as_ptr()) };
        }
    }

    /// Exclusively create `name`. The returned descriptor closes on every exit;
    /// the caller's guard owns the name.
    pub(super) fn open_exclusive(name: &CString) -> OwnedFd {
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        assert!(fd >= 0, "exclusive shm_open for an owned fixture name");
        // SAFETY: successful shm_open transfers this descriptor to OwnedFd.
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    /// Size an owned object and copy `data` into it through a guarded mapping.
    pub(super) fn fill(fd: OwnedFd, data: &[u8]) {
        struct Mapping(*mut libc::c_void, usize);
        impl Drop for Mapping {
            fn drop(&mut self) {
                unsafe { libc::munmap(self.0, self.1) };
            }
        }

        assert_eq!(
            unsafe { libc::ftruncate(fd.as_raw_fd(), data.len() as libc::off_t) },
            0,
            "size the owned fixture"
        );
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                data.len(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        assert!(addr != libc::MAP_FAILED, "map the owned fixture");
        let mapping = Mapping(addr, data.len());
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapping.0.cast::<u8>(), data.len());
        }
    }

    /// Exclusively create `name` holding `data`; the caller's guard owns the name.
    pub(super) fn create_exclusive(name: &CString, data: &[u8]) {
        fill(open_exclusive(name), data);
    }

    #[test]
    fn an_identity_without_an_object_number_never_authorizes_an_unlink() {
        // Owner, mode and size alone, as a platform reporting no object number
        // for shared memory leaves them, are shared by a same-sized replacement.
        let equal_metadata: transport::ShmIdentity = (0, 0, 1000, 0o100600, 16384);
        assert!(!transport::shm_identity_proves_object(&equal_metadata));
        let numbered: transport::ShmIdentity = (23, 4242, 1000, 0o100600, 16384);
        assert!(transport::shm_identity_proves_object(&numbered));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reports_an_object_number_for_shared_memory() {
        let fixture = ShmFixture::new("n", ADMITTED);
        assert!(
            fixture.identity_proves_object(),
            "Linux keeps identity-checked unlinking for t=s"
        );
    }

    #[test]
    fn shm_read_unlinks_the_object_that_was_read() {
        let fixture = ShmFixture::new("r", ADMITTED);
        let provable = fixture.identity_proves_object();
        assert_eq!(
            transport::read_shm_transport(fixture.0.as_bytes(), 4096, Some(ADMITTED.len())),
            Ok(ADMITTED.to_vec())
        );
        assert_eq!(
            fixture.exists(),
            !provable,
            "the object that was read is unlinked exactly where its identity is provable"
        );
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
        let provable = fixture.identity_proves_object();
        let name = fixture.0.clone();
        // A replacement in a different page count, so that size alone would
        // tell the objects apart.
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
        if provable {
            assert_eq!(result, Err(TransportError::ObjectChanged));
        } else {
            assert_eq!(
                result,
                Ok(ADMITTED.to_vec()),
                "the name is retained and the read stands"
            );
        }
    }

    #[test]
    fn shm_name_rebound_with_equal_metadata_keeps_the_new_object() {
        let fixture = ShmFixture::new("e", ADMITTED);
        let provable = fixture.identity_proves_object();
        let name = fixture.0.clone();
        // Same owner, mode and size as the object that was read: only an
        // object number can tell the two apart.
        let replacement = vec![b'u'; ADMITTED.len()];
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
            "an equal-metadata replacement must never be unlinked"
        );
        if provable {
            assert_eq!(result, Err(TransportError::ObjectChanged));
        } else {
            assert_eq!(
                result,
                Ok(ADMITTED.to_vec()),
                "the name is retained and the read stands"
            );
        }
    }
}
