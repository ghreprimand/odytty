// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::connection_hosts::{ConnectionHost, ConnectionHostSource};

fn host(alias: &str) -> ConnectionHost {
    ConnectionHost {
        alias: alias.to_owned(),
        host_name: None,
        user: None,
        port: None,
        theme: None,
        font: None,
        title: None,
        integration: None,
        reuse: None,
        tmux: None,
        protocol: None,
        identity_file: None,
        persist: None,
        source: ConnectionHostSource::Odytty,
    }
}

fn argv(command: &SshCommand) -> Vec<String> {
    std::iter::once(command.program().to_string_lossy().into_owned())
        .chain(
            command
                .args()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        )
        .collect()
}

fn bootstrap_arg(command: &SshCommand) -> String {
    let outer = command
        .args()
        .last()
        .expect("bootstrap arg")
        .to_string_lossy()
        .into_owned();
    outer
        .strip_prefix("sh -c '")
        .and_then(|s| s.strip_suffix('\''))
        .expect("POSIX command wrapper")
        .replace("'\\''", "'")
}

fn remote_host(alias: &str, user: &str, host_name: &str) -> ConnectionHost {
    let mut entry = host(alias);
    entry.user = Some(user.to_owned());
    entry.host_name = Some(host_name.to_owned());
    entry
}

#[test]
fn base64_matches_rfc4648_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

#[test]
fn per_host_integration_overrides_global_default() {
    assert!(remote_integration_enabled(None, true));
    assert!(!remote_integration_enabled(None, false));
    assert!(!remote_integration_enabled(Some(false), true));
    assert!(remote_integration_enabled(Some(true), false));
}

#[test]
fn tab_title_prefers_user_at_host() {
    assert_eq!(
        ssh_tab_title(&remote_host("web1", "deploy", "web1.example.invalid")),
        "deploy@web1.example.invalid"
    );
    let mut no_user = host("web1");
    no_user.host_name = Some("web1.example.invalid".to_owned());
    assert_eq!(ssh_tab_title(&no_user), "web1.example.invalid");
    assert_eq!(ssh_tab_title(&host("bare")), "bare");
}

#[test]
fn integration_off_argv_is_byte_identical_to_plain_connect() {
    let mut entry = remote_host("web1", "deploy", "web1.example.invalid");
    entry.port = Some(2222);
    let plain = ssh_command_for_host(&entry).expect("plain");
    let off = ssh_command_for_host_with_integration(&entry, false).expect("off");
    assert_eq!(plain, off);
    // Requesting reuse can never resurrect control options on the
    // integration-off path: it stays byte-identical to a plain connect.
    let off_but_reuse = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: false,
            reuse: true,
            tmux: false,
            control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
            control_persist: None,
        },
    )
    .expect("off+reuse");
    assert_eq!(plain, off_but_reuse);
}

#[test]
fn per_host_reuse_overrides_global_default() {
    assert!(remote_reuse_enabled(None, true));
    assert!(!remote_reuse_enabled(None, false));
    assert!(!remote_reuse_enabled(Some(false), true));
    assert!(remote_reuse_enabled(Some(true), false));
}

#[test]
fn per_host_tmux_overrides_global_default() {
    assert!(remote_tmux_enabled(None, true));
    assert!(!remote_tmux_enabled(None, false));
    assert!(!remote_tmux_enabled(Some(false), true));
    assert!(remote_tmux_enabled(Some(true), false));
}

#[test]
fn identity_file_adds_dash_i_to_the_connect_argv() {
    // ODP-9 Tier 1: a set IdentityFile puts `-i <path>` in the plain connect
    // argv, as a separate argv element before the destination operand.
    let mut h = host("k");
    h.identity_file = Some("/fixture/keys/id_ed25519.example".to_owned());
    let args = argv(&ssh_command_for_host(&h).expect("cmd"));
    let i = args.iter().position(|a| a == "-i").expect("-i present");
    assert_eq!(args[i + 1], "/fixture/keys/id_ed25519.example");
    // `-i` and its path come before the `--` destination separator.
    let sep = args.iter().position(|a| a == "--").expect("--");
    assert!(i + 1 < sep, "identity args precede the destination operand");
}

#[test]
fn no_identity_file_emits_no_dash_i() {
    // Byte-identical to before when no IdentityFile is set.
    let args = argv(&ssh_command_for_host(&host("k")).expect("cmd"));
    assert!(
        !args.iter().any(|a| a == "-i"),
        "no -i without IdentityFile"
    );
}

#[test]
fn probe_argv_is_batchmode_noninteractive_and_argv_only() {
    // The probe is a no-shell one-shot `exit` with BatchMode + a
    // bounded ConnectTimeout, argv-only, guarded by `--`.
    let mut h = remote_host("k", "deploy", "k.example.invalid");
    h.port = Some(2222);
    h.identity_file = Some("/fixture/keys/id_ed25519.example".to_owned());
    let args = argv(&ssh_probe_command_for_host(&h).expect("probe"));
    assert!(args.iter().any(|a| a == "BatchMode=yes"));
    assert!(args.iter().any(|a| a == "ConnectTimeout=5"));
    // Port, identity, and destination are all present, destination after --.
    let p = args.iter().position(|a| a == "-p").expect("-p");
    assert_eq!(args[p + 1], "2222");
    let i = args.iter().position(|a| a == "-i").expect("-i");
    assert_eq!(args[i + 1], "/fixture/keys/id_ed25519.example");
    let sep = args.iter().position(|a| a == "--").expect("--");
    assert_eq!(args[sep + 1], "deploy@k.example.invalid");
    // The remote command is a bare `exit`; no `-t`, no ControlPath.
    assert_eq!(args.last().map(String::as_str), Some("exit"));
    assert!(!args.iter().any(|a| a == "-t"));
    assert!(!args.iter().any(|a| a.starts_with("ControlPath")));
}

#[test]
fn classify_probe_maps_authentication_and_host_key_outcomes() {
    // Exit 0 = key/agent auth OK.
    assert_eq!(classify_probe(true, ""), ProbeClass::AuthOk);
    // A host-key change is a distinct, security-relevant failure.
    assert_eq!(
        classify_probe(
            false,
            "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@"
        ),
        ProbeClass::HostKeyMismatch
    );
    assert_eq!(
        classify_probe(false, "Host key verification failed."),
        ProbeClass::HostKeyUntrusted
    );
    // Reachable but interactive auth (the expected password-host state).
    assert_eq!(
        classify_probe(
            false,
            "debug1: Authentications that can continue: publickey,password"
        ),
        ProbeClass::InteractiveAuth
    );
    assert_eq!(
        classify_probe(
            false,
            "deploy@host: Permission denied (publickey,password)."
        ),
        ProbeClass::InteractiveAuth
    );
    // Unreachable classes.
    assert_eq!(
        classify_probe(false, "ssh: connect to host x port 22: Connection refused"),
        ProbeClass::Unreachable
    );
    assert_eq!(
        classify_probe(
            false,
            "ssh: Could not resolve hostname x: Name or service not known"
        ),
        ProbeClass::Unreachable
    );
}

#[test]
fn identity_file_adds_dash_i_to_the_integration_argv() {
    // The integration (bootstrap) argv also carries `-i <path>` when set.
    let mut h = remote_host("k", "deploy", "k.example.invalid");
    h.identity_file = Some("/fixture/keys/id_ed25519.example".to_owned());
    let command = ssh_command_for_host_with_integration(&h, true).expect("cmd");
    let args = argv(&command);
    let i = args.iter().position(|a| a == "-i").expect("-i present");
    assert_eq!(args[i + 1], "/fixture/keys/id_ed25519.example");
    let sep = args.iter().position(|a| a == "--").expect("--");
    assert!(
        i + 1 < sep,
        "identity precedes destination in the bootstrap argv"
    );
}

#[test]
fn remote_upload_target_fails_closed_when_entropy_is_unavailable() {
    let result = remote_upload_target_with(|_| Err(getrandom::Error::UNSUPPORTED));
    assert!(result.is_err(), "no path is minted without OS entropy");
}

#[test]
fn remote_cleanup_only_names_minted_paths() {
    let minted = remote_upload_target().expect("OS entropy");
    assert!(is_minted_remote_upload_path(&minted));
    let paths = vec![
        "/tmp/not-odytty.png".to_owned(),
        "/tmp/odytty-paste-ABCDEF.png".to_owned(),
        "/etc/passwd".to_owned(),
        minted.clone(),
    ];
    let command =
        remote_cleanup_command("host.example.invalid", None, None, &paths).expect("cleanup");
    let joined = argv(&command).join(" ");
    assert!(joined.ends_with(&format!("rm -f '{minted}'")), "{joined}");
    assert!(!joined.contains("not-odytty") && !joined.contains("passwd"));
    assert!(
        remote_cleanup_command(
            "host.example.invalid",
            None,
            None,
            &["/tmp/not-odytty.png".to_owned()]
        )
        .is_none(),
        "no minted path means no remote command at all"
    );
}

#[test]
fn remote_upload_target_is_random_and_well_formed() {
    let a = remote_upload_target().expect("OS entropy");
    let b = remote_upload_target().expect("OS entropy");
    assert!(a.starts_with("/tmp/odytty-paste-"));
    assert!(a.ends_with(".png"));
    // Unguessability requires two mints to differ.
    assert_ne!(a, b);
    // Only hex in the random stem, so the whole path is safe to single-quote
    // into a remote shell command with no escaping.
    let stem = a
        .trim_start_matches("/tmp/odytty-paste-")
        .trim_end_matches(".png");
    assert!(!stem.is_empty());
    assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
}

#[cfg(not(windows))]
#[test]
fn remote_upload_command_streams_over_ssh_reusing_the_master() {
    let dir = std::path::Path::new("/run/user/1000/odytty/ssh");
    let command = remote_upload_command(
        "deploy@web1.example.invalid",
        Some(2222),
        Some(dir),
        "/tmp/odytty-paste-abc.png",
    );
    assert_eq!(command.program(), &OsString::from("ssh"));
    let args = argv(&command);
    // Multiplex over the live master, port before the destination operand,
    // destination after `--`.
    assert!(
        args.iter()
            .any(|a| a == "ControlPath=/run/user/1000/odytty/ssh/ssh-%C")
    );
    let p = args.iter().position(|a| a == "-p").expect("-p");
    assert_eq!(args[p + 1], "2222");
    let sep = args.iter().position(|a| a == "--").expect("--");
    let dest = args
        .iter()
        .position(|a| a == "deploy@web1.example.invalid")
        .expect("dest");
    assert!(sep < dest);
    // The remote command creates the file 0600 and streams stdin into it.
    assert_eq!(
        args.last().map(String::as_str),
        Some(remote_posix_command("umask 077; set -C; cat > '/tmp/odytty-paste-abc.png'").as_str())
    );
    // A one-shot, not an interactive shell: no PTY-forcing `-t`.
    assert!(!args.iter().any(|a| a == "-t"));
    // A fresh connection gives up on an unreachable host.
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "-o" && pair[1] == "ConnectTimeout=10")
    );
}

#[test]
fn remote_upload_command_without_a_control_dir_emits_no_control_option() {
    let command = remote_upload_command(
        "host.example.invalid",
        None,
        None,
        "/tmp/odytty-paste-x.png",
    );
    let joined = argv(&command).join(" ");
    assert!(!joined.contains("ControlPath"));
    assert!(bootstrap_arg(&command).contains("umask 077; set -C; cat > '/tmp/odytty-paste-x.png'"));
    assert!(joined.starts_with("ssh -o ConnectTimeout=10 -- host.example.invalid"));
}

#[test]
fn remote_cleanup_command_is_none_when_nothing_was_uploaded() {
    assert!(remote_cleanup_command("host.example.invalid", None, None, &[]).is_none());
}

#[test]
fn remote_cleanup_command_removes_each_uploaded_path() {
    let a = format!("/tmp/odytty-paste-{}.png", "a".repeat(32));
    let b = format!("/tmp/odytty-paste-{}.png", "b".repeat(32));
    let paths = vec![a.clone(), b.clone()];
    let command = remote_cleanup_command("host.example.invalid", None, None, &paths).expect("cmd");
    assert_eq!(
        argv(&command).last().map(String::as_str),
        Some(format!("rm -f '{a}' '{b}'").as_str())
    );
}

#[cfg(windows)]
#[test]
fn windows_remote_upload_command_emits_no_control_path() {
    let command = remote_upload_command(
        "deploy@web1.example.invalid",
        None,
        Some(std::path::Path::new("C:/odytty/ssh")),
        "/tmp/odytty-paste-abc.png",
    );
    assert_eq!(command.program(), &OsString::from("ssh"));
    let joined = argv(&command).join(" ");
    assert!(!joined.contains("ControlPath"));
    assert!(joined.contains("umask 077; set -C; cat"));
}

fn tmux_bootstrap(entry: &ConnectionHost, tmux: bool) -> String {
    let command = ssh_command_for_host_with_options(
        entry,
        &RemoteSshOptions {
            integration: true,
            reuse: false,
            tmux,
            control_dir: None,
            control_persist: None,
        },
    )
    .expect("argv");
    bootstrap_arg(&command)
}

#[test]
fn tmux_off_bootstrap_execs_bash_without_any_tmux() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let bootstrap = tmux_bootstrap(&entry, false);
    assert!(bootstrap.contains("exec bash --rcfile"));
    assert!(!bootstrap.contains("tmux"));
}

#[test]
fn tmux_on_bootstrap_nests_bash_inside_a_persistent_session_and_degrades() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let bootstrap = tmux_bootstrap(&entry, true);
    // Create-or-attach a persistent `odytty` session running the integrated
    // bash, so a reconnect reattaches the same remote state.
    assert!(bootstrap.contains("exec tmux new-session -A -s odytty bash --rcfile"));
    // Degrade path: a runtime `command -v tmux` guard falls back to plain
    // bash when the remote has no tmux, so opting in never breaks a session.
    assert!(bootstrap.contains("command -v tmux"));
    assert!(bootstrap.contains("else exec bash --rcfile"));
    // The unconditional plain-shell fallback (missing bash/base64) is intact.
    assert!(
        bootstrap
            .trim_end()
            .ends_with("exec \"${SHELL:-/bin/sh}\" -l")
    );
}

#[test]
fn integration_off_argv_ignores_tmux_and_stays_byte_identical() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let plain = ssh_command_for_host(&entry).expect("plain");
    // tmux is a bootstrap-only wrap, so with integration off (no bootstrap)
    // it can never alter the argv: it stays byte-identical to a plain ssh.
    let off_but_tmux = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: false,
            reuse: false,
            tmux: true,
            control_dir: None,
            control_persist: None,
        },
    )
    .expect("off+tmux");
    assert_eq!(plain, off_but_tmux);
}

#[cfg(not(windows))]
#[test]
fn reuse_adds_control_options_after_pty_and_before_destination() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let command = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: true,
            reuse: true,
            tmux: false,
            control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
            control_persist: None,
        },
    )
    .expect("argv");
    let args = argv(&command);
    let t_pos = args.iter().position(|a| a == "-t").expect("-t");
    let cm = args
        .iter()
        .position(|a| a == "ControlMaster=auto")
        .expect("ControlMaster");
    let sep = args.iter().position(|a| a == "--").expect("--");
    // Control options sit after `-t` and before the destination separator.
    assert!(t_pos < cm && cm < sep);
    assert!(args.iter().any(|a| a == "ControlPersist=600"));
    assert!(
        args.iter()
            .any(|a| a == "ControlPath=/run/user/1000/odytty/ssh/ssh-%C")
    );
    // Exactly three `-o` flags for the three control options.
    assert_eq!(args.iter().filter(|a| a.as_str() == "-o").count(), 3);
}

#[cfg(not(windows))]
#[test]
fn reuse_without_control_dir_emits_no_control_options() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let command = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: true,
            reuse: true,
            tmux: false,
            control_dir: None,
            control_persist: None,
        },
    )
    .expect("argv");
    let joined = argv(&command).join(" ");
    assert!(!joined.contains("ControlMaster"));
    assert!(joined.contains("-t"));
}

#[cfg(not(windows))]
#[test]
fn reuse_off_emits_no_control_options() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let command = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: true,
            reuse: false,
            tmux: false,
            control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
            control_persist: None,
        },
    )
    .expect("argv");
    assert!(!argv(&command).join(" ").contains("ControlMaster"));
}

// ODP-9 Tier 2: the `remote_persist` knob replaces the historically-fixed
// ControlPersist window. The default resolves to `None` here, which MUST keep
// emitting the historical `ControlPersist=600` so the argv is byte-identical
// to before the knob (the non-regression gate the ruling called out).
#[cfg(not(windows))]
#[test]
fn default_persist_is_byte_identical_to_the_fixed_600s_window() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let with_default = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: true,
            reuse: true,
            tmux: false,
            control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
            control_persist: None,
        },
    )
    .expect("argv");
    // Resolving the default `10m` preset to its token is the same string.
    let with_ten_min = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: true,
            reuse: true,
            tmux: false,
            control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
            control_persist: Some("600".to_owned()),
        },
    )
    .expect("argv");
    assert!(
        argv(&with_default)
            .iter()
            .any(|a| a == "ControlPersist=600")
    );
    assert_eq!(argv(&with_default), argv(&with_ten_min));
}

#[cfg(not(windows))]
#[test]
fn explicit_persist_token_replaces_the_window_exactly_once() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    for (token, expected) in [("no", "ControlPersist=no"), ("7200", "ControlPersist=7200")] {
        let command = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: true,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
                control_persist: Some(token.to_owned()),
            },
        )
        .expect("argv");
        let args = argv(&command);
        assert!(args.iter().any(|a| a == expected), "{expected} present");
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("ControlPersist="))
                .count(),
            1,
            "exactly one ControlPersist option"
        );
    }
}

#[test]
fn parse_control_persist_maps_off_tokens_and_durations() {
    // Off tokens tear the master down with its last connection.
    for off in ["off", "0", "no", "none", "false", "  OFF  "] {
        assert_eq!(parse_control_persist(off).as_deref(), Some("no"), "{off}");
    }
    // Presets and raw durations normalize to whole seconds.
    assert_eq!(parse_control_persist("10m").as_deref(), Some("600"));
    assert_eq!(parse_control_persist("30m").as_deref(), Some("1800"));
    assert_eq!(parse_control_persist("2h").as_deref(), Some("7200"));
    assert_eq!(parse_control_persist("1h30m").as_deref(), Some("5400"));
    assert_eq!(parse_control_persist("90s").as_deref(), Some("90"));
    assert_eq!(parse_control_persist("600").as_deref(), Some("600"));
    // A duration that totals zero is off, never `0` (OpenSSH: persist forever).
    for zero in ["0s", "00", "0m", "0h", "0h0m0s", "000s"] {
        assert_eq!(parse_control_persist(zero).as_deref(), Some("no"), "{zero}");
    }
    // A window past the 30-day ceiling is clamped; the ceiling itself stays.
    assert_eq!(
        parse_control_persist("99999999999999").as_deref(),
        Some("2592000")
    );
    assert_eq!(parse_control_persist("721h").as_deref(), Some("2592000"));
    assert_eq!(parse_control_persist("720h").as_deref(), Some("2592000"));
    assert_eq!(parse_control_persist("719h").as_deref(), Some("2588400"));
    // Garbage yields None so the caller can fall back to the global default.
    assert_eq!(parse_control_persist("soon"), None);
    assert_eq!(parse_control_persist("1h30"), None);
    assert_eq!(parse_control_persist(""), None);
}

// Windows client: OpenSSH has no ControlMaster, so the reuse control options
// (including ControlPersist) are compiled out and the knob is inert. This test
// pins that a Windows argv carries NO ControlPersist regardless of the token.
#[cfg(windows)]
#[test]
fn windows_never_emits_control_persist() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let command = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: true,
            reuse: true,
            tmux: false,
            control_dir: Some(std::path::PathBuf::from("C:/odytty/ssh")),
            control_persist: Some("7200".to_owned()),
        },
    )
    .expect("argv");
    let joined = argv(&command).join(" ");
    assert!(!joined.contains("ControlPersist"));
    assert!(!joined.contains("ControlMaster"));
}

#[cfg(not(windows))]
#[test]
fn control_socket_path_uses_openssh_connection_hash_within_sun_path_limit() {
    // `%C` expands to OpenSSH's 40-hex-character hash over `%l%h%p%r`, so
    // the same destination on ports 22 and 2222 cannot share a socket. The
    // expansion is fixed-width: only the control-dir length varies.
    let dir = std::path::Path::new("/run/user/1000/odytty/ssh");
    let socket = control_socket_path(dir);
    assert_eq!(socket, dir.join("ssh-%C"));
    let expanded_len = dir.as_os_str().len() + 1 + "ssh-".len() + 40;
    assert!(
        expanded_len < 104,
        "expanded socket path fits the shortest sun_path"
    );
}

#[cfg(unix)]
#[test]
fn ensure_control_dir_creates_owner_only_directory() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!(
        "odytty-ctrl-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    let dir = base.join("ssh");
    ensure_control_dir(&dir).expect("create control dir");
    let mode = std::fs::metadata(&dir)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    // Idempotent: a second call tightens rather than fails.
    ensure_control_dir(&dir).expect("second call");
    std::fs::remove_dir_all(&base).ok();
}

/// A unique scratch base under the OS temp dir for control-dir tests.
#[cfg(unix)]
fn control_dir_scratch(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "odytty-ctrl-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ))
}

#[cfg(unix)]
#[test]
fn ensure_control_dir_rejects_symlink_leaf_without_touching_target() {
    use std::os::unix::fs::PermissionsExt;
    let base = control_dir_scratch("symlink");
    let target = base.join("target");
    std::fs::create_dir_all(&target).expect("create target");
    std::fs::write(target.join("keep.txt"), b"untouched").expect("seed target");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
        .expect("broaden target");
    let dir = base.join("ssh");
    std::os::unix::fs::symlink(&target, &dir).expect("plant symlink leaf");

    let error = ensure_control_dir(&dir).expect_err("symlink leaf must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    // The symlink target keeps its permissions and content: no follow-chmod.
    let mode = std::fs::metadata(&target)
        .expect("target metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(
        std::fs::read(target.join("keep.txt")).expect("target content"),
        b"untouched"
    );
    // The planted link itself is left in place, not replaced or deleted.
    assert!(
        std::fs::symlink_metadata(&dir)
            .expect("leaf metadata")
            .file_type()
            .is_symlink()
    );
    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn ensure_control_dir_rejects_dangling_symlink_leaf() {
    let base = control_dir_scratch("dangling");
    std::fs::create_dir_all(&base).expect("create base");
    let dir = base.join("ssh");
    std::os::unix::fs::symlink(base.join("missing"), &dir).expect("plant dangling link");

    let error = ensure_control_dir(&dir).expect_err("dangling symlink must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        std::fs::symlink_metadata(&dir)
            .expect("leaf metadata")
            .file_type()
            .is_symlink()
    );
    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn ensure_control_dir_rejects_regular_file_leaf_without_touching_it() {
    let base = control_dir_scratch("file");
    std::fs::create_dir_all(&base).expect("create base");
    let dir = base.join("ssh");
    std::fs::write(&dir, b"not a directory").expect("plant file leaf");

    let error = ensure_control_dir(&dir).expect_err("file leaf must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        std::fs::read(&dir).expect("leaf content"),
        b"not a directory"
    );
    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn ensure_control_dir_repairs_broad_mode_on_same_owner_directory() {
    use std::os::unix::fs::PermissionsExt;
    let base = control_dir_scratch("repair");
    let dir = base.join("ssh");
    std::fs::create_dir_all(&dir).expect("create dir");
    std::fs::write(dir.join("ssh-deadbeef"), b"socket placeholder").expect("seed dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("broaden dir");

    ensure_control_dir(&dir).expect("same-owner repair succeeds");
    let mode = std::fs::metadata(&dir)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    // Repair only tightens the mode; existing contents are preserved.
    assert_eq!(
        std::fs::read(dir.join("ssh-deadbeef")).expect("content"),
        b"socket placeholder"
    );
    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn control_leaf_rejection_fails_closed_on_symlink_type_and_owner() {
    // Symlink beats everything else.
    assert_eq!(
        control_leaf_rejection(true, true, 1000, 1000),
        Some("path is a symlink")
    );
    // A non-directory object is rejected.
    assert_eq!(
        control_leaf_rejection(false, false, 1000, 1000),
        Some("path is not a directory")
    );
    // A directory owned by another user is rejected (root-squashed or
    // pre-planted foreign dirs never get sockets or a chmod).
    assert_eq!(
        control_leaf_rejection(false, true, 0, 1000),
        Some("directory is owned by another user")
    );
    // The happy path: real same-owner directory passes.
    assert_eq!(control_leaf_rejection(false, true, 1000, 1000), None);
}

#[cfg(windows)]
#[test]
fn windows_reuse_never_emits_control_options() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let command = ssh_command_for_host_with_options(
        &entry,
        &RemoteSshOptions {
            integration: true,
            reuse: true,
            tmux: false,
            control_dir: Some(std::path::PathBuf::from("C:/odytty/ssh")),
            control_persist: None,
        },
    )
    .expect("argv");
    let joined = argv(&command).join(" ");
    assert_eq!(command.program(), &OsString::from("ssh"));
    assert!(joined.contains("-t"));
    assert!(!joined.contains("ControlMaster"));
    assert!(!joined.contains("ControlPath"));
    assert!(!joined.contains("ControlPersist"));
}

#[test]
fn integration_argv_forces_pty_and_injects_bootstrap() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let command = ssh_command_for_host_with_integration(&entry, true).expect("argv");
    let args = argv(&command);
    assert_eq!(args[0], "ssh");
    let t_pos = args.iter().position(|a| a == "-t").expect("-t");
    let sep = args.iter().position(|a| a == "--").expect("--");
    let dest = args
        .iter()
        .position(|a| a == "deploy@web1.example.invalid")
        .expect("dest");
    // `-t` forces the PTY, and the destination operand follows `--`.
    assert!(t_pos < sep && sep < dest);
    let bootstrap = bootstrap_arg(&command);
    assert!(bootstrap.starts_with("ODYTTY_RC='"));
    assert_eq!(bootstrap.matches("ODYTTY_RC=").count(), 1);
}

#[test]
fn integration_argv_passes_port_before_destination() {
    let mut entry = remote_host("web1", "deploy", "web1.example.invalid");
    entry.port = Some(2222);
    let args = argv(&ssh_command_for_host_with_integration(&entry, true).expect("argv"));
    let p = args.iter().position(|a| a == "-p").expect("-p");
    assert_eq!(args[p + 1], "2222");
    let sep = args.iter().position(|a| a == "--").expect("--");
    assert!(p < sep);
}

#[test]
fn bootstrap_blob_encodes_shared_integration_payload() {
    let command = ssh_command_for_host_with_integration(&host("web1"), true).expect("argv");
    let bootstrap = bootstrap_arg(&command);
    let rc = remote_bash_rc();
    // The remote prefix loads login files before the shared interactive payload.
    assert!(rc.contains(&crate::shell_integration::bash_integration_rc()));
    assert!(rc.starts_with("rm -f \"${BASH_SOURCE[0]}\""));
    let blob = base64_encode(rc.as_bytes());
    assert!(bootstrap.contains(&format!("ODYTTY_RC='{blob}'")));
}

#[test]
fn bootstrap_is_host_independent_and_leaks_no_identifiers() {
    let a = ssh_command_for_host_with_integration(
        &remote_host("a", "alice", "a.example.invalid"),
        true,
    )
    .expect("a");
    let b =
        ssh_command_for_host_with_integration(&remote_host("b", "bob", "b.example.invalid"), true)
            .expect("b");
    // No host/user data reaches the remote command; it rides in ssh operands.
    assert_eq!(bootstrap_arg(&a), bootstrap_arg(&b));
    let bootstrap = bootstrap_arg(&a);
    assert!(!bootstrap.contains("alice"));
    assert!(!bootstrap.contains("a.example.invalid"));
}

#[test]
fn bootstrap_always_terminates_in_an_exec() {
    let bootstrap =
        bootstrap_arg(&ssh_command_for_host_with_integration(&host("web1"), true).expect("a"));
    assert!(
        bootstrap
            .trim_end()
            .ends_with("exec \"${SHELL:-/bin/sh}\" -l")
    );
    // Reuse options belong to the local SSH argv rather than the remote script.
    assert!(!bootstrap.contains("ControlMaster"));
}

#[cfg(windows)]
#[test]
fn windows_integration_argv_uses_ssh_without_controlmaster() {
    let entry = remote_host("web1", "deploy", "web1.example.invalid");
    let command = ssh_command_for_host_with_integration(&entry, true).expect("argv");
    assert_eq!(command.program(), &OsString::from("ssh"));
    let joined = argv(&command).join(" ");
    assert!(joined.contains("-t"));
    assert!(!joined.contains("ControlMaster"));
    assert!(!joined.contains("ControlPath"));
}

#[test]
fn builds_host_only_ssh_argv() {
    let command = ssh_command_for_host(&host("web1")).expect("argv");
    assert_eq!(argv(&command), vec!["ssh", "--", "web1"]);
}

#[test]
fn builds_user_host_ssh_argv_from_resolved_host_name() {
    let mut entry = host("web1");
    entry.host_name = Some("web1.example.invalid".to_owned());
    entry.user = Some("deploy".to_owned());

    let command = ssh_command_for_host(&entry).expect("argv");
    assert_eq!(
        argv(&command),
        vec!["ssh", "--", "deploy@web1.example.invalid"]
    );
}

#[test]
fn builds_user_port_host_ssh_argv() {
    let mut entry = host("web1");
    entry.host_name = Some("web1.example.invalid".to_owned());
    entry.user = Some("deploy".to_owned());
    entry.port = Some(2222);

    let command = ssh_command_for_host(&entry).expect("argv");
    assert_eq!(
        argv(&command),
        vec!["ssh", "-p", "2222", "--", "deploy@web1.example.invalid"]
    );
}

#[test]
fn no_profile_or_credential_material_enters_argv() {
    let mut entry = host("web1");
    entry.host_name = Some("web1.example.invalid".to_owned());
    entry.user = Some("deploy".to_owned());
    entry.port = Some(2222);
    entry.theme = Some("odyssey".to_owned());
    entry.font = Some("Victor Mono".to_owned());
    entry.title = Some("synthetic-password-marker".to_owned());

    let command = ssh_command_for_host(&entry).expect("argv");
    let joined = argv(&command).join(" ");
    assert!(!joined.contains("odyssey"));
    assert!(!joined.contains("Victor"));
    assert!(!joined.contains("synthetic-password-marker"));
}

#[test]
fn rejects_credential_like_destination_fields() {
    let mut entry = host("web1");
    entry.host_name = Some("deploy:synthetic-secret@example.invalid".to_owned());
    assert_eq!(
        ssh_command_for_host(&entry).expect_err("credential-like target rejected"),
        SshConnectError::InvalidField("host")
    );

    let mut entry = host("web1");
    entry.user = Some("deploy@example".to_owned());
    assert_eq!(
        ssh_command_for_host(&entry).expect_err("ambiguous user rejected"),
        SshConnectError::InvalidField("user")
    );
}

#[cfg(unix)]
#[test]
fn ssh_session_is_resumable_host_exec_command() {
    let mut entry = host("web1");
    entry.host_name = Some("web1.example.invalid".to_owned());
    entry.user = Some("deploy".to_owned());
    entry.port = Some(2222);

    let config = detached_ssh_host_config("ssh-web1", &entry, None, Dimensions::new(100, 30))
        .expect("host config");
    assert_eq!(config.session_id, "ssh-web1");
    assert_eq!(config.dimensions, Dimensions::new(100, 30));
    match config.command {
        HostCommand::Exec {
            program,
            args,
            working_directory,
        } => {
            assert_eq!(program, OsString::from("ssh"));
            assert_eq!(
                args,
                vec![
                    OsString::from("-p"),
                    OsString::from("2222"),
                    OsString::from("--"),
                    OsString::from("deploy@web1.example.invalid")
                ]
            );
            assert_eq!(working_directory, None);
        }
        other => panic!("expected ssh exec host command, got {other:?}"),
    }
}

#[test]
fn imported_alias_preserves_openssh_routing_for_connect_and_probe() {
    let mut h = remote_host("route", "account", "endpoint.example.invalid");
    h.source = ConnectionHostSource::SshConfig;
    h.port = Some(2222);
    assert_eq!(ssh_destination(&h).unwrap(), "route");
    for command in [
        ssh_command_for_host(&h).unwrap(),
        ssh_probe_command_for_host(&h).unwrap(),
        ssh_command_for_host_with_integration(&h, true).unwrap(),
    ] {
        let args = argv(&command);
        let sep = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[sep + 1], "route");
        assert!(!args[..sep].iter().any(|arg| arg == "-p"));
    }
}

#[test]
fn publickey_only_probe_is_not_interactive_auth() {
    assert_ne!(
        classify_probe(
            false,
            "debug1: Authentications that can continue: publickey\nPermission denied (publickey)."
        ),
        ProbeClass::InteractiveAuth
    );
    assert_ne!(
        classify_probe(
            false,
            "debug1: identity file /synthetic/password-key type -1\nConnection refused"
        ),
        ProbeClass::InteractiveAuth
    );
}

#[test]
fn untrusted_host_key_is_not_a_changed_key() {
    assert_ne!(
        classify_probe(false, "Host key verification failed."),
        ProbeClass::HostKeyMismatch
    );
}

#[test]
fn remote_posix_wrapper_preserves_shell_metacharacters_as_data() {
    let script = "printf '%s' \"$HOME\"; : \\ 'quoted'";
    let outer = remote_posix_command(script);
    assert!(outer.starts_with("sh -c '"));
    let restored = outer
        .strip_prefix("sh -c '")
        .unwrap()
        .strip_suffix('\'')
        .unwrap()
        .replace("'\\''", "'");
    assert_eq!(restored, script);
}

#[test]
fn probe_method_lists_and_changed_keys_have_distinct_outcomes() {
    for text in [
        "Permission denied (publickey).",
        "debug1: Authentications that can continue: publickey,password\ndebug1: Authentications that can continue: publickey",
        "debug1: Authentications that can continue: password\nPermission denied (publickey).",
    ] {
        assert_eq!(classify_probe(false, text), ProbeClass::AuthRejected);
    }
    assert_eq!(
        classify_probe(false, "Host key verification failed."),
        ProbeClass::HostKeyUntrusted
    );
    assert_eq!(
        classify_probe(
            false,
            "REMOTE HOST IDENTIFICATION HAS CHANGED!\nHost key verification failed."
        ),
        ProbeClass::HostKeyMismatch
    );
    assert_eq!(
        classify_probe(
            false,
            "debug1: Authentications that can continue: publickey,keyboard-interactive"
        ),
        ProbeClass::InteractiveAuth
    );
}

#[test]
fn imported_alias_delegates_identity_and_rejects_unsafe_aliases() {
    let mut h = host("route");
    h.source = ConnectionHostSource::SshConfig;
    h.identity_file = Some("/fixture/keys/ignored".to_owned());
    h.port = Some(2222);
    h.host_name = Some("display-only.example.invalid".to_owned());
    assert_eq!(
        argv(&ssh_command_for_host(&h).unwrap()),
        ["ssh", "--", "route"]
    );
    h.alias = "bad alias".to_owned();
    assert!(ssh_command_for_host(&h).is_err());
}

/// The socket directory must leave room for the name the master binds first:
/// `<dir>/ssh-<40 hex>` plus OpenSSH's `.<16 characters>` suffix.
#[cfg(unix)]
#[test]
fn a_control_dir_leaves_room_for_the_masters_temporary_socket_name() {
    use std::path::Path;
    let limit = 103;
    let room = limit - (1 + 4 + 40 + 1 + 16);
    let at_limit = "a".repeat(room);
    assert!(control_dir_fits_socket_limit(Path::new(&at_limit), limit));
    assert!(!control_dir_fits_socket_limit(
        Path::new(&format!("{at_limit}a")),
        limit
    ));
    // The macOS state directory is `/Users/<name>/Library/Logs/odytty/ssh`:
    // ten characters of user name fit, eleven do not.
    let dir = |name: &str| format!("/Users/{name}/Library/Logs/odytty/ssh");
    assert!(control_dir_fits_socket_limit(
        Path::new(&dir("abcdefghij")),
        103
    ));
    assert!(!control_dir_fits_socket_limit(
        Path::new(&dir("abcdefghijk")),
        103
    ));
    // Linux allows 107 bytes.
    assert!(control_dir_fits_socket_limit(
        Path::new(&dir("abcdefghijklmn")),
        107
    ));
}

/// The short fallback is a fixed, per-user, shallow directory.
#[cfg(unix)]
#[test]
fn the_short_control_dir_is_per_user_and_always_fits() {
    let dir = short_control_dir();
    assert!(dir.starts_with("/tmp"));
    assert!(control_dir_fits_socket_limit(&dir, 103), "{dir:?}");
    // A user id of any width still fits the strictest platform limit.
    assert!(control_dir_fits_socket_limit(
        std::path::Path::new("/tmp/odytty-4294967295"),
        103
    ));
}
