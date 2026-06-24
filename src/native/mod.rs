// SPDX-License-Identifier: GPL-3.0-only
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
mod attach;
mod bindings;
mod clipboard;
mod connection_overlay;
mod context_menu_ui;
mod copy_mode;
mod cursor;
mod cvd_theme;
mod font_picker;
mod gpu;
mod image_layer;
mod key_remap_ui;
mod layout;
mod onboarding;
mod options;
mod output_recorder;
mod overlay;
mod palette_overlay;
mod pty;
mod render_helpers;
mod replay_overlay;
mod resize;
mod search_ui;
mod session;
mod session_attach_overlay;
mod settings_panel;
mod theme_builder;
mod theme_picker;
mod viewport;

#[cfg(test)]
mod gpu_tests;
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

pub use options::{NativeCommand, NativeError, NativeOptions};
pub(crate) use viewport::WindowPadding;

use app::App;
use pty::{PtyWriter, UserEvent, spawn_pty_pump};
use session::{Session, SessionToken, TabSet};

pub fn run_native(options: NativeOptions, settings: Settings) -> Result<(), NativeError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| NativeError::EventLoop(err.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // Select the presentation theme once via Settings and apply its default cell
    // colors process-wide before any rendering. This only
    // affects how Color::Default paints; the terminal core is unaware of it.
    // U4: publish the *effective* theme so a launch-time `cvd_mode` is honored
    // from the first frame. Off (the default) returns the authored theme
    // unchanged, so the plain path is byte-identical.
    let theme =
        cvd_theme::effective_theme(&settings.theme, settings.cvd_mode, settings.cvd_strength);
    text::set_default_colors(theme.foreground, theme.background);
    text::set_ansi_palette(&theme.palette);
    // Publish the RV1 minimum-contrast floor process-wide before the first
    // frame so a launch-time `ODYTTY_MIN_CONTRAST` is honored immediately; the
    // grid resolve seam reads it per cell. The reload path republishes it on
    // change. Passthrough at the default 1.0, so the plain path is unchanged.
    text::set_min_contrast(settings.effective_min_contrast());
    // Publish the synthetic-styles kill switch process-wide before the GPU
    // surface (and its glyph atlas) is built on resume, so a launch-time
    // `synthetic_styles = off` is honored from the first frame. The config
    // reload path republishes it on change (see `apply_reloadable_values`).
    crate::settings::set_synthetic_styles_enabled(settings.synthetic_styles);
    // Publish the RV2 geometric box-drawing switch before the atlas is built;
    // the reload path republishes it and the renderer rebuilds when it flips.
    crate::settings::set_geometric_boxdraw_enabled(settings.geometric_boxdraw);
    // Publish the RV6 symbol fallback knobs before the atlas is built; the
    // reload path republishes them and the renderer re-resolves/rebuilds when
    // either value changes.
    crate::settings::set_symbol_fallback_enabled(settings.symbol_fallback);
    crate::settings::set_symbol_font_path(settings.symbol_font.clone());
    // Publish the SYMMAP override map before the atlas is built; the reload path
    // republishes it and the renderer re-resolves/rebuilds when the map changes.
    crate::settings::set_symbol_map(settings.symbol_map.clone());
    // Shared terminal model, sized to the initial grid. The pump thread writes
    // to it; the UI thread snapshots from it.
    let mut model = Terminal::new(options.initial_grid.columns, options.initial_grid.rows);
    model.set_base_colors(
        rgb(theme.foreground),
        rgb(theme.background),
        rgb(if settings.themed_ui_roles {
            theme.cursor
        } else {
            theme.foreground
        }),
    );
    model.set_osc52_read_enabled(settings.osc52_read);
    // Bound scrollback memory from the start so the very first session is capped
    // before any output streams in (`0` = unbounded). See SCROLLBACK-CAP.
    model.set_scrollback_limit(settings.scrollback_limit());
    // Apply the host default cursor shape/blink policy from settings before any
    // output. An application's DECSCUSR can still override this at runtime; RIS/
    // DECSTR return to it. Presentation policy only — the grid contents are
    // unaffected.
    model.set_cursor_defaults(settings.cursor_style, settings.cursor_blink.enabled());
    let terminal = Arc::new(Mutex::new(model));

    // Spawn the shell PTY and start pumping its output into the shared terminal.
    let session = if let Some(command) = &options.command {
        PtySession::spawn_exec(
            options.initial_grid,
            command.program.clone(),
            command.args.clone(),
            options.working_directory.clone(),
        )
    } else {
        PtySession::spawn_default_shell_in(options.initial_grid, options.working_directory.clone())
    }
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
    // One recorder handle shared between the initial session's pump thread and
    // the session itself, so recorded frames (when `session_replay` is on) land
    // in the ring the App later scrubs. Disabled by default ⇒ no overhead.
    let recorder = output_recorder::RecorderHandle::new();
    let pump_thread = spawn_pty_pump(
        reader,
        writer.clone(),
        terminal.clone(),
        proxy.clone(),
        SessionToken(0),
        recorder.clone(),
    );

    // Share the session: the App pushes window-size changes to it on resize,
    // and this function reaps the child on the way out.
    let session = Arc::new(Mutex::new(session));
    let session_set = TabSet::new(
        Session::new_local_with_recorder(
            SessionToken(0),
            terminal,
            writer,
            session.clone(),
            Some(pump_thread),
            recorder,
        ),
        Some(proxy),
    );

    // Phase 2 startup attach (opt-in; `None` leaves the launch byte-identical):
    // the window opened its normal initial local session above, and now also
    // attaches the requested detached session as a live tab and focuses it. The
    // initial local session is untouched, so the default path is unchanged.
    let attach_session = options.attach_session.clone();
    let mut app = App::new_with_sessions(
        options,
        session_set,
        settings.clone(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    if let Some(session_id) = attach_session
        && let Err(err) = app.attach_session_in_new_tab(None, &session_id)
    {
        eprintln!("odytty: attach session {session_id} failed: {err}");
    }
    let run_result = event_loop
        .run_app(&mut app)
        .map_err(|err| NativeError::EventLoop(err.to_string()));

    // Tear down deterministically: kill + reap the shell, which closes the PTY
    // master and unblocks the pump thread's `read`, then join the thread. The
    // App's session clone is dropped with `app` after this; reaping the child
    // is what EOFs the pump's reader, independent of master drop order.
    app.close_all_sessions();

    run_result?;
    if let Some(err) = app.startup_error {
        return Err(err);
    }
    Ok(())
}

fn rgb(color: (u8, u8, u8)) -> RgbColor {
    RgbColor::new(color.0, color.1, color.2)
}
