// SPDX-License-Identifier: GPL-3.0-only
//! Lossless migration helpers for legacy profile references.

use crate::connection_hosts::ConnectionHost;

use super::schema::{LaunchProfile, ProfileAppearance, ProfileError};

/// Build a named profile from legacy per-host Theme/Font/Title fields.
///
/// Connection-specific transport fields remain in `hosts.conf`; the profile
/// stores only the reusable appearance overrides plus an optional connection
/// alias reference.
pub fn profile_from_connection_host(host: &ConnectionHost) -> Result<LaunchProfile, ProfileError> {
    let mut profile = LaunchProfile::new(host.alias.clone())?;
    profile.display_name = host.title.clone().or_else(|| Some(host.alias.clone()));
    profile.connection = Some(host.alias.clone());
    profile.appearance = ProfileAppearance {
        theme: host.theme.clone(),
        font: host.font.clone(),
        font_family: None,
        font_weight: None,
        font_size_px: None,
        title: host.title.clone(),
        visual: None,
        follow_external_palette: None,
        external_palette_provider: None,
        external_palette_path: None,
        preserved: Default::default(),
    };
    Ok(profile)
}

/// Normalize the shipped [`WorkspaceShape::default_profile`](crate::native::persistence::WorkspaceShape::default_profile) string.
///
/// Today this field stores a **connection-host alias** set by the SSH connect
/// path so New Tab routes through that host. It is not a general named launch
/// profile and must not be passed to the named-profile catalog resolver.
pub fn normalize_workspace_connection_binding(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::connection_hosts::ConnectionHostSource;
    use crate::profiles::{
        LaunchCliOverrides, LiveLaunchOverrides, ProfileCatalog, RestoredLaunchOverrides,
        precedence::resolve_effective_launch,
    };
    use crate::settings::DEFAULT_THEME;

    #[test]
    fn connection_host_migration_preserves_appearance_and_alias() {
        let host = ConnectionHost {
            alias: "edge".to_owned(),
            host_name: Some("edge.example.test".to_owned()),
            user: None,
            port: None,
            theme: Some("odyssey".to_owned()),
            font: Some("Victor Mono".to_owned()),
            title: Some("Edge".to_owned()),
            integration: None,
            reuse: None,
            tmux: None,
            protocol: None,
            identity_file: None,
            persist: None,
            source: ConnectionHostSource::Odytty,
        };
        let profile = profile_from_connection_host(&host).expect("migrate");
        assert_eq!(profile.name, "edge");
        assert_eq!(profile.connection.as_deref(), Some("edge"));
        assert_eq!(profile.appearance.theme.as_deref(), Some("odyssey"));
        assert_eq!(profile.appearance.font.as_deref(), Some("Victor Mono"));
        assert_eq!(profile.appearance.title.as_deref(), Some("Edge"));
    }

    #[test]
    fn workspace_connection_binding_trims_without_dropping_non_empty_values() {
        assert_eq!(
            normalize_workspace_connection_binding(Some(" prod ")).as_deref(),
            Some("prod")
        );
        assert_eq!(normalize_workspace_connection_binding(Some("   ")), None);
        assert_eq!(normalize_workspace_connection_binding(None), None);
    }

    #[test]
    fn legacy_default_profile_host_alias_does_not_select_named_profile() {
        let alias = normalize_workspace_connection_binding(Some("prod-web"));
        assert_eq!(alias.as_deref(), Some("prod-web"));

        let mut profile = LaunchProfile::new("prod-web").expect("profile");
        profile.appearance.theme = Some("plain".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("prod-web".to_owned(), profile);

        let effective = resolve_effective_launch(
            None,
            &HashMap::new(),
            &catalog,
            &LaunchCliOverrides::default(),
            &RestoredLaunchOverrides::default(),
            &LiveLaunchOverrides::default(),
            None,
        );
        assert_eq!(effective.profile_name, None);
        assert_eq!(effective.settings.theme, DEFAULT_THEME);
    }
}
