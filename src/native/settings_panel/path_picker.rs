// SPDX-License-Identifier: GPL-3.0-only
//! Inline path-picker sub-state for `SettingsPanel` (SETTINGS-REDESIGN).
//!
//! Activated when Enter is pressed on a `SettingKind::Path` row at Level 2.
//! Renders a minimal file browser inside the settings panel body (no new
//! `OverlayMode`). Navigation: Up/Down move the selection, Enter on a
//! directory navigates into it, Enter on a file commits the path. Esc
//! cancels without changing the setting. v1 is select-only (no typed input
//! in the picker itself).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use super::SettingsPanelLine;
use super::pointer::{RowHit, RowZone};
use crate::native::overlay::OverlayInput;

const MAX_DIR_ENTRIES: usize = 1024;

/// State for an active path-picker session.
#[derive(Debug, Clone)]
pub(super) struct PathPickerState {
    /// The setting key this picker is editing.
    pub(super) key: &'static str,
    /// Currently-browsed directory.
    current_dir: PathBuf,
    /// Sorted entries in `current_dir` (dirs first, then filtered files).
    entries: Vec<PathEntry>,
    entries_cache: HashMap<PathBuf, Vec<PathEntry>>,
    pending: Option<PendingRead>,
    loading: bool,
    selected: usize,
    scroll: usize,
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
}

type ReadResult = (PathBuf, Vec<PathEntry>);

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
    /// A file was selected — commit this path string.
    Selected(String),
    /// Picker was cancelled; restore the original value.
    Cancelled,
}

impl PathPickerState {
    /// Create a new picker for `key`, starting in `start_dir`. `original` is
    /// the setting's current value — restored if the user presses Esc.
    pub(super) fn new(key: &'static str, start_dir: PathBuf, original: String) -> Self {
        let mut picker = Self {
            key,
            current_dir: PathBuf::new(),
            entries: Vec::new(),
            entries_cache: HashMap::new(),
            pending: None,
            loading: false,
            selected: 0,
            scroll: 0,
            original,
        };
        picker.navigate_to(start_dir);
        picker
    }

    /// Navigate into `dir`, refreshing the entry list.
    fn navigate_to(&mut self, dir: PathBuf) {
        self.current_dir = dir;
        if let Some(cached) = self.entries_cache.get(&self.current_dir) {
            self.entries = cached.clone();
            self.pending = None;
            self.loading = false;
        } else {
            self.entries = parent_entry_for(&self.current_dir).into_iter().collect();
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
            self.pending = None;
            self.loading = false;
            return;
        };
        let Ok((dir, entries)) = rx.try_recv() else {
            return;
        };
        drop(rx);
        self.entries_cache.insert(dir.clone(), entries.clone());
        if dir == self.current_dir {
            self.entries = entries;
            self.selected = 0;
            self.scroll = 0;
            self.loading = false;
            self.pending = None;
        } else if let Some(cached) = self.entries_cache.get(&self.current_dir) {
            self.entries = cached.clone();
            self.selected = 0;
            self.scroll = 0;
            self.loading = false;
            self.pending = None;
        };
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
        } else {
            PathPickerOutcome::Selected(entry.path.display().to_string())
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
        // Approximate: use 8 lines of slack.
        if self.selected >= self.scroll + 8 {
            self.scroll = self.selected.saturating_sub(7);
        }
        self.scroll = self.scroll.min(self.entries.len() - 1);
    }

    /// Build the body rows for the picker panel (used by `build_visible_rows`).
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

        // Directory breadcrumb header.
        let dir_str = self.current_dir.display().to_string();
        let dir_display = if dir_str.chars().count() > body_width.saturating_sub(3) {
            format!(
                "…{}",
                &dir_str[dir_str.len().saturating_sub(body_width.saturating_sub(4))..]
            )
        } else {
            dir_str
        };
        if rows.len() < body_height {
            rows.push((
                SettingsPanelLine {
                    text: format!("  {dir_display}"),
                    focused: false,
                    bold: false,
                },
                inert,
            ));
        }

        // Separator.
        if rows.len() < body_height {
            let sep = "─".repeat(body_width.min(48));
            rows.push((
                SettingsPanelLine {
                    text: format!("  {sep}"),
                    focused: false,
                    bold: false,
                },
                inert,
            ));
        }

        // Entry rows.
        if self.loading && rows.len() < body_height {
            rows.push((
                SettingsPanelLine {
                    text: "  Loading...".to_owned(),
                    focused: false,
                    bold: false,
                },
                inert,
            ));
        }
        for (index, entry) in self.entries.iter().enumerate().skip(self.scroll) {
            if rows.len() >= body_height {
                break;
            }
            let focused = index == self.selected;
            let marker = if focused { ">" } else { " " };
            let max_name = body_width.saturating_sub(4);
            let name = if entry.name.chars().count() > max_name {
                let mut n = entry.name.chars().take(max_name - 1).collect::<String>();
                n.push('~');
                n
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
                    zone: RowZone::Value,
                },
            ));
        }

        // Empty-dir notice.
        if self.entries.is_empty() && rows.len() < body_height {
            rows.push((
                SettingsPanelLine {
                    text: "  (empty directory)".to_owned(),
                    focused: false,
                    bold: false,
                },
                inert,
            ));
        }

        // Footer hint.
        if rows.len() < body_height {
            rows.push((
                SettingsPanelLine {
                    text: "  Enter open  Esc cancel".to_owned(),
                    focused: false,
                    bold: false,
                },
                inert,
            ));
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
    let (tx, rx) = mpsc::channel();
    let thread_dir = dir.clone();
    std::thread::spawn(move || {
        let _ = tx.send((
            thread_dir.clone(),
            read_dir_entries(&thread_dir, ext_filter),
        ));
    });
    PendingRead {
        rx: Arc::new(Mutex::new(rx)),
    }
}

/// Read the entries of `dir`: directories first (sorted), then files whose
/// extension matches `ext_filter` (sorted). If `ext_filter` is empty, all
/// non-directory files are shown. Hidden entries (starting with `.`) are
/// excluded from file listings but included for directories.
fn read_dir_entries(dir: &Path, ext_filter: &[&str]) -> Vec<PathEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathEntry> = Vec::new();
    let mut files: Vec<PathEntry> = Vec::new();

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
            files.push(PathEntry {
                name: name.into_owned(),
                path,
                is_dir: false,
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
    dirs
}

fn parent_entry_for(dir: &Path) -> Option<PathEntry> {
    dir.parent().map(|parent| PathEntry {
        name: "../".to_owned(),
        path: parent.to_path_buf(),
        is_dir: true,
    })
}

/// Resolve the starting directory for a picker from the setting's current value.
pub(super) fn resolve_start_dir(current_value: &str) -> PathBuf {
    let p = Path::new(current_value.trim());
    if p.is_file() {
        return p.parent().unwrap_or(p).to_path_buf();
    }
    if p.is_dir() {
        return p.to_path_buf();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
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
}
