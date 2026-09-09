// SPDX-License-Identifier: GPL-3.0-only
//! Lazy process owner for the local structural-control endpoint.
//!
//! The protocol and transport remain independent of the GUI. This module owns
//! only their native lifecycle: explicit opt-in, post-first-frame binding,
//! process-instance entropy, bounded event-loop dispatch, and teardown.

use crate::automation::dispatch::DispatchQueue;
use crate::automation::protocol::{Reply, Request};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs::{self, Metadata};
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::path::PathBuf;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::automation::unix::Server;
#[cfg(windows)]
use crate::automation::windows::Server;

#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_STALE_ENTRIES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum ReconcileOutcome {
    Unchanged,
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos", windows)),
        allow(dead_code)
    )]
    Started(String),
    Stopped,
    Unavailable(String),
    /// The listener thread stopped on its own after a successful start. The
    /// runtime has already been torn down; no retry happens until the setting
    /// is toggled off and on again.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos", windows)),
        allow(dead_code)
    )]
    Faulted(String),
}

/// One endpoint for the process. Default construction allocates no entropy,
/// queue, socket, or thread; every resource remains behind explicit opt-in and
/// the first-presented-frame gate.
#[derive(Default)]
pub(in crate::native) struct AutomationRuntime {
    attempted: bool,
    instance: Option<[u8; 16]>,
    queue: Option<DispatchQueue>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    server: Option<Server>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    endpoint: Option<PathBuf>,
}

impl AutomationRuntime {
    pub(in crate::native) fn reconcile(
        &mut self,
        enabled: bool,
        first_frame_presented: bool,
        wake: impl Fn() -> bool + Send + Sync + 'static,
    ) -> ReconcileOutcome {
        if !enabled {
            self.attempted = false;
            return if self.is_running() {
                self.shutdown();
                ReconcileOutcome::Stopped
            } else {
                ReconcileOutcome::Unchanged
            };
        }

        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if let Some(reason) = self.server.as_ref().and_then(Server::fault) {
            // A listener that exits on its own must not leave the setting
            // reporting an endpoint that no longer accepts clients.
            self.shutdown();
            return ReconcileOutcome::Faulted(reason);
        }
        if let Some(queue) = self.queue.as_mut() {
            // The same opt-in controls endpoint availability and structural
            // mutations. Route policy changes through the queue's epoch seam so
            // a future split setting cannot revive an older queued mutation.
            queue.set_structural_control(enabled);
            return ReconcileOutcome::Unchanged;
        }
        if !first_frame_presented || self.attempted {
            return ReconcileOutcome::Unchanged;
        }
        self.attempted = true;

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            match self.start_unix(wake) {
                Ok(path) => ReconcileOutcome::Started(path.display().to_string()),
                Err(error) => ReconcileOutcome::Unavailable(error.to_string()),
            }
        }

        #[cfg(windows)]
        {
            match self.start_windows(wake) {
                Ok(path) => ReconcileOutcome::Started(path.display().to_string()),
                Err(error) => ReconcileOutcome::Unavailable(error.to_string()),
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = wake;
            ReconcileOutcome::Unavailable(
                "local automation transport is unavailable on this platform".to_owned(),
            )
        }
    }

    #[cfg(windows)]
    fn start_windows(
        &mut self,
        wake: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<PathBuf> {
        let endpoint = crate::automation::windows::endpoint(std::process::id());
        if self.instance.is_none() {
            let mut instance = [0; 16];
            getrandom::fill(&mut instance)
                .map_err(|error| io::Error::other(format!("instance entropy: {error}")))?;
            self.instance = Some(instance);
        }
        let (submission, mut queue) = crate::automation::dispatch::channel(true);
        queue.set_structural_control(true);
        let server = Server::bind(&endpoint, submission, wake)?;
        self.endpoint = Some(endpoint.clone());
        self.queue = Some(queue);
        self.server = Some(server);
        Ok(endpoint)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn start_unix(
        &mut self,
        wake: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<PathBuf> {
        let endpoint = endpoint_path(std::process::id())?;
        self.start_unix_at(endpoint, wake)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(in crate::native) fn start_unix_at(
        &mut self,
        endpoint: PathBuf,
        wake: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<PathBuf> {
        let parent = endpoint.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "automation endpoint has no parent",
            )
        })?;
        cleanup_stale_endpoints(parent)?;

        if self.instance.is_none() {
            let mut instance = [0; 16];
            getrandom::fill(&mut instance)
                .map_err(|error| io::Error::other(format!("instance entropy: {error}")))?;
            self.instance = Some(instance);
        }
        let (submission, mut queue) = crate::automation::dispatch::channel(true);
        queue.set_structural_control(true);
        let server = Server::bind(&endpoint, submission, wake)?;
        self.endpoint = Some(endpoint.clone());
        self.queue = Some(queue);
        self.server = Some(server);
        Ok(endpoint)
    }

    pub(in crate::native) fn dispatch(&self, apply: impl FnMut(Request) -> Reply) -> usize {
        self.queue.as_ref().map_or(0, |queue| queue.dispatch(apply))
    }

    pub(in crate::native) fn instance(&self) -> Option<[u8; 16]> {
        self.instance
    }

    /// True while a dispatch queue exists and the listener has not faulted.
    pub(in crate::native) fn is_running(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if self.server.as_ref().and_then(Server::fault).is_some() {
            return false;
        }
        self.queue.is_some()
    }

    pub(in crate::native) fn shutdown(&mut self) {
        if let Some(queue) = self.queue.as_mut() {
            queue.shutdown();
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            // Unix removes only its owned socket inode. Windows has no
            // filesystem cleanup: the named pipe vanishes with its handles.
            drop(self.server.take());
            self.endpoint = None;
        }
        self.queue = None;
    }

    #[cfg(test)]
    pub(in crate::native) fn install_queue_for_test(
        &mut self,
        instance: [u8; 16],
        queue: DispatchQueue,
    ) {
        self.instance = Some(instance);
        self.queue = Some(queue);
        self.attempted = true;
    }
}

impl Drop for AutomationRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(target_os = "linux")]
fn endpoint_path(pid: u32) -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR");
    linux_endpoint_path_from(base.as_deref(), pid)
}

#[cfg(target_os = "linux")]
fn linux_endpoint_path_from(base: Option<&std::ffi::OsStr>, pid: u32) -> io::Result<PathBuf> {
    let base = base.filter(|value| !value.is_empty()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is required; refusing a /tmp fallback",
        )
    })?;
    let dir = crate::session_host::prepare_runtime_dir(Path::new(&base))
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(dir.join(format!("control-{pid}.sock")))
}

#[cfg(target_os = "macos")]
fn endpoint_path(pid: u32) -> io::Result<PathBuf> {
    let dir = crate::logging::prepare_state_log_dir()?.join("control");
    crate::state_dir::prepare_private_dir(&dir)?;
    Ok(dir.join(format!("control-{pid}.sock")))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn cleanup_stale_endpoints(dir: &Path) -> io::Result<()> {
    let mut count = 0usize;
    for entry in fs::read_dir(dir)? {
        count += 1;
        if count > MAX_STALE_ENTRIES {
            return Err(io::Error::other(
                "automation endpoint cleanup entry limit exceeded",
            ));
        }
        let entry = entry?;
        let Some(pid) = endpoint_pid(&entry.file_name()) else {
            continue;
        };
        if !process_is_dead(pid) {
            continue;
        }
        let initial = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) if metadata.file_type().is_socket() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if process_is_dead(pid) {
            remove_if_same_socket(&entry.path(), &initial)?;
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn endpoint_pid(name: &std::ffi::OsStr) -> Option<i32> {
    let name = name.to_str()?;
    let pid = name.strip_prefix("control-")?.strip_suffix(".sock")?;
    let pid = pid.parse::<i32>().ok()?;
    (pid > 0).then_some(pid)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn process_is_dead(pid: i32) -> bool {
    // SAFETY: signal 0 performs a liveness/permission check and sends no signal.
    let result = unsafe { libc::kill(pid, 0) };
    result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Inode numbers are recycled: on ext4 and tmpfs a socket bound immediately
/// after an unlink commonly receives the freed inode back, so dev/ino alone
/// cannot tell a replacement from the entry observed earlier. The inode change
/// time is part of the identity for that reason.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_if_same_socket(path: &Path, expected: &Metadata) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(current) if current.file_type().is_socket() && same_file(&current, expected) => {
            fs::remove_file(path)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_runtime_is_inert_and_readiness_blocks_start() {
        let mut runtime = AutomationRuntime::default();
        assert!(!runtime.is_running());
        assert_eq!(
            runtime.reconcile(false, true, || panic!("disabled endpoint must not wake")),
            ReconcileOutcome::Unchanged
        );
        assert_eq!(
            runtime.reconcile(true, false, || panic!("pre-frame endpoint must not wake")),
            ReconcileOutcome::Unchanged
        );
        assert!(!runtime.is_running());
        assert!(
            runtime.instance().is_none(),
            "readiness allocates no entropy"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_starts_only_after_readiness_and_shutdown_drops_the_pipe() {
        let mut runtime = AutomationRuntime::default();
        let expected = crate::automation::windows::endpoint(std::process::id());
        assert_eq!(
            runtime.reconcile(true, true, || true),
            ReconcileOutcome::Started(expected.display().to_string())
        );
        assert!(runtime.is_running());
        runtime.shutdown();
        assert!(!runtime.is_running());
    }

    // Fixture directory names stay short: macOS places `temp_dir()` under
    // `/var/folders/...`, and `sun_path` allows 104 bytes there.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stale_cleanup_removes_dead_socket_but_preserves_live_and_non_socket_entries() {
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tag = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("oa-c{:x}-{tag:x}", std::process::id()));
        fs::create_dir(&dir).expect("create fixture dir");

        let stale = dir.join("control-2147483647.sock");
        let live = dir.join(format!("control-{}.sock", std::process::id()));
        let decoy = dir.join("control-2147483646.sock");
        let stale_listener = UnixListener::bind(&stale).expect("stale socket");
        let live_listener = UnixListener::bind(&live).expect("live socket");
        fs::write(&decoy, b"not a socket").expect("decoy");

        cleanup_stale_endpoints(&dir).expect("cleanup");
        assert!(!stale.exists(), "dead owner's socket removed");
        assert!(live.exists(), "live owner's socket preserved");
        assert!(decoy.exists(), "non-socket entry preserved");

        drop(stale_listener);
        drop(live_listener);
        let _ = fs::remove_file(live);
        let _ = fs::remove_file(decoy);
        let _ = fs::remove_dir(dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_endpoint_refuses_to_fall_back_without_xdg_runtime_dir() {
        let error = linux_endpoint_path_from(None, 42).expect_err("missing runtime dir");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("refusing a /tmp fallback"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stale_cleanup_identity_check_preserves_a_replacement_socket() {
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tag = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("oa-r{:x}-{tag:x}", std::process::id()));
        fs::create_dir(&dir).expect("create fixture dir");
        let path = dir.join("control-2147483647.sock");
        let first = UnixListener::bind(&path).expect("first socket");
        let first_metadata = fs::symlink_metadata(&path).expect("first metadata");
        drop(first);
        fs::remove_file(&path).expect("remove first name");
        let replacement = UnixListener::bind(&path).expect("replacement socket");

        remove_if_same_socket(&path, &first_metadata).expect("identity-gated cleanup");
        assert!(path.exists(), "replacement inode must never be unlinked");

        drop(replacement);
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(dir);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_runtime_bind_and_shutdown_own_one_endpoint_lifecycle() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tag = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("oa-l{:x}-{tag:x}", std::process::id()));
        crate::state_dir::prepare_private_dir(&dir).expect("owner-private fixture dir");
        let endpoint = dir.join(format!("control-{}.sock", std::process::id()));
        let mut runtime = AutomationRuntime::default();

        assert_eq!(
            runtime
                .start_unix_at(endpoint.clone(), || true)
                .expect("bind endpoint"),
            endpoint
        );
        assert!(runtime.is_running());
        assert!(endpoint.exists());
        runtime.shutdown();
        assert!(!runtime.is_running());
        assert!(!endpoint.exists(), "owned endpoint removed on shutdown");

        let _ = fs::remove_dir(dir);
    }
}
