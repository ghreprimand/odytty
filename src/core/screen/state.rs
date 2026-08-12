// SPDX-License-Identifier: GPL-3.0-only
//! Screen construction, live configuration, dynamic color, and clipboard state.

use super::*;

impl Screen {
    pub fn new(columns: usize, rows: usize) -> Self {
        let dimensions = Dimensions::new(columns, rows);
        Self {
            dimensions,
            rows: vec![blank_row(dimensions.columns); dimensions.rows],
            scrollback: Scrollback::new(),
            cursor: Position::default(),
            cursor_visible: true,
            pending_wrap: false,
            output_since_last_resize: false,
            shell_owns_cursor_on_resize: false,
            saved_cursor: None,
            primary_screen: None,
            scroll_region: None,
            origin_mode: false,
            auto_wrap: true,
            insert_mode: false,
            bracketed_paste: false,
            alternate_scroll: true,
            current_attrs: Attrs::default(),
            current_protected: false,
            rect_attr_extent: RectAttributeExtent::default(),
            active_hyperlink: None,
            dirty: DirtyRegion::Full,
            render_revision: 0,
            host_output: Vec::new(),
            clipboard_requests: Vec::new(),
            osc52_read_enabled: false,
            kitty_named_transports_enabled: false,
            bell_pending: false,
            base_colors: DynamicColors::default(),
            base_palette: std::array::from_fn(|i| indexed_srgb(i as u8)),
            dynamic_colors: DynamicColors::default(),
            last_graphic_char: None,
            tab_stops: default_tab_stops(dimensions.columns),
            title: None,
            title_changed: false,
            working_directory: None,
            local_hostname: None,
            working_directory_changed: false,
            prompt_marks_changed: false,
            mouse: MouseProtocol::default(),
            keyboard: KeyboardModes::default(),
            charsets: CharsetModes::default(),
            kitty_keyboard_stack: Vec::new(),
            focus_reporting: false,
            click_events_enabled: false,
            active_prompt_input_start: None,
            active_edit_region: None,
            active_prompt_start: None,
            cursor_style: CursorStyle::default(),
            cursor_blink: true,
            default_cursor_style: CursorStyle::default(),
            default_cursor_blink: true,
            graphics: ImageScene::default(),
            hyperlinks: HyperlinkTable::default(),
            buttons: ButtonTable::default(),
            buttons_enabled: false,
            buttons_iterm_compat: true,
            buttons_sticky: true,
            active_button_run: None,
            dcs_capture: None,
            dcs_query: None,
            graphics_stats: GraphicsStats::default(),
            cell_metrics: CellMetrics::default(),
            sixel_display_mode: false,
            synchronized_output: false,
        }
    }

    /// Set the host default cursor shape and blink policy (from settings).
    ///
    /// Establishes the values DECSCUSR 0 / RIS / DECSTR reset to, and applies
    /// them immediately as the current effective cursor since no application has
    /// issued DECSCUSR yet. Intended to be called once at startup before output.
    pub fn set_cursor_defaults(&mut self, style: CursorStyle, blink: bool) {
        let changed = self.default_cursor_style != style
            || self.default_cursor_blink != blink
            || self.cursor_style != style
            || self.cursor_blink != blink;
        self.default_cursor_style = style;
        self.default_cursor_blink = blink;
        self.cursor_style = style;
        self.cursor_blink = blink;
        if changed {
            self.mark_dirty();
        }
    }

    /// Set the live cell pixel metrics used by the graphics routing layer
    /// (Sixel/Kitty extent calculation). The native layer calls this at startup
    /// and on every rescale/font-size rebuild so new placements use the real
    /// glyph cell size. Dimensions are clamped to `[1, 1024]` — zero is never
    /// stored.
    ///
    /// Existing placements are **not** recomputed — they retain the extent
    /// calculated at creation time. Only placements created after this call
    /// use the updated metrics (new-placements-only policy, documented).
    pub fn set_cell_metrics(&mut self, width_px: u32, height_px: u32) {
        self.cell_metrics = CellMetrics::new(width_px, height_px);
    }

    /// Set whether the backend's shell owns cursor placement on resize (it
    /// repaints with absolute positioning, e.g. ConPTY/conhost). When true, the
    /// resize reflow keeps the incoming cursor clamped to the new dims and lets
    /// the shell's repaint own placement, instead of translating the cursor (the
    /// `preserve_cursor_physical_line` override + content map). Wired once from
    /// the PTY backend capability when the native layer creates a local session.
    pub fn set_shell_owns_cursor_on_resize(&mut self, value: bool) {
        self.shell_owns_cursor_on_resize = value;
    }

    /// Whether the backend's shell owns cursor placement on resize.
    /// See [`Self::set_shell_owns_cursor_on_resize`]. Test-only: production code
    /// only ever sets this (from the backend capability); the getter exists to
    /// tie the flag to real resize behavior in the cross-platform guard tests.
    #[cfg(test)]
    pub(crate) fn shell_owns_cursor_on_resize(&self) -> bool {
        self.shell_owns_cursor_on_resize
    }

    /// Current cell pixel metrics. See [`Self::set_cell_metrics`].
    pub fn cell_metrics(&self) -> CellMetrics {
        self.cell_metrics
    }

    /// Monotonic counter for visible terminal-state changes.
    ///
    /// The native renderer uses this as an additive invalidation seam: every
    /// path that calls [`Self::mark_dirty`] changes the text/graphics/cursor
    /// pixels that a future snapshot can produce. Title-only and host-response
    /// changes deliberately do not bump it because they do not affect the cell
    /// framebuffer.
    pub fn render_revision(&self) -> u64 {
        self.render_revision
    }

    pub fn dynamic_colors(&self) -> &DynamicColors {
        &self.dynamic_colors
    }

    pub fn set_base_colors(
        &mut self,
        foreground: RgbColor,
        background: RgbColor,
        cursor: RgbColor,
    ) {
        self.base_colors.foreground = foreground;
        self.base_colors.background = background;
        self.base_colors.cursor = cursor;
        self.dynamic_colors.foreground = foreground;
        self.dynamic_colors.background = background;
        self.dynamic_colors.cursor = cursor;
        self.mark_dirty();
    }

    /// C29: seed the base 16 ANSI palette from the active theme so OSC 4
    /// queries report the colors actually rendered. Sibling of
    /// [`Self::set_base_colors`]; called on startup and on every theme change.
    pub fn set_base_palette(&mut self, palette: [RgbColor; 16]) {
        self.base_palette = palette;
    }

    pub fn set_osc52_read_enabled(&mut self, enabled: bool) {
        self.osc52_read_enabled = enabled;
    }

    /// Permit Kitty graphics transports that name host files or POSIX shared
    /// memory. Disabled by default; direct and chunked-inline data are
    /// unaffected.
    pub fn set_kitty_named_transports_enabled(&mut self, enabled: bool) {
        self.kitty_named_transports_enabled = enabled;
    }

    /// Set the scrollback retention cap in logical lines (`0` = unbounded),
    /// trimming any excess immediately. Applies to the active buffer and, when a
    /// TUI is on the alternate screen, the stored primary scrollback as well, so
    /// a live config reload during a full-screen app takes effect on return.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.scrollback.set_limit(limit);
        if let Some(stored) = self.primary_screen.as_mut() {
            stored.scrollback.set_limit(limit);
        }
    }

    pub fn take_clipboard_requests(&mut self) -> Vec<ClipboardRequest> {
        std::mem::take(&mut self.clipboard_requests)
    }

    pub fn answer_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        self.host_output.extend_from_slice(b"\x1b]52;");
        self.host_output
            .extend_from_slice(osc52_selection_bytes(selection));
        self.host_output.push(b';');
        self.host_output
            .extend_from_slice(encode_base64_bytes(text.as_bytes()).as_bytes());
        self.host_output.extend_from_slice(b"\x1b\\");
    }

    /// DECSDM (private mode 80): when `true`, sixel images anchor at the cursor
    /// and the cursor does NOT move after display. When `false` (default),
    /// the cursor moves below the image.
    pub fn sixel_display_mode(&self) -> bool {
        self.sixel_display_mode
    }

    /// DECSET 2026 synchronized-output mode. The core owns the mode state and
    /// DECRQM/RIS/DECSTR behavior; native presentation owns the safety timeout
    /// so a crashed app cannot freeze the visible frame.
    pub fn synchronized_output_enabled(&self) -> bool {
        self.synchronized_output
    }

    /// The cursor shape currently in effect (DECSCUSR or host default).
    pub fn cursor_style(&self) -> CursorStyle {
        self.cursor_style
    }

    /// Whether the cursor's blink policy is currently enabled.
    pub fn cursor_blinking(&self) -> bool {
        self.cursor_blink
    }

    /// DECSCUSR (`CSI Ps SP q`): set the cursor shape and blink.
    ///
    /// Per the VT520/xterm convention: `0` resets to the host default policy,
    /// odd values blink and even values are steady, with `1/2` = block,
    /// `3/4` = underline, `5/6` = bar. Unknown values are ignored.
    pub(super) fn set_cursor_style(&mut self, ps: usize) {
        let (style, blink) = match ps {
            0 => (self.default_cursor_style, self.default_cursor_blink),
            1 => (CursorStyle::Block, true),
            2 => (CursorStyle::Block, false),
            3 => (CursorStyle::Underline, true),
            4 => (CursorStyle::Underline, false),
            5 => (CursorStyle::Bar, true),
            6 => (CursorStyle::Bar, false),
            _ => return,
        };
        self.cursor_style = style;
        self.cursor_blink = blink;
        self.mark_dirty();
    }

    pub(super) fn osc_default_color(&mut self, params: &[&[u8]], slot: DefaultColorSlot) {
        let Some(&value) = params.get(1) else {
            return;
        };
        if value == b"?" {
            self.host_output.extend_from_slice(b"\x1b]");
            self.host_output
                .extend_from_slice(default_color_osc_code(slot));
            self.host_output.push(b';');
            self.host_output
                .extend_from_slice(format_xterm_rgb(self.default_color(slot)).as_bytes());
            self.host_output.extend_from_slice(b"\x1b\\");
            return;
        }
        let Some(color) = parse_xterm_rgb(value) else {
            return;
        };
        match slot {
            DefaultColorSlot::Foreground => self.dynamic_colors.foreground = color,
            DefaultColorSlot::Background => self.dynamic_colors.background = color,
            DefaultColorSlot::Cursor => self.dynamic_colors.cursor = color,
        }
        self.mark_dirty();
    }

    pub(super) fn reset_default_color(&mut self, slot: DefaultColorSlot) {
        match slot {
            DefaultColorSlot::Foreground => {
                self.dynamic_colors.foreground = self.base_colors.foreground;
            }
            DefaultColorSlot::Background => {
                self.dynamic_colors.background = self.base_colors.background;
            }
            DefaultColorSlot::Cursor => {
                self.dynamic_colors.cursor = self.base_colors.cursor;
            }
        }
        self.mark_dirty();
    }

    /// The 16 ANSI colors this screen is **actually displaying**: a live `OSC 4`
    /// override where one exists, otherwise the theme-seeded base palette
    /// ([`Self::set_base_palette`]). Same precedence the OSC 4 query path
    /// reports and the renderer resolves against, exposed as a whole array for
    /// callers that need the effective palette rather than one slot — notably
    /// theme capture, which must reproduce the screen, not the theme.
    ///
    /// Only indices 0..16 are theme-relevant; 16..=255 stay on the xterm cube
    /// and are not part of a theme.
    pub fn effective_ansi_palette(&self) -> [RgbColor; 16] {
        std::array::from_fn(|index| self.palette_color(index as u8))
    }

    pub(super) fn default_color(&self, slot: DefaultColorSlot) -> RgbColor {
        match slot {
            DefaultColorSlot::Foreground => self.dynamic_colors.foreground,
            DefaultColorSlot::Background => self.dynamic_colors.background,
            DefaultColorSlot::Cursor => self.dynamic_colors.cursor,
        }
    }

    pub(super) fn osc_palette(&mut self, params: &[&[u8]]) {
        for pair in params[1..].chunks(2) {
            let [index, value] = pair else {
                return;
            };
            let Ok(index) = std::str::from_utf8(index).unwrap_or("").parse::<u16>() else {
                continue;
            };
            if index > 255 {
                continue;
            }
            let index = index as u8;
            if *value == b"?" {
                self.host_output.extend_from_slice(b"\x1b]4;");
                self.host_output
                    .extend_from_slice(index.to_string().as_bytes());
                self.host_output.push(b';');
                self.host_output
                    .extend_from_slice(format_xterm_rgb(self.palette_color(index)).as_bytes());
                self.host_output.extend_from_slice(b"\x1b\\");
            } else if let Some(color) = parse_xterm_rgb(value) {
                self.dynamic_colors.palette[index as usize] = Some(color);
                self.mark_dirty();
            }
        }
    }

    pub(super) fn osc_reset_palette(&mut self, params: &[&[u8]]) {
        if params.len() == 1 {
            if self.dynamic_colors.palette.iter().any(Option::is_some) {
                self.dynamic_colors.palette = [None; 256];
                self.mark_dirty();
            }
            return;
        }

        let mut changed = false;
        for raw in &params[1..] {
            let Ok(index) = std::str::from_utf8(raw).unwrap_or("").parse::<u16>() else {
                continue;
            };
            if index <= 255 {
                changed |= self.dynamic_colors.palette[index as usize].take().is_some();
            }
        }
        if changed {
            self.mark_dirty();
        }
    }

    /// Effective palette color for OSC 4 replies: a live OSC 4 override wins,
    /// then the theme's base 16 (C29), then the xterm table for 16..=255.
    pub(super) fn palette_color(&self, index: u8) -> RgbColor {
        self.dynamic_colors.palette[index as usize].unwrap_or_else(|| {
            self.base_palette
                .get(index as usize)
                .copied()
                .unwrap_or_else(|| indexed_srgb(index))
        })
    }

    pub(super) fn osc_clipboard(&mut self, params: &[&[u8]]) {
        let selectors = params
            .get(1)
            .copied()
            .and_then(osc52_selections)
            .unwrap_or_else(|| vec![ClipboardSelection::Clipboard]);
        let Some(&payload) = params.get(2) else {
            return;
        };

        if payload == b"?" {
            if self.osc52_read_enabled {
                self.clipboard_requests.extend(
                    selectors
                        .into_iter()
                        .map(|selection| ClipboardRequest::Read { selection }),
                );
            }
            return;
        }

        let Some(decoded) = decode_base64_bytes(payload, OSC52_CLIPBOARD_MAX_BYTES) else {
            return;
        };
        let Ok(text) = String::from_utf8(decoded) else {
            return;
        };
        self.clipboard_requests
            .extend(
                selectors
                    .into_iter()
                    .map(|selection| ClipboardRequest::Write {
                        selection,
                        text: text.clone(),
                    }),
            );
    }
}
