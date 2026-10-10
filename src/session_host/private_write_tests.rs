// SPDX-License-Identifier: GPL-3.0-only
//! Link-planting fixtures for the private files in the session runtime
//! directory. Everything is project-authored and lives in a fresh directory.
use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Self {
        Self(crate::test_dirs::fresh_socket_dir(tag))
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

/// A world-readable file outside the runtime directory that a planted link
/// points at. Nothing may truncate, rewrite or chmod it.
fn victim(root: &Dir) -> PathBuf {
    let path = root.0.join("victim");
    fs::write(&path, b"victim bytes").expect("seed victim");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod victim");
    path
}

fn sample(id: &str) -> SessionMetadata {
    SessionMetadata {
        id: id.to_owned(),
        name: "Linked".to_owned(),
        created_unix_ms: 7,
        pane_count: 3,
    }
}

#[test]
fn metadata_writer_refuses_a_planted_link_and_leaves_its_target_alone() {
    let root = Dir::new("sh-pw-link");
    let runtime_dir = prepare_runtime_dir(&root.0).expect("runtime dir");
    let target = victim(&root);
    let leaf = session_metadata_path(&runtime_dir, "linked").expect("metadata path");
    symlink(&target, &leaf).expect("plant link");

    assert!(write_session_metadata(&runtime_dir, &sample("linked")).is_err());
    assert_eq!(fs::read(&target).expect("victim"), b"victim bytes");
    assert_eq!(mode(&target), 0o644);
}

#[test]
fn metadata_writer_publishes_an_owner_private_file_without_leftovers() {
    let root = Dir::new("sh-pw-ok");
    let runtime_dir = prepare_runtime_dir(&root.0).expect("runtime dir");
    write_session_metadata(&runtime_dir, &sample("plain")).expect("first write");
    let mut second = sample("plain");
    second.name = "Renamed".to_owned();
    write_session_metadata(&runtime_dir, &second).expect("replacing write");

    let leaf = session_metadata_path(&runtime_dir, "plain").expect("metadata path");
    assert_eq!(mode(&leaf), 0o600);
    let read = read_session_metadata(&runtime_dir, "plain")
        .expect("read")
        .expect("present");
    assert_eq!(read, second);
    let names: Vec<_> = fs::read_dir(&runtime_dir)
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names.len(), 1, "no temporary sibling remains: {names:?}");
}

#[test]
fn startup_lock_refuses_a_planted_link_and_leaves_its_target_alone() {
    let root = Dir::new("sh-pw-lock");
    let runtime_dir = prepare_runtime_dir(&root.0).expect("runtime dir");
    let target = victim(&root);
    let lock = runtime_dir.join("linked.sock.lock");
    symlink(&target, &lock).expect("plant link");

    assert!(StartupLock::acquire(&lock).is_err());
    assert_eq!(fs::read(&target).expect("victim"), b"victim bytes");
    assert_eq!(mode(&target), 0o644);
}

#[test]
fn startup_lock_stays_exclusive_and_repairs_a_loose_regular_file() {
    let root = Dir::new("sh-pw-lock-ok");
    let runtime_dir = prepare_runtime_dir(&root.0).expect("runtime dir");
    let lock = runtime_dir.join("plain.sock.lock");
    fs::write(&lock, b"").expect("seed lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).expect("loosen lock");

    let held = StartupLock::acquire(&lock).expect("first holder");
    assert_eq!(mode(&lock), 0o600);
    assert!(StartupLock::acquire(&lock).is_err(), "second holder");
    drop(held);
    StartupLock::acquire(&lock).expect("released lock is reusable");
}
