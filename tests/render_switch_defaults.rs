// SPDX-License-Identifier: GPL-3.0-only
// The process-wide render switches start at the shipped settings defaults.
// This binary holds one test and never publishes a setting, so it observes the
// values every switch has before startup or a reload publishes anything.
use odytty::settings::{
    Settings, geometric_boxdraw_enabled, ligatures_enabled, script_shaping_enabled,
    symbol_fallback_enabled, synthetic_styles_enabled,
};

#[test]
fn unpublished_render_switches_match_the_settings_defaults() {
    let defaults = Settings::default();
    assert_eq!(synthetic_styles_enabled(), defaults.synthetic_styles);
    assert_eq!(geometric_boxdraw_enabled(), defaults.geometric_boxdraw);
    assert_eq!(symbol_fallback_enabled(), defaults.symbol_fallback);
    assert_eq!(ligatures_enabled(), defaults.ligatures);
    assert_eq!(script_shaping_enabled(), defaults.script_shaping);
}
