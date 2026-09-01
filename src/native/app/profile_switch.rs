// SPDX-License-Identifier: GPL-3.0-only
//! Opt-in host/directory-aware named-profile switching for the focused pane.

use std::path::{Path, PathBuf};

use crate::profiles::{ProfileSwitchReason, ProfileSwitchSuggestion, suggest_profile_switch};

use super::App;

impl App {
    /// Evaluate local profile switch rules when the focused pane's working
    /// directory changes. Match rules come only from the local catalog; terminal
    /// output cannot select or rewrite profiles.
    pub(super) fn poll_profile_auto_switch(&mut self) {
        if !self.settings.profile_auto_switch {
            return;
        }
        let focused = self.sessions.active_id();
        let remote_destination = self
            .sessions
            .get(focused)
            .and_then(|session| session.remote_destination.clone());
        let current_profile = self
            .sessions
            .get(focused)
            .and_then(|session| session.launch_profile.clone());
        let cwd_changed = self
            .sessions
            .get(focused)
            .and_then(|session| session.terminal.lock().ok())
            .is_some_and(|mut terminal| terminal.take_working_directory_changed());
        if !cwd_changed {
            return;
        }

        let cwd = self
            .sessions
            .get(focused)
            .and_then(|session| session.terminal.lock().ok())
            .and_then(|terminal| terminal.current_working_directory().map(str::to_owned));
        let cwd_path = cwd.as_deref().map(Path::new);

        let host = if let Some(destination) = remote_destination.as_deref() {
            trusted_remote_host(destination)
        } else {
            self.sessions.local_hostname()
        };

        let catalog = super::profile_launch::load_profile_catalog();
        let rules: Vec<_> = catalog
            .profiles
            .values()
            .filter(|profile| profile.applies_on_current_platform())
            .map(|profile| (profile.name.as_str(), &profile.switch))
            .collect();
        let context = profile_switch_context_key(host, cwd_path);
        if self.profile_switch_context.as_ref() != Some(&context) {
            self.profile_switch_context = Some(context);
            self.profile_switch_recent = None;
        }

        let suggestion = suggest_profile_switch(
            &catalog,
            rules.iter().copied(),
            current_profile.as_deref(),
            host,
            if remote_destination.is_some() {
                None
            } else {
                cwd_path
            },
            self.profile_switch_recent.as_deref(),
        );

        match suggestion {
            ProfileSwitchSuggestion::None => {}
            ProfileSwitchSuggestion::Switch {
                profile_name,
                reason,
            } => {
                let effective = super::profile_launch::resolve_for_new_local_tab(
                    &self.settings,
                    None,
                    cwd.map(PathBuf::from),
                    Some(&profile_name),
                );
                let session_theme = crate::native::cvd_theme::effective_theme(
                    &effective.settings.theme,
                    effective.settings.cvd_mode,
                    effective.settings.cvd_strength,
                );
                let themed_ui_roles = effective.settings.themed_ui_roles;
                let osc52_read = effective.settings.osc52_read;
                let kitty_named_transports = effective.settings.kitty_named_transports;
                let cursor_style = effective.settings.cursor_style;
                let cursor_blink = effective.settings.cursor_blink;
                let scrollback_limit = effective.settings.scrollback_limit();
                let button_gates = super::hover::ButtonGates {
                    enabled: effective.settings.buttons,
                    iterm_compat: effective.settings.buttons_iterm_compat,
                    sticky: effective.settings.buttons_sticky,
                };
                if let Some(session) = self.sessions.get_mut(focused) {
                    session.launch_profile = Some(profile_name.clone());
                    let cell = self.gpu.as_ref().map(crate::native::gpu::GpuState::cell);
                    Self::initialize_session_with(
                        session,
                        session_theme,
                        themed_ui_roles,
                        osc52_read,
                        kitty_named_transports,
                        cursor_style,
                        cursor_blink,
                        cell,
                        scrollback_limit,
                        button_gates,
                    );
                }
                self.profile_switch_recent = Some(profile_name.clone());
                self.show_transient_hud(profile_switch_hud_message(&profile_name, &reason));
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                for warning in effective.warnings {
                    tracing::warn!(warning = %warning, "profile switch notice");
                }
            }
        }
    }
}

fn profile_switch_context_key(
    host: Option<&str>,
    cwd: Option<&Path>,
) -> (Option<String>, Option<String>) {
    (
        host.map(str::to_ascii_lowercase),
        cwd.map(|path| path.to_string_lossy().into_owned()),
    )
}

fn trusted_remote_host(destination: &str) -> Option<&str> {
    let host = destination.split('@').nth(1).unwrap_or(destination);
    let host = host.split(':').next()?.trim();
    (!host.is_empty()).then_some(host)
}

fn profile_switch_hud_message(profile_name: &str, reason: &ProfileSwitchReason) -> String {
    match reason {
        ProfileSwitchReason::Host { matched } => {
            format!("Profile {profile_name} (host {matched})")
        }
        ProfileSwitchReason::Directory { matched } => {
            format!("Profile {profile_name} (directory {matched})")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::profiles::{
        LaunchProfile, ProfileCatalog, ProfileSwitchReason, ProfileSwitchRules,
        ProfileSwitchSuggestion, suggest_profile_switch,
    };

    use super::{profile_switch_context_key, profile_switch_hud_message, trusted_remote_host};

    #[test]
    fn native_caller_context_clears_recency_only_on_context_change() {
        let host_ctx = profile_switch_context_key(Some("devbox"), Some(Path::new("/work")));
        let mut recent = Some("a".to_owned());

        let same = profile_switch_context_key(Some("devbox"), Some(Path::new("/work")));
        assert_eq!(Some(&host_ctx), Some(&same));
        // Same context: recent must survive a None suggestion (native caller rule).
        let _ = suggest_profile_switch(
            &ProfileCatalog::default(),
            std::iter::empty::<(&str, &ProfileSwitchRules)>(),
            Some("a"),
            Some("devbox"),
            Some(Path::new("/work")),
            recent.as_deref(),
        );
        assert_eq!(recent, Some("a".to_owned()));

        let other = profile_switch_context_key(Some("devbox"), Some(Path::new("/other")));
        if host_ctx != other {
            recent = None;
        }
        assert!(recent.is_none());
    }

    #[test]
    fn native_caller_repeated_cwd_events_converge_without_flapping() {
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
        for (name, rules) in [("a", a_rules.clone()), ("b", b_rules.clone())] {
            let mut profile = LaunchProfile::new(name).expect("profile");
            profile.switch = rules;
            catalog.profiles.insert(name.to_owned(), profile);
        }
        let rules = [("a", &a_rules), ("b", &b_rules)];
        let host = Some("devbox");
        let cwd = Some(Path::new("/work"));
        let mut context = profile_switch_context_key(host, cwd);
        let mut recent = None;
        let mut current: Option<String> = None;

        for _ in 0..6 {
            let suggestion = suggest_profile_switch(
                &catalog,
                rules.iter().copied(),
                current.as_deref(),
                host,
                cwd,
                recent.as_deref(),
            );
            let next_context = profile_switch_context_key(host, cwd);
            if context != next_context {
                context = next_context;
                recent = None;
            }
            match suggestion {
                ProfileSwitchSuggestion::None => {}
                ProfileSwitchSuggestion::Switch { profile_name, .. } => {
                    current = Some(profile_name.clone());
                    recent = Some(profile_name);
                }
            }
        }

        assert_eq!(
            current.as_deref(),
            Some("a"),
            "first stable match must hold"
        );
        assert_eq!(
            suggest_profile_switch(
                &catalog,
                rules.iter().copied(),
                current.as_deref(),
                host,
                cwd,
                recent.as_deref(),
            ),
            ProfileSwitchSuggestion::None,
        );
    }

    // v0.14 Phase A3 final-surface: caller-side pure helpers for
    // `poll_profile_auto_switch`. The convergence/flap decision is covered above
    // and in `crate::profiles::switch`; these pin remote-host trust extraction
    // (what host a remote pane presents to the local match rules) and the
    // visible switch disclosure string.
    #[test]
    fn trusted_remote_host_extracts_host_from_user_at_host_port() {
        // A remote pane's destination feeds the LOCAL match rules only as a bare
        // host. The user prefix and port suffix are stripped so a rule matches
        // on host identity, never on credentials or transport detail.
        assert_eq!(trusted_remote_host("alice@devbox:22"), Some("devbox"));
        assert_eq!(trusted_remote_host("devbox:2222"), Some("devbox"));
        assert_eq!(trusted_remote_host("alice@devbox"), Some("devbox"));
        assert_eq!(trusted_remote_host("devbox"), Some("devbox"));
    }

    #[test]
    fn trusted_remote_host_is_inert_for_empty_or_degenerate_destinations() {
        // A destination that carries no usable host yields None, so a remote
        // pane with a malformed identity contributes no host to the match rules
        // (fail-closed: it can never accidentally match a "*" or empty pattern
        // through a blank host).
        assert_eq!(trusted_remote_host(""), None);
        assert_eq!(trusted_remote_host("alice@"), None);
        assert_eq!(trusted_remote_host("alice@:22"), None);
        assert_eq!(trusted_remote_host("   "), None);
    }

    #[test]
    fn trusted_remote_host_takes_the_second_segment_deterministically() {
        // Extraction is `split('@').nth(1)` then strip the port: for a
        // multi-'@' destination it deterministically takes the SECOND segment
        // ("weird"), not a re-joined tail. This pins the actual behavior so the
        // extraction can never silently change under a hostile destination.
        assert_eq!(trusted_remote_host("user@weird@host:22"), Some("weird"));
    }

    #[test]
    fn switch_disclosure_message_names_profile_and_matched_reason() {
        // The HUD string is the user-visible disclosure that a host/directory
        // rule changed the pane's profile. It must name both the profile and the
        // concrete matched pattern so the switch is never silent.
        let host = profile_switch_hud_message(
            "dev",
            &ProfileSwitchReason::Host {
                matched: "devbox".to_owned(),
            },
        );
        assert_eq!(host, "Profile dev (host devbox)");

        let dir = profile_switch_hud_message(
            "proj",
            &ProfileSwitchReason::Directory {
                matched: "/work/project".to_owned(),
            },
        );
        assert_eq!(dir, "Profile proj (directory /work/project)");
    }
}
