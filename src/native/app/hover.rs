// SPDX-License-Identifier: GPL-3.0-only
//! Hover resolution for the native app: hyperlink, path, URL, button, and
//! inline-image hit resolution behind the pointer.
//!
//! Split out of the pointer-interaction module so hover resolution has its own
//! boundary. `App` remains the state owner; these are `App` methods in a child
//! module so they reach `App` fields and sibling methods directly.

use super::*;

/// The three button-protocol gates as one copyable unit (BUTTONS-SETTINGS):
/// snapshotted from `Settings` and pushed onto every session's `Terminal` at
/// spawn and on every settings apply/reload, so a panel or config change takes
/// effect live (the pointer arm reads the terminal-level gate per click).
#[derive(Debug, Clone, Copy)]
pub(super) struct ButtonGates {
    pub(super) enabled: bool,
    pub(super) iterm_compat: bool,
    pub(super) sticky: bool,
}

impl ButtonGates {
    pub(super) fn apply(self, terminal: &mut crate::core::Terminal) {
        terminal.set_buttons_enabled(self.enabled);
        terminal.set_buttons_iterm_compat(self.iterm_compat);
        terminal.set_buttons_sticky(self.sticky);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InteractivePathOpenKind {
    InlineImage,
    External,
}

pub(super) fn interactive_path_open_kind(
    settings: &crate::settings::Settings,
    resolved: &crate::paths::Resolved,
) -> InteractivePathOpenKind {
    if settings.interactive_paths_image_inline && crate::paths::is_image_path(&resolved.abs) {
        InteractivePathOpenKind::InlineImage
    } else {
        InteractivePathOpenKind::External
    }
}

impl App {
    /// Snapshot of the three button-protocol gates (BUTTONS-SETTINGS), copied
    /// out of `Settings` before a session borrow so the push sites never hold
    /// `self.settings` across the arena. Named fields rather than three loose
    /// bools so call sites cannot transpose the sub-gates.
    pub(super) fn button_gates(&self) -> ButtonGates {
        ButtonGates {
            enabled: self.settings.buttons,
            iterm_compat: self.settings.buttons_iterm_compat,
            sticky: self.settings.buttons_sticky,
        }
    }

    pub(super) fn update_hover_hyperlink(&mut self) {
        let hovered = self
            .pointer_cell
            .and_then(|point| self.visible_cell_hyperlink(point));
        if self.hovered_hyperlink != hovered {
            self.hovered_hyperlink = hovered;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Button Protocol chip hover: recompute the LIVE button under the pointer.
    /// Drives the hand cursor and the chip's hovered visual state; a change
    /// forces a rebuild so the hover restyle paints this frame. Gated on the
    /// `buttons` setting BEFORE any terminal query, so with the protocol off
    /// (the default) this is a single bool test and the hover path stays
    /// byte-identical. Invalidated buttons never hover — a dead chip is inert
    /// and must not invite a click it will swallow.
    pub(super) fn update_hover_button(&mut self) {
        let hovered = if self.settings.buttons {
            self.pointer_cell
                .and_then(|point| self.visible_cell_button(point))
                .filter(|hit| hit.state == crate::core::ButtonState::Live)
        } else {
            None
        };
        if self.hovered_button != hovered {
            self.hovered_button = hovered;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// INTERACTIVE-PATHS (Phase 7): recompute the resolved path span under the
    /// pointer and update the hover state that drives the pointer (hand) cursor.
    ///
    /// **The byte-identity gate.** The very first thing this does is check the
    /// `interactive_paths` setting; when it is off (the default) it returns
    /// before any terminal lock, row build, `detect_paths` scan, or stat probe —
    /// so the default hover path never scans and produces byte-identical frames.
    /// When on, it dedupes exactly like [`Self::update_hover_hyperlink`]: the
    /// rebuild flag/redraw fire only when the resolved span actually changes.
    pub(super) fn update_hover_path(&mut self) {
        if !self.settings.interactive_paths {
            // Clear a stale span if the setting was toggled off live while one
            // was hovered; otherwise nothing to do — the scanner never runs.
            if self.hovered_path.is_some() || self.hovered_path_cells.is_some() {
                self.hovered_path = None;
                self.hovered_path_cells = None;
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            self.hover_path_probe_key = None;
            return;
        }
        // Per-cell probe memo: `CursorMoved` fires on every reported pointer
        // motion, so re-running the up-to-8 `symlink_metadata` probes for a
        // pointer that has not left the same cell (and over unchanged content)
        // is wasted work and, on an autofs/stale-NFS path, a repeatable UI-thread
        // wedge. Skip the whole probe when the pointer cell, the scrollback
        // viewport offset, and the front-trim epoch are all unchanged since the
        // last probe. Scroll or a front-trim moves the row under the pointer and
        // invalidates the memo, so a genuinely different span is never missed.
        let probe_key = self.pointer_cell.map(|cell| {
            (
                cell,
                self.viewport.offset(),
                self.last_scrollback_trim_epoch,
            )
        });
        if probe_key.is_some() && probe_key == self.hover_path_probe_key {
            return;
        }
        self.hover_path_probe_key = probe_key;
        let (resolved, cells) = match self.resolved_hovered_path_with_cells() {
            Some((resolved, cells)) => (Some(resolved), Some(cells)),
            None => (None, None),
        };
        // Compare BOTH the resolved entry and the cell span: two occurrences of
        // the same filename on different rows resolve to the same `Resolved`, so
        // the span comparison is what moves the armed underline between them.
        if self.hovered_path != resolved || self.hovered_path_cells != cells {
            self.hovered_path = resolved;
            self.hovered_path_cells = cells;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Resolve the path span (if any) under the current pointer cell against the
    /// pane's OSC 7 working directory and `$HOME`, stat-gated through the active
    /// [`crate::paths::ResolveProbe`]. Pure aside from the single probe call;
    /// `None` when no live filesystem path sits under the pointer. Thin wrapper
    /// over [`Self::resolved_hovered_path_with_cells`] that drops the span — used
    /// by the context-menu path target which only needs the resolved entry.
    pub(super) fn resolved_hovered_path(&self) -> Option<crate::paths::Resolved> {
        self.resolved_hovered_path_with_cells()
            .map(|(resolved, _)| resolved)
    }

    /// INTERACTIVE-URLS: recompute the bare-URL span under the pointer.
    ///
    /// Mirrors [`Self::update_hover_path`]: when `interactive_urls` is off (and
    /// after clearing any stale span) it returns before any terminal lock or
    /// scan, so the default hover path is a single bool test and byte-identical.
    /// When on, it latches the openable URL under the pointer (if any) and fires
    /// a redraw only when the resolved URL or its span actually changes.
    pub(super) fn update_hover_url(&mut self) {
        if !self.settings.interactive_urls {
            if self.hovered_url.is_some() || self.hovered_url_cells.is_some() {
                self.hovered_url = None;
                self.hovered_url_cells = None;
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            return;
        }
        let (url, cells) = match self.resolved_hovered_url_with_cells() {
            Some((url, cells)) => (Some(url), Some(cells)),
            None => (None, None),
        };
        if self.hovered_url != url || self.hovered_url_cells != cells {
            self.hovered_url = url;
            self.hovered_url_cells = cells;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Find the bare (non-OSC-8) URL under the pointer cell and its visible-cell
    /// span. Runs the shared, tested [`crate::hints`] URL scanner over the single
    /// hovered row, picks the match covering the pointer column, and keeps it
    /// only when its scheme is openable ([`openable_hyperlink_uri`]). Returns
    /// `None` when no URL sits under the pointer, when the scheme is not openable
    /// (e.g. `ftp`/`ssh` are detected but not opened), or when the hovered cell
    /// already carries an OSC 8 hyperlink — that explicit path wins, so a cell is
    /// never double-decorated. One terminal lock, no filesystem or network access.
    pub(super) fn resolved_hovered_url_with_cells(
        &self,
    ) -> Option<(String, super::click_hint::HoverPathCells)> {
        let point = self.pointer_cell?;
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        let cols = snapshot.dimensions.columns;
        if cols == 0 || point.row >= snapshot.dimensions.rows {
            return None;
        }
        let start = point.row * cols;
        let row_cells = snapshot.cells.get(start..start + cols)?;
        // OSC 8 wins: an explicit hyperlink under the pointer is handled by the
        // OSC 8 path, so never light the bare-URL decoration on the same cell.
        if row_cells
            .get(point.column)
            .and_then(|cell| cell.attrs.hyperlink)
            .is_some()
        {
            return None;
        }
        let rows = [crate::core::SearchRow {
            cells: row_cells,
            wrapped: false,
        }];
        let matched = crate::hints::scan(&rows, crate::hints::HintKinds::URLS)
            .into_iter()
            .find(|m| {
                m.start.row == 0 && m.start.column <= point.column && point.column <= m.end.column
            })?;
        if !openable_hyperlink_uri(&matched.text) {
            return None;
        }
        let cells = super::click_hint::HoverPathCells {
            row: point.row,
            start: matched.start.column,
            end: matched.end.column + 1,
        };
        Some((matched.text, cells))
    }

    /// As [`Self::resolved_hovered_path`], but also returns the visible-cell span
    /// (UX-A): the row and column range the detected path occupies, so the
    /// Ctrl+hover armed underline can decorate exactly those cells. The span's
    /// byte offsets are mapped to column indices by counting chars (correct for
    /// any multi-byte content earlier in the row, though paths are ASCII/narrow).
    pub(super) fn resolved_hovered_path_with_cells(
        &self,
    ) -> Option<(crate::paths::Resolved, super::click_hint::HoverPathCells)> {
        let point = self.pointer_cell?;
        let (line, column, cwd) = self.hovered_row_text_and_cwd(point)?;
        // Map the pointer's cell column to a byte offset in the row string. Paths
        // are ASCII/narrow, so one char per cell column keeps the column and char
        // indices aligned.
        let target = line.char_indices().nth(column).map(|(byte, _)| byte)?;
        let options = crate::paths::DetectionOptions {
            barewords: self.settings.interactive_paths_barewords,
        };
        // Stat-guided span expansion: the scanner tokenizes on whitespace, so a
        // filename containing a space is split into separate tokens and never
        // resolves as one. Probe the contiguous token-run candidates that include
        // the hovered token, longest-first; the FIRST one the stat-gate confirms
        // exists wins. This picks the most-specific existing name (`my notes.txt`
        // over `notes.txt`) while prose runs that name no real file stay inert.
        // The single hovered token is always among the candidates, so a spaceless
        // filename resolves byte-identically to the previous single-span path.
        for span in crate::paths::detect_path_candidates_at(&line, target, options) {
            let Some(resolved) =
                self.classify_hovered_path(&span, cwd.as_deref(), self.home_dir.as_deref())
            else {
                continue;
            };
            // Panic-free byte→column mapping: count chars whose byte offset is
            // below the span boundary (never indexes a String slice at a raw
            // byte).
            let start = line
                .char_indices()
                .filter(|(byte, _)| *byte < span.start)
                .count();
            let end = line
                .char_indices()
                .filter(|(byte, _)| *byte < span.end)
                .count();
            let cells = super::click_hint::HoverPathCells {
                row: point.row,
                start,
                end,
            };
            return Some((resolved, cells));
        }
        None
    }

    /// UX-A (Phase 11): note a plain left-click that landed on a resolved path
    /// but did NOT open (the open-modifier gate failed — Ctrl on Linux, Cmd on
    /// macOS) — the "I clicked,
    /// nothing happened" mis-click. Raises the bottom-left teaching hint once two
    /// such mis-clicks land within the window. Gated INSIDE `interactive_paths`
    /// AND `interactive_paths_click_hint`; a no-op (no redraw) on every other
    /// path, so feature-off frames are byte-identical. Called from the left-press
    /// arm only when neither open helper fired.
    pub(super) fn note_possible_path_misclick(&mut self) {
        if !self.settings.interactive_paths || !self.settings.interactive_paths_click_hint {
            return;
        }
        // Only a click that actually landed on a resolved path counts.
        if self.hovered_path.is_none() {
            return;
        }
        // If the open gate WOULD have fired, this is not a mis-click (the open
        // path already handled it before we were called).
        if hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return;
        }
        if self.click_hint.note_misclick(std::time::Instant::now()) {
            self.request_selection_redraw();
        }
    }

    /// Single-lock fetch of the row text under `point` plus the pane's OSC 7
    /// working directory. Mirrors [`Self::visible_cell_hyperlink`]'s one-lock
    /// structure: the row string and the cwd both come from the same `terminal`
    /// lock. The row is built one char per cell column so a column index maps to
    /// a char index.
    fn hovered_row_text_and_cwd(
        &self,
        point: CellPoint,
    ) -> Option<(String, usize, Option<String>)> {
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        let cols = snapshot.dimensions.columns;
        if point.row >= snapshot.dimensions.rows {
            return None;
        }
        let start = point.row * cols;
        let row = snapshot.cells.get(start..start + cols)?;
        let line: String = row.iter().map(|cell| cell.ch).collect();
        let cwd = terminal.current_working_directory().map(str::to_owned);
        Some((line, point.column, cwd))
    }

    /// Stat-gate a candidate span through the production probe. Split on
    /// `cfg(test)` so headless hover tests resolve against an injected synthetic
    /// fs map (`test_path_probe`) and never touch the real filesystem, while
    /// production wires the real `std::fs::symlink_metadata` probe.
    #[cfg(not(test))]
    fn classify_hovered_path(
        &self,
        span: &crate::paths::PathSpan,
        cwd: Option<&str>,
        home: Option<&str>,
    ) -> Option<crate::paths::Resolved> {
        crate::paths::resolve(span, cwd, home, &super::interactive_paths::FsResolveProbe)
    }

    #[cfg(test)]
    fn classify_hovered_path(
        &self,
        span: &crate::paths::PathSpan,
        cwd: Option<&str>,
        home: Option<&str>,
    ) -> Option<crate::paths::Resolved> {
        crate::paths::resolve(span, cwd, home, &self.test_path_probe)
    }

    pub(super) fn visible_cell_hyperlink(&self, point: CellPoint) -> Option<LinkId> {
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        snapshot
            .cells
            .get(point.row * snapshot.dimensions.columns + point.column)
            .and_then(|cell| cell.attrs.hyperlink)
    }

    /// Button Protocol B3: resolve the button under the pointer cell against
    /// the core's viewport hit-test. One terminal lock; `None` whenever the
    /// master gate is off (the core query gates independently of the OSC arm,
    /// so disabling `buttons` kills clickability outright, not just new
    /// definitions).
    fn visible_cell_button(&self, point: CellPoint) -> Option<crate::core::ButtonHit> {
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        terminal.button_at(self.viewport.offset(), point.row, point.column)
    }

    /// Button Protocol B3 press arm: latch a live button under a plain left
    /// press. Returns `true` when the press was consumed (no selection, no
    /// open ladder, no misclick note for this gesture).
    ///
    /// Activation is PLAIN click only — buttons are explicit UI chips, so the
    /// click-hint "the cursor lies" problem does not apply — with exactly one
    /// modifier exception: while a mouse-reporting TUI owns clicks, Shift is
    /// the established local-content override (it bypasses the report gate),
    /// so a Shift+click is how a button stays reachable there, the same
    /// convention as selection. Outside reporting, Shift keeps its
    /// selection-extend meaning and Ctrl/Cmd (open) and Alt (block selection)
    /// keep theirs — those presses skip this arm entirely.
    ///
    /// The focus-transfer exclusion (#11167 class) consumes the pending
    /// focus-click marker even when the press misses every button: whatever
    /// that first-click-after-focus-gain landed on, it was spent activating
    /// the window.
    pub(super) fn try_press_button(&mut self) -> bool {
        let focus_click = std::mem::take(&mut self.focus_click_pending);
        let shift_override_active = self.modifiers.shift && self.mouse_reporting_enabled();
        if open_modifier_held(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) || self.modifiers.alt
            || (self.modifiers.shift && !shift_override_active)
        {
            return false;
        }
        let Some(point) = self.pointer_cell else {
            return false;
        };
        let Some(hit) = self.visible_cell_button(point) else {
            return false;
        };
        if hit.state != crate::core::ButtonState::Live || focus_click {
            // An invalidated chip renders dimmed and is inert — the press
            // falls through to selection so the surrounding text stays
            // selectable. A window-activating click over a button is likewise
            // never an activation; it falls through byte-identically to the
            // historical path, matching common focus-click behavior.
            return false;
        }
        self.pressed_button = Some(hit);
        // Backstop reuse of the open-click latch: if the paired release stops
        // being routable to the button arm (e.g. the app enables mouse
        // reporting mid-gesture), the reporting TUI must not see an unpaired
        // release. The button release arm runs BEFORE the swallow arm and
        // clears both latches on the normal path.
        self.swallow_open_left_release = true;
        true
    }

    /// Button Protocol B3 release arm: fire or cancel the latched press.
    ///
    /// Fires only when the release resolves the SAME span (matching id,
    /// viewport row, and start column) still `Live` under the release
    /// position — press+release same-span semantics, so drag-off, scrolling
    /// between press and release, and mid-gesture invalidation all cancel
    /// silently.
    ///
    /// STICKY PROMPT-ACTIVE SUPPRESSION (the §6 decision, resolved here):
    /// while a cooperating shell reports an active prompt (OSC 133 `A` with no
    /// `C`/`D` since), a `scope=sticky` button click is swallowed. A sticky
    /// button outliving its program has no reader that understands the report
    /// at a prompt — the bytes would land in the shell's line editor, where
    /// the best case is "consumed as an unknown escape" and the worst observed
    /// class is a stray literal `~` on very old readline builds. Nothing can
    /// act on it, so nothing is sent. Block-scoped buttons are deliberately
    /// NOT suppressed: a live block button during the prompt phase was defined
    /// IN that prompt block (a prompt-embedded chip a shell widget emitted),
    /// and its emitter is exactly the line editor currently reading stdin.
    /// Without shell integration the prompt state is never active and every
    /// live button reports.
    ///
    /// The report is composed by [`crate::core::click_report_bytes`] from the
    /// parsed integer only and enters the PTY through [`Self::write_pty_bytes`]
    /// — the same funnel mouse reports use (on Windows that is the ConPTY
    /// input pipe; there is no platform-specific surface here). No
    /// `return_to_live`: clicking a scrollback button must not yank the
    /// viewport.
    pub(super) fn finish_button_click(&mut self) {
        let Some(pressed) = self.pressed_button.take() else {
            return;
        };
        let Some(point) = self.pointer_cell else {
            return;
        };
        let (hit, prompt_active) = {
            let Ok(terminal) = self.terminal.lock() else {
                return;
            };
            (
                terminal.button_at(self.viewport.offset(), point.row, point.column),
                terminal.prompt_active(),
            )
        };
        let Some(hit) = hit else {
            return;
        };
        if hit.id != pressed.id
            || hit.row != pressed.row
            || hit.start_col != pressed.start_col
            || hit.state != crate::core::ButtonState::Live
        {
            return;
        }
        if hit.scope == crate::core::ButtonScope::Sticky && prompt_active {
            return;
        }
        self.write_pty_bytes(&crate::core::click_report_bytes(hit.code));
    }

    fn hovered_hyperlink_uri(&self) -> Option<String> {
        let id = self.hovered_hyperlink?;
        self.terminal
            .lock()
            .ok()?
            .hyperlink(id)
            .map(|link| link.uri.clone())
    }

    pub(super) fn try_open_hovered_hyperlink(&mut self) -> bool {
        if !hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return false;
        }
        let Some(uri) = self.hovered_hyperlink_uri() else {
            return false;
        };
        if !openable_hyperlink_uri(&uri) {
            return false;
        }

        // Security: OdyTTY never auto-opens OSC 8 links. A URI is opened only
        // after an explicit modifier+click (Ctrl on Linux, Cmd on macOS) and
        // scheme allowlist filtering, then passed as a single inert argv element
        // to the platform default opener. No shell command line is constructed
        // on any platform: Linux uses `xdg-open`, macOS `open`, and Windows
        // `explorer.exe <target>` (NOT `cmd /C start`, whose command line would
        // split an attacker-supplied URI on `&`/`%VAR%`). Routed through the
        // single argv-only spawn point shared with path opens; a failed/missing
        // opener surfaces a transient notice (P0-2).
        let argv = super::platform_opener::open_default_argv(
            super::platform_opener::OpenerOs::host(),
            &uri,
        );
        self.spawn_open_or_notice(&argv);
        true
    }

    /// INTERACTIVE-PATHS (Phase 8 / C3): modifier+click open for a resolved
    /// path span under the pointer (Ctrl on Linux, Cmd on macOS). Chained in the
    /// pointer Pressed arm AFTER
    /// [`Self::try_open_hovered_hyperlink`] (OSC 8 wins ties) and BEFORE
    /// `begin_selection`, so when this returns `false` the selection path is
    /// byte-identical.
    ///
    /// Returns `false` immediately — opening nothing, starting no selection
    /// change — when the feature is off, the open-modifier gate is not
    /// satisfied, or no live path span sits under the pointer. The gate reused
    /// is exactly the hyperlink one ([`hyperlink_action_allowed`]): the platform
    /// open modifier required (Ctrl on Linux, Cmd on macOS), suppressed under
    /// mouse reporting unless Shift overrides. The open itself
    /// is an argv-only [`super::interactive_paths::spawn_detached`] of the
    /// dispatch vector ([`super::interactive_paths::path_open_argv`]) — never a
    /// shell string.
    pub(super) fn try_open_hovered_path(&mut self) -> bool {
        if !self.settings.interactive_paths {
            return false;
        }
        if !hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return false;
        }
        let Some(resolved) = self.hovered_path.clone() else {
            return false;
        };
        if interactive_path_open_kind(&self.settings, &resolved)
            == InteractivePathOpenKind::InlineImage
            && self.open_image_view(&resolved)
        {
            return true;
        }
        let argv = self.path_open_argv_for(&resolved);
        self.spawn_open_or_notice(&argv);
        true
    }

    /// INTERACTIVE-URLS: modifier+click open for a bare (non-OSC-8) URL span
    /// under the pointer (Ctrl on Linux, Cmd on macOS). Chained in the pointer
    /// Pressed arm AFTER [`Self::try_open_hovered_hyperlink`] and
    /// [`Self::try_open_hovered_path`] (OSC 8 and resolved paths win ties), before
    /// `begin_selection`, so a `false` return leaves the selection path
    /// byte-identical.
    ///
    /// Returns `false` immediately — opening nothing, starting no selection
    /// change — when the feature is off, the open-modifier gate is not satisfied,
    /// or no openable URL sits under the pointer. The gate and the open dispatch
    /// are exactly the OSC 8 ones: [`hyperlink_action_allowed`] (platform open
    /// modifier, suppressed under mouse reporting unless Shift overrides),
    /// [`openable_hyperlink_uri`] scheme allowlist, and the argv-only
    /// [`super::platform_opener::open_default_argv`] dispatch — never a shell
    /// string, never auto-opened.
    pub(super) fn try_open_hovered_url(&mut self) -> bool {
        if !self.settings.interactive_urls {
            return false;
        }
        if !hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return false;
        }
        let Some(uri) = self.hovered_url.clone() else {
            return false;
        };
        if !openable_hyperlink_uri(&uri) {
            return false;
        }
        let argv = super::platform_opener::open_default_argv(
            super::platform_opener::OpenerOs::host(),
            &uri,
        );
        self.spawn_open_or_notice(&argv);
        true
    }

    /// Build the argv vector to open a resolved path, threading the configured
    /// editor override (`interactive_paths_editor`) and the `$EDITOR`/`$VISUAL`
    /// environment (read at open time). Pure aside from the env read; the spawn
    /// is the caller's separate step. Shared by the Ctrl+click path and the
    /// context-menu Open item so both dispatch identically.
    pub(super) fn path_open_argv_for(&self, resolved: &crate::paths::Resolved) -> Vec<String> {
        let editor_env = std::env::var("EDITOR")
            .ok()
            .or_else(|| std::env::var("VISUAL").ok());
        super::interactive_paths::path_open_argv(
            resolved,
            &self.settings.interactive_paths_editor,
            editor_env.as_deref(),
            super::platform_opener::OpenerOs::host(),
        )
    }

    /// Open a resolved image span in the in-terminal viewer (Phase 9 / C4).
    /// Decodes the file through the single bounded decode point
    /// ([`super::image_decode::decode_image_rgba`], FLAG B), uploads the pixels
    /// to the GPU image layer, and opens the `ImageView` overlay. Returns
    /// `false` when the resolved target is not a file, decode is refused/fails,
    /// or no GPU image layer exists; Ctrl+click uses that to fall back to the
    /// external opener. The context-menu action keeps its historical no-op on
    /// failure by ignoring this return value. Presentation-only.
    pub(super) fn open_image_view(&mut self, resolved: &crate::paths::Resolved) -> bool {
        // Only files are images; a directory span never reaches here, but guard
        // anyway so the decode is never attempted on a non-file.
        if resolved.kind != crate::paths::FsKind::File {
            return false;
        }
        let decode_started = Instant::now();
        let Some((rgba, width, height)) =
            crate::native::image_decode::decode_image_rgba(std::path::Path::new(&resolved.abs))
        else {
            return false;
        };
        let decode_elapsed = decode_started.elapsed();
        let Some(gpu) = self.gpu.as_mut() else {
            return false;
        };
        // Hand the pixels to the GPU overlay slot (centered fit computed there),
        // then open the presentation-only overlay with the filename caption.
        let upload_started = Instant::now();
        gpu.set_overlay_image(Some((rgba.as_slice(), width, height)));
        let upload_elapsed = upload_started.elapsed();
        tracing::debug!(
            width,
            height,
            decode_ms = decode_elapsed.as_millis(),
            upload_ms = upload_elapsed.as_millis(),
            "inline image viewer loaded"
        );
        let caption = resolved
            .abs
            .rsplit('/')
            .next()
            .unwrap_or(resolved.abs.as_str())
            .to_owned();
        self.overlay.open_image_view(caption);
        self.image_overlay = Some(super::interactive_paths::ImageOverlayState {
            rgba,
            width,
            height,
        });
        self.needs_rebuild = true;
        true
    }

    /// Keep the GPU image-viewer overlay in lockstep with the overlay state
    /// (C4). Called once per frame before drawing: when the `ImageView` overlay
    /// is no longer open (dismissed via Esc, click-outside, or any mode switch),
    /// clear the decoded buffer and the GPU overlay texture so the very next
    /// frame is byte-identical to the no-viewer path. Cheap no-op while the
    /// viewer stays open or was never opened.
    pub(super) fn sync_image_overlay(&mut self) {
        if self.image_overlay.is_some() && !self.overlay.image_view_open() {
            self.image_overlay = None;
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.set_overlay_image(None);
            }
        }
    }

    /// Re-push the current image-viewer overlay image after a surface resize so
    /// its centered fit-rect is recomputed for the new dimensions (C4). No-op
    /// when the viewer is closed.
    pub(super) fn refresh_image_overlay_on_resize(&mut self) {
        if let Some(state) = self.image_overlay.as_ref() {
            let rgba = state.rgba.clone();
            let (width, height) = (state.width, state.height);
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.set_overlay_image(Some((rgba.as_slice(), width, height)));
            }
        }
    }
}

#[cfg(test)]
mod tests;
