// SPDX-License-Identifier: GPL-3.0-only
//! A followed-palette status change must request a repaint, because the
//! Settings status row is painted only when a frame is requested. Drives the
//! real follower poll on an App with an owned palette file.

use super::*;
use crate::external_palette::ExternalPaletteProvider;
use crate::test_dirs::fresh_temp_dir;
use std::time::{Duration, Instant};

const PALETTE: &str = "\
foreground = #112233
background = #112233
clear = #112233
cursor = #112233
selection = #112233
search = #112233
border = #112233
inactive = #112233
bright_foreground = #112233
muted = #112233
dark_foreground = #112233
darker_background = #112233
color0 = #112233
color1 = #112233
color2 = #112233
color3 = #112233
color4 = #112233
color5 = #112233
color6 = #112233
color7 = #112233
color8 = #112233
color9 = #112233
color10 = #112233
color11 = #112233
color12 = #112233
color13 = #112233
color14 = #112233
color15 = #112233
";

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn followed_palette_status_changes_repaint_the_settings_row() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let root = Scratch(fresh_temp_dir("odytty-palette-status-redraw-"));
    super::config_env::with_config_base(&root.0, true, || {
        let _reads = crate::test_lock::palette_read_lock();
        let path = root.0.join("palette.txt");
        std::fs::write(&path, PALETTE).expect("owned palette source");
        let settings = Settings {
            follow_external_palette: true,
            external_palette_provider: ExternalPaletteProvider::OdyttyAnsi,
            external_palette_path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let dims = Dimensions::new(80, 24);
        let (mut app, _terminal) = headless_app_with(NativeOptions::default(), dims, settings);
        let start = Instant::now();

        app.sync_palette_follow_for_test(start);
        assert_eq!(
            app.palette_status_row_for_test().as_deref(),
            Some("applied")
        );
        let after_apply = app.palette_status_redraws_for_test();

        // Source removed: the status row changes to the retained state.
        std::fs::remove_file(&path).expect("remove owned palette source");
        app.poll_palette_follow_for_test(start + Duration::from_secs(2));
        let retained = app.palette_status_row_for_test().expect("status row");
        assert!(retained.starts_with("retained"), "{retained}");
        let after_retain = app.palette_status_redraws_for_test();
        assert!(
            after_retain > after_apply,
            "a status change from the poll must request a repaint"
        );

        // A poll with nothing new changes no row and requests nothing.
        app.poll_palette_follow_for_test(start + Duration::from_secs(3));
        assert_eq!(app.palette_status_redraws_for_test(), after_retain);

        // Source restored: the row returns to applied and repaints again.
        std::fs::write(&path, PALETTE).expect("restore owned palette source");
        app.poll_palette_follow_for_test(start + Duration::from_secs(4));
        assert_eq!(
            app.palette_status_row_for_test().as_deref(),
            Some("applied")
        );
        assert!(app.palette_status_redraws_for_test() > after_retain);
    });
}
