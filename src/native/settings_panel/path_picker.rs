// SPDX-License-Identifier: GPL-3.0-only
//! Inline path-picker sub-state for `SettingsPanel`.
//!
//! Activated when Enter is pressed on a `SettingKind::Path` row at Level 2.
//! Renders a minimal file browser inside the settings panel body (no new
//! `OverlayMode`). Navigation: Up/Down move the selection, Enter on a
//! directory navigates into it, Enter on a file commits the path. Esc
//! cancels without changing the setting. v1 is select-only (no typed input
//! in the picker itself).

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::SettingsPanelLine;
use super::pointer::{RowHit, RowZone};
use crate::native::overlay::{OverlayInput, fit_chars, fit_chars_tail, text_display_width};

const MAX_DIR_ENTRIES: usize = 1024;

/// Directory listings the picker keeps for revisits, and the entries they may
/// hold together. The oldest listing goes first. One listing is at most
/// [`MAX_DIR_ENTRIES`] plus the parent row, so a single directory always fits.
const MAX_CACHED_DIRS: usize = 16;
const MAX_CACHED_ENTRIES: usize = 4096;

/// Directory reads that may be in flight at once, across every picker. A read
/// of a stalled mount can block its thread indefinitely, and navigating away
/// abandons it without cancelling it, so reads past this cap are refused (the
/// picker shows the directory as unreadable) instead of piling up threads.
const MAX_CONCURRENT_READS: usize = 4;

/// Longest the picker waits on the GUI thread for the starting directory to be
/// resolved from the setting's value. A stalled mount falls back to the home
/// directory instead of freezing the window.
const START_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// State for an active path-picker session.
#[derive(Debug, Clone)]
pub(super) struct PathPickerState {
    /// The setting key this picker is editing.
    pub(super) key: &'static str,
    /// Currently-browsed directory.
    current_dir: PathBuf,
    /// Sorted entries in `current_dir` (dirs first, then filtered files).
    entries: Vec<PathEntry>,
    /// Names in `current_dir` that were not listed because they cannot be
    /// written to a setting value (not valid UTF-8).
    skipped: usize,
    cache: DirCache,
    pending: Option<PendingRead>,
    loading: bool,
    /// The last read of `current_dir` failed: the worker was refused, lost, or
    /// poisoned. Shown as a row; navigating elsewhere clears it.
    load_failed: bool,
    selected: usize,
    scroll: usize,
    /// Entry rows the last rendered body could show. Keyboard focus follows
    /// the selection against this, not a fixed count; eight until the first
    /// render.
    capacity: Cell<usize>,
    /// The original setting value; restored on Esc. Kept for callers that
    /// need to restore the value if the picker is externally cancelled.
    #[allow(dead_code)]
    pub(super) original: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct PathPickerSignature {
    pub(super) key: &'static str,
    pub(super) current_dir: PathBuf,
    pub(super) loading: bool,
    pub(super) load_failed: bool,
    pub(super) skipped: usize,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) entry_count: usize,
}

#[derive(Debug, Clone)]
struct PathEntry {
    /// Display name (filename + "/" suffix for directories).
    name: String,
    path: PathBuf,
    is_dir: bool,
    /// When `Some`, this is a synthetic action row (e.g. "Default (bundled)")
    /// that commits this literal string instead of its (empty) path. `None`
    /// for ordinary filesystem entries, which commit their path.
    commit_override: Option<&'static str>,
}

/// One directory read: the listing and how many names were left out.
#[derive(Debug, Clone)]
struct Listing {
    dir: PathBuf,
    entries: Vec<PathEntry>,
    skipped: usize,
}

type ReadResult = Listing;

/// Directory listings kept for revisits: bounded in directories and in total
/// entries, oldest first out.
#[derive(Debug, Clone, Default)]
struct DirCache {
    listings: HashMap<PathBuf, (Vec<PathEntry>, usize)>,
    order: VecDeque<PathBuf>,
    total: usize,
}

impl DirCache {
    fn get(&self, dir: &Path) -> Option<(Vec<PathEntry>, usize)> {
        self.listings.get(dir).cloned()
    }

    fn insert(&mut self, listing: &Listing) {
        self.remove(&listing.dir);
        self.total += listing.entries.len();
        self.order.push_back(listing.dir.clone());
        self.listings.insert(
            listing.dir.clone(),
            (listing.entries.clone(), listing.skipped),
        );
        while self.order.len() > MAX_CACHED_DIRS || self.total > MAX_CACHED_ENTRIES {
            let Some(oldest) = self.order.front().cloned() else {
                break;
            };
            if self.order.len() == 1 {
                break;
            }
            self.remove(&oldest);
        }
    }

    fn remove(&mut self, dir: &Path) {
        if let Some((entries, _)) = self.listings.remove(dir) {
            self.total = self.total.saturating_sub(entries.len());
            self.order.retain(|queued| queued != dir);
        }
    }
}

/// Counts directory reads in flight against a cap.
struct ReadLimiter {
    active: AtomicUsize,
    max: usize,
}

/// Every picker's reads.
static READS: ReadLimiter = ReadLimiter::new(MAX_CONCURRENT_READS);

impl ReadLimiter {
    const fn new(max: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max,
        }
    }

    fn acquire(&'static self) -> Option<ReadSlot> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.max).then_some(active + 1)
            })
            .ok()
            .map(|_| ReadSlot { limiter: self })
    }
}

/// Releases one in-flight read when the worker ends, finishes, or never starts.
struct ReadSlot {
    limiter: &'static ReadLimiter,
}

impl Drop for ReadSlot {
    fn drop(&mut self) {
        self.limiter.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
struct PendingRead {
    rx: Arc<Mutex<mpsc::Receiver<ReadResult>>>,
}

impl std::fmt::Debug for PendingRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRead").finish_non_exhaustive()
    }
}

/// What the path picker wants the owning panel to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PathPickerOutcome {
    /// Input consumed; keep the picker open.
    Consumed,
    /// A file was selected - commit this path string.
    Selected(String),
    /// Picker was cancelled; restore the original value.
    Cancelled,
}

impl PathPickerState {
    /// Create a new picker for `key`, starting in `start_dir`. `original` is
    /// the setting's current value - restored if the user presses Esc.
    pub(super) fn new(key: &'static str, start_dir: PathBuf, original: String) -> Self {
        let mut picker = Self {
            key,
            current_dir: PathBuf::new(),
            entries: Vec::new(),
            skipped: 0,
            cache: DirCache::default(),
            pending: None,
            loading: false,
            load_failed: false,
            selected: 0,
            scroll: 0,
            capacity: Cell::new(8),
            original,
        };
        picker.navigate_to(start_dir);
        picker
    }

    /// Navigate into `dir`, refreshing the entry list.
    fn navigate_to(&mut self, dir: PathBuf) {
        self.current_dir = dir;
        self.load_failed = false;
        if let Some((cached, skipped)) = self.cache.get(&self.current_dir) {
            self.entries = with_actions(self.key, cached);
            self.skipped = skipped;
            self.pending = None;
            self.loading = false;
        } else {
            let base = parent_entry_for(&self.current_dir).into_iter().collect();
            self.entries = with_actions(self.key, base);
            self.skipped = 0;
            self.pending = Some(spawn_read_dir(
                self.current_dir.clone(),
                extension_filter(self.key),
            ));
            self.loading = true;
        }
        self.selected = 0;
        self.scroll = 0;
    }

    pub(super) fn render_signature(&self) -> PathPickerSignature {
        PathPickerSignature {
            key: self.key,
            current_dir: self.current_dir.clone(),
            loading: self.loading,
            load_failed: self.load_failed,
            skipped: self.skipped,
            selected: self.selected,
            scroll: self.scroll,
            entry_count: self.entries.len(),
        }
    }

    pub(super) fn poll_pending(&mut self) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let Ok(rx) = pending.rx.lock() else {
            self.settle_failed_read();
            return;
        };
        let listing = match rx.try_recv() {
            Ok(listing) => listing,
            Err(mpsc::TryRecvError::Empty) => return,
            // The worker ended without a result (it panicked, was refused, or
            // could not be started): settle instead of loading forever.
            Err(mpsc::TryRecvError::Disconnected) => {
                drop(rx);
                self.settle_failed_read();
                return;
            }
        };
        drop(rx);
        self.cache.insert(&listing);
        if listing.dir == self.current_dir {
            self.entries = with_actions(self.key, listing.entries);
            self.skipped = listing.skipped;
            self.selected = 0;
            self.scroll = 0;
            self.loading = false;
            self.pending = None;
        } else if let Some((cached, skipped)) = self.cache.get(&self.current_dir) {
            self.entries = with_actions(self.key, cached);
            self.skipped = skipped;
            self.selected = 0;
            self.scroll = 0;
            self.loading = false;
            self.pending = None;
        };
    }

    /// The pending read will never deliver: stop loading and show the
    /// directory as unreadable. The parent row and actions stay usable.
    fn settle_failed_read(&mut self) {
        self.pending = None;
        self.loading = false;
        self.load_failed = true;
    }

    /// Handle one key event. Returns the picker outcome.
    pub(super) fn handle_input(&mut self, input: OverlayInput) -> PathPickerOutcome {
        self.poll_pending();
        if self.loading
            && matches!(
                input,
                OverlayInput::Down | OverlayInput::PageDown | OverlayInput::End
            )
        {
            return PathPickerOutcome::Consumed;
        }
        match input {
            OverlayInput::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.clamp_scroll();
                }
            }
            OverlayInput::Down => {
                if self.selected + 1 < self.entries.len() {
                    self.selected += 1;
                    self.clamp_scroll();
                }
            }
            OverlayInput::Home => {
                self.selected = 0;
                self.clamp_scroll();
            }
            OverlayInput::End => {
                self.selected = self.entries.len().saturating_sub(1);
                self.clamp_scroll();
            }
            OverlayInput::PageUp => {
                self.selected = self.selected.saturating_sub(6);
                self.clamp_scroll();
            }
            OverlayInput::PageDown => {
                self.selected = (self.selected + 6).min(self.entries.len().saturating_sub(1));
                self.clamp_scroll();
            }
            OverlayInput::Activate => {
                return self.activate_selected();
            }
            OverlayInput::Close => {
                return PathPickerOutcome::Cancelled;
            }
            _ => {}
        }
        PathPickerOutcome::Consumed
    }

    fn activate_selected(&mut self) -> PathPickerOutcome {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return PathPickerOutcome::Consumed;
        };
        if entry.is_dir {
            self.navigate_to(entry.path);
            PathPickerOutcome::Consumed
        } else if let Some(value) = entry.commit_override {
            PathPickerOutcome::Selected(value.to_owned())
        } else if let Some(value) = entry.path.to_str() {
            PathPickerOutcome::Selected(value.to_owned())
        } else {
            // A path that is not valid UTF-8 cannot be written to a setting
            // without becoming a different path; refuse it. The listing does
            // not offer such files, so this is a guard.
            PathPickerOutcome::Consumed
        }
    }

    pub(super) fn activate_index(&mut self, index: usize) -> PathPickerOutcome {
        if index >= self.entries.len() {
            return PathPickerOutcome::Consumed;
        }
        self.selected = index;
        self.clamp_scroll();
        self.activate_selected()
    }

    /// Wheel-driven free scroll.
    pub(super) fn scroll_lines(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.scroll = 0;
            return;
        }
        let max = self.entries.len().saturating_sub(1) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max) as usize;
    }

    fn clamp_scroll(&mut self) {
        if self.entries.is_empty() {
            self.scroll = 0;
            return;
        }
        self.selected = self.selected.min(self.entries.len() - 1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        // Keep the selection inside the entry rows the last render showed.
        let capacity = self.capacity.get().max(1);
        if self.selected >= self.scroll + capacity {
            self.scroll = self.selected + 1 - capacity;
        }
        self.scroll = self.scroll.min(self.entries.len() - 1);
    }

    /// Build the body rows for the picker panel (used by `build_visible_rows`).
    ///
    /// Layout: breadcrumb, separator, an optional loading row, the entry rows,
    /// notice rows (unreadable, empty, names not shown), then a footer hint.
    /// Every row is added only while the body has room, and the footer keeps
    /// its row when the body is at least four rows high. The number of entry
    /// rows is recorded so keyboard focus can follow the selection.
    pub(super) fn build_visible_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(SettingsPanelLine, RowHit)> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }
        let mut rows: Vec<(SettingsPanelLine, RowHit)> = Vec::new();
        let inert = RowHit {
            entry_index: None,
            zone: RowZone::GroupHeader,
        };
        let text_row = |text: String| {
            (
                SettingsPanelLine {
                    text,
                    focused: false,
                    bold: false,
                },
                inert,
            )
        };

        // Directory breadcrumb header, cut to display columns so the trailing
        // path component stays visible whatever the scripts in the path.
        let dir_str = self.current_dir.display().to_string();
        let dir_display = if text_display_width(&dir_str) > body_width.saturating_sub(3) {
            format!(
                "…{}",
                fit_chars_tail(&dir_str, body_width.saturating_sub(4))
            )
        } else {
            dir_str
        };
        rows.push(text_row(format!("  {dir_display}")));

        // Separator.
        if rows.len() < body_height {
            let sep = "─".repeat(body_width.min(48));
            rows.push(text_row(format!("  {sep}")));
        }

        // Notices that follow the entries.
        let mut notices: Vec<String> = Vec::new();
        if self.load_failed {
            notices.push("  (this directory could not be read)".to_owned());
        } else if self.entries.is_empty() && !self.loading {
            notices.push("  (empty directory)".to_owned());
        }
        if self.skipped > 0 {
            notices.push(format!(
                "  ({} names not shown: not valid UTF-8)",
                self.skipped
            ));
        }

        let loading_rows = usize::from(self.loading);
        let footer_rows = usize::from(body_height >= 4);
        let capacity = body_height
            .saturating_sub(rows.len() + loading_rows + notices.len() + footer_rows)
            .max(1);
        self.capacity.set(capacity);

        if self.loading && rows.len() < body_height {
            rows.push(text_row("  Loading...".to_owned()));
        }
        for (index, entry) in self.entries.iter().enumerate().skip(self.scroll) {
            if rows.len() >= body_height || index >= self.scroll + capacity {
                break;
            }
            let focused = index == self.selected;
            let marker = if focused { ">" } else { " " };
            let max_name = body_width.saturating_sub(4);
            // Cut by display columns, with one column kept for the `~` marker;
            // a body too narrow to hold even that renders an empty name.
            let name = if text_display_width(&entry.name) > max_name {
                match max_name.checked_sub(1) {
                    Some(keep) => {
                        let mut n = fit_chars(&entry.name, keep);
                        n.push('~');
                        n
                    }
                    None => String::new(),
                }
            } else {
                entry.name.clone()
            };
            rows.push((
                SettingsPanelLine {
                    text: format!("{marker} {name}"),
                    focused,
                    bold: focused && !entry.is_dir,
                },
                RowHit {
                    // entry_index carries the path-entry index for pointer clicks.
                    entry_index: Some(index),
                    zone: RowZone::Value { value_x0: 0 },
                },
            ));
        }

        for notice in notices {
            if rows.len() < body_height {
                rows.push(text_row(notice));
            }
        }

        // Footer hint.
        if rows.len() < body_height {
            rows.push(text_row("  Enter open  Esc cancel".to_owned()));
        }

        // Every row honors the body width in display columns, whatever its
        // indent and marker add: cut between owners, never by bytes or scalars.
        for (line, _) in &mut rows {
            if text_display_width(&line.text) > body_width {
                line.text = fit_chars(&line.text, body_width);
            }
        }
        rows
    }
}

/// Determine which file extensions to show based on the setting key.
fn extension_filter(key: &str) -> &'static [&'static str] {
    match key {
        "font" | "symbol_font" => &["ttf", "otf", "ttc"],
        "background_image" => &["png", "jpg", "jpeg", "webp"],
        _ => &[],
    }
}

fn spawn_read_dir(dir: PathBuf, ext_filter: &'static [&'static str]) -> PendingRead {
    spawn_read_dir_limited(&READS, dir, ext_filter)
}

fn spawn_read_dir_limited(
    limiter: &'static ReadLimiter,
    dir: PathBuf,
    ext_filter: &'static [&'static str],
) -> PendingRead {
    let (tx, rx) = mpsc::channel();
    // The read runs on a named worker so a slow directory never blocks the UI
    // thread. When no worker can run (all read slots are busy, or the thread
    // cannot be created) the sender is dropped without a result: the picker
    // sees a disconnected channel and shows the directory as unreadable. No
    // filesystem work ever falls back onto the calling (GUI) thread.
    // Platform-neutral; no Windows-specific surface.
    if let Some(slot) = limiter.acquire() {
        // On a spawn failure the closure, and with it `tx` and the slot, is
        // dropped, which releases both.
        let _ = std::thread::Builder::new()
            .name("odytty-path-picker-read".to_owned())
            .spawn(move || {
                let _slot = slot;
                // An unreadable directory sends nothing, which the picker
                // reads from the closed channel as a failed read.
                if let Some(listing) = read_dir_listing(&dir, ext_filter) {
                    let _ = tx.send(listing);
                }
            });
    }
    PendingRead {
        rx: Arc::new(Mutex::new(rx)),
    }
}

/// Read the entries of `dir`: directories first (sorted), then files whose
/// extension matches `ext_filter` (sorted). If `ext_filter` is empty, all
/// non-directory files are shown. Hidden entries (starting with `.`), files and
/// directories alike, are not listed, and a symlink is neither a file nor a
/// directory here: the picker lists real entries only. A file whose path is not
/// valid UTF-8 cannot be written to a setting value without becoming a
/// different path, so it is not listed and is counted in the second field.
/// `None` means the directory could not be read at all.
fn list_dir(dir: &Path, ext_filter: &[&str]) -> Option<(Vec<PathEntry>, usize)> {
    let read = std::fs::read_dir(dir).ok()?;
    let mut dirs: Vec<PathEntry> = Vec::new();
    let mut files: Vec<PathEntry> = Vec::new();
    let mut skipped = 0usize;

    if let Some(parent) = parent_entry_for(dir) {
        dirs.push(parent);
    }

    for entry in read.flatten() {
        if dirs.len() + files.len() >= MAX_DIR_ENTRIES {
            break;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name_os = entry.file_name();
        let name = name_os.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        if file_type.is_dir() {
            dirs.push(PathEntry {
                name: format!("{name}/"),
                path,
                is_dir: true,
                commit_override: None,
            });
        } else if file_type.is_file() {
            if !ext_filter.is_empty() {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !ext_filter.contains(&ext.as_str()) {
                    continue;
                }
            }
            if path.to_str().is_none() {
                skipped += 1;
                continue;
            }
            files.push(PathEntry {
                name: name.into_owned(),
                path,
                is_dir: false,
                commit_override: None,
            });
        }
    }

    let has_parent = dirs.first().is_some_and(|entry| entry.name == "../");
    if has_parent {
        dirs[1..].sort_by(|a, b| a.name.cmp(&b.name));
    } else {
        dirs.sort_by(|a, b| a.name.cmp(&b.name));
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    dirs.extend(files);
    Some((dirs, skipped))
}

/// The worker's job: list `dir`, or deliver nothing when it cannot be read.
fn read_dir_listing(dir: &Path, ext_filter: &[&str]) -> Option<Listing> {
    let (entries, skipped) = list_dir(dir, ext_filter)?;
    Some(Listing {
        dir: dir.to_path_buf(),
        entries,
        skipped,
    })
}

/// The entries of `dir`, empty when it cannot be read.
#[cfg(test)]
fn read_dir_entries(dir: &Path, ext_filter: &[&str]) -> Vec<PathEntry> {
    list_dir(dir, ext_filter)
        .map(|(entries, _)| entries)
        .unwrap_or_default()
}

/// Synthetic action rows shown at the top of the picker for certain keys, so a
/// value the filesystem can't represent (the bundled default, or "no image") is
/// always one keypress away. Currently only `background_image` has any.
fn action_entries(key: &str) -> Vec<PathEntry> {
    if key == "background_image" {
        vec![
            PathEntry {
                name: "Default (bundled)".to_owned(),
                path: PathBuf::new(),
                is_dir: false,
                commit_override: Some(crate::settings::BUNDLED_BACKGROUND_TOKEN),
            },
            PathEntry {
                name: "None (no image)".to_owned(),
                path: PathBuf::new(),
                is_dir: false,
                commit_override: Some("none"),
            },
        ]
    } else {
        Vec::new()
    }
}

/// Prepend the key's synthetic action rows (if any) to a freshly-built or cached
/// filesystem entry list. The cache stores raw filesystem entries only; actions
/// are injected here when assigning `self.entries` so they never get cached.
fn with_actions(key: &str, entries: Vec<PathEntry>) -> Vec<PathEntry> {
    let mut combined = action_entries(key);
    if combined.is_empty() {
        return entries;
    }
    combined.extend(entries);
    combined
}

fn parent_entry_for(dir: &Path) -> Option<PathEntry> {
    dir.parent().map(|parent| PathEntry {
        name: "../".to_owned(),
        path: parent.to_path_buf(),
        is_dir: true,
        commit_override: None,
    })
}

/// What the setting's current value names on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartKind {
    File,
    Dir,
    Other,
}

fn probe_start(value: &Path) -> StartKind {
    if value.is_file() {
        StartKind::File
    } else if value.is_dir() {
        StartKind::Dir
    } else {
        StartKind::Other
    }
}

/// Resolve the starting directory for a picker from the setting's current value.
/// The filesystem probe runs on a worker and the GUI thread waits at most
/// [`START_PROBE_TIMEOUT`]: a value on a stalled mount, or a worker that cannot
/// be created, falls back to the home directory instead of freezing the window.
pub(super) fn resolve_start_dir(current_value: &str) -> PathBuf {
    resolve_start_dir_with(current_value, START_PROBE_TIMEOUT, probe_start)
}

fn resolve_start_dir_with(
    current_value: &str,
    timeout: Duration,
    probe: impl FnOnce(&Path) -> StartKind + Send + 'static,
) -> PathBuf {
    let value = PathBuf::from(current_value.trim());
    let (tx, rx) = mpsc::channel();
    let worker_value = value.clone();
    let started = std::thread::Builder::new()
        .name("odytty-path-picker-probe".to_owned())
        .spawn(move || {
            let _ = tx.send(probe(&worker_value));
        })
        .is_ok();
    let kind = if started {
        rx.recv_timeout(timeout).unwrap_or(StartKind::Other)
    } else {
        StartKind::Other
    };
    match kind {
        StartKind::File => value.parent().unwrap_or(&value).to_path_buf(),
        StartKind::Dir => value,
        // Route through `restore_home_dir` so the picker opens at
        // `%USERPROFILE%` on Windows instead of a never-set `$HOME`.
        StartKind::Other => {
            crate::native::persistence::restore_home_dir().unwrap_or_else(|| PathBuf::from("/"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("odytty-path-picker-{label}-{unique}"));
        fs::create_dir(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn lists_parent_dirs_first_and_filters_image_files() {
        let dir = temp_dir("filter");
        fs::create_dir(dir.join("photos")).expect("create child dir");
        fs::write(dir.join("wallpaper.PNG"), b"not a real png").expect("write png");
        fs::write(dir.join("notes.txt"), b"text").expect("write text");

        let entries = read_dir_entries(&dir, extension_filter("background_image"));
        let names = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names[0], "../");
        assert!(names.contains(&"photos/"));
        assert!(names.contains(&"wallpaper.PNG"));
        assert!(!names.contains(&"notes.txt"));

        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[test]
    fn background_image_picker_offers_default_and_none_actions() {
        let dir = temp_dir("actions");
        let picker = PathPickerState::new("background_image", dir.clone(), String::new());
        // Synthetic actions sit at the very top, above the parent ("../") entry,
        // so the bundled default is always reachable without editing the config.
        assert_eq!(picker.entries[0].name, "Default (bundled)");
        assert_eq!(
            picker.entries[0].commit_override,
            Some(crate::settings::BUNDLED_BACKGROUND_TOKEN)
        );
        assert_eq!(picker.entries[1].name, "None (no image)");
        assert_eq!(picker.entries[1].commit_override, Some("none"));
        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[test]
    fn activating_actions_commits_their_literal_tokens() {
        let dir = temp_dir("actions-commit");
        let mut default_picker =
            PathPickerState::new("background_image", dir.clone(), String::new());
        assert_eq!(
            default_picker.activate_index(0),
            PathPickerOutcome::Selected(crate::settings::BUNDLED_BACKGROUND_TOKEN.to_owned())
        );
        let mut none_picker = PathPickerState::new("background_image", dir.clone(), String::new());
        assert_eq!(
            none_picker.activate_index(1),
            PathPickerOutcome::Selected("none".to_owned())
        );
        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[test]
    fn only_background_image_has_action_rows() {
        assert!(action_entries("font").is_empty());
        assert!(action_entries("symbol_font").is_empty());
        assert_eq!(action_entries("background_image").len(), 2);
    }

    // Narrow bodies with multibyte paths must never panic on a
    // mid-codepoint byte slice or a `max_name - 1` underflow.
    #[test]
    fn narrow_terminal_multibyte_paths_render_without_panic() {
        let mut picker = PathPickerState::new("font", PathBuf::from("/tmp"), String::new());
        // A deep multibyte breadcrumb plus multibyte entry names exercise both
        // the breadcrumb tail slice and the entry-name truncation.
        picker.current_dir = PathBuf::from("/köln/über/naïve/文档/résumé");
        picker.entries = vec![
            PathEntry {
                name: "café_文档/".to_owned(),
                path: PathBuf::from("/köln/über/naïve/文档/résumé/café_文档"),
                is_dir: true,
                commit_override: None,
            },
            PathEntry {
                name: "αναφορά_ναı.txt".to_owned(),
                path: PathBuf::from("/köln/über/naïve/文档/résumé/αναφορά_ναı.txt"),
                is_dir: false,
                commit_override: None,
            },
        ];
        // Sweep the pathological narrow widths (and 0) - the test passing is the
        // no-panic assertion; also every rendered line honors the char budget.
        for w in 0..=7usize {
            // Byte-indexing or an unchecked subtraction would panic here; a clean
            // return of any row set is the first pass condition.
            let rows = picker.build_visible_rows(w, 6);
            // The second is the char budget: no row is wider than the body, and
            // a body with room for anything renders rows at all.
            for (line, _) in &rows {
                assert!(
                    text_display_width(&line.text) <= w,
                    "width {w}: {:?} is {} columns",
                    line.text,
                    text_display_width(&line.text)
                );
            }
            assert_eq!(rows.is_empty(), w == 0, "width {w}");
        }
        // A normal width still renders both entries.
        let wide = picker.build_visible_rows(40, 10);
        assert!(wide.iter().any(|(l, _)| l.text.contains("café_文档")));
    }

    #[test]
    fn spawn_read_dir_delivers_entries_through_the_channel() {
        // Whichever mechanism runs - the spawned worker on the success path or
        // the synchronous fallback when a thread cannot be created - the
        // directory's entries must reach the channel exactly once, so
        // `poll_pending` always resolves the picker. This pins that contract
        // against a regression in the spawn/fallback seam.
        let dir = temp_dir("spawn-channel");
        fs::create_dir(dir.join("sub")).expect("create child dir");
        fs::write(dir.join("pic.png"), b"png").expect("write png");

        let pending = spawn_read_dir(dir.clone(), extension_filter("background_image"));
        let rx = pending.rx.lock().expect("lock receiver");
        let Listing {
            dir: got_dir,
            entries,
            skipped,
        } = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the channel yields the read result");
        assert_eq!(got_dir, dir);
        assert_eq!(skipped, 0);
        // Same result the direct read produces - the async path adds nothing.
        let direct = read_dir_entries(&dir, extension_filter("background_image"));
        let got_names = entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>();
        let direct_names = direct.iter().map(|e| e.name.as_str()).collect::<Vec<_>>();
        assert_eq!(got_names, direct_names);
        assert!(got_names.contains(&"sub/"));
        assert!(got_names.contains(&"pic.png"));
        // Exactly one message: no double-send across the spawn/fallback split.
        assert!(
            rx.try_recv().is_err(),
            "the channel carries a single result"
        );

        drop(rx);
        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[test]
    fn hidden_entries_are_not_listed_whether_file_or_directory() {
        let dir = temp_dir("hidden");
        fs::create_dir(dir.join(".hidden-dir")).expect("hidden dir");
        fs::create_dir(dir.join("shown-dir")).expect("shown dir");
        fs::write(dir.join(".hidden.png"), b"png").expect("hidden file");
        fs::write(dir.join("shown.png"), b"png").expect("shown file");
        let names: Vec<String> = read_dir_entries(&dir, extension_filter("background_image"))
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| name != "../")
            .collect();
        assert_eq!(names, ["shown-dir/", "shown.png"]);
        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_not_listed_either() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("symlink-file");
        let target = temp_dir("symlink-file-target");
        fs::write(target.join("wall.png"), b"png").expect("write target");
        symlink(target.join("wall.png"), dir.join("linked.png")).expect("create symlink");
        fs::write(dir.join("real.png"), b"png").expect("real file");
        let names: Vec<String> = read_dir_entries(&dir, extension_filter("background_image"))
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| name != "../")
            .collect();
        assert_eq!(names, ["real.png"], "only the real file is listed");
        fs::remove_dir_all(dir).expect("remove temp dir");
        fs::remove_dir_all(target).expect("remove target dir");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_are_not_followed_during_listing() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("symlink");
        let target = temp_dir("target");
        fs::write(target.join("target.png"), b"png").expect("write target");
        symlink(&target, dir.join("linked-target")).expect("create symlink");

        let entries = read_dir_entries(&dir, extension_filter("background_image"));
        assert!(
            entries.iter().all(|entry| entry.name != "linked-target/"),
            "listing must not follow symlinked directories"
        );

        fs::remove_dir_all(dir).expect("remove temp dir");
        fs::remove_dir_all(target).expect("remove target dir");
    }

    fn file_entries(count: usize) -> Vec<PathEntry> {
        (0..count)
            .map(|index| PathEntry {
                name: format!("file-{index:03}.png"),
                path: PathBuf::from(format!("/pictures/file-{index:03}.png")),
                is_dir: false,
                commit_override: None,
            })
            .collect()
    }

    fn picker_with(entries: Vec<PathEntry>) -> PathPickerState {
        let mut picker = PathPickerState::new("font", PathBuf::from("/tmp"), String::new());
        picker.pending = None;
        picker.loading = false;
        picker.entries = entries;
        picker
    }

    fn lost_worker() -> PendingRead {
        let (sender, receiver) = mpsc::channel::<Listing>();
        drop(sender);
        PendingRead {
            rx: Arc::new(Mutex::new(receiver)),
        }
    }

    #[test]
    fn a_read_whose_worker_is_lost_settles_as_unreadable_instead_of_loading_forever() {
        let mut picker = picker_with(Vec::new());
        picker.pending = Some(lost_worker());
        picker.loading = true;
        let before = picker.render_signature();
        picker.poll_pending();
        assert!(!picker.loading, "the picker stops loading");
        assert!(picker.pending.is_none());
        assert!(picker.load_failed);
        assert_ne!(
            picker.render_signature(),
            before,
            "the failure state reaches the render signature"
        );
        let rows = picker.build_visible_rows(60, 10);
        assert!(
            rows.iter()
                .any(|(line, _)| line.text.contains("could not be read")),
            "{rows:?}"
        );
        // Navigating again clears the failure and reads the new directory.
        picker.navigate_to(PathBuf::from("/tmp"));
        assert!(!picker.load_failed);
    }

    #[test]
    fn reads_past_the_cap_are_refused_without_a_worker_or_filesystem_work() {
        static LIMITER: ReadLimiter = ReadLimiter::new(2);
        let held: Vec<_> = (0..2)
            .map(|_| LIMITER.acquire().expect("a free read slot"))
            .collect();
        assert!(LIMITER.acquire().is_none(), "the cap holds");
        let refused = spawn_read_dir_limited(&LIMITER, PathBuf::from("/tmp"), &[]);
        let rx = refused.rx.lock().expect("receiver");
        assert!(
            matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
            "no worker ran and no listing was produced on this thread"
        );
        drop(rx);
        drop(held);
        assert!(
            LIMITER.acquire().is_some(),
            "finished reads free their slots"
        );
    }

    #[test]
    fn the_directory_cache_is_bounded_in_directories_and_entries() {
        let listing = |name: &str, count: usize| Listing {
            dir: PathBuf::from(name),
            entries: file_entries(count),
            skipped: 0,
        };
        let mut cache = DirCache::default();
        for index in 0..MAX_CACHED_DIRS + 5 {
            cache.insert(&listing(&format!("/d{index}"), 1));
        }
        assert_eq!(cache.listings.len(), MAX_CACHED_DIRS);
        assert!(
            cache.get(Path::new("/d0")).is_none(),
            "the oldest went first"
        );
        assert!(cache.get(Path::new("/d20")).is_some());

        let mut cache = DirCache::default();
        for index in 0..8 {
            cache.insert(&listing(&format!("/big{index}"), 1000));
        }
        assert!(cache.total <= MAX_CACHED_ENTRIES, "total {}", cache.total);
        assert!(cache.get(Path::new("/big7")).is_some(), "the newest stays");
        // Re-inserting a directory replaces it instead of double counting.
        cache.insert(&listing("/big7", 10));
        assert!(cache.total <= MAX_CACHED_ENTRIES);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_whose_name_is_not_valid_utf8_is_not_offered_and_is_counted() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = temp_dir("non-utf8");
        fs::write(dir.join("fine.png"), b"png").expect("fine file");
        fs::write(dir.join(OsStr::from_bytes(b"bad-\xff.png")), b"png").expect("bad name");
        let (entries, skipped) =
            list_dir(&dir, extension_filter("background_image")).expect("read");
        let names: Vec<&str> = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .filter(|name| *name != "../")
            .collect();
        assert_eq!(names, ["fine.png"]);
        assert_eq!(skipped, 1);

        let mut picker = picker_with(entries);
        picker.skipped = skipped;
        let rows = picker.build_visible_rows(60, 10);
        assert!(
            rows.iter()
                .any(|(line, _)| line.text.contains("1 names not shown")),
            "{rows:?}"
        );
        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_valid_utf8_is_never_committed() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let mut picker = picker_with(vec![PathEntry {
            name: "bad.png".to_owned(),
            path: PathBuf::from(OsStr::from_bytes(b"/pictures/bad-\xff.png")),
            is_dir: false,
            commit_override: None,
        }]);
        assert_eq!(picker.activate_index(0), PathPickerOutcome::Consumed);
    }

    #[test]
    fn keyboard_focus_follows_the_rows_the_body_really_shows() {
        let mut picker = picker_with(file_entries(30));
        // A seven-row body holds the breadcrumb, the separator and the footer,
        // which leaves four entry rows.
        let first = picker.build_visible_rows(40, 7);
        assert_eq!(
            first
                .iter()
                .filter(|(line, _)| line.text.contains("file-"))
                .count(),
            4
        );
        for _ in 0..10 {
            picker.handle_input(OverlayInput::Down);
        }
        let rows = picker.build_visible_rows(40, 7);
        let focused: Vec<_> = rows.iter().filter(|(line, _)| line.focused).collect();
        assert_eq!(
            focused.len(),
            1,
            "the selection is inside the body: {rows:?}"
        );
        assert_eq!(focused[0].1.entry_index, Some(10));
        // Moving back up scrolls back instead of leaving the selection above.
        for _ in 0..10 {
            picker.handle_input(OverlayInput::Up);
        }
        let rows = picker.build_visible_rows(40, 7);
        assert!(rows.iter().any(|(line, _)| line.focused), "{rows:?}");
        assert_eq!(picker.scroll, 0);
    }

    #[test]
    fn the_footer_keeps_its_row_and_no_row_exceeds_the_body_height() {
        let picker = picker_with(file_entries(30));
        for height in 1..=12 {
            let rows = picker.build_visible_rows(40, height);
            assert!(rows.len() <= height, "height {height}: {} rows", rows.len());
            if height >= 4 {
                assert!(
                    rows.last()
                        .is_some_and(|(line, _)| line.text.contains("Esc cancel")),
                    "height {height}: {rows:?}"
                );
            }
        }
    }

    #[test]
    fn wide_names_are_cut_by_display_columns_not_scalars() {
        let picker = picker_with(vec![PathEntry {
            name: "文档文档文档文档文档.png".to_owned(),
            path: PathBuf::from("/pictures/文档文档文档文档文档.png"),
            is_dir: false,
            commit_override: None,
        }]);
        for width in 4..=24 {
            let rows = picker.build_visible_rows(width, 6);
            for (line, _) in &rows {
                assert!(
                    text_display_width(&line.text) <= width,
                    "width {width}: {:?} is {} columns",
                    line.text,
                    text_display_width(&line.text)
                );
            }
        }
        let rows = picker.build_visible_rows(12, 6);
        assert!(
            rows.iter().any(|(line, _)| line.text.contains('~')),
            "a cut name ends in the marker: {rows:?}"
        );
    }

    #[test]
    fn the_breadcrumb_keeps_the_trailing_component_by_display_columns() {
        let mut picker = picker_with(Vec::new());
        picker.current_dir = PathBuf::from("/家/文档/图片/最新");
        let rows = picker.build_visible_rows(14, 6);
        let crumb = &rows[0].0.text;
        assert!(crumb.contains("最新"), "{crumb:?}");
        assert!(text_display_width(crumb) <= 14, "{crumb:?}");
    }

    #[test]
    fn a_stalled_start_probe_falls_back_to_home_within_the_timeout() {
        let (release, wait) = mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let resolved = resolve_start_dir_with(
            "/mounts/stalled/wallpaper.png",
            Duration::from_millis(30),
            move |_| {
                let _ = wait.recv();
                StartKind::Dir
            },
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let home =
            crate::native::persistence::restore_home_dir().unwrap_or_else(|| PathBuf::from("/"));
        assert_eq!(resolved, home);
        let _ = release.send(());
    }

    #[test]
    fn the_start_probe_resolves_a_file_to_its_directory_and_a_directory_to_itself() {
        let long = Duration::from_secs(5);
        assert_eq!(
            resolve_start_dir_with("/pictures/wall.png", long, |_| StartKind::File),
            PathBuf::from("/pictures")
        );
        assert_eq!(
            resolve_start_dir_with("/pictures", long, |_| StartKind::Dir),
            PathBuf::from("/pictures")
        );
    }
}
