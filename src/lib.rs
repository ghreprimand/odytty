// SPDX-License-Identifier: GPL-3.0-only
// `app` is a POSIX module top-to-bottom (module-level `rustix::termios` /
// `std::os::fd` imports for the `--interactive` headless raw-mode path), so it
// is Unix-only. The native windowed terminal does not depend on it.
#[cfg(unix)]
pub mod app;
pub mod atlas;
pub mod boxdraw;
pub mod color;
pub mod connection_hosts;
pub mod core;
pub mod cvd;
pub mod desktop;
pub mod emoji;
pub(crate) mod font_file;
pub mod fuzzy;
pub mod graphics;
pub mod grid;
pub mod hints;
pub mod input;
pub mod ligature;
pub(crate) mod local_hostname;
pub mod logging;
pub mod memory_report;
pub mod native;
pub mod palette;
pub mod palette_catalog;
pub mod palette_gen;
pub mod palette_sources;
pub mod parser;
pub mod paths;
pub mod pty;
pub mod render;
pub mod selection;
pub mod session_host;
pub mod settings;
pub mod shell_integration;
pub mod spawn_util;
pub mod ssh_config;
pub mod ssh_connect;
pub(crate) mod state_dir;
pub mod text;
pub mod theme;
pub mod theme_author;

/// Test-only serialization and restoration for the process-global render
/// knobs.
///
/// The render path reads `text::MIN_CONTRAST`, the `text` default
/// foreground/background and ANSI palette, and `atlas::STEM_DARKEN` lock-free
/// from atomics. A handful of tests set them directly, and the settings-reload
/// seam republishes them from `Settings`. Because `cargo test` runs tests in
/// parallel threads inside one process, an unguarded write is visible to an
/// unrelated test that asserts the default: both a window race (the value is
/// wrong only while the writer's scope is open) and a residual-state leak (the
/// value stays wrong for the rest of the binary's life).
///
/// This module is the single serialization point. It combines mutual exclusion
/// with automatic restoration, so correctness no longer depends on every writer
/// remembering to hand-restore a baseline on every exit path — including the
/// unwinding one.
///
/// Which readers need this guard: any assertion whose value depends on one of
/// those globals. That is coverage MAGNITUDE (ink sums, byte-identity between
/// two rasterization passes) and resolved COLOR (a floored foreground, an
/// indexed palette entry, or two vertex builds compared byte-for-byte). It is
/// NOT presence: rasterization drops zero-coverage samples before stem
/// darkening is applied and returns the `0`/`255` endpoints exactly, so
/// `ink > 0` scans and the ink-box geometry derived from them are invariant
/// under the gain, and a test that supplies its own explicit colors never
/// reaches the palette or the floor.
///
/// The snapshot covers every process-global value the settings-reload seam can
/// republish, audited from that seam outward rather than from the list of
/// globals tests happen to set today: the contrast floor, the default
/// foreground/background and ANSI palette, the stem-darkening gain, the
/// box-drawing thickness multiplier, and the six atlas/shaping switches the
/// reload helper publishes (synthetic styles, geometric box drawing,
/// ligatures, optional ss01/ss02, symbol fallback, symbol font path, symbol
/// map). A global that is
/// only latent today still belongs here: the cost of snapshotting one more
/// value is trivial next to a leak that surfaces as an unrelated test failing
/// on someone else's machine. Extend this struct, never add a second lock.
#[cfg(test)]
pub(crate) mod test_lock {
    use std::cell::Cell;
    use std::sync::{Mutex, MutexGuard};

    static RENDER_GLOBALS_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        /// Re-entrancy depth for the current thread. A test body may take the
        /// guard and then call a seam that takes it again; the inner
        /// acquisition must not deadlock on the non-reentrant mutex. Only the
        /// *mutex* is re-entrant — every scope, nested or not, snapshots on
        /// entry and restores on exit, so each scope is individually isolating.
        static RENDER_GLOBALS_DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    /// The full set of process-global render knobs a test can perturb.
    ///
    /// Captured on every acquisition and written back verbatim on drop, so a
    /// test that sets one of these — or that reaches a production seam which
    /// republishes them — cannot leak the value into whatever test libtest
    /// schedules next. Nested acquisitions capture their own entry state while
    /// only the outermost acquisition holds the mutex.
    #[derive(Clone, Debug, PartialEq)]
    pub(crate) struct RenderGlobals {
        min_contrast: f32,
        stem_darken: f32,
        box_thickness: f32,
        colors: crate::text::ColorGlobals,
        synthetic_styles: bool,
        geometric_boxdraw: bool,
        ligatures: bool,
        ligature_ss01: bool,
        ligature_ss02: bool,
        symbol_fallback: bool,
        symbol_font: Option<std::path::PathBuf>,
        symbol_map: crate::text::SymbolMap,
    }

    impl RenderGlobals {
        pub(crate) fn capture() -> Self {
            Self {
                min_contrast: crate::text::min_contrast(),
                stem_darken: crate::atlas::stem_darken_strength_for_test(),
                box_thickness: crate::boxdraw::box_thickness_for_test(),
                colors: crate::text::color_globals_for_test(),
                synthetic_styles: crate::settings::synthetic_styles_enabled(),
                geometric_boxdraw: crate::settings::geometric_boxdraw_enabled(),
                ligatures: crate::settings::ligatures_enabled(),
                ligature_ss01: crate::settings::ligature_ss01_enabled(),
                ligature_ss02: crate::settings::ligature_ss02_enabled(),
                symbol_fallback: crate::settings::symbol_fallback_enabled(),
                symbol_font: crate::settings::symbol_font_path(),
                symbol_map: crate::settings::symbol_map(),
            }
        }

        fn restore(&self) {
            crate::text::set_min_contrast(self.min_contrast);
            crate::atlas::set_stem_darken(self.stem_darken);
            crate::boxdraw::set_box_thickness(self.box_thickness);
            crate::text::set_default_colors(self.colors.default_fg, self.colors.default_bg);
            crate::text::set_ansi_palette(&self.colors.ansi_palette);
            crate::settings::set_synthetic_styles_enabled(self.synthetic_styles);
            crate::settings::set_geometric_boxdraw_enabled(self.geometric_boxdraw);
            crate::settings::set_ligatures_enabled(self.ligatures);
            crate::settings::set_ligature_ss01_enabled(self.ligature_ss01);
            crate::settings::set_ligature_ss02_enabled(self.ligature_ss02);
            crate::settings::set_symbol_fallback_enabled(self.symbol_fallback);
            crate::settings::set_symbol_font_path(self.symbol_font.clone());
            // The map slot starts as `None` and its only reader resolves that to
            // the default map, so writing the captured (possibly default) map
            // back is observationally exact even for a never-published slot.
            crate::settings::set_symbol_map(self.symbol_map.clone());
        }
    }

    /// RAII handle over the render-globals lock.
    ///
    /// Every handle snapshots the globals on entry and writes that snapshot
    /// back on drop, including while a panic unwinds. Only the outermost handle
    /// on a thread holds the mutex; a nested handle skips the acquisition (the
    /// mutex is not re-entrant) but still restores at its own scope exit, so a
    /// production seam that takes the guard leaves no residue even when the
    /// calling test already holds it.
    pub(crate) struct RenderGlobalsGuard {
        snapshot: RenderGlobals,
        lock: Option<MutexGuard<'static, ()>>,
    }

    impl Drop for RenderGlobalsGuard {
        fn drop(&mut self) {
            // Restore before releasing the mutex so no other thread can observe
            // the perturbed value between the write-back and the unlock.
            self.snapshot.restore();
            RENDER_GLOBALS_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
            drop(self.lock.take());
        }
    }

    /// Acquire the shared render-globals guard for the current scope.
    ///
    /// Process-global render knobs (`text::MIN_CONTRAST`, the default
    /// foreground/background and ANSI palette in `text`, `atlas::STEM_DARKEN`,
    /// `boxdraw::BOX_THICKNESS`, and the atlas/shaping switches in `settings`)
    /// are read lock-free by the render path and mutated by a handful of tests
    /// and by the settings-reload seam. `cargo test` runs tests in parallel
    /// threads sharing one process, so an unguarded mutation is observable by
    /// an unrelated test that asserts the default. [`RenderGlobals`] is the
    /// authoritative list; keep it in step with that seam.
    ///
    /// Every test that *sets* one of these globals, that reaches a seam which
    /// republishes them, or that *reads* one expecting a specific baseline
    /// takes this guard for the duration of its body. Holding one shared lock
    /// (rather than per-global locks) keeps the ordering trivially
    /// deadlock-free; it must never be taken while `device_creation_lock` is
    /// held. Poison is recovered with `into_inner` so a panicking test does not
    /// wedge the rest of the suite, and restoration is automatic so a panicking
    /// test cannot leak a perturbed global either.
    pub(crate) fn render_globals_lock() -> RenderGlobalsGuard {
        let outer = RENDER_GLOBALS_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current + 1);
            current == 0
        });
        let lock = if outer {
            Some(
                RENDER_GLOBALS_LOCK
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
        } else {
            None
        };
        RenderGlobalsGuard {
            snapshot: RenderGlobals::capture(),
            lock,
        }
    }

    static DEVICE_CREATION_LOCK: Mutex<()> = Mutex::new(());

    /// Serialize headless wgpu instance/adapter/device creation across parallel
    /// test threads. Concurrent device bring-up on the same adapter can deadlock
    /// inside the driver; holding this lock for the duration of each creation
    /// block makes that concurrency impossible while leaving the created device
    /// free to run in parallel afterward. The deadlock was originally assumed to
    /// be specific to the software Vulkan ICD, but a thread dump taken at a
    /// reproduced hang showed every open descriptor pointing at the accelerated
    /// device nodes and the blocked threads inside the vendor driver's own
    /// worker pool, with no software-ICD descriptor present — so the hazard is
    /// concurrent `vkCreateDevice` generally, not one ICD. Acquire it strictly around the creation block and release
    /// before taking any other test lock, so it never nests with
    /// `render_globals_lock` and cannot form a cycle. Poison is recovered with
    /// `into_inner` so a panicking test does not wedge later device creation.
    pub(crate) fn device_creation_lock() -> MutexGuard<'static, ()> {
        DEVICE_CREATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Sentinel values distinct from every shipped default, so a leak is
        /// unambiguous.
        const PROBE_CONTRAST: f32 = 13.5;
        const PROBE_STEM: f32 = 0.75;
        const PROBE_THICKNESS: f32 = 2.75;
        const PROBE_FG: (u8, u8, u8) = (1, 2, 3);
        const PROBE_BG: (u8, u8, u8) = (4, 5, 6);

        /// A symbol-map override no shipped default produces, so a restored map
        /// is distinguishable from a leaked one.
        fn probe_symbol_map() -> crate::text::SymbolMap {
            let mut map = crate::text::SymbolMap::new();
            assert!(map.push(0xE000, 0xE00F, "probe-symbol-family"));
            map
        }

        /// Move every global in the snapshot off its default, so a missing
        /// field in `restore` fails a test instead of leaking silently.
        fn perturb_every_global() {
            crate::text::set_min_contrast(PROBE_CONTRAST);
            crate::atlas::set_stem_darken(PROBE_STEM);
            crate::boxdraw::set_box_thickness(PROBE_THICKNESS);
            crate::text::set_default_colors(PROBE_FG, PROBE_BG);
            crate::text::set_ansi_palette(&[PROBE_FG; 16]);
            crate::settings::set_synthetic_styles_enabled(false);
            crate::settings::set_geometric_boxdraw_enabled(true);
            crate::settings::set_ligatures_enabled(!crate::settings::ligatures_enabled());
            crate::settings::set_ligature_ss01_enabled(true);
            crate::settings::set_ligature_ss02_enabled(true);
            crate::settings::set_symbol_fallback_enabled(false);
            crate::settings::set_symbol_font_path(Some(std::path::PathBuf::from("probe.ttf")));
            crate::settings::set_symbol_map(probe_symbol_map());
        }

        /// Read the same set the guard snapshots, so a comparison covers every
        /// field rather than the three the first version of this guard held.
        fn observed() -> RenderGlobals {
            RenderGlobals::capture()
        }

        #[test]
        fn guard_restores_every_render_global_on_drop() {
            let baseline = {
                let _guard = render_globals_lock();
                let baseline = observed();
                perturb_every_global();
                assert_eq!(crate::text::min_contrast(), PROBE_CONTRAST);
                baseline
            };
            // Re-acquire to read under the lock, so this assertion cannot race
            // another test's guarded window.
            let _guard = render_globals_lock();
            assert_eq!(observed(), baseline, "drop must restore every global");
        }

        #[test]
        fn guard_restores_globals_while_a_panic_unwinds() {
            let baseline = {
                let _guard = render_globals_lock();
                observed()
            };
            let unwound = std::panic::catch_unwind(|| {
                let _guard = render_globals_lock();
                perturb_every_global();
                panic!("simulated test failure inside a guarded body");
            });
            assert!(unwound.is_err(), "the probe body must have panicked");
            let _guard = render_globals_lock();
            assert_eq!(
                observed(),
                baseline,
                "an unwinding body must not leak a perturbed global"
            );
        }

        #[test]
        fn nested_acquisition_neither_deadlocks_nor_leaks() {
            let _outer = render_globals_lock();
            let baseline = observed();
            {
                // A production seam that takes the guard while the test body
                // already holds it must not deadlock on the non-re-entrant
                // mutex, and must restore at its own scope exit.
                let _inner = render_globals_lock();
                perturb_every_global();
            }
            assert_eq!(
                observed(),
                baseline,
                "a nested scope restores what it saw on entry"
            );
        }

        #[test]
        fn nested_scope_restores_only_to_its_own_entry_state() {
            let _outer = render_globals_lock();
            let baseline = observed();
            crate::text::set_min_contrast(PROBE_CONTRAST);
            {
                let _inner = render_globals_lock();
                crate::atlas::set_stem_darken(PROBE_STEM);
            }
            assert_eq!(
                crate::text::min_contrast(),
                PROBE_CONTRAST,
                "a nested scope must not undo state the outer scope set before it"
            );
            drop(_outer);
            let _reacquired = render_globals_lock();
            assert_eq!(
                observed(),
                baseline,
                "the outer scope restores its own baseline"
            );
        }

        /// Named coverage for the box-drawing thickness multiplier. The
        /// combined assertions above would also catch a missing write-back, but
        /// a failure there names the whole snapshot; this one names the global.
        #[test]
        fn guard_restores_box_thickness_on_drop() {
            let baseline = {
                let _guard = render_globals_lock();
                crate::boxdraw::box_thickness_for_test()
            };
            {
                let _guard = render_globals_lock();
                crate::boxdraw::set_box_thickness(PROBE_THICKNESS);
                assert_eq!(
                    crate::boxdraw::box_thickness_for_test(),
                    PROBE_THICKNESS,
                    "the probe must actually reach the global"
                );
            }
            let _guard = render_globals_lock();
            assert_eq!(
                crate::boxdraw::box_thickness_for_test(),
                baseline,
                "drop must restore the box-drawing thickness multiplier"
            );
        }

        #[test]
        fn guard_restores_box_thickness_while_a_panic_unwinds() {
            let baseline = {
                let _guard = render_globals_lock();
                crate::boxdraw::box_thickness_for_test()
            };
            let unwound = std::panic::catch_unwind(|| {
                let _guard = render_globals_lock();
                crate::boxdraw::set_box_thickness(PROBE_THICKNESS);
                panic!("simulated test failure after a thickness republish");
            });
            assert!(unwound.is_err(), "the probe body must have panicked");
            let _guard = render_globals_lock();
            assert_eq!(
                crate::boxdraw::box_thickness_for_test(),
                baseline,
                "an unwinding body must not leak a perturbed thickness"
            );
        }

        #[test]
        fn nested_scope_restores_box_thickness_to_its_own_entry_state() {
            let _outer = render_globals_lock();
            let baseline = crate::boxdraw::box_thickness_for_test();
            crate::boxdraw::set_box_thickness(PROBE_THICKNESS);
            {
                // Stands in for the reload seam, which republishes the
                // thickness through the renderer's text-options path while a
                // test body may already hold the guard.
                let _inner = render_globals_lock();
                crate::boxdraw::set_box_thickness(1.5);
            }
            assert_eq!(
                crate::boxdraw::box_thickness_for_test(),
                PROBE_THICKNESS,
                "a nested scope must restore to its own entry state, not to the default"
            );
            drop(_outer);
            let _reacquired = render_globals_lock();
            assert_eq!(
                crate::boxdraw::box_thickness_for_test(),
                baseline,
                "the outer scope restores the pre-test thickness"
            );
        }

        /// The reload seam publishes the atlas/shaping switches through
        /// `apply_reloadable_values` before it reaches the renderer, so they are
        /// part of the same leak class as the raster knobs.
        #[test]
        fn guard_restores_the_settings_switches_the_reload_seam_publishes() {
            let baseline = {
                let _guard = render_globals_lock();
                observed()
            };
            {
                let _guard = render_globals_lock();
                crate::settings::set_synthetic_styles_enabled(false);
                crate::settings::set_geometric_boxdraw_enabled(true);
                crate::settings::set_symbol_fallback_enabled(false);
                crate::settings::set_symbol_font_path(Some(std::path::PathBuf::from("probe.ttf")));
                crate::settings::set_symbol_map(probe_symbol_map());
                assert!(!crate::settings::synthetic_styles_enabled());
                assert_eq!(crate::settings::symbol_map().len(), 1);
            }
            let _guard = render_globals_lock();
            assert_eq!(
                observed(),
                baseline,
                "drop must restore every switch the reload helper republishes"
            );
        }
    }
}
