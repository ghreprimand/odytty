// SPDX-License-Identifier: GPL-3.0-only
//! Hand-rolled parsers for the freedesktop text formats the "Open With…"
//! enumeration needs (C3b): `.desktop` entries, `mimeapps.list`, and
//! `mimeinfo.cache`. All three are the same INI-ish `key=value` grouped format,
//! so one small group reader backs them — no new dependency, no `ini` crate.
//!
//! Pure and std-only: every function takes the file's text and returns parsed
//! values, so the whole layer is testable on in-memory fixtures with zero real
//! filesystem access.

/// The fields of a `[Desktop Entry]` group the picker cares about. Localized
/// keys (`Name[de]`) are ignored for v1 — the plain `Name` is enough.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct DesktopEntry {
    pub(super) name: Option<String>,
    pub(super) exec: Option<String>,
    pub(super) type_field: Option<String>,
    pub(super) no_display: bool,
    pub(super) hidden: bool,
    pub(super) terminal: bool,
}

impl DesktopEntry {
    /// Whether this entry should be OFFERED in the picker (C3b filter rule):
    /// `Type=Application`, not `NoDisplay`, not `Hidden`, not `Terminal=true`
    /// (TTY-owning apps misbehave when launched detached with null stdio — an
    /// explicit v1 exclusion), and a non-empty `Exec`. A failing entry is simply
    /// dropped from the list (never an error).
    pub(super) fn is_offerable(&self) -> bool {
        self.type_field.as_deref() == Some("Application")
            && !self.no_display
            && !self.hidden
            && !self.terminal
            && self.exec.as_deref().is_some_and(|e| !e.trim().is_empty())
    }
}

/// Parse the `[Desktop Entry]` group out of a `.desktop` file's text. Only the
/// first `[Desktop Entry]` group is read; "Desktop Action" groups and any other
/// group are ignored. A malformed line is skipped, never panicked.
pub(super) fn parse_desktop_entry(text: &str) -> DesktopEntry {
    let mut entry = DesktopEntry::default();
    let mut in_group = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(group) = group_header(line) {
            // Stop once we leave the [Desktop Entry] group we already entered,
            // so action groups cannot overwrite the primary fields.
            if in_group {
                break;
            }
            in_group = group == "Desktop Entry";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = split_key_value(line) else {
            continue;
        };
        // Skip localized variants (`Name[de]`) — plain keys only for v1.
        if key.contains('[') {
            continue;
        }
        match key {
            "Name" => entry.name = Some(value.to_owned()),
            "Exec" => entry.exec = Some(value.to_owned()),
            "Type" => entry.type_field = Some(value.to_owned()),
            "NoDisplay" => entry.no_display = parse_bool(value),
            "Hidden" => entry.hidden = parse_bool(value),
            "Terminal" => entry.terminal = parse_bool(value),
            _ => {}
        }
    }
    entry
}

/// Parse a single `[Section]` value (`mime/type=a.desktop;b.desktop;`) out of a
/// `mimeapps.list` / `mimeinfo.cache`-style file for the given section name and
/// MIME type. Returns the desktop ids in file order. A missing section or key
/// yields an empty vector. Bounded by the file's own size (the caller caps the
/// read).
pub(super) fn parse_association_list(text: &str, section: &str, mime: &str) -> Vec<String> {
    let mut in_section = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(group) = group_header(line) {
            in_section = group == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = split_key_value(line) else {
            continue;
        };
        if key == mime {
            return split_desktop_ids(value);
        }
    }
    Vec::new()
}

/// Split a `;`-separated desktop-id list, trimming whitespace and dropping empty
/// fields (a trailing `;` is conventional and must not yield an empty id).
fn split_desktop_ids(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The group name inside a `[...]` header line, or `None` if the line is not a
/// well-formed header.
fn group_header(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    rest.strip_suffix(']')
}

/// Split a `key=value` line at the first `=`. The key is trimmed; the value is
/// taken verbatim after the `=` (Desktop-Entry values are not whitespace-trimmed
/// on the right because trailing spaces can be significant, but we trim the key).
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let idx = line.find('=')?;
    let key = line[..idx].trim();
    let value = &line[idx + 1..];
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

/// freedesktop booleans are the literal strings `true` / `false`; anything else
/// (including `1`/`0`, which are NOT valid per the current spec) is `false`.
fn parse_bool(value: &str) -> bool {
    value.trim() == "true"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_desktop_entry() {
        let text = "\
[Desktop Entry]
Type=Application
Name=Image Viewer
Exec=eog %f
";
        let entry = parse_desktop_entry(text);
        assert_eq!(entry.name.as_deref(), Some("Image Viewer"));
        assert_eq!(entry.exec.as_deref(), Some("eog %f"));
        assert_eq!(entry.type_field.as_deref(), Some("Application"));
        assert!(entry.is_offerable());
    }

    #[test]
    fn action_group_does_not_override_primary() {
        let text = "\
[Desktop Entry]
Type=Application
Name=Real
Exec=real %f
[Desktop Action new]
Name=New Window
Exec=real --new
";
        let entry = parse_desktop_entry(text);
        assert_eq!(entry.name.as_deref(), Some("Real"));
        assert_eq!(entry.exec.as_deref(), Some("real %f"));
    }

    #[test]
    fn localized_name_is_ignored_plain_name_kept() {
        let text = "\
[Desktop Entry]
Type=Application
Name=Editor
Name[de]=Editor DE
Exec=ed %f
";
        let entry = parse_desktop_entry(text);
        assert_eq!(entry.name.as_deref(), Some("Editor"));
    }

    #[test]
    fn nodisplay_hidden_terminal_block_offerable() {
        let base = "[Desktop Entry]\nType=Application\nName=X\nExec=x %f\n";
        assert!(parse_desktop_entry(base).is_offerable());
        assert!(!parse_desktop_entry(&format!("{base}NoDisplay=true\n")).is_offerable());
        assert!(!parse_desktop_entry(&format!("{base}Hidden=true\n")).is_offerable());
        assert!(!parse_desktop_entry(&format!("{base}Terminal=true\n")).is_offerable());
    }

    #[test]
    fn non_application_type_not_offerable() {
        let text = "[Desktop Entry]\nType=Link\nName=L\nExec=x %f\n";
        assert!(!parse_desktop_entry(text).is_offerable());
    }

    #[test]
    fn missing_or_empty_exec_not_offerable() {
        let no_exec = "[Desktop Entry]\nType=Application\nName=X\n";
        assert!(!parse_desktop_entry(no_exec).is_offerable());
        let empty_exec = "[Desktop Entry]\nType=Application\nName=X\nExec=   \n";
        assert!(!parse_desktop_entry(empty_exec).is_offerable());
    }

    #[test]
    fn malformed_lines_are_skipped_not_panicked() {
        let text = "\
[Desktop Entry]
this is not a key value
=novalue
Type=Application
Name=Survivor
Exec=s %f
###
";
        let entry = parse_desktop_entry(text);
        assert_eq!(entry.name.as_deref(), Some("Survivor"));
        assert!(entry.is_offerable());
    }

    #[test]
    fn parses_association_list_section() {
        let text = "\
[Default Applications]
image/png=eog.desktop;gimp.desktop;
text/plain=gedit.desktop
[Added Associations]
image/png=krita.desktop;
";
        assert_eq!(
            parse_association_list(text, "Default Applications", "image/png"),
            vec!["eog.desktop".to_owned(), "gimp.desktop".to_owned()]
        );
        assert_eq!(
            parse_association_list(text, "Added Associations", "image/png"),
            vec!["krita.desktop".to_owned()]
        );
        // A MIME not present in the section yields an empty list.
        assert!(parse_association_list(text, "Default Applications", "image/gif").is_empty());
        // A missing section yields an empty list.
        assert!(parse_association_list(text, "Removed Associations", "image/png").is_empty());
    }

    #[test]
    fn association_list_drops_trailing_empty_field() {
        let text = "[MIME Cache]\nimage/png=a.desktop;b.desktop;;\n";
        assert_eq!(
            parse_association_list(text, "MIME Cache", "image/png"),
            vec!["a.desktop".to_owned(), "b.desktop".to_owned()]
        );
    }

    #[test]
    fn bool_parsing_is_strict_true() {
        let one = "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=1\n";
        // `1` is not a valid freedesktop boolean → NoDisplay stays false.
        assert!(!parse_desktop_entry(one).no_display);
    }
}
