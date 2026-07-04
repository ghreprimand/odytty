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
        /// The detached session-host id this pane was attached to (WP3 / 8h), or
        /// `None` for a locally-spawned pane. On restore (Unix only), an id whose
        /// session-host is still alive is reattached; a dead id spawns a fresh
        /// shell silently. Never captured on Windows (no detached-session
        /// transport there), and tolerated-absent on pre-WP3 snapshots.
        session_host_id: Option<String>,
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
            PaneShape::Leaf {
                cwd,
                session_host_id,
            } => Json::obj([(
                "leaf",
                Json::obj([
                    ("cwd", opt_str(cwd)),
                    ("session_host_id", opt_str(session_host_id)),
                ]),
            )]),
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
                session_host_id: leaf.get("session_host_id").and_then(Json::as_owned_str),
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
    /// The host alias this workspace is bound to (F6-W5), or `None` for a plain
    /// local workspace. When set, restore re-applies the binding so a New Tab in
    /// the restored workspace routes through the remote connect path again.
    /// Tolerated-absent on old snapshots (WP1 forward-compat): a missing field
    /// parses to `None`, i.e. an unbound workspace.
    pub(crate) default_profile: Option<String>,
    pub(crate) active_tab: usize,
    pub(crate) tabs: Vec<TabShape>,
}

impl WorkspaceShape {
    fn to_json(&self) -> Json {
        Json::obj([
            ("name", Json::Str(self.name.clone())),
            ("default_profile", opt_str(&self.default_profile)),
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
            default_profile: value.get("default_profile").and_then(Json::as_owned_str),
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

/// Sanitize a layout name into a safe single-segment filename stem (WP3 / 8e).
/// Keeps ASCII letters, digits, spaces, `-` and `_`; every other character
/// (path separators, dots, control characters, non-ASCII) becomes `_`. This
/// blocks path traversal (`..`, `/`, `\\`) and hidden-file names by
/// construction, and caps the length. Returns `None` when nothing usable
/// remains, so an empty or all-illegal name never yields a stray file.
pub(crate) fn sanitize_layout_name(name: &str) -> Option<String> {
    let mut out = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
        if out.len() >= 96 {
            break;
        }
    }
    let trimmed = out.trim().to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Absolute path of the layout file for `name` under `dir`, or `None` when the
/// name sanitizes to nothing. The sanitized stem guarantees a single path
/// segment (no traversal).
fn layout_path_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let stem = sanitize_layout_name(name)?;
    Some(dir.join(format!("{stem}.json")))
}

/// Absolute path of the layout file for `name`, or `None` when the name
/// sanitizes to nothing. The file lives directly under [`layouts_dir`].
pub(crate) fn layout_path(name: &str) -> Option<PathBuf> {
    layout_path_in(&layouts_dir(), name)
}

/// Serialize and atomically write a named layout into `dir` (WP3 / 8g core).
fn save_layout_in(dir: &Path, name: &str, snapshot: &ShapeSnapshot) -> io::Result<String> {
    let stem = sanitize_layout_name(name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty layout name"))?;
    let path = dir.join(format!("{stem}.json"));
    write_atomic(&path, &snapshot.to_json_pretty())?;
    Ok(stem)
}

/// Serialize and atomically write a named layout (WP3 / 8g). The layout is a
/// [`ShapeSnapshot`] holding the workspace(s) to instantiate later; reusing the
/// snapshot schema keeps the layout and autosave formats identical (including
/// the W5 `default_profile` binding and per-pane cwd). Returns the sanitized
/// name actually written, or an error when the name is unusable or the write
/// fails.
pub(crate) fn save_layout(name: &str, snapshot: &ShapeSnapshot) -> io::Result<String> {
    save_layout_in(&layouts_dir(), name, snapshot)
}

/// The sorted `*.json` file stems under `dir` (WP3 core). A missing directory is
/// an empty list, never an error; non-`*.json` files are ignored.
fn list_layout_names_in(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    names
}

/// The names of all saved layouts, sorted (WP3). Empty when nothing is saved.
pub(crate) fn list_layout_names() -> Vec<String> {
    list_layout_names_in(&layouts_dir())
}

/// Read and classify a named layout from `dir` (WP3 core).
fn load_layout_in(dir: &Path, name: &str) -> LoadOutcome {
    let Some(path) = layout_path_in(dir, name) else {
        return LoadOutcome::Absent;
    };
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

/// Read and classify a named layout (WP3). Mirrors [`load_snapshot`]'s
/// four-way outcome so the caller degrades a corrupt/skewed layout to a notice
/// instead of instantiating a broken workspace.
pub(crate) fn load_layout(name: &str) -> LoadOutcome {
    load_layout_in(&layouts_dir(), name)
}

/// Delete a named layout from `dir` (WP3 core). A missing file is success.
fn delete_layout_in(dir: &Path, name: &str) -> io::Result<()> {
    let Some(path) = layout_path_in(dir, name) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Delete a named layout (WP3). A missing file is treated as success (the end
/// state — no such layout — is what the caller wanted).
pub(crate) fn delete_layout(name: &str) -> io::Result<()> {
    delete_layout_in(&layouts_dir(), name)
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

/// Where a restored pane should open, plus whether its captured directory was
/// found to be stale. Resolves the design §10.5 degrade path in one place so
/// launch-time first-pane seeding and the workspace rebuild agree exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedCwd {
    /// The directory to spawn the pane's shell in. `None` means "spawn wherever
    /// the process already is" (only when no home is resolvable at all).
    pub(crate) path: Option<PathBuf>,
    /// True when a specific directory WAS captured but no longer resolves to an
    /// existing directory, so the pane falls back to home. Drives the single
    /// compact restore notice (sub-ODP 8f); an unknown (never-captured) cwd is
    /// a quiet home fallback and is NOT counted here.
    pub(crate) stale: bool,
}

/// Resolve a captured pane cwd against the live filesystem (design §10.5,
/// sub-ODP 8f). A captured directory that still exists is used as-is; a captured
/// directory that has since disappeared falls back to `home` and is flagged
/// stale (for the notice); an unknown (`None`) cwd falls back to `home` quietly.
/// Never aborts and never touches anything but a single `metadata` probe.
pub(crate) fn resolve_cwd(captured: Option<&str>, home: Option<&Path>) -> ResolvedCwd {
    match captured {
        Some(dir) if is_existing_dir(Path::new(dir)) => ResolvedCwd {
            path: Some(PathBuf::from(dir)),
            stale: false,
        },
        Some(_) => ResolvedCwd {
            path: home.map(Path::to_path_buf),
            stale: true,
        },
        None => ResolvedCwd {
            path: home.map(Path::to_path_buf),
            stale: false,
        },
    }
}

fn is_existing_dir(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// The user's home directory for restore fallbacks: `$HOME` on Unix,
/// `%USERPROFILE%` on Windows (sub-ODP 8f). `None` when unset/empty, in which
/// case a stale pane simply spawns wherever the process already is rather than
/// aborting.
pub(crate) fn restore_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
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
