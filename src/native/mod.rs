//! Native window + GPU surface (Linux-first, Wayland-native).
//!
//! This module owns the seam between the OS window/event loop, the GPU surface,
//! and the rest of OdyTTY. It is built up incrementally so each piece is
//! reviewable on its own:
//!
//! - **Window lifecycle** — a `winit` window that opens and closes cleanly.
//! - **GPU surface bring-up** — a `wgpu` surface/device/queue that survives
//!   resize.
//! - **Glyph text rendering** — the `crate::text` atlas is uploaded to an
//!   `R8Unorm` texture and the `crate::grid` geometry is drawn as textured
//!   quads with the shared `cell.wgsl` pipeline, so the window shows readable
//!   monospaced text.
//!
//! The surface is cleared to the active theme's clear color before the cell
//! geometry is drawn over it. The theme (selected by `ODYTTY_THEME`, defaulting
//! to the plain baseline) also sets the default foreground/background used for
//! `Color::Default` cells. Themes are presentation-only: they never touch the
//! terminal core or cell attributes (see [`crate::theme`]).
//!
//! The window now opens a real shell: PTY output is rendered live and keyboard
//! input is encoded and written back to the PTY (via the shared
//! [`crate::input`] encoder), so the read+write loop is complete. Still
//! deliberately absent: richer Odyssey visual treatments (motion/effects) and
//! workflow polish beyond the first daily-loop basics.
//!
//! ## Ownership split (filled in incrementally)
//!
//! The native app keeps the owned terminal core (`crate::core`) separate from
//! windowing, GPU rendering, and any later Odyssey visual layer:
//!
//! - **Event loop** — `winit` owns the OS window, input events, and resize.
//!   *(done)*
//! - **GPU surface/device** — `wgpu` owns the surface, device, queue, and swap
//!   chain, presenting frames to the window. *(this packet: solid clear only)*
//! - **Glyph atlas / text renderer** — a CPU-rasterized monospace glyph atlas
//!   (`crate::text`) uploaded to a `wgpu` texture; cells drawn as textured
//!   quads. *(this packet)*
//! - **Grid presentation** — maps an owned-core `Snapshot` to positioned cell
//!   quads via `crate::grid`, with no terminal semantics in the renderer.
//!   *(this packet)*
//!
//! ## Linux / Wayland
//!
//! `winit` compiles in both Wayland and X11 backends and selects Wayland at
//! runtime when `WAYLAND_DISPLAY` is set, so under Hyprland this is a native
//! Wayland surface (no XWayland). `wgpu` presents to that surface via its
//! default backends (Vulkan on Linux), so the GPU path is Wayland-native too.

mod app;
mod bindings;
mod clipboard;
mod cursor;
mod gpu;
mod image_layer;
mod options;
mod overlay;
mod pty;
mod render_helpers;
mod resize;
mod search_ui;
mod viewport;

#[cfg(test)]
mod image_layer_tests;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::core::{RgbColor, Terminal};
use crate::pty::PtySession;
use crate::settings::Settings;
use crate::text;

use winit::event_loop::{ControlFlow, EventLoop};

pub use options::{NativeError, NativeOptions};

use app::App;
use pty::{PtyWriter, UserEvent, spawn_pty_pump};

pub fn run_native(options: NativeOptions, settings: Settings) -> Result<(), NativeError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| NativeError::EventLoop(err.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // Select the presentation theme once via Settings and apply its default cell
    // colors process-wide before any rendering. This only
    // affects how Color::Default paints; the terminal core is unaware of it.
    let theme = settings.theme;
    text::set_default_colors(theme.foreground, theme.background);
    text::set_ansi_palette(&theme.palette);
    // Publish the synthetic-styles kill switch process-wide before the GPU
    // surface (and its glyph atlas) is built on resume, so a launch-time
    // `synthetic_styles = off` is honored from the first frame. The config
    // reload path republishes it on change (see `apply_reloadable_values`).
    crate::settings::set_synthetic_styles_enabled(settings.synthetic_styles);
    // Shared terminal model, sized to the initial grid. The pump thread writes
    // to it; the UI thread snapshots from it.
    let mut model = Terminal::new(options.initial_grid.columns, options.initial_grid.rows);
    model.set_base_colors(
        rgb(theme.foreground),
        rgb(theme.background),
        rgb(theme.foreground),
    );
    model.set_osc52_read_enabled(settings.osc52_read);
    // Apply the host default cursor shape/blink policy from settings before any
    // output. An application's DECSCUSR can still override this at runtime; RIS/
    // DECSTR return to it. Presentation policy only — the grid contents are
    // unaffected.
    model.set_cursor_defaults(settings.cursor_style, settings.cursor_blink.enabled());
    let terminal = Arc::new(Mutex::new(model));

    // Spawn the shell PTY and start pumping its output into the shared terminal.
    let session = PtySession::spawn_default_shell(options.initial_grid)
        .map_err(|err| NativeError::Pty(err.to_string()))?;
    let reader = session
        .try_clone_reader()
        .map_err(|err| NativeError::Pty(err.to_string()))?;
    // One writer, shared: the pump thread sends host responses through it, and
    // the App sends encoded keystrokes through its clone.
    let writer: PtyWriter = Arc::new(Mutex::new(
        session
            .take_writer()
            .map_err(|err| NativeError::Pty(err.to_string()))?,
    ));

    let proxy = event_loop.create_proxy();
    let pump_thread = spawn_pty_pump(reader, writer.clone(), terminal.clone(), proxy);

    // Share the session: the App pushes window-size changes to it on resize,
    // and this function reaps the child on the way out.
    let session = Arc::new(Mutex::new(session));

    let mut app = App::new(
        options,
        terminal,
        writer,
        session.clone(),
        settings.clone(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let run_result = event_loop
        .run_app(&mut app)
        .map_err(|err| NativeError::EventLoop(err.to_string()));

    // Tear down deterministically: kill + reap the shell, which closes the PTY
    // master and unblocks the pump thread's `read`, then join the thread. The
    // App's session clone is dropped with `app` after this; reaping the child
    // is what EOFs the pump's reader, independent of master drop order.
    {
        let mut session = session.lock().expect("pty session");
        let _ = session.kill();
        let _ = session.wait();
    }
    let _ = pump_thread.join();

    run_result?;
    if let Some(err) = app.startup_error {
        return Err(err);
    }
    Ok(())
}

fn rgb(color: (u8, u8, u8)) -> RgbColor {
    RgbColor::new(color.0, color.1, color.2)
}
