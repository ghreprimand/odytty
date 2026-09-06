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
//! This is a full terminal, not a prototype: PTY output is rendered live and
//! keyboard input is encoded and written back to the PTY (via the shared
//! [`crate::input`] encoder), and the layer now carries the shipped feature set
//! -- panes/splits, workspaces, session attach/detach and the unified Session
//! Navigator, the connection manager, profile manager, theme builder, replay,
//! and the Odyssey visual treatments (bloom/CRT post-processing in `gpu/post.rs`,
//! cursor trail/streak, new-row fade, scroll and graphics animation). The
//! per-submodule doc comments below are the authoritative description of each
//! piece.
//!
//! ## Ownership split
//!
//! The native app keeps the owned terminal core (`crate::core`) separate from
//! windowing, GPU rendering, and the Odyssey visual layer:
//!
//! - **Event loop** — `winit` owns the OS window, input events, and resize.
//! - **GPU surface/device** — `wgpu` owns the surface, device, queue, and swap
//!   chain, presenting frames (including the post-processing passes) to the
//!   window.
//! - **Glyph atlas / text renderer** — a CPU-rasterized monospace glyph atlas
//!   (`crate::text`) uploaded to a `wgpu` texture; cells drawn as textured quads.
//! - **Grid presentation** — maps an owned-core `Snapshot` to positioned cell
//!   quads via `crate::grid`, with no terminal semantics in the renderer.
//!
//! ## Linux / Wayland
//!
//! `winit` compiles in both Wayland and X11 backends and selects Wayland at
//! runtime when `WAYLAND_DISPLAY` is set, so under Hyprland this is a native
//! Wayland surface (no XWayland). `wgpu` presents to that surface via its
//! default backends (Vulkan on Linux), so the GPU path is Wayland-native too.

mod about;
mod app;
// The attach client (Unix-domain socket transport to a detached session-host)
// is Unix-only; the attach overlay UI stays cross-platform with an empty list.
#[cfg(unix)]
mod attach;
mod bindings;
mod clipboard;
mod command_export;
mod connection_form;
mod connection_overlay;
mod context_menu_ui;
mod copy_mode;
mod cursor;
mod cvd_theme;
mod font_picker;
mod gpu;
mod image_decode;
mod image_layer;
mod instance_lock;
mod key_event_diagnostics;
mod key_remap_ui;
mod layout;
#[cfg(target_os = "macos")]
mod macos_open_with;
mod notifications;
mod onboarding;
mod open_with_overlay;
mod options;
mod output_recorder;
mod overlay;
mod palette_overlay;
mod panic_log;
mod paste_policy;
mod persistence;
mod profile_manager;
mod profile_picker;
mod pty;
mod pty_writer;
mod render_helpers;
mod replay_overlay;
mod resize;
mod save_dialog;
mod search_ui;
mod session;
mod session_attach_overlay;
mod session_navigator;
mod settings_panel;
mod shell_discovery;
mod texture_limits;
mod theme_builder;
mod theme_picker;
mod viewport;
mod watchdog;
mod window_icon;
mod workspace_picker;

#[cfg(test)]
mod gpu_tests;
#[cfg(test)]
mod image_layer_tests;
#[cfg(test)]
pub(in crate::native) mod test_support;
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
use session::{
    Session, SessionToken, WorkspaceSet, apply_local_backend_caps, seed_initial_working_directory,
};

pub fn run_native(options: NativeOptions, settings: Settings) -> Result<(), NativeError> {
    panic_log::install_panic_hook();

    let (settings, startup_plan, startup_warnings) =
        app::profile_launch::resolve_startup_launch(&options, settings);
    for warning in startup_warnings {
        tracing::warn!(warning = %warning, "profile launch notice");
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| NativeError::EventLoop(err.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    // WP2 sub-ODP 8d: elect a single primary instance. The lock is held for the
    // whole process lifetime (this binding is never dropped until `run_native`
    // returns). Only the primary autosaves and restores the workspace shape; a
    // second concurrent window runs with `is_primary == false` and stays inert
    // on both, so two windows never race on `workspaces.json`.
    let instance_lock = instance_lock::PrimaryInstanceLock::acquire();
    let is_primary = instance_lock.is_some();

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
    // Publish contextual shaping before the GPU renderer is created.
    crate::settings::set_ligatures_enabled(settings.ligatures);
    crate::settings::set_ligature_ss01_enabled(settings.ligature_ss01);
    crate::settings::set_ligature_ss02_enabled(settings.ligature_ss02);
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
    let local_hostname = crate::local_hostname::get();
    let mut model = Terminal::new(options.initial_grid.columns, options.initial_grid.rows);
    model.set_local_hostname(local_hostname.clone());
    seed_initial_working_directory(&mut model, options.working_directory.as_deref());
    seed_launch_session_model(&mut model, &settings);

    // Spawn the shell PTY before wrapping the model so the backend capabilities
    // can be applied to the model first. The spawn has no dependency on
    // `terminal`, so this ordering is safe; the reader/writer/pump wiring stays
    // below as before.
    let session = if let Some(command) = &options.command {
        PtySession::spawn_exec(
            options.initial_grid,
            command.program.clone(),
            command.args.clone(),
            options.working_directory.clone(),
        )
    } else if let Some(plan) = startup_plan.as_ref() {
        crate::profiles::spawn_local_plan(options.initial_grid, plan)
    } else {
        PtySession::spawn_default_shell_in_with_settings(
            options.initial_grid,
            options.working_directory.clone(),
            &settings,
        )
    }
    .map_err(|err| NativeError::Pty(err.to_string()))?;
    // Defer resize cursor placement to the shell when the backend repaints
    // absolutely (ConPTY on Windows). Funneled through the same helper as the
    // split/new-tab path so the startup pane can't drift out of lockstep.
    apply_local_backend_caps(&mut model, &session);
    let terminal = Arc::new(Mutex::new(model));

    // Start pumping the shell PTY output into the shared terminal.
    let reader = session
        .try_clone_reader()
        .map_err(|err| NativeError::Pty(err.to_string()))?;
    // One writer, shared: the pump thread sends host responses through it, and
    // the App sends encoded keystrokes through its clone.
    let writer: PtyWriter = Arc::new(Mutex::new(
        pty_writer::writer_shim(
            session
                .take_writer()
                .map_err(|err| NativeError::Pty(err.to_string()))?,
            SessionToken(0),
        )
        .map_err(|err| NativeError::Pty(err.to_string()))?,
    ));

    // Windows only: a child that dies during its own initialization makes the
    // spawn return `Ok` yet would leave the pane blank (the failure is a
    // post-spawn loader/init exit, not a `CreateProcessW` error). The ConPTY
    // backend's child-waiter thread detects that abnormal-and-immediate exit,
    // records a diagnostic into this slot (and stderr), and closes the
    // pseudoconsole; the pump writes the slot into the pane on the resulting EOF.
    // This replaces the former synchronous 250 ms spawn-path wait, so a healthy
    // shell pays no startup tax. `None` on Unix (byte-identical).
    let diagnostic = session.pending_diagnostic_slot();

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
        diagnostic,
    )
    .map_err(|err| NativeError::Pty(err.to_string()))?;

    // Share the session: the App pushes window-size changes to it on resize,
    // and this function reaps the child on the way out.
    let session = Arc::new(Mutex::new(session));
    let mut session_set = WorkspaceSet::new(
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
    session_set.set_local_hostname(local_hostname);

    // Phase 2 startup attach (opt-in; `None` leaves the launch byte-identical):
    // the window opened its normal initial local session above, and now also
    // attaches the requested detached session as a live tab and focuses it. The
    // initial local session is untouched, so the default path is unchanged.
    let attach_session = options.attach_session.clone();
    let bare_launch = options.bare_launch;
    let mut app = App::new_with_sessions(
        options,
        session_set,
        settings.clone(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    if let Some(session_id) = attach_session
        && let Err(err) = app.attach_session_in_new_tab(None, &session_id)
    {
        tracing::error!("attach session {session_id} failed: {err}");
    }
    // WP2: gate autosave/restore on primary-instance status, then restore the
    // saved workspace shape only for a bare `odytty` launch with the setting on
    // (sub-ODPs 8a/8b). Any CLI argument leaves `bare_launch` false and starts
    // fresh; a secondary instance never restores.
    app.set_primary_instance(is_primary);
    if is_primary {
        // C27: clear crash-orphaned atomic-write temporaries from the state and
        // layouts dirs before autosave begins (only the primary writes them).
        crate::native::persistence::sweep_stale_temp_files();
    }
    if is_primary && bare_launch && settings.restore_workspaces {
        app.restore_workspaces_on_launch();
    }
    // SECONDARY-INSTANCE-NOTICE: a second concurrent window is silently inert on
    // restore/autosave (a live primary holds the instance lock). Surface that
    // once at startup when the user expects restore, so relaunching over a
    // still-running or wedged first window no longer reads as "restore failed".
    app.notice_secondary_instance_if_suppressed();
    // FREEZE-HARDEN (b): run the app under the freeze watchdog — a thin
    // ApplicationHandler wrapper noting input/redraw activity and mirroring a
    // state snapshot, plus a detached monitor thread that logs the state
    // machine when work stays pending >10s with no presented frame. The
    // monitor holds only a weak reference, so it winds down with the loop.
    let watchdog_shared = watchdog::WatchdogShared::new();
    watchdog::spawn_monitor(&watchdog_shared);
    let mut watched = watchdog::WatchdogApp::new(app, watchdog_shared);
    let run_result = event_loop
        .run_app(&mut watched)
        .map_err(|err| NativeError::EventLoop(err.to_string()));
    let mut app = watched.into_inner();

    // WP2 sub-ODP 8c: unconditional shape save on a clean exit (primary only,
    // self-guarded). Runs while the sessions are still live so per-pane cwds are
    // captured, and only when the loop exited cleanly so a startup failure never
    // clobbers a good snapshot.
    if run_result.is_ok() && app.startup_error.is_none() {
        app.save_shape_on_exit();
    }

    // Tear down deterministically: kill + reap the shell, which closes the PTY
    // master and unblocks the pump thread's `read`, then join the thread. The
    // App's session clone is dropped with `app` after this; reaping the child
    // is what EOFs the pump's reader, independent of master drop order.
    app.close_all_sessions();
    drop(instance_lock);

    run_result?;
    if let Some(err) = app.startup_error {
        return Err(err);
    }
    Ok(())
}

fn rgb(color: (u8, u8, u8)) -> RgbColor {
    RgbColor::new(color.0, color.1, color.2)
}

/// Seed a freshly constructed launch-session `Terminal` with every
/// settings-derived per-session default: base colors and palette (from the
/// effective theme, CVD-adjusted), OSC 52 read policy, kitty named transports,
/// the scrollback cap, host cursor defaults, and the button-protocol gates.
///
/// This is the ONE seeding path for the first session in a window. New tabs,
/// panes, and attaches receive the same defaults through
/// `initialize_session_with`; the launch session is built ahead of that path,
/// and the headless test harness (`test_support::headless_app_with_writer`)
/// builds its terminal through this same function — so a per-session default
/// added to the launch path is automatically exercised by every headless test,
/// and the two construction paths cannot drift. The button gate previously
/// bypassed this coupling and shipped a launch-pane-only regression; keep any
/// future per-session gate inside this helper.
///
/// The effective theme is derived here (not passed in) so both callers resolve
/// it identically; at the default CVD settings the authored theme is returned
/// unchanged, so this stays byte-identical with the previous inline seeding.
pub(in crate::native) fn seed_launch_session_model(model: &mut Terminal, settings: &Settings) {
    let theme =
        cvd_theme::effective_theme(&settings.theme, settings.cvd_mode, settings.cvd_strength);
    model.set_base_colors(
        rgb(theme.foreground),
        rgb(theme.background),
        rgb(if settings.themed_ui_roles {
            theme.cursor
        } else {
            theme.foreground
        }),
    );
    // C29: seed the base 16 ANSI palette so OSC 4 queries report the theme's
    // colors rather than the hardcoded xterm table.
    model.set_base_palette(theme.palette.map(rgb));
    model.set_osc52_read_enabled(settings.osc52_read);
    model.set_kitty_named_transports_enabled(settings.kitty_named_transports);
    // Bound scrollback memory from the start so the very first session is capped
    // before any output streams in (`0` = unbounded). See SCROLLBACK-CAP.
    model.set_scrollback_limit(settings.scrollback_limit());
    // Apply the host default cursor shape/blink policy from settings before any
    // output. An application's DECSCUSR can still override this at runtime; RIS/
    // DECSTR return to it. Presentation policy only — the grid contents are
    // unaffected.
    model.set_cursor_defaults(settings.cursor_style, settings.cursor_blink.enabled());
    // Button protocol gates (docs/buttons.md). Turning the default-on master
    // off restores the byte-identical plain path. Sub-gates ride the same
    // master.
    model.set_buttons_enabled(settings.buttons);
    model.set_buttons_iterm_compat(settings.buttons_iterm_compat);
    model.set_buttons_sticky(settings.buttons_sticky);
}

/// Lock a `Mutex`, recovering the guard if a previous holder panicked while
/// holding it (P0-3 defense-in-depth).
///
/// The shared terminal model's invariants survive a poisoned panic: the panics
/// this guards against live in scanner / paint / title code that reads the grid
/// or appends bytes without leaving it half-mutated, so taking the inner guard
/// is safe. Recovering keeps the event loop alive instead of converting the next
/// mouse-move / paint / OSC-title event into a second abort that unwinds across
/// the AppKit→Rust FFI boundary. **Byte-identical on the happy path** — the
/// recovery closure runs only when the lock is already poisoned.
pub(crate) fn lock_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
