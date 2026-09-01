// SPDX-License-Identifier: GPL-3.0-only
//! Native connection-manager overlay state (Phase 4).
//!
//! The overlay is presentation state only: it owns a frozen, decoupled list of
//! local connection candidates (from [`crate::connection_hosts`]), a type-to-
//! filter query, and a selection cursor. It never writes to the PTY and never
//! mutates the live terminal model. Accepting a row returns a
//! [`ConnectionOverlayOutcome::Connect`] carrying the chosen host for the App to
//! act on after the overlay closes — the actual `ssh <host>` spawn lives in the
//! App's connect action, not here.
//!
//! Privacy: this module never reads `~/.ssh` itself. It only displays whatever
//! the App loaded through the data layer, which gates OpenSSH-config import
//! behind the `ssh_config_hosts` opt-in. When the opt-in is off the App hands
//! this overlay only OdyTTY-owned hosts.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::connection_hosts::{ConnectionHost, ConnectionHostSource, parse_adhoc_target};
use crate::fuzzy;

use super::overlay::OverlayInput;
use super::session::SessionToken;

/// Maximum rows rendered in the result list (keeps the overlay compact and the
/// fuzzy ranking bounded regardless of how large the hosts list is).
const MAX_RESULTS: usize = 40;

/// FORM-DISCOVERABILITY: the always-visible affordance footer reserves the two
/// bottom body rows of the connection manager — an actionable "+ Add
/// connection…" row and a key-hint line — so the Add/Edit/save actions are
/// reachable by sight and by mouse, not only through invisible chords.
const FOOTER_ROWS: usize = 2;
const ADD_ROW_LABEL: &str = "+ Add connection\u{2026}";
const KEY_HINT_LINE: &str =
    "Tab add \u{b7} \u{2192} edit \u{b7} Enter connect \u{b7} Shift+Enter save typed host";

/// One named launch profile row in the connection manager (v0.14). Loaded lazily
/// when the overlay opens; never scanned on the default launch path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConnectionProfileRow {
    pub(super) name: String,
    pub(super) label: String,
    pub(super) connection: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilteredRow {
    Profile(usize),
    Host(usize),
}

#[derive(Debug, Clone, Default)]
pub(super) struct ConnectionOverlay {
    /// The frozen connection list captured at open time, in load order
    /// (OdyTTY-owned hosts first, then any opt-in OpenSSH-config names).
    entries: Vec<ConnectionHost>,
    /// Named launch profiles for the connection-manager (`Connect`) purpose.
    profile_rows: Vec<ConnectionProfileRow>,
    /// Current type-to-filter query.
    query: String,
    /// Profile or host rows that match `query`, best-first. Recomputed whenever
    /// the query changes.
    filtered: Vec<FilteredRow>,
    /// Selection cursor into `filtered`. Clamped whenever `filtered` changes.
    selected: usize,
    /// Scroll offset into `filtered` for the visible window on a short overlay
    /// (OVERLAY-SMALL-WINDOW). Interior-mutable so the render pass — which is
    /// the only place the live body height is known — can keep the selection in
    /// view, mirroring the palette overlay's pattern. `0` whenever everything
    /// fits, so a tall overlay is byte-identical to before scrolling existed.
    scroll_offset: Cell<usize>,
    /// The last body height the render pass saw, so keyboard nav (which has no
    /// body height) can re-follow the selection through the same math.
    last_body_height: Cell<usize>,
    /// What accepting a row does (ODP-1B). Default `Connect` is the connection
    /// manager; a tagged purpose makes this the same list a shared picker for a
    /// pending menu action. Reset on every `open`.
    purpose: ConnectionPickerPurpose,
    /// Whether the always-visible "+ Add connection…" footer row is the current
    /// selection (FORM-DISCOVERABILITY). Reached by `Down` past the last host or
    /// a click on the row; `Enter` there opens the Add form. Reset on open and
    /// on any query change. Only meaningful in the `Connect` purpose (the footer
    /// is not shown for transient pickers).
    add_row_focused: bool,
}

/// What accepting a row in the connection picker means (ODP-1B shared picker
/// infra). `Connect` is the historical connection-manager behavior: the picker
/// is the connect surface, so accept spawns ssh (and Shift+Enter optionally
/// saves an ad-hoc host). The tagged variants turn the SAME filtered host list
/// into the generic "menu item → seeded picker → tagged accept" mechanism: a
/// context-menu item opens the picker seeded with saved hosts plus a pending
/// action, and accepting a row hands the App the chosen host paired with that
/// action to route — instead of connecting. New consumers (tab "Connect to
/// host", connection-row actions) add their own variant when they land; the
/// mechanism does not change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ConnectionPickerPurpose {
    /// The connection manager: accept connects; ad-hoc save is offered.
    #[default]
    Connect,
    /// Bind the active workspace to the chosen saved host (ODP-6B). Ad-hoc /
    /// unsaved rows are not offered — a workspace binds by saved-host alias.
    BindWorkspace,
    /// Bind the workspace at this rail index to the chosen saved host
    /// (RAIL-BIND). Like [`Self::BindWorkspace`] but targets the CLICKED slot
    /// from the rail context menu instead of the active workspace.
    BindWorkspaceIndex(usize),
    /// Open the chosen saved host in a NEW tab positioned right after the tab
    /// that owns this token (ODP-5D "Connect to host ▸"). The clicked tab is
    /// left untouched — the new remote tab reads as "connect from here".
    ConnectTabAfter(SessionToken),
    /// Replace the tab that owns this token with the chosen saved host (ODP-5D
    /// "Replace this tab with ▸"). The App gates the destructive close behind a
    /// confirm when that tab holds a running foreground child.
    ReplaceTab(SessionToken),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ConnectionOverlayOutcome {
    Consumed,
    Close,
    /// The user accepted a host. The App spawns the connection (presentation is
    /// done; this overlay never spawns anything itself).
    Connect(Box<ConnectionHost>),
    /// The user accepted an ad-hoc `[user@]host[:port]` that matched no saved
    /// host and asked to save it: the App connects AND appends a `Host` block to
    /// `hosts.conf`. Carried only from the synthetic "Connect to: …" row.
    ConnectAndSave(Box<ConnectionHost>),
    /// The user accepted a saved host while the picker was opened for a tagged
    /// pending action (ODP-1B). The App routes the chosen host per the purpose
    /// rather than connecting. Never emitted in the default `Connect` purpose.
    Pick(Box<ConnectionHost>, ConnectionPickerPurpose),
    /// Launch a new tab through the named-profile resolver (v0.14). Emitted only
    /// from a profile row in the connection-manager (`Connect`) purpose.
    LaunchProfile(String),
    /// Open the Add-connection form (REMOTE-UX P4). Raised by Tab in the
    /// connection manager; the overlay switches itself to the form mode.
    AddConnection,
    /// Open the Edit form pre-filled from the selected OdyTTY-owned host
    /// (REMOTE-UX P4). Raised by `\u{2192}` on a saved OdyTTY row;
    /// `ssh-config`-imported rows are read-only and never emit this.
    EditConnection(Box<ConnectionHost>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConnectionOverlayLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConnectionOverlaySignature {
    pub(super) query: String,
    pub(super) selected: Option<usize>,
    pub(super) results_len: usize,
    pub(super) results_fingerprint: u64,
}

impl ConnectionOverlay {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Load a frozen set of connection candidates and reset the query/cursor.
    /// The list is owned by the overlay, so it stays stable while open even if
    /// the underlying files change.
    /// Open the connection manager (the default `Connect` purpose). Test-only:
    /// production opens through the App/overlay via [`Self::open_for_purpose`],
    /// which threads the picker purpose.
    #[cfg(test)]
    pub(super) fn open(&mut self, entries: Vec<ConnectionHost>) {
        self.open_for_purpose(entries, ConnectionPickerPurpose::Connect, Vec::new());
    }

    /// Load a frozen candidate set for a tagged pending action (ODP-1B). The
    /// list/filter/cursor behavior is identical to [`Self::open`]; only the
    /// meaning of accept differs (see [`ConnectionPickerPurpose`]).
    pub(super) fn open_for_purpose(
        &mut self,
        entries: Vec<ConnectionHost>,
        purpose: ConnectionPickerPurpose,
        profile_rows: Vec<ConnectionProfileRow>,
    ) {
        self.entries = entries;
        self.profile_rows = if matches!(purpose, ConnectionPickerPurpose::Connect) {
            profile_rows
        } else {
            Vec::new()
        };
        self.query.clear();
        self.selected = 0;
        self.purpose = purpose;
        self.add_row_focused = false;
        self.reset_scroll();
        self.recompute();
    }

    /// Body rows reserved at the bottom for the affordance footer
    /// (FORM-DISCOVERABILITY). Only the connection-manager (`Connect`) purpose
    /// shows it, and only when the window has room for the query row, at least
    /// one result row, and the two footer rows — a tiny window falls back to the
    /// pre-footer layout so the list is never squeezed to nothing.
    fn footer_rows(&self, body_height: usize) -> usize {
        if matches!(self.purpose, ConnectionPickerPurpose::Connect)
            && body_height >= 1 + 1 + FOOTER_ROWS
        {
            FOOTER_ROWS
        } else {
            0
        }
    }

    /// Result rows visible in the scrolling window: the body minus the query row
    /// and any reserved footer rows. Every list-geometry method funnels through
    /// this so the footer reservation stays consistent across render, scroll,
    /// and click mapping.
    fn visible_results_rows(&self, body_height: usize) -> usize {
        body_height.saturating_sub(1 + self.footer_rows(body_height))
    }

    /// Whether the affordance footer is currently drawn, using the last body
    /// height the render pass recorded (keyboard nav has no live body height).
    /// `false` until the overlay has rendered once.
    fn footer_active(&self) -> bool {
        self.footer_rows(self.last_body_height.get()) > 0
    }

    /// Whether the ad-hoc "Connect to: …" affordance is offered. Only the
    /// connection-manager (`Connect`) purpose connects to an unsaved host; a
    /// bind picker offers saved hosts only, so it shows the plain "No matches".
    fn allows_adhoc(&self) -> bool {
        matches!(self.purpose, ConnectionPickerPurpose::Connect)
    }

    #[cfg(test)]
    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Recompute the filtered/ranked view from the current query.
    ///
    /// Empty query preserves load order (OdyTTY hosts first); a non-empty query
    /// uses the shared fuzzy scorer over each row's searchable text.
    fn recompute(&mut self) {
        if self.query.is_empty() {
            self.filtered.clear();
            for index in 0..self.profile_rows.len() {
                self.filtered.push(FilteredRow::Profile(index));
                if self.filtered.len() >= MAX_RESULTS {
                    break;
                }
            }
            if self.filtered.len() < MAX_RESULTS {
                for index in 0..self.entries.len() {
                    self.filtered.push(FilteredRow::Host(index));
                    if self.filtered.len() >= MAX_RESULTS {
                        break;
                    }
                }
            }
        } else {
            let mut haystacks = Vec::with_capacity(self.profile_rows.len() + self.entries.len());
            let mut kinds = Vec::with_capacity(self.profile_rows.len() + self.entries.len());
            for (index, row) in self.profile_rows.iter().enumerate() {
                haystacks.push(profile_match_text(row));
                kinds.push(FilteredRow::Profile(index));
            }
            for (index, entry) in self.entries.iter().enumerate() {
                haystacks.push(match_text(entry));
                kinds.push(FilteredRow::Host(index));
            }
            self.filtered = fuzzy::rank(&self.query, &haystacks)
                .into_iter()
                .take(MAX_RESULTS)
                .filter_map(|(index, _)| kinds.get(index).copied())
                .collect();
        }
        // A query change re-anchors the selection to a real row; the add-row
        // focus never survives a filter change.
        self.add_row_focused = false;
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        let next = (self.selected as isize + delta).clamp(0, max as isize);
        self.selected = next as usize;
    }

    fn selected_row(&self) -> Option<FilteredRow> {
        self.filtered.get(self.selected).copied()
    }

    fn selected_entry(&self) -> Option<&ConnectionHost> {
        let FilteredRow::Host(entry_index) = self.selected_row()? else {
            return None;
        };
        self.entries.get(entry_index)
    }

    /// Whether the selection sits on the last (or no) host — the point from
    /// which `Down` steps onto the "+ Add connection…" footer row.
    fn at_last_result(&self) -> bool {
        self.filtered.is_empty() || self.selected + 1 >= self.filtered.len()
    }

    /// The aliases of the OdyTTY-owned saved hosts, for the Add/Edit form's
    /// alias-collision guard. `ssh-config`-imported names live in a different
    /// file and never collide with a `hosts.conf` block, so they are excluded.
    pub(super) fn odytty_aliases(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.source == ConnectionHostSource::Odytty)
            .map(|entry| entry.alias.clone())
            .collect()
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ConnectionOverlayOutcome {
        match input {
            OverlayInput::Close => ConnectionOverlayOutcome::Close,
            OverlayInput::Up => {
                // Leaving the footer's "+ Add connection…" row returns to the
                // last host; otherwise walk the result list as before.
                if self.add_row_focused {
                    self.add_row_focused = false;
                } else {
                    self.move_selection(-1);
                }
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::Down => {
                // Past the last host, `Down` steps onto the always-visible
                // "+ Add connection…" footer row (FORM-DISCOVERABILITY) rather
                // than sticking on the final host.
                if self.footer_active() && !self.add_row_focused && self.at_last_result() {
                    self.add_row_focused = true;
                } else if !self.add_row_focused {
                    self.move_selection(1);
                }
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.add_row_focused = false;
                self.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
                self.add_row_focused = false;
                self.move_selection(MAX_RESULTS as isize);
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.query.pop();
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::Activate => match self.purpose {
                // Connection manager: the "+ Add connection…" footer row opens
                // the Add form; else accept a saved row, else offer the ad-hoc
                // connect when the query is a well-formed destination.
                ConnectionPickerPurpose::Connect if self.add_row_focused => {
                    ConnectionOverlayOutcome::AddConnection
                }
                ConnectionPickerPurpose::Connect => match self.selected_row() {
                    Some(FilteredRow::Profile(index)) => self
                        .profile_rows
                        .get(index)
                        .map(|row| ConnectionOverlayOutcome::LaunchProfile(row.name.clone()))
                        .unwrap_or(ConnectionOverlayOutcome::Consumed),
                    Some(FilteredRow::Host(_)) => match self.selected_entry() {
                        Some(entry) => ConnectionOverlayOutcome::Connect(Box::new(entry.clone())),
                        None => ConnectionOverlayOutcome::Consumed,
                    },
                    None => match self.adhoc_target() {
                        Some(host) => ConnectionOverlayOutcome::Connect(Box::new(host)),
                        None => ConnectionOverlayOutcome::Consumed,
                    },
                },
                // Any tagged pending action (bind a workspace, connect from a
                // tab, replace a tab): only a saved host is acceptable — these
                // pickers offer saved hosts only, so an empty selection is inert.
                // The App routes the pick per the carried purpose.
                ConnectionPickerPurpose::BindWorkspace
                | ConnectionPickerPurpose::BindWorkspaceIndex(_)
                | ConnectionPickerPurpose::ConnectTabAfter(_)
                | ConnectionPickerPurpose::ReplaceTab(_) => match self.selected_entry() {
                    Some(entry) => {
                        ConnectionOverlayOutcome::Pick(Box::new(entry.clone()), self.purpose)
                    }
                    None => ConnectionOverlayOutcome::Consumed,
                },
            },
            // Shift+Enter (ActivateAlt) or Ctrl+S (Save) on the synthetic ad-hoc
            // row = connect AND append the host to hosts.conf. On a saved row, in
            // a bind picker, or when the query is not ad-hoc it is inert. Ctrl+S
            // is the Windows-safe alternative to Shift+Enter and behaves
            // identically here.
            OverlayInput::ActivateAlt | OverlayInput::Save => {
                if !self.add_row_focused
                    && self.selected_entry().is_none()
                    && let Some(host) = self.adhoc_target()
                {
                    ConnectionOverlayOutcome::ConnectAndSave(Box::new(host))
                } else {
                    ConnectionOverlayOutcome::Consumed
                }
            }
            // Tab opens the Add-connection form; `\u{2192}` opens the Edit form for a
            // selected OdyTTY-owned row (P4). An `ssh-config`-imported row is
            // read-only, so `\u{2192}` there is inert. The footer add-row is not
            // an editable host, so `\u{2192}` there is inert too.
            OverlayInput::Tab => ConnectionOverlayOutcome::AddConnection,
            OverlayInput::Right if self.add_row_focused => ConnectionOverlayOutcome::Consumed,
            OverlayInput::Right => match self.selected_entry() {
                Some(entry) if entry.source == ConnectionHostSource::Odytty => {
                    ConnectionOverlayOutcome::EditConnection(Box::new(entry.clone()))
                }
                _ => ConnectionOverlayOutcome::Consumed,
            },
            OverlayInput::Char(_) | OverlayInput::Left => ConnectionOverlayOutcome::Consumed,
        }
    }

    /// The ad-hoc connection host for the current query, or `None` when the
    /// query matches a saved host (so the normal list is shown) or does not
    /// parse as `[user@]host[:port]`. Only meaningful when the filtered list is
    /// empty — a query that fuzzy-matches a saved row never offers ad-hoc.
    fn adhoc_target(&self) -> Option<ConnectionHost> {
        if !self.allows_adhoc() || !self.filtered.is_empty() {
            return None;
        }
        parse_adhoc_target(&self.query).map(|target| target.to_connection_host())
    }

    /// Scroll one row in response to a wheel notch (negative = toward the top).
    pub(super) fn scroll_lines(&mut self, lines: isize) {
        self.move_selection(lines.signum());
        self.follow_selection_for_known_body_height();
    }

    /// Map a clicked body row to the selection cursor it represents — the
    /// inverse of the [`Self::visible_lines`] windowing (UX4-P1 click→Activate).
    /// Row 0 is the `> query` prompt; host rows follow from the live
    /// `scroll_offset`. Returns `None` for the prompt row, the empty/"No
    /// matches" hint, or a click past the last host.
    pub(super) fn row_at(&self, row_in_body: usize, body_height: usize) -> Option<usize> {
        if body_height == 0 || row_in_body == 0 || self.filtered.is_empty() {
            return None;
        }
        let visible_results = self.visible_results_rows(body_height);
        let within = row_in_body - 1;
        if within >= visible_results {
            return None;
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let cursor = scroll_offset + within;
        (cursor < self.filtered.len()).then_some(cursor)
    }

    /// Map a right-clicked body row to `(filtered cursor, cloned host)` for the
    /// ODP-2C connection-row context menu. Returns `None` for the prompt row,
    /// the empty/"No matches"/ad-hoc hint, or a click past the last host — a
    /// menu only opens on a real saved-host row. Read-only: unlike `click_row`
    /// it never moves the selection, so right-click-for-menu leaves the keyboard
    /// cursor where it was.
    pub(super) fn host_at_row(
        &self,
        row_in_body: usize,
        body_height: usize,
    ) -> Option<(usize, ConnectionHost)> {
        let cursor = self.row_at(row_in_body, body_height)?;
        let FilteredRow::Host(entry_index) = *self.filtered.get(cursor)? else {
            return None;
        };
        let host = self.entries.get(entry_index)?.clone();
        Some((cursor, host))
    }

    /// Select the row under a left-click, reporting whether it landed on a
    /// selectable row so the caller can route the existing Activate. Parity with
    /// Down×N + Activate by construction.
    pub(super) fn click_row(&mut self, row_in_body: usize, body_height: usize) -> bool {
        // FORM-DISCOVERABILITY: the pinned "+ Add connection…" footer row sits at
        // `body_height - FOOTER_ROWS`; a click there focuses it so the caller's
        // routed Activate opens the Add form.
        let footer = self.footer_rows(body_height);
        if footer > 0 && row_in_body == body_height - footer {
            self.add_row_focused = true;
            return true;
        }
        // The synthetic ad-hoc "Connect to: …" row sits at body row 1 when the
        // filtered list is empty but the query parses; a click there connects
        // (the caller routes Activate, which the empty-selection path resolves
        // to the ad-hoc host). Saving still requires Shift+Enter / Ctrl+S.
        if body_height > 1
            && row_in_body == 1
            && self.filtered.is_empty()
            && self.adhoc_target().is_some()
        {
            self.add_row_focused = false;
            return true;
        }
        match self.row_at(row_in_body, body_height) {
            Some(cursor) => {
                self.selected = cursor;
                self.add_row_focused = false;
                self.follow_selection_for_known_body_height();
                true
            }
            None => false,
        }
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ConnectionOverlayLine> {
        if body_height == 0 {
            self.last_body_height.set(0);
            self.scroll_offset.set(0);
            return Vec::new();
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        // FORM-DISCOVERABILITY: reserve the bottom rows for the pinned affordance
        // footer so it is always visible; content fills the space above it.
        let footer = self.footer_rows(body_height);
        let content_cap = body_height.saturating_sub(footer);
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2 + FOOTER_ROWS));
        lines.push(ConnectionOverlayLine {
            text: truncate_for_width(&format!("> {}", self.query), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() < content_cap {
            if self.entries.is_empty() && self.profile_rows.is_empty() {
                self.scroll_offset.set(0);
                lines.push(ConnectionOverlayLine {
                    text: truncate_for_width(
                        "No saved connections — add hosts to hosts.conf or enable ssh_config_hosts.",
                        body_width,
                    ),
                    focused: false,
                    bold: false,
                });
            } else if self.filtered.is_empty() {
                self.scroll_offset.set(0);
                // When the query is a well-formed `[user@]host[:port]` that
                // matches no saved host, offer an ad-hoc connect row in place of
                // "No matches" — with a key hint so both actions are
                // discoverable. A bind picker (no ad-hoc) shows plain "No
                // matches".
                if let Some(target) = self
                    .allows_adhoc()
                    .then(|| parse_adhoc_target(&self.query))
                    .flatten()
                {
                    lines.push(ConnectionOverlayLine {
                        text: truncate_for_width(
                            &format!("Connect to: {}", target.display()),
                            body_width,
                        ),
                        focused: !self.add_row_focused,
                        bold: false,
                    });
                    if lines.len() < content_cap {
                        lines.push(ConnectionOverlayLine {
                            text: truncate_for_width(
                                "[Enter] connect · [Shift+Enter] connect + save",
                                body_width,
                            ),
                            focused: false,
                            bold: false,
                        });
                    }
                } else {
                    lines.push(ConnectionOverlayLine {
                        text: "No matches".to_owned(),
                        focused: false,
                        bold: false,
                    });
                }
            } else {
                let visible_results = self.visible_results_rows(body_height);
                for (visible_index, filtered_row) in self
                    .filtered
                    .iter()
                    .skip(scroll_offset)
                    .take(visible_results)
                    .enumerate()
                {
                    let row = scroll_offset + visible_index;
                    let label = match filtered_row {
                        FilteredRow::Profile(index) => self
                            .profile_rows
                            .get(*index)
                            .map(profile_row_label)
                            .unwrap_or_default(),
                        FilteredRow::Host(index) => {
                            self.entries.get(*index).map(row_label).unwrap_or_default()
                        }
                    };
                    lines.push(ConnectionOverlayLine {
                        text: truncate_for_width(&label, body_width),
                        focused: row == self.selected && !self.add_row_focused,
                        bold: false,
                    });
                }
            }
        }
        // Pin the affordance footer to the bottom of the body: pad the gap above
        // it with blanks, then the actionable "+ Add connection…" row and the
        // key-hint line. `footer == 0` (a transient picker or a window too short)
        // leaves the layout byte-identical to the pre-footer manager.
        if footer > 0 {
            while lines.len() < body_height - footer {
                lines.push(ConnectionOverlayLine {
                    text: String::new(),
                    focused: false,
                    bold: false,
                });
            }
            lines.push(ConnectionOverlayLine {
                text: truncate_for_width(ADD_ROW_LABEL, body_width),
                focused: self.add_row_focused,
                bold: false,
            });
            lines.push(ConnectionOverlayLine {
                text: truncate_for_width(KEY_HINT_LINE, body_width),
                focused: false,
                bold: false,
            });
        }
        lines
    }

    /// Hidden result rows above / below the visible window, for the shared
    /// scroll affordance (OVERLAY-SMALL-WINDOW). One body row is the query
    /// line, so the result viewport is `body_height - 1`. `(false, false)`
    /// whenever everything fits, so a tall overlay draws no arrows.
    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        let visible_results = self.visible_results_rows(body_height);
        if visible_results == 0 || self.filtered.len() <= visible_results {
            self.scroll_offset.set(0);
            return (false, false);
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        (
            scroll_offset > 0,
            scroll_offset + visible_results < self.filtered.len(),
        )
    }

    fn reset_scroll(&self) {
        self.scroll_offset.set(0);
    }

    /// Re-follow the selection using the last body height the render pass saw,
    /// so keyboard/wheel nav (which has no body height) keeps the selection in
    /// view between frames. No-op until the overlay has rendered once.
    fn follow_selection_for_known_body_height(&self) {
        let body_height = self.last_body_height.get();
        if body_height > 0 {
            self.scroll_offset_for_body_height(body_height);
        }
    }

    /// Resolve (and memoize) the scroll offset for a given body height, keeping
    /// the selected row inside the `[offset, offset + visible_results)` window.
    /// Mirrors the palette overlay so the two list overlays scroll identically.
    fn scroll_offset_for_body_height(&self, body_height: usize) -> usize {
        self.last_body_height.set(body_height);
        let visible_results = self.visible_results_rows(body_height);
        let results_len = self.filtered.len();
        if visible_results == 0 || results_len <= visible_results {
            self.scroll_offset.set(0);
            return 0;
        }
        let max_scroll = results_len - visible_results;
        let mut scroll_offset = self.scroll_offset.get().min(max_scroll);
        if self.selected < scroll_offset {
            scroll_offset = self.selected;
        } else if self.selected >= scroll_offset + visible_results {
            scroll_offset = self.selected + 1 - visible_results;
        }
        self.scroll_offset.set(scroll_offset);
        scroll_offset
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        columns.min(84)
    }

    pub(super) fn render_signature(&self) -> ConnectionOverlaySignature {
        ConnectionOverlaySignature {
            query: self.query.clone(),
            selected: if self.filtered.is_empty() {
                None
            } else {
                Some(self.selected)
            },
            results_len: self.filtered.len(),
            results_fingerprint: self.results_fingerprint(),
        }
    }

    fn results_fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        // Fold in the scroll offset so a view-only scroll (selection unchanged)
        // still changes the signature → the render cache repaints instead of
        // classifying the frame `Retained` and freezing the list (the
        // cache-staleness lesson from the settings Level-1 list).
        self.scroll_offset.get().hash(&mut hasher);
        // FORM-DISCOVERABILITY: toggling focus onto the "+ Add connection…"
        // footer row moves the highlight without changing query/selection, so
        // fold it in or the highlight would freeze on a `Retained` frame.
        self.add_row_focused.hash(&mut hasher);
        for filtered_row in self.filtered.iter().take(MAX_RESULTS) {
            match filtered_row {
                FilteredRow::Profile(index) => {
                    if let Some(row) = self.profile_rows.get(*index) {
                        row.name.hash(&mut hasher);
                        row.label.hash(&mut hasher);
                        row.connection.hash(&mut hasher);
                    }
                }
                FilteredRow::Host(index) => {
                    if let Some(entry) = self.entries.get(*index) {
                        entry.alias.hash(&mut hasher);
                        entry.host_name.hash(&mut hasher);
                        entry.user.hash(&mut hasher);
                        entry.port.hash(&mut hasher);
                    }
                }
            }
        }
        hasher.finish()
    }
}

fn profile_match_text(row: &ConnectionProfileRow) -> String {
    let mut text = row.name.clone();
    text.push(' ');
    text.push_str(&row.label);
    if let Some(connection) = row.connection.as_deref() {
        text.push(' ');
        text.push_str(connection);
    }
    sanitize(&text)
}

fn profile_row_label(row: &ConnectionProfileRow) -> String {
    let label = sanitize(&row.label);
    match row.connection.as_deref() {
        Some(connection) => format!("Profile: {label}   -> {}", sanitize(connection)),
        None => format!("Profile: {label}   (local)"),
    }
}

/// Build the searchable text for one host: alias plus host name and user so a
/// user can fuzzy-match on any of them. Profile fields (theme/font/title) are
/// not searched — they are display metadata, not connection identity.
fn match_text(entry: &ConnectionHost) -> String {
    let mut text = entry.alias.clone();
    if let Some(host_name) = entry.host_name.as_deref() {
        text.push(' ');
        text.push_str(host_name);
    }
    if let Some(user) = entry.user.as_deref() {
        text.push(' ');
        text.push_str(user);
    }
    sanitize(&text)
}

/// Render one host row: `alias   user@host:port   [theme]   (source)`.
fn row_label(entry: &ConnectionHost) -> String {
    let mut label = sanitize(&entry.alias);
    let target = connection_target(entry);
    if !target.is_empty() {
        label.push_str("   ");
        label.push_str(&target);
    }
    if let Some(theme) = entry.theme.as_deref() {
        label.push_str("   [");
        label.push_str(&sanitize(theme));
        label.push(']');
    }
    label.push_str("   (");
    label.push_str(source_tag(entry.source));
    label.push(')');
    label
}

/// `user@host:port`, omitting any absent part. Falls back to the alias-only
/// case by returning an empty string (the alias already leads the row).
fn connection_target(entry: &ConnectionHost) -> String {
    let host = entry.host_name.as_deref().unwrap_or("");
    let mut target = String::new();
    if let Some(user) = entry.user.as_deref() {
        target.push_str(&sanitize(user));
        if !host.is_empty() {
            target.push('@');
        }
    }
    target.push_str(&sanitize(host));
    if let Some(port) = entry.port {
        target.push(':');
        target.push_str(&port.to_string());
    }
    target
}

fn source_tag(source: ConnectionHostSource) -> &'static str {
    match source {
        ConnectionHostSource::Odytty => "OdyTTY",
        ConnectionHostSource::SshConfig => "ssh-config",
    }
}

/// Strip control characters so a malformed host name can never inject escape
/// sequences into the overlay's plain-text rows.
fn sanitize(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control()).collect()
}

fn truncate_for_width(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(
        alias: &str,
        host_name: Option<&str>,
        user: Option<&str>,
        port: Option<u16>,
        source: ConnectionHostSource,
    ) -> ConnectionHost {
        ConnectionHost {
            alias: alias.to_owned(),
            host_name: host_name.map(str::to_owned),
            user: user.map(str::to_owned),
            port,
            theme: None,
            font: None,
            title: None,
            integration: None,
            reuse: None,
            tmux: None,
            protocol: None,
            identity_file: None,
            persist: None,
            source,
        }
    }

    fn entries() -> Vec<ConnectionHost> {
        vec![
            host(
                "web1",
                Some("gateway.example.invalid"),
                Some("deploy"),
                Some(2222),
                ConnectionHostSource::Odytty,
            ),
            host(
                "db-primary",
                Some("db.example.invalid"),
                None,
                None,
                ConnectionHostSource::Odytty,
            ),
            host(
                "remote",
                Some("remote.example.invalid"),
                None,
                None,
                ConnectionHostSource::SshConfig,
            ),
        ]
    }

    fn open(entries: Vec<ConnectionHost>) -> ConnectionOverlay {
        let mut overlay = ConnectionOverlay::new();
        overlay.open(entries);
        overlay
    }

    fn type_query(overlay: &mut ConnectionOverlay, query: &str) {
        for ch in query.chars() {
            assert_eq!(
                overlay.handle_input(OverlayInput::Char(ch)),
                ConnectionOverlayOutcome::Consumed
            );
        }
    }

    #[test]
    fn empty_query_lists_all_in_load_order() {
        let overlay = open(entries());
        assert_eq!(overlay.render_signature().results_len, 3);
        // Load order is preserved: OdyTTY hosts first, ssh-config last.
        let lines = overlay.visible_lines(80, 10);
        // Line 0 is the query prompt; rows follow.
        assert!(lines[1].text.starts_with("web1"));
        assert!(lines[2].text.starts_with("db-primary"));
        assert!(lines[3].text.starts_with("remote"));
    }

    #[test]
    fn manager_renders_add_row_and_key_hint_footer() {
        // FORM-DISCOVERABILITY: the manager pins a "+ Add connection…" row and a
        // key-hint line to the bottom of the body so the form is discoverable.
        let overlay = open(entries());
        let lines = overlay.visible_lines(80, 12);
        let add = &lines[lines.len() - 2];
        let hint = &lines[lines.len() - 1];
        assert!(add.text.contains("Add connection"), "add-row pinned bottom");
        assert!(hint.text.contains("Tab add"), "key hint pinned bottom");
        assert!(hint.text.contains("Shift+Enter save"), "save chord shown");
    }

    #[test]
    fn down_past_last_host_focuses_add_row_and_enter_opens_form() {
        // Walking `Down` past the last host lands on the add-row; Enter there
        // opens the Add form, and Up steps back onto the last host.
        let mut overlay = open(entries());
        let _ = overlay.visible_lines(80, 12); // prime the body height
        for _ in 0..entries().len() {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::AddConnection,
            "Enter on the add-row opens the Add form"
        );
        // Re-open (Activate did not close the manager itself) and confirm Up
        // leaves the add-row.
        let _ = overlay.handle_input(OverlayInput::Up);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::Connect(Box::new(entries()[2].clone())),
            "Up returns to the last host, where Enter connects"
        );
    }

    #[test]
    fn click_add_row_opens_form() {
        // A click on the pinned add-row focuses it (click_row true) so the
        // caller's routed Activate opens the Add form.
        let mut overlay = open(entries());
        let body_height = 12;
        let _ = overlay.visible_lines(80, body_height);
        assert!(
            overlay.click_row(body_height - FOOTER_ROWS, body_height),
            "click lands on the add-row"
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::AddConnection
        );
    }

    #[test]
    fn bind_picker_has_no_add_row_footer() {
        // A transient picker (bind a workspace) is a host chooser, not the
        // manager — it shows no Add footer.
        let overlay = open_for_bind(entries());
        let lines = overlay.visible_lines(80, 12);
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(
            !joined.contains("Add connection"),
            "picker hides the add-row"
        );
    }

    #[test]
    fn fuzzy_filter_ranks_matching_host_first() {
        // FUZZY-FILTER-RANKS-ENTRIES: typing narrows and ranks by match quality.
        let mut overlay = open(entries());
        type_query(&mut overlay, "web");
        let signature = overlay.render_signature();
        assert_eq!(signature.results_len, 1);
        // The single match is the selected row.
        assert_eq!(signature.selected, Some(0));
        let lines = overlay.visible_lines(80, 10);
        assert!(lines[1].text.starts_with("web1"));
    }

    #[test]
    fn fuzzy_matches_host_name_and_user_not_only_alias() {
        let mut overlay = open(entries());
        // "deploy" only appears in the User field of web1.
        type_query(&mut overlay, "deploy");
        assert_eq!(overlay.render_signature().results_len, 1);
        assert!(matches!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::Connect(host) if host.alias == "web1"
        ));
    }

    #[test]
    fn accept_emits_connect_for_selected_host() {
        let mut overlay = open(entries());
        overlay.handle_input(OverlayInput::Down); // select db-primary
        assert!(matches!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::Connect(host) if host.alias == "db-primary"
        ));
    }

    // ── UX4-P1 click→Activate parity ───────────────────────────────────────

    #[test]
    fn click_row_selects_same_host_as_down_then_activate() {
        for target in 0..3 {
            let mut by_click = open(entries());
            let _ = by_click.visible_lines(80, 10);
            assert!(by_click.click_row(target + 1, 10));
            let click_connect = by_click.handle_input(OverlayInput::Activate);

            let mut by_keys = open(entries());
            for _ in 0..target {
                by_keys.handle_input(OverlayInput::Down);
            }
            let key_connect = by_keys.handle_input(OverlayInput::Activate);

            assert_eq!(click_connect, key_connect, "row {target} parity");
        }
    }

    #[test]
    fn host_at_row_returns_row_host_and_ignores_non_host_rows() {
        // ODP-2C: a right-click hit-test resolves a body row to (cursor, host)
        // for the connection-row menu; the prompt row and past-end rows yield
        // None, and the selection is never moved (read-only).
        let overlay = open(entries());
        let _ = overlay.visible_lines(80, 10);
        // Row 0 is the query prompt → no host.
        assert!(overlay.host_at_row(0, 10).is_none());
        // Rows 1..=3 map to the three loaded hosts in load order.
        assert_eq!(
            overlay
                .host_at_row(1, 10)
                .map(|(cursor, host)| (cursor, host.alias)),
            Some((0, "web1".to_owned()))
        );
        assert_eq!(
            overlay
                .host_at_row(3, 10)
                .map(|(cursor, host)| (cursor, host.alias)),
            Some((2, "remote".to_owned()))
        );
        // Past the last host → None.
        assert!(overlay.host_at_row(4, 10).is_none());
        // Selection is untouched by the read-only hit-test.
        assert_eq!(overlay.render_signature().selected, Some(0));
    }

    #[test]
    fn click_query_prompt_and_past_end_are_inert() {
        let mut overlay = open(entries());
        let _ = overlay.visible_lines(80, 10);
        assert!(!overlay.click_row(0, 10)); // query prompt
        assert!(!overlay.click_row(4, 10)); // past the last host
    }

    #[test]
    fn unparseable_no_match_query_emits_consumed_on_activate() {
        // A query that matches nothing AND does not parse as a destination (it
        // has an embedded space) keeps the old inert behavior: "No matches",
        // Activate is a no-op.
        let mut overlay = open(entries());
        type_query(&mut overlay, "bad host");
        assert_eq!(overlay.render_signature().results_len, 0);
        let lines = overlay.visible_lines(80, 10);
        assert!(lines[1].text.contains("No matches"));
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::Consumed
        );
    }

    #[test]
    fn adhoc_query_offers_connect_row_and_connects_on_activate() {
        // A query that matches no saved host but parses as user@host:port shows
        // a "Connect to: …" row plus a key hint, and Activate connects the
        // ad-hoc host (global remote defaults; no per-host overrides).
        let mut overlay = open(entries());
        type_query(&mut overlay, "deploy@host.example.invalid:2200");
        assert_eq!(overlay.render_signature().results_len, 0);
        let lines = overlay.visible_lines(80, 10);
        assert!(
            lines[1]
                .text
                .contains("Connect to: deploy@host.example.invalid:2200")
        );
        assert!(lines[2].text.contains("[Enter] connect"));
        assert!(lines[2].text.contains("[Shift+Enter] connect + save"));

        let ConnectionOverlayOutcome::Connect(host) = overlay.handle_input(OverlayInput::Activate)
        else {
            panic!("ad-hoc Activate must connect");
        };
        assert_eq!(host.alias, "host.example.invalid");
        assert_eq!(host.host_name, None);
        assert_eq!(host.user.as_deref(), Some("deploy"));
        assert_eq!(host.port, Some(2200));
        assert_eq!(host.integration, None, "no per-host override");
    }

    #[test]
    fn adhoc_shift_enter_and_ctrl_s_emit_connect_and_save() {
        // Shift+Enter (ActivateAlt) and Ctrl+S (Save) both connect AND save the
        // ad-hoc host.
        for input in [OverlayInput::ActivateAlt, OverlayInput::Save] {
            let mut overlay = open(entries());
            type_query(&mut overlay, "host.example.invalid");
            let ConnectionOverlayOutcome::ConnectAndSave(host) = overlay.handle_input(input) else {
                panic!("ad-hoc {input:?} must connect-and-save");
            };
            assert_eq!(host.alias, "host.example.invalid");
        }
    }

    #[test]
    fn adhoc_save_inputs_are_inert_on_a_saved_row() {
        // Shift+Enter / Ctrl+S over a real saved host is a no-op (already saved).
        for input in [OverlayInput::ActivateAlt, OverlayInput::Save] {
            let mut overlay = open(entries());
            assert_eq!(
                overlay.handle_input(input),
                ConnectionOverlayOutcome::Consumed
            );
        }
    }

    #[test]
    fn adhoc_connect_row_is_clickable() {
        // Clicking body row 1 (the "Connect to: …" row) connects, mirroring the
        // Enter path; the hint row and prompt row are inert.
        let mut overlay = open(entries());
        type_query(&mut overlay, "host.example.invalid");
        let _ = overlay.visible_lines(80, 10);
        assert!(!overlay.click_row(0, 10), "query prompt inert");
        assert!(overlay.click_row(1, 10), "connect row is clickable");
        assert!(!overlay.click_row(2, 10), "hint row inert");
    }

    // ── ODP-1B shared picker: bind purpose ─────────────────────────────────

    #[test]
    fn connect_purpose_profile_row_launches_named_profile() {
        let mut overlay = ConnectionOverlay::new();
        overlay.open_for_purpose(
            entries(),
            ConnectionPickerPurpose::Connect,
            vec![ConnectionProfileRow {
                name: "edge".to_owned(),
                label: "Edge SSH".to_owned(),
                connection: Some("edge".to_owned()),
            }],
        );
        let lines = overlay.visible_lines(80, 10);
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("Profile: Edge SSH"))
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::LaunchProfile("edge".to_owned())
        );
    }

    #[test]
    fn connect_purpose_profile_rows_filter_by_name() {
        let mut overlay = ConnectionOverlay::new();
        overlay.open_for_purpose(
            entries(),
            ConnectionPickerPurpose::Connect,
            vec![ConnectionProfileRow {
                name: "web-profile".to_owned(),
                label: "Web".to_owned(),
                connection: None,
            }],
        );
        type_query(&mut overlay, "web-profile");
        assert_eq!(overlay.render_signature().results_len, 1);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::LaunchProfile("web-profile".to_owned())
        );
    }

    #[test]
    fn non_connect_purpose_never_offers_profile_rows() {
        // v0.14 Phase A3 route inertness: the profile launch route belongs only
        // to the Connect picker. A bind/replace/connect-tab picker must drop any
        // supplied profile rows so it can never emit LaunchProfile: those
        // pickers act on a saved host only. Even when profile rows are handed
        // in, they must not appear and Activate must not produce a LaunchProfile.
        for purpose in [
            ConnectionPickerPurpose::BindWorkspace,
            ConnectionPickerPurpose::ReplaceTab(SessionToken(7)),
            ConnectionPickerPurpose::ConnectTabAfter(SessionToken(7)),
        ] {
            let mut overlay = ConnectionOverlay::new();
            overlay.open_for_purpose(
                entries(),
                purpose,
                vec![ConnectionProfileRow {
                    name: "edge".to_owned(),
                    label: "Edge SSH".to_owned(),
                    connection: Some("edge".to_owned()),
                }],
            );
            let lines = overlay.visible_lines(80, 10);
            assert!(
                !lines.iter().any(|line| line.text.contains("Profile:")),
                "a non-Connect picker must not render any profile row for {purpose:?}"
            );
            let outcome = overlay.handle_input(OverlayInput::Activate);
            assert!(
                !matches!(outcome, ConnectionOverlayOutcome::LaunchProfile(_)),
                "a non-Connect picker must never emit LaunchProfile for {purpose:?}"
            );
        }
    }

    fn open_for_bind(entries: Vec<ConnectionHost>) -> ConnectionOverlay {
        let mut overlay = ConnectionOverlay::new();
        overlay.open_for_purpose(entries, ConnectionPickerPurpose::BindWorkspace, Vec::new());
        overlay
    }

    #[test]
    fn bind_purpose_accept_emits_pick_not_connect() {
        // A tagged bind picker returns Pick(host, BindWorkspace) on accept, so
        // the App binds the workspace instead of spawning a connection.
        let mut overlay = open_for_bind(entries());
        overlay.handle_input(OverlayInput::Down); // select db-primary
        let ConnectionOverlayOutcome::Pick(host, purpose) =
            overlay.handle_input(OverlayInput::Activate)
        else {
            panic!("bind purpose must emit Pick");
        };
        assert_eq!(host.alias, "db-primary");
        assert_eq!(purpose, ConnectionPickerPurpose::BindWorkspace);
    }

    #[test]
    fn bind_purpose_suppresses_adhoc_row_and_save() {
        // A bind picker never offers the ad-hoc "Connect to: …" row (a workspace
        // binds by saved-host alias), and Shift+Enter / Ctrl+S are inert.
        let mut overlay = open_for_bind(entries());
        type_query(&mut overlay, "host.example.invalid");
        assert_eq!(overlay.render_signature().results_len, 0);
        let lines = overlay.visible_lines(80, 10);
        assert!(lines[1].text.contains("No matches"));
        assert!(!lines[1].text.contains("Connect to"));
        // No selectable row → Activate and the save inputs are all inert.
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::Consumed
        );
        for input in [OverlayInput::ActivateAlt, OverlayInput::Save] {
            assert_eq!(
                overlay.handle_input(input),
                ConnectionOverlayOutcome::Consumed
            );
        }
        assert!(!overlay.click_row(1, 10), "no ad-hoc row to click");
    }

    #[test]
    fn connect_tab_after_purpose_emits_pick_with_token() {
        // ODP-5D: the tab "Connect to host" picker returns Pick(host, purpose)
        // carrying the clicked tab's token, so the App opens the host in a new
        // tab adjacent to THAT tab. Ad-hoc rows are suppressed like every tagged
        // purpose.
        let token = SessionToken(7);
        let mut overlay = ConnectionOverlay::new();
        overlay.open_for_purpose(
            entries(),
            ConnectionPickerPurpose::ConnectTabAfter(token),
            Vec::new(),
        );
        type_query(&mut overlay, "host.example.invalid");
        assert_eq!(overlay.render_signature().results_len, 0);
        assert!(!overlay.visible_lines(80, 10)[1].text.contains("Connect to"));
        // A saved row still picks and routes with the token intact.
        overlay.open_for_purpose(
            entries(),
            ConnectionPickerPurpose::ConnectTabAfter(token),
            Vec::new(),
        );
        let ConnectionOverlayOutcome::Pick(host, purpose) =
            overlay.handle_input(OverlayInput::Activate)
        else {
            panic!("connect-tab-after purpose must emit Pick");
        };
        assert_eq!(host.alias, "web1");
        assert_eq!(purpose, ConnectionPickerPurpose::ConnectTabAfter(token));
    }

    #[test]
    fn replace_tab_purpose_emits_pick_with_token() {
        // ODP-5D: the "Replace this tab with" picker carries the target tab's
        // token so the App knows which tab to replace after the pick.
        let token = SessionToken(3);
        let mut overlay = ConnectionOverlay::new();
        overlay.open_for_purpose(
            entries(),
            ConnectionPickerPurpose::ReplaceTab(token),
            Vec::new(),
        );
        overlay.handle_input(OverlayInput::Down); // select db-primary
        let ConnectionOverlayOutcome::Pick(host, purpose) =
            overlay.handle_input(OverlayInput::Activate)
        else {
            panic!("replace-tab purpose must emit Pick");
        };
        assert_eq!(host.alias, "db-primary");
        assert_eq!(purpose, ConnectionPickerPurpose::ReplaceTab(token));
    }

    #[test]
    fn empty_overlay_shows_hint_and_activate_is_inert() {
        let mut overlay = open(Vec::new());
        assert_eq!(overlay.entry_count(), 0);
        let lines = overlay.visible_lines(80, 10);
        assert!(lines[1].text.contains("No saved connections"));
        // FORM-DISCOVERABILITY: even an empty manager shows the affordance
        // footer so a first-time user can reach the Add form.
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(joined.contains("Add connection"), "add-row visible");
        assert!(joined.contains("Tab add"), "key hint visible");
        // Nothing is selected and the add-row is not focused, so Enter is inert.
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            ConnectionOverlayOutcome::Consumed
        );
    }

    #[test]
    fn close_input_requests_close() {
        let mut overlay = open(entries());
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            ConnectionOverlayOutcome::Close
        );
    }

    #[test]
    fn selection_clamps_within_filtered_rows() {
        let mut overlay = open(entries());
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(overlay.render_signature().selected, Some(2));
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Up);
        }
        assert_eq!(overlay.render_signature().selected, Some(0));
    }

    #[test]
    fn visible_lines_are_bounded_by_body_height() {
        let overlay = open(entries());
        assert_eq!(overlay.visible_lines(80, 2).len(), 2);
        assert!(overlay.visible_lines(80, 10).len() <= 10);
    }

    #[test]
    fn row_label_includes_target_and_source() {
        let overlay = open(entries());
        let lines = overlay.visible_lines(120, 10);
        // web1 shows user@host:port and the OdyTTY source tag.
        assert!(
            lines[1]
                .text
                .contains("deploy@gateway.example.invalid:2222")
        );
        assert!(lines[1].text.contains("(OdyTTY)"));
        // remote shows the ssh-config source tag.
        assert!(lines[3].text.contains("(ssh-config)"));
    }

    #[test]
    fn control_chars_in_host_fields_are_sanitized() {
        let overlay = open(vec![host(
            "evil\u{1b}[31m",
            Some("h\u{7}ost"),
            None,
            None,
            ConnectionHostSource::Odytty,
        )]);
        let lines = overlay.visible_lines(120, 10);
        assert!(!lines[1].text.contains('\u{1b}'));
        assert!(!lines[1].text.contains('\u{7}'));
    }
}
