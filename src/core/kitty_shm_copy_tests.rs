// SPDX-License-Identifier: GPL-3.0-only
//! The shared-memory readers' child reaping and post-copy size check, driven
//! through deterministic seams. The helper tests run on every Unix; the
//! isolated-copy tests exercise the macOS reader and run only on macOS.

use super::kitty_transport::{TransportError, ensure_size_unchanged, reap_child};
use std::io;

fn interrupted() -> io::Error {
    io::Error::from_raw_os_error(libc::EINTR)
}

#[test]
fn reaping_retries_interrupted_waits() {
    let mut calls = 0;
    let status = reap_child(4242, |pid, status| {
        calls += 1;
        assert_eq!(pid, 4242);
        if calls < 3 {
            return Err(interrupted());
        }
        *status = 0;
        Ok(pid)
    });
    assert_eq!((status, calls), (Some(0), 3));
}

#[test]
fn reaping_reports_other_wait_failures() {
    let status = reap_child(4242, |_, _| Err(io::Error::from_raw_os_error(libc::ECHILD)));
    assert_eq!(status, None);
    assert_eq!(reap_child(4242, |_, _| Ok(7)), None);
}

#[test]
fn a_size_change_after_the_copy_is_refused() {
    assert_eq!(ensure_size_unchanged(Ok(16), 16), Ok(()));
    assert!(matches!(
        ensure_size_unchanged(Ok(17), 16),
        Err(TransportError::ShmError(_))
    ));
    assert_eq!(
        ensure_size_unchanged(Err(TransportError::TooLarge), 16),
        Err(TransportError::TooLarge)
    );
}

#[cfg(target_os = "macos")]
mod isolated_copy {
    use super::super::kitty_transport::{
        IsolatedCopyOps, TransportError, read_shm_isolated, shm_object_size,
    };
    use super::interrupted;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A uniquely named shm segment holding `data`, unlinked on drop.
    struct Segment {
        name: CString,
        fd: i32,
    }

    impl Segment {
        fn new(data: &[u8]) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            // macOS limits shm names to 31 bytes; pid and a counter keep this
            // one unique across parallel tests and runs.
            let name = CString::new(format!(
                "/odytty-cp-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .unwrap();
            // SAFETY: a valid NUL-terminated name; the descriptor is owned.
            let fd = unsafe {
                libc::shm_open(
                    name.as_ptr(),
                    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                    0o600,
                )
            };
            assert!(fd >= 0, "create owned shm segment");
            assert_eq!(unsafe { libc::ftruncate(fd, data.len() as libc::off_t) }, 0);
            // SAFETY: map exactly the segment length for one copy, then unmap.
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
            assert_ne!(addr, libc::MAP_FAILED);
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), addr.cast::<u8>(), data.len());
                assert_eq!(libc::munmap(addr, data.len()), 0);
            }
            Self { name, fd }
        }
    }

    impl Drop for Segment {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.fd);
                libc::shm_unlink(self.name.as_ptr());
            }
        }
    }

    /// Interrupts the first wait, then waits for real; reports `late_size`
    /// from the second size query on, as if the segment were resized while
    /// the child copied it.
    struct Steered {
        interrupts: usize,
        size_queries: usize,
        late_size: Option<usize>,
        reaped: Option<libc::pid_t>,
    }

    impl IsolatedCopyOps for Steered {
        fn wait(&mut self, pid: libc::pid_t, status: &mut i32) -> std::io::Result<libc::pid_t> {
            if self.interrupts > 0 {
                self.interrupts -= 1;
                return Err(interrupted());
            }
            let reaped = unsafe { libc::waitpid(pid, status, 0) };
            self.reaped = Some(reaped);
            if reaped < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(reaped)
            }
        }

        fn size(&mut self, fd: i32) -> Result<usize, TransportError> {
            self.size_queries += 1;
            match self.late_size {
                Some(size) if self.size_queries > 1 => Ok(size),
                _ => shm_object_size(fd),
            }
        }
    }

    fn no_child_left(pid: libc::pid_t) -> bool {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
    }

    #[test]
    fn an_interrupted_wait_still_reaps_the_copy_child() {
        let segment = Segment::new(&[0x5a; 32]);
        let mut ops = Steered {
            interrupts: 1,
            size_queries: 0,
            late_size: None,
            reaped: None,
        };
        // macOS reports the page-rounded object size; 32 bytes are read.
        let object_size = shm_object_size(segment.fd).unwrap();
        let bytes = read_shm_isolated(segment.fd, object_size, 32, &mut ops).unwrap();
        assert_eq!(bytes, [0x5a; 32]);
        let pid = ops.reaped.expect("the child was waited for");
        assert!(pid > 0);
        assert!(no_child_left(pid));
        assert_eq!(ops.size_queries, 2, "size is re-read after the copy");
    }

    #[test]
    fn a_size_change_during_the_isolated_copy_is_refused() {
        let segment = Segment::new(&[0x5a; 32]);
        let mut ops = Steered {
            interrupts: 0,
            size_queries: 0,
            late_size: Some(48),
            reaped: None,
        };
        let object_size = shm_object_size(segment.fd).unwrap();
        let result = read_shm_isolated(segment.fd, object_size, 32, &mut ops);
        assert!(matches!(result, Err(TransportError::ShmError(_))));
        assert!(no_child_left(ops.reaped.expect("reaped before the check")));
    }
}
