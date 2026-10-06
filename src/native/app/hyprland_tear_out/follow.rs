// SPDX-License-Identifier: GPL-3.0-only
//! Bounded Hyprland follow effects. The mailbox holds one latest frame or drop;
//! cancellation never waits on IPC, and effects target one surface incarnation.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy, Debug)]
pub(in crate::native::app) struct Geometry {
    pub offset: [f64; 2],
    pub size: [f64; 2],
}

enum Command {
    Move(Geometry),
    Drop(Destination),
}

struct Shared {
    command: Mutex<Option<Command>>,
    wake: Condvar,
    cancelled: AtomicBool,
    failed: AtomicBool,
    completed: AtomicBool,
}

pub(in crate::native::app) struct Follow {
    shared: Arc<Shared>,
    pub(in crate::native::app) released: bool,
    pub(in crate::native::app) release_target: Option<Destination>,
    pub(in crate::native::app) retired: bool,
    pub(in crate::native::app) replacement_ready: bool,
}

impl Follow {
    pub(in crate::native::app) fn start(
        identity: String,
        proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
        token: SessionToken,
    ) -> Self {
        let shared = Arc::new(Shared {
            command: Mutex::new(None),
            wake: Condvar::new(),
            cancelled: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            completed: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let started = std::thread::Builder::new()
            .name("odytty-tab-follow".to_owned())
            .spawn(move || {
                let result = run(&worker, &identity);
                if result.is_err() && !worker.cancelled.load(Ordering::Acquire) {
                    worker.failed.store(true, Ordering::Release);
                }
                if let Some(proxy) = proxy {
                    let _ = proxy.send_event(UserEvent::Redraw { session: token });
                }
            });
        if started.is_err() {
            shared.failed.store(true, Ordering::Release);
        }
        Self {
            shared,
            released: false,
            release_target: None,
            retired: false,
            replacement_ready: false,
        }
    }

    pub(in crate::native::app) fn frame(&self, geometry: Geometry) {
        if self.released || self.failed() {
            return;
        }
        *crate::native::lock_recover(&self.shared.command) = Some(Command::Move(geometry));
        self.shared.wake.notify_one();
    }

    pub(in crate::native::app) fn release(&mut self, target: Option<Destination>) {
        if self.released {
            return;
        }
        self.released = true;
        self.release_target = target;
        if let Some(target) = target {
            *crate::native::lock_recover(&self.shared.command) = Some(Command::Drop(target));
            self.shared.wake.notify_one();
        } else {
            self.shared.failed.store(true, Ordering::Release);
            self.stop();
        }
    }
    pub(in crate::native::app) fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }
    pub(in crate::native::app) fn ready(&self) -> bool {
        self.replacement_ready || self.shared.completed.load(Ordering::Acquire)
    }
    pub(in crate::native::app) fn stop(&self) {
        let mut guard = crate::native::lock_recover(&self.shared.command);
        self.shared.cancelled.store(true, Ordering::Release);
        *guard = None;
        self.shared.wake.notify_one();
    }
}
impl Drop for Follow {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
impl Follow {
    pub(in crate::native::app) fn fake() -> Self {
        Self {
            shared: Arc::new(Shared {
                command: Mutex::new(None),
                wake: Condvar::new(),
                cancelled: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                completed: AtomicBool::new(false),
            }),
            released: false,
            release_target: None,
            retired: false,
            replacement_ready: false,
        }
    }
    pub(in crate::native::app) fn fail_for_test(&self) {
        self.shared.failed.store(true, Ordering::Release);
    }
}

fn run(shared: &Shared, identity: &str) -> Result<(), ()> {
    let mut ipc = SocketIpc::from_environment(PLACEMENT_BUDGET).ok_or(())?;
    while address(&mut ipc, identity, std::process::id())?.is_none() {
        if shared.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if shared.cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut floated = false;
    loop {
        let command = {
            let mut guard = crate::native::lock_recover(&shared.command);
            while guard.is_none() && !shared.cancelled.load(Ordering::Acquire) {
                guard = shared
                    .wake
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if shared.cancelled.load(Ordering::Acquire) {
                return Ok(());
            }
            guard.take().ok_or(())?
        };
        let mut ipc = SocketIpc::from_environment(Duration::from_millis(650)).ok_or(())?;
        match command {
            Command::Move(geometry) => {
                follow_frame(&mut ipc, identity, geometry, &mut floated, || {
                    shared.cancelled.load(Ordering::Acquire)
                })?;
            }
            Command::Drop(target) => {
                if shared.cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                apply(&mut ipc, target, identity)?;
                shared.completed.store(true, Ordering::Release);
                return Ok(());
            }
        }
    }
}

fn selector(identity: &str) -> Result<String, ()> {
    if !identity.starts_with("OdyTTY-transfer-")
        || identity.len() > 80
        || !identity
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(());
    }
    Ok(format!("initialtitle:^{identity}$"))
}

fn follow_frame(
    ipc: &mut impl HyprlandIpc,
    identity: &str,
    geometry: Geometry,
    floated: &mut bool,
    cancelled: impl Fn() -> bool,
) -> Result<(), ()> {
    let window = selector(identity)?;
    let position = position(ipc, geometry)?;
    if cancelled() {
        return Ok(());
    }
    if !*floated {
        dispatch(
            ipc,
            &format!("/dispatch setfloating {window}"),
            &format!("/dispatch hl.dsp.window.float({{window='{window}',action='enable'}})"),
        )?;
        *floated = true;
    }
    if cancelled() {
        return Ok(());
    }
    let [x, y] = position;
    dispatch(
        ipc,
        &format!("/dispatch movewindowpixel exact {x} {y},{window}"),
        &format!("/dispatch hl.dsp.window.move({{window='{window}',x={x},y={y},relative=false}})"),
    )
}

fn position(ipc: &mut impl HyprlandIpc, geometry: Geometry) -> Result<[i32; 2], ()> {
    if !geometry
        .offset
        .into_iter()
        .all(|v| v.is_finite() && (0.0..=65_536.0).contains(&v))
        || !geometry
            .size
            .into_iter()
            .all(|v| v.is_finite() && (1.0..=65_536.0).contains(&v))
    {
        return Err(());
    }
    let cursor = json(ipc, "j/cursorpos")?;
    let point = [
        number(&cursor, "x").ok_or(())?,
        number(&cursor, "y").ok_or(())?,
    ];
    let monitors = json(ipc, "j/monitors")?;
    let monitors = monitors.as_array().ok_or(())?;
    if monitors.len() > 64 {
        return Err(());
    }
    for monitor in monitors {
        let origin = [
            number(monitor, "x").ok_or(())?,
            number(monitor, "y").ok_or(())?,
        ];
        let mut extent = [
            number(monitor, "width").ok_or(())?,
            number(monitor, "height").ok_or(())?,
        ];
        let scale = number(monitor, "scale").ok_or(())?;
        let transform = integer(monitor, "transform").ok_or(())?;
        if !(0..=7).contains(&transform)
            || !(0.25..=8.0).contains(&scale)
            || !extent.into_iter().all(|v| (1.0..=65_536.0).contains(&v))
        {
            return Err(());
        }
        if transform % 2 == 1 {
            extent.swap(0, 1);
        }
        extent = extent.map(|v| v / scale);
        if (0..2)
            .all(|axis| point[axis] >= origin[axis] && point[axis] < origin[axis] + extent[axis])
        {
            let coordinate = |axis: usize| {
                let end = origin[axis] + (extent[axis] - geometry.size[axis]).max(0.0);
                let value = (point[axis] - geometry.offset[axis])
                    .clamp(origin[axis], end)
                    .round();
                if value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
                    Err(())
                } else {
                    Ok(value as i32)
                }
            };
            return Ok([coordinate(0)?, coordinate(1)?]);
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{MONITORS, fake};
    use super::*;
    #[test]
    fn follow_uses_logical_cursor_and_clamps_negative_and_mixed_scale_monitors() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":2000,"y":400}"#),
            ("j/monitors", MONITORS),
        ]);
        assert_eq!(
            position(
                &mut ipc,
                Geometry {
                    offset: [20.0, 8.0],
                    size: [640.0, 384.0]
                }
            ),
            Ok([1980, 392])
        );
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":5,"y":-2}"#),
            ("j/monitors", MONITORS),
        ]);
        assert_eq!(
            position(
                &mut ipc,
                Geometry {
                    offset: [20.0, 8.0],
                    size: [640.0, 384.0]
                }
            ),
            Ok([0, -384])
        );
    }
    #[test]
    fn follow_clamps_the_bottom_edge_with_fractional_monitor_scale() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":2000,"y":500}"#),
            ("j/monitors", MONITORS),
        ]);
        // Logical bottom is 1440/1.667; a 384-high window ends at 479.827...
        assert_eq!(
            position(
                &mut ipc,
                Geometry {
                    offset: [20.0, 8.0],
                    size: [640.0, 384.0]
                }
            ),
            Ok([1980, 480])
        );
    }
    #[test]
    fn follow_float_and_move_use_only_the_unique_initial_title() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":2000,"y":400}"#),
            ("j/monitors", MONITORS),
            (
                "/dispatch setfloating initialtitle:^OdyTTY-transfer-42-7-9$",
                "ok",
            ),
            (
                "/dispatch movewindowpixel exact 1980 392,initialtitle:^OdyTTY-transfer-42-7-9$",
                "ok",
            ),
        ]);
        let mut floated = false;
        assert!(
            follow_frame(
                &mut ipc,
                "OdyTTY-transfer-42-7-9",
                Geometry {
                    offset: [20.0, 8.0],
                    size: [640.0, 384.0]
                },
                &mut floated,
                || false
            )
            .is_ok()
        );
        assert!(floated);
    }
    #[test]
    fn follow_dispatch_failure_after_float_stops_further_effects() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":2000,"y":400}"#),
            ("j/monitors", MONITORS),
            (
                "/dispatch setfloating initialtitle:^OdyTTY-transfer-42-7-9$",
                "ok",
            ),
            (
                "/dispatch movewindowpixel exact 1980 392,initialtitle:^OdyTTY-transfer-42-7-9$",
                "refused",
            ),
        ]);
        let mut floated = false;
        assert!(
            follow_frame(
                &mut ipc,
                "OdyTTY-transfer-42-7-9",
                Geometry {
                    offset: [20.0, 8.0],
                    size: [640.0, 384.0]
                },
                &mut floated,
                || false
            )
            .is_err()
        );
        assert!(floated, "failure may occur after the floating side effect");
    }
    #[test]
    fn follow_lua_mode_uses_explicit_coordinates_and_window_identity() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":2000,"y":400}"#),
            ("j/monitors", MONITORS),
            (
                "/dispatch setfloating initialtitle:^OdyTTY-transfer-42-7-9$",
                "dispatch in lua is a shorthand for hl.dispatch",
            ),
            (
                "/dispatch hl.dsp.window.float({window='initialtitle:^OdyTTY-transfer-42-7-9$',action='enable'})",
                "ok",
            ),
            (
                "/dispatch movewindowpixel exact 1980 392,initialtitle:^OdyTTY-transfer-42-7-9$",
                "dispatch in lua is a shorthand for hl.dispatch",
            ),
            (
                "/dispatch hl.dsp.window.move({window='initialtitle:^OdyTTY-transfer-42-7-9$',x=1980,y=392,relative=false})",
                "ok",
            ),
        ]);
        assert!(
            follow_frame(
                &mut ipc,
                "OdyTTY-transfer-42-7-9",
                Geometry {
                    offset: [20.0, 8.0],
                    size: [640.0, 384.0]
                },
                &mut false,
                || false
            )
            .is_ok()
        );
    }
    #[test]
    fn follow_rejects_nonfinite_geometry_and_selector_injection_before_effects() {
        let mut ipc = fake(&[]);
        assert!(
            position(
                &mut ipc,
                Geometry {
                    offset: [f64::NAN, 0.0],
                    size: [640.0, 384.0]
                }
            )
            .is_err()
        );
        assert!(selector("OdyTTY-transfer-42;other").is_err());
        assert!(selector("OdyTTY-transfer-42-7-9").is_ok());
    }
}
