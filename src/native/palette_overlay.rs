// SPDX-License-Identifier: GPL-3.0-only
//! Native command-palette overlay state.
//!
//! The overlay is presentation state only: it owns a query, ranked row list,
//! and recent-directory cache, but it never writes to the PTY and never mutates
//! the terminal model. Accepting a row returns an outcome for the App to run
//! after the overlay closes.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::palette::{
    PaletteModel, PaletteOptions, PaletteSelection, PaletteSourceKind, SelectionWrap,
};
use crate::palette_catalog::compose_default_palette_entries;
use crate::palette_sources::{RecentDirs, read_history_for_shell};

use super::overlay::OverlayInput;

const MAX_RESULTS: usize = 40;

#[derive(Debug, Clone)]
pub(super) struct PaletteOverlay {
    model: PaletteModel,
    recent_dirs: RecentDirs,
    scroll_offset: Cell<usize>,
    last_body_height: Cell<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PaletteOverlayOutcome {
    Consumed,
    Close,
    TypeText(String),
    Action(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaletteOverlayLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaletteOverlaySignature {
    pub(super) query: String,
    pub(super) selected: Option<usize>,
    pub(super) results_len: usize,
    pub(super) results_fingerprint: u64,
}

impl Default for PaletteOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteOverlay {
    pub(super) fn new() -> Self {
        Self {
            model: PaletteModel::with_options(Vec::new(), palette_options()),
            recent_dirs: RecentDirs::default(),
            scroll_offset: Cell::new(0),
            last_body_height: Cell::new(0),
        }
    }

    pub(super) fn open_from_process_env(&mut self, cwd: Option<&str>) {
        let history = read_history_from_process_env();
        self.open_with_history_and_cwd(history, cwd);
    }

    #[cfg(test)]
    pub(super) fn open_for_test<H, S>(&mut self, history: H, cwd: Option<&str>)
    where
        H: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.open_with_history_and_cwd(history, cwd);
    }

    fn open_with_history_and_cwd<H, S>(&mut self, history: H, cwd: Option<&str>)
    where
        H: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.recent_dirs.observe_osc7_cwd(cwd);
        let directories = self.recent_dirs.candidates();
        let entries = compose_default_palette_entries(history, directories);
        self.model = PaletteModel::with_options(entries, palette_options());
        self.reset_scroll();
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> PaletteOverlayOutcome {
        match input {
            OverlayInput::Close => PaletteOverlayOutcome::Close,
            OverlayInput::Up => {
                self.model.select_previous();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Down => {
                self.model.select_next();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.model.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
                self.model.move_selection(MAX_RESULTS as isize);
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.model.backspace_query();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.model.push_query_char(ch);
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Char(_) => PaletteOverlayOutcome::Consumed,
            OverlayInput::Activate => match self.model.selected_selection() {
                Some(PaletteSelection::Action { id }) => PaletteOverlayOutcome::Action(id),
                Some(PaletteSelection::TypeText { text, .. }) => {
                    PaletteOverlayOutcome::TypeText(text)
                }
                None => PaletteOverlayOutcome::Consumed,
            },
            OverlayInput::Left | OverlayInput::Right | OverlayInput::Save | OverlayInput::Tab => {
                PaletteOverlayOutcome::Consumed
            }
        }
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<PaletteOverlayLine> {
        if body_height == 0 {
            self.last_body_height.set(0);
            self.scroll_offset.set(0);
            return Vec::new();
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2));
        lines.push(PaletteOverlayLine {
            text: truncate_for_width(&format!("> {}", self.model.query()), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        if self.model.results().is_empty() {
            self.scroll_offset.set(0);
            lines.push(PaletteOverlayLine {
                text: "No matches".to_owned(),
                focused: false,
                bold: false,
            });
            return lines;
        }
        let remaining = body_height - lines.len();
        for (visible_index, result) in self
            .model
            .results()
            .iter()
            .skip(scroll_offset)
            .take(remaining)
            .enumerate()
        {
            let index = scroll_offset + visible_index;
            let source = source_tag(result.kind);
            let label = sanitize_label(&result.label);
            lines.push(PaletteOverlayLine {
                text: truncate_for_width(&format!("{source}  {label}"), body_width),
                focused: self.model.selected_index() == Some(index),
                bold: result.kind == PaletteSourceKind::Action,
            });
        }
        lines
    }

    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        let visible_results = visible_result_rows(body_height);
        if visible_results == 0 || self.model.results().len() <= visible_results {
            self.scroll_offset.set(0);
            return (false, false);
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        (
            scroll_offset > 0,
            scroll_offset + visible_results < self.model.results().len(),
        )
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        columns.min(84)
    }

    pub(super) fn render_signature(&self) -> PaletteOverlaySignature {
        PaletteOverlaySignature {
            query: self.model.query().to_owned(),
            selected: self.model.selected_index(),
            results_len: self.model.results().len(),
            results_fingerprint: results_fingerprint(&self.model, self.scroll_offset.get()),
        }
    }

    fn reset_scroll(&self) {
        self.scroll_offset.set(0);
    }

    fn follow_selection_for_known_body_height(&self) {
        let body_height = self.last_body_height.get();
        if body_height > 0 {
            self.scroll_offset_for_body_height(body_height);
        }
    }

    fn scroll_offset_for_body_height(&self, body_height: usize) -> usize {
        self.last_body_height.set(body_height);
        let visible_results = visible_result_rows(body_height);
        let results_len = self.model.results().len();
        if visible_results == 0 || results_len <= visible_results {
            self.scroll_offset.set(0);
            return 0;
        }

        let max_scroll = results_len - visible_results;
        let mut scroll_offset = self.scroll_offset.get().min(max_scroll);
        if let Some(selected) = self.model.selected_index() {
            if selected < scroll_offset {
                scroll_offset = selected;
            } else if selected >= scroll_offset + visible_results {
                scroll_offset = selected + 1 - visible_results;
            }
        }
        self.scroll_offset.set(scroll_offset);
        scroll_offset
    }
}

fn palette_options() -> PaletteOptions {
    PaletteOptions {
        max_results: MAX_RESULTS,
        selection_wrap: SelectionWrap::Clamp,
    }
}

fn visible_result_rows(body_height: usize) -> usize {
    body_height.saturating_sub(1)
}

fn read_history_from_process_env() -> Vec<String> {
    let Some(shell) = env::var_os("SHELL").and_then(|value| value.into_string().ok()) else {
        return Vec::new();
    };
    let Some(home) = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let xdg_data_home = env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    read_history_for_shell(
        shell,
        home,
        xdg_config_home.as_deref(),
        xdg_data_home.as_deref(),
    )
}

fn source_tag(kind: PaletteSourceKind) -> &'static str {
    match kind {
        PaletteSourceKind::Action => "Action",
        PaletteSourceKind::History => "History",
        PaletteSourceKind::Directory => "Directory",
    }
}

fn results_fingerprint(model: &PaletteModel, scroll_offset: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    scroll_offset.hash(&mut hasher);
    for result in model.results().iter().take(MAX_RESULTS) {
        result.kind.hash(&mut hasher);
        result.label.hash(&mut hasher);
        result.entry_index.hash(&mut hasher);
    }
    hasher.finish()
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
}

fn truncate_for_width(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    for ch in text.chars().take(max_chars) {
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(history: &[&str], cwd: Option<&str>) -> PaletteOverlay {
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().copied(), cwd);
        overlay
    }

    fn type_query(overlay: &mut PaletteOverlay, query: &str) {
        for ch in query.chars() {
            assert_eq!(
                overlay.handle_input(OverlayInput::Char(ch)),
                PaletteOverlayOutcome::Consumed
            );
        }
    }

    fn command_history(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("qqq {index:02}")).collect()
    }

    #[test]
    fn fuzzy_query_ranks_history_candidate() {
        let mut overlay = open(&["git status", "cargo test"], Some("/workspace/service"));
        type_query(&mut overlay, "gst");

        let labels: Vec<_> = overlay
            .model
            .results()
            .iter()
            .map(|result| result.label.as_str())
            .collect();
        assert_eq!(labels.first().copied(), Some("git status"));
    }

    #[test]
    fn accept_history_types_text_without_newline() {
        let mut overlay = open(&["cargo test"], None);
        type_query(&mut overlay, "cargo");

        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            PaletteOverlayOutcome::TypeText("cargo test".to_owned())
        );
    }

    #[test]
    fn accept_action_returns_stable_action_id() {
        let mut overlay = open(&[], None);
        type_query(&mut overlay, "settings");

        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            PaletteOverlayOutcome::Action("settings".to_owned())
        );
    }

    #[test]
    fn recent_directories_survive_reopen() {
        let mut overlay = open(&[], Some("/work/one"));
        overlay.open_for_test(std::iter::empty::<&str>(), Some("/work/two"));
        type_query(&mut overlay, "/work");

        let labels: Vec<_> = overlay
            .model
            .results()
            .iter()
            .filter(|result| result.kind == PaletteSourceKind::Directory)
            .map(|result| result.label.as_str())
            .collect();
        assert_eq!(labels, vec!["/work/two", "/work/one"]);
    }

    #[test]
    fn visible_lines_are_bounded_by_body_height() {
        let overlay = open(&["one", "two", "three"], None);

        assert_eq!(overlay.visible_lines(80, 2).len(), 2);
    }

    #[test]
    fn visible_lines_follow_selection_when_body_overflows() {
        let history = command_history(10);
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().map(String::as_str), None);
        type_query(&mut overlay, "qqq");

        let before_lines = overlay.visible_lines(80, 4);
        let before_signature = overlay.render_signature();
        assert_eq!(before_lines[1].text, "History  qqq 00");
        assert!(before_lines[1].focused);

        for _ in 0..4 {
            assert_eq!(
                overlay.handle_input(OverlayInput::Down),
                PaletteOverlayOutcome::Consumed
            );
        }

        let after_lines = overlay.visible_lines(80, 4);
        let after_signature = overlay.render_signature();
        assert_ne!(after_lines, before_lines);
        assert_ne!(after_signature, before_signature);
        assert_ne!(
            after_signature.results_fingerprint,
            before_signature.results_fingerprint
        );
        assert_eq!(after_lines[1].text, "History  qqq 02");
        assert!(
            after_lines
                .iter()
                .any(|line| line.text == "History  qqq 04" && line.focused),
            "selected row must remain rendered after the view scrolls"
        );
    }

    #[test]
    fn scroll_indicator_is_inert_when_results_fit() {
        let history = command_history(3);
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().map(String::as_str), None);
        type_query(&mut overlay, "qqq");

        let before_signature = overlay.render_signature();
        let lines = overlay.visible_lines(80, 8);
        let after_signature = overlay.render_signature();

        assert_eq!(lines.len(), 4);
        assert_eq!(overlay.scroll_indicator(8), (false, false));
        assert_eq!(after_signature, before_signature);
    }

    #[test]
    fn scroll_indicator_reports_hidden_rows_after_selection_follow() {
        let history = command_history(10);
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().map(String::as_str), None);
        type_query(&mut overlay, "qqq");
        let _ = overlay.visible_lines(80, 4);

        for _ in 0..4 {
            assert_eq!(
                overlay.handle_input(OverlayInput::Down),
                PaletteOverlayOutcome::Consumed
            );
        }

        assert_eq!(overlay.scroll_indicator(4), (true, true));

        assert_eq!(
            overlay.handle_input(OverlayInput::End),
            PaletteOverlayOutcome::Consumed
        );

        assert_eq!(overlay.scroll_indicator(4), (true, false));
        assert_eq!(overlay.visible_lines(80, 4)[1].text, "History  qqq 07");
    }

    #[test]
    fn close_input_requests_close() {
        let mut overlay = open(&[], None);

        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            PaletteOverlayOutcome::Close
        );
    }
}
