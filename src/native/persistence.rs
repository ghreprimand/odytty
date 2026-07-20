// SPDX-License-Identifier: GPL-3.0-only
//! Workspace SHAPE persistence — a serializable snapshot of the window's
//! workspace / tab / pane STRUCTURE (design doc §10).
//!
//! What a snapshot captures: workspace names and order, tab titles / count /
//! order, each tab's pane split tree + ratios, and per-pane cwd. What it
//! NEVER captures: terminal grid content, scrollback, environment, or command
//! lines. That exclusion is a hard privacy invariant (the FREEZE-HARDEN rule,
//! §10.1) and a security posture: a local shape restore lands a fresh
//! interactive shell at the captured cwd and never re-executes a captured
//! command. A live detached host can be reattached, while a captured remote
//! pane reconnects at the remote login shell's default directory.
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

use std::io::{self, Read};
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

/// Aggregate load budgets (PLAUS-01 hardening). Workspace state is owner-written
/// and trusted, but a corrupt or hand-broken file must fail closed to a fresh
/// launch rather than exhaust memory or spawn an unbounded number of restore
/// processes. Each cap sits far above any state the UI can produce yet well
/// below a resource-exhaustion threshold; over-cap classifies as
/// [`LoadError::Malformed`], degrading to a fresh session on the existing
/// corruption-fallback path (a state-only notice, no file contents logged).
/// Platform-neutral: the on-disk format and these bounds are identical on every
/// OS, Windows included.
///
/// Max bytes read from a single state/layout file before parsing. Legitimate
/// snapshots are kilobytes; this bounds both the whole-file read and the
/// parser's `Vec<char>` clone of the input.
const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
/// Max workspaces accepted from one snapshot.
const MAX_WORKSPACES: usize = 512;
/// Max tabs accepted in any one workspace.
const MAX_TABS_PER_WORKSPACE: usize = 512;
/// Max pane-tree nesting depth accepted in any one tab (a leaf is depth 1). Sits
/// under the JSON parser's own nesting-depth guard.
const MAX_PANE_DEPTH: usize = 48;
/// Max total leaves (restorable panes) across the whole snapshot. This bounds
/// the restore batch's process spawns — the restore path spawns one child per
/// leaf, so this is the hard ceiling on that fan-out.
const MAX_TOTAL_LEAVES: usize = 8192;

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
        /// The remote host this pane was connected to (RESTORE-REMOTE), or
        /// `None` for a locally-spawned pane. Holds the saved-profile alias when
        /// the pane was opened from a `hosts.conf` entry (so restore re-resolves
        /// its full per-host config), else the literal `[user@]host[:port]`
        /// destination for an ad-hoc connection. On restore this pane respawns
        /// through the `ssh` connect path — a fresh remote login shell, never a
        /// re-run of any captured command (8i). Serialized as `null` for a local
        /// pane and tolerated-absent on pre-RESTORE-REMOTE snapshots (a missing
        /// key loads as `None`, i.e. a local pane).
        remote_host: Option<String>,
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

    /// Nesting depth of this subtree: a leaf is depth 1, a split is one deeper
    /// than its deeper child. Used to bound restore against a pathologically
    /// deep pane tree (PLAUS-01).
    fn depth(&self) -> usize {
        match self {
            PaneShape::Leaf { .. } => 1,
            PaneShape::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }

    fn to_json(&self) -> Json {
        match self {
            PaneShape::Leaf {
                cwd,
                session_host_id,
                remote_host,
            } => Json::obj([(
                "leaf",
                Json::obj([
                    ("cwd", opt_str(cwd)),
                    ("session_host_id", opt_str(session_host_id)),
                    ("remote_host", opt_str(remote_host)),
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
                remote_host: leaf.get("remote_host").and_then(Json::as_owned_str),
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
        let snapshot = ShapeSnapshot {
            version,
            active_workspace: value
                .get("active_workspace")
                .and_then(Json::as_usize)
                .unwrap_or(0),
            workspaces,
        };
        snapshot.check_budgets()?;
        Ok(snapshot)
    }

    /// Reject a parsed snapshot that exceeds the aggregate load budgets
    /// (PLAUS-01). Returns the first budget violated as a
    /// [`LoadError::Malformed`] so the caller degrades to a fresh launch (fail
    /// closed) before any restore process is spawned, rather than instantiating
    /// a pathological tree. The bounds are generous enough that no legitimate
    /// UI-produced state reaches them. Platform-neutral.
    fn check_budgets(&self) -> Result<(), LoadError> {
        if self.workspaces.len() > MAX_WORKSPACES {
            return Err(LoadError::malformed(
                "workspace count exceeds the load budget",
            ));
        }
        let mut total_leaves: usize = 0;
        for ws in &self.workspaces {
            if ws.tabs.len() > MAX_TABS_PER_WORKSPACE {
                return Err(LoadError::malformed("tab count exceeds the load budget"));
            }
            for tab in &ws.tabs {
                if tab.layout.depth() > MAX_PANE_DEPTH {
                    return Err(LoadError::malformed(
                        "pane nesting depth exceeds the load budget",
                    ));
                }
                total_leaves = total_leaves.saturating_add(tab.layout.leaf_count());
                if total_leaves > MAX_TOTAL_LEAVES {
                    return Err(LoadError::malformed(
                        "total pane count exceeds the load budget",
                    ));
                }
            }
        }
        Ok(())
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
/// file, flush it to disk, then rename it over the target. A crash mid-write can
/// only ever leave the temp file behind (cleaned up on the error path), never a
/// half-written snapshot (design §10.4 / sub-ODP 8c). The temp file's data is
/// `sync_all`'d and the parent directory is fsync'd before returning, so the
/// rename is durable across a power loss — parity with the settings writeback
/// path. The rename is atomic and replaces an existing target on both Unix and
/// Windows. Creates the parent directory if absent.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    // Session state is owner-private; route through the shared policy-driven
    // writer under the Sensitive policy (private-dir prep, exclusive 0600 temp,
    // owner/regular-file target repair, data + directory fsync).
    crate::state_dir::write_atomic(
        path,
        contents.as_bytes(),
        crate::state_dir::WriteMode::Sensitive,
    )
}

/// Serialize and atomically write the whole-app snapshot to its state-dir path.
pub(crate) fn save_snapshot(snapshot: &ShapeSnapshot) -> io::Result<()> {
    let dir = crate::logging::prepare_state_log_dir()?;
    write_atomic(&dir.join(SNAPSHOT_FILE), &snapshot.to_json_pretty())
}

fn prepared_layouts_dir() -> io::Result<PathBuf> {
    let dir = crate::logging::prepare_state_log_dir()?.join(LAYOUTS_DIR);
    crate::state_dir::prepare_private_dir(&dir)?;
    repair_direct_layout_files(&dir)?;
    Ok(dir)
}

/// Repair only the known JSON files directly inside `layouts`.  Deliberately do
/// not recurse: unknown files, socket objects, and nested directories are not
/// OdyTTY persistence data and remain untouched.
fn repair_direct_layout_files(dir: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            crate::state_dir::repair_existing_sensitive(&path)?;
        }
    }
    Ok(())
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
        return None;
    }
    // C26: Windows reserved device stems (CON, PRN, AUX, NUL, COM1-9, LPT1-9)
    // are illegal filenames on Windows EVEN WITH an extension -- "CON.json"
    // resolves to the console device, not a file. Mangle them cross-platform so
    // a layout saved on any OS is portable and can never open a device.
    if is_windows_reserved_stem(&trimmed) {
        return Some(format!("_{trimmed}"));
    }
    Some(trimmed)
}

/// Whether `stem` (a filename stem, no extension) collides with a Windows
/// reserved device name, compared case-insensitively against the segment before
/// any dot. `create_temp_sibling` and `sanitize_layout_name` both keep files off
/// these names so persistence stays portable to Windows.
fn is_windows_reserved_stem(stem: &str) -> bool {
    let head = stem.split('.').next().unwrap_or(stem).trim();
    let upper = head.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    // COM1-COM9 and LPT1-LPT9 (COM0 / LPT0 are not reserved).
    (upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && matches!(upper.as_bytes()[3], b'1'..=b'9')
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
    save_layout_in(&prepared_layouts_dir()?, name, snapshot)
}

/// Whether a saved layout already exists for `name` in `dir` (OVERWRITE-WARN).
/// Keyed by the SAME sanitized stem [`save_layout_in`] writes, so the existence
/// check can never disagree with the writer about which file a name maps to. An
/// unusable name (nothing survives sanitization) can never collide, so it is
/// reported absent.
fn layout_exists_in(dir: &Path, name: &str) -> bool {
    layout_path_in(dir, name)
        .is_some_and(|path| crate::state_dir::open_existing_sensitive(&path).is_ok())
}

/// Whether a saved layout already exists for `name` (OVERWRITE-WARN). The save
/// paths call this before writing so a name collision can prompt the user
/// (replace vs. a different name) instead of silently clobbering.
pub(crate) fn layout_exists(name: &str) -> bool {
    prepared_layouts_dir()
        .ok()
        .is_some_and(|dir| layout_exists_in(&dir, name))
}

/// The sorted `*.json` file stems under `dir` (WP3 core). A missing directory is
/// an empty list, never an error; non-`*.json` files are ignored.
fn list_layout_names_in(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if crate::state_dir::validate_private_dir(dir).is_err() {
        return names;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if crate::state_dir::open_existing_sensitive(&path).is_err() {
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
    prepared_layouts_dir()
        .map(|dir| list_layout_names_in(&dir))
        .unwrap_or_default()
}

/// Read and classify a named layout from `dir` (WP3 core).
fn load_layout_in(dir: &Path, name: &str) -> LoadOutcome {
    let Some(path) = layout_path_in(dir, name) else {
        return LoadOutcome::Absent;
    };
    match read_sensitive_to_string(&path) {
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
    match prepared_layouts_dir() {
        Ok(dir) => load_layout_in(&dir, name),
        Err(_) => LoadOutcome::Corrupt("secure layout state unavailable".to_owned()),
    }
}

/// Delete a named layout from `dir` (WP3 core). A missing file is success.
fn delete_layout_in(dir: &Path, name: &str) -> io::Result<()> {
    let Some(path) = layout_path_in(dir, name) else {
        return Ok(());
    };
    crate::state_dir::repair_existing_sensitive(&path)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Delete a named layout (WP3). A missing file is treated as success (the end
/// state — no such layout — is what the caller wanted).
pub(crate) fn delete_layout(name: &str) -> io::Result<()> {
    delete_layout_in(&prepared_layouts_dir()?, name)
}

/// Read and classify the whole-app snapshot from its state-dir path.
pub(crate) fn load_snapshot() -> LoadOutcome {
    let path = match crate::logging::prepare_state_log_dir() {
        Ok(dir) => dir.join(SNAPSHOT_FILE),
        Err(_) => return LoadOutcome::Corrupt("secure workspace state unavailable".to_owned()),
    };
    match read_sensitive_to_string(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => LoadOutcome::Absent,
        Err(err) => LoadOutcome::Corrupt(err.to_string()),
        Ok(text) => match ShapeSnapshot::from_json_str(&text) {
            Ok(snapshot) => LoadOutcome::Loaded(snapshot),
            Err(LoadError::VersionSkew { found }) => LoadOutcome::Skew { found },
            Err(LoadError::Malformed(message)) => LoadOutcome::Corrupt(message),
        },
    }
}

fn read_sensitive_to_string(path: &Path) -> io::Result<String> {
    let mut file = crate::state_dir::open_existing_sensitive(path)?;
    // PLAUS-01: reject an implausibly large state file before reading it into
    // memory. Legitimate workspace state is kilobytes; this cap bounds both the
    // whole-file read and the parser's char-vector clone of the input. Over-cap
    // surfaces as a corrupt-load outcome (fail closed to a fresh launch), never
    // logging file contents. Platform-neutral: same limit and code path on
    // every OS.
    let len = file.metadata()?.len();
    if len > MAX_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("state file is {len} bytes, over the {MAX_SNAPSHOT_BYTES}-byte load budget"),
        ));
    }
    let mut text = String::with_capacity(len as usize);
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// C27: remove crash-orphaned atomic-write temporaries from the state and
/// layouts directories at startup. A crash between [`create_temp_sibling`] and
/// its rename leaves a hidden `.<base>...tmp` sibling behind; without a sweep
/// these accumulate across sessions. Only files matching that shape AND older
/// than a conservative age are removed, so a temporary mid-write by a concurrent
/// instance is never disturbed. Best-effort: an unreadable directory is skipped.
/// Runs on Windows too (the atomic-write temp path is cross-platform).
pub(crate) fn sweep_stale_temp_files() {
    let Ok(dir) = crate::logging::prepare_state_log_dir() else {
        return;
    };
    let now = std::time::SystemTime::now();
    sweep_stale_temp_siblings_at(&dir, now, STALE_TEMP_AFTER);
    sweep_stale_temp_siblings_at(&dir.join(LAYOUTS_DIR), now, STALE_TEMP_AFTER);
}

/// Temporaries older than this are treated as crash-orphaned. Generous, so a
/// slow atomic write by a concurrent instance is never mistaken for stale.
const STALE_TEMP_AFTER: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Age-gated sweep of `.<...>.tmp` siblings in `dir`. Split from
/// [`sweep_stale_temp_files`] so `now` / `stale_after` are injectable for tests.
fn sweep_stale_temp_siblings_at(
    dir: &Path,
    now: std::time::SystemTime,
    stale_after: std::time::Duration,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Match the `create_temp_sibling` shape: a hidden `.<...>.tmp` sibling.
        if !(name.starts_with('.') && name.ends_with(".tmp")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= stale_after);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
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

/// Validate an interactively tracked (OSC 7) cwd before it seeds a spawn
/// (audit D-1). Unlike [`resolve_cwd`] (the restore path), an unknown cwd stays
/// `None` -- New Tab / Duplicate / New Window then spawn in the default
/// directory, the pre-fix behavior -- rather than falling back to home. A
/// tracked directory that still exists is used as-is; one that does not, or a
/// non-filesystem path the Windows PowerShell integration can manufacture (a UNC
/// share parsed to `//srv/share`, a PSDrive parsed to `/HKLM:/...`) or that a
/// hostile OSC 7 from ordinary output can inject, is a directory `CreateProcessW`
/// / `posix_spawn` would reject or silently mis-seed, so it falls back to the
/// user's home. Only a single `metadata` probe; never aborts.
pub(crate) fn validate_interactive_cwd(
    captured: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let dir = captured?;
    if is_existing_dir(Path::new(dir)) {
        Some(PathBuf::from(dir))
    } else {
        home.map(Path::to_path_buf)
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
