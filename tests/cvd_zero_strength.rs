// SPDX-License-Identifier: GPL-3.0-only
// Project-authored palette exercises zero-strength role correction.
use odytty::cvd::{CvdType, adapt_palette, cvd_adapt};
use odytty::theme::{Appearance, ThemeSpec, contrast_ratio};

#[test]
fn zero_strength_corrects_unfloored_roles_without_color_adaptation() {
    let spec = ThemeSpec {
        name: "zero-strength fixture".into(),
        appearance: Appearance::Dark,
        background: (0, 0, 0),
        foreground: (4, 4, 4),
        palette: [(4, 4, 4); 16],
        cursor: (4, 4, 4),
        selection: (4, 4, 4),
        search: (4, 4, 4),
        ..ThemeSpec::default()
    };
    assert!(contrast_ratio(spec.foreground, spec.background) < 4.5);
    for ty in [CvdType::Protan, CvdType::Deutan, CvdType::Tritan] {
        for color in [spec.foreground, spec.cursor, spec.selection, spec.search] {
            assert_eq!(cvd_adapt(color, ty, 0.0), color);
        }
        let out = adapt_palette(&spec, ty, 0.0);
        assert_ne!(out.foreground, spec.foreground);
        assert_ne!(out.cursor, spec.cursor);
        assert!(contrast_ratio(out.foreground, out.background) >= 4.5);
        assert!(contrast_ratio(out.cursor, out.background) >= 4.5);
        for (index, (&actual, &original)) in out.palette.iter().zip(&spec.palette).enumerate() {
            if [0, 8].contains(&index) {
                assert_eq!(actual, original);
            } else {
                assert_ne!(actual, original);
                assert!(contrast_ratio(actual, out.background) >= 4.5);
            }
        }
        assert!(contrast_ratio(out.selection, out.foreground) >= 4.5);
        assert!(contrast_ratio(out.search, out.foreground) >= 4.5);
        assert_eq!(out.background, spec.background);
        assert_eq!(out.clear, spec.clear);
        assert_eq!(out.border, spec.border);
        assert_eq!(out.inactive, spec.inactive);
        assert_eq!(out.name, spec.name);
        assert_eq!(out.appearance, spec.appearance);
        assert_eq!(out.font_family, spec.font_family);
        assert_eq!(out.font_size, spec.font_size);
        assert_eq!(out.visual, spec.visual);
        assert_eq!(adapt_palette(&out, ty, 0.0), out);
    }
}
