// SPDX-License-Identifier: GPL-3.0-only
//! Round-trip, version-skew, atomic-write, and hand-serializer tests for the
//! workspace shape snapshot. These are model-free (they build `ShapeSnapshot`
//! values directly); the capture-from-the-live-model path is exercised in
//! `tabs_sessions.rs` where headless `App` construction lives.

use super::*;

fn leaf(cwd: Option<&str>) -> PaneShape {
    PaneShape::Leaf {
        cwd: cwd.map(str::to_owned),
        session_host_id: None,
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
                active_tab: 1,
                tabs: vec![
                    TabShape {
                        title: None,
                        focused_leaf: 0,
                        layout: leaf(Some("/home/joel")),
                    },
                    TabShape {
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
                active_tab: 0,
                tabs: vec![TabShape {
                    title: None,
                    focused_leaf: 2,
                    layout: PaneShape::Split {
                        axis: SplitAxisShape::Rows,
                        ratio: 0.35,
                        first: Box::new(leaf(Some(r"C:\Users\joel\project"))),
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
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: leaf(Some(r"C:\Users\joel\Documents")),
            }],
        }],
    };
    let text = snapshot.to_json_pretty();
    // Backslashes must be JSON-escaped on the wire.
    assert!(
        text.contains(r#""cwd": "C:\\Users\\joel\\Documents""#),
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
            active_tab: 0,
            tabs: vec![TabShape {
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
            active_tab: 0,
            tabs: vec![TabShape {
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
fn atomic_write_replaces_target_and_leaves_no_temp_file() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("odytty-wp1-atomic-{}-{nanos}", std::process::id()));
    let path = dir.join("workspaces.json");

    write_atomic(&path, "first\n").expect("first write");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "first\n");

    // Overwrite must replace in place, atomically.
    write_atomic(&path, "second\n").expect("second write");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "second\n");

    // Only the target file remains — no `.tmp` sibling left behind.
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

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn json_escapes_round_trip_through_the_parser() {
    let value = json::parse(r#"{ "s": "a\"b\\c\n\t\/\u0041\uD83D\uDE00" }"#).expect("parse");
    assert_eq!(
        value.get("s").and_then(json::Json::as_str),
        Some("a\"b\\c\n\t/A😀")
    );
}

// ---- WP2: cwd resolution for restore (design §10.5, sub-ODP 8f) ----

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

/// F6-W5: a workspace's bound host alias round-trips through the snapshot and
/// serializes under the stable `default_profile` key.
#[test]
fn workspace_default_profile_round_trips() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "remote".to_owned(),
            default_profile: Some("prod-web".to_owned()),
            active_tab: 0,
            tabs: vec![TabShape {
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

/// Forward-compat: a snapshot written before F6-W5 (no `default_profile` field)
/// parses to an unbound workspace, not an error — WP1's unknown-field tolerance
/// covers the new optional key.
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

/// WP3 / 8h: a pane's detached session-host id round-trips through the snapshot
/// under the stable `session_host_id` key, and a pre-WP3 snapshot (no such key)
/// parses to a plain local pane.
#[test]
fn pane_session_host_id_round_trips_and_is_forward_compatible() {
    let snapshot = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "w".to_owned(),
            default_profile: None,
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Leaf {
                    cwd: Some("/srv".to_owned()),
                    session_host_id: Some("odytty-4f2a".to_owned()),
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

/// WP3: layout name sanitization blocks path traversal and empty names while
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

/// WP3: a layout saves, lists, loads (equal to what was saved), and deletes —
/// exercised against an explicit temp directory so the real state dir is never
/// touched.
#[test]
fn layout_save_list_load_delete_round_trip() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("odytty-wp3-layouts-{}-{nanos}", std::process::id()));

    let layout = ShapeSnapshot {
        version: SNAPSHOT_VERSION,
        active_workspace: 0,
        workspaces: vec![WorkspaceShape {
            name: "dev".to_owned(),
            default_profile: Some("edge".to_owned()),
            active_tab: 0,
            tabs: vec![TabShape {
                title: Some("build".to_owned()),
                focused_leaf: 0,
                layout: leaf(Some("/home/joel")),
            }],
        }],
    };

    let stem = save_layout_in(&dir, "dev", &layout).expect("save");
    assert_eq!(stem, "dev");
    assert_eq!(list_layout_names_in(&dir), vec!["dev".to_owned()]);

    match load_layout_in(&dir, "dev") {
        LoadOutcome::Loaded(loaded) => assert_eq!(loaded, layout),
        other => panic!("expected Loaded, got {other:?}"),
    }

    delete_layout_in(&dir, "dev").expect("delete");
    assert!(list_layout_names_in(&dir).is_empty(), "layout removed");
    assert!(matches!(load_layout_in(&dir, "dev"), LoadOutcome::Absent));
    // Deleting a missing layout is a success no-op.
    delete_layout_in(&dir, "dev").expect("idempotent delete");

    let _ = std::fs::remove_dir_all(&dir);
}
