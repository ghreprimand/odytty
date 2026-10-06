// SPDX-License-Identifier: GPL-3.0-only
//! Independent script/Latin switches through the real Settings input path.
//! Pixels use the production shapers, atlas and cell vertex builder with
//! existing project-authored Arabic and OFL Devanagari fixtures.

use super::*;
use crate::atlas::GlyphAtlas;
use crate::complex_shaping::{ComplexShaper, merge_runs};
use crate::grid::{
    BackgroundTreatmentParams, BidiDisplayMap, ChromePin, INSTANCES_PER_QUAD, Vertex,
    build_cell_vertices_with_bidi_into,
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into,
};
use crate::ligature::{LatinShapingFeatures, LigatureFonts, LigatureShaper, ShapingSwitches};
use crate::text::{FontHandle, FontStyle};
use winit::keyboard::{Key, NamedKey};

struct Fonts(FontHandle);
impl LigatureFonts for Fonts {
    fn ligature_font(&self, _: FontStyle) -> &FontHandle {
        &self.0
    }
}

fn select(app: &mut App, row: &str) {
    app.drive_overlay_key_for_test(Key::Named(NamedKey::Home), false, false);
    for section in 0..24 {
        for _ in 0..section {
            app.drive_overlay_key_for_test(Key::Named(NamedKey::ArrowDown), false, false);
        }
        app.drive_overlay_key_for_test(Key::Named(NamedKey::Enter), false, false);
        let panel = app.overlay_signature_for_test().panel;
        if let Some(target) = panel.entries.iter().position(|entry| entry.key == row) {
            for key in ["ligatures", "script_shaping"] {
                assert!(panel.entries.iter().any(|entry| entry.key == key));
            }
            app.drive_overlay_key_for_test(Key::Named(NamedKey::Home), false, false);
            for _ in 0..target {
                app.drive_overlay_key_for_test(Key::Named(NamedKey::ArrowDown), false, false);
            }
            return;
        }
        app.drive_overlay_key_for_test(Key::Named(NamedKey::Escape), false, false);
        for _ in 0..section {
            app.drive_overlay_key_for_test(Key::Named(NamedKey::ArrowUp), false, false);
        }
    }
    panic!("missing rendering row {row}");
}

fn toggle(app: &mut App, row: &str) {
    select(app, row);
    let epoch = app.presentation_epoch_for_test();
    app.drive_overlay_key_for_test(Key::Named(NamedKey::Enter), false, false);
    app.flush_pending_overlay_settings_for_test();
    assert!(
        app.presentation_epoch_for_test() > epoch,
        "{row} rekeys the frame, current {:?}",
        app.shaping_switches_for_test()
    );
    app.drive_overlay_key_for_test(Key::Named(NamedKey::Escape), false, false);
}

fn render(
    snapshot: &Snapshot,
    fonts: &Fonts,
    switches: ShapingSwitches,
    bidi: bool,
    shaper: &mut LigatureShaper,
    scalar: bool,
) -> Vec<[f32; 3]> {
    let mut atlas = GlyphAtlas::build(&fonts.0, 28.0);
    for cell in &snapshot.cells {
        if !cell.wide_continuation {
            atlas.ensure(&fonts.0, cell.ch);
            for &ch in cell.combining() {
                atlas.ensure(&fonts.0, ch);
            }
        }
    }
    let map = bidi.then(|| BidiDisplayMap::plan(snapshot, &vec![false; snapshot.dimensions.rows]));
    let mut runs = if scalar {
        vec![]
    } else {
        shaper.build_runs_with_switches(
            switches,
            snapshot,
            fonts,
            &[],
            LatinShapingFeatures::default(),
            map.as_ref(),
        )
    };
    for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
        atlas.ensure_shaped(&fonts.0, glyph.key);
        assert!(atlas.contains_shaped(glyph.key));
    }
    if !scalar {
        let complex = ComplexShaper::new().build_runs_with_switches(
            switches,
            snapshot,
            fonts,
            &mut atlas,
            &[],
        );
        merge_runs(&mut runs, complex);
    }
    let mut verts = vec![];
    if let Some(map) = map {
        build_cell_vertices_with_bidi_into(&mut verts, snapshot, &atlas, &[], &runs, &map);
    } else {
        build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
            &mut verts,
            snapshot,
            &atlas,
            &[],
            &runs,
            0.0,
            [0.0, 0.0],
            BackgroundTreatmentParams::default(),
            1.0,
            1.0,
            None,
            ChromePin::NONE,
        );
    }
    composite(snapshot, &atlas, &verts)
}

fn composite(snapshot: &Snapshot, atlas: &GlyphAtlas, verts: &[Vertex]) -> Vec<[f32; 3]> {
    let cell_w = atlas.cell.width as usize;
    let width = snapshot.dimensions.columns * cell_w;
    let height = snapshot.dimensions.rows * atlas.cell.height as usize;
    let mut px = vec![[0.0_f32; 3]; width * height];
    for quad in verts.as_chunks::<INSTANCES_PER_QUAD>().0 {
        let v = &quad[0];
        let [x0, y0] = v.pos;
        let [x1, y1] = v.end_pos;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        for py in (y0.floor().max(0.0) as usize)..(y1.ceil() as usize).min(height) {
            let cy = py as f32 + 0.5;
            if cy < y0 || cy >= y1 {
                continue;
            }
            for px_x in (x0.floor().max(0.0) as usize)..(x1.ceil() as usize).min(width) {
                let cx = px_x as f32 + 0.5;
                if cx < x0 || cx >= x1 {
                    continue;
                }
                let alpha = if v.is_glyph > 0.5 {
                    let u = v.uv[0] + (cx - x0) / (x1 - x0) * (v.end_uv[0] - v.uv[0]);
                    let t = v.uv[1] + (cy - y0) / (y1 - y0) * (v.end_uv[1] - v.uv[1]);
                    let ax = ((u * atlas.width as f32) as usize).min(atlas.width as usize - 1);
                    let ay = ((t * atlas.height as f32) as usize).min(atlas.height as usize - 1);
                    v.color[3] * f32::from(atlas.data[ay * atlas.width as usize + ax]) / 255.0
                } else {
                    v.color[3]
                };
                let dst = &mut px[py * width + px_x];
                for (out, ink) in dst.iter_mut().zip(v.color) {
                    *out = ink * alpha + *out * (1.0 - alpha);
                }
            }
        }
    }
    px
}

#[test]
fn independent_switches_change_pixels_and_live_settings_rekey_the_frame() {
    let _globals = crate::test_lock::render_globals_lock();
    let devanagari = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/s5b/northern-indic/Devanagari-subset.ttf")
            .to_vec(),
    )
    .unwrap();
    let arabic = FontHandle::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/arabic-marks.ttf").to_vec(),
    )
    .unwrap();
    let latin = crate::text::load_bundled_font().unwrap();
    for bidi in [false, true] {
        for (text, font, script) in [
            ("\u{0915}\u{094D}\u{0937}", devanagari.clone(), true),
            ("\u{0628}\u{064E}\u{0644}\u{0627}", arabic.clone(), true),
            ("->", latin.clone(), false),
        ] {
            let (mut app, terminal) = headless_app_with(
                NativeOptions::default(),
                Dimensions::new(16, 2),
                Settings::default(),
            );
            let conf_dir =
                std::env::temp_dir().join(format!("odytty-script-shaping-{}", std::process::id()));
            std::fs::create_dir_all(&conf_dir).unwrap();
            let conf = conf_dir.join("odytty.conf");
            std::fs::write(&conf, "# kept\n").unwrap();
            app.set_config_path_for_test(conf.clone());
            app.open_settings_overlay_for_test();
            let snapshot = {
                let mut t = terminal.lock().unwrap();
                t.advance(b"\x1b[?25l");
                t.advance(text.as_bytes());
                t.snapshot()
            };
            let fonts = Fonts(font);
            let mut shaper = LigatureShaper::new();
            let switches = |app: &App| app.shaping_switches_for_test();
            let scalar = render(&snapshot, &fonts, switches(&app), bidi, &mut shaper, true);
            let on = render(&snapshot, &fonts, switches(&app), bidi, &mut shaper, false);
            assert!(on != scalar, "fixture must shape: {text:?}, bidi={bidi}");
            toggle(&mut app, "ligatures");
            assert!(!switches(&app).ligatures && switches(&app).scripts);
            let latin_off = render(&snapshot, &fonts, switches(&app), bidi, &mut shaper, false);
            assert!(
                latin_off == if script { on.clone() } else { scalar.clone() },
                "Latin switch scope: {text:?}, bidi={bidi}"
            );
            toggle(&mut app, "ligatures");
            assert!(
                render(&snapshot, &fonts, switches(&app), bidi, &mut shaper, false) == on,
                "toggling back restores pixels: {text:?}, bidi={bidi}"
            );
            toggle(&mut app, "script_shaping");
            assert!(switches(&app).ligatures && !switches(&app).scripts);
            let scripts_off = render(&snapshot, &fonts, switches(&app), bidi, &mut shaper, false);
            assert!(
                scripts_off == if script { scalar.clone() } else { on.clone() },
                "Script switch scope: {text:?}, bidi={bidi}"
            );
            toggle(&mut app, "script_shaping");
            assert!(
                render(&snapshot, &fonts, switches(&app), bidi, &mut shaper, false) == on,
                "toggling back restores pixels: {text:?}, bidi={bidi}"
            );
            assert_eq!(terminal.lock().unwrap().snapshot().cells, snapshot.cells);
            std::fs::remove_dir_all(&conf_dir).unwrap();
        }
    }
}
