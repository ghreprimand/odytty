use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::{SettingEdit, config::config_key_to_env, config::env_to_config_key, config_file_path};

const PANEL_SECTION: &str = "# OdyTTY settings panel";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWritebackResult {
    pub path: PathBuf,
    pub changed: usize,
}

#[derive(Debug)]
pub struct ConfigWritebackError {
    pub message: String,
    source: Option<io::Error>,
}

impl ConfigWritebackError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    fn with_source(message: impl Into<String>, source: io::Error) -> Self {
        Self {
            message: message.into(),
            source: Some(source),
        }
    }
}

impl std::fmt::Display for ConfigWritebackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(source) = self.source.as_ref() {
            write!(f, "{}: {source}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for ConfigWritebackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

pub fn write_settings_changes(
    changes: &[SettingEdit],
) -> Result<ConfigWritebackResult, ConfigWritebackError> {
    let path = config_file_path().ok_or_else(|| {
        ConfigWritebackError::new("could not resolve odytty.conf path; set XDG_CONFIG_HOME or HOME")
    })?;
    write_settings_changes_to_path(&path, changes)
}

pub fn write_settings_changes_to_path(
    path: &Path,
    changes: &[SettingEdit],
) -> Result<ConfigWritebackResult, ConfigWritebackError> {
    let changes = canonical_changes(changes);
    if changes.is_empty() {
        return Ok(ConfigWritebackResult {
            path: path.to_path_buf(),
            changed: 0,
        });
    }

    let existing = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(ConfigWritebackError::with_source(
                format!("could not read {}", path.display()),
                error,
            ));
        }
    };
    let next = rewrite_config(&existing, &changes);
    atomic_write(path, next.as_bytes())?;
    Ok(ConfigWritebackResult {
        path: path.to_path_buf(),
        changed: changes.len(),
    })
}

fn canonical_changes(changes: &[SettingEdit]) -> BTreeMap<&'static str, String> {
    changes
        .iter()
        .filter_map(|change| env_to_config_key(change.env).map(|key| (key, change.value.clone())))
        .collect()
}

fn rewrite_config(contents: &str, changes: &BTreeMap<&'static str, String>) -> String {
    let mut lines = split_lines(contents);
    let mut env_by_key = BTreeMap::new();
    for (key, value) in changes {
        if let Some(env) = config_key_to_env(key) {
            env_by_key.insert(env, (*key, value.as_str()));
        }
    }

    let mut matching = BTreeMap::<&str, Vec<usize>>::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(env) = line_env(&line.text) else {
            continue;
        };
        if env_by_key.contains_key(env) {
            matching.entry(env).or_default().push(index);
        }
    }

    let mut appended = Vec::new();
    for (env, (key, value)) in &env_by_key {
        match matching.get(env) {
            Some(indexes) if value.is_empty() => {
                for index in indexes {
                    lines[*index].text = comment_out_setting(&lines[*index].text);
                }
            }
            Some(indexes) => {
                if let Some(index) = indexes.last().copied() {
                    lines[index].text = replace_line_value(&lines[index].text, value);
                }
            }
            None if !value.is_empty() => appended.push((*key, *value)),
            None => {}
        }
    }

    let mut output = join_lines(&lines, contents.ends_with('\n'));
    if !appended.is_empty() {
        append_panel_section(&mut output, &appended);
    }
    output
}

#[derive(Debug, Clone)]
struct ConfigLine {
    text: String,
    newline: &'static str,
}

fn split_lines(contents: &str) -> Vec<ConfigLine> {
    let mut out = Vec::new();
    for segment in contents.split_inclusive('\n') {
        if let Some(stripped) = segment.strip_suffix('\n') {
            out.push(ConfigLine {
                text: stripped.strip_suffix('\r').unwrap_or(stripped).to_owned(),
                newline: if stripped.ends_with('\r') {
                    "\r\n"
                } else {
                    "\n"
                },
            });
        } else if !segment.is_empty() {
            out.push(ConfigLine {
                text: segment.to_owned(),
                newline: "",
            });
        }
    }
    out
}

fn join_lines(lines: &[ConfigLine], had_trailing_newline: bool) -> String {
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        out.push_str(&line.text);
        if line.newline.is_empty() {
            if index + 1 < lines.len() || had_trailing_newline {
                out.push('\n');
            }
        } else {
            out.push_str(line.newline);
        }
    }
    out
}

fn append_panel_section(output: &mut String, appended: &[(&str, &str)]) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(PANEL_SECTION);
    output.push('\n');
    for (key, value) in appended {
        output.push_str(key);
        output.push_str(" = ");
        output.push_str(value);
        output.push('\n');
    }
}

fn line_env(line: &str) -> Option<&'static str> {
    let before_comment = line
        .split_once('#')
        .map(|(before, _)| before)
        .unwrap_or(line);
    let (key, _) = before_comment.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    config_key_to_env(key)
}

fn replace_line_value(line: &str, value: &str) -> String {
    let comment_start = line.find('#');
    let value_end = comment_start.unwrap_or(line.len());
    let Some(eq_index) = line[..value_end].find('=') else {
        return line.to_owned();
    };
    let value_start = eq_index + 1;
    let leading_ws_len = line[value_start..value_end]
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>();
    let prefix_end = value_start + leading_ws_len;
    let trailing_ws = line[..value_end]
        .chars()
        .rev()
        .take_while(|ch| ch.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>();
    let suffix_start = value_end.saturating_sub(trailing_ws);

    let mut out = String::new();
    out.push_str(&line[..prefix_end]);
    out.push_str(value);
    out.push_str(&line[suffix_start..]);
    out
}

fn comment_out_setting(line: &str) -> String {
    if line.trim_start().starts_with('#') {
        return line.to_owned();
    }
    format!("# disabled by OdyTTY settings panel: {line}")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigWritebackError> {
    let parent = path.parent().ok_or_else(|| {
        ConfigWritebackError::new(format!(
            "could not resolve parent directory for {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        ConfigWritebackError::with_source(format!("could not create {}", parent.display()), error)
    })?;

    let temp_path = create_temp_path(path);
    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o644))?;
        }
        fs::rename(&temp_path, path)?;
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(ConfigWritebackError::with_source(
            format!("could not write {}", path.display()),
            error,
        ));
    }
    Ok(())
}

fn create_temp_path(path: &Path) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("odytty.conf");
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        counter
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_preserves_comments_unknowns_and_updates_last_matching_key() {
        let changes = canonical_changes(&[
            SettingEdit {
                key: "font_size",
                env: super::super::FONT_SIZE_ENV,
                value: "21".to_owned(),
            },
            SettingEdit {
                key: "theme",
                env: super::super::THEME_ENV,
                value: "odyssey".to_owned(),
            },
        ]);
        let output = rewrite_config(
            "# header\nfont_size = 16 # keep unit\nunknown_future = yes\nfont_size = 18\n",
            &changes,
        );

        assert!(output.contains("# header"));
        assert!(output.contains("unknown_future = yes"));
        assert!(output.contains("font_size = 16 # keep unit"));
        assert!(output.contains("font_size = 21\n"));
        assert!(output.contains("theme = odyssey\n"));
    }

    #[test]
    fn rewrite_comments_out_cleared_keys() {
        let changes = canonical_changes(&[SettingEdit {
            key: "font",
            env: super::super::FONT_ENV,
            value: String::new(),
        }]);
        let output = rewrite_config("font = fonts/Old.ttf\nfont = fonts/New.ttf\n", &changes);

        assert!(output.contains("# disabled by OdyTTY settings panel: font = fonts/Old.ttf"));
        assert!(output.contains("# disabled by OdyTTY settings panel: font = fonts/New.ttf"));
    }
}
