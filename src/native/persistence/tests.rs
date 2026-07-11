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
        remote_host: None,
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
                        layout: leaf(Some("/home/tester")),
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
            active_tab: 0,
            tabs: vec![TabShape {
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
    // A structure nested to a normal-but-non-trivial depth (well inside the
    // 128-frame cap) must still parse cleanly; the guard must not reject real,
    // moderately-nested layouts.
    let depth = 32;
    let text = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    assert!(
        json::parse(&text).is_ok(),
        "a moderately-nested array must still parse"
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
                    remote_host: None,
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

/// RESTORE-REMOTE: a pane's remote host round-trips through the snapshot under
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
            active_tab: 0,
            tabs: vec![TabShape {
                title: None,
                focused_leaf: 0,
                layout: PaneShape::Leaf {
                    cwd: Some("/home/me".to_owned()),
                    session_host_id: None,
                    remote_host: Some("prod".to_owned()),
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
                layout: leaf(Some("/home/tester")),
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

/// OVERWRITE-WARN: `layout_exists_in` reports presence keyed by the SAME
/// sanitized stem the writer uses, so a name that maps onto an existing file is
/// detected even when it differs only in unsanitized characters — the check can
/// never disagree with the writer about which file a name means.
#[test]
fn layout_exists_matches_the_writer_stem() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("odytty-overwrite-{}-{nanos}", std::process::id()));

    let layout = ShapeSnapshot {
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

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- H4: malformed-snapshot launch robustness (belt-and-suspenders) ----
//
// A corrupt / truncated / version-skewed / garbage `workspaces.json` must never
// panic launch. The launch reader (`load_snapshot`) reads the file with
// `read_to_string` then classifies via `ShapeSnapshot::from_json_str` into a
// non-fatal `LoadOutcome`, so `restore_workspaces_on_launch` silently falls
// back to a fresh session on anything but a clean parse. A launch-time panic
// here would brick startup until the file was deleted by hand, so these lock
// the degrade contract at the classify boundary every launch funnels through.
// Platform-neutral: the path, read, and parse are byte-identical on Windows
// (the state dir has a tested `%LOCALAPPDATA%` arm, §10.8) — no OS surface.

#[test]
fn truncated_snapshot_never_panics_and_degrades() {
    // A write interrupted at any point (crash / power loss) leaves a partial
    // file. Cutting a valid serialization at EVERY byte offset simulates that:
    // none may panic, and a genuinely mid-structure cut classifies as Malformed
    // (a cut that happens to drop only trailing whitespace still parses cleanly,
    // which is equally fine — the contract is "never panic", not "always fail").
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
    // Any version this build does not speak — older OR newer — is a soft skew
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
    // hostile file below must yield a soft `LoadOutcome` — never a panic.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("odytty-h4-corrupt-{}-{nanos}", std::process::id()));
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

    let _ = std::fs::remove_dir_all(&dir);
}
