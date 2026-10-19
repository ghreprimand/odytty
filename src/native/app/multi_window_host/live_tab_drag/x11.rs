// SPDX-License-Identifier: GPL-3.0-only
//! Unmanaged X11 drag preview, converted back to an ordinary client on release.
//! A private protocol connection keeps checked failures away from winit's Xlib
//! error handler. Only the preview's own XID can be configured through it.
use std::error::Error;
use std::time::{Duration, Instant};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;
use x11rb::connection::Connection;
use x11rb::properties::WmHints;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt, PropMode, StackMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(super) struct Follow {
    connection: RustConnection,
    window: u32,
    type_atom: u32,
    normal_atom: u32,
    hints: WmHints,
}

impl Follow {
    /// None on Wayland. Headless App tests never manufacture a connection.
    pub(super) fn start(window: &Window) -> Result<Option<Self>> {
        let id = match window.window_handle()?.as_raw() {
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)?,
            RawWindowHandle::Xcb(handle) => handle.window.get(),
            _ => return Ok(None),
        };
        let (connection, _) = x11rb::connect(None)?;
        if !connection
            .get_window_attributes(id)?
            .reply()?
            .override_redirect
        {
            return Err("X11 drag preview must be unmanaged before mapping".into());
        }
        let type_atom = connection
            .intern_atom(false, b"_NET_WM_WINDOW_TYPE")?
            .reply()?
            .atom;
        let normal_atom = connection
            .intern_atom(false, b"_NET_WM_WINDOW_TYPE_NORMAL")?
            .reply()?
            .atom;
        let mut hints = WmHints::get(&connection, id)?.reply()?.unwrap_or_default();
        // Ordinary winit clients may omit WM_HINTS entirely. Spell out their
        // input-capable policy when this preview becomes a managed client.
        hints.input = Some(hints.input.unwrap_or(true));
        let mut preview_hints = hints;
        preview_hints.input = Some(false);
        preview_hints.set(&connection, id)?.check()?;
        connection.flush()?;
        Ok(Some(Self {
            connection,
            window: id,
            type_atom,
            normal_atom,
            hints,
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

    /// Allow 150 ms for fresh ICCCM NormalState evidence after the MapRequest.
    /// Some Xwayland WMs never adopt a former override-redirect window. A
    /// successful X request alone is not evidence that the client is managed.
    pub(super) fn finish(&self) -> Result<bool> {
        let state = self
            .connection
            .intern_atom(false, b"WM_STATE")?
            .reply()?
            .atom;
        self.connection
            .delete_property(self.window, state)?
            .check()?;
        self.connection.unmap_window(self.window)?.check()?;
        self.connection
            .change_property32(
                PropMode::REPLACE,
                self.window,
                self.type_atom,
                AtomEnum::ATOM,
                &[self.normal_atom],
            )?
            .check()?;
        self.hints.set(&self.connection, self.window)?.check()?;
        self.connection
            .change_window_attributes(
                self.window,
                &ChangeWindowAttributesAux::new().override_redirect(0),
            )?
            .check()?;
        self.connection.map_window(self.window)?.check()?;
        self.connection.flush()?;
        let deadline = Instant::now() + Duration::from_millis(150);
        loop {
            let reply = self
                .connection
                .get_property(false, self.window, state, state, 0, 2)?
                .reply()?;
            if reply.type_ == state
                && reply.bytes_after == 0
                && reply.value32().is_some_and(|mut words| {
                    words.next() == Some(1) && words.next().is_some() && words.next().is_none()
                })
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
