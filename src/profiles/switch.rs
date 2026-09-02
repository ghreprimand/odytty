// SPDX-License-Identifier: GPL-3.0-only
//! Optional host/directory-aware named-profile switching.
//!
//! Switching is opt-in via settings, uses only local profile match rules, and
//! never creates or rewrites profiles from terminal output.

use std::path::{Path, PathBuf};

use super::schema::ProfileSwitchRules;
use super::store::ProfileCatalog;

/// Outcome of evaluating whether a pane should adopt a different profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileSwitchSuggestion {
    None,
    Switch {
        profile_name: String,
        reason: ProfileSwitchReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileSwitchReason {
    Host { matched: String },
    Directory { matched: String },
}

/// Evaluate local match rules against the current host and cwd.
///
/// `current_profile` suppresses no-op suggestions. When the current profile's
/// rules still match the context, switching holds steady even if another
/// profile would also match (prevents A<->B flapping on repeated cwd events).
/// `recently_applied` bounds immediate re-entry to a profile that was just left
/// in the same context before `current_profile` catches up.
pub fn suggest_profile_switch<'a>(
    catalog: &ProfileCatalog,
    rules_by_profile: impl IntoIterator<Item = (&'a str, &'a ProfileSwitchRules)>,
    current_profile: Option<&str>,
    host: Option<&str>,
    cwd: Option<&Path>,
    recently_applied: Option<&str>,
) -> ProfileSwitchSuggestion {
    let host = host.map(str::trim).filter(|value| !value.is_empty());
    let cwd = cwd.filter(|path| !path.as_os_str().is_empty());
    let mut profiles: Vec<_> = rules_by_profile.into_iter().collect();
    profiles.sort_by_key(|(name, _)| *name);

    if let Some(current) = current_profile
        && catalog.get(current).is_some()
        && profiles
            .iter()
            .find(|(name, _)| *name == current)
            .is_some_and(|(_, rules)| rules_match_context(rules, host, cwd))
    {
        return ProfileSwitchSuggestion::None;
    }

    for (name, rules) in profiles {
        if recently_applied == Some(name) {
            continue;
        }
        if let Some(matched) = rules
            .match_hosts
            .iter()
            .find(|pattern| host_matches(host, pattern))
            .cloned()
            && catalog.get(name).is_some()
        {
            return ProfileSwitchSuggestion::Switch {
                profile_name: name.to_owned(),
                reason: ProfileSwitchReason::Host { matched },
            };
        }
        if let Some(matched) = rules
            .match_directories
            .iter()
            .find(|pattern| directory_matches(cwd, pattern))
            .cloned()
            && catalog.get(name).is_some()
        {
            return ProfileSwitchSuggestion::Switch {
                profile_name: name.to_owned(),
                reason: ProfileSwitchReason::Directory { matched },
            };
        }
    }
    ProfileSwitchSuggestion::None
}

fn rules_match_context(rules: &ProfileSwitchRules, host: Option<&str>, cwd: Option<&Path>) -> bool {
    rules
        .match_hosts
        .iter()
        .any(|pattern| host_matches(host, pattern))
        || rules
            .match_directories
            .iter()
            .any(|pattern| directory_matches(cwd, pattern))
}

fn host_matches(host: Option<&str>, pattern: &str) -> bool {
    let Some(host) = host else {
        return false;
    };
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        if suffix.is_empty() {
            return false;
        }
        return host.len() >= suffix.len()
            && host[host.len() - suffix.len()..].eq_ignore_ascii_case(suffix);
    }
    host.eq_ignore_ascii_case(pattern)
}

fn directory_matches(cwd: Option<&Path>, pattern: &str) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    let pattern = Path::new(pattern);
    if !is_rooted_pattern(pattern) {
        return false;
    }
    let cwd_path = cwd.to_path_buf();
    let pattern_path = pattern.to_path_buf();
    if cwd_path.starts_with(&pattern_path) {
        return true;
    }
    let mut prefix = pattern_path;
    if !prefix.as_os_str().is_empty() && !prefix.ends_with(std::ffi::OsStr::new("/")) {
        prefix.push("");
    }
    cwd_path.starts_with(prefix)
}

/// Normalize a directory match pattern for storage.
pub fn normalize_directory_pattern(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if !is_rooted_pattern(&path) {
        return None;
    }
    Some(path.to_string_lossy().into_owned())
}

/// A directory pattern must start at a filesystem root: relative and
/// traversal-only patterns are rejected. `has_root` rather than `is_absolute`
/// so a Unix-style `/work/project` pattern stays valid on Windows, where WSL
/// and remote panes report POSIX working directories through OSC 7 while
/// `Path::is_absolute` would demand a drive prefix. Drive-letter patterns
/// (`C:\work`) are rooted as well.
fn is_rooted_pattern(pattern: &Path) -> bool {
    pattern.has_root()
}

/// Normalize a host match pattern for storage.
pub fn normalize_host_pattern(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "*" {
        return Some("*".to_owned());
    }
    if trimmed
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '\\'))
    {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::LaunchProfile;

    fn catalog_with(name: &str, rules: ProfileSwitchRules) -> (ProfileCatalog, ProfileSwitchRules) {
        let mut profile = LaunchProfile::new(name).expect("profile");
        profile.switch = rules.clone();
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert(name.to_owned(), profile);
        (catalog, rules)
    }

    #[test]
    fn host_rule_suggests_matching_profile() {
        let rules = ProfileSwitchRules {
            match_hosts: vec!["devbox".to_owned()],
            match_directories: Vec::new(),
            preserved: Default::default(),
        };
        let (catalog, rules) = catalog_with("dev", rules);
        let suggestion = suggest_profile_switch(
            &catalog,
            std::iter::once(("dev", &rules)),
            None,
            Some("devbox"),
            None,
            None,
        );
        assert!(matches!(
            suggestion,
            ProfileSwitchSuggestion::Switch { ref profile_name, .. } if profile_name == "dev"
        ));
    }

    #[test]
    fn directory_rule_matches_child_paths() {
        let rules = ProfileSwitchRules {
            match_hosts: Vec::new(),
            match_directories: vec!["/work/project".to_owned()],
            preserved: Default::default(),
        };
        let (catalog, rules) = catalog_with("proj", rules);
        let suggestion = suggest_profile_switch(
            &catalog,
            std::iter::once(("proj", &rules)),
            None,
            None,
            Some(Path::new("/work/project/src")),
            None,
        );
        assert!(matches!(suggestion, ProfileSwitchSuggestion::Switch { .. }));
    }

    #[test]
    fn current_profile_suppresses_repeat_switch() {
        let rules = ProfileSwitchRules {
            match_hosts: vec!["devbox".to_owned()],
            match_directories: Vec::new(),
            preserved: Default::default(),
        };
        let (catalog, rules) = catalog_with("dev", rules);
        let suggestion = suggest_profile_switch(
            &catalog,
            std::iter::once(("dev", &rules)),
            Some("dev"),
            Some("devbox"),
            None,
            None,
        );
        assert_eq!(suggestion, ProfileSwitchSuggestion::None);
    }

    // ---- v0.14 Phase A3 adversarial: loop bound, trust, remote-inertness ----

    #[test]
    fn a3_current_profile_holds_when_context_still_matches() {
        // When two profiles share a host rule, the active profile must hold
        // steady across repeated evaluations instead of alternating A<->B.
        let a_rules = ProfileSwitchRules {
            match_hosts: vec!["devbox".to_owned()],
            match_directories: Vec::new(),
            preserved: Default::default(),
        };
        let b_rules = ProfileSwitchRules {
            match_hosts: vec!["devbox".to_owned()],
            match_directories: Vec::new(),
            preserved: Default::default(),
        };
        let mut catalog = ProfileCatalog::default();
        for name in ["a", "b"] {
            let mut profile = LaunchProfile::new(name).expect("profile");
            profile.switch = if name == "a" {
                a_rules.clone()
            } else {
                b_rules.clone()
            };
            catalog.profiles.insert(name.to_owned(), profile);
        }
        let rules = [("a", &a_rules), ("b", &b_rules)];
        let mut current = Some("a".to_owned());
        let mut recent: Option<String> = None;
        for _ in 0..4 {
            let suggestion = suggest_profile_switch(
                &catalog,
                rules.iter().copied(),
                current.as_deref(),
                Some("devbox"),
                None,
                recent.as_deref(),
            );
            match suggestion {
                ProfileSwitchSuggestion::None => break,
                ProfileSwitchSuggestion::Switch { profile_name, .. } => {
                    current = Some(profile_name);
                    recent = current.clone();
                }
            }
        }
        assert_eq!(current.as_deref(), Some("a"));
        assert_eq!(
            suggest_profile_switch(
                &catalog,
                rules.iter().copied(),
                current.as_deref(),
                Some("devbox"),
                None,
                recent.as_deref(),
            ),
            ProfileSwitchSuggestion::None,
        );
    }

    #[test]
    fn host_wildcard_suffix_matches_domain_tail() {
        assert!(host_matches(Some("host.example"), "*.example"));
        assert!(host_matches(Some("HOST.EXAMPLE"), "*.example"));
        assert!(!host_matches(Some("host.example.evil"), "*.example"));
        assert!(!host_matches(Some("notexample"), "*.example"));
        assert!(host_matches(Some("anything"), "*"));
    }

    #[test]
    fn a3_recently_applied_profile_is_suppressed_to_bound_switching() {
        // Section 6.b / termination bound: a profile just applied by a switch
        // must not immediately re-trigger. `recently_applied` suppresses it even
        // when its own rule still matches the context, breaking A->B->A ping-pong.
        let rules = ProfileSwitchRules {
            match_hosts: vec!["devbox".to_owned()],
            match_directories: Vec::new(),
            preserved: Default::default(),
        };
        let (catalog, rules) = catalog_with("dev", rules);
        let suggestion = suggest_profile_switch(
            &catalog,
            std::iter::once(("dev", &rules)),
            None,
            Some("devbox"),
            None,
            Some("dev"),
        );
        assert_eq!(
            suggestion,
            ProfileSwitchSuggestion::None,
            "a recently-applied profile must not be re-suggested in the same context"
        );
    }

    #[test]
    fn a3_rule_referencing_a_missing_profile_is_inert() {
        // Section 6.e: hostile/stale remote context cannot manufacture a profile.
        // A switch rule whose target profile is absent from the catalog yields no
        // suggestion; switching never creates a profile.
        let rules = ProfileSwitchRules {
            match_hosts: vec!["devbox".to_owned()],
            match_directories: Vec::new(),
            preserved: Default::default(),
        };
        let empty = ProfileCatalog::default();
        let suggestion = suggest_profile_switch(
            &empty,
            std::iter::once(("ghost", &rules)),
            None,
            Some("devbox"),
            None,
            None,
        );
        assert_eq!(
            suggestion,
            ProfileSwitchSuggestion::None,
            "a rule targeting a non-existent profile must never suggest a switch"
        );
    }

    #[test]
    fn a3_untrusted_or_unmatched_context_is_inert() {
        // Section 6.c: a host/dir that matches no rule produces no switch. Remote
        // output that does not match a locally-configured pattern is inert.
        let rules = ProfileSwitchRules {
            match_hosts: vec!["devbox".to_owned()],
            match_directories: vec!["/work/project".to_owned()],
            preserved: Default::default(),
        };
        let (catalog, rules) = catalog_with("dev", rules);
        let suggestion = suggest_profile_switch(
            &catalog,
            std::iter::once(("dev", &rules)),
            None,
            Some("attacker.example"),
            Some(Path::new("/tmp/elsewhere")),
            None,
        );
        assert_eq!(suggestion, ProfileSwitchSuggestion::None);
    }

    #[test]
    fn a3_directory_boundary_holds_against_sibling_prefix() {
        // A directory pattern must only match true descendants, never a sibling
        // sharing a name prefix.
        let rules = ProfileSwitchRules {
            match_hosts: Vec::new(),
            match_directories: vec!["/work/project".to_owned()],
            preserved: Default::default(),
        };
        let (catalog, rules) = catalog_with("proj", rules);
        let sibling = suggest_profile_switch(
            &catalog,
            std::iter::once(("proj", &rules)),
            None,
            None,
            Some(Path::new("/work/project-other/src")),
            None,
        );
        assert_eq!(
            sibling,
            ProfileSwitchSuggestion::None,
            "a directory rule must not match a sibling sharing a name prefix"
        );
    }

    #[test]
    fn a3_normalize_rejects_traversal_relative_and_separator_patterns() {
        // Directory patterns must be absolute; host patterns must carry no path
        // separators or whitespace. This blocks a hostile pattern from encoding
        // traversal or a path where a hostname is expected.
        assert_eq!(normalize_directory_pattern("../secret"), None);
        assert_eq!(normalize_directory_pattern("relative/dir"), None);
        assert_eq!(normalize_directory_pattern("   "), None);
        assert!(normalize_directory_pattern("/work/project").is_some());

        assert_eq!(normalize_host_pattern("bad/host"), None);
        assert_eq!(normalize_host_pattern("bad\\host"), None);
        assert_eq!(normalize_host_pattern("has space"), None);
        assert_eq!(normalize_host_pattern("*"), Some("*".to_owned()));
        assert_eq!(
            normalize_host_pattern("DevBox"),
            Some("devbox".to_owned()),
            "host patterns normalize case for stable matching"
        );
    }
}
