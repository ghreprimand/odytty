// SPDX-License-Identifier: GPL-3.0-only
//! Pure, bounded candidate sources for the future command palette.
//!
//! Privacy boundary: shell history is local user data. This module only reads a
//! caller-provided path, or a path derived from caller-provided shell/home
//! inputs, into memory; it never logs entries, never persists them, and never
//! transmits them. Tests use synthetic temp-file fixtures only.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Default maximum history bytes read from the tail of a file.
pub const DEFAULT_HISTORY_MAX_BYTES: u64 = 1024 * 1024;
/// Default maximum physical history lines scanned from the bounded tail.
pub const DEFAULT_HISTORY_MAX_LINES: usize = 20_000;
/// Default maximum number of history entries returned.
pub const DEFAULT_HISTORY_MAX_ENTRIES: usize = 5000;
/// Default maximum characters retained for one command/directory candidate.
pub const DEFAULT_SOURCE_ENTRY_MAX_CHARS: usize = 4096;

/// Bounds for shell-history reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryReadLimits {
    /// Maximum bytes read from the end of the history file.
    pub max_bytes: u64,
    /// Maximum physical lines scanned from the bounded tail.
    pub max_lines: usize,
    /// Maximum parsed entries returned.
    pub max_entries: usize,
    /// Maximum characters retained per entry.
    pub max_entry_chars: usize,
}

impl Default for HistoryReadLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_HISTORY_MAX_BYTES,
            max_lines: DEFAULT_HISTORY_MAX_LINES,
            max_entries: DEFAULT_HISTORY_MAX_ENTRIES,
            max_entry_chars: DEFAULT_SOURCE_ENTRY_MAX_CHARS,
        }
    }
}

/// Shell history file formats understood by the bounded reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellHistoryFormat {
    /// Infer from the filename, falling back to plain line-based history.
    Auto,
    /// Bash/plain history: one command per line.
    Plain,
    /// Zsh extended history: `: <timestamp>:<duration>;<command>`.
    ZshExtended,
    /// Fish history: YAML-ish entries containing `- cmd: <command>`.
    Fish,
}

/// Resolved history file and parser for a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellHistorySource {
    /// History file path. The caller decides whether to read it.
    pub path: PathBuf,
    /// Parser format for `path`.
    pub format: ShellHistoryFormat,
}

/// Resolve the conventional history file for a shell executable.
///
/// The function only derives a path; it does not touch the filesystem. `shell`
/// may be an executable basename (`zsh`) or a path (`/bin/zsh`). Fish history
/// follows `$XDG_CONFIG_HOME/fish/fish_history`, or
/// `~/.config/fish/fish_history` when no XDG config dir is supplied.
pub fn detect_shell_history(
    shell: impl AsRef<str>,
    home_dir: impl AsRef<Path>,
    xdg_config_home: Option<&Path>,
) -> Option<ShellHistorySource> {
    let shell_name = Path::new(shell.as_ref())
        .file_name()
        .and_then(|name| name.to_str())?
        .trim_start_matches('-');
    let home_dir = home_dir.as_ref();
    match shell_name {
        "bash" => Some(ShellHistorySource {
            path: home_dir.join(".bash_history"),
            format: ShellHistoryFormat::Plain,
        }),
        "zsh" => Some(ShellHistorySource {
            path: home_dir.join(".zsh_history"),
            format: ShellHistoryFormat::ZshExtended,
        }),
        "fish" => {
            let config_dir = xdg_config_home
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir.join(".config"));
            Some(ShellHistorySource {
                path: config_dir.join("fish").join("fish_history"),
                format: ShellHistoryFormat::Fish,
            })
        }
        _ => None,
    }
}

/// Resolve and read a shell's conventional history file with default limits.
pub fn read_history_for_shell(
    shell: impl AsRef<str>,
    home_dir: impl AsRef<Path>,
    xdg_config_home: Option<&Path>,
) -> Vec<String> {
    read_history_for_shell_with_limits(
        shell,
        home_dir,
        xdg_config_home,
        HistoryReadLimits::default(),
    )
}

/// Resolve and read a shell's conventional history file with explicit limits.
pub fn read_history_for_shell_with_limits(
    shell: impl AsRef<str>,
    home_dir: impl AsRef<Path>,
    xdg_config_home: Option<&Path>,
    limits: HistoryReadLimits,
) -> Vec<String> {
    let Some(source) = detect_shell_history(shell, home_dir, xdg_config_home) else {
        return Vec::new();
    };
    read_shell_history_with_limits(source.path, source.format, limits)
}

/// Read a shell history file with default limits.
///
/// Missing, unreadable, malformed, binary, or oversized files return the
/// readable bounded subset, or an empty vector. Entries are most-recent-first,
/// with older duplicates collapsed after reversing.
pub fn read_shell_history(path: impl AsRef<Path>, format: ShellHistoryFormat) -> Vec<String> {
    read_shell_history_with_limits(path, format, HistoryReadLimits::default())
}

/// Read a shell history file with explicit hard limits.
///
/// This function never reads more than `limits.max_bytes` from the tail of the
/// file, never scans more than `limits.max_lines` physical lines, and never
/// returns more than `limits.max_entries`.
pub fn read_shell_history_with_limits(
    path: impl AsRef<Path>,
    format: ShellHistoryFormat,
    limits: HistoryReadLimits,
) -> Vec<String> {
    if limits.max_bytes == 0
        || limits.max_lines == 0
        || limits.max_entries == 0
        || limits.max_entry_chars == 0
    {
        return Vec::new();
    }
    let path = path.as_ref();
    let format = resolve_format(path, format);
    let Some(bytes) = read_tail(path, limits.max_bytes) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines = bounded_tail_lines(&text, limits.max_lines);
    let entries = parse_history_entries(&lines, format, limits.max_entry_chars);
    dedupe_most_recent(entries, limits.max_entries)
}

fn bounded_tail_lines(text: &str, max_lines: usize) -> Vec<&str> {
    let mut lines: Vec<_> = text.lines().rev().take(max_lines).collect();
    lines.reverse();
    lines
}

fn parse_history_entries(
    lines: &[&str],
    format: ShellHistoryFormat,
    max_entry_chars: usize,
) -> Vec<String> {
    match format {
        ShellHistoryFormat::Auto => Vec::new(),
        ShellHistoryFormat::Fish => parse_fish_history_entries(lines, max_entry_chars),
        ShellHistoryFormat::Plain | ShellHistoryFormat::ZshExtended => lines
            .iter()
            .filter_map(|line| parse_history_line(line, format, max_entry_chars))
            .collect(),
    }
}

fn dedupe_most_recent(entries: Vec<String>, max_entries: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in entries.into_iter().rev() {
        if !seen.insert(entry.clone()) {
            continue;
        }
        out.push(entry);
        if out.len() >= max_entries {
            break;
        }
    }
    out
}

/// Bounds for recent-directory candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectorySourceLimits {
    /// Maximum directory entries retained.
    pub max_entries: usize,
    /// Maximum characters retained per directory.
    pub max_entry_chars: usize,
}

impl Default for DirectorySourceLimits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_HISTORY_MAX_ENTRIES,
            max_entry_chars: DEFAULT_SOURCE_ENTRY_MAX_CHARS,
        }
    }
}

/// Small bounded most-recent-first directory source.
///
/// The live palette can feed this from OSC 7 cwd updates later; this type only
/// owns the pure de-duplication and bounds behavior.
#[derive(Debug, Clone)]
pub struct RecentDirs {
    limits: DirectorySourceLimits,
    entries: VecDeque<String>,
}

impl RecentDirs {
    /// Create an empty source with explicit limits.
    pub fn new(limits: DirectorySourceLimits) -> Self {
        Self {
            limits,
            entries: VecDeque::new(),
        }
    }

    /// Record a directory as most recent.
    ///
    /// Empty values are ignored. Existing entries are moved to the front.
    pub fn observe(&mut self, directory: impl AsRef<str>) {
        if self.limits.max_entries == 0 || self.limits.max_entry_chars == 0 {
            return;
        }
        let Some(entry) = normalize_entry(directory.as_ref(), self.limits.max_entry_chars) else {
            return;
        };
        if let Some(index) = self
            .entries
            .iter()
            .position(|candidate| candidate == &entry)
        {
            self.entries.remove(index);
        }
        self.entries.push_front(entry);
        while self.entries.len() > self.limits.max_entries {
            self.entries.pop_back();
        }
    }

    /// Record the advisory OSC 7 cwd for the focused terminal, when present.
    ///
    /// This accepts an already-parsed cwd from `Terminal::current_working_directory`.
    /// It never queries the filesystem and never infers directories from history.
    pub fn observe_osc7_cwd(&mut self, cwd: Option<&str>) {
        if let Some(cwd) = cwd {
            self.observe(cwd);
        }
    }

    /// Return most-recent-first directory candidates.
    pub fn candidates(&self) -> Vec<String> {
        self.entries.iter().cloned().collect()
    }
}

impl Default for RecentDirs {
    fn default() -> Self {
        Self::new(DirectorySourceLimits::default())
    }
}

/// Build a bounded directory candidate list from oldest-to-newest observations.
pub fn directory_candidates<I, S>(directories: I, limits: DirectorySourceLimits) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut recent = RecentDirs::new(limits);
    for directory in directories {
        recent.observe(directory);
    }
    recent.candidates()
}

fn read_tail(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let read_len = len.min(max_bytes);
    let start = len.saturating_sub(read_len);
    file.seek(SeekFrom::Start(start)).ok()?;

    let mut bytes = Vec::with_capacity(read_len as usize);
    file.take(read_len).read_to_end(&mut bytes).ok()?;
    if start == 0 {
        return Some(bytes);
    }

    let first_newline = bytes.iter().position(|byte| *byte == b'\n')?;
    Some(bytes[(first_newline + 1)..].to_vec())
}

fn resolve_format(path: &Path, format: ShellHistoryFormat) -> ShellHistoryFormat {
    if format != ShellHistoryFormat::Auto {
        return format;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    match name {
        ".zsh_history" | "zsh_history" => ShellHistoryFormat::ZshExtended,
        "fish_history" => ShellHistoryFormat::Fish,
        _ => ShellHistoryFormat::Plain,
    }
}

fn parse_history_line(
    line: &str,
    format: ShellHistoryFormat,
    max_entry_chars: usize,
) -> Option<String> {
    match format {
        ShellHistoryFormat::Auto => None,
        ShellHistoryFormat::Plain => normalize_entry(line, max_entry_chars),
        ShellHistoryFormat::ZshExtended => parse_zsh_extended(line, max_entry_chars),
        ShellHistoryFormat::Fish => parse_fish_history(line, max_entry_chars),
    }
}

fn parse_zsh_extended(line: &str, max_entry_chars: usize) -> Option<String> {
    let trimmed = line.trim_end_matches('\r');
    if !trimmed.starts_with(": ") {
        return normalize_entry(trimmed, max_entry_chars);
    }
    let (_, command) = trimmed.split_once(';')?;
    normalize_entry(command, max_entry_chars)
}

fn parse_fish_history(line: &str, max_entry_chars: usize) -> Option<String> {
    let trimmed = line.trim_start().trim_end_matches('\r');
    let rest = trimmed.strip_prefix("- cmd:")?.trim_start();
    let unquoted = strip_matching_quotes(rest);
    normalize_entry(unquoted, max_entry_chars)
}

fn parse_fish_history_entries(lines: &[&str], max_entry_chars: usize) -> Vec<String> {
    let mut entries = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_start().trim_end_matches('\r');
        let Some(rest) = trimmed.strip_prefix("- cmd:") else {
            index += 1;
            continue;
        };

        let rest = rest.trim_start();
        if is_yaml_block_scalar(rest) {
            let (command, next_index) = collect_fish_block_command(lines, index + 1);
            if let Some(entry) = normalize_entry(&command, max_entry_chars) {
                entries.push(entry);
            }
            index = next_index;
            continue;
        }

        let unquoted = strip_matching_quotes(rest);
        if let Some(entry) = normalize_entry(unquoted, max_entry_chars) {
            entries.push(entry);
        }
        index += 1;
    }
    entries
}

fn is_yaml_block_scalar(value: &str) -> bool {
    matches!(value, "|" | "|-" | "|+" | ">" | ">-" | ">+")
}

fn collect_fish_block_command(lines: &[&str], mut index: usize) -> (String, usize) {
    let mut command = String::new();
    while let Some(line) = lines.get(index) {
        if !line.starts_with("    ") && !line.starts_with('\t') {
            break;
        }
        if !command.is_empty() {
            command.push('\n');
        }
        command.push_str(line.trim_start().trim_end_matches('\r'));
        index += 1;
    }
    (command, index)
}

fn strip_matching_quotes(value: &str) -> &str {
    if value.len() < 2 {
        return value;
    }
    let bytes = value.as_bytes();
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn normalize_entry(raw: &str, max_entry_chars: usize) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trim_chars(trimmed, max_entry_chars))
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "odytty-palette-sources-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
        let path = temp_file(name);
        fs::write(&path, bytes).expect("write synthetic history fixture");
        path
    }

    fn small_limits() -> HistoryReadLimits {
        HistoryReadLimits {
            max_bytes: 1024,
            max_lines: 64,
            max_entries: 8,
            max_entry_chars: 128,
        }
    }

    #[test]
    fn detects_conventional_shell_history_paths_without_reading() {
        let home = PathBuf::from("/synthetic/home");
        let xdg = PathBuf::from("/synthetic/config");

        assert_eq!(
            detect_shell_history("/usr/bin/bash", &home, None),
            Some(ShellHistorySource {
                path: home.join(".bash_history"),
                format: ShellHistoryFormat::Plain,
            })
        );
        assert_eq!(
            detect_shell_history("-zsh", &home, None),
            Some(ShellHistorySource {
                path: home.join(".zsh_history"),
                format: ShellHistoryFormat::ZshExtended,
            })
        );
        assert_eq!(
            detect_shell_history("fish", &home, Some(&xdg)),
            Some(ShellHistorySource {
                path: xdg.join("fish").join("fish_history"),
                format: ShellHistoryFormat::Fish,
            })
        );
        assert!(detect_shell_history("sh", &home, None).is_none());
    }

    #[test]
    fn read_history_for_shell_uses_detected_synthetic_file() {
        let home = TempDir::new("odytty-palette-source-home");
        let path = home.path().join(".zsh_history");
        fs::write(&path, b": 1700000000:0;cargo test\n").expect("write synthetic zsh history");

        let entries =
            read_history_for_shell_with_limits("/bin/zsh", home.path(), None, small_limits());

        assert_eq!(entries, vec!["cargo test"]);
    }

    #[test]
    fn bash_history_is_most_recent_first_and_collapses_all_duplicates() {
        let path = write_fixture("bash", b"ls\nmake test\ncd /tmp\nmake test\n");

        let entries =
            read_shell_history_with_limits(&path, ShellHistoryFormat::Plain, small_limits());

        assert_eq!(entries, vec!["make test", "cd /tmp", "ls"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn zsh_extended_history_extracts_command_after_first_semicolon() {
        let path = write_fixture(
            "zsh",
            b": 1700000000:0;cargo test\n: 1700000001:2;git status --short\n",
        );

        let entries =
            read_shell_history_with_limits(&path, ShellHistoryFormat::ZshExtended, small_limits());

        assert_eq!(entries, vec!["git status --short", "cargo test"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn fish_history_reads_cmd_rows_when_present() {
        let path = write_fixture(
            "fish_history",
            b"- cmd: cargo build\n  when: 1700000000\n- cmd: \"git status\"\n",
        );

        let entries =
            read_shell_history_with_limits(&path, ShellHistoryFormat::Fish, small_limits());

        assert_eq!(entries, vec!["git status", "cargo build"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn fish_history_accepts_multiline_blocks_and_truncated_blocks() {
        let path = write_fixture(
            "fish-block",
            b"- cmd: |-\n    echo one\n    echo two\n  when: 1700000000\n- cmd: |-\n  when: 1700000001\n- cmd: tail\n",
        );

        let entries =
            read_shell_history_with_limits(&path, ShellHistoryFormat::Fish, small_limits());

        assert_eq!(entries, vec!["tail", "echo one\necho two"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_and_garbage_lines_are_skipped_or_return_partial_without_panicking() {
        let path = write_fixture(
            "mixed",
            b": missing semicolon\n: 1700000000:0;echo ok\n\xff\xfeplain bytes\n",
        );

        let entries =
            read_shell_history_with_limits(&path, ShellHistoryFormat::ZshExtended, small_limits());

        assert_eq!(entries, vec!["\u{fffd}\u{fffd}plain bytes", "echo ok"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn missing_file_returns_empty() {
        let path = temp_file("missing");

        let entries =
            read_shell_history_with_limits(&path, ShellHistoryFormat::Plain, small_limits());

        assert!(entries.is_empty());
    }

    #[test]
    fn oversized_file_reads_only_tail_and_respects_entry_cap() {
        let path = write_fixture(
            "oversized",
            b"old-00\nold-01\nold-02\nold-03\nold-04\nnew-05\nnew-06\nnew-07\n",
        );
        let limits = HistoryReadLimits {
            max_bytes: 24,
            max_lines: 64,
            max_entries: 2,
            max_entry_chars: 64,
        };

        let entries = read_shell_history_with_limits(&path, ShellHistoryFormat::Plain, limits);

        assert_eq!(entries, vec!["new-07", "new-06"]);
        assert!(!entries.iter().any(|entry| entry.starts_with("old-")));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn line_scan_cap_keeps_only_recent_physical_lines() {
        let path = write_fixture("line-cap", b"old\nmiddle\nnew\n");
        let limits = HistoryReadLimits {
            max_bytes: 1024,
            max_lines: 2,
            max_entries: 8,
            max_entry_chars: 64,
        };

        let entries = read_shell_history_with_limits(&path, ShellHistoryFormat::Plain, limits);

        assert_eq!(entries, vec!["new", "middle"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn per_entry_length_cap_is_enforced() {
        let path = write_fixture("long", b"abcdef\n");
        let limits = HistoryReadLimits {
            max_bytes: 1024,
            max_lines: 64,
            max_entries: 8,
            max_entry_chars: 3,
        };

        let entries = read_shell_history_with_limits(&path, ShellHistoryFormat::Plain, limits);

        assert_eq!(entries, vec!["abc"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn zero_limits_return_empty_without_reading() {
        let path = write_fixture("zero", b"echo hidden\n");
        let limits = HistoryReadLimits {
            max_bytes: 0,
            max_lines: 64,
            max_entries: 8,
            max_entry_chars: 128,
        };

        let entries = read_shell_history_with_limits(&path, ShellHistoryFormat::Plain, limits);

        assert!(entries.is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unreadable_or_non_file_path_returns_empty() {
        let dir = TempDir::new("odytty-palette-source-dir");

        let entries = read_shell_history_with_limits(
            dir.path(),
            ShellHistoryFormat::Plain,
            HistoryReadLimits::default(),
        );

        assert!(entries.is_empty());
    }

    #[test]
    fn recent_dirs_are_bounded_most_recent_first_and_unique() {
        let limits = DirectorySourceLimits {
            max_entries: 3,
            max_entry_chars: 128,
        };
        let mut dirs = RecentDirs::new(limits);

        dirs.observe("/work/a");
        dirs.observe("/work/b");
        dirs.observe("/work/c");
        dirs.observe("/work/b");
        dirs.observe("/work/d");

        assert_eq!(dirs.candidates(), vec!["/work/d", "/work/b", "/work/c"]);
    }

    #[test]
    fn recent_dirs_can_be_fed_from_osc7_cwd_values() {
        let mut dirs = RecentDirs::new(DirectorySourceLimits {
            max_entries: 2,
            max_entry_chars: 128,
        });

        dirs.observe_osc7_cwd(None);
        dirs.observe_osc7_cwd(Some("/work/one"));
        dirs.observe_osc7_cwd(Some("/work/two"));
        dirs.observe_osc7_cwd(Some("/work/one"));

        assert_eq!(dirs.candidates(), vec!["/work/one", "/work/two"]);
    }

    #[test]
    fn directory_candidates_ignores_empty_and_truncates_long_entries() {
        let limits = DirectorySourceLimits {
            max_entries: 4,
            max_entry_chars: 5,
        };

        let dirs = directory_candidates(["", "   ", "/abcdef"], limits);

        assert_eq!(dirs, vec!["/abcd"]);
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
