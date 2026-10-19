// SPDX-License-Identifier: GPL-3.0-only
//! Round-trip, version-skew, atomic-write, and hand-serializer tests for the
//! workspace shape snapshot. These are model-free (they build `ShapeSnapshot`
//! values directly); the capture-from-the-live-model path is exercised in
//! `tabs_sessions.rs` where headless `App` construction lives.

use super::*;

/// Mutation: default legacy leaves to read-only, drop nested split ratios,
/// or parse foreign schema payloads before returning VersionSkew.
#[test]
fn phase9_legacy_nested_layout_is_writable_and_foreign_schema_is_not_restored() {
    let text = r#"{
      "version": 1, "active_workspace": 0, "workspaces": [{
        "name": "legacy", "active_tab": 0, "tabs": [{
          "title": "legacy split", "focused_leaf": 1,
          "layout": {"split": {"axis": "columns", "ratio": 0.25,
            "first": {"leaf": {"cwd": "C:\\synthetic\\work"}},
            "second": {"leaf": {"cwd": null}}
          }}
        }]
      }]
    }"#;
    let snapshot = ShapeSnapshot::from_json_str(text).expect("legacy schema supported");
    let layout = &snapshot.workspaces[0].tabs[0].layout;
    assert_eq!(
        layout,
        &PaneShape::Split {
            axis: SplitAxisShape::Columns,
            ratio: 0.25,
            first: Box::new(leaf(Some(r"C:\synthetic\work"))),
            second: Box::new(leaf(None)),
        }
    );
    assert_eq!(
        ShapeSnapshot::from_json_str(&snapshot.to_json_pretty()).unwrap(),
        snapshot
    );
    for version in [0, SNAPSHOT_VERSION + 1] {
        let foreign = format!(r#"{{"version":{version},"workspaces":"foreign-format"}}"#);
        assert!(matches!(ShapeSnapshot::from_json_str(&foreign),
            Err(LoadError::VersionSkew { found }) if found == version));
    }
}

fn leaf(cwd: Option<&str>) -> PaneShape {
    PaneShape::Leaf {
        cwd: cwd.map(str::to_owned),
        session_host_id: None,
        remote_host: None,
        launch_profile: None,
        read_only: false,
    }
}

/// A deliberately rich sample: two workspaces, nested splits on both axes, a
/// tab title, a Windows drive-letter cwd, and an unknown (None) cwd.
fn sample_snapshot() -> ShapeSnapshot {
    ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 1,
        workspaces: vec![
            WorkspaceShape {
                name: "Workspace 1".to_owned(),
                default_profile: None,
                launch_profile: None,
                active_tab: 1,
                tabs: vec![
                    TabShape {
                        arrangement: Default::default(),
                        title: None,
                        focused_leaf: 0,
                        layout: leaf(Some("/home/tester")),
                    },
                    TabShape {
                        arrangement: Default::default(),
                        title: Some("build".to_owned()),
                        focused_leaf: 1,
                        layout: PaneShape::Split {
                            axis: SplitAxisShape::Columns,
                            ratio: 0.5,
                            first: Box::new(leaf(Some("/tmp"))),
                            second: Box::new(leaf(None)),
                        },
                    },
                ],
            },
            WorkspaceShape {
                name: "logs".to_owned(),
                default_profile: None,
                launch_profile: None,
                active_tab: 0,
                tabs: vec![TabShape {
                    arrangement: Default::default(),
                    title: None,
                    focused_leaf: 2,
                    layout: PaneShape::Split {
                        axis: SplitAxisShape::Rows,
                        ratio: 0.35,
                        first: Box::new(leaf(Some(r"C:\Users\Tester\project"))),
                        second: Box::new(PaneShape::Split {
                            axis: SplitAxisShape::Columns,
                            ratio: 0.6,
                            first: Box::new(leaf(None)),
                            second: Box::new(leaf(Some("/var/log"))),
                        }),
                    },
                }],
            },
        ],
    }
}

#[test]
fn round_trips_a_multi_workspace_multi_pane_tree() {
    let snapshot = sample_snapshot();
    let text = snapshot.to_json_pretty();
    let parsed = ShapeSnapshot::from_json_str(&text).expect("valid snapshot round-trips");
    assert_eq!(parsed, snapshot);
}

#[test]
fn pretty_output_puts_version_first_and_indents() {
    let text = sample_snapshot().to_json_pretty();
    assert!(
        text.starts_with("{\n  \"version\": 1,\n"),
        "version must be the first, integer-valued key: {text}"
    );
    assert!(
        text.ends_with("}\n"),
        "a trailing newline keeps the file tidy"
    );
    // Ratios keep their fractional form; indices/version print as integers.
    assert!(text.contains("\"ratio\": 0.5"), "0.5 stays fractional");
    assert!(text.contains("\"active_tab\": 1"), "indices are integers");
}

#[test]
fn newer_version_is_reported_as_skew_not_a_hard_error() {
    let text = r#"{ "version": 999, "active_workspace": 0, "workspaces": [] }"#;
    match ShapeSnapshot::from_json_str(text) {
        Err(LoadError::VersionSkew { found: 999 }) => {}
        other => panic!("a newer version must be a soft skew, got {other:?}"),
    }
}

#[test]
fn unknown_fields_are_ignored_for_forward_compat() {
    // A future writer adds keys at every level; this reader must ignore them.
    let text = r#"{
      "version": 1,
      "active_workspace": 0,
      "future_top_level": true,
      "workspaces": [
        {
          "name": "w",
          "active_tab": 0,
          "color": "teal",
          "tabs": [
            { "title": null, "focused_leaf": 0, "pinned": true,
              "layout": { "leaf": { "cwd": "/x", "host": "example" } } }
          ]
        }
      ]
    }"#;
    let parsed = ShapeSnapshot::from_json_str(text).expect("unknown keys are tolerated");
    assert_eq!(parsed.workspaces.len(), 1);
    assert_eq!(parsed.workspaces[0].name, "w");
    assert_eq!(
        parsed.workspaces[0].tabs[0].layout,
        leaf(Some("/x")),
        "known leaf fields survive, unknown ones are dropped"
    );
}

#[test]
fn windows_drive_letter_cwd_round_trips_with_escaped_backslashes() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout: leaf(Some(r"C:\Users\Tester\Documents")),
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    // Backslashes must be JSON-escaped on the wire.
    assert!(
        text.contains(r#""cwd": "C:\\Users\\Tester\\Documents""#),
        "drive-letter path must serialize with escaped backslashes: {text}"
    );
    let parsed = ShapeSnapshot::from_json_str(&text).expect("round-trips");
    assert_eq!(parsed, snapshot);
}

#[test]
fn unicode_and_control_characters_in_names_round_trip() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "研究 🚀 \"quoted\"\tname\nwith\rcontrols".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: Some("emoji 😺 tab".to_owned()),
                focused_leaf: 0,
                layout: leaf(Some("/tmp")),
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    let parsed = ShapeSnapshot::from_json_str(&text).expect("unicode/control round-trips");
    assert_eq!(parsed, snapshot);
}

#[test]
fn null_title_and_cwd_round_trip_to_none() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout: leaf(None),
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    assert!(text.contains("\"title\": null"));
    assert!(text.contains("\"cwd\": null"));
    let parsed = ShapeSnapshot::from_json_str(&text).expect("round-trips");
    assert_eq!(parsed, snapshot);
    assert_eq!(parsed.workspaces[0].tabs[0].title, None);
}

#[test]
fn malformed_json_is_reported_not_panicked() {
    for text in [
        "{ not json",
        "",
        "{ \"version\": 1 ",
        "{ \"version\": 1, \"workspaces\": [ { \"tabs\": [ { } ] } ] }",
    ] {
        match ShapeSnapshot::from_json_str(text) {
            Err(LoadError::Malformed(_)) => {}
            other => panic!("{text:?} must be Malformed, got {other:?}"),
        }
    }
}

#[test]
fn missing_version_is_malformed() {
    let text = r#"{ "active_workspace": 0, "workspaces": [] }"#;
    assert!(matches!(
        ShapeSnapshot::from_json_str(text),
        Err(LoadError::Malformed(_))
    ));
}

#[test]
fn atomic_write_syncs_and_round_trips_a_multi_kib_payload() {
    // F23: write_atomic now sync_all's the temp file and fsyncs the parent dir
    // before returning (parity with the settings writeback path). The durable
    // path must still round-trip exactly and leave no temp sibling - a crash can
    // only ever leave the temp behind, never a renamed-but-empty target.
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    let path = dir.join("nested").join("workspaces.json");

    // A multi-KiB payload exercises the explicit write_all + sync_all path, and a
    // not-yet-existing nested parent exercises create_dir_all + the dir fsync.
    let payload = "line\n".repeat(4096);
    write_atomic(&path, &payload).expect("durable write");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), payload);

    // Overwrite in place, then confirm no `.tmp` sibling remains.
    write_atomic(&path, "small\n").expect("overwrite");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "small\n");
    let leftovers: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        vec!["workspaces.json".to_owned()],
        "no temp leftovers"
    );
}

#[test]
fn atomic_write_replaces_target_and_leaves_no_temp_file() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    let path = dir.join("workspaces.json");

    write_atomic(&path, "first\n").expect("first write");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "first\n");

    // Overwrite must replace in place, atomically.
    write_atomic(&path, "second\n").expect("second write");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "second\n");

    // Only the target file remains - no `.tmp` sibling left behind.
    let entries: Vec<String> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec!["workspaces.json".to_owned()],
        "no temp leftovers"
    );
}

#[cfg(unix)]
#[test]
fn atomic_write_repairs_parent_and_target_modes_and_rejects_a_symlink_target() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let root = fixture.path().join("state");
    std::fs::create_dir(&root).expect("create root");
    let state = root.join("state");
    std::fs::create_dir(&state).expect("create state");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).expect("chmod state");
    let path = state.join("workspaces.json");
    std::fs::write(&path, "old\n").expect("seed snapshot");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("chmod snapshot");

    write_atomic(&path, "new\n").expect("replace snapshot");
    assert_eq!(
        std::fs::read_to_string(&path).expect("snapshot contents"),
        "new\n"
    );
    assert_eq!(
        std::fs::metadata(&state)
            .expect("state metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path)
            .expect("snapshot metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let target = root.join("unrelated");
    std::fs::write(&target, "keep\n").expect("seed unrelated target");
    let linked = state.join("linked.json");
    symlink(&target, &linked).expect("create target symlink");
    assert!(write_atomic(&linked, "must not replace\n").is_err());
    assert_eq!(
        std::fs::read_to_string(&target).expect("unrelated contents"),
        "keep\n"
    );
    assert!(
        std::fs::read_dir(&state)
            .expect("state entries")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
        "failed replacement cleans up its owned temporary sibling"
    );
}

#[test]
fn json_escapes_round_trip_through_the_parser() {
    let value = json::parse(r#"{ "s": "a\"b\\c\n\t\/\u0041\uD83D\uDE00" }"#).expect("parse");
    assert_eq!(
        value.get("s").and_then(json::Json::as_str),
        Some("a\"b\\c\n\t/A😀")
    );
}

#[test]
fn deeply_nested_json_returns_an_error_instead_of_overflowing_the_stack() {
    // A pathological run of opening brackets (far past any real layout depth)
    // must be rejected as malformed, not abort the process via stack overflow.
    // This routes the file to the Corrupt -> fresh-launch degrade path.
    let bomb = "[".repeat(10_000);
    let parsed = json::parse(&bomb);
    assert!(
        parsed.is_err(),
        "deeply-nested input must return Err, not crash"
    );

    // The same input at the snapshot layer classifies as Malformed (soft), so
    // load degrades to a fresh launch rather than re-aborting every launch.
    assert!(matches!(
        ShapeSnapshot::from_json_str(&bomb),
        Err(LoadError::Malformed(_))
    ));
}

#[test]
fn nesting_just_within_the_cap_still_parses() {
    // Empty innermost containers consume one frame each, so these exercise
    // the exact parser boundary, through both readers and container kinds.
    for (open, close) in [("[", "]"), ("{\"nested\":", "}")] {
        for depth in [128, 129] {
            let text = if open == "[" {
                format!("{}{}", open.repeat(depth), close.repeat(depth))
            } else {
                format!("{}{{}}{}", open.repeat(depth - 1), close.repeat(depth - 1))
            };
            for parsed in [
                json::parse(&text),
                crate::profiles::parse_json(&text).map(|_| Json::Null),
            ] {
                if depth == 128 {
                    assert!(parsed.is_ok(), "depth 128 must parse");
                } else {
                    let position = 128 * open.len();
                    assert_eq!(
                        parsed.unwrap_err(),
                        format!("maximum nesting depth 128 exceeded at position {position}")
                    );
                }
            }
        }
    }
}

// ---- cwd resolution for restore ----

#[test]
fn resolve_cwd_keeps_an_existing_directory() {
    let existing = std::env::temp_dir();
    let home = PathBuf::from("/some/home");
    let resolved = resolve_cwd(existing.to_str(), Some(&home));
    assert_eq!(resolved.path.as_deref(), Some(existing.as_path()));
    assert!(!resolved.stale);
}

#[test]
fn resolve_cwd_falls_back_to_home_when_directory_is_gone() {
    let missing = "/definitely/not/a/real/directory/odytty-wp2-marker";
    let home = std::env::temp_dir();
    let resolved = resolve_cwd(Some(missing), Some(&home));
    assert_eq!(
        resolved.path.as_deref(),
        Some(home.as_path()),
        "a stale dir lands at home"
    );
    assert!(
        resolved.stale,
        "a captured-but-missing dir is flagged stale"
    );
}

#[test]
fn resolve_cwd_unknown_is_a_quiet_home_fallback() {
    let home = std::env::temp_dir();
    let resolved = resolve_cwd(None, Some(&home));
    assert_eq!(resolved.path.as_deref(), Some(home.as_path()));
    assert!(
        !resolved.stale,
        "an unknown (never-captured) cwd is a quiet home fallback, not stale"
    );
}

#[test]
fn resolve_cwd_without_a_home_spawns_in_place() {
    let missing = "/definitely/not/a/real/directory/odytty-wp2-marker";
    let resolved = resolve_cwd(Some(missing), None);
    assert_eq!(resolved.path, None, "no home => let the shell inherit cwd");
    assert!(resolved.stale);
}

#[test]
fn validate_interactive_cwd_keeps_an_existing_directory() {
    // A tracked cwd that exists on disk seeds the spawn as-is.
    let existing = std::env::temp_dir();
    let home = PathBuf::from("/some/home");
    let cwd = validate_interactive_cwd(existing.to_str(), Some(&home));
    assert_eq!(cwd.as_deref(), Some(existing.as_path()));
}

#[test]
fn validate_interactive_cwd_rejects_a_bogus_path_and_falls_back_to_home() {
    // A missing ordinary path falls back to home on every platform. Network
    // paths are covered by a no-probe Windows fixture below.
    let home = std::env::temp_dir();
    for bogus in ["/HKLM:/SOFTWARE", "/definitely/missing/odytty-cwd-fixture"] {
        let cwd = validate_interactive_cwd(Some(bogus), Some(&home));
        assert_eq!(
            cwd.as_deref(),
            Some(home.as_path()),
            "bogus cwd {bogus:?} must fall back to home"
        );
    }
}

#[test]
fn validate_interactive_cwd_unknown_stays_none_for_the_default_dir() {
    // Unlike the restore path, an unknown interactive cwd is left as None so
    // New Tab / Duplicate / New Window spawn in the default directory unchanged,
    // NOT forced into home.
    let home = std::env::temp_dir();
    assert_eq!(validate_interactive_cwd(None, Some(&home)), None);
}

#[test]
fn validate_interactive_cwd_without_a_home_drops_a_bogus_cwd() {
    // With no home to fall back to, a bogus cwd yields None (spawn in place)
    // rather than passing the bogus path through.
    let bogus = "/definitely/not/a/real/directory/odytty-d1-marker";
    assert_eq!(validate_interactive_cwd(Some(bogus), None), None);
}

/// A workspace's bound host alias round-trips through the snapshot and
/// serializes under the stable `default_profile` key.
#[test]
fn workspace_default_profile_round_trips() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "remote".to_owned(),
            default_profile: Some("prod-web".to_owned()),
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout: leaf(None),
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    assert!(
        text.contains("\"default_profile\": \"prod-web\""),
        "binding must serialize under default_profile: {text}"
    );
    let parsed = ShapeSnapshot::from_json_str(&text).expect("round-trips");
    assert_eq!(parsed, snapshot);
    assert_eq!(
        parsed.workspaces[0].default_profile.as_deref(),
        Some("prod-web")
    );
}

/// Forward-compat: a snapshot written before host binding (no `default_profile` field)
/// parses to an unbound workspace, not an error - the snapshot format's
/// unknown-field tolerance covers the new optional key.
#[test]
fn snapshot_without_default_profile_parses_to_unbound() {
    let legacy = r#"{
        "version": 1,
        "active_workspace": 0,
        "workspaces": [
            { "name": "w", "active_tab": 0, "tabs": [
                { "title": null, "focused_leaf": 0, "layout": { "leaf": { "cwd": null } } }
            ] }
        ]
    }"#;
    let parsed = ShapeSnapshot::from_json_str(legacy).expect("legacy snapshot still parses");
    assert_eq!(
        parsed.workspaces[0].default_profile, None,
        "a missing default_profile is an unbound workspace"
    );
}

/// A pane's detached session-host id round-trips through the snapshot
/// under the stable `session_host_id` key, and an older snapshot (no such key)
/// parses to a plain local pane.
#[test]
fn pane_session_host_id_round_trips_and_is_forward_compatible() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Leaf {
                    cwd: Some("/srv".to_owned()),
                    session_host_id: Some("odytty-4f2a".to_owned()),
                    remote_host: None,
                    launch_profile: None,
                    read_only: false,
                },
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    assert!(
        text.contains("\"session_host_id\": \"odytty-4f2a\""),
        "{text}"
    );
    assert_eq!(
        ShapeSnapshot::from_json_str(&text).expect("round-trips"),
        snapshot
    );

    let legacy = r#"{ "version": 1, "active_workspace": 0, "workspaces": [
        { "name": "w", "active_tab": 0, "tabs": [
            { "title": null, "focused_leaf": 0, "layout": { "leaf": { "cwd": null } } }
        ] } ] }"#;
    let parsed = ShapeSnapshot::from_json_str(legacy).expect("legacy parses");
    match &parsed.workspaces[0].tabs[0].layout {
        PaneShape::Leaf {
            session_host_id, ..
        } => assert_eq!(*session_host_id, None),
        other => panic!("expected a leaf, got {other:?}"),
    }
}

/// Per-pane named launch profile round-trips through the snapshot and legacy
/// snapshots without the key parse to `None`.
#[test]
fn pane_launch_profile_round_trips_and_is_forward_compatible() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Leaf {
                    cwd: Some("/work".to_owned()),
                    session_host_id: None,
                    remote_host: None,
                    launch_profile: Some("dev".to_owned()),
                    read_only: false,
                },
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    assert!(text.contains("\"launch_profile\": \"dev\""), "{text}");
    assert_eq!(
        ShapeSnapshot::from_json_str(&text).expect("round-trips"),
        snapshot
    );

    let legacy = r#"{ "version": 1, "active_workspace": 0, "workspaces": [
        { "name": "w", "active_tab": 0, "tabs": [
            { "title": null, "focused_leaf": 0, "layout": { "leaf": { "cwd": null } } }
        ] } ] }"#;
    let parsed = ShapeSnapshot::from_json_str(legacy).expect("legacy parses");
    match &parsed.workspaces[0].tabs[0].layout {
        PaneShape::Leaf { launch_profile, .. } => assert_eq!(*launch_profile, None),
        other => panic!("expected a leaf, got {other:?}"),
    }
}

/// A pane's remote host round-trips through the snapshot under
/// the stable `remote_host` key, and a pre-RESTORE-REMOTE snapshot (no such
/// key) parses to a plain local pane.
#[test]
fn pane_remote_host_round_trips_and_is_forward_compatible() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Leaf {
                    cwd: Some("/home/me".to_owned()),
                    session_host_id: None,
                    remote_host: Some("prod".to_owned()),
                    launch_profile: None,
                    read_only: false,
                },
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    assert!(text.contains("\"remote_host\": \"prod\""), "{text}");
    assert_eq!(
        ShapeSnapshot::from_json_str(&text).expect("round-trips"),
        snapshot
    );

    // A snapshot written before the remote_host key existed loads as a local
    // pane (a missing key parses to None, not an error).
    let legacy = r#"{ "version": 1, "active_workspace": 0, "workspaces": [
        { "name": "w", "active_tab": 0, "tabs": [
            { "title": null, "focused_leaf": 0, "layout": { "leaf": { "cwd": "/srv", "session_host_id": null } } }
        ] } ] }"#;
    let parsed = ShapeSnapshot::from_json_str(legacy).expect("legacy parses");
    match &parsed.workspaces[0].tabs[0].layout {
        PaneShape::Leaf { remote_host, .. } => assert_eq!(*remote_host, None),
        other => panic!("expected a leaf, got {other:?}"),
    }
}

/// Layout name sanitization blocks path traversal and empty names while
/// keeping ordinary names intact.
#[test]
fn sanitize_layout_name_blocks_traversal_and_empties() {
    assert_eq!(
        sanitize_layout_name("My Work-1"),
        Some("My Work-1".to_owned())
    );
    assert_eq!(
        sanitize_layout_name("  spaced  "),
        Some("spaced".to_owned())
    );
    // Path separators, dots, and null bytes are neutralized to underscores, so
    // no traversal segment survives.
    assert_eq!(
        sanitize_layout_name("../etc/passwd"),
        Some("___etc_passwd".to_owned())
    );
    assert_eq!(sanitize_layout_name(".."), Some("__".to_owned()));
    assert_eq!(sanitize_layout_name("a/b\\c"), Some("a_b_c".to_owned()));
    assert_eq!(sanitize_layout_name("   "), None);
    assert_eq!(sanitize_layout_name(""), None);
}

/// Windows reserved device stems are mangled cross-platform so a layout
/// file can never resolve to a device (`CON.json` opens the console on Windows).
#[test]
fn sanitize_layout_name_mangles_windows_reserved_device_stems() {
    // Bare reserved names (any case) get an underscore prefix.
    assert_eq!(sanitize_layout_name("CON"), Some("_CON".to_owned()));
    assert_eq!(sanitize_layout_name("nul"), Some("_nul".to_owned()));
    assert_eq!(sanitize_layout_name("Aux"), Some("_Aux".to_owned()));
    assert_eq!(sanitize_layout_name("COM1"), Some("_COM1".to_owned()));
    assert_eq!(sanitize_layout_name("LPT9"), Some("_LPT9".to_owned()));
    // A reserved stem with an extension is still reserved on Windows -- but the
    // dot is neutralized to `_` first, so "con.bak" becomes the non-device
    // "con_bak" and needs no mangling.
    assert_eq!(sanitize_layout_name("con.bak"), Some("con_bak".to_owned()));
    // Zero-numbered ports follow the conservative shared profile rule.
    assert_eq!(sanitize_layout_name("COM0"), Some("_COM0".to_owned()));
    assert_eq!(sanitize_layout_name("LPT0"), Some("_LPT0".to_owned()));
    // Non-reserved lookalikes are left untouched.
    assert_eq!(sanitize_layout_name("console"), Some("console".to_owned()));
    assert_eq!(sanitize_layout_name("COM10"), Some("COM10".to_owned()));
}

/// The startup sweep removes crash-orphaned `.<...>.tmp` siblings that are
/// older than the stale threshold, while keeping fresh temporaries and any
/// non-temp files untouched.
#[test]
fn sweep_stale_temp_siblings_removes_only_aged_temporaries() {
    use std::time::{Duration, SystemTime};
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let orphan = dir.join(".shape.json.1234.567.0.tmp");
    let keep_json = dir.join("layout.json");
    std::fs::write(&orphan, b"partial").expect("write orphan");
    std::fs::write(&keep_json, b"{}").expect("write json");

    // A `now` far in the future makes every file exceed the threshold, so the
    // matching temp is swept while the non-temp file survives.
    let future = SystemTime::now() + Duration::from_secs(24 * 60 * 60);
    super::sweep_stale_temp_siblings_at(&dir, future, Duration::from_secs(60 * 60));
    assert!(!orphan.exists(), "an aged .tmp sibling is swept");
    assert!(keep_json.exists(), "a non-temp file is never swept");

    // A fresh temp (now == its mtime) is below the threshold and preserved.
    let fresh = dir.join(".shape.json.9999.111.0.tmp");
    std::fs::write(&fresh, b"in-flight").expect("write fresh");
    super::sweep_stale_temp_siblings_at(&dir, SystemTime::now(), Duration::from_secs(60 * 60));
    assert!(fresh.exists(), "a fresh in-flight temp is left alone");
}

/// A layout saves, lists, loads (equal to what was saved), and deletes -
/// exercised against an explicit temp directory so the real state dir is never
/// touched.
#[test]
fn layout_save_list_load_delete_round_trip() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");

    let layout = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "dev".to_owned(),
            default_profile: Some("edge".to_owned()),
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: Some("build".to_owned()),
                focused_leaf: 0,
                layout: leaf(Some("/home/tester")),
            }],
        }],
    };

    let stem = save_layout_in(&dir, "dev", &layout).expect("save");
    assert_eq!(stem, "dev");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(&dir)
                .expect("layouts metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "layouts is an owner-private leaf"
        );
        assert_eq!(
            std::fs::metadata(dir.join("dev.json"))
                .expect("layout metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "direct layout JSON is owner-private"
        );
    }
    assert_eq!(list_layout_names_in(&dir).names, vec!["dev".to_owned()]);

    match load_layout_in(&dir, "dev") {
        LoadOutcome::Loaded(loaded) => assert_eq!(loaded, layout),
        other => panic!("expected Loaded, got {other:?}"),
    }

    delete_layout_in(&dir, "dev").expect("delete");
    assert!(
        list_layout_names_in(&dir).names.is_empty(),
        "layout removed"
    );
    assert!(matches!(load_layout_in(&dir, "dev"), LoadOutcome::Absent));
    // Deleting a missing layout is a success no-op.
    delete_layout_in(&dir, "dev").expect("idempotent delete");
}

/// Ordinary names collide after the same sanitization the writer uses.
#[test]
fn layout_exists_matches_the_writer_stem() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");

    let layout = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout: leaf(None),
            }],
        }],
    };

    // Absent before any write.
    assert!(!layout_exists_in(&dir, "My Layout"));
    // An unusable name can never collide.
    assert!(!layout_exists_in(&dir, "   "));

    let stem = save_layout_in(&dir, "My/Layout", &layout).expect("save");
    // "My/Layout" and "My_Layout" sanitize to the same stem, so both report the
    // written file as existing.
    assert_eq!(stem, "My_Layout");
    assert!(layout_exists_in(&dir, "My/Layout"));
    assert!(layout_exists_in(&dir, "My_Layout"));
    // A genuinely different name does not collide.
    assert!(!layout_exists_in(&dir, "Other"));
}

#[cfg(unix)]
#[test]
fn layout_read_repairs_only_its_target_and_leaves_unknown_entries_alone() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    crate::state_dir::prepare_private_dir(&dir).expect("prepare layouts leaf");
    let direct_json = dir.join("legacy.json");
    std::fs::write(&direct_json, "{}\n").expect("seed direct json");
    std::fs::set_permissions(&direct_json, std::fs::Permissions::from_mode(0o644))
        .expect("chmod direct json");
    let unknown = dir.join("notes.txt");
    std::fs::write(&unknown, "keep\n").expect("seed unknown entry");
    std::fs::set_permissions(&unknown, std::fs::Permissions::from_mode(0o644))
        .expect("chmod unknown entry");
    let nested = dir.join("nested");
    std::fs::create_dir(&nested).expect("seed nested directory");
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755))
        .expect("chmod nested directory");

    assert!(matches!(
        load_layout_in(&dir, "legacy"),
        LoadOutcome::Corrupt(_)
    ));

    assert_eq!(
        std::fs::metadata(&direct_json)
            .expect("direct json metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&unknown)
            .expect("unknown metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "unknown state entries are not recursively normalized"
    );
    assert_eq!(
        std::fs::metadata(&nested)
            .expect("nested metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "nested directories are left alone"
    );
}

// ---- malformed-snapshot launch robustness (belt-and-suspenders) ----
//
// A corrupt / truncated / version-skewed / garbage `workspaces.json` must never
// panic launch. The launch reader (`load_snapshot`) reads the file with
// `read_to_string` then classifies via `ShapeSnapshot::from_json_str` into a
// non-fatal `LoadOutcome`, so `restore_workspaces_on_launch` silently falls
// back to a fresh session on anything but a clean parse. A launch-time panic
// here would brick startup until the file was deleted by hand, so these lock
// the degrade contract at the classify boundary every launch funnels through.
// Platform-neutral: the path, read, and parse are byte-identical on Windows
// (the state dir has a tested `%LOCALAPPDATA%` arm, §10.8) - no OS surface.

#[test]
fn truncated_snapshot_never_panics_and_degrades() {
    // A write interrupted at any point (crash / power loss) leaves a partial
    // file. Cutting a valid serialization at EVERY byte offset simulates that:
    // none may panic, and a genuinely mid-structure cut classifies as Malformed
    // (a cut that happens to drop only trailing whitespace still parses cleanly,
    // which is equally fine - the contract is "never panic", not "always fail").
    let full = sample_snapshot().to_json_pretty();
    for cut in 0..=full.len() {
        if !full.is_char_boundary(cut) {
            continue;
        }
        // The call returning at all (rather than unwinding) is the invariant a
        // panic would violate; the classification value is unused here.
        let _ = ShapeSnapshot::from_json_str(&full[..cut]);
    }
    // A representative mid-structure truncation is explicitly a soft Malformed.
    assert!(matches!(
        ShapeSnapshot::from_json_str(&full[..full.len() / 2]),
        Err(LoadError::Malformed(_))
    ));
}

#[test]
fn well_formed_json_of_the_wrong_shape_is_malformed_never_panics() {
    // Valid JSON whose SHAPE violates the schema (wrong types, missing required
    // structure) must classify as Malformed rather than parse to a half-built
    // snapshot or panic. Only genuinely-required structure is asserted here:
    // a missing `name`/`tabs`/`active_tab`/`ratio` is deliberately tolerated
    // (forward-compat), so those are covered by the round-trip tests instead.
    for text in [
        // `version` present but not a number.
        r#"{ "version": "one", "workspaces": [] }"#,
        // `workspaces` is an object, not the required array.
        r#"{ "version": 1, "workspaces": {} }"#,
        // A tab with no `layout` at all.
        r#"{ "version": 1, "workspaces": [ { "name": "w", "active_tab": 0,
             "tabs": [ { "focused_leaf": 0 } ] } ] }"#,
        // A split node missing its `second` branch.
        r#"{ "version": 1, "workspaces": [ { "name": "w", "active_tab": 0,
             "tabs": [ { "focused_leaf": 0, "layout": { "split": {
               "axis": "columns", "ratio": 0.5, "first": { "leaf": {} } } } } ] } ] }"#,
        // A split node with an unrecognized axis.
        r#"{ "version": 1, "workspaces": [ { "name": "w", "active_tab": 0,
             "tabs": [ { "focused_leaf": 0, "layout": { "split": {
               "axis": "diagonal", "ratio": 0.5,
               "first": { "leaf": {} }, "second": { "leaf": {} } } } } ] } ] }"#,
        // A pane node that is neither a leaf nor a split.
        r#"{ "version": 1, "workspaces": [ { "name": "w", "active_tab": 0,
             "tabs": [ { "focused_leaf": 0, "layout": { "frobnicate": {} } } ] } ] }"#,
    ] {
        match ShapeSnapshot::from_json_str(text) {
            Err(LoadError::Malformed(_)) => {}
            other => panic!("wrong-shape input {text:?} must be Malformed, got {other:?}"),
        }
    }
}

#[test]
fn older_and_newer_schema_versions_are_soft_skew() {
    // Any version this build does not speak - older OR newer - is a soft skew
    // (ignore + fresh launch + notice), never a hard error or panic. The reader
    // never best-effort-parses a foreign schema (§10.4, forward-compat by
    // construction).
    for v in [0u32, 2, 7, 999] {
        let text = format!(r#"{{ "version": {v}, "active_workspace": 0, "workspaces": [] }}"#);
        match ShapeSnapshot::from_json_str(&text) {
            Err(LoadError::VersionSkew { found }) if found == v => {}
            other => panic!("schema version {v} must be a soft skew, got {other:?}"),
        }
    }
}

#[test]
fn garbage_and_broken_files_on_disk_degrade_to_a_soft_outcome_never_panic() {
    // The launch reader `load_snapshot` reads the on-disk file with
    // `read_to_string` then classifies with `from_json_str`. `load_layout_in`
    // shares that exact read -> classify path (it is also the named-layout parse
    // path) but takes an injectable directory, so it exercises the identical
    // file-level degradation without ever touching the real state dir. Every
    // hostile file below must yield a soft `LoadOutcome` - never a panic.
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let name = "state";
    let path = layout_path_in(&dir, name).expect("a plain name resolves to a path");

    // (d) outright garbage / non-UTF-8 bytes: `read_to_string` fails -> Corrupt.
    std::fs::write(
        &path,
        [0xff, 0xfe, 0x00, 0x01, 0x80, 0x9c, b'{', 0xc3, 0x28],
    )
    .expect("write garbage bytes");
    assert!(
        matches!(load_layout_in(&dir, name), LoadOutcome::Corrupt(_)),
        "non-UTF-8 bytes must degrade to Corrupt, not panic"
    );

    // (a) valid UTF-8 but truncated JSON on disk -> Corrupt (malformed classify).
    std::fs::write(&path, b"{ \"version\": 1, \"workspaces\": [ {").expect("write truncated");
    assert!(matches!(
        load_layout_in(&dir, name),
        LoadOutcome::Corrupt(_)
    ));

    // A wholly empty (0-byte) file -> Corrupt, never a panic.
    std::fs::write(&path, b"").expect("write empty");
    assert!(matches!(
        load_layout_in(&dir, name),
        LoadOutcome::Corrupt(_)
    ));

    // (c) a version-skewed file on disk -> Skew (soft: fresh launch + notice).
    std::fs::write(&path, br#"{ "version": 999, "workspaces": [] }"#).expect("write skew");
    assert!(matches!(
        load_layout_in(&dir, name),
        LoadOutcome::Skew { found: 999 }
    ));
}

// ---- aggregate load budgets (fail closed, no restore spawn) ----

fn single_leaf_tab() -> TabShape {
    TabShape {
        arrangement: Default::default(),
        title: None,
        focused_leaf: 0,
        layout: leaf(None),
    }
}

/// A linear split chain of the requested `depth` (depth 1 == a bare leaf). Its
/// [`PaneShape::depth`] equals `depth` and its leaf count also equals `depth`.
fn linear_split_chain(depth: usize) -> PaneShape {
    let mut node = leaf(None);
    for _ in 1..depth {
        node = PaneShape::Split {
            axis: SplitAxisShape::Rows,
            ratio: 0.5,
            first: Box::new(leaf(None)),
            second: Box::new(node),
        };
    }
    node
}

fn one_workspace(tabs: Vec<TabShape>) -> ShapeSnapshot {
    ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs,
        }],
    }
}

#[test]
fn budget_accepts_realistic_and_at_cap_state() {
    // A rich but realistic snapshot is nowhere near any cap.
    assert!(sample_snapshot().check_budgets().is_ok());

    // Exactly at the tab cap in one workspace is accepted.
    let at_tabs = one_workspace(vec![single_leaf_tab(); MAX_TABS_PER_WORKSPACE]);
    assert!(
        at_tabs.check_budgets().is_ok(),
        "a workspace exactly at the tab cap loads"
    );

    // A pane tree exactly at the depth cap is accepted.
    let at_depth = one_workspace(vec![TabShape {
        arrangement: Default::default(),
        title: None,
        focused_leaf: 0,
        layout: linear_split_chain(MAX_PANE_DEPTH),
    }]);
    assert_eq!(linear_split_chain(MAX_PANE_DEPTH).depth(), MAX_PANE_DEPTH);
    assert!(
        at_depth.check_budgets().is_ok(),
        "a pane tree exactly at the depth cap loads"
    );
}

#[test]
fn budget_rejects_excess_workspaces() {
    let snap = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: (0..=MAX_WORKSPACES)
            .map(|i| WorkspaceShape {
                name: format!("w{i}"),
                default_profile: None,
                launch_profile: None,
                active_tab: 0,
                tabs: vec![single_leaf_tab()],
            })
            .collect(),
    };
    assert!(matches!(snap.check_budgets(), Err(LoadError::Malformed(_))));
}

#[test]
fn budget_rejects_excess_tabs_in_one_workspace() {
    let snap = one_workspace(vec![single_leaf_tab(); MAX_TABS_PER_WORKSPACE + 1]);
    assert!(matches!(snap.check_budgets(), Err(LoadError::Malformed(_))));
}

#[test]
fn budget_rejects_pane_tree_deeper_than_the_cap() {
    let snap = one_workspace(vec![TabShape {
        arrangement: Default::default(),
        title: None,
        focused_leaf: 0,
        layout: linear_split_chain(MAX_PANE_DEPTH + 1),
    }]);
    assert!(matches!(snap.check_budgets(), Err(LoadError::Malformed(_))));
}

#[test]
fn budget_rejects_total_leaves_over_the_spawn_ceiling() {
    // Spread leaves so neither the workspace, tab, nor depth cap trips first -
    // only the aggregate leaf ceiling. 512 tabs * 17 leaves = 8704 > 8192, with
    // tab count exactly at its cap and depth (17) well under its cap.
    let tab = TabShape {
        arrangement: Default::default(),
        title: None,
        focused_leaf: 0,
        layout: linear_split_chain(17),
    };
    let snap = one_workspace(vec![tab; MAX_TABS_PER_WORKSPACE]);
    assert!(matches!(snap.check_budgets(), Err(LoadError::Malformed(_))));
}

#[test]
fn over_budget_snapshot_from_json_degrades_to_malformed() {
    // The budget check runs inside from_json_str, so an over-cap file that
    // parses cleanly still classifies as Malformed (fail closed) rather than
    // yielding a restorable snapshot.
    let snap = one_workspace(vec![single_leaf_tab(); MAX_TABS_PER_WORKSPACE + 1]);
    let text = snap.to_json_pretty();
    assert!(matches!(
        ShapeSnapshot::from_json_str(&text),
        Err(LoadError::Malformed(_))
    ));
}

#[test]
fn oversized_state_file_is_rejected_before_parsing() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let name = "state";
    let path = layout_path_in(&dir, name).expect("a plain name resolves to a path");

    // One byte over the cap -> Corrupt (fail closed), never a full read+parse.
    let oversized = vec![b' '; (MAX_SNAPSHOT_BYTES as usize) + 1];
    std::fs::write(&path, &oversized).expect("write oversized state");
    assert!(
        matches!(load_layout_in(&dir, name), LoadOutcome::Corrupt(_)),
        "a state file over the byte budget must degrade to Corrupt, not be read+parsed"
    );
}

#[test]
fn sensitive_state_reader_enforces_the_limit_during_the_read() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("state.json");

    std::fs::write(&path, b"12345678").expect("write exact-limit state");
    assert_eq!(
        read_sensitive_to_string_with_limit(&path, 8).expect("exact limit is accepted"),
        "12345678"
    );

    std::fs::write(&path, b"123456789").expect("write over-limit state");
    let error = read_sensitive_to_string_with_limit(&path, 8)
        .expect_err("one byte over the limit is rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("8-byte load budget"));
}

fn snapshot_with(layout: PaneShape) -> ShapeSnapshot {
    ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            launch_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                arrangement: Default::default(),
                title: None,
                focused_leaf: 0,
                layout,
            }],
        }],
    }
}

/// The read-only flag serializes only when set, so a writable pane's JSON is
/// byte-identical to earlier versions, and it round-trips when set.
#[test]
fn read_only_leaf_round_trips_and_writable_leaf_omits_the_key() {
    let writable = snapshot_with(PaneShape::Leaf {
        cwd: None,
        session_host_id: None,
        remote_host: None,
        launch_profile: None,
        read_only: false,
    });
    let text = writable.to_json_pretty();
    assert!(
        !text.contains("read_only"),
        "writable panes keep the old schema"
    );
    assert_eq!(
        ShapeSnapshot::from_json_str(&text).expect("parse"),
        writable
    );

    let locked = snapshot_with(PaneShape::Leaf {
        cwd: None,
        session_host_id: None,
        remote_host: None,
        launch_profile: None,
        read_only: true,
    });
    let text = locked.to_json_pretty();
    assert!(text.contains("\"read_only\": true"));
    assert_eq!(ShapeSnapshot::from_json_str(&text).expect("parse"), locked);
}

/// Older snapshots (no key) and hand edits with a non-boolean value load as
/// writable rather than failing restore.
#[test]
fn missing_or_malformed_read_only_loads_writable() {
    for leaf in [
        r#"{"cwd": null}"#,
        r#"{"cwd": null, "read_only": "yes"}"#,
        r#"{"cwd": null, "read_only": 1}"#,
        r#"{"cwd": null, "read_only": false}"#,
        r#"{"cwd": null, "read_only": true, "future_field": 7}"#,
    ] {
        let text = format!(
            r#"{{"version": 1, "active_workspace": 0, "workspaces": [{{"name": "w", "active_tab": 0, "tabs": [{{"title": null, "focused_leaf": 0, "layout": {{"leaf": {leaf}}}}}]}}]}}"#
        );
        let parsed = ShapeSnapshot::from_json_str(&text).expect("parse");
        let expected = leaf.contains("\"read_only\": true");
        match &parsed.workspaces[0].tabs[0].layout {
            PaneShape::Leaf { read_only, .. } => assert_eq!(*read_only, expected, "{leaf}"),
            other => panic!("expected a leaf, got {other:?}"),
        }
    }
}

#[test]
fn layout_listing_examines_a_bounded_number_of_entries() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-test-");
    let dir = fixture.path().join("state");
    let layout = sample_snapshot();
    save_layout_in(&dir, "dev", &layout).expect("save");
    let small = list_layout_names_in(&dir);
    assert_eq!(small.names, vec!["dev".to_owned()]);
    assert!(!small.truncated);

    for index in 0..MAX_LAYOUT_DIR_SCAN_ENTRIES {
        std::fs::write(dir.join(format!("note{index:04}.txt")), b"x").expect("write other file");
    }
    let large = list_layout_names_in(&dir);
    assert!(
        large.truncated,
        "a directory past the bound reports truncation"
    );
    assert!(large.names.len() <= 1);
}

#[cfg(windows)]
#[test]
fn windows_network_cwd_validation_and_restore_refuse_before_metadata() {
    let home = PathBuf::from(r"C:\fixture");
    for path in [
        r"//fixture.invalid/share",
        r"\\fixture.invalid\share",
        r"\\?\C:\fixture",
        r"\\.\pipe\fixture",
        r"/\fixture.invalid/share",
        r"\/fixture.invalid/share",
    ] {
        // The shared probe seam separately asserts that these prefixes cannot
        // reach metadata, even when a probe would otherwise report success.
        assert_eq!(
            validate_interactive_cwd(Some(path), Some(&home)),
            Some(home.clone())
        );
        let restored = resolve_cwd(Some(path), Some(&home));
        assert_eq!(restored.path, Some(home.clone()));
        assert!(restored.stale);
        assert!(!dir_exists_for_spawn(
            Path::new(path),
            crate::cwd::DirectorySource::Derived
        ));
        assert_eq!(validate_interactive_cwd(Some(path), None), None);
        assert_eq!(resolve_cwd(Some(path), None).path, None);
    }
    let bad_home = Path::new(r"\\fixture.invalid\share");
    assert_eq!(resolve_cwd(None, Some(bad_home)).path, None);
    assert_eq!(
        validate_interactive_cwd(Some("/missing-fixture"), Some(bad_home)),
        None
    );
}

fn layout_identity_scratch_dir(label: &str) -> crate::test_dirs::ScratchDir {
    crate::test_dirs::ScratchDir::new(&format!("odytty-layout-{label}-"))
}

/// A layout delete bound to the file seen when the row was listed refuses a
/// file that was removed, or replaced under the same name, and never reaches a
/// neighbouring layout.
#[test]
fn layout_delete_refuses_a_target_that_changed_after_listing() {
    let fixture = layout_identity_scratch_dir("delete-identity");
    let dir = fixture.path().join("state");
    let layout = sample_snapshot();
    save_layout_in(&dir, "alpha", &layout).expect("save alpha");
    save_layout_in(&dir, "beta", &layout).expect("save beta");
    let alpha = layout_stamp_in(&dir, "alpha").expect("alpha stamp");
    let beta = layout_stamp_in(&dir, "beta").expect("beta stamp");

    // Removed elsewhere: the captured alpha row deletes nothing, beta stays.
    delete_layout_in(&dir, "alpha").expect("external delete");
    assert_eq!(
        delete_layout_if_unchanged_in(&dir, "alpha", alpha).expect("delete gone"),
        LayoutDeleteOutcome::Gone
    );
    assert_eq!(list_layout_names_in(&dir).names, vec!["beta".to_owned()]);

    // Rewritten under the same name with different contents: refused, kept.
    std::fs::write(dir.join("beta.json"), b"{}").expect("rewrite beta");
    assert_eq!(
        delete_layout_if_unchanged_in(&dir, "beta", beta).expect("delete changed"),
        LayoutDeleteOutcome::Changed
    );
    assert!(dir.join("beta.json").exists(), "a changed layout is kept");

    // Unchanged since it was listed: deleted.
    let current = layout_stamp_in(&dir, "beta").expect("current stamp");
    assert_eq!(
        delete_layout_if_unchanged_in(&dir, "beta", current).expect("delete"),
        LayoutDeleteOutcome::Deleted
    );
    assert!(list_layout_names_in(&dir).names.is_empty());
}

/// A file whose stem is not its own sanitized name is not listed, because
/// every layout action addresses the sanitized stem and would reach a
/// different file.
#[test]
fn layout_listing_skips_stems_that_address_a_different_file() {
    let fixture = layout_identity_scratch_dir("stem-roundtrip");
    let dir = fixture.path().join("state");
    let layout = sample_snapshot();
    save_layout_in(&dir, "a_b", &layout).expect("save a_b");
    write_atomic(&dir.join("a.b.json"), &layout.to_json_pretty()).expect("write a.b");
    assert_eq!(list_layout_names_in(&dir).names, vec!["a_b".to_owned()]);
    assert_eq!(layout_stamp_in(&dir, "a.b"), None);
}

#[test]
fn snapshot_version_rejects_fractional_negative_and_overflow_numbers() {
    for version in ["1.9", "1.0000001", "-1", "4294967296", "1e100"] {
        let text = format!(r#"{{"version":{version},"workspaces":[]}}"#);
        assert!(
            matches!(
                ShapeSnapshot::from_json_str(&text),
                Err(LoadError::Malformed(_))
            ),
            "{version}"
        );
    }
}

#[test]
fn snapshot_version_accepts_integral_notation_and_reports_integer_skew() {
    for version in ["1", "1.0", "1e0"] {
        let text = format!(r#"{{"version":{version},"workspaces":[]}}"#);
        assert!(ShapeSnapshot::from_json_str(&text).is_ok(), "{version}");
    }
    for version in [0, 2, u32::MAX] {
        let text = format!(r#"{{"version":{version},"workspaces":[]}}"#);
        assert!(
            matches!(ShapeSnapshot::from_json_str(&text), Err(LoadError::VersionSkew { found }) if found == version)
        );
    }
}

#[test]
fn layout_names_preserve_unicode_and_bound_utf8_bytes() {
    for name in [
        "\u{65e5}\u{672c}",
        "\u{65e5}\u{672c}\u{8a9e}",
        "\u{0645}\u{0644}\u{0641}",
        "\u{0915}\u{093f}",
        "e\u{0301}",
    ] {
        assert_eq!(sanitize_layout_name(name).as_deref(), Some(name));
    }
    let name = "\u{65e5}".repeat(33);
    assert_eq!(sanitize_layout_name(&name).unwrap(), "\u{65e5}".repeat(32));
    let edge = format!("{}\u{65e5}", "a".repeat(95));
    assert_eq!(sanitize_layout_name(&edge).unwrap(), "a".repeat(95));
    assert_ne!(
        sanitize_layout_name("\u{65e5}\u{672c}"),
        sanitize_layout_name("\u{0645}\u{0644}")
    );
}

#[test]
fn layout_names_replace_default_ignorable_scalars() {
    for ch in [
        '\u{ad}',
        '\u{34f}',
        '\u{61c}',
        '\u{115f}',
        '\u{17b4}',
        '\u{180b}',
        '\u{200b}',
        '\u{200e}',
        '\u{200f}',
        '\u{202a}',
        '\u{202b}',
        '\u{202c}',
        '\u{202d}',
        '\u{202e}',
        '\u{2060}',
        '\u{2066}',
        '\u{2067}',
        '\u{2068}',
        '\u{2069}',
        '\u{3164}',
        '\u{feff}',
        '\u{ffa0}',
        '\u{fff0}',
        '\u{1bca0}',
        '\u{1d173}',
        '\u{e0001}',
        '\u{e0100}',
    ] {
        assert_eq!(
            sanitize_layout_name(&format!("visible{ch}name")),
            Some("visible_name".to_owned()),
            "U+{:04X}",
            ch as u32
        );
    }
}

#[test]
fn layout_names_mangle_superscript_windows_device_stems() {
    for prefix in ["COM", "LPT", "com", "lpt"] {
        for digit in ['\u{b9}', '\u{b2}', '\u{b3}'] {
            let name = format!("{prefix}{digit}");
            assert_eq!(sanitize_layout_name(&name), Some(format!("_{name}")));
        }
    }
}

#[test]
fn layout_names_mangle_zero_device_stems_like_profiles() {
    for name in ["COM0", "LPT0", "com0", "lpt0"] {
        assert!(crate::profiles::validate_profile_name(name).is_err());
        assert_eq!(sanitize_layout_name(name), Some(format!("_{name}")));
    }
}

#[test]
fn layout_names_replace_unicode_line_and_paragraph_separators() {
    for ch in ['\u{2028}', '\u{2029}'] {
        assert_eq!(
            sanitize_layout_name(&format!("visible{ch}name")),
            Some("visible_name".to_owned())
        );
    }
}

#[test]
fn layout_version_rejects_fractional_literals_that_round_to_supported_version() {
    for literal in [
        "1.00000000000000000001",
        "0.99999999999999999999",
        "100000000000000000001e-20",
    ] {
        let text = format!(r#"{{"version":{literal},"active_workspace":0,"workspaces":[]}}"#);
        assert!(
            matches!(
                ShapeSnapshot::from_json_str(&text),
                Err(LoadError::Malformed(_))
            ),
            "fractional literal {literal} must not become a supported version"
        );
    }
}

#[test]
fn layout_version_accepts_equivalent_whole_literals_and_preserves_lenient_fields() {
    for literal in ["1", "1.0", "10e-1", "1.00000000000000000000"] {
        let text =
            format!(r#"{{"version":{literal},"active_workspace":0,"workspaces":[],"future":2.}}"#);
        assert!(
            ShapeSnapshot::from_json_str(&text).is_ok(),
            "whole literal {literal}"
        );
    }
}

#[test]
fn legacy_zero_port_layouts_remain_listed_loadable_and_deletable() {
    for name in ["COM0", "LPT0", "com0", "lpt0"] {
        // Separate directories also exercise case-insensitive filesystems.
        let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-legacy-");
        let dir = fixture.path().join("state");
        crate::state_dir::prepare_private_dir(&dir).unwrap();
        let snapshot = sample_snapshot();
        write_atomic(
            &dir.join(format!("{name}.json")),
            &snapshot.to_json_pretty(),
        )
        .unwrap();
        assert_eq!(list_layout_names_in(&dir).names, vec![name]);
        let LoadOutcome::Loaded(loaded) = load_layout_in(&dir, name) else {
            panic!("legacy layout did not load")
        };
        assert_eq!(loaded, snapshot);
        assert!(
            layout_exists_in(&dir, name),
            "legacy entries collide with the portable save name"
        );
        let stamp = layout_stamp_in(&dir, name).unwrap();
        assert_eq!(
            save_layout_in(&dir, name, &snapshot).unwrap(),
            format!("_{name}")
        );
        assert_eq!(
            delete_layout_if_unchanged_in(&dir, name, stamp).unwrap(),
            LayoutDeleteOutcome::Gone
        );
        assert!(dir.join(format!("_{name}.json")).is_file());
        assert!(!dir.join(format!("{name}.json")).exists());
    }
}

#[test]
fn layout_names_keep_joiners_and_variation_selectors_only_after_visible_text() {
    for modifier in ['\u{200c}', '\u{200d}', '\u{fe0e}', '\u{fe0f}'] {
        let name = format!("\u{915}{modifier}\u{937}");
        assert_eq!(sanitize_layout_name(&name), Some(name.clone()));
        assert_eq!(
            sanitize_layout_name(&format!("{modifier}name")),
            Some("_name".to_owned())
        );
        assert_eq!(
            sanitize_layout_name(&format!(" {modifier}name")),
            Some("_name".to_owned())
        );
        assert_eq!(
            sanitize_layout_name(&format!("\u{301}{modifier}name")),
            Some("\u{301}_name".to_owned())
        );
        let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-modifier-");
        let dir = fixture.path().join("state");
        let snapshot = sample_snapshot();
        assert_eq!(save_layout_in(&dir, &name, &snapshot).unwrap(), name);
        assert_eq!(list_layout_names_in(&dir).names, vec![name.clone()]);
        let LoadOutcome::Loaded(loaded) = load_layout_in(&dir, &name) else {
            panic!("modified layout name did not load")
        };
        assert_eq!(loaded, snapshot);
    }
}

#[test]
fn stray_json_directory_does_not_disable_other_layout_operations() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-stray-");
    crate::native::tests::config_env::with_config_base(fixture.path(), true, || {
        let dir = prepared_layouts_dir().unwrap();
        std::fs::create_dir(dir.join("stray.json")).unwrap();
        let snapshot = sample_snapshot();
        assert_eq!(save_layout("valid", &snapshot).unwrap(), "valid");
        assert_eq!(list_layout_names(), vec!["valid"]);
        assert!(layout_exists("valid"));
        let LoadOutcome::Loaded(loaded) = load_layout("valid") else {
            panic!("valid layout did not load")
        };
        assert_eq!(loaded, snapshot);
        assert!(matches!(load_layout("stray"), LoadOutcome::Corrupt(_)));
        delete_layout_in(&dir, "valid").unwrap();
        assert!(dir.join("stray.json").is_dir());
    });
}

#[cfg(unix)]
#[test]
fn stray_json_symlink_is_refused_without_disabling_other_layouts() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-link-");
    crate::native::tests::config_env::with_config_base(fixture.path(), true, || {
        let dir = prepared_layouts_dir().unwrap();
        let external = fixture.path().join("external");
        std::fs::write(&external, "untouched").unwrap();
        std::os::unix::fs::symlink(&external, dir.join("stray.json")).unwrap();
        save_layout("valid", &sample_snapshot()).unwrap();
        assert_eq!(list_layout_names(), vec!["valid"]);
        assert!(matches!(load_layout("stray"), LoadOutcome::Corrupt(_)));
        assert!(delete_layout_in(&dir, "stray").is_err());
        assert_eq!(std::fs::read_to_string(external).unwrap(), "untouched");
    });
}

#[test]
fn gone_legacy_layout_cannot_retarget_delete_to_its_portable_name() {
    let fixture = crate::test_dirs::ScratchDir::new("odytty-layout-gone-legacy-");
    let dir = fixture.path().join("state");
    crate::state_dir::prepare_private_dir(&dir).unwrap();
    let snapshot = sample_snapshot();
    write_atomic(&dir.join("COM0.json"), &snapshot.to_json_pretty()).unwrap();
    let stamp = layout_stamp_in(&dir, "COM0").unwrap();
    save_layout_in(&dir, "COM0", &snapshot).unwrap();
    assert!(
        !dir.join("COM0.json").exists(),
        "save already migrated the legacy entry"
    );
    assert_eq!(
        delete_layout_if_unchanged_in(&dir, "COM0", stamp).unwrap(),
        LayoutDeleteOutcome::Gone
    );
    assert!(dir.join("_COM0.json").is_file());
    assert!(matches!(load_layout_in(&dir, "COM0"), LoadOutcome::Absent));
}

#[test]
fn legacy_layout_save_collision_migrates_to_one_safe_entry() {
    let scratch = crate::test_dirs::ScratchDir::new("layout-legacy-collision-");
    let dir = scratch.path().join("state");
    crate::state_dir::prepare_private_dir(&dir).unwrap();
    let snapshot = sample_snapshot();
    write_atomic(&dir.join("COM0.json"), &snapshot.to_json_pretty()).unwrap();
    assert!(layout_exists_in(&dir, "COM0"));
    assert_eq!(save_layout_in(&dir, "COM0", &snapshot).unwrap(), "_COM0");
    assert_eq!(list_layout_names_in(&dir).names, vec!["_COM0"]);
    assert!(!dir.join("COM0.json").exists());
}

#[test]
fn joiner_and_selector_layout_collisions_keep_only_the_new_stored_name() {
    for modifier in ['\u{200c}', '\u{200d}', '\u{fe0e}', '\u{fe0f}'] {
        let scratch = crate::test_dirs::ScratchDir::new("layout-modifier-collision-");
        let dir = scratch.path().join("state");
        let snapshot = sample_snapshot();
        save_layout_in(&dir, "work", &snapshot).unwrap();
        let name = format!("work{modifier}");
        assert!(layout_exists_in(&dir, &name));
        assert_eq!(save_layout_in(&dir, &name, &snapshot).unwrap(), name);
        assert_eq!(list_layout_names_in(&dir).names, vec![name.clone()]);
        assert!(matches!(
            load_layout_in(&dir, &name),
            LoadOutcome::Loaded(_)
        ));
        assert!(layout_exists_in(&dir, "work"));
        save_layout_in(&dir, "work", &snapshot).unwrap();
        assert_eq!(list_layout_names_in(&dir).names, vec!["work"]);
    }
}

#[test]
fn layout_collision_keys_agree_with_sanitization_and_keep_distinct_visible_names() {
    for name in ["\u{200d}work", "COM0\u{200d}", "LPT0\u{fe0f}"] {
        let scratch = crate::test_dirs::ScratchDir::new("layout-collision-sanitization-");
        let dir = scratch.path().join("state");
        let snapshot = sample_snapshot();
        let stem = save_layout_in(&dir, name, &snapshot).unwrap();
        assert!(layout_exists_in(&dir, name));
        assert!(layout_exists_in(&dir, &stem));
    }
    let scratch = crate::test_dirs::ScratchDir::new("layout-distinct-names-");
    let dir = scratch.path().join("state");
    save_layout_in(&dir, "work", &sample_snapshot()).unwrap();
    assert!(!layout_exists_in(&dir, "other"));
    assert!(!layout_exists_in(&dir, ""));
}

#[test]
fn incomplete_layout_collision_scan_refuses_to_overwrite_existing_data() {
    let scratch = crate::test_dirs::ScratchDir::new("layout-collision-bound-");
    let dir = scratch.path().join("state");
    let old = sample_snapshot();
    save_layout_in(&dir, "work", &old).unwrap();
    for index in 0..MAX_LAYOUT_DIR_SCAN_ENTRIES {
        std::fs::create_dir(dir.join(format!("noise-{index}"))).unwrap();
    }
    let mut replacement = old.clone();
    replacement.workspaces[0].name = "replacement".to_owned();
    assert!(save_layout_in(&dir, "work", &replacement).is_err());
    assert!(
        matches!(load_layout_in(&dir, "work"), LoadOutcome::Loaded(snapshot) if snapshot == old)
    );
}

#[cfg(unix)]
#[test]
fn unsafe_layout_collision_is_refused_before_any_alias_write() {
    let scratch = crate::test_dirs::ScratchDir::new("layout-unsafe-collision-");
    let dir = scratch.path().join("state");
    crate::state_dir::prepare_private_dir(&dir).unwrap();
    let target = scratch.path().join("target");
    std::fs::write(&target, b"preserve").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("work.json")).unwrap();
    assert!(layout_exists_in(&dir, "work\u{200d}"));
    assert!(save_layout_in(&dir, "work\u{200d}", &sample_snapshot()).is_err());
    assert!(!dir.join("work\u{200d}.json").exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
}

#[test]
fn legacy_layout_can_still_be_deleted_without_a_save() {
    let scratch = crate::test_dirs::ScratchDir::new("layout-legacy-delete-");
    let dir = scratch.path().join("state");
    crate::state_dir::prepare_private_dir(&dir).unwrap();
    write_atomic(&dir.join("LPT0.json"), &sample_snapshot().to_json_pretty()).unwrap();
    let stamp = layout_stamp_in(&dir, "LPT0").unwrap();
    assert!(matches!(
        load_layout_in(&dir, "LPT0"),
        LoadOutcome::Loaded(_)
    ));
    assert_eq!(
        delete_layout_if_unchanged_in(&dir, "LPT0", stamp).unwrap(),
        LayoutDeleteOutcome::Deleted
    );
    assert!(list_layout_names_in(&dir).names.is_empty());
}

#[test]
fn layout_save_checks_preserve_the_filesystems_case_semantics() {
    let scratch = crate::test_dirs::ScratchDir::new("layout-case-collision-");
    let dir = scratch.path().join("state");
    let snapshot = sample_snapshot();
    save_layout_in(&dir, "work", &snapshot).unwrap();
    let same_file = dir.join("Work.json").is_file();
    assert_eq!(layout_exists_in(&dir, "Work"), same_file);
    if same_file {
        save_layout_in(&dir, "Work", &snapshot).unwrap();
        assert_eq!(list_layout_names_in(&dir).names.len(), 1);
    }
    write_atomic(&dir.join("com0.json"), &snapshot.to_json_pretty()).unwrap();
    if dir.join("COM0.json").is_file() {
        assert!(layout_exists_in(&dir, "COM0"));
        save_layout_in(&dir, "COM0", &snapshot).unwrap();
        assert!(!dir.join("com0.json").exists());
        assert!(dir.join("_COM0.json").is_file());
    } else {
        assert!(!layout_exists_in(&dir, "COM0"));
    }
}

#[test]
fn non_ascii_layout_names_keep_joiner_and_selector_identity() {
    for (first, second) in [
        (
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{1f468}\u{1f469}\u{1f467}",
        ),
        ("\u{2764}\u{fe0f}", "\u{2764}\u{fe0e}"),
        (
            "\u{0915}\u{094d}\u{200d}\u{0937}",
            "\u{0915}\u{094d}\u{0937}",
        ),
    ] {
        let scratch = crate::test_dirs::ScratchDir::new("layout-non-ascii-identity-");
        let dir = scratch.path().join("state");
        let original = sample_snapshot();
        let mut other = original.clone();
        other.workspaces[0].name = "second".to_owned();
        save_layout_in(&dir, first, &original).unwrap();
        assert!(
            !layout_exists_in(&dir, second),
            "distinct scalar identities"
        );
        save_layout_in(&dir, second, &other).unwrap();
        assert_eq!(list_layout_names_in(&dir).names.len(), 2);
        assert!(
            matches!(load_layout_in(&dir, first), LoadOutcome::Loaded(value) if value == original)
        );
        assert!(
            matches!(load_layout_in(&dir, second), LoadOutcome::Loaded(value) if value == other)
        );
    }
}
