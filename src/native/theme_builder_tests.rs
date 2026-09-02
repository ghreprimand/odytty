// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for the theme builder (U2/U3). Kept as a child `mod tests` of
//! `theme_builder` via `#[path]`, so `use super::*` still reaches the builder’s
//! private items — this is a pure file move for module-size relief, not a
//! behavior or visibility change.

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "odytty-theme-builder-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn edit_state_machine_previews_color_changes() {
    let mut builder = ThemeBuilder::new(&Settings::default());
    assert_eq!(
        builder.handle_input(OverlayInput::Activate),
        ThemeBuilderOutcome::Consumed
    );
    for _ in 0..7 {
        builder.handle_input(OverlayInput::Backspace);
    }
    for ch in "#123456".chars() {
        builder.handle_input(OverlayInput::Char(ch));
    }

    let ThemeBuilderOutcome::Preview(theme) = builder.handle_input(OverlayInput::Activate) else {
        panic!("expected preview");
    };

    assert_eq!(theme.foreground, (0x12, 0x34, 0x56));
    assert_eq!(builder.render_signature().editing, None);
}

#[test]
fn serialize_round_trips_to_valid_theme() {
    let mut builder = ThemeBuilder::new(&Settings::default());
    builder.handle_input(OverlayInput::Save);
    for _ in 0..builder
        .render_signature()
        .editing
        .as_ref()
        .and_then(|edit| match edit {
            ThemeBuilderEditSignature::Name { buffer } => Some(buffer.len()),
            _ => None,
        })
        .unwrap()
    {
        builder.handle_input(OverlayInput::Backspace);
    }
    for ch in "my-theme".chars() {
        builder.handle_input(OverlayInput::Char(ch));
    }
    let ThemeBuilderOutcome::Save(request) = builder.handle_input(OverlayInput::Activate) else {
        panic!("expected save request");
    };

    let reparsed = ThemeSpec::parse(&request.spec.serialize(), |m| panic!("warn: {m}"));
    assert_eq!(reparsed, request.spec);
    assert_eq!(request.name, "my-theme");
}

#[test]
fn save_writes_to_injected_temp_dir() {
    let dir = temp_dir("save");
    let mut spec = ThemeSpec::from_theme(&Theme::ODYSSEY);
    spec.name = "test-theme".to_owned();
    let request = ThemeBuilderSaveRequest {
        name: "test-theme".to_owned(),
        spec: spec.clone(),
    };

    let path = save_theme_to_dir(&dir, &request).unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    let reparsed = ThemeSpec::parse(&contents, |m| panic!("warn: {m}"));

    assert_eq!(path, dir.join("test-theme.theme"));
    assert_eq!(reparsed, spec);
    assert!(!contents.contains("/home/"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn save_sink_rejects_untrusted_theme_names_without_creating_a_file() {
    let root = temp_dir("sink-rejection");
    let theme_dir = root.join("themes");
    let spec = ThemeSpec::from_theme(&Theme::ODYSSEY);
    let invalid_names = [
        "",
        "../escape",
        "nested/name",
        "nested\\name",
        "line\nbreak",
        "line\r\nbreak",
        "tab\tname",
        "nul\0name",
        "wrong-extension.theme",
        "lookalike\u{2215}slash",
        "lookalike\u{ff3c}slash",
    ];

    for name in invalid_names {
        let request = ThemeBuilderSaveRequest {
            name: name.to_owned(),
            spec: spec.clone(),
        };
        let error = save_theme_to_dir(&theme_dir, &request).expect_err(name);
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{name:?}");
        assert!(
            !theme_dir.exists(),
            "{name:?} created the output directory before rejection"
        );
        assert!(
            !root.join("escape.theme").exists(),
            "{name:?} escaped the theme directory"
        );
    }

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn save_rejects_invalid_name_at_filesystem_boundary_without_writing() {
    let dir = temp_dir("reject-invalid-name");
    let spec = ThemeSpec::from_theme(&Theme::ODYSSEY);
    for invalid in [
        "",
        "../outside",
        "nested/name",
        "nested\\name",
        "name.theme",
        "line\nfeed",
    ] {
        let request = ThemeBuilderSaveRequest {
            name: invalid.to_owned(),
            spec: spec.clone(),
        };
        let error = save_theme_to_dir(&dir, &request).expect_err("invalid name rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_restores_original_theme() {
    let settings = Settings {
        theme: Theme::ODYSSEY,
        ..Settings::default()
    };
    let mut builder = ThemeBuilder::new(&settings);
    let ThemeBuilderOutcome::Preview(_) = builder.handle_input(OverlayInput::Right) else {
        panic!("expected preview");
    };

    let ThemeBuilderOutcome::Cancel(theme) = builder.handle_input(OverlayInput::Close) else {
        panic!("expected cancel");
    };

    assert_eq!(theme, Theme::ODYSSEY);
}

#[test]
fn arrows_drive_the_focused_oklch_channel_via_core_nudge() {
    let mut b = ThemeBuilder::new(&Settings::default());
    let field = FIELDS[b.selected];

    // Default channel = Lightness: Right == core nudge with +L_STEP only.
    let start = b.color(field);
    b.handle_input(OverlayInput::Right);
    assert_eq!(b.color(field), theme_author::nudge(start, L_STEP, 0.0, 0.0));

    // `]` cycles to Chroma: Right == +C_STEP only.
    b.handle_input(OverlayInput::Char(']'));
    let before_c = b.color(field);
    b.handle_input(OverlayInput::Right);
    assert_eq!(
        b.color(field),
        theme_author::nudge(before_c, 0.0, C_STEP, 0.0)
    );

    // `]` cycles to Hue: Right == +H_STEP rotation only.
    b.handle_input(OverlayInput::Char(']'));
    let before_h = b.color(field);
    b.handle_input(OverlayInput::Right);
    assert_eq!(
        b.color(field),
        theme_author::nudge(before_h, 0.0, 0.0, H_STEP_DEG.to_radians())
    );

    // Left is the negative delta of the focused channel.
    let before_neg = b.color(field);
    b.handle_input(OverlayInput::Left);
    assert_eq!(
        b.color(field),
        theme_author::nudge(before_neg, 0.0, 0.0, -H_STEP_DEG.to_radians())
    );
}

#[test]
fn channel_cycles_both_directions() {
    let mut b = ThemeBuilder::new(&Settings::default());
    assert_eq!(b.channel, OklchChannel::Lightness);
    b.handle_input(OverlayInput::Char(']'));
    assert_eq!(b.channel, OklchChannel::Chroma);
    b.handle_input(OverlayInput::Char(']'));
    assert_eq!(b.channel, OklchChannel::Hue);
    b.handle_input(OverlayInput::Char(']'));
    assert_eq!(b.channel, OklchChannel::Lightness); // wraps
    b.handle_input(OverlayInput::Char('['));
    assert_eq!(b.channel, OklchChannel::Hue); // wraps backward
}

#[test]
fn snap_lifts_failing_floored_role_to_aa_and_is_idempotent() {
    let mut b = ThemeBuilder::new(&Settings::default());
    // Sabotage foreground to equal background (contrast ~1, fails AA).
    let bg = b.spec.background;
    b.set_color(ThemeField::Foreground, bg);
    b.selected = 0; // foreground

    let out = b.snap_selected();
    assert!(matches!(out, ThemeBuilderOutcome::Preview(_)));

    let partner = theme_author::partner_color(&b.spec, AuthorRole::Foreground).unwrap();
    let lifted = b.color(ThemeField::Foreground);
    assert!(theme_author::authoring_contrast(lifted, partner) >= AUTHORING_CONTRAST_FLOOR);

    // Idempotent: a second snap is a byte no-op.
    b.snap_selected();
    assert_eq!(b.color(ThemeField::Foreground), lifted);
}

#[test]
fn snap_is_inert_on_not_floored_roles() {
    let mut b = ThemeBuilder::new(&Settings::default());
    // Background is never floored.
    b.selected = FIELDS
        .iter()
        .position(|f| matches!(f, ThemeField::Background))
        .unwrap();
    let before = b.color(ThemeField::Background);
    let out = b.snap_selected();
    assert_eq!(out, ThemeBuilderOutcome::Consumed);
    assert_eq!(b.color(ThemeField::Background), before);
}

#[test]
fn readout_reflects_pass_fail_and_not_floored() {
    let mut b = ThemeBuilder::new(&Settings::default());

    // Foreground floored against bg: force a fail, then snap to pass.
    let bg = b.spec.background;
    b.set_color(ThemeField::Foreground, bg);
    b.selected = 0;
    assert!(b.readout_lines()[0].contains("FAIL"));
    b.snap_selected();
    assert!(b.readout_lines()[0].contains("PASS"));

    // Border is not floored.
    b.selected = FIELDS
        .iter()
        .position(|f| matches!(f, ThemeField::Border))
        .unwrap();
    assert!(b.readout_lines()[0].contains("not floored"));
}

#[test]
fn save_auto_snaps_every_floored_role_to_aa() {
    let mut b = ThemeBuilder::new(&Settings::default());
    // Sabotage a chromatic palette slot to fail AA against the background.
    let bg = b.spec.background;
    b.set_color(ThemeField::Palette(1), bg);

    let ThemeBuilderOutcome::Save(req) = b.save_request("mytheme".to_owned()) else {
        panic!("expected save request");
    };
    assert_eq!(req.name, "mytheme");
    assert!(b.save_snap_count >= 1);

    // Every floored role in the saved spec now clears the authoring floor.
    for field in FIELDS {
        let role = author_role(field);
        if let Some(partner) = theme_author::partner_color(&b.spec, role) {
            let ratio = theme_author::authoring_contrast(b.color(field), partner);
            assert!(
                ratio >= AUTHORING_CONTRAST_FLOOR,
                "{field:?} only reached {ratio:.2}"
            );
        }
    }

    // The save confirmation reports the backstop.
    b.save_succeeded("mytheme", Path::new("/tmp/mytheme.theme"), 1);
    assert!(b.message.as_deref().unwrap().contains("Snapped"));
}

#[test]
fn hex_entry_still_applies_verbatim_without_snap() {
    let mut b = ThemeBuilder::new(&Settings::default());
    b.selected = 0; // foreground
    let bg = b.spec.background;
    // Type the background hex into the foreground: a deliberate low-contrast
    // expert choice that must be applied verbatim (no auto-snap on entry).
    b.handle_input(OverlayInput::Activate);
    for _ in 0..8 {
        b.handle_input(OverlayInput::Backspace);
    }
    for ch in hex(bg).chars() {
        b.handle_input(OverlayInput::Char(ch));
    }
    let ThemeBuilderOutcome::Preview(_) = b.handle_input(OverlayInput::Activate) else {
        panic!("expected preview");
    };
    assert_eq!(b.color(ThemeField::Foreground), bg); // verbatim, not snapped
}

// --- U2 Step 2/3: pointer (slider / click-to-focus / Tab) ---------------

const W: usize = 72;
const H: usize = 400;

/// The body row + zone of the channel picker line.
fn channel_pick_row(b: &ThemeBuilder) -> (usize, usize, usize, usize, usize) {
    b.build_rows(W, H)
        .iter()
        .enumerate()
        .find_map(|(row, (_, zone))| match zone {
            BuilderZone::ChannelPick {
                l_x0,
                c_x0,
                h_x0,
                tok_w,
            } => Some((row, *l_x0, *c_x0, *h_x0, *tok_w)),
            _ => None,
        })
        .expect("channel picker row present")
}

/// The body row + track geometry of the focused-channel slider.
fn slider_row(b: &ThemeBuilder) -> (usize, usize, usize) {
    b.build_rows(W, H)
        .iter()
        .enumerate()
        .find_map(|(row, (_, zone))| match zone {
            BuilderZone::Slider { track_x0, track_w } => Some((row, *track_x0, *track_w)),
            _ => None,
        })
        .expect("slider row present")
}

/// The body row of the first visible role field with the given `FIELDS` index.
fn field_row(b: &ThemeBuilder, index: usize) -> usize {
    b.build_rows(W, H)
        .iter()
        .position(|(_, zone)| matches!(zone, BuilderZone::Field { index: i, .. } if *i == index))
        .expect("field row present")
}

fn field_value_hit(b: &ThemeBuilder, index: usize) -> (usize, usize) {
    b.build_rows(W, H)
        .iter()
        .enumerate()
        .find_map(|(row, (_, zone))| match zone {
            BuilderZone::Field {
                index: field_index,
                value_x0,
            } if *field_index == index => Some((row, *value_x0)),
            _ => None,
        })
        .expect("field value present")
}

#[test]
fn clicking_a_field_row_focuses_that_role() {
    let mut b = ThemeBuilder::new(&Settings::default());
    let target = FIELDS
        .iter()
        .position(|f| matches!(f, ThemeField::Cursor))
        .unwrap();
    let row = field_row(&b, target);
    assert_eq!(
        b.handle_pointer_press(W, H, row, 0, PointerButton::Left),
        ThemeBuilderOutcome::Consumed
    );
    assert_eq!(b.selected, target, "field click focuses its role");
    assert!(b.editing.is_none(), "label click leaves sliders available");
}

#[test]
fn clicking_a_field_hex_opens_entry_and_typing_replaces_the_value() {
    let mut b = ThemeBuilder::new(&Settings::default());
    let target = FIELDS
        .iter()
        .position(|f| matches!(f, ThemeField::Cursor))
        .unwrap();
    let (row, value_x0) = field_value_hit(&b, target);

    assert_eq!(
        b.handle_pointer_press(W, H, row, value_x0, PointerButton::Left),
        ThemeBuilderOutcome::Consumed
    );
    assert_eq!(b.selected, target);
    assert!(matches!(b.editing, Some(EditMode::Color { .. })));

    for ch in "#123456".chars() {
        assert_eq!(
            b.handle_input(OverlayInput::Char(ch)),
            ThemeBuilderOutcome::Consumed
        );
    }
    assert!(matches!(
        b.render_signature().editing,
        Some(ThemeBuilderEditSignature::Color { ref buffer, .. }) if buffer == "#123456"
    ));

    let ThemeBuilderOutcome::Preview(_) = b.handle_input(OverlayInput::Activate) else {
        panic!("valid direct hex entry should preview");
    };
    assert_eq!(b.color(ThemeField::Cursor), (0x12, 0x34, 0x56));
}

#[test]
fn right_clicking_a_field_hex_focuses_without_editing() {
    let mut b = ThemeBuilder::new(&Settings::default());
    let target = FIELDS
        .iter()
        .position(|f| matches!(f, ThemeField::Cursor))
        .unwrap();
    let (row, value_x0) = field_value_hit(&b, target);

    assert_eq!(
        b.handle_pointer_press(W, H, row, value_x0, PointerButton::Right),
        ThemeBuilderOutcome::Consumed
    );
    assert_eq!(b.selected, target);
    assert!(b.editing.is_none(), "right click must not open hex entry");
}

#[test]
fn clicking_a_channel_token_focuses_that_channel() {
    let mut b = ThemeBuilder::new(&Settings::default());
    assert_eq!(b.channel, OklchChannel::Lightness);
    let (row, _l, _c, h_x0, _tok) = channel_pick_row(&b);
    // Click squarely on the H token.
    let _ = b.handle_pointer_press(W, H, row, h_x0, PointerButton::Left);
    assert_eq!(b.channel, OklchChannel::Hue, "clicked H token focuses Hue");

    let (row, _l, c_x0, _h, _tok) = channel_pick_row(&b);
    let _ = b.handle_pointer_press(W, H, row, c_x0, PointerButton::Left);
    assert_eq!(
        b.channel,
        OklchChannel::Chroma,
        "clicked C token focuses Chroma"
    );
}

#[test]
fn dragging_the_slider_sets_the_focused_channel_via_core_nudge() {
    let mut b = ThemeBuilder::new(&Settings::default());
    b.selected = 0; // foreground, default channel = Lightness
    let start = b.color(ThemeField::Foreground);
    let (row, track_x0, track_w) = slider_row(&b);

    // Press the far right of the track → fraction 1.0 → set lightness to 1.0
    // via a delta through core nudge; assert it matches the math exactly.
    let l = oklch_of(start).0;
    let ThemeBuilderOutcome::Preview(_) =
        b.handle_pointer_press(W, H, row, track_x0 + track_w - 1, PointerButton::Left)
    else {
        panic!("track press previews");
    };
    assert!(b.is_dragging(), "track press arms the drag");
    assert_eq!(
        b.color(ThemeField::Foreground),
        theme_author::nudge(start, 1.0 - l, 0.0, 0.0),
        "far-right press sets lightness to the top of the range"
    );

    // Drag far left (past the edge) → fraction 0.0 → lightness 0.0.
    let mid = b.color(ThemeField::Foreground);
    let mid_l = oklch_of(mid).0;
    let ThemeBuilderOutcome::Preview(_) = b.handle_pointer_drag(W, H, 0) else {
        panic!("drag previews");
    };
    assert_eq!(
        b.color(ThemeField::Foreground),
        theme_author::nudge(mid, 0.0 - mid_l, 0.0, 0.0)
    );

    // Release ends the drag; a later move is inert.
    b.end_channel_drag();
    assert!(!b.is_dragging());
    assert_eq!(
        b.handle_pointer_drag(W, H, track_x0),
        ThemeBuilderOutcome::Consumed,
        "no drag after release"
    );
}

#[test]
fn right_click_on_the_slider_is_inert() {
    let mut b = ThemeBuilder::new(&Settings::default());
    b.selected = 0;
    let before = b.color(ThemeField::Foreground);
    let (row, track_x0, _track_w) = slider_row(&b);
    assert_eq!(
        b.handle_pointer_press(W, H, row, track_x0, PointerButton::Right),
        ThemeBuilderOutcome::Consumed
    );
    assert_eq!(
        b.color(ThemeField::Foreground),
        before,
        "right-click changes nothing"
    );
    assert!(!b.is_dragging(), "right-click does not arm a drag");
}

#[test]
fn tab_cycles_the_channel_like_the_bracket_keys() {
    let mut b = ThemeBuilder::new(&Settings::default());
    assert_eq!(b.channel, OklchChannel::Lightness);
    b.handle_input(OverlayInput::Tab);
    assert_eq!(b.channel, OklchChannel::Chroma);
    b.handle_input(OverlayInput::Tab);
    assert_eq!(b.channel, OklchChannel::Hue);
    b.handle_input(OverlayInput::Tab);
    assert_eq!(b.channel, OklchChannel::Lightness, "Tab wraps");
}

#[test]
fn a_narrow_panel_drops_the_slider_to_the_keyboard_readout() {
    let b = ThemeBuilder::new(&Settings::default());
    // Too narrow for a usable track: no Slider zone, and the channel row is
    // the plain keyboard readout (keyboard editing still works).
    let narrow = 20;
    let zones = b.visible_hit_map(narrow, H);
    assert!(
        !zones
            .iter()
            .any(|z| matches!(z, BuilderZone::Slider { .. })),
        "narrow panel has no slider"
    );
    assert!(b.channel_slider_line(narrow).is_none());
}

#[test]
fn signature_tracks_channel_focus_and_selected_color() {
    let mut b = ThemeBuilder::new(&Settings::default());
    let base = b.render_signature();
    // Cycling the channel changes the signature even though the colors did not.
    b.handle_input(OverlayInput::Tab);
    let after_channel = b.render_signature();
    assert_ne!(base.channel, after_channel.channel);

    // Nudging the selected color changes the signature's selected_color, so a
    // slider drag repaints without relying on the message field.
    b.selected = 0;
    let before_color = b.render_signature().selected_color;
    let (row, track_x0, track_w) = slider_row(&b);
    let _ = b.handle_pointer_press(W, H, row, track_x0 + track_w - 1, PointerButton::Left);
    assert_ne!(b.render_signature().selected_color, before_color);
}

#[test]
fn visible_lines_and_hit_map_stay_lockstep() {
    let b = ThemeBuilder::new(&Settings::default());
    let lines = b.visible_lines(W, H);
    let hits = b.visible_hit_map(W, H);
    assert_eq!(lines.len(), hits.len(), "lines and hit-map are 1:1");
}

/// Drive the U3 generate action end-to-end through the key path: G opens seed
/// entry, the typed accent feeds `palette_gen::generate`, and the loaded spec
/// is RV1-valid by construction — so the AA readout reads PASS (or "not
/// floored") on *every* role, never FAIL.
#[test]
fn generate_from_seed_yields_aa_clear_readout_for_every_role() {
    let mut b = ThemeBuilder::new(&Settings::default());

    // G enters seed-entry; the buffer pre-fills with the current accent.
    b.handle_input(OverlayInput::Char('g'));
    assert!(matches!(b.editing, Some(EditMode::Seed { .. })));

    // Clear the pre-fill and type a fresh seed accent.
    for _ in 0..8 {
        b.handle_input(OverlayInput::Backspace);
    }
    for ch in "#3aa0ff".chars() {
        b.handle_input(OverlayInput::Char(ch));
    }
    let out = b.handle_input(OverlayInput::Activate);
    assert!(
        matches!(out, ThemeBuilderOutcome::Preview(_)),
        "generate previews"
    );
    assert!(b.editing.is_none(), "generate exits edit mode");

    // The legibility guarantee: no floored role fails the authoring floor, so
    // the readout is PASS or "not floored" for all 24 roles.
    for (index, field) in FIELDS.iter().enumerate() {
        b.set_selection(index);
        let line = b.contrast_readout_line();
        assert!(
            !line.contains("FAIL"),
            "freshly generated {field:?} should clear AA, got: {line}"
        );
        // Cross-check the numeric guarantee for floored roles directly.
        let role = author_role(*field);
        if let Some(partner) = theme_author::partner_color(&b.spec, role) {
            let ratio = theme_author::authoring_contrast(b.color(*field), partner);
            assert!(
                ratio >= AUTHORING_CONTRAST_FLOOR,
                "{field:?} only reached {ratio:.2}"
            );
        }
    }
}

/// The generated spec follows the builder's current appearance polarity, not
/// the seed's own lightness: a bright seed in a dark draft still yields a dark
/// theme. Generation also preserves the in-progress save name.
#[test]
fn generate_honors_appearance_and_preserves_name() {
    let mut b = ThemeBuilder::new(&Settings::default());
    b.spec.appearance = Appearance::Dark;
    b.spec.name = "draft-name".to_owned();

    // A bright accent seed; appearance stays Dark, so the background stays dark.
    let out = b.generate_from_seed((250, 250, 250));
    assert!(matches!(out, ThemeBuilderOutcome::Preview(_)));
    assert_eq!(b.spec.appearance, Appearance::Dark);
    assert!(
        relative_luminance(b.spec.background) <= 0.18,
        "dark appearance keeps a dark background regardless of seed lightness"
    );
    assert_eq!(
        b.spec.name, "draft-name",
        "generation preserves the draft name"
    );
}

// --- OVERLAY-SMALL-WINDOW: role-list selection-follow + ▲/▼ affordance -------

/// On a mid-size window the role list fits far fewer than the old hardcoded
/// slack of 8, so keyboard nav must follow the selection through the *real*
/// recorded capacity. Genuine-guard: reverting `clamp` to `let visible_slack =
/// 8;` scrolls the selection off-screen here and this assert fails.
#[test]
fn role_follow_uses_real_capacity_not_fixed_slack() {
    let mut b = ThemeBuilder::new(&Settings::default());
    let (w, h) = (68, 12);
    // Seed the recorded role capacity from a real render (selected = 0).
    let _ = b.visible_lines(w, h);
    // Navigate to a high field index; clamp must scroll it into view.
    b.set_selection(12);
    let lines = b.visible_lines(w, h);
    assert!(
        lines.iter().any(|line| line.focused),
        "selected role stays visible at body_height={h}; slack=8 would scroll it off"
    );
}

/// The role list is the only windowed region: `scroll_indicator` flags overflow
/// on a short window and clears (byte-identity) once every role fits a tall one.
#[test]
fn scroll_indicator_flags_role_overflow_and_clears_when_all_fit() {
    let mut b = ThemeBuilder::new(&Settings::default());

    // Tall window: every role fits below the header block → no arrows, and all
    // 24 roles render (the affordance's byte-identity baseline).
    let _ = b.visible_lines(68, 40);
    assert_eq!(b.scroll_indicator(40), (false, false));
    let field_rows = b
        .visible_hit_map(68, 40)
        .into_iter()
        .filter(|zone| matches!(zone, BuilderZone::Field { .. }))
        .count();
    assert_eq!(
        field_rows,
        FIELDS.len(),
        "all roles render at a tall window"
    );

    // Short window at the top of the list: more below, none above.
    let _ = b.visible_lines(68, 12);
    assert_eq!(b.scroll_indicator(12), (false, true));

    // Scroll to the last role: more above, none below.
    b.set_selection(FIELDS.len() - 1);
    let _ = b.visible_lines(68, 12);
    assert_eq!(b.scroll_indicator(12), (true, false));
}

/// Esc out of seed entry cancels without authoring a theme.
#[test]
fn seed_entry_cancels_on_escape() {
    let mut b = ThemeBuilder::new(&Settings::default());
    let before = b.spec.clone();
    b.handle_input(OverlayInput::Char('g'));
    assert!(matches!(b.editing, Some(EditMode::Seed { .. })));
    b.handle_input(OverlayInput::Close);
    assert!(b.editing.is_none());
    assert_eq!(
        b.spec, before,
        "cancelled generate leaves the spec untouched"
    );
}

// ---------------------------------------------------------------------------
// THEME-CAPTURE: opening on a captured draft, and re-capturing in-editor
// ---------------------------------------------------------------------------

/// A capture-shaped spec whose colors are unmistakably not any built-in theme,
/// so "did the draft actually take the captured values" is unambiguous.
fn synthetic_capture(name: &str) -> ThemeSpec {
    crate::theme::capture_spec(
        &crate::theme::LiveColors {
            foreground: (0x12, 0x34, 0x56),
            background: (0x65, 0x43, 0x21),
            cursor: (0xAB, 0xCD, 0xEF),
            palette: std::array::from_fn(|index| (index as u8, 0x10, 0x20)),
        },
        name,
    )
}

#[test]
fn opening_captured_loads_the_captured_colors_into_the_draft() {
    let settings = Settings::default();
    let mut builder = ThemeBuilder::new(&settings);
    let captured = synthetic_capture("odyssey-capture");
    builder.open_captured(&settings, captured.clone());

    // The working draft is the captured state verbatim. (Save applies the
    // authoring AA backstop, which is a separate, already-tested behavior; the
    // capture itself must not pre-adjust anything.)
    assert_eq!(builder.spec.foreground, captured.foreground);
    assert_eq!(builder.spec.background, captured.background);
    assert_eq!(builder.spec.cursor, captured.cursor);
    assert_eq!(builder.spec.palette, captured.palette);
    assert_eq!(builder.spec.selection, captured.selection);
    assert_eq!(builder.spec.search, captured.search);
    assert_eq!(builder.spec.border, captured.border);
    assert_eq!(builder.spec.inactive, captured.inactive);
    assert_eq!(builder.spec.name, "odyssey-capture");
}

#[test]
fn opening_captured_keeps_the_applied_theme_as_the_cancel_target() {
    // The capture is a draft: cancelling restores the theme that was actually
    // applied, never the captured colors.
    let settings = Settings::default();
    let mut builder = ThemeBuilder::new(&settings);
    builder.open_captured(&settings, synthetic_capture("draft"));
    let ThemeBuilderOutcome::Cancel(theme) = builder.handle_input(OverlayInput::Close) else {
        panic!("expected cancel");
    };
    assert_eq!(theme.name, settings.theme.name);
    assert_eq!(theme.background, settings.theme.background);
}

#[test]
fn capture_key_asks_the_app_for_live_colors() {
    // The builder cannot read terminal state, so `C` must surface a request
    // rather than silently doing nothing.
    let mut builder = ThemeBuilder::new(&Settings::default());
    assert_eq!(
        builder.handle_input(OverlayInput::Char('c')),
        ThemeBuilderOutcome::CaptureLiveColors
    );
    assert_eq!(
        builder.handle_input(OverlayInput::Char('C')),
        ThemeBuilderOutcome::CaptureLiveColors
    );
}

#[test]
fn capture_key_is_inert_while_typing_a_value() {
    // `C` is a plain character inside the hex/name/seed editors; capturing
    // mid-entry would eat the keystroke.
    let mut builder = ThemeBuilder::new(&Settings::default());
    builder.handle_input(OverlayInput::Activate);
    assert_eq!(
        builder.handle_input(OverlayInput::Char('c')),
        ThemeBuilderOutcome::Consumed
    );
}

#[test]
fn applying_a_capture_mid_edit_keeps_the_draft_name() {
    // Re-capturing replaces colors, not the name the user is working under.
    let settings = Settings::default();
    let mut builder = ThemeBuilder::new(&settings);
    builder.open_captured(&settings, synthetic_capture("first-name"));
    let mut second = synthetic_capture("second-name");
    second.foreground = (0x01, 0x02, 0x03);
    builder.apply_capture(second);

    assert_eq!(builder.spec.name, "first-name");
    assert_eq!(builder.spec.foreground, (0x01, 0x02, 0x03));
}

#[test]
fn help_line_persists_and_status_region_is_fixed_height() {
    // The key-reference help line is a fixed rendered element: it must
    // survive any status change (a channel click replaces the open status,
    // which previously deleted the only copy of the help). The transient
    // status region always occupies exactly two rows, so a status appearing,
    // changing, or clearing never shifts the preview/role rows below it.
    let settings = Settings::default();
    let mut builder = ThemeBuilder::new(&settings);
    builder.open(&settings);

    let help_marker = "Tab/[ ] channel";
    let rows_with_status = builder.build_rows(W, H);
    assert!(
        rows_with_status
            .iter()
            .any(|(line, _)| line.text.contains(help_marker)),
        "help line rendered while the open status is showing"
    );
    let first_field_with_status = rows_with_status
        .iter()
        .position(|(_, zone)| matches!(zone, BuilderZone::Field { .. }))
        .expect("role rows present");

    // Clear the status entirely — the strongest layout-shift case.
    builder.message = None;
    let rows_without_status = builder.build_rows(W, H);
    assert!(
        rows_without_status
            .iter()
            .any(|(line, _)| line.text.contains(help_marker)),
        "help line still rendered after the status is cleared"
    );
    let first_field_without_status = rows_without_status
        .iter()
        .position(|(_, zone)| matches!(zone, BuilderZone::Field { .. }))
        .expect("role rows present");
    assert_eq!(
        first_field_with_status, first_field_without_status,
        "clearing the status must not shift the role rows"
    );
}
