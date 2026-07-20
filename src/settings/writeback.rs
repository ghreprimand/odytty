// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeMap;
#[cfg(test)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    SettingEdit, config::config_key_to_env, config::env_to_config_key, config::quote_config_value,
    config::strip_inline_comment, config_file_path,
};

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

    let existing = match super::fs_read::read_capped(path) {
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

/// First-run marker written when the user dismisses the onboarding card without
/// having saved any setting. Comment-only, so it parses to all defaults; its
/// mere existence is what suppresses the onboarding card on later launches
/// (onboarding's first-run gate is "does `odytty.conf` exist").
const FIRST_RUN_STUB: &str = concat!(
    "# OdyTTY configuration\n",
    "# Created on first launch. Edit settings here or use the in-app settings\n",
    "# panel. This file existing is what stops the welcome card from reshowing.\n",
);

/// Ensure the user's `odytty.conf` exists so first-run onboarding does not
/// reshow after the user dismisses the welcome card. Resolves the path with the
/// same rules as [`write_settings_changes`].
pub fn ensure_config_file_exists() -> Result<ConfigWritebackResult, ConfigWritebackError> {
    let path = config_file_path().ok_or_else(|| {
        ConfigWritebackError::new("could not resolve odytty.conf path; set XDG_CONFIG_HOME or HOME")
    })?;
    ensure_config_file_exists_at(&path)
}

/// Path-explicit form of [`ensure_config_file_exists`]. No-op (reports
/// `changed: 0`) when the file already exists, so it never clobbers user
/// content; otherwise atomically writes a comment-only first-run stub.
pub fn ensure_config_file_exists_at(
    path: &Path,
) -> Result<ConfigWritebackResult, ConfigWritebackError> {
    if path.exists() {
        return Ok(ConfigWritebackResult {
            path: path.to_path_buf(),
            changed: 0,
        });
    }
    atomic_write(path, FIRST_RUN_STUB.as_bytes())?;
    Ok(ConfigWritebackResult {
        path: path.to_path_buf(),
        changed: 1,
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

    // A visibility-only edit (`workspace_rail` -> auto|always) that replaces a
    // line still encoding the side as `workspace_rail = left|right` would drop
    // that side. Capture it now so it can be materialized into the canonical side
    // key after the rewrite, unless the same save also writes the side directly.
    let folded_side_to_preserve = folded_workspace_rail_side_to_preserve(&lines, &env_by_key);

    let mut matching = BTreeMap::<&str, Vec<usize>>::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(env) = line_env(&line.text) else {
            continue;
        };
        if env_by_key.contains_key(env) {
            matching.entry(env).or_default().push(index);
        }
    }

    let mut appended: Vec<(&'static str, String)> = Vec::new();
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
            None if !value.is_empty() => appended.push((*key, (*value).to_owned())),
            None => {}
        }
    }

    // Reads prefer the canonical `WORKSPACE_RAIL_*`/`workspace_rail_side` member of
    // each rail alias family, so a written legacy key alone leaves a standing
    // canonical twin that silently reverts the change on hot-reload. Comment out
    // the twins of every written member so the just-written value is the one that
    // survives.
    reconcile_alias_family_shadows(&mut lines, &env_by_key);

    // Writing the side explicitly must also drop the side carried by a standing
    // `workspace_rail = left|right`, which otherwise shadows the written side.
    // Its visibility intent (Always) is preserved by rewriting it to `always`.
    neutralize_workspace_rail_side_fold(&mut lines, &env_by_key);

    // Materialize a preserved folded side so a visibility-only edit keeps the side.
    if let Some(side) = folded_side_to_preserve {
        materialize_workspace_rail_side(&mut lines, &mut appended, side);
    }

    let mut output = join_lines(&lines, contents.ends_with('\n'));
    if !appended.is_empty() {
        append_panel_section(&mut output, &appended);
    }
    output
}

/// Rail settings whose canonical `WORKSPACE_RAIL_*`/`workspace_rail_side` key and
/// legacy `TAB_RAIL_*`/`tab_bar_placement` key resolve to the SAME `Settings`
/// field. The panel and the mouse-resize persistence write the legacy member,
/// while reads prefer the canonical one, so writing without reconciling the twin
/// lets a standing canonical line revert the edit on the next hot-reload.
const RAIL_ALIAS_FAMILIES: &[&[&str]] = &[
    &[super::WORKSPACE_RAIL_SIDE_ENV, super::TAB_BAR_PLACEMENT_ENV],
    &[super::WORKSPACE_RAIL_WIDTH_ENV, super::TAB_RAIL_WIDTH_ENV],
    &[
        super::WORKSPACE_RAIL_MAX_WIDTH_ENV,
        super::TAB_RAIL_MAX_WIDTH_ENV,
    ],
    &[super::WORKSPACE_RAIL_GAP_ENV, super::TAB_RAIL_GAP_ENV],
    &[
        super::WORKSPACE_RAIL_SLOT_ROWS_ENV,
        super::TAB_RAIL_SLOT_ROWS_ENV,
    ],
    &[
        super::WORKSPACE_RAIL_AUTOHIDE_ENV,
        super::TAB_RAIL_AUTOHIDE_ENV,
    ],
    &[
        super::WORKSPACE_RAIL_REVEAL_PX_ENV,
        super::TAB_RAIL_REVEAL_PX_ENV,
    ],
];

/// Comment out any active conf line that is an alias twin of a written rail key
/// but is not itself part of this save, so the canonical-vs-legacy read
/// precedence cannot resurrect an old value over the one just written.
fn reconcile_alias_family_shadows(
    lines: &mut [ConfigLine],
    env_by_key: &BTreeMap<&'static str, (&'static str, &str)>,
) {
    for family in RAIL_ALIAS_FAMILIES {
        let writes_member = family.iter().any(|env| env_by_key.contains_key(env));
        if !writes_member {
            continue;
        }
        for line in lines.iter_mut() {
            let Some(env) = line_env(&line.text) else {
                continue;
            };
            // Comment out a twin only when it is a different family member than
            // any explicitly written one; a member being written is handled by
            // the main rewrite loop.
            if family.contains(&env) && !env_by_key.contains_key(env) {
                line.text = comment_out_setting(&line.text);
            }
        }
    }
}

/// The `left|right` side carried by a standing `workspace_rail` line that a
/// visibility-only edit is about to overwrite, when the same save does not also
/// set the side directly. `None` when there is nothing to preserve.
fn folded_workspace_rail_side_to_preserve(
    lines: &[ConfigLine],
    env_by_key: &BTreeMap<&'static str, (&'static str, &str)>,
) -> Option<String> {
    let (_, new_value) = env_by_key.get(super::WORKSPACE_RAIL_ENV)?;
    // Only a switch to a non-side visibility value can drop the folded side.
    if is_rail_side_value(new_value) {
        return None;
    }
    // An explicit side write in the same save already carries the side.
    if env_by_key.contains_key(super::WORKSPACE_RAIL_SIDE_ENV)
        || env_by_key.contains_key(super::TAB_BAR_PLACEMENT_ENV)
    {
        return None;
    }
    lines.iter().rev().find_map(|line| {
        let env = line_env(&line.text)?;
        if env != super::WORKSPACE_RAIL_ENV {
            return None;
        }
        let value = line_value(&line.text)?;
        is_rail_side_value(value).then(|| value.to_ascii_lowercase())
    })
}

/// Drop the shadowing side from a standing `workspace_rail = left|right` line when
/// the side is being written explicitly, keeping its visibility intent as
/// `always`. No-op unless a side member is part of this save.
fn neutralize_workspace_rail_side_fold(
    lines: &mut [ConfigLine],
    env_by_key: &BTreeMap<&'static str, (&'static str, &str)>,
) {
    let writes_side = env_by_key.contains_key(super::WORKSPACE_RAIL_SIDE_ENV)
        || env_by_key.contains_key(super::TAB_BAR_PLACEMENT_ENV);
    if !writes_side || env_by_key.contains_key(super::WORKSPACE_RAIL_ENV) {
        return;
    }
    for line in lines.iter_mut() {
        let Some(env) = line_env(&line.text) else {
            continue;
        };
        if env != super::WORKSPACE_RAIL_ENV {
            continue;
        }
        if line_value(&line.text).is_some_and(is_rail_side_value) {
            line.text = replace_line_value(&line.text, "always");
        }
    }
}

/// Ensure the canonical `workspace_rail_side` key carries `side`: update an
/// existing line in place, or append one to the panel section.
fn materialize_workspace_rail_side(
    lines: &mut [ConfigLine],
    appended: &mut Vec<(&'static str, String)>,
    side: String,
) {
    for line in lines.iter_mut() {
        if line_env(&line.text) == Some(super::WORKSPACE_RAIL_SIDE_ENV) {
            line.text = replace_line_value(&line.text, &side);
            return;
        }
    }
    appended.push(("workspace_rail_side", side));
}

/// A rail-side token (`left`/`right`, case-insensitive) as accepted by the legacy
/// `workspace_rail = left|right` side syntax.
fn is_rail_side_value(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "left" | "right")
}

/// The active value of a `key = value` line, trimmed and stripped of any trailing
/// `# comment`. `None` for comment-only or valueless lines.
fn line_value(line: &str) -> Option<&str> {
    let before_comment = strip_inline_comment(line);
    let (key, value) = before_comment.split_once('=')?;
    if key.trim().is_empty() {
        return None;
    }
    let value = value.trim();
    (!value.is_empty()).then_some(value)
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

fn append_panel_section(output: &mut String, appended: &[(&'static str, String)]) {
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
        // C36: quote a value that would break the line format on read-back
        // (contains `#`, a quote, or edge whitespace); ordinary values are
        // written verbatim, so the file stays byte-identical for them.
        output.push_str(&quote_config_value(value));
        output.push('\n');
    }
}

fn line_env(line: &str) -> Option<&'static str> {
    let before_comment = strip_inline_comment(line);
    let (key, _) = before_comment.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    config_key_to_env(key)
}

fn replace_line_value(line: &str, value: &str) -> String {
    // C36: locate the comment quote-aware, so a `#` inside the existing quoted
    // value is not mistaken for a comment start.
    let content = strip_inline_comment(line);
    let comment_start = (content.len() < line.len()).then_some(content.len());
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
    // C36: quote the replacement value when it would otherwise break the format.
    out.push_str(&quote_config_value(value));
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
    // Route through the shared policy-driven writer under the Config policy: an
    // exclusively-created temp, data + parent-directory fsync, and an atomic
    // rename. Unlike the previous local writer this PRESERVES a stricter existing
    // mode (a config the user tightened to 0600 is no longer widened to 0644);
    // a new file still lands at 0644.
    crate::state_dir::write_atomic(path, bytes, crate::state_dir::WriteMode::Config).map_err(
        |error| {
            ConfigWritebackError::with_source(format!("could not write {}", path.display()), error)
        },
    )
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
    fn value_with_hash_round_trips_through_write_and_read() {
        use super::super::config::ConfigValues;
        // A path value containing '#' must survive a write then a config read
        // without being truncated at the '#' (C36).
        let path_value = "/photos/#1 best/wall.png";
        let changes = canonical_changes(&[SettingEdit {
            key: "background_image",
            env: super::super::BACKGROUND_IMAGE_ENV,
            value: path_value.to_owned(),
        }]);

        // Append path (no existing line): quoted on write, honored on read.
        let output = rewrite_config("# existing config\n", &changes);
        let parsed = ConfigValues::parse(&output, |_| {});
        assert_eq!(
            parsed
                .get(super::super::BACKGROUND_IMAGE_ENV)
                .and_then(|v| v.to_str()),
            Some(path_value),
            "appended '#' value round-trips through write + read: {output:?}"
        );

        // Replace path (existing line edited in place): same round-trip, and the
        // trailing comment is preserved because the '#' locator is quote-aware.
        let output2 = rewrite_config("background_image = /old/path.png # note\n", &changes);
        let parsed2 = ConfigValues::parse(&output2, |_| {});
        assert_eq!(
            parsed2
                .get(super::super::BACKGROUND_IMAGE_ENV)
                .and_then(|v| v.to_str()),
            Some(path_value),
            "in-place edit with a '#' value round-trips: {output2:?}"
        );
        assert!(
            output2.contains("# note"),
            "the trailing comment survives the in-place edit: {output2:?}"
        );
    }

    fn side_edit(value: &str) -> Vec<SettingEdit> {
        vec![SettingEdit {
            key: "tab_bar_placement",
            env: super::super::TAB_BAR_PLACEMENT_ENV,
            value: value.to_owned(),
        }]
    }

    #[test]
    fn canonical_side_shadow_is_cleared_when_legacy_side_is_written() {
        // Repro 1: a standing canonical `workspace_rail_side = right` would win on
        // hot-reload and revert a panel Rail-side edit that writes the legacy
        // `tab_bar_placement` key. Reconciliation must clear the shadow.
        let changes = canonical_changes(&side_edit("left"));
        let output = rewrite_config("workspace_rail_side = right\n", &changes);

        assert!(
            output.contains("# disabled by OdyTTY settings panel: workspace_rail_side = right"),
            "the canonical side twin must be commented out, not left shadowing: {output:?}"
        );
        assert!(
            output.contains("tab_bar_placement = left"),
            "the written side must be present: {output:?}"
        );
        // No active canonical side line remains to revert the edit.
        assert!(!output.lines().any(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with('#') && trimmed.starts_with("workspace_rail_side")
        }));
    }

    #[test]
    fn canonical_width_shadow_is_cleared_when_legacy_width_is_written() {
        // Repro 2: a seam-drag persists the legacy `tab_rail_width`; a standing
        // canonical `workspace_rail_width` must not snap the width back on reload.
        let changes = canonical_changes(&[SettingEdit {
            key: "tab_rail_width",
            env: super::super::TAB_RAIL_WIDTH_ENV,
            value: "20".to_owned(),
        }]);
        let output = rewrite_config("workspace_rail_width = 30\n", &changes);

        assert!(
            output.contains("# disabled by OdyTTY settings panel: workspace_rail_width = 30"),
            "the canonical width twin must be commented out: {output:?}"
        );
        assert!(output.contains("tab_rail_width = 20"));
    }

    #[test]
    fn visibility_only_edit_preserves_folded_side() {
        // Repro 3: pre-0.8.5 `workspace_rail = right` encodes the side; setting the
        // visibility to auto must not silently drop that side to the default left.
        let changes = canonical_changes(&[SettingEdit {
            key: "workspace_rail",
            env: super::super::WORKSPACE_RAIL_ENV,
            value: "auto".to_owned(),
        }]);
        let output = rewrite_config("workspace_rail = right\n", &changes);

        assert!(
            output.contains("workspace_rail = auto"),
            "visibility must be updated: {output:?}"
        );
        assert!(
            output.contains("workspace_rail_side = right"),
            "the folded side must be materialized into the canonical key: {output:?}"
        );
    }

    #[test]
    fn explicit_side_edit_neutralizes_workspace_rail_side_fold() {
        // The mirror of repro 3: a standing `workspace_rail = right` folds to
        // side=right and would shadow a panel Rail-side edit. Writing the side must
        // convert the fold to plain `always` (keeping its visibility intent) so the
        // written side wins.
        let changes = canonical_changes(&side_edit("left"));
        let output = rewrite_config("workspace_rail = right\n", &changes);

        assert!(
            output.contains("workspace_rail = always"),
            "the side fold must become plain visibility: {output:?}"
        );
        assert!(output.contains("tab_bar_placement = left"));
        assert!(
            !output
                .lines()
                .any(|line| line.trim() == "workspace_rail = right"),
            "no left|right side fold may remain to shadow the written side: {output:?}"
        );
    }

    #[test]
    fn side_edit_leaves_unrelated_rail_keys_untouched() {
        // Family reconciliation must be scoped to the edited field: a width line is
        // not a side twin and must survive a side edit.
        let changes = canonical_changes(&side_edit("left"));
        let output = rewrite_config("workspace_rail_width = 20\nfont_size = 16\n", &changes);

        assert!(output.contains("workspace_rail_width = 20"));
        assert!(output.contains("font_size = 16"));
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

    #[test]
    fn ensure_config_file_creates_when_missing_and_preserves_when_present() {
        let dir = std::env::temp_dir().join(format!("odytty-ensure-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("odytty.conf");

        // Missing -> created with the first-run stub, reported as one change.
        assert!(!path.exists());
        let created = ensure_config_file_exists_at(&path).expect("create config");
        assert_eq!(created.changed, 1);
        assert!(path.exists());
        assert!(
            fs::read_to_string(&path)
                .expect("read stub")
                .contains("OdyTTY configuration")
        );

        // Already present -> no-op; never clobbers existing user content.
        fs::write(&path, "font_size = 22\n").expect("write user conf");
        let again = ensure_config_file_exists_at(&path).expect("idempotent");
        assert_eq!(again.changed, 0);
        assert_eq!(
            fs::read_to_string(&path).expect("reread"),
            "font_size = 22\n"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
