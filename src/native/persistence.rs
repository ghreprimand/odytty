// SPDX-License-Identifier: GPL-3.0-only
//! Workspace SHAPE persistence — a serializable snapshot of the window's
//! workspace / tab / pane STRUCTURE (design doc §10).
//!
//! What a snapshot captures: workspace names and order, tab titles / count /
//! order, each tab's pane split tree + ratios, and per-pane cwd. What it
//! NEVER captures: terminal grid content, scrollback, environment, or command
//! lines. That exclusion is a hard privacy invariant (the FREEZE-HARDEN rule,
//! §10.1) and a security posture: shape restore always lands a fresh
//! interactive shell at the captured cwd and never re-executes a captured
//! command (the tmux-resurrect footgun, explicit non-goal sub-ODP 8i).
//!
//! Platform-neutral and first-class on Windows (§10.8): the state dir has a
//! tested `%LOCALAPPDATA%` arm (NF13), OSC 7 cwd capture covers PowerShell and
//! drive-letter paths, and the fresh-shell restore path is the only path on
//! Windows (no live session-host to re-attach), so this works identically
//! there.
//!
//! Format is JSON, pretty-printed and hand-editable (§10.4). `serde_json` is
//! not in the dependency tree, so a small self-contained reader/writer (`json`
//! below) handles the fixed, shallow schema — the design sanctions this over
//! adding a dependency for a shape this small.
//!
//! This module is the WP1 infra layer: capture + serialize + atomic write +
//! read-back + version-skew tolerance. Restore-on-launch wiring, the
//! `restore_workspaces` setting, the debounced autosave, and the
//! primary-instance lock are WP2 and consume these types — hence the
//! module-level `dead_code` allow, mirroring the `layout.rs` scaffold
//! precedent.

#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};

use self::json::Json;
use super::layout::{EVEN_RATIO, SplitAxis};
use crate::logging::state_log_dir;

/// Current on-disk schema version. Bumped only on a breaking shape change; a
/// reader that sees a newer version ignores the file and launches fresh rather
/// than erroring or best-effort-parsing (design §10.4, forward-compat by
/// construction).
pub(crate) const SNAPSHOT_VERSION: u32 = 1;

/// Basename of the single whole-app autosave snapshot in the state dir.
const SNAPSHOT_FILE: &str = "workspaces.json";

/// Subdirectory of the state dir that will hold named layouts (WP3). Declared
/// here so the on-disk layout is fixed from WP1; WP3 writes `<name>.json`
/// files under it. Kept separate from the autosave so a corrupt or
/// hand-broken layout can never poison launch restore (design §10.4).
const LAYOUTS_DIR: &str = "layouts";

/// The split axis, mirrored off [`SplitAxis`] so the on-disk schema does not
/// depend on the internal layout enum's representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitAxisShape {
    Columns,
    Rows,
}

impl SplitAxisShape {
    fn as_str(self) -> &'static str {
        match self {
            SplitAxisShape::Columns => "columns",
            SplitAxisShape::Rows => "rows",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "columns" => Some(SplitAxisShape::Columns),
            "rows" => Some(SplitAxisShape::Rows),
            _ => None,
        }
    }

    /// Convert back to the internal layout axis (consumed by WP2 rebuild).
    pub(crate) fn to_split_axis(self) -> SplitAxis {
        match self {
            SplitAxisShape::Columns => SplitAxis::Columns,
            SplitAxisShape::Rows => SplitAxis::Rows,
        }
    }
}

impl From<SplitAxis> for SplitAxisShape {
    fn from(axis: SplitAxis) -> Self {
        match axis {
            SplitAxis::Columns => SplitAxisShape::Columns,
            SplitAxis::Rows => SplitAxisShape::Rows,
        }
    }
}

/// A tab's pane layout, mirrored off the internal `PaneNode` tree but with each
/// leaf carrying its captured cwd (a restorable value) instead of a live,
/// ephemeral session token. A leaf `cwd` of `None` means "unknown" — restore
/// lands that pane at `$HOME` / `%USERPROFILE%` (design §10.5 degrade path).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PaneShape {
    Leaf {
        cwd: Option<String>,
    },
    Split {
        axis: SplitAxisShape,
        ratio: f32,
        first: Box<PaneShape>,
        second: Box<PaneShape>,
    },
}

impl PaneShape {
    /// Number of leaves (panes) in this subtree.
    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            PaneShape::Leaf { .. } => 1,
            PaneShape::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    fn to_json(&self) -> Json {
        match self {
            PaneShape::Leaf { cwd } => Json::obj([("leaf", Json::obj([("cwd", opt_str(cwd))]))]),
            PaneShape::Split {
                axis,
                ratio,
                first,
                second,
            } => Json::obj([(
                "split",
                Json::obj([
                    ("axis", Json::Str(axis.as_str().to_owned())),
                    ("ratio", Json::Num(f64::from(*ratio))),
                    ("first", first.to_json()),
                    ("second", second.to_json()),
                ]),
            )]),
        }
    }

    fn from_json(value: &Json) -> Result<Self, LoadError> {
        if let Some(leaf) = value.get("leaf") {
            Ok(PaneShape::Leaf {
                cwd: leaf.get("cwd").and_then(Json::as_owned_str),
            })
        } else if let Some(split) = value.get("split") {
            let axis = split
                .get("axis")
                .and_then(Json::as_str)
                .and_then(SplitAxisShape::parse)
                .ok_or_else(|| LoadError::malformed("split node missing a valid \"axis\""))?;
            let ratio = split
                .get("ratio")
                .and_then(Json::as_f64)
                .map(|r| r as f32)
                .unwrap_or(EVEN_RATIO);
            let first = split
                .get("first")
                .ok_or_else(|| LoadError::malformed("split node missing \"first\""))
                .and_then(PaneShape::from_json)?;
            let second = split
                .get("second")
                .ok_or_else(|| LoadError::malformed("split node missing \"second\""))
                .and_then(PaneShape::from_json)?;
            Ok(PaneShape::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            })
        } else {
            Err(LoadError::malformed(
                "pane node is neither a \"leaf\" nor a \"split\"",
            ))
        }
    }
}

/// One tab: its optional user title override, the tree-order index of the
/// focused pane, and its pane layout.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TabShape {
    pub(crate) title: Option<String>,
    pub(crate) focused_leaf: usize,
    pub(crate) layout: PaneShape,
}

impl TabShape {
    fn to_json(&self) -> Json {
        Json::obj([
            ("title", opt_str(&self.title)),
            ("focused_leaf", Json::Num(self.focused_leaf as f64)),
            ("layout", self.layout.to_json()),
        ])
    }

    fn from_json(value: &Json) -> Result<Self, LoadError> {
        let layout = value
            .get("layout")
            .ok_or_else(|| LoadError::malformed("tab missing \"layout\""))
            .and_then(PaneShape::from_json)?;
        Ok(TabShape {
            title: value.get("title").and_then(Json::as_owned_str),
            focused_leaf: value
                .get("focused_leaf")
                .and_then(Json::as_usize)
                .unwrap_or(0),
            layout,
        })
    }
}

/// One workspace: its name, the index of its active tab, and its ordered tabs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkspaceShape {
    pub(crate) name: String,
    pub(crate) active_tab: usize,
    pub(crate) tabs: Vec<TabShape>,
}

impl WorkspaceShape {
    fn to_json(&self) -> Json {
        Json::obj([
            ("name", Json::Str(self.name.clone())),
            ("active_tab", Json::Num(self.active_tab as f64)),
            (
                "tabs",
                Json::Arr(self.tabs.iter().map(TabShape::to_json).collect()),
            ),
        ])
    }

    fn from_json(value: &Json) -> Result<Self, LoadError> {
        let tabs = match value.get("tabs").and_then(Json::as_array) {
            Some(items) => items
                .iter()
                .map(TabShape::from_json)
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        Ok(WorkspaceShape {
            name: value
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_owned(),
            active_tab: value
                .get("active_tab")
                .and_then(Json::as_usize)
                .unwrap_or(0),
            tabs,
        })
    }
}

/// The whole-window shape: the schema version, the active workspace index, and
/// the ordered workspaces. This is the root serialized to `workspaces.json`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShapeSnapshot {
    pub(crate) version: u32,
    pub(crate) active_workspace: usize,
    pub(crate) workspaces: Vec<WorkspaceShape>,
}

impl ShapeSnapshot {
    fn to_json(&self) -> Json {
        Json::obj([
            ("version", Json::Num(f64::from(self.version))),
            ("active_workspace", Json::Num(self.active_workspace as f64)),
            (
                "workspaces",
                Json::Arr(
                    self.workspaces
                        .iter()
                        .map(WorkspaceShape::to_json)
                        .collect(),
                ),
            ),
        ])
    }

    /// Serialize to pretty-printed, hand-editable JSON with a trailing newline.
    pub(crate) fn to_json_pretty(&self) -> String {
        json::to_pretty(&self.to_json())
    }

    /// Parse a snapshot from JSON text. A newer/unknown `version` is reported as
    /// [`LoadError::VersionSkew`] (ignore + fresh launch, not a hard error);
    /// anything else malformed is [`LoadError::Malformed`].
    pub(crate) fn from_json_str(input: &str) -> Result<ShapeSnapshot, LoadError> {
        let value = json::parse(input).map_err(LoadError::Malformed)?;
        let version = value
            .get("version")
            .and_then(Json::as_f64)
            .ok_or_else(|| LoadError::malformed("missing \"version\""))?;
        let version = version as u32;
        if version != SNAPSHOT_VERSION {
            return Err(LoadError::VersionSkew { found: version });
        }
        let workspaces = match value.get("workspaces").and_then(Json::as_array) {
            Some(items) => items
                .iter()
                .map(WorkspaceShape::from_json)
                .collect::<Result<Vec<_>, _>>()?,
            None => return Err(LoadError::malformed("missing \"workspaces\" array")),
        };
        Ok(ShapeSnapshot {
            version,
            active_workspace: value
                .get("active_workspace")
                .and_then(Json::as_usize)
                .unwrap_or(0),
            workspaces,
        })
    }
}

/// Why a snapshot failed to load. Neither variant is fatal to launch: WP2 falls
/// back to a fresh session (with a one-line notice) in both cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoadError {
    /// The file parsed but declared a schema version this build does not
    /// understand. Ignored with a notice; never a hard error (design §10.4).
    VersionSkew { found: u32 },
    /// The file is not well-formed JSON or is missing required structure.
    Malformed(String),
}

impl LoadError {
    fn malformed(message: &str) -> Self {
        LoadError::Malformed(message.to_owned())
    }
}

/// The four-way outcome of a launch-time load, so WP2 can drive the right
/// behavior without re-implementing the classification.
#[derive(Debug)]
pub(crate) enum LoadOutcome {
    /// A valid snapshot to restore.
    Loaded(ShapeSnapshot),
    /// No snapshot file exists yet (first launch, or restore never saved).
    Absent,
    /// A newer schema version — ignore and launch fresh, with a notice.
    Skew { found: u32 },
    /// Unreadable or malformed — ignore and launch fresh, with a notice.
    Corrupt(String),
}

/// Absolute path of the whole-app autosave snapshot in the state dir.
pub(crate) fn snapshot_path() -> PathBuf {
    state_log_dir().join(SNAPSHOT_FILE)
}

/// Absolute path of the named-layouts directory in the state dir (WP3).
pub(crate) fn layouts_dir() -> PathBuf {
    state_log_dir().join(LAYOUTS_DIR)
}

/// Atomically write `contents` to `path`: write a uniquely-named sibling temp
/// file, flush, then rename it over the target. A crash mid-write can only ever
/// leave the temp file behind (cleaned up on the error path), never a
/// half-written snapshot (design §10.4 / sub-ODP 8c). The rename is atomic and
/// replaces an existing target on both Unix and Windows. Creates the parent
/// directory if absent.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(path);
    std::fs::write(&tmp, contents)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(SNAPSHOT_FILE);
    let tmp_name = format!(".{base}.{}.{nanos}.tmp", std::process::id());
    match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

/// Serialize and atomically write the whole-app snapshot to its state-dir path.
pub(crate) fn save_snapshot(snapshot: &ShapeSnapshot) -> io::Result<()> {
    write_atomic(&snapshot_path(), &snapshot.to_json_pretty())
}

/// Read and classify the whole-app snapshot from its state-dir path.
pub(crate) fn load_snapshot() -> LoadOutcome {
    let path = snapshot_path();
    match std::fs::read_to_string(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => LoadOutcome::Absent,
        Err(err) => LoadOutcome::Corrupt(err.to_string()),
        Ok(text) => match ShapeSnapshot::from_json_str(&text) {
            Ok(snapshot) => LoadOutcome::Loaded(snapshot),
            Err(LoadError::VersionSkew { found }) => LoadOutcome::Skew { found },
            Err(LoadError::Malformed(message)) => LoadOutcome::Corrupt(message),
        },
    }
}

fn opt_str(value: &Option<String>) -> Json {
    match value {
        Some(text) => Json::Str(text.clone()),
        None => Json::Null,
    }
}

mod json;

#[cfg(test)]
mod tests;
