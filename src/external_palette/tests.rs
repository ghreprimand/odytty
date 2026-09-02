// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for external palette following.

use super::*;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn complete_odytty() -> String {
    let mut out = String::from(
        "foreground = #d6def4\n\
         background = #0c1224\n\
         clear = #070b18\n\
         cursor = #86c1ff\n\
         selection = #243352\n\
         search = #4a4018\n\
         border = #1b243e\n\
         inactive = #5a6480\n",
    );
    for index in 0..16 {
        let v = format!("{:02x}", index * 8);
        out.push_str(&format!("color{index} = #{v}{v}{v}\n"));
    }
    out
}

fn complete_colors_toml_current() -> String {
    String::from(
        "background = \"#1a1b26\"\n\
         foreground = \"#a9b1d6\"\n\
         bright_foreground = \"#c0caf5\"\n\
         selection = \"#33467c\"\n\
         muted = \"#565f89\"\n\
         dark_foreground = \"#565f89\"\n\
         darker_background = \"#0e0e14\"\n\
         red = \"#f7768e\"\n\
         green = \"#9ece6a\"\n\
         yellow = \"#e0af68\"\n\
         blue = \"#7aa2f7\"\n\
         magenta = \"#bb9af7\"\n\
         cyan = \"#7dcfff\"\n\
         bright_red = \"#f7768e\"\n\
         bright_green = \"#9ece6a\"\n\
         bright_yellow = \"#e0af68\"\n\
         bright_blue = \"#7aa2f7\"\n\
         bright_magenta = \"#bb9af7\"\n\
         bright_cyan = \"#7dcfff\"\n",
    )
}

fn complete_colors_toml_legacy() -> String {
    let mut out = String::from(
        "background = \"#1a1b26\"\n\
         foreground = \"#a9b1d6\"\n\
         bright_foreground = \"#c0caf5\"\n\
         selection = \"#33467c\"\n\
         muted = \"#565f89\"\n\
         dark_foreground = \"#565f89\"\n\
         darker_background = \"#0e0e14\"\n",
    );
    for index in 0..16 {
        let v = format!("{:02x}", 16 + index * 8);
        out.push_str(&format!("color{index} = \"#{v}{v}{v}\"\n"));
    }
    out
}

fn complete_colors_json() -> String {
    let mut colors = String::new();
    for index in 0..16 {
        let v = format!("{:02x}", 32 + index * 8);
        if index > 0 {
            colors.push(',');
        }
        colors.push_str(&format!("\"color{index}\":\"#{v}{v}{v}\""));
    }
    format!(
        "{{\"special\":{{\"background\":\"#0c1224\",\"foreground\":\"#d6def4\",\"cursor\":\"#86c1ff\"}},\"colors\":{{{colors}}}}}"
    )
}

fn temp_path(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "odytty-ext-palette-{name}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn odytty_complete_parses_and_projects() {
    let palette = parse_palette_bytes(
        ExternalPaletteProvider::OdyttyAnsi,
        complete_odytty().as_bytes(),
    )
    .expect("parse");
    let theme = palette.to_theme();
    assert_eq!(theme.foreground, (0xd6, 0xde, 0xf4));
    assert_eq!(theme.background, (0x0c, 0x12, 0x24));
    assert_eq!(theme.palette[0], (0x00, 0x00, 0x00));
}

#[test]
fn odytty_partial_fails_closed() {
    let text = "foreground = #ffffff\nbackground = #000000\n";
    let error = parse_palette_bytes(ExternalPaletteProvider::OdyttyAnsi, text.as_bytes())
        .expect_err("partial");
    assert!(matches!(error, ExternalPaletteError::Incomplete(_)));
}

#[test]
fn colors_toml_and_json_complete_parse() {
    parse_palette_bytes(
        ExternalPaletteProvider::ColorsToml,
        complete_colors_toml_current().as_bytes(),
    )
    .expect("toml current");
    parse_palette_bytes(
        ExternalPaletteProvider::ColorsToml,
        complete_colors_toml_legacy().as_bytes(),
    )
    .expect("toml legacy");
    parse_palette_bytes(
        ExternalPaletteProvider::ColorsJson,
        complete_colors_json().as_bytes(),
    )
    .expect("json");
}

#[test]
fn colors_toml_current_projects_exact_ansi_mapping() {
    let palette = parse_palette_bytes(
        ExternalPaletteProvider::ColorsToml,
        complete_colors_toml_current().as_bytes(),
    )
    .expect("parse");
    let theme = palette.to_theme();
    assert_eq!(theme.background, (0x1a, 0x1b, 0x26));
    assert_eq!(theme.palette[1], (0xf7, 0x76, 0x8e));
    assert_eq!(theme.palette[3], (0xe0, 0xaf, 0x68));
    assert_eq!(theme.search, theme.palette[3]);
    assert_eq!(theme.cursor, (0xc0, 0xca, 0xf5));
}

#[test]
fn colors_toml_partial_current_fails_closed() {
    let text = "background = \"#111111\"\n\
                foreground = \"#eeeeee\"\n\
                red = \"#ff0000\"\n\
                green = \"#00ff00\"\n";
    let error = parse_palette_bytes(ExternalPaletteProvider::ColorsToml, text.as_bytes())
        .expect_err("partial current");
    assert!(matches!(error, ExternalPaletteError::Incomplete(_)));
}

#[test]
fn enabled_follow_performs_first_read_on_refresh() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    reset_palette_read_count_for_test();
    let path = temp_path("launch-read");
    std::fs::write(&path, complete_odytty()).expect("seed");
    let before = palette_read_count_for_test();
    let mut follow = ExternalPaletteFollow::new();
    let now = Instant::now();
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
    assert_eq!(palette_read_count_for_test(), before + 1);
    assert!(matches!(follow.status(), FollowStatus::Applied));
    let _ = std::fs::remove_file(path);
}

#[test]
fn content_replacement_with_same_length_reloads() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    reset_palette_read_count_for_test();
    let path = temp_path("same-len");
    std::fs::write(&path, complete_odytty()).expect("seed");
    let mut follow = ExternalPaletteFollow::new();
    let now = Instant::now();
    follow.configure(
        true,
        ExternalPaletteProvider::OdyttyAnsi,
        Some(path.clone()),
        now,
    );
    let first = follow.refresh_now(now);
    assert!(matches!(first, FollowPollOutcome::Applied(_)));

    let mut replacement = complete_odytty();
    // Same length: flip one hex digit in foreground.
    replacement = replacement.replace("#d6def4", "#d6def5");
    assert_eq!(replacement.len(), complete_odytty().len());
    std::fs::write(&path, replacement).expect("replace");
    let second = follow.poll(now + Duration::from_secs(2));
    assert!(matches!(second, FollowPollOutcome::Applied(_)));
    let _ = std::fs::remove_file(path);
}

#[test]
fn malformed_retains_last_known_good() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    let path = temp_path("lkg");
    std::fs::write(&path, complete_odytty()).expect("seed");
    let mut follow = ExternalPaletteFollow::new();
    let now = Instant::now();
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
    std::fs::write(&path, "not a palette\n").expect("corrupt");
    let outcome = follow.poll(now + Duration::from_secs(2));
    assert!(matches!(outcome, FollowPollOutcome::Retained));
    assert!(follow.last_known_good_theme().is_some());
    assert!(matches!(
        follow.status(),
        FollowStatus::RetainedLastKnownGood { .. }
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn disabled_follower_never_reads() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    reset_palette_read_count_for_test();
    let path = temp_path("disabled");
    std::fs::write(&path, complete_odytty()).expect("seed");
    let mut follow = ExternalPaletteFollow::new();
    let before = palette_read_count_for_test();
    follow.configure(
        false,
        ExternalPaletteProvider::OdyttyAnsi,
        Some(path.clone()),
        Instant::now(),
    );
    let _ = follow.poll(Instant::now() + Duration::from_secs(5));
    assert_eq!(palette_read_count_for_test(), before);
    let _ = std::fs::remove_file(path);
}

#[test]
fn default_construct_does_not_read() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    reset_palette_read_count_for_test();
    let before = palette_read_count_for_test();
    let _ = ExternalPaletteFollow::new();
    assert_eq!(palette_read_count_for_test(), before);
}

// Adversarial external-palette coverage. Test-only; synthetic fixtures.

fn complete_base16() -> String {
    // base00..base0f each a distinct grey #vvvvvv where v = index * 0x11, so the
    // Base16 -> ANSI remap is unambiguous per projected slot.
    let mut out = String::new();
    for index in 0u8..16 {
        let v = index.wrapping_mul(0x11);
        out.push_str(&format!("base{index:02x} = #{v:02x}{v:02x}{v:02x}\n"));
    }
    out
}

fn odytty_without_clear() -> String {
    complete_odytty()
        .lines()
        .filter(|line| !line.trim_start().starts_with("clear"))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn odytty_projection_is_byte_exact_across_every_role_and_ansi_slot() {
    let palette = parse_palette_bytes(
        ExternalPaletteProvider::OdyttyAnsi,
        complete_odytty().as_bytes(),
    )
    .expect("parse");
    let theme = palette.to_theme();
    // Every semantic role projects byte-for-byte, no perceptual re-nudge.
    assert_eq!(theme.foreground, (0xd6, 0xde, 0xf4));
    assert_eq!(theme.background, (0x0c, 0x12, 0x24));
    assert_eq!(theme.clear, (0x07, 0x0b, 0x18));
    assert_eq!(theme.cursor, (0x86, 0xc1, 0xff));
    assert_eq!(theme.selection, (0x24, 0x33, 0x52));
    assert_eq!(theme.search, (0x4a, 0x40, 0x18));
    assert_eq!(theme.border, (0x1b, 0x24, 0x3e));
    assert_eq!(theme.inactive, (0x5a, 0x64, 0x80));
    // All 16 ANSI slots: colorN == (N*8, N*8, N*8) in the fixture.
    for index in 0usize..16 {
        let v = (index * 8) as u8;
        assert_eq!(
            theme.palette[index],
            (v, v, v),
            "ANSI slot {index} must project byte-exact"
        );
    }
}

#[test]
fn odytty_clear_defaults_to_background_when_omitted() {
    let palette = parse_palette_bytes(
        ExternalPaletteProvider::OdyttyAnsi,
        odytty_without_clear().as_bytes(),
    )
    .expect("parse");
    let theme = palette.to_theme();
    assert_eq!(
        theme.clear, theme.background,
        "omitted clear must equal background per the documented ThemeSpec rule"
    );
    assert_eq!(theme.background, (0x0c, 0x12, 0x24));
}

#[test]
fn base16_projection_follows_the_documented_remap() {
    let palette = parse_palette_bytes(
        ExternalPaletteProvider::OdyttyAnsi,
        complete_base16().as_bytes(),
    )
    .expect("base16 parse");
    let theme = palette.to_theme();
    // Roles: fg<-base05, bg/clear<-base00, cursor<-base07, selection<-base02,
    // search<-base0a, border/inactive<-base03.
    assert_eq!(theme.foreground, (0x55, 0x55, 0x55));
    assert_eq!(theme.background, (0x00, 0x00, 0x00));
    assert_eq!(theme.clear, (0x00, 0x00, 0x00));
    assert_eq!(theme.cursor, (0x77, 0x77, 0x77));
    assert_eq!(theme.selection, (0x22, 0x22, 0x22));
    assert_eq!(theme.search, (0xaa, 0xaa, 0xaa));
    assert_eq!(theme.border, (0x33, 0x33, 0x33));
    assert_eq!(theme.inactive, (0x33, 0x33, 0x33));
    // ANSI remap uniqueness pins: 1<-base08, 2<-base0b, 3<-base0a, 7<-base05, 15<-base07.
    assert_eq!(theme.palette[0], (0x00, 0x00, 0x00));
    assert_eq!(theme.palette[1], (0x88, 0x88, 0x88));
    assert_eq!(theme.palette[2], (0xbb, 0xbb, 0xbb));
    assert_eq!(theme.palette[3], (0xaa, 0xaa, 0xaa));
    assert_eq!(theme.palette[7], (0x55, 0x55, 0x55));
    assert_eq!(theme.palette[15], (0x77, 0x77, 0x77));
}

#[test]
fn base16_missing_one_base_key_fails_closed() {
    // Drop base0f: an incomplete Base16 set must not project a blended theme.
    let text: String = complete_base16()
        .lines()
        .filter(|line| !line.starts_with("base0f"))
        .map(|line| format!("{line}\n"))
        .collect();
    let error = parse_palette_bytes(ExternalPaletteProvider::OdyttyAnsi, text.as_bytes())
        .expect_err("incomplete base16");
    assert!(matches!(error, ExternalPaletteError::Incomplete(_)));
}

#[test]
fn pywal_json_projects_documented_roles_exactly() {
    let palette = parse_palette_bytes(
        ExternalPaletteProvider::ColorsJson,
        complete_colors_json().as_bytes(),
    )
    .expect("pywal parse");
    let theme = palette.to_theme();
    // special block -> fg/bg/cursor; clear<-background.
    assert_eq!(theme.foreground, (0xd6, 0xde, 0xf4));
    assert_eq!(theme.background, (0x0c, 0x12, 0x24));
    assert_eq!(theme.clear, (0x0c, 0x12, 0x24));
    assert_eq!(theme.cursor, (0x86, 0xc1, 0xff));
    // colorN = (32 + N*8) grey; selection/border/inactive<-color8, search<-color3.
    let c8 = 32u8 + 8 * 8; // 0x60
    let c3 = 32u8 + 3 * 8; // 0x38
    assert_eq!(theme.selection, (c8, c8, c8));
    assert_eq!(theme.border, (c8, c8, c8));
    assert_eq!(theme.inactive, (c8, c8, c8));
    assert_eq!(theme.search, (c3, c3, c3));
}

#[test]
fn pywal_json_missing_colors_fails_closed() {
    let text = "{\"special\":{\"background\":\"#000000\",\"foreground\":\"#ffffff\",\"cursor\":\"#888888\"},\"colors\":{\"color0\":\"#000000\"}}";
    let error = parse_palette_bytes(ExternalPaletteProvider::ColorsJson, text.as_bytes())
        .expect_err("partial pywal");
    assert!(matches!(error, ExternalPaletteError::Incomplete(_)));
}

#[test]
fn empty_input_fails_closed() {
    let error =
        parse_palette_bytes(ExternalPaletteProvider::OdyttyAnsi, b"").expect_err("empty must fail");
    assert!(matches!(error, ExternalPaletteError::Empty));
}

#[test]
fn oversized_input_is_refused_before_parse() {
    let big = vec![b'a'; (MAX_EXTERNAL_PALETTE_BYTES as usize) + 1];
    let error = parse_palette_bytes(ExternalPaletteProvider::OdyttyAnsi, &big)
        .expect_err("oversized must fail");
    assert!(matches!(error, ExternalPaletteError::Oversized));
}

#[test]
fn non_utf8_input_is_rejected_as_malformed() {
    let error = parse_palette_bytes(
        ExternalPaletteProvider::OdyttyAnsi,
        &[0xff, 0xfe, 0x00, 0x01],
    )
    .expect_err("non-utf8 must fail");
    assert!(matches!(error, ExternalPaletteError::Malformed(_)));
}

#[test]
fn too_many_lines_is_bounded() {
    // Comment lines still count toward the line cap; exceeding it fails closed
    // rather than scanning unboundedly.
    let mut text = String::with_capacity(MAX_EXTERNAL_PALETTE_LINES * 2 + 8);
    for _ in 0..(MAX_EXTERNAL_PALETTE_LINES + 8) {
        text.push_str("#\n");
    }
    let error = parse_palette_bytes(ExternalPaletteProvider::OdyttyAnsi, text.as_bytes())
        .expect_err("too many lines");
    assert!(matches!(error, ExternalPaletteError::TooManyLines));
}

#[test]
fn transient_disappearance_retains_last_known_good_then_reapplies_on_return() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    reset_palette_read_count_for_test();
    let path = temp_path("transient");
    std::fs::write(&path, complete_odytty()).expect("seed");
    let mut follow = ExternalPaletteFollow::new();
    let now = Instant::now();
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
    let good = follow.last_known_good_theme().expect("lkg present");

    // File vanishes mid-session: retain LKG, never snap to a default palette.
    std::fs::remove_file(&path).expect("remove");
    let missing = follow.poll(now + Duration::from_secs(2));
    assert!(matches!(missing, FollowPollOutcome::Retained));
    assert!(matches!(
        follow.status(),
        FollowStatus::RetainedLastKnownGood { .. }
    ));
    assert_eq!(
        follow.last_known_good_theme(),
        Some(good),
        "LKG palette is held unchanged while the source is absent"
    );

    // File reappears with a DIFFERENT valid palette: re-project it.
    let replacement = complete_odytty().replace("#d6def4", "#010203");
    std::fs::write(&path, replacement).expect("restore");
    let restored = follow.poll(now + Duration::from_secs(4));
    match restored {
        FollowPollOutcome::Applied(theme) => {
            assert_eq!(theme.foreground, (0x01, 0x02, 0x03));
        }
        other => panic!("expected Applied on return, got {other:?}"),
    }
    assert!(matches!(follow.status(), FollowStatus::Applied));
    let _ = std::fs::remove_file(path);
}

#[test]
fn identical_content_rewrite_does_not_reapply() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    let path = temp_path("no-thrash");
    std::fs::write(&path, complete_odytty()).expect("seed");
    let mut follow = ExternalPaletteFollow::new();
    let now = Instant::now();
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
    // Rewrite byte-identical content (a mtime-only touch): fingerprint is
    // unchanged so the follower must NOT thrash a re-apply.
    std::fs::write(&path, complete_odytty()).expect("rewrite identical");
    let outcome = follow.poll(now + Duration::from_secs(2));
    assert!(
        matches!(outcome, FollowPollOutcome::Unchanged),
        "identical content must not re-apply"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_bad_first_source_yields_no_theme_and_no_last_known_good() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    // Fail-closed at the follow level: if the very first source is unusable,
    // nothing is applied and there is no last-known-good to fall back to.
    let path = temp_path("bad-first");
    std::fs::write(&path, "not a palette\n").expect("seed bad");
    let mut follow = ExternalPaletteFollow::new();
    let now = Instant::now();
    follow.configure(
        true,
        ExternalPaletteProvider::OdyttyAnsi,
        Some(path.clone()),
        now,
    );
    let outcome = follow.refresh_now(now);
    assert!(matches!(outcome, FollowPollOutcome::Retained));
    assert!(
        follow.last_known_good_theme().is_none(),
        "a bad first source must not manufacture a last-known-good theme"
    );
    assert!(matches!(follow.status(), FollowStatus::Error { .. }));
    let _ = std::fs::remove_file(path);
}

#[test]
fn provider_alias_parsing_maps_documented_names() {
    for alias in ["odytty", "ANSI", "theme", "base16"] {
        assert_eq!(
            ExternalPaletteProvider::parse(alias),
            Some(ExternalPaletteProvider::OdyttyAnsi),
            "{alias:?} must map to OdyttyAnsi"
        );
    }
    for alias in ["colors_toml", "toml", "omarchy", "omarchy-compat"] {
        assert_eq!(
            ExternalPaletteProvider::parse(alias),
            Some(ExternalPaletteProvider::ColorsToml),
            "{alias:?} must map to ColorsToml"
        );
    }
    for alias in ["colors.json", "pywal", "wal"] {
        assert_eq!(
            ExternalPaletteProvider::parse(alias),
            Some(ExternalPaletteProvider::ColorsJson),
            "{alias:?} must map to ColorsJson"
        );
    }
    assert_eq!(ExternalPaletteProvider::parse("no-such-provider"), None);
}

#[test]
fn default_settings_startup_reads_zero_external_palette_files() {
    // Serialize with every other palette-reading test (crate::test_lock):
    // the read counter is process-global and CI runs the suite in parallel.
    let _read_guard = crate::test_lock::palette_read_lock();
    // Opt-in isolation: constructing settings from a clean environment and a
    // default follower performs no external-palette read at all.
    reset_palette_read_count_for_test();
    let before = palette_read_count_for_test();
    let _settings = crate::settings::Settings::from_env();
    let follow = ExternalPaletteFollow::new();
    assert!(!follow.is_enabled());
    assert_eq!(
        palette_read_count_for_test(),
        before,
        "default startup must not read any external palette source"
    );
}
