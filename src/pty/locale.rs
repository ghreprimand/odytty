// SPDX-License-Identifier: GPL-3.0-only
//! A locale for children of a macOS application launched without a shell.
use super::CommandBuilder;
use std::ffi::OsString;

#[cfg(target_os = "macos")]
mod macos;

impl CommandBuilder {
    /// Apply only at final command conversion, after profile overrides.
    #[cfg(target_os = "macos")]
    pub(super) fn with_macos_locale(mut self) -> Self {
        self.apply_locale_default_with(
            |key| std::env::var_os(key),
            || {
                let apple = macos::apple_locale();
                utf8_locale(apple.as_deref(), |name| {
                    std::path::Path::new("/usr/share/locale")
                        .join(name)
                        .is_dir()
                })
            },
        );
        self
    }

    fn apply_locale_default_with(
        &mut self,
        inherited: impl Fn(&str) -> Option<OsString>,
        preferred: impl FnOnce() -> String,
    ) {
        // An explicit removal is also a caller choice. Empty values count as
        // set, matching inherited and profile environment semantics.
        if self.env_remove.iter().any(|key| key == "LANG") {
            return;
        }
        let has_locale = ["LANG", "LC_ALL", "LC_CTYPE"].iter().any(|key| {
            self.env.iter().any(|(name, _)| name == *key)
                || (!self.env_remove.iter().any(|name| name == *key) && inherited(key).is_some())
        });
        if !has_locale {
            self.env("LANG", preferred());
        }
    }
}

/// Reject path separators, control bytes and oversized preference values before
/// looking below the fixed system locale directory.
fn utf8_locale(apple: Option<&str>, installed: impl Fn(&str) -> bool) -> String {
    if let Some(locale) = apple.filter(|locale| {
        !locale.is_empty()
            && locale.len() <= 128
            && locale
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_@=;.-".contains(&byte))
    }) {
        let locale = locale.split('@').next().unwrap_or(locale);
        let (language, region) = locale.split_once('_').unwrap_or((locale, ""));
        let language = language
            .split_once('-')
            .filter(|(_, script)| {
                script.len() == 4 && script.bytes().all(|b| b.is_ascii_alphabetic())
            })
            .map_or(language, |(language, _)| language);
        let candidate = if region.is_empty() {
            format!("{language}.UTF-8")
        } else {
            format!("{language}_{region}.UTF-8")
        };
        if installed(&candidate) {
            return candidate;
        }
    }
    "en_US.UTF-8".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locale(command: &CommandBuilder) -> Option<&std::ffi::OsStr> {
        command
            .env
            .iter()
            .rev()
            .find(|(key, _)| key == "LANG")
            .map(|(_, value)| value.as_os_str())
    }

    #[test]
    fn macos_locale_default_selects_installed_apple_locale_or_utf8_fallback() {
        assert_eq!(
            utf8_locale(Some("en_CA"), |name| name == "en_CA.UTF-8"),
            "en_CA.UTF-8"
        );
        assert_eq!(utf8_locale(Some("fr_CA"), |_| false), "en_US.UTF-8");
        assert_eq!(
            utf8_locale(None, |_| panic!("missing preference has no lookup")),
            "en_US.UTF-8"
        );
        for invalid in [
            "",
            "../en_CA",
            "en_CA/../../etc",
            "en_CA\0",
            "en_CA\n",
            &"a".repeat(129),
        ] {
            assert_eq!(
                utf8_locale(Some(invalid), |_| panic!(
                    "invalid preference has no lookup"
                )),
                "en_US.UTF-8"
            );
        }
    }

    #[test]
    fn macos_locale_default_normalizes_modifiers_and_script_subtags() {
        for (apple, expected) in [
            ("en_US@rg=cazzzz", "en_US.UTF-8"),
            ("en_CA@calendar=gregorian", "en_CA.UTF-8"),
            ("zh-Hans_CN", "zh_CN.UTF-8"),
            ("sr-Latn_RS", "sr_RS.UTF-8"),
        ] {
            assert_eq!(
                utf8_locale(Some(apple), |name| {
                    assert_eq!(name, expected, "only the normalized locale is looked up");
                    true
                }),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn macos_locale_default_reaches_the_final_child_command() {
        let mut builder = CommandBuilder::new("shell");
        builder.apply_locale_default_with(|_| None, || "en_CA.UTF-8".into());
        let command = builder.into_command();
        let lang = command
            .get_envs()
            .find(|(key, _)| *key == "LANG")
            .and_then(|(_, value)| value);
        assert_eq!(lang, Some(std::ffi::OsStr::new("en_CA.UTF-8")));
    }

    #[test]
    fn macos_locale_default_ignores_a_removed_inherited_override() {
        let mut builder = CommandBuilder::new("shell");
        builder.env_remove("LC_ALL");
        builder.apply_locale_default_with(
            |key| (key == "LC_ALL").then(|| "C".into()),
            || "en_CA.UTF-8".into(),
        );
        assert_eq!(locale(&builder), Some(std::ffi::OsStr::new("en_CA.UTF-8")));
    }

    #[test]
    fn macos_locale_default_supplies_missing_child_lang() {
        let mut command = CommandBuilder::new("shell");
        command.apply_locale_default_with(|_| None, || "en_CA.UTF-8".into());
        assert_eq!(locale(&command), Some(std::ffi::OsStr::new("en_CA.UTF-8")));
    }

    #[test]
    fn macos_locale_default_preserves_each_inherited_or_profile_locale() {
        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            for value in ["", "C", "ja_JP.UTF-8"] {
                let mut inherited = CommandBuilder::new("shell");
                inherited.apply_locale_default_with(
                    |candidate| (candidate == key).then(|| value.into()),
                    || panic!("explicit inherited locale must win"),
                );
                assert!(locale(&inherited).is_none());
                let mut profile = CommandBuilder::new("shell");
                profile.env(key, value);
                profile.apply_locale_default_with(|_| None, || panic!("profile locale must win"));
                if key == "LANG" {
                    assert_eq!(locale(&profile), Some(std::ffi::OsStr::new(value)));
                } else {
                    assert!(locale(&profile).is_none());
                }
            }
        }
    }

    #[test]
    fn macos_locale_default_honors_explicit_lang_removal() {
        let mut command = CommandBuilder::new("shell");
        command.env_remove("LANG");
        command.apply_locale_default_with(|_| None, || panic!("explicit removal must win"));
        assert!(locale(&command).is_none());
    }
}
