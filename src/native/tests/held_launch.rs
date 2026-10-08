// SPDX-License-Identifier: GPL-3.0-only
//! Held first launch: a window's placeholder-sized shells start only after
//! the first surface-derived grid reaches their backends (see
//! `crate::pty::spawn_held`).

use super::*;

/// The window's launches are released by the first real surface grid, and
/// only by it: a minimized (0x0) surface leaves them held.
#[test]
fn launches_settle_at_the_first_real_surface_grid() {
    let (mut app, _terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        crate::settings::Settings::default(),
    );
    assert!(
        !app.workspace_set().launch_geometry_settled(),
        "a new window's launches wait for its real grid"
    );

    app.apply_grid_resize_for_test(PendingResize {
        cell: cell(8, 16),
        padding: WindowPadding::ZERO,
        width_px: 0,
        height_px: 0,
    });
    assert!(
        !app.workspace_set().launch_geometry_settled(),
        "a minimized surface is not a real grid"
    );

    app.apply_grid_resize_for_test(PendingResize {
        cell: cell(8, 16),
        padding: WindowPadding::ZERO,
        width_px: 1096,
        height_px: 656,
    });
    assert!(app.workspace_set().launch_geometry_settled());
}

/// A 0x0 surface must not reflow the live grid down to the 1x1 clamp.
/// `recompute_grid_for_tab_bar` applies the same width/height check, but a
/// headless app has no window, so that function returns before reading a size.
/// This test drives the resize path that unit tests can reach.
#[test]
fn zero_size_resize_keeps_grid_dimensions() {
    let (mut app, _terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        crate::settings::Settings::default(),
    );
    let before = app.tab_dimensions_at_position_for_test(0);
    assert_eq!(before, Some(Dimensions::new(80, 24)));
    app.apply_grid_resize_for_test(PendingResize {
        cell: cell(8, 16),
        padding: WindowPadding::ZERO,
        width_px: 0,
        height_px: 0,
    });
    assert_eq!(
        app.tab_dimensions_at_position_for_test(0),
        before,
        "a 0x0 resize must leave the grid alone"
    );
}

/// End to end through the production grid path on ConPTY: a held child does
/// not run at the 80x24 placeholder, and after the first surface grid it
/// observes the real size. `mode con` prints the console's column count at
/// its own startup; without the hold it runs at once and reports 80.
#[cfg(windows)]
#[test]
fn a_held_windows_child_first_sees_the_surface_grid() {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let placeholder = Dimensions::new(80, 24);
    let session = crate::pty::spawn_held(|| {
        crate::pty::PtySession::spawn_exec(
            placeholder,
            "cmd.exe".into(),
            vec!["/d".into(), "/c".into(), "mode con".into()],
            None,
        )
    })
    .expect("spawn held cmd.exe");
    let mut reader = session.try_clone_reader().expect("reader");
    let output = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&output);
    let pump = std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            sink.lock()
                .expect("output")
                .extend_from_slice(&buffer[..read]);
        }
    });
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().expect("writer")));
    let terminal = Arc::new(Mutex::new(Terminal::new(
        placeholder.columns,
        placeholder.rows,
    )));
    let pty = Arc::new(Mutex::new(session));
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty.clone(),
        crate::settings::Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );

    // Long enough for an unheld `cmd /c mode con` to have printed its status.
    std::thread::sleep(Duration::from_millis(750));
    assert!(pty.lock().expect("pty").start_is_held(), "still held");

    // 1096x656 px at 8x16 cells is a 137x41 grid.
    assert!(app.resize_grid(cell(8, 16), 1096, 656));
    assert!(!pty.lock().expect("pty").start_is_held(), "released");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = String::from_utf8_lossy(&output.lock().expect("output")).into_owned();
        // `mode con` reports once, at its own startup, so this width can
        // only appear if the child first ran at the surface grid.
        if text.contains("137") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the child never reported the surface width: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(app);
    if let Ok(mut pty) = pty.lock() {
        let _ = pty.kill();
    }
    let _ = pump.join();
}
