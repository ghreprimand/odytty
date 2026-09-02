// SPDX-License-Identifier: GPL-3.0-only
//! Bounded, presentation-only read model for the unified session navigator.
//!
//! This module deliberately borrows no live terminal output.  It snapshots
//! structural metadata owned by `WorkspaceSet` and the detached-session
//! registry when the navigator opens; commands continue to operate through
//! their existing owners after an explicit selection.

use crate::session_host::ListedSession;

use super::session::{SessionToken, WorkspaceSet};

/// Snapshot safety cap. Display remains bounded by the picker's independent
/// result cap, so later rows can still be found by filtering.
pub(super) const MAX_NAVIGATOR_ROWS: usize = 1024;
pub(super) const MAX_RECENTLY_CLOSED: usize = 16;

/// A process-lifetime launch descriptor, not a suspended terminal. Reopen
/// creates a fresh shell at the captured directory/profile and restores only
/// the user-visible tab/workspace title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClosedNavigatorItem {
    pub(super) kind: ClosedNavigatorKind,
    pub(super) title: String,
    pub(super) cwd: Option<String>,
    pub(super) profile: Option<String>,
    /// The source workspace's rail position for tab records. Informational only:
    /// positions are intentionally not treated as persistent identities.
    pub(super) workspace_id: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClosedNavigatorKind {
    Tab,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NavigatorTarget {
    /// The first live pane token gives a workspace a stable identity without
    /// inventing parallel workspace ids.
    Workspace(SessionToken),
    /// A tab's focused pane is its existing stable identity.
    Tab(SessionToken),
    Live(SessionToken),
    Detached(String),
}

impl NavigatorTarget {
    pub(super) fn stable_id(&self) -> String {
        match self {
            Self::Workspace(token) => format!("workspace:{}", token.0),
            Self::Tab(token) => format!("tab:{}", token.0),
            Self::Live(token) => format!("live:{}", token.0),
            Self::Detached(id) => format!("detached:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NavigatorEntry {
    pub(super) target: NavigatorTarget,
    pub(super) stable_id: String,
    pub(super) name: String,
    pub(super) detail: String,
    pub(super) status: String,
    pub(super) unread: bool,
    pub(super) profile: Option<String>,
    /// Frozen, redacted last-eight-line preview. Empty unless explicitly
    /// requested while opening the navigator.
    pub(super) preview: Vec<String>,
}

/// Navigator commands carry only existing stable ownership identifiers. The
/// App resolves them through WorkspaceSet and its established command paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NavigatorAction {
    Rename(NavigatorTarget),
    Duplicate(NavigatorTarget),
    Move(NavigatorTarget),
    Close(NavigatorTarget),
    Reopen,
}

impl NavigatorEntry {
    pub(super) fn searchable_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.name, self.detail, self.status, self.stable_id
        )
    }

    pub(super) fn row_label(&self) -> String {
        let unread = if self.unread { " unread" } else { "" };
        let profile = self
            .profile
            .as_deref()
            .map(|profile| format!(" profile:{}", sanitize(profile)))
            .unwrap_or_default();
        let identifier = match &self.target {
            NavigatorTarget::Detached(id) if self.name != *id => format!(" ({})", sanitize(id)),
            NavigatorTarget::Workspace(_) | NavigatorTarget::Tab(_) | NavigatorTarget::Live(_) => {
                String::new()
            }
            NavigatorTarget::Detached(_) => String::new(),
        };
        format!(
            "{}{}  {}  {}{}{}",
            sanitize(&self.name),
            identifier,
            sanitize(&self.detail),
            sanitize(&self.status),
            unread,
            profile
        )
    }
}

impl From<ListedSession> for NavigatorEntry {
    fn from(session: ListedSession) -> Self {
        let unavailable = session.state == "error" || session.pane_count == 0;
        Self {
            stable_id: format!("detached:{}", session.id),
            target: NavigatorTarget::Detached(session.id),
            name: session.name,
            detail: format!("detached {} panes", session.pane_count),
            status: if unavailable {
                "session unavailable (stale registry entry)".to_owned()
            } else {
                session.state.to_owned()
            },
            unread: false,
            profile: None,
            preview: vec!["preview unavailable".to_owned()],
        }
    }
}

/// Snapshot every live pane in workspace, tab, pane order.  The only terminal
/// read is the bounded OSC 7 working directory; no screen cells or command
/// output enter the read model.
pub(super) fn live_entries(set: &WorkspaceSet, include_preview: bool) -> Vec<NavigatorEntry> {
    let mut entries = Vec::new();
    for (workspace_index, workspace) in set.workspaces.iter().enumerate() {
        let Some(workspace_token) = workspace
            .tabs
            .first()
            .and_then(|tab| tab.layout.leaves().into_iter().next())
        else {
            continue;
        };
        entries.push(NavigatorEntry {
            target: NavigatorTarget::Workspace(workspace_token),
            stable_id: format!("workspace:{}", workspace_token.0),
            name: workspace.name.clone(),
            detail: format!("workspace {} tabs", workspace.tabs.len()),
            status: progress_status(set.workspace_progress(workspace_index)),
            unread: set.workspace_has_activity(workspace_index),
            profile: workspace.launch_profile.clone(),
            preview: Vec::new(),
        });
        for tab in &workspace.tabs {
            entries.push(NavigatorEntry {
                target: NavigatorTarget::Tab(tab.focused),
                stable_id: format!("tab:{}", tab.focused.0),
                name: tab
                    .title_override
                    .clone()
                    .or_else(|| {
                        set.sessions
                            .get(&tab.focused)
                            .map(|session| session.tab_title.clone())
                    })
                    .unwrap_or_else(|| "Untitled tab".to_owned()),
                detail: format!("tab {} panes", tab.layout.pane_count()),
                status: progress_status(
                    tab.layout
                        .leaves()
                        .into_iter()
                        .filter_map(|token| set.sessions.get(&token))
                        .find_map(|session| session.attention.progress),
                ),
                unread: tab.activity,
                profile: None,
                preview: Vec::new(),
            });
            for token in tab.layout.leaves() {
                let Some(session) = set.sessions.get(&token) else {
                    continue;
                };
                let remote = session.remote_destination.as_deref();
                let location = remote.map(redacted_remote_identity).unwrap_or_else(|| {
                    session
                        .terminal
                        .lock()
                        .ok()
                        .and_then(|terminal| terminal.current_working_directory().map(bound))
                        .unwrap_or_else(|| "cwd unavailable".to_owned())
                });
                let class = if remote.is_some() {
                    "remote"
                } else if session.attached_session_id.is_some() {
                    "attached"
                } else {
                    "local"
                };
                let status = if session.awaiting_reconnect {
                    "error"
                } else if session.pump_thread.is_some() {
                    if session.attention.has_badge() || tab.activity {
                        "running"
                    } else {
                        "idle"
                    }
                } else {
                    "exited"
                };
                entries.push(NavigatorEntry {
                    target: NavigatorTarget::Live(token),
                    stable_id: format!("live:{}", token.0),
                    name: tab
                        .title_override
                        .clone()
                        .unwrap_or_else(|| session.tab_title.clone()),
                    detail: format!("{class} {}", bound(&location)),
                    status: status.to_owned(),
                    unread: tab.activity || session.attention.unread,
                    profile: session.launch_profile.clone(),
                    preview: if include_preview {
                        preview_lines(session.last_presented_snapshot.as_ref())
                    } else {
                        Vec::new()
                    },
                });
                if entries.len() >= MAX_NAVIGATOR_ROWS {
                    return entries;
                }
            }
        }
    }
    entries
}

/// Unix only: the detached-session registry has no Windows surface before
/// Phase 11, so this helper is compiled out there rather than left dead.
#[cfg(unix)]
pub(super) fn append_detached(entries: &mut Vec<NavigatorEntry>, sessions: Vec<ListedSession>) {
    entries.extend(
        sessions
            .into_iter()
            .map(NavigatorEntry::from)
            .take(MAX_NAVIGATOR_ROWS.saturating_sub(entries.len())),
    );
}

fn progress_status(progress: Option<crate::core::TerminalProgress>) -> String {
    let Some(progress) = progress else {
        return "running".to_owned();
    };
    match progress.value {
        Some(value) => format!("progress {value}%"),
        None => format!("progress {:?}", progress.kind).to_ascii_lowercase(),
    }
}

fn preview_lines(snapshot: Option<&crate::core::Snapshot>) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return vec!["preview unavailable".to_owned()];
    };
    let columns = snapshot.dimensions.columns;
    if columns == 0 {
        return vec!["preview unavailable".to_owned()];
    }
    snapshot
        .cells
        .chunks(columns)
        .rev()
        .take(8)
        .map(|row| redact_preview(&row.iter().map(|cell| cell.ch).collect::<String>()))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn redact_preview(value: &str) -> String {
    let value = bound(value);
    if value.to_ascii_uppercase().contains("PRIVATE KEY") {
        return "[redacted]".to_owned();
    }
    let words: Vec<_> = value.split_whitespace().collect();
    let mut redacted = Vec::with_capacity(words.len());
    let mut index = 0;
    while let Some(word) = words.get(index) {
        if let Some((key, has_value)) = sensitive_assignment_key(word) {
            if !has_value
                && key
                    .trim_end_matches(':')
                    .eq_ignore_ascii_case("authorization")
                && words
                    .get(index + 1)
                    .is_some_and(|next| next.eq_ignore_ascii_case("bearer"))
                && words.get(index + 2).is_some()
            {
                redacted.push(format!("{key} Bearer [redacted]"));
                index += 2;
            } else {
                redacted.push(format!("{key}[redacted]"));
                if !has_value && words.get(index + 1).is_some() {
                    index += 1;
                }
            }
        } else if is_sensitive_label(word) && words.get(index + 1).is_some() {
            redacted.push((*word).to_owned());
            if word
                .trim_end_matches(':')
                .eq_ignore_ascii_case("authorization")
                && words
                    .get(index + 1)
                    .is_some_and(|next| next.eq_ignore_ascii_case("bearer"))
                && words.get(index + 2).is_some()
            {
                redacted.push("Bearer [redacted]".to_owned());
                index += 2;
            } else {
                redacted.push("[redacted]".to_owned());
                index += 1;
            }
        } else if is_base64_or_hex_shaped(word) || (word.contains('@') && word.contains(':')) {
            redacted.push("[redacted]".to_owned());
        } else {
            redacted.push((*word).to_owned());
        }
        index += 1;
    }
    redacted.join(" ")
}

/// Return the displayable `key=`/`key:` prefix for sensitive assignments. The
/// value never enters the preview, while retaining the key makes a redaction
/// useful when diagnosing a command. `export`, `set`, and `$env:` prefixes are
/// naturally split by whitespace or remain part of the assignment key.
fn sensitive_assignment_key(word: &str) -> Option<(&str, bool)> {
    let (delimiter, offset) = if let Some(offset) = word.find('=') {
        ('=', offset)
    } else {
        (':', word.find(':')?)
    };
    let value = &word[offset + delimiter.len_utf8()..];
    if value.is_empty() && delimiter == '=' {
        return None;
    }
    let key = &word[..offset];
    let key = key.strip_prefix("$env:").unwrap_or(key);
    let lower = key.to_ascii_lowercase();
    [
        "pass",
        "pwd",
        "secret",
        "token",
        "key",
        "auth",
        "bearer",
        "credential",
        "cookie",
        "session",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    .then_some((&word[..offset + delimiter.len_utf8()], !value.is_empty()))
}

fn is_sensitive_label(word: &str) -> bool {
    let normalized = word.trim_end_matches(':').to_ascii_lowercase();
    matches!(normalized.as_str(), "authorization" | "bearer")
}

fn is_base64_or_hex_shaped(word: &str) -> bool {
    let token = word.trim_matches(|ch: char| matches!(ch, ',' | ';' | '"' | '\''));
    token.len() >= 20
        && (token.bytes().all(|byte| byte.is_ascii_hexdigit())
            || token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
            }))
}

fn redacted_remote_identity(value: &str) -> String {
    let host = value.rsplit('@').next().unwrap_or(value);
    let host = host.split(':').next().unwrap_or(host);
    format!("remote {}", bound(host))
}

fn bound(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(96)
        .collect()
}

fn sanitize(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_identity_redacts_user_and_port() {
        assert_eq!(
            redacted_remote_identity("operator@example.test:2200"),
            "remote example.test"
        );
    }

    #[test]
    fn detached_rows_keep_registry_ids_as_stable_targets() {
        let entry = NavigatorEntry::from(ListedSession {
            id: "s-001".to_owned(),
            name: "build".to_owned(),
            state: "idle",
            age_ms: 0,
            pane_count: 2,
        });
        assert_eq!(entry.target, NavigatorTarget::Detached("s-001".to_owned()));
        assert!(entry.row_label().contains("detached 2 panes"));
    }

    #[test]
    fn preview_redacts_assignments_and_remote_identity() {
        let preview = redact_preview("token=secret demo@example.test:2222 visible");
        assert_eq!(preview, "token=[redacted] [redacted] visible");
    }

    #[test]
    fn preview_redacts_every_sensitive_assignment_family_case_insensitively() {
        for (input, expected_key) in [
            ("PASSWORD=hidden", "PASSWORD="),
            ("pwd=hidden", "pwd="),
            ("AWS_SECRET=hidden", "AWS_SECRET="),
            ("TOKEN=hidden", "TOKEN="),
            ("PRIVATE_KEY=hidden", "PRIVATE_KEY="),
            ("apikey=hidden", "apikey="),
            ("api_key=hidden", "api_key="),
            ("AUTH=hidden", "AUTH="),
            ("credential=hidden", "credential="),
            ("cookie=hidden", "cookie="),
            ("session=hidden", "session="),
        ] {
            let preview = redact_preview(input);
            assert_eq!(preview, format!("{expected_key}[redacted]"));
        }
    }

    #[test]
    fn preview_redacts_export_set_env_and_authorization_forms() {
        assert_eq!(
            redact_preview("export AWS_SECRET=hidden"),
            "export AWS_SECRET=[redacted]"
        );
        assert_eq!(redact_preview("set TOKEN=hidden"), "set TOKEN=[redacted]");
        assert_eq!(
            redact_preview("$env:PRIVATE_KEY=hidden"),
            "$env:PRIVATE_KEY=[redacted]"
        );
        assert_eq!(
            redact_preview("Authorization: Bearer hidden"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(redact_preview("Bearer hidden"), "Bearer [redacted]");
    }

    #[test]
    fn preview_redacts_long_token_shapes_without_a_sensitive_key() {
        assert_eq!(
            redact_preview("value abcdef0123456789abcdef0123456789"),
            "value [redacted]"
        );
        assert_eq!(
            redact_preview("value QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
            "value [redacted]"
        );
    }

    #[test]
    fn preview_keeps_non_sensitive_shell_text() {
        assert_eq!(
            redact_preview("PATH=/usr/bin ls -la"),
            "PATH=/usr/bin ls -la"
        );
    }
}
