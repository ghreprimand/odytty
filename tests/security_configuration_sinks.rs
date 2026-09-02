// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use odytty::connection_hosts::{
    AppendHostOutcome, ConnectionHost, ConnectionHostSource, HostsEditOutcome, append_adhoc_host,
    edit_host_block, parse_odytty_hosts_bytes, remove_host_block,
};
use odytty::ssh_connect::{remote_cleanup_command, remote_upload_command, remote_upload_target};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "odytty-security-{prefix}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create synthetic temporary directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn host(alias: &str) -> ConnectionHost {
    ConnectionHost {
        alias: alias.to_owned(),
        host_name: Some("node.example.invalid".to_owned()),
        user: Some("operator".to_owned()),
        port: Some(2200),
        theme: Some("odyssey-default".to_owned()),
        font: Some("Mono".to_owned()),
        title: Some("Synthetic node".to_owned()),
        integration: Some(true),
        reuse: Some(true),
        tmux: Some(false),
        protocol: Some("ssh".to_owned()),
        identity_file: Some("synthetic-key".to_owned()),
        persist: Some("off".to_owned()),
        source: ConnectionHostSource::Odytty,
    }
}

fn hosts_path(dir: &TempDir) -> PathBuf {
    dir.path().join("hosts.conf")
}

fn assert_control_error(result: io::Result<impl std::fmt::Debug>) {
    let error = result.expect_err("control character must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

fn set_field(host: &mut ConnectionHost, field: usize, value: &str) {
    match field {
        0 => host.alias = value.to_owned(),
        1 => host.host_name = Some(value.to_owned()),
        2 => host.user = Some(value.to_owned()),
        3 => host.theme = Some(value.to_owned()),
        4 => host.font = Some(value.to_owned()),
        5 => host.title = Some(value.to_owned()),
        6 => host.protocol = Some(value.to_owned()),
        7 => host.identity_file = Some(value.to_owned()),
        8 => host.persist = Some(value.to_owned()),
        _ => panic!("unknown test field"),
    }
}

#[test]
fn hosts_append_rejects_controls_in_every_serialized_field_without_mutating() {
    let dir = TempDir::new("append-controls");
    let path = hosts_path(&dir);
    let original = b"# synthetic preamble\nHost preserved\n    User retained\n";
    fs::write(&path, original).expect("seed hosts file");

    for control in [
        "carriage\rreturn",
        "line\nfeed",
        "pair\r\nline",
        "tab\tfield",
        "nul\0field",
    ] {
        for field in 0..=8 {
            let mut candidate = host("new-node");
            set_field(&mut candidate, field, control);
            assert_control_error(append_adhoc_host(&path, &candidate));
            assert_eq!(fs::read(&path).expect("read hosts file"), original);
        }
    }
}

#[test]
fn hosts_edit_rejects_controls_in_target_or_sibling_field_without_mutating() {
    let dir = TempDir::new("edit-controls");
    let path = hosts_path(&dir);
    let original = b"# synthetic preamble\nHost target sibling\n    HostName node.example.invalid\n    User retained\n\nHost untouched\n    User preserved\n";
    fs::write(&path, original).expect("seed hosts file");

    for control in [
        "carriage\rreturn",
        "line\nfeed",
        "pair\r\nline",
        "tab\tfield",
        "nul\0field",
    ] {
        let updated = host("target");
        assert_control_error(edit_host_block(&path, control, &updated));
        assert_eq!(fs::read(&path).expect("read hosts file"), original);

        for field in 0..=8 {
            let mut invalid = updated.clone();
            set_field(&mut invalid, field, control);
            assert_control_error(edit_host_block(&path, "target", &invalid));
            assert_eq!(fs::read(&path).expect("read hosts file"), original);
        }
    }
}

#[test]
fn hosts_remove_rejects_controls_without_mutating() {
    let dir = TempDir::new("remove-controls");
    let path = hosts_path(&dir);
    let original = b"Host target\n    User retained\n\nHost sibling\n    User preserved\n";
    fs::write(&path, original).expect("seed hosts file");

    for control in [
        "carriage\rreturn",
        "line\nfeed",
        "pair\r\nline",
        "tab\tfield",
        "nul\0field",
    ] {
        assert_control_error(remove_host_block(&path, control));
        assert_eq!(fs::read(&path).expect("read hosts file"), original);
    }
}

#[test]
fn hosts_quote_is_escaped_and_sibling_fields_remain_parseable() {
    let dir = TempDir::new("quotes");
    let path = hosts_path(&dir);
    let mut quoted = host("quoted");
    quoted.user = Some("quoted \"operator\"".to_owned());
    quoted.title = Some("Sibling field".to_owned());

    assert_eq!(
        append_adhoc_host(&path, &quoted).expect("quoted field is valid"),
        AppendHostOutcome::Appended
    );
    let bytes = fs::read(&path).expect("read hosts file");
    let escaped_user = br#"User "quoted \"operator\"""#;
    assert!(
        bytes
            .windows(escaped_user.len())
            .any(|window| window == escaped_user)
    );
    let parsed = parse_odytty_hosts_bytes(&bytes);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].user.as_deref(), Some("quoted \"operator\""));
    assert_eq!(parsed[0].title.as_deref(), Some("Sibling field"));

    assert_eq!(
        edit_host_block(&path, "quoted", &quoted).expect("quoted edit is valid"),
        HostsEditOutcome::Written
    );
}

#[test]
fn remote_paste_targets_are_fixed_hex_and_remote_create_is_noclobber() {
    let target = remote_upload_target().expect("OS CSPRNG is available");
    let stem = target
        .strip_prefix("/tmp/odytty-paste-")
        .and_then(|value| value.strip_suffix(".png"))
        .expect("fixed upload target shape");
    assert_eq!(stem.len(), 32, "128-bit token is exactly 32 hex characters");
    assert!(stem.chars().all(|character| character.is_ascii_hexdigit()));

    let command = remote_upload_command("node.example.invalid", Some(2200), None, &target);
    let remote_command = command
        .args()
        .last()
        .expect("remote command argument")
        .to_string_lossy();
    assert!(remote_command.starts_with("umask 077; set -C; cat > '"));
    assert!(remote_command.ends_with(".png'"));
}

#[test]
fn remote_cleanup_ignores_paths_outside_the_generated_upload_shape() {
    let generated = "/tmp/odytty-paste-0123456789abcdef0123456789abcdef.png".to_owned();
    let command = remote_cleanup_command(
        "node.example.invalid",
        None,
        None,
        &[
            generated.clone(),
            "/tmp/not-odytty.png".to_owned(),
            "/tmp/odytty-paste-not-hex.png".to_owned(),
            "/tmp/odytty-paste-0123456789abcdef0123456789abcdef.png'; touch injected".to_owned(),
        ],
    )
    .expect("generated path remains eligible for cleanup");
    let remote_command = command
        .args()
        .last()
        .expect("remote command argument")
        .to_string_lossy();
    assert!(remote_command.contains(&generated));
    assert!(!remote_command.contains("not-odytty"));
    assert!(!remote_command.contains("not-hex"));
    assert!(!remote_command.contains("injected"));
}
