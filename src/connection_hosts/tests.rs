// SPDX-License-Identifier: GPL-3.0-only
//! Hosts-file reader, writer, and splice tests.

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn limits() -> ConnectionHostsLimits {
    ConnectionHostsLimits {
        max_bytes: 4096,
        max_entries: 16,
        max_field_chars: 128,
    }
}

fn host(alias: &str, source: ConnectionHostSource) -> ConnectionHost {
    ConnectionHost {
        alias: alias.to_owned(),
        host_name: Some(format!("{alias}.example.invalid")),
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
        source,
    }
}

fn temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path).expect("create synthetic temp dir");
    path
}

#[test]
fn parse_adhoc_target_accepts_all_destination_shapes() {
    assert_eq!(
        parse_adhoc_target("host.example.invalid"),
        Some(AdhocTarget {
            user: None,
            host: "host.example.invalid".to_owned(),
            port: None,
        })
    );
    assert_eq!(
        parse_adhoc_target("deploy@host.example.invalid"),
        Some(AdhocTarget {
            user: Some("deploy".to_owned()),
            host: "host.example.invalid".to_owned(),
            port: None,
        })
    );
    assert_eq!(
        parse_adhoc_target("host.example.invalid:2200"),
        Some(AdhocTarget {
            user: None,
            host: "host.example.invalid".to_owned(),
            port: Some(2200),
        })
    );
    assert_eq!(
        parse_adhoc_target("deploy@host.example.invalid:22"),
        Some(AdhocTarget {
            user: Some("deploy".to_owned()),
            host: "host.example.invalid".to_owned(),
            port: Some(22),
        })
    );
}

#[test]
fn parse_adhoc_target_rejects_unsafe_and_malformed_input() {
    // Empty / whitespace / embedded space.
    assert_eq!(parse_adhoc_target(""), None);
    assert_eq!(parse_adhoc_target("   "), None);
    assert_eq!(parse_adhoc_target("bad host"), None);
    // Option-injection leading dash (host and user parts).
    assert_eq!(parse_adhoc_target("-oProxyCommand=x"), None);
    assert_eq!(parse_adhoc_target("-host.example.invalid"), None);
    assert_eq!(parse_adhoc_target("-u@host.example.invalid"), None);
    // Missing host, empty user, double `@`.
    assert_eq!(parse_adhoc_target("user@"), None);
    assert_eq!(parse_adhoc_target("@host.example.invalid"), None);
    assert_eq!(parse_adhoc_target("a@b@host.example.invalid"), None);
    // Bad ports.
    assert_eq!(parse_adhoc_target("host.example.invalid:"), None);
    assert_eq!(parse_adhoc_target("host.example.invalid:0"), None);
    assert_eq!(parse_adhoc_target("host.example.invalid:70000"), None);
    assert_eq!(parse_adhoc_target("host.example.invalid:ssh"), None);
    assert_eq!(parse_adhoc_target("host.example.invalid:22:22"), None);
}

#[test]
fn adhoc_target_builds_a_defaults_only_connection_host() {
    let host = parse_adhoc_target("deploy@host.example.invalid:2200")
        .expect("valid target")
        .to_connection_host();
    assert_eq!(host.alias, "host.example.invalid");
    assert_eq!(
        host.host_name, None,
        "alias is the host, no separate HostName"
    );
    assert_eq!(host.user.as_deref(), Some("deploy"));
    assert_eq!(host.port, Some(2200));
    // Ad-hoc carries no per-host overrides - global defaults apply.
    assert_eq!(host.integration, None);
    assert_eq!(host.reuse, None);
    assert_eq!(host.tmux, None);
}

#[test]
fn append_adhoc_host_creates_file_and_writes_a_block() {
    let dir = temp_dir("odytty-adhoc-append");
    let path = dir.join("nested").join(CONNECTION_HOSTS_FILE_NAME);
    let host = parse_adhoc_target("deploy@host.example.invalid:2200")
        .expect("valid")
        .to_connection_host();

    let outcome = append_adhoc_host(&path, &host).expect("append");
    assert_eq!(outcome, AppendHostOutcome::Appended);

    let written = fs::read_to_string(&path).expect("read back");
    assert!(written.contains("Host host.example.invalid"));
    assert!(
        !written.contains("HostName"),
        "no redundant HostName for a bare host"
    );
    assert!(written.contains("    User deploy"));
    assert!(written.contains("    Port 2200"));

    // The written block round-trips through the parser.
    let reparsed = read_odytty_hosts_with_limits(&path, limits());
    assert_eq!(reparsed.len(), 1);
    assert_eq!(reparsed[0].alias, "host.example.invalid");
    assert_eq!(reparsed[0].user.as_deref(), Some("deploy"));
    assert_eq!(reparsed[0].port, Some(2200));

    fs::remove_dir_all(dir).ok();
}

#[test]
fn append_adhoc_host_preserves_existing_content_and_appends() {
    let dir = temp_dir("odytty-adhoc-preserve");
    let path = dir.join(CONNECTION_HOSTS_FILE_NAME);
    // Existing content with a comment and custom spacing to prove byte
    // preservation of everything before the appended block.
    let existing = "# my hosts\nHost existing\n    HostName existing.example.invalid\n";
    fs::write(&path, existing).expect("seed");

    let host = parse_adhoc_target("new.example.invalid")
        .expect("valid")
        .to_connection_host();
    let outcome = append_adhoc_host(&path, &host).expect("append");
    assert_eq!(outcome, AppendHostOutcome::Appended);

    let written = fs::read_to_string(&path).expect("read back");
    assert!(
        written.starts_with(existing),
        "existing bytes preserved verbatim"
    );
    assert!(written.contains("Host new.example.invalid"));
    // Both hosts parse.
    let reparsed = read_odytty_hosts_with_limits(&path, limits());
    assert_eq!(reparsed.len(), 2);

    fs::remove_dir_all(dir).ok();
}

#[test]
fn append_adhoc_host_skips_write_on_exact_alias_collision() {
    let dir = temp_dir("odytty-adhoc-collision");
    let path = dir.join(CONNECTION_HOSTS_FILE_NAME);
    let existing = "Host host.example.invalid\n    User someone\n";
    fs::write(&path, existing).expect("seed");

    let host = parse_adhoc_target("host.example.invalid")
        .expect("valid")
        .to_connection_host();
    let outcome = append_adhoc_host(&path, &host).expect("append");
    assert_eq!(outcome, AppendHostOutcome::AlreadyExists);

    // File untouched byte-for-byte.
    let written = fs::read_to_string(&path).expect("read back");
    assert_eq!(written, existing);

    fs::remove_dir_all(dir).ok();
}

#[test]
fn mutation_reader_accepts_exact_limit_and_rejects_limit_plus_one() {
    let dir = temp_dir("odytty-hosts-mutation-cap");
    let path = dir.join(CONNECTION_HOSTS_FILE_NAME);
    let mut bytes = vec![b'#'; DEFAULT_CONNECTION_HOSTS_MAX_BYTES as usize];
    fs::write(&path, &bytes).expect("seed exact-limit file");
    assert_eq!(
        read_hosts_for_mutation(&path)
            .expect("read exact-limit file")
            .len(),
        DEFAULT_CONNECTION_HOSTS_MAX_BYTES as usize
    );

    bytes.push(b'\n');
    fs::write(&path, &bytes).expect("seed over-limit file");
    let error = read_hosts_for_mutation(&path).expect_err("reject limit plus one");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn every_mutation_rejects_oversized_input_without_changing_it() {
    let dir = temp_dir("odytty-hosts-all-mutations-cap");
    let path = dir.join(CONNECTION_HOSTS_FILE_NAME);
    let mut oversized = b"Host existing.example.invalid\n    User original\n".to_vec();
    oversized.resize(DEFAULT_CONNECTION_HOSTS_MAX_BYTES as usize + 1, b'#');
    fs::write(&path, &oversized).expect("seed oversized file");

    let appended = host("new.example.invalid", ConnectionHostSource::Odytty);
    let append_error = append_adhoc_host(&path, &appended).expect_err("append must reject");
    assert_eq!(append_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).expect("read after append"), oversized);

    let mut updated = host("existing.example.invalid", ConnectionHostSource::Odytty);
    updated.user = Some("updated".to_owned());
    let edit_error =
        edit_host_block(&path, "existing.example.invalid", &updated).expect_err("edit must reject");
    assert_eq!(edit_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).expect("read after edit"), oversized);

    let remove_error =
        remove_host_block(&path, "existing.example.invalid").expect_err("remove must reject");
    assert_eq!(remove_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).expect("read after remove"), oversized);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn append_refuses_to_grow_an_accepted_file_past_the_limit() {
    let dir = temp_dir("odytty-hosts-output-cap");
    let path = dir.join(CONNECTION_HOSTS_FILE_NAME);
    let existing = vec![b'#'; DEFAULT_CONNECTION_HOSTS_MAX_BYTES as usize - 8];
    fs::write(&path, &existing).expect("seed near-limit file");

    let appended = host("new.example.invalid", ConnectionHostSource::Odytty);
    let error = append_adhoc_host(&path, &appended).expect_err("growth must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        fs::read(&path).expect("read after rejected growth"),
        existing
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn parses_odytty_owned_hosts_with_profile_fields() {
    let entries = parse_odytty_hosts_bytes_with_limits(
        br#"
                # synthetic OdyTTY-owned hosts list
                Host web1 web2
                    HostName gateway.example.invalid
                    User deploy
                    Port 2222
                    Theme odyssey
                    Font "Victor Mono"
                    Title "Synthetic Web"
            "#,
        limits(),
    );

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].alias, "web1");
    assert_eq!(
        entries[0].host_name.as_deref(),
        Some("gateway.example.invalid")
    );
    assert_eq!(entries[0].user.as_deref(), Some("deploy"));
    assert_eq!(entries[0].port, Some(2222));
    assert_eq!(entries[0].theme.as_deref(), Some("odyssey"));
    assert_eq!(entries[0].font.as_deref(), Some("Victor Mono"));
    assert_eq!(entries[0].title.as_deref(), Some("Synthetic Web"));
    assert_eq!(entries[0].integration, None);
    assert_eq!(entries[0].source, ConnectionHostSource::Odytty);
}

#[test]
fn parses_and_round_trips_per_host_integration_optout() {
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host secure\n    HostName secure.example.invalid\n    Integration off\n",
        limits(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].integration, Some(false));

    // The field survives a render/reload round-trip.
    let mut formatted = String::new();
    push_host_block(&mut formatted, &entries[0]);
    assert!(formatted.contains("Integration off"));
    let reparsed = parse_odytty_hosts_bytes_with_limits(formatted.as_bytes(), limits());
    assert_eq!(reparsed[0].integration, Some(false));
}

#[test]
fn per_host_integration_ignores_unrecognized_values() {
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host a\nHost b\n    Integration maybe\n    Integration on\n",
        limits(),
    );
    // Absent field inherits the global default (None); a later valid value
    // wins over an earlier unrecognized one.
    assert_eq!(entries[0].integration, None);
    assert_eq!(entries[1].integration, Some(true));
}

#[test]
fn parses_and_round_trips_per_host_reuse_optout() {
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host bastion\n    HostName bastion.example.invalid\n    Reuse off\n",
        limits(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].reuse, Some(false));
    assert_eq!(entries[0].integration, None);

    let mut formatted = String::new();
    push_host_block(&mut formatted, &entries[0]);
    assert!(formatted.contains("Reuse off"));
    let reparsed = parse_odytty_hosts_bytes_with_limits(formatted.as_bytes(), limits());
    assert_eq!(reparsed[0].reuse, Some(false));
}

#[test]
fn parses_and_round_trips_per_host_tmux_override() {
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host persistent\n    HostName persistent.example.invalid\n    Tmux on\n",
        limits(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].tmux, Some(true));
    assert_eq!(entries[0].reuse, None);
    assert_eq!(entries[0].integration, None);

    let mut formatted = String::new();
    push_host_block(&mut formatted, &entries[0]);
    assert!(formatted.contains("Tmux on"));
    let reparsed = parse_odytty_hosts_bytes_with_limits(formatted.as_bytes(), limits());
    assert_eq!(reparsed[0].tmux, Some(true));
}

#[test]
fn append_round_trips_odytty_owned_hosts() {
    let dir = temp_dir("odytty-connection-hosts");
    let path = hosts_file_path(&dir);
    let mut entry = host("web 1", ConnectionHostSource::Odytty);
    entry.user = Some("deploy".to_owned());
    entry.title = Some("Synthetic Web".to_owned());

    append_adhoc_host(&path, &entry).expect("append synthetic host");
    let loaded = read_odytty_hosts_with_limits(&path, limits());

    assert_eq!(loaded, vec![entry]);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn mutation_rejects_control_characters_without_changing_existing_bytes() {
    let dir = temp_dir("odytty-hosts-controls");
    let path = hosts_file_path(&dir);
    let original = annotated_fixture();
    for control in ['\r', '\n', '\t', '\0', '\u{0085}'] {
        fs::write(&path, &original).expect("seed fixture");
        let mut appended = host("safe", ConnectionHostSource::Odytty);
        appended.title = Some(format!("unsafe{control}value"));
        let append_error = append_adhoc_host(&path, &appended).expect_err("append rejects control");
        assert_eq!(append_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        let mut updated = host("alpha", ConnectionHostSource::Odytty);
        updated.user = Some(format!("unsafe{control}value"));
        let edit_error =
            edit_host_block(&path, "alpha", &updated).expect_err("edit rejects control");
        assert_eq!(edit_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        let remove_error = remove_host_block(&path, &format!("alpha{control}"))
            .expect_err("remove rejects control selector");
        assert_eq!(remove_error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn missing_and_malformed_hosts_files_degrade_to_empty_or_partial() {
    let dir = temp_dir("odytty-connection-hosts-missing");
    assert!(read_odytty_hosts_with_limits(dir.join("missing"), limits()).is_empty());

    let partial = parse_odytty_hosts_bytes_with_limits(
        b"Host good\nHostName good.example.invalid\n\xff",
        limits(),
    );
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].alias, "good");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn caps_are_enforced_for_owned_hosts() {
    let capped = ConnectionHostsLimits {
        max_bytes: 48,
        max_entries: 1,
        max_field_chars: 4,
    };
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host abcdef\nHostName abcdef.example.invalid\nHost second\n",
        capped,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].alias, "abcd");
    assert_eq!(entries[0].host_name.as_deref(), Some("abcd"));
}

#[test]
fn disabled_ssh_config_integration_never_calls_ssh_loader() {
    let entries = load_connection_hosts_with_loaders(
        false,
        || vec![host("owned", ConnectionHostSource::Odytty)],
        || panic!("disabled SSH config source must not be read"),
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].alias, "owned");
}

#[test]
fn opt_in_ssh_config_hosts_are_merged_after_owned_hosts() {
    let entries = load_connection_hosts_with_loaders(
        true,
        || vec![host("owned", ConnectionHostSource::Odytty)],
        || {
            vec![
                SshHostEntry {
                    alias: "owned".to_owned(),
                    host_name: Some("ssh-owned.example.invalid".to_owned()),
                    user: Some("ssh".to_owned()),
                    port: Some(2200),
                },
                SshHostEntry {
                    alias: "remote".to_owned(),
                    host_name: Some("remote.example.invalid".to_owned()),
                    user: None,
                    port: None,
                },
            ]
        },
    );

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.alias.as_str())
            .collect::<Vec<_>>(),
        vec!["owned", "remote"]
    );
    assert_eq!(entries[0].source, ConnectionHostSource::Odytty);
    assert_eq!(entries[1].source, ConnectionHostSource::SshConfig);
}

#[test]
fn loaded_connection_hosts_honor_settings_opt_in() {
    let dir = temp_dir("odytty-connection-hosts-settings");
    let owned_path = hosts_file_path(&dir);
    let ssh_path = dir.join("ssh_config");
    fs::write(&owned_path, b"Host owned\nHostName owned.example.invalid\n")
        .expect("write synthetic owned hosts");
    fs::write(&ssh_path, b"Host remote\nHostName remote.example.invalid\n")
        .expect("write synthetic ssh config");
    let paths = ConnectionHostPaths::new(&owned_path, Some(ssh_path));

    let disabled = load_connection_hosts(&Settings::default(), &paths);
    let enabled = load_connection_hosts(
        &Settings {
            ssh_config_hosts: true,
            ..Settings::default()
        },
        &paths,
    );

    assert_eq!(
        disabled
            .iter()
            .map(|entry| entry.alias.as_str())
            .collect::<Vec<_>>(),
        vec!["owned"]
    );
    assert_eq!(
        enabled
            .iter()
            .map(|entry| entry.alias.as_str())
            .collect::<Vec<_>>(),
        vec!["owned", "remote"]
    );
    fs::remove_dir_all(dir).ok();
}

// ---- Block-span byte-splice edit / remove ----

/// A hand-annotated fixture: a leading comment, three blocks (one
/// multi-alias), an in-block comment, an unknown field, a tab indent, and
/// blank-line separators - the shapes an in-place edit must not disturb.
fn annotated_fixture() -> String {
    [
        "# OdyTTY hosts - hand annotated",
        "",
        "Host alpha",
        "    HostName alpha.example.invalid",
        "    User alice",
        "    # alpha is the bastion",
        "    XCustomField keep-me",
        "",
        "Host beta gamma",
        "\tHostName beta.example.invalid",
        "    User bob",
        "",
        "# a note that belongs to delta",
        "Host delta",
        "    HostName delta.example.invalid",
        "",
    ]
    .join("\n")
}

fn max_chars() -> usize {
    limits().max_field_chars
}

#[test]
fn edit_replaces_only_the_target_block_bytes() {
    let orig = annotated_fixture();
    let mut updated = host("beta", ConnectionHostSource::Odytty);
    updated.host_name = Some("beta.example.invalid".to_owned());
    updated.user = Some("bob-2".to_owned());

    let edited = splice_host_block_edit(&orig, "beta", &updated, max_chars()).expect("beta edited");

    // Everything before the edited block is byte-identical...
    let head = orig.find("Host beta gamma").expect("beta present");
    assert_eq!(
        &edited[..head],
        &orig[..head],
        "preamble + alpha block untouched"
    );
    // ...and everything from the delta preamble comment onward is too.
    let tail = orig
        .find("# a note that belongs to delta")
        .expect("delta note present");
    assert!(
        edited.ends_with(&orig[tail..]),
        "delta note + delta block untouched"
    );
    // The edited block took the new user and canonicalized its tab indent;
    // its sibling alias survived.
    assert!(edited.contains("Host beta gamma"), "sibling alias kept");
    assert!(edited.contains("    User bob-2"));
    assert!(
        !edited.contains("\tHostName beta"),
        "tab indent canonicalized"
    );
    assert!(!edited.contains("    User bob\n"), "old user replaced");
}

#[test]
fn edit_preserves_unknown_lines_inside_the_edited_block() {
    let orig = annotated_fixture();
    let mut updated = host("alpha", ConnectionHostSource::Odytty);
    updated.host_name = Some("alpha.example.invalid".to_owned());
    updated.user = Some("alice-2".to_owned());

    let edited =
        splice_host_block_edit(&orig, "alpha", &updated, max_chars()).expect("alpha edited");

    assert!(edited.contains("    User alice-2"), "new user applied");
    // The unknown field and in-block comment survive, re-emitted after the
    // known fields.
    assert!(
        edited.contains("    XCustomField keep-me"),
        "unknown field kept"
    );
    assert!(
        edited.contains("    # alpha is the bastion"),
        "in-block comment kept"
    );
    let user_at = edited.find("User alice-2").unwrap();
    let unknown_at = edited.find("XCustomField keep-me").unwrap();
    assert!(
        unknown_at > user_at,
        "unknown lines follow the known fields"
    );
}

#[test]
fn edit_then_parse_round_trips_the_new_value() {
    let dir = temp_dir("odytty-edit-parse");
    let path = hosts_file_path(&dir);
    fs::write(&path, annotated_fixture()).expect("seed");

    let mut updated = host("alpha", ConnectionHostSource::Odytty);
    updated.host_name = Some("alpha.example.invalid".to_owned());
    updated.user = Some("alice-2".to_owned());
    let outcome = edit_host_block(&path, "alpha", &updated).expect("edit");
    assert_eq!(outcome, HostsEditOutcome::Written);

    let reparsed = read_odytty_hosts_with_limits(&path, limits());
    let alpha = reparsed
        .iter()
        .find(|h| h.alias == "alpha")
        .expect("alpha still present");
    assert_eq!(alpha.user.as_deref(), Some("alice-2"));
    // The unknown field remains in the raw file after the round trip.
    let raw = fs::read_to_string(&path).expect("read back");
    assert!(raw.contains("XCustomField keep-me"));
    fs::remove_dir_all(dir).ok();
}

#[test]
fn remove_middle_block_leaves_no_doubled_gap() {
    let orig = annotated_fixture();
    let edited = splice_host_block_remove(&orig, "beta", max_chars()).expect("beta removed");

    let head = orig.find("Host beta gamma").unwrap();
    assert_eq!(&edited[..head], &orig[..head], "prefix untouched");
    let tail = orig.find("# a note that belongs to delta").unwrap();
    assert!(edited.ends_with(&orig[tail..]), "suffix untouched");
    assert!(!edited.contains("Host beta gamma"));
    assert!(!edited.contains("User bob"));
    assert!(
        !edited.contains("\n\n\n"),
        "trailing blank consumed, no doubled gap"
    );
}

#[test]
fn remove_first_block_keeps_the_preamble() {
    let orig = annotated_fixture();
    let edited = splice_host_block_remove(&orig, "alpha", max_chars()).expect("alpha removed");

    let head = orig.find("Host alpha").unwrap();
    assert_eq!(&edited[..head], &orig[..head], "leading comment preserved");
    let tail = orig.find("Host beta gamma").unwrap();
    assert!(
        edited.ends_with(&orig[tail..]),
        "remaining blocks untouched"
    );
    assert!(edited.starts_with("# OdyTTY hosts"));
    assert!(!edited.contains("Host alpha"));
    assert!(
        !edited.contains("XCustomField"),
        "alpha's unknown field left with it"
    );
}

#[test]
fn remove_last_block_orphans_only_its_own_body() {
    let orig = annotated_fixture();
    let edited = splice_host_block_remove(&orig, "delta", max_chars()).expect("delta removed");

    let head = orig.find("Host delta").unwrap();
    assert_eq!(&edited[..head], &orig[..head], "prefix untouched");
    assert!(!edited.contains("Host delta"));
    // The note above delta is its preamble (blank-line separated), owned by
    // no block, so it stays.
    assert!(edited.contains("# a note that belongs to delta"));
    assert!(edited.contains("Host beta gamma"));
}

#[test]
fn edit_and_remove_on_a_missing_file_report_not_found() {
    let dir = temp_dir("odytty-edit-missing");
    let path = dir.join("does-not-exist").join(CONNECTION_HOSTS_FILE_NAME);
    let updated = host("x", ConnectionHostSource::Odytty);

    assert_eq!(
        edit_host_block(&path, "x", &updated).expect("edit missing"),
        HostsEditOutcome::NotFound
    );
    assert_eq!(
        remove_host_block(&path, "x").expect("remove missing"),
        HostsEditOutcome::NotFound
    );
    assert!(!path.exists(), "a not-found edit never creates the file");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn edit_on_empty_or_unmatched_file_leaves_it_untouched() {
    let dir = temp_dir("odytty-edit-untouched");
    let path = hosts_file_path(&dir);

    // Empty file: nothing to edit.
    fs::write(&path, b"").expect("seed empty");
    assert_eq!(
        edit_host_block(&path, "any", &host("any", ConnectionHostSource::Odytty))
            .expect("edit empty"),
        HostsEditOutcome::NotFound
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "");

    // Populated file, alias that matches no block: bytes unchanged.
    let fixture = annotated_fixture();
    fs::write(&path, &fixture).expect("seed fixture");
    assert_eq!(
        remove_host_block(&path, "nonexistent").expect("remove unmatched"),
        HostsEditOutcome::NotFound
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), fixture);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn edit_stays_byte_aligned_at_eof_without_a_trailing_newline() {
    let orig = "Host solo\n    User x".to_owned();
    let mut updated = host("solo", ConnectionHostSource::Odytty);
    updated.host_name = None;
    updated.user = Some("y".to_owned());

    let edited = splice_host_block_edit(&orig, "solo", &updated, max_chars()).expect("solo edited");
    assert_eq!(
        edited, "Host solo\n    User y",
        "no trailing newline injected"
    );
}

#[test]
fn persist_field_parses_emits_and_survives_an_edit() {
    // A per-host Persist override parses and round-trips.
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host pw\n    HostName pw.example.invalid\n    Persist 2h\n",
        limits(),
    );
    assert_eq!(entries[0].persist.as_deref(), Some("2h"));
    let mut rendered = String::new();
    push_host_block(&mut rendered, &entries[0]);
    assert!(rendered.contains("Persist 2h"));

    // An off override is preserved verbatim.
    let off = parse_odytty_hosts_bytes_with_limits(b"Host po\n    Persist off\n", limits());
    assert_eq!(off[0].persist.as_deref(), Some("off"));
}

#[test]
fn protocol_field_parses_emits_and_preserves_odd_values() {
    // The reserved Protocol field parses and round-trips through a render.
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host p\n    HostName p.example.invalid\n    Protocol ssh\n",
        limits(),
    );
    assert_eq!(entries[0].protocol.as_deref(), Some("ssh"));
    let mut rendered = String::new();
    push_host_block(&mut rendered, &entries[0]);
    assert!(rendered.contains("Protocol ssh"));

    // A non-`ssh` value is preserved verbatim (reserved, forward-compatible)
    // rather than dropped, so a hand-added protocol survives a reparse.
    let odd = parse_odytty_hosts_bytes_with_limits(b"Host q\n    Protocol quic\n", limits());
    assert_eq!(odd[0].protocol.as_deref(), Some("quic"));
}

#[test]
fn identity_file_parses_emits_and_survives_an_edit() {
    // IdentityFile stores a key PATH (never a secret) and
    // round-trips through a render.
    let entries = parse_odytty_hosts_bytes_with_limits(
        b"Host k\n    HostName k.example.invalid\n    IdentityFile /home/user/.ssh/id_ed25519.example\n",
        limits(),
    );
    assert_eq!(
        entries[0].identity_file.as_deref(),
        Some("/home/user/.ssh/id_ed25519.example")
    );
    let mut rendered = String::new();
    push_host_block(&mut rendered, &entries[0]);
    assert!(rendered.contains("IdentityFile /home/user/.ssh/id_ed25519.example"));

    // An in-place edit that sets the identity path splices it in while every
    // other byte is preserved.
    let dir = temp_dir("odytty-identity-edit");
    let path = hosts_file_path(&dir);
    fs::write(&path, annotated_fixture()).expect("seed");
    let mut updated = host("alpha", ConnectionHostSource::Odytty);
    updated.host_name = Some("alpha.example.invalid".to_owned());
    updated.identity_file = Some("/home/user/.ssh/alpha.example".to_owned());
    assert_eq!(
        edit_host_block(&path, "alpha", &updated).expect("edit"),
        HostsEditOutcome::Written
    );
    let reparsed = read_odytty_hosts_with_limits(&path, limits());
    let alpha = reparsed.iter().find(|h| h.alias == "alpha").expect("alpha");
    assert_eq!(
        alpha.identity_file.as_deref(),
        Some("/home/user/.ssh/alpha.example")
    );
    let raw = fs::read_to_string(&path).expect("read back");
    assert!(
        raw.contains("XCustomField keep-me"),
        "unknown field preserved"
    );
    fs::remove_dir_all(dir).ok();
}

fn plain(alias: &str) -> ConnectionHost {
    ConnectionHost {
        host_name: None,
        ..host(alias, ConnectionHostSource::Odytty)
    }
}

/// A byte order mark at the start of the file no longer hides the first host
/// from the reader, the mutation lookups, or the append collision check, and
/// editing or removing a host keeps the mark.
#[test]
fn a_byte_order_mark_does_not_hide_the_first_host() {
    let dir = crate::test_dirs::ScratchDir::new("odytty-hosts-bom");
    let path = dir.path().join("hosts.conf");
    fs::write(
        &path,
        b"\xEF\xBB\xBFHost first\n    User deploy\n\nHost second\n    User ops\n",
    )
    .expect("fixture");
    let aliases: Vec<_> = read_odytty_hosts(&path)
        .into_iter()
        .map(|entry| entry.alias)
        .collect();
    assert_eq!(aliases, ["first", "second"]);
    assert_eq!(
        append_adhoc_host(&path, &plain("first")).expect("append"),
        AppendHostOutcome::AlreadyExists
    );
    let mut renamed = plain("first");
    renamed.user = Some("admin".to_owned());
    assert_eq!(
        edit_host_block(&path, "first", &renamed).expect("edit"),
        HostsEditOutcome::Written
    );
    assert_eq!(
        remove_host_block(&path, "second").expect("remove"),
        HostsEditOutcome::Written
    );
    let saved = fs::read(&path).expect("saved");
    assert!(saved.starts_with(UTF8_BOM), "the mark is kept");
    assert_eq!(&saved[UTF8_BOM.len()..], b"Host first\n    User admin\n\n");
}

/// Editing or removing a host refuses a file that is not valid UTF-8 and leaves
/// every byte as it was, instead of rewriting other lines from a lossy decode.
#[test]
fn mutations_refuse_a_file_that_is_not_utf8_and_leave_it_untouched() {
    let dir = crate::test_dirs::ScratchDir::new("odytty-hosts-latin1");
    let path = dir.path().join("hosts.conf");
    let original = b"# caf\xE9 servers\nHost one\n    User a\n\nHost two\n".to_vec();
    fs::write(&path, &original).expect("fixture");
    let error = edit_host_block(&path, "one", &plain("one")).expect_err("edit refuses");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let error = remove_host_block(&path, "two").expect_err("remove refuses");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).expect("unchanged"), original);
}

/// The writer refuses what the reader would drop or alter: a non-concrete
/// alias and port 0. An empty field is written as unset rather than as a bare
/// key, and the reader takes only digit ports from 1 to 65535.
#[test]
fn the_writer_refuses_what_the_reader_would_drop() {
    let dir = crate::test_dirs::ScratchDir::new("odytty-hosts-roundtrip");
    let path = dir.path().join("hosts.conf");
    for alias in ["", "!negated", "web*", "db?"] {
        let error = append_adhoc_host(&path, &plain(alias)).expect_err(alias);
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{alias:?}");
    }
    let mut zero = plain("zero");
    zero.port = Some(0);
    assert_eq!(
        append_adhoc_host(&path, &zero).expect_err("port 0").kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!path.exists(), "nothing was written");

    let mut blank = plain("blank");
    blank.user = Some(String::new());
    append_adhoc_host(&path, &blank).expect("append");
    assert_eq!(fs::read_to_string(&path).expect("saved"), "Host blank\n");
    assert_eq!(read_odytty_hosts(&path)[0].user, None);

    let ports = parse_odytty_hosts_bytes(
        b"Host a\n    Port 0\nHost b\n    Port +22\nHost c\n    Port 22\n",
    );
    let ports: Vec<_> = ports.iter().map(|entry| entry.port).collect();
    assert_eq!(ports, [None, None, Some(22)]);
}

/// The append collision check sees a saved alias past the reader's entry cap.
#[test]
fn the_append_collision_check_sees_hosts_past_the_entry_cap() {
    let dir = crate::test_dirs::ScratchDir::new("odytty-hosts-many");
    let path = dir.path().join("hosts.conf");
    let mut text = String::new();
    for index in 0..DEFAULT_CONNECTION_HOSTS_MAX_ENTRIES + 10 {
        text.push_str(&format!("Host h{index}\n"));
    }
    fs::write(&path, &text).expect("fixture");
    let last = format!("h{}", DEFAULT_CONNECTION_HOSTS_MAX_ENTRIES + 9);
    assert_eq!(
        append_adhoc_host(&path, &plain(&last)).expect("append"),
        AppendHostOutcome::AlreadyExists
    );
    assert_eq!(fs::read_to_string(&path).expect("unchanged"), text);
}

/// A file longer than the read limit keeps only the complete lines before the
/// cut, so a value the cut ended is never read as written.
#[test]
fn a_cut_read_drops_the_partial_last_line() {
    let text = "Host a\n    HostName long.example.invalid\n";
    let cut = text.find("long.exa").expect("value") + "long.exa".len();
    let limited = ConnectionHostsLimits {
        max_bytes: cut as u64,
        ..limits()
    };
    let entries = parse_odytty_hosts_bytes_with_limits(text.as_bytes(), limited);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].host_name, None, "the cut HostName is dropped");

    let dir = crate::test_dirs::ScratchDir::new("odytty-hosts-cut");
    let path = dir.path().join("hosts.conf");
    fs::write(&path, text).expect("fixture");
    let read = read_odytty_hosts_with_limits(&path, limited);
    assert_eq!(read[0].host_name, None, "the file reader agrees");
    // A file that ends exactly at the limit is whole, not cut.
    let whole = ConnectionHostsLimits {
        max_bytes: text.len() as u64,
        ..limits()
    };
    assert_eq!(
        read_odytty_hosts_with_limits(&path, whole)[0]
            .host_name
            .as_deref(),
        Some("long.example.invalid")
    );
}

/// The cut is found in file bytes, a byte order mark included, so a file with
/// a mark that the limit cuts inside `HostName` drops the partial line just as
/// a file without one does, on the byte parser and the file reader alike.
/// Before, the mark was removed before the length check, so such a file read
/// as complete and kept the cut value.
#[test]
fn a_cut_read_with_a_byte_order_mark_drops_the_partial_last_line() {
    for bom in [&b""[..], UTF8_BOM] {
        let mut file = bom.to_vec();
        file.extend_from_slice(b"Host a\n    HostName long.example.invalid\n");
        let cut = bom.len() + "Host a\n    HostName long.exa".len();
        let dir = crate::test_dirs::ScratchDir::new("odytty-hosts-bom-cut");
        let path = dir.path().join("hosts.conf");
        fs::write(&path, &file).expect("fixture");
        for max_bytes in [cut, cut + 1, cut + 2] {
            let limited = ConnectionHostsLimits {
                max_bytes: max_bytes as u64,
                ..limits()
            };
            let parsed = parse_odytty_hosts_bytes_with_limits(&file, limited);
            let read = read_odytty_hosts_with_limits(&path, limited);
            for (source, entries) in [("bytes", &parsed), ("file", &read)] {
                assert_eq!(
                    entries.len(),
                    1,
                    "{source}, mark {}, cap {max_bytes}",
                    bom.len()
                );
                assert_eq!(entries[0].alias, "a");
                assert_eq!(
                    entries[0].host_name,
                    None,
                    "{source}, mark {}, cap {max_bytes}: the cut HostName is dropped",
                    bom.len()
                );
            }
        }
        // Ending exactly at the limit, the file is whole.
        let whole = ConnectionHostsLimits {
            max_bytes: file.len() as u64,
            ..limits()
        };
        for entries in [
            parse_odytty_hosts_bytes_with_limits(&file, whole),
            read_odytty_hosts_with_limits(&path, whole),
        ] {
            assert_eq!(
                entries[0].host_name.as_deref(),
                Some("long.example.invalid"),
                "mark {}",
                bom.len()
            );
        }
    }
}

/// When the first byte past the limit is a newline, the last line inside the
/// limit is complete and is kept, with or without a byte order mark.
#[test]
fn a_cut_just_before_a_newline_keeps_the_complete_last_line() {
    for bom in [&b""[..], UTF8_BOM] {
        let mut file = bom.to_vec();
        file.extend_from_slice(b"Host a\n    HostName a.example.invalid\nHost b\n");
        let limit = bom.len() + "Host a\n    HostName a.example.invalid".len();
        let limited = ConnectionHostsLimits {
            max_bytes: limit as u64,
            ..limits()
        };
        let dir = crate::test_dirs::ScratchDir::new("odytty-hosts-newline-cut");
        let path = dir.path().join("hosts.conf");
        fs::write(&path, &file).expect("fixture");
        for entries in [
            parse_odytty_hosts_bytes_with_limits(&file, limited),
            read_odytty_hosts_with_limits(&path, limited),
        ] {
            assert_eq!(
                entries.len(),
                1,
                "mark {}: host b is past the limit",
                bom.len()
            );
            assert_eq!(
                entries[0].host_name.as_deref(),
                Some("a.example.invalid"),
                "mark {}",
                bom.len()
            );
        }
    }
}
