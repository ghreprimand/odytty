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

/// Maximum rows rendered in the result list (keeps the overlay compact and the
/// fuzzy ranking bounded regardless of how large the hosts list is).
const MAX_RESULTS: usize = 40;

#[derive(Debug, Clone, Default)]
pub(super) struct ConnectionOverlay {
    /// The frozen connection list captured at open time, in load order
    /// (OdyTTY-owned hosts first, then any opt-in OpenSSH-config names).
    entries: Vec<ConnectionHost>,
    /// Current type-to-filter query.
    query: String,
    /// Indexes into `entries` for the rows that match `query`, best-first.
    /// Recomputed whenever the query changes.
    filtered: Vec<usize>,
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
        self.open_for_purpose(entries, ConnectionPickerPurpose::Connect);
    }

    /// Load a frozen candidate set for a tagged pending action (ODP-1B). The
    /// list/filter/cursor behavior is identical to [`Self::open`]; only the
    /// meaning of accept differs (see [`ConnectionPickerPurpose`]).
    pub(super) fn open_for_purpose(
        &mut self,
        entries: Vec<ConnectionHost>,
        purpose: ConnectionPickerPurpose,
    ) {
        self.entries = entries;
        self.query.clear();
        self.selected = 0;
        self.purpose = purpose;
        self.reset_scroll();
        self.recompute();
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
            self.filtered = (0..self.entries.len()).take(MAX_RESULTS).collect();
        } else {
            let haystacks: Vec<String> = self.entries.iter().map(match_text).collect();
            self.filtered = fuzzy::rank(&self.query, &haystacks)
                .into_iter()
                .take(MAX_RESULTS)
                .map(|(index, _)| index)
                .collect();
        }
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

    fn selected_entry(&self) -> Option<&ConnectionHost> {
        let entry_index = *self.filtered.get(self.selected)?;
        self.entries.get(entry_index)
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ConnectionOverlayOutcome {
        match input {
            OverlayInput::Close => ConnectionOverlayOutcome::Close,
            OverlayInput::Up => {
                self.move_selection(-1);
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::Down => {
                self.move_selection(1);
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                ConnectionOverlayOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
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
                // Connection manager: accept a saved row, else offer the ad-hoc
                // connect when the query is a well-formed destination.
                ConnectionPickerPurpose::Connect => match self.selected_entry() {
                    Some(entry) => ConnectionOverlayOutcome::Connect(Box::new(entry.clone())),
                    None => match self.adhoc_target() {
                        Some(host) => ConnectionOverlayOutcome::Connect(Box::new(host)),
                        None => ConnectionOverlayOutcome::Consumed,
                    },
                },
                // Tagged pending action: only a saved host is acceptable (a
                // workspace binds by saved-host alias), so an empty selection is
                // inert. The App routes the pick per the purpose.
                ConnectionPickerPurpose::BindWorkspace => match self.selected_entry() {
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
                if self.selected_entry().is_none()
                    && let Some(host) = self.adhoc_target()
                {
                    ConnectionOverlayOutcome::ConnectAndSave(Box::new(host))
                } else {
                    ConnectionOverlayOutcome::Consumed
                }
            }
            OverlayInput::Char(_)
            | OverlayInput::Left
            | OverlayInput::Right
            | OverlayInput::Tab => ConnectionOverlayOutcome::Consumed,
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
        let visible_results = body_height - 1;
        let within = row_in_body - 1;
        if within >= visible_results {
            return None;
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let cursor = scroll_offset + within;
        (cursor < self.filtered.len()).then_some(cursor)
    }

    /// Select the row under a left-click, reporting whether it landed on a
    /// selectable row so the caller can route the existing Activate. Parity with
    /// Down×N + Activate by construction.
    pub(super) fn click_row(&mut self, row_in_body: usize, body_height: usize) -> bool {
        // The synthetic ad-hoc "Connect to: …" row sits at body row 1 when the
        // filtered list is empty but the query parses; a click there connects
        // (the caller routes Activate, which the empty-selection path resolves
        // to the ad-hoc host). Saving still requires Shift+Enter / Ctrl+S.
        if body_height > 1
            && row_in_body == 1
            && self.filtered.is_empty()
            && self.adhoc_target().is_some()
        {
            return true;
        }
        match self.row_at(row_in_body, body_height) {
            Some(cursor) => {
                self.selected = cursor;
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
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2));
        lines.push(ConnectionOverlayLine {
            text: truncate_for_width(&format!("> {}", self.query), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        if self.entries.is_empty() {
            self.scroll_offset.set(0);
            lines.push(ConnectionOverlayLine {
                text: truncate_for_width(
                    "No saved connections — add hosts to hosts.conf or enable ssh_config_hosts.",
                    body_width,
                ),
                focused: false,
                bold: false,
            });
            return lines;
        }
        if self.filtered.is_empty() {
            self.scroll_offset.set(0);
            // When the query is a well-formed `[user@]host[:port]` that matches
            // no saved host, offer an ad-hoc connect row in place of "No
            // matches" — with a key hint so both actions are discoverable. A
            // bind picker (no ad-hoc) always shows the plain "No matches".
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
                    focused: true,
                    bold: false,
                });
                if lines.len() < body_height {
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
            return lines;
        }
        let remaining = body_height - lines.len();
        for (visible_index, &entry_index) in self
            .filtered
            .iter()
            .skip(scroll_offset)
            .take(remaining)
            .enumerate()
        {
            let row = scroll_offset + visible_index;
            let Some(entry) = self.entries.get(entry_index) else {
                continue;
            };
            lines.push(ConnectionOverlayLine {
                text: truncate_for_width(&row_label(entry), body_width),
                focused: row == self.selected,
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
        let visible_results = body_height.saturating_sub(1);
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
        let visible_results = body_height.saturating_sub(1);
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
        for &entry_index in self.filtered.iter().take(MAX_RESULTS) {
            if let Some(entry) = self.entries.get(entry_index) {
                entry.alias.hash(&mut hasher);
                entry.host_name.hash(&mut hasher);
                entry.user.hash(&mut hasher);
                entry.port.hash(&mut hasher);
            }
        }
        hasher.finish()
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

    fn open_for_bind(entries: Vec<ConnectionHost>) -> ConnectionOverlay {
        let mut overlay = ConnectionOverlay::new();
        overlay.open_for_purpose(entries, ConnectionPickerPurpose::BindWorkspace);
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
    fn empty_overlay_shows_hint_and_activate_is_inert() {
        let mut overlay = open(Vec::new());
        assert_eq!(overlay.entry_count(), 0);
        let lines = overlay.visible_lines(80, 10);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].text.contains("No saved connections"));
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
