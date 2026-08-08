// SPDX-License-Identifier: GPL-3.0-only
//! Parser-owned terminal facade and public delegation surface.

use super::*;

pub struct Terminal {
    parser: OdyParser,
    pub(super) screen: Screen,
}
impl Terminal {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            parser: OdyParser::new(),
            screen: Screen::new(columns, rows),
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }

    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.screen.resize(columns, rows);
    }

    pub fn take_host_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.screen.host_output)
    }

    pub fn take_clipboard_requests(&mut self) -> Vec<ClipboardRequest> {
        self.screen.take_clipboard_requests()
    }

    /// Drain the BEL latch (see [`Screen::take_bell`]). `true` means the host
    /// rang the bell at least once since the previous drain.
    pub fn take_bell(&mut self) -> bool {
        self.screen.take_bell()
    }

    pub fn set_osc52_read_enabled(&mut self, enabled: bool) {
        self.screen.set_osc52_read_enabled(enabled);
    }

    pub fn set_kitty_named_transports_enabled(&mut self, enabled: bool) {
        self.screen.set_kitty_named_transports_enabled(enabled);
    }

    /// Set the scrollback retention cap in logical lines (`0` = unbounded). See
    /// [`Screen::set_scrollback_limit`].
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.screen.set_scrollback_limit(limit);
    }

    /// Master gate for the button protocol (default off). See
    /// [`Screen::set_buttons_enabled`].
    pub fn set_buttons_enabled(&mut self, enabled: bool) {
        self.screen.set_buttons_enabled(enabled);
    }

    /// Accept the iTerm2 `OSC 1337 ; Button=` spelling (default on; inert
    /// while the master gate is off).
    pub fn set_buttons_iterm_compat(&mut self, enabled: bool) {
        self.screen.set_buttons_iterm_compat(enabled);
    }

    /// Honor `scope=sticky` (default on; off downgrades every definition to
    /// block scope).
    pub fn set_buttons_sticky(&mut self, enabled: bool) {
        self.screen.set_buttons_sticky(enabled);
    }

    /// Number of interned button entries (live + invalidated-but-referenced).
    /// Diagnostic/test surface.
    pub fn button_entry_count(&self) -> usize {
        self.screen.button_entry_count()
    }

    /// Button hit-test under a visible viewport cell (pointer arm, B3). See
    /// [`Screen::button_at`] — master-gate enforced, so this is `None` for
    /// every cell whenever the button protocol is off.
    pub fn button_at(&self, offset_rows: usize, row: usize, column: usize) -> Option<ButtonHit> {
        self.screen.button_at(offset_rows, row, column)
    }

    /// Whether a cooperating shell currently reports an active prompt. See
    /// [`Screen::prompt_active`].
    pub fn prompt_active(&self) -> bool {
        self.screen.prompt_active()
    }

    /// Buttons visible in the current viewport, projected onto viewport rows
    /// for rendering (Button Protocol B2). See
    /// [`Screen::visible_button_spans`]; gate-scoped to an empty vector when the
    /// button protocol is off.
    pub fn visible_button_spans(&self, offset_rows: usize) -> Vec<SnapshotButton> {
        self.screen.visible_button_spans(offset_rows)
    }

    pub fn scrollback_trim_epoch(&self) -> u64 {
        self.screen.scrollback_trim_epoch()
    }

    pub fn answer_clipboard_read(&mut self, selection: ClipboardSelection, text: &str) {
        self.screen.answer_clipboard_read(selection, text);
    }

    pub fn set_base_colors(
        &mut self,
        foreground: RgbColor,
        background: RgbColor,
        cursor: RgbColor,
    ) {
        self.screen.set_base_colors(foreground, background, cursor);
    }

    /// C29: seed the base 16 ANSI palette from the active theme. See
    /// [`Screen::set_base_palette`].
    pub fn set_base_palette(&mut self, palette: [RgbColor; 16]) {
        self.screen.set_base_palette(palette);
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.screen.bracketed_paste_enabled()
    }

    /// Reset the transient input-reporting mode family (bracketed paste, mouse,
    /// application cursor keys, focus reporting, click events, alternate scroll)
    /// to power-on defaults without touching cells, scrollback, or the cursor.
    /// See [`Screen::reset_input_reporting_modes`]; used on remote reconnect to
    /// keep a dropped session's latched modes from leaking into the fresh shell.
    pub(crate) fn reset_input_reporting_modes(&mut self) {
        self.screen.reset_input_reporting_modes();
    }

    /// DECSET 1007 (alternate scroll mode) state. See
    /// [`Screen::alternate_scroll_enabled`].
    pub fn alternate_scroll_enabled(&self) -> bool {
        self.screen.alternate_scroll_enabled()
    }

    /// Whether the alternate screen buffer is currently active. See
    /// [`Screen::on_alternate_screen`].
    pub fn on_alternate_screen(&self) -> bool {
        self.screen.on_alternate_screen()
    }

    /// The current window title (OSC 0/2), or `None` if never set.
    pub fn title(&self) -> Option<&str> {
        self.screen.title()
    }

    /// Whether the title changed since the last poll; clears the flag.
    pub fn take_title_changed(&mut self) -> bool {
        self.screen.take_title_changed()
    }

    /// The current working directory reported via OSC 7, or `None` if unset.
    pub fn current_working_directory(&self) -> Option<&str> {
        self.screen.current_working_directory()
    }

    /// Seed the advisory working directory from the local spawn cwd before the
    /// shell has a chance to emit OSC 7. Later OSC 7 updates still use the same
    /// parser/hostname policy and overwrite this seed.
    pub(crate) fn seed_working_directory(&mut self, cwd: String) {
        self.screen.set_working_directory(cwd);
    }

    /// Whether the working directory changed since the last poll; clears the
    /// flag.
    pub fn take_working_directory_changed(&mut self) -> bool {
        self.screen.take_working_directory_changed()
    }

    /// Set or clear the local hostname accepted by OSC 7. See
    /// [`Screen::set_local_hostname`].
    pub fn set_local_hostname(&mut self, local_hostname: Option<String>) {
        self.screen.set_local_hostname(local_hostname);
    }

    /// The OSC 133 prompt mark anchored to absolute row `row` (SH1), or `None`.
    /// Row `0` is the oldest scrollback row; see [`Screen::prompt_mark_at`].
    pub fn prompt_mark_at(&self, row: usize) -> Option<PromptKind> {
        self.screen.prompt_mark_at(row)
    }

    /// Every OSC 133 prompt mark as `(absolute_row, kind)` pairs in ascending
    /// row order (row `0` = oldest scrollback). The enumeration counterpart to
    /// [`Terminal::prompt_mark_at`]; see [`Screen::prompt_marks`].
    pub fn prompt_marks(&self) -> Vec<(usize, PromptKind)> {
        self.screen.prompt_marks()
    }

    /// Whether the set of prompt marks may have changed since the last poll
    /// (a new mark stamped, or marks cleared/repositioned by RIS, erase, resize,
    /// or an alternate-screen switch); clears the flag. See
    /// [`Screen::take_prompt_marks_changed`] for the conservative contract.
    pub fn take_prompt_marks_changed(&mut self) -> bool {
        self.screen.take_prompt_marks_changed()
    }

    /// The active mouse reporting protocol (tracking mode + encoding).
    pub fn mouse_protocol(&self) -> MouseProtocol {
        self.screen.mouse_protocol()
    }

    /// Keyboard modes that affect front-end key encoding.
    pub fn keyboard_modes(&self) -> KeyboardModes {
        self.screen.keyboard_modes()
    }

    /// G0/G1 charset designations and the SO/SI GL selection.
    pub fn charset_modes(&self) -> CharsetModes {
        self.screen.charset_modes()
    }

    /// Whether DECSET 1004 focus reporting is enabled.
    pub fn focus_reporting(&self) -> bool {
        self.screen.focus_reporting()
    }

    /// Whether OSC 133 click-to-position (SH-CLICK) is currently enabled by the
    /// shell; see [`Screen::click_events_enabled`].
    pub fn click_events_enabled(&self) -> bool {
        self.screen.click_events_enabled()
    }

    /// Active OSC 133 `B` input-start boundary as `(absolute_row, column)`;
    /// see [`Screen::active_prompt_input_start`].
    pub fn active_prompt_input_start(&self) -> Option<(usize, usize)> {
        self.screen.active_prompt_input_start()
    }

    /// The live editable prompt-input region; see [`Screen::input_region`].
    pub fn input_region(&self) -> Option<crate::core::input_region::InputRegion> {
        self.screen.input_region()
    }

    pub fn hyperlink(&self, id: LinkId) -> Option<&Hyperlink> {
        self.screen.hyperlink(id)
    }

    #[cfg(test)]
    pub(crate) fn hyperlink_count_for_test(&self) -> usize {
        self.screen.hyperlink_count()
    }

    /// The cursor shape currently in effect (DECSCUSR or host default).
    pub fn cursor_style(&self) -> CursorStyle {
        self.screen.cursor_style()
    }

    /// Whether the cursor's blink policy is currently enabled.
    pub fn cursor_blinking(&self) -> bool {
        self.screen.cursor_blinking()
    }

    /// Monotonic counter for visible terminal-state changes. See
    /// [`Screen::render_revision`].
    pub fn render_revision(&self) -> u64 {
        self.screen.render_revision()
    }

    pub fn dynamic_colors(&self) -> &DynamicColors {
        self.screen.dynamic_colors()
    }

    /// Whether DECSET 2026 synchronized output is currently enabled.
    pub fn synchronized_output_enabled(&self) -> bool {
        self.screen.synchronized_output_enabled()
    }

    /// Set the host default cursor shape and blink policy (from settings). See
    /// [`Screen::set_cursor_defaults`].
    pub fn set_cursor_defaults(&mut self, style: CursorStyle, blink: bool) {
        self.screen.set_cursor_defaults(style, blink);
    }

    /// Set the live cell pixel metrics for graphics extent calculation.
    /// See [`Screen::set_cell_metrics`].
    pub fn set_cell_metrics(&mut self, width_px: u32, height_px: u32) {
        self.screen.set_cell_metrics(width_px, height_px);
    }

    /// Set whether the backend's shell owns cursor placement on resize.
    /// See [`Screen::set_shell_owns_cursor_on_resize`].
    pub fn set_shell_owns_cursor_on_resize(&mut self, value: bool) {
        self.screen.set_shell_owns_cursor_on_resize(value);
    }

    /// Whether the backend's shell owns cursor placement on resize.
    /// See [`Screen::shell_owns_cursor_on_resize`]. Test-only (see the Screen
    /// getter): production only sets this from the backend capability.
    #[cfg(test)]
    pub(crate) fn shell_owns_cursor_on_resize(&self) -> bool {
        self.screen.shell_owns_cursor_on_resize()
    }

    /// Current cell pixel metrics. See [`Screen::cell_metrics`].
    pub fn cell_metrics(&self) -> CellMetrics {
        self.screen.cell_metrics()
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn graphics(&self) -> &ImageScene {
        self.screen.graphics()
    }

    pub fn graphics_mut(&mut self) -> &mut ImageScene {
        self.screen.graphics_mut()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.screen.snapshot()
    }

    /// Snapshot the visible grid at a scrollback viewport `offset_rows` (0 ==
    /// live screen). See [`Screen::snapshot_with_scrollback`] for the offset,
    /// clamping, cursor, and alternate-screen policy.
    pub fn snapshot_with_scrollback(&self, offset_rows: usize) -> Snapshot {
        self.screen.snapshot_with_scrollback(offset_rows)
    }

    /// Copy the constrained Phase 2 persistence subset into owned DTOs. See
    /// [`Screen::snapshot_state`].
    pub fn snapshot_state(&self, max_scrollback_rows: usize) -> SnapshotTerminalState {
        self.screen.snapshot_state(max_scrollback_rows)
    }

    /// Copy layout-affecting terminal state that is not part of the render
    /// [`Snapshot`] surface. See [`Screen::snapshot_layout_state`].
    pub fn snapshot_layout_state(&self) -> SnapshotLayoutState {
        self.screen.snapshot_layout_state()
    }

    pub fn visible_graphics(&self, offset_rows: usize) -> Vec<VisiblePlacement> {
        self.screen.visible_graphics(offset_rows)
    }

    /// Search the combined scrollback + visible buffer for `query`. See
    /// [`Screen::search`] for the coordinate convention and result ordering.
    pub fn search(&self, query: &str, options: SearchOptions) -> Vec<SearchMatch> {
        self.screen.search(query, options)
    }

    /// The visible viewport's physical rows (with `wrapped` flags) at scrollback
    /// `offset_rows`, for the hint / quick-select scanner. See
    /// [`Screen::visible_search_rows`] for the window and coordinate convention.
    pub fn visible_search_rows(&self, offset_rows: usize) -> Vec<VisibleRow> {
        self.screen.visible_search_rows(offset_rows)
    }

    /// Apply a decoded Phase 2 snapshot envelope into this terminal model.
    ///
    /// The parser is reset because the snapshot format stores terminal state,
    /// not an in-flight escape/DCS parser state.
    pub fn restore_from_envelope(
        &mut self,
        envelope: &SnapshotEnvelope,
    ) -> Result<(), SnapshotEnvelopeError> {
        self.screen.restore_from_envelope(envelope)?;
        self.parser = OdyParser::new();
        Ok(())
    }

    /// Construct a fresh terminal from a decoded Phase 2 snapshot envelope.
    pub fn from_snapshot_envelope(
        envelope: &SnapshotEnvelope,
    ) -> Result<Self, SnapshotEnvelopeError> {
        let mut terminal = Self::new(
            envelope.terminal.dimensions.columns,
            envelope.terminal.dimensions.rows,
        );
        terminal.restore_from_envelope(envelope)?;
        Ok(terminal)
    }
}
