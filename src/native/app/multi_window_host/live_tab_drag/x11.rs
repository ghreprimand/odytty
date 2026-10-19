// SPDX-License-Identifier: GPL-3.0-only
//! Unmanaged X11 drag preview, retired before ordinary window creation on release.
//! A private protocol connection keeps checked failures away from winit's Xlib
//! error handler. Each connection configures only its supplied App window.
use std::error::Error;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;
use x11rb::connection::Connection;
use x11rb::properties::WmHints;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt, StackMode};
use x11rb::rust_connection::RustConnection;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(super) struct Follow {
    connection: RustConnection,
    window: u32,
}

impl Follow {
    /// None on Wayland. Headless App tests never manufacture a connection.
    pub(super) fn start(window: &Window) -> Result<Option<Self>> {
        let Some(id) = window_id(window)? else {
            return Ok(None);
        };
        let (connection, _) = x11rb::connect(None)?;
        if !connection
            .get_window_attributes(id)?
            .reply()?
            .override_redirect
        {
            return Err("X11 drag preview must be unmanaged before mapping".into());
        }
        let mut hints = WmHints::get(&connection, id)?.reply()?.unwrap_or_default();
        hints.input = Some(false);
        hints.set(&connection, id)?.check()?;
        connection.flush()?;
        Ok(Some(Self {
            connection,
            window: id,
        }))
    }

    pub(super) fn place(&self, position: [i32; 2]) -> Result<()> {
        // Core X11 geometry has signed 16-bit coordinates. Clamp before the
        // server narrows them instead of wrapping a distant monitor position.
        let coordinate = |value: i32| value.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.connection
            .configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .x(coordinate(position[0]))
                    .y(coordinate(position[1]))
                    .stack_mode(StackMode::ABOVE),
            )?
            .check()?;
        self.connection.flush()?;
        Ok(())
    }
}

/// Set an ordinary tear-out's input policy explicitly. Its surface was created
/// through the normal winit path; it must never be an unmanaged preview.
pub(in crate::native::app::multi_window_host) fn prepare_ordinary(window: &Window) -> Result<()> {
    let Some(id) = window_id(window)? else {
        return Ok(());
    };
    let (connection, _) = x11rb::connect(None)?;
    if connection
        .get_window_attributes(id)?
        .reply()?
        .override_redirect
    {
        tracing::warn!("ordinary X11 tear-out unexpectedly has override-redirect enabled");
    }
    let mut hints = WmHints::get(&connection, id)?.reply()?.unwrap_or_default();
    hints.input = Some(true);
    hints.set(&connection, id)?.check()?;
    connection.flush()?;
    Ok(())
}

fn window_id(window: &Window) -> Result<Option<u32>> {
    Ok(match window.window_handle()?.as_raw() {
        RawWindowHandle::Xlib(handle) => Some(u32::try_from(handle.window)?),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    })
}
