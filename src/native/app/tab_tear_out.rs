// SPDX-License-Identifier: GPL-3.0-only
//! Tab tear-out gesture and release intent, shared by all native platforms.
use super::*;

/// Distance beyond the surface edge required to arm a tear-out, in physical px.
const EDGE_THRESHOLD: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct TearOutRelease {
    /// Global physical release point, unavailable on Wayland.
    pub(in crate::native) position: Option<[i32; 2]>,
    pub(in crate::native) tab: SessionToken,
    #[cfg(target_os = "linux")]
    pub(in crate::native) hyprland: Option<super::hyprland_tear_out::Destination>,
    #[cfg(target_os = "linux")]
    pub(in crate::native) hyprland_requested: bool,
}

impl App {
    pub(super) fn tab_tear_out_at(&self, x: f64, y: f64) -> bool {
        // winit's current client size is authoritative during a debounced GPU
        // resize. Headless input tests use the same surface geometry seam.
        let size = self
            .window
            .as_ref()
            .map(|window| {
                let size = window.inner_size();
                (size.width, size.height)
            })
            .or_else(|| {
                self.resolved_surface()
                    .map(|(width, height, _)| (width, height))
            });
        let Some((width, height)) = size else {
            return false;
        };
        width > 0
            && height > 0
            && x.is_finite()
            && y.is_finite()
            && (x <= -EDGE_THRESHOLD
                || y <= -EDGE_THRESHOLD
                || x >= f64::from(width) + EDGE_THRESHOLD
                || y >= f64::from(height) + EDGE_THRESHOLD)
    }

    pub(super) fn tab_tear_out_signature(&self) -> bool {
        self.top_tab_drag.is_some_and(|drag| drag.tear_out)
            && self
                .window_pointer_px
                .is_some_and(|(x, y)| self.tab_tear_out_at(x, y))
    }

    pub(super) fn request_tab_tear_out(&mut self, token: SessionToken) {
        let position = self.window.as_ref().and_then(|window| {
            let origin = window.inner_position().ok()?;
            let (x, y) = self.window_pointer_px?;
            global_release([origin.x, origin.y], [x, y])
        });
        if !self.sessions.owns_session(token) {
            return;
        }
        #[cfg(target_os = "linux")]
        let hyprland_requested = self.window.as_ref().is_some_and(|window| {
            use winit::platform::wayland::WindowExtWayland;
            window.xdg_toplevel().is_some()
                && std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        });
        #[cfg(target_os = "linux")]
        let hyprland = hyprland_requested
            .then(super::hyprland_tear_out::capture_destination)
            .flatten();
        self.pending_move = Some(reparent::MoveRequest::TearOut(TearOutRelease {
            position,
            tab: token,
            #[cfg(target_os = "linux")]
            hyprland,
            #[cfg(target_os = "linux")]
            hyprland_requested,
        }));
    }
}

fn global_release(origin: [i32; 2], point: [f64; 2]) -> Option<[i32; 2]> {
    let convert = |origin, value: f64| {
        let value = f64::from(origin) + value;
        if !value.is_finite() || value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
            None
        } else {
            Some(value.round() as i32)
        }
    };
    Some([convert(origin[0], point[0])?, convert(origin[1], point[1])?])
}

/// Surface placement effect, separate from coordinate decisions for fake tests.
trait WindowPlacement {
    fn monitors(&self) -> Vec<([i32; 2], [u32; 2])>;
    fn size(&self) -> [u32; 2];
    fn position(&mut self, position: [i32; 2]);
}

struct NativePlacement<'a>(&'a Window);
impl WindowPlacement for NativePlacement<'_> {
    fn monitors(&self) -> Vec<([i32; 2], [u32; 2])> {
        self.0
            .available_monitors()
            .take(64)
            .map(|monitor| {
                let p = monitor.position();
                let s = monitor.size();
                ([p.x, p.y], [s.width, s.height])
            })
            .collect()
    }
    fn size(&self) -> [u32; 2] {
        let size = self.0.outer_size();
        [size.width, size.height]
    }
    fn position(&mut self, p: [i32; 2]) {
        self.0
            .set_outer_position(winit::dpi::PhysicalPosition::new(p[0], p[1]));
    }
}

pub(super) fn place_native(window: &Window, release: TearOutRelease) {
    place(&mut NativePlacement(window), release);
}

fn place(surface: &mut impl WindowPlacement, release: TearOutRelease) {
    let Some(point) = release.position else {
        return;
    };
    for (origin, size) in surface.monitors() {
        let contains = (0..2).all(|axis| {
            let point = i64::from(point[axis]);
            let start = i64::from(origin[axis]);
            point >= start && point < start + i64::from(size[axis])
        });
        if contains {
            let extent = surface.size();
            let clamp = |axis: usize| {
                let start = i64::from(origin[axis]);
                let end = start + i64::from(size[axis].saturating_sub(extent[axis]));
                i64::from(point[axis])
                    .clamp(start, end)
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
            };
            surface.position([clamp(0), clamp(1)]);
            return;
        }
    }
}

#[cfg(test)]
impl App {
    pub(in crate::native) fn tear_out_visual_for_test(&self) -> (bool, String) {
        let output = self.render_top_bar_widget(
            80,
            0.0,
            self.resolved_cell().expect("cell"),
            WindowPadding::ZERO,
        );
        (
            self.tab_tear_out_signature(),
            output.glyphs.iter().map(|glyph| glyph.ch).collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct PlacementFake {
        monitors: Vec<([i32; 2], [u32; 2])>,
        size: [u32; 2],
        placed: Vec<[i32; 2]>,
    }
    impl WindowPlacement for PlacementFake {
        fn monitors(&self) -> Vec<([i32; 2], [u32; 2])> {
            self.monitors.clone()
        }
        fn size(&self) -> [u32; 2] {
            self.size
        }
        fn position(&mut self, p: [i32; 2]) {
            self.placed.push(p);
        }
    }
    fn release(point: Option<[i32; 2]>) -> TearOutRelease {
        TearOutRelease {
            position: point,
            tab: SessionToken(0),
            #[cfg(target_os = "linux")]
            hyprland: None,
            #[cfg(target_os = "linux")]
            hyprland_requested: false,
        }
    }
    #[test]
    fn placement_clamps_to_the_negative_monitor_under_the_release_point() {
        let mut fake = PlacementFake {
            monitors: vec![([-1600, -900], [1600, 900]), ([0, 0], [1920, 1080])],
            size: [640, 384],
            ..Default::default()
        };
        place(&mut fake, release(Some([-20, -20])));
        assert_eq!(fake.placed, vec![[-640, -384]]);
        place(&mut fake, release(Some([1900, 1070])));
        assert_eq!(fake.placed, vec![[-640, -384], [1280, 696]]);
    }
    #[test]
    fn missing_wayland_positions_and_unmatched_monitors_do_not_position() {
        let mut fake = PlacementFake {
            monitors: vec![([0, 0], [640, 384])],
            size: [800, 600],
            ..Default::default()
        };
        place(&mut fake, release(None));
        place(&mut fake, release(Some([-2, -2])));
        assert!(fake.placed.is_empty());
        place(&mut fake, release(Some([600, 300])));
        assert_eq!(
            fake.placed,
            vec![[0, 0]],
            "oversize windows anchor at the monitor origin"
        );
    }
    #[test]
    fn global_release_preserves_signed_coordinates_and_rejects_nonfinite_or_overflow() {
        assert_eq!(global_release([-200, 30], [-50.0, 10.0]), Some([-250, 40]));
        assert_eq!(global_release([i32::MAX, 0], [2.0, 0.0]), None);
        assert_eq!(global_release([0, 0], [f64::NAN, 0.0]), None);
    }
}
