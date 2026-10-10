// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored palette parsing and recovery regressions, with no display dependency.

use crate::external_palette::{
    ExternalPaletteFollow, ExternalPaletteProvider, FollowPollOutcome, FollowStatus,
    parse_palette_bytes,
};
use std::time::{Duration, Instant};

fn complete_palette() -> String {
    let mut text = String::new();
    for key in [
        "foreground",
        "background",
        "clear",
        "cursor",
        "selection",
        "search",
        "border",
        "inactive",
        "bright_foreground",
        "muted",
        "dark_foreground",
        "darker_background",
    ] {
        text.push_str(&format!("{key} = #112233\n"));
    }
    for index in 0..16 {
        text.push_str(&format!("color{index} = #112233\n"));
    }
    text
}

#[test]
fn flat_providers_accept_comments_after_bare_and_quoted_colours() {
    let base16 = (0..16)
        .map(|index| format!("base{index:02x} = #112233\n"))
        .collect::<String>();
    let mut named = complete_palette()
        .lines()
        .filter(|line| !line.starts_with("color"))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    for colour in ["red", "green", "yellow", "blue", "magenta", "cyan"] {
        named.push_str(&format!("{colour} = #112233\nbright_{colour} = #112233\n"));
    }
    for (provider, source) in [
        (ExternalPaletteProvider::OdyttyAnsi, complete_palette()),
        (ExternalPaletteProvider::OdyttyAnsi, base16),
        (ExternalPaletteProvider::ColorsToml, complete_palette()),
        (ExternalPaletteProvider::ColorsToml, named),
    ] {
        let expected =
            parse_palette_bytes(provider, source.as_bytes()).expect("plain complete map");
        for quote in ["", "\"", "'"] {
            for comment in ["", " # project-authored comment", " \t# second # comment"] {
                let text = source.replace("#112233", &format!("{quote}#112233{quote}{comment}"));
                let actual = parse_palette_bytes(provider, text.as_bytes()).expect("commented map");
                assert_eq!(actual, expected, "{provider:?}, quote {quote:?}");
            }
        }
    }
}

#[test]
fn quotes_do_not_hide_malformed_colour_suffixes() {
    for value in [
        "\"#112233",
        "'#112233\"",
        "\"#112233\" suffix",
        "#112233suffix",
        "\"#112233 # inside\"",
        "\"#112233\"#adjacent",
        "#112233#adjacent",
    ] {
        let text = complete_palette()
            .replace("foreground = #112233\n", &format!("foreground = {value}\n"));
        assert!(
            parse_palette_bytes(ExternalPaletteProvider::OdyttyAnsi, text.as_bytes()).is_err(),
            "{value}"
        );
    }
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn with_source(f: impl FnOnce(&std::path::Path, &str, &mut ExternalPaletteFollow, Instant)) {
    let root = Scratch(crate::test_dirs::fresh_temp_dir("odytty-palette-recovery-"));
    super::config_env::with_config_base(&root.0, true, || {
        let _reads = crate::test_lock::palette_read_lock();
        let path = root.0.join("palette.txt");
        let original = complete_palette();
        std::fs::write(&path, &original).expect("owned source");
        let now = Instant::now();
        let mut follow = ExternalPaletteFollow::new();
        follow.configure(
            true,
            ExternalPaletteProvider::OdyttyAnsi,
            Some(path.clone()),
            now,
        );
        assert!(matches!(
            follow.refresh_now(now),
            FollowPollOutcome::Applied(_)
        ));
        assert_eq!(follow.status(), &FollowStatus::Applied);
        f(&path, &original, &mut follow, now);
    });
}

fn recover_identical(missing: bool) {
    with_source(|path, original, follow, now| {
        let good = follow.last_known_good_theme();
        if missing {
            std::fs::remove_file(path).expect("remove owned source");
        } else {
            std::fs::write(path, "project-authored malformed source")
                .expect("corrupt owned source");
        }
        assert_eq!(
            follow.poll(now + Duration::from_secs(2)),
            FollowPollOutcome::Retained
        );
        assert!(matches!(
            follow.status(),
            FollowStatus::RetainedLastKnownGood { .. }
        ));
        assert_eq!(follow.last_known_good_theme(), good);
        // A poll before the deadline must not announce recovery without reading.
        assert_eq!(
            follow.poll(now + Duration::from_secs(2)),
            FollowPollOutcome::Unchanged
        );
        assert!(matches!(
            follow.status(),
            FollowStatus::RetainedLastKnownGood { .. }
        ));
        std::fs::write(path, original).expect("restore identical source");
        assert_eq!(
            follow.poll(now + Duration::from_secs(4)),
            FollowPollOutcome::Unchanged
        );
        assert_eq!(follow.status(), &FollowStatus::Applied);
        assert_eq!(follow.last_known_good_theme(), good);
    });
}

#[test]
fn identical_recovery_after_missing_source_clears_retained_status() {
    recover_identical(true);
}

#[test]
fn identical_recovery_after_malformed_source_clears_retained_status() {
    recover_identical(false);
}

#[test]
fn same_source_reconfiguration_returns_to_applied_without_reapply() {
    with_source(|path, _, follow, now| {
        let good = follow.last_known_good_theme();
        follow.configure(
            true,
            ExternalPaletteProvider::OdyttyAnsi,
            Some(path.to_owned()),
            now,
        );
        assert_eq!(follow.status(), &FollowStatus::Watching);
        assert_eq!(follow.refresh_now(now), FollowPollOutcome::Unchanged);
        assert_eq!(follow.status(), &FollowStatus::Applied);
        assert_eq!(follow.last_known_good_theme(), good);
        follow.configure(
            false,
            ExternalPaletteProvider::OdyttyAnsi,
            Some(path.to_owned()),
            now,
        );
        assert_eq!(follow.status(), &FollowStatus::Disabled);
        assert_eq!(follow.refresh_now(now), FollowPollOutcome::Unchanged);
        assert_eq!(follow.status(), &FollowStatus::Disabled);
    });
}
