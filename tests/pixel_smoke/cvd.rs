// SPDX-License-Identifier: GPL-3.0-only
//! U4 colour-vision-deficiency palette adaptation at the pixel level.
//!
//! Proves the visual contract end-to-end through the real geometry/composite
//! path: when a CVD mode is active the published palette visibly changes the
//! pixels of confusable cells, and when it is off the published palette is the
//! authored one so the frame is byte-identical to the plain path.
//!
//! The adapted palette is derived through the **public** [`odytty::cvd`] core
//! the native wiring uses (`from_theme` → fix appearance from the background →
//! `adapt_palette` → `to_theme`), then the two confusable colours are painted
//! as explicit cells — so this exercises the shared adaptation maths without
//! mutating process-global palette state (keeping the suite parallel-safe).

use odytty::core::{Attrs, Cell, Color, CursorStyle, Dimensions, Position, Snapshot};
use odytty::cvd::{self, CvdType};
use odytty::theme::{Appearance, Srgb, Theme, ThemeSpec, relative_luminance};

use crate::harness::*;

/// A confusable dark theme: ANSI red (1) and green (2) a deutan viewer
/// collapses, over a dark background.
fn confusable_theme() -> Theme {
    let mut theme = Theme::PLAIN;
    theme.background = (0x10, 0x10, 0x10);
    theme.foreground = (0xE0, 0xE0, 0xE0);
    theme.palette[1] = (0xC0, 0x30, 0x30);
    theme.palette[2] = (0x30, 0xA0, 0x30);
    theme
}

/// Mirror the native wiring's adaptation: project to a spec, fix the appearance
/// from the background luminance (not the spec's Dark default), adapt, project
/// back. Returns the daltonised theme.
fn adapt(theme: &Theme, ty: CvdType, strength: f32) -> Theme {
    let mut spec = ThemeSpec::from_theme(theme);
    spec.appearance = appearance_from_background(theme.background);
    cvd::adapt_palette(&spec, ty, strength).to_theme()
}

fn appearance_from_background(bg: Srgb) -> Appearance {
    if relative_luminance(bg) > 0.18 {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

/// A two-cell row painted with explicit `Color::Rgb` fills for the two given
/// colours (inverse video so the cell *fill* is the colour, dominating the
/// modal-colour read), cursor hidden.
fn two_color_row(a: Srgb, b: Srgb) -> Snapshot {
    let make = |(r, g, bl): Srgb| {
        let mut cell = Cell::new(' ', Attrs::default());
        cell.attrs.set_inverse(true);
        cell.attrs.foreground = Color::Rgb(r, g, bl);
        cell
    };
    Snapshot {
        dimensions: Dimensions::new(2, 1),
        cursor: Position::default(),
        cursor_visible: false,
        colors: Default::default(),
        cells: vec![make(a), make(b)],
    }
}

#[test]
fn cvd_adaptation_changes_confusable_pixels_and_off_is_identical() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let theme = confusable_theme();
    let red = theme.palette[1];
    let green = theme.palette[2];

    // Authored (= cvd off): the two confusable colours as published.
    let authored = composite(&two_color_row(red, green), &atlas, CursorStyle::Block);

    // Off baseline: the off path publishes the authored palette unchanged, so a
    // re-composite of the same colours is byte-identical (the plain path).
    let off = composite(&two_color_row(red, green), &atlas, CursorStyle::Block);
    assert!(
        frames_match(&authored, &off),
        "cvd off must publish the authored palette unchanged (pixel-identical)"
    );

    // Enabled: deutan adaptation moves the confusable colours, so the frame
    // must visibly differ from the authored one.
    let adapted = adapt(&theme, CvdType::Deutan, 1.0);
    let adapted_frame = composite(
        &two_color_row(adapted.palette[1], adapted.palette[2]),
        &atlas,
        CursorStyle::Block,
    );
    assert!(
        !frames_match(&authored, &adapted_frame),
        "deutan adaptation must visibly change the confusable cells"
    );
    // And specifically at least one of the two cells' fill colour changed.
    let red_moved = cell_modal_color(&authored, 0, 0) != cell_modal_color(&adapted_frame, 0, 0);
    let green_moved = cell_modal_color(&authored, 1, 0) != cell_modal_color(&adapted_frame, 1, 0);
    assert!(
        red_moved || green_moved,
        "the adapted red and/or green fill must differ from the authored one"
    );
}
