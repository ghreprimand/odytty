// SPDX-License-Identifier: GPL-3.0-only
//! Pointer-driven interaction for the settings panel (UX4-P1): the shared
//! row walker that backs both the rendered lines and the cell→row→zone
//! hit-map, plus click dispatch and free wheel-scroll.
//!
//! Split out of `settings_panel/mod.rs` so the keyboard model and the
//! mouse/hit-test bulk stay under the source-size cap. These are `SettingsPanel`
//! methods in a child module, so they reach the parent's private fields and
//! helpers directly; methods the parent calls back into are `pub(super)`.
//!
//! Mouse is purely additive: every value change still terminates in the
//! existing `commit_value`/`apply_raw` seam — no new write path. Numeric rows
//! use discrete stepper buttons plus a click-to-type readout, both committing
//! through that same seam.

use super::SettingsLevel;
use super::path_picker::PathPickerOutcome;
use super::sections::SECTIONS;
use super::{RowEdit, SettingKind, SettingsPanel, SettingsPanelLine, SettingsPanelOutcome};
use super::{SettingInfo, ellipsize, setting_detail, wrap_words};
use crate::native::overlay::PointerButton;

const STEPPER_BUTTON_W: usize = 3;

fn center_text(text: &str, width: usize) -> String {
    let text_w = text.chars().count();
    if text_w >= width {
        return text.to_owned();
    }
    let pad = width - text_w;
    let left = pad / 2;
    let right = pad - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

/// The role of one rendered body line, used to dispatch a click. Produced in
/// lockstep with the rendered text by [`SettingsPanel::build_visible_rows`] so
/// the two views can never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum RowZone {
    /// A group label line — inert.
    GroupHeader,
    /// The `"name: value"` line — the primary action zone.
    Value,
    /// A numeric row rendered as a stepper: the down/up controls decrement or
    /// increment once per click, while the readout starts click-to-type edit.
    /// All columns are body-relative (0 = first body cell).
    Stepper {
        down_x0: usize,
        down_w: usize,
        readout_x0: usize,
        readout_w: usize,
        up_x0: usize,
        up_w: usize,
    },
    /// A wrapped help line — selects its owning row only, no value change.
    Detail,
    /// A `"! ..."` notice line — inert.
    Message,
    /// A section row in the Level-1 section list (SETTINGS-REDESIGN).
    /// `entry_index` carries the section index; a click drills into that
    /// section. This zone only appears in Level-1 hit-maps (T-level-hitmap).
    SectionRow,
    /// An About-view clickable project link (ABOUT). Carries the `'static` URL
    /// to open; a click routes through the allowlisted opener.
    AboutLink { url: &'static str },
    /// The About-view "Copy diagnostics" action row (ABOUT). A click copies the
    /// diagnostics block to the clipboard.
    AboutCopy,
}

/// One entry of the hit-map: which setting row a body line belongs to (if any)
/// and what role it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct RowHit {
    pub(in crate::native) entry_index: Option<usize>,
    pub(in crate::native) zone: RowZone,
}

impl SettingsPanel {
    /// The single source of truth for the panel body: walks the entries exactly
    /// once and emits each rendered line paired with its hit-map role. Both
    /// [`SettingsPanel::visible_lines`] and [`SettingsPanel::visible_hit_map`]
    /// project from this, guaranteeing the rendered geometry and the hit-test
    /// geometry are identical (T-level-hitmap).
    ///
    /// Dispatches on the current level:
    /// - Level 1 (SectionList): builds section-list rows with `SectionRow` zones.
    /// - Level 2 (SectionDetail) or search: builds setting-entry rows.
    /// - Path picker / close prompt: delegates to their own builders.
    pub(super) fn build_visible_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(SettingsPanelLine, RowHit)> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }
        // Path picker owns the body when active.
        if let Some(picker) = &self.path_picker {
            return picker.build_visible_rows(body_width, body_height);
        }
        // Dirty-close prompt owns the body when active.
        if self.pending_close_prompt {
            return self.build_close_prompt_rows(body_width, body_height);
        }
        // Level 2 (ABOUT): the read-only About view.
        if matches!(self.level, SettingsLevel::About) {
            return self.build_about_rows(body_width, body_height);
        }
        // Level 1 (section list) unless search mode is forcing the flat view.
        if matches!(self.level, SettingsLevel::SectionList) && !self.search_active {
            return self.build_section_list_rows(body_width, body_height);
        }
        // Level 2 / search: build the settings-entry rows.
        self.build_settings_rows(body_width, body_height)
    }

    /// Level-1 section-list rows. Each section maps to a `SectionRow` hit so
    /// pointer presses drill in correctly (T-level-hitmap).
    fn build_section_list_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(SettingsPanelLine, RowHit)> {
        let mut rows: Vec<(SettingsPanelLine, RowHit)> = Vec::new();
        for (index, section) in SECTIONS.iter().enumerate().skip(self.section_scroll) {
            if rows.len() >= body_height {
                break;
            }
            let count = self
                .all_entries
                .iter()
                .filter(|e| section.groups.contains(&e.group))
                .count();
            let focused = index == self.section_selected;
            let marker = if focused { ">" } else { " " };
            let count_str = format!("({count})");
            let name_w = section.name.chars().count();
            let available = body_width.saturating_sub(name_w + 4 + count_str.len());
            // Right-align the count with padding.
            let text = if available > 0 {
                format!(
                    "{marker} {}{:>pad$}",
                    section.name,
                    count_str,
                    pad = available + count_str.len()
                )
            } else {
                format!("{marker} {}  {count_str}", section.name)
            };
            rows.push((
                SettingsPanelLine {
                    text,
                    focused,
                    bold: focused,
                },
                RowHit {
                    entry_index: Some(index),
                    zone: RowZone::SectionRow,
                },
            ));
        }
        // Synthetic "About" row at logical index SECTIONS.len(), appended after
        // the real sections. Drilling into it opens the read-only About view.
        // It carries no entry count (informational, not setting-backed).
        let about_index = SECTIONS.len();
        if rows.len() < body_height && about_index >= self.section_scroll {
            let focused = about_index == self.section_selected;
            let marker = if focused { ">" } else { " " };
            rows.push((
                SettingsPanelLine {
                    text: format!("{marker} About"),
                    focused,
                    bold: focused,
                },
                RowHit {
                    entry_index: Some(about_index),
                    zone: RowZone::SectionRow,
                },
            ));
        }
        // Footer hint. Word-fitted to the body so a narrow window degrades to a
        // shorter whole-word hint instead of a mid-word cut ("Ctrl+S sav");
        // byte-identical on a normal window where the full hint fits.
        if rows.len() < body_height {
            rows.push((
                SettingsPanelLine {
                    text: crate::native::overlay::fit_hint_to_width(
                        "  Enter/\u{2192} open  / search  Ctrl+S save  Esc close",
                        body_width,
                    ),
                    focused: false,
                    bold: false,
                },
                RowHit {
                    entry_index: None,
                    zone: RowZone::GroupHeader,
                },
            ));
        }
        rows
    }

    /// Level-2 (ABOUT) read-only About view rows: inert info lines from
    /// `AboutInfo::info_lines`, then the focusable project links and the Copy
    /// diagnostics row, then any transient message. `self.selected` indexes the
    /// actionable rows (0..ABOUT_LINKS.len() = links, then the Copy row), and is
    /// reflected as the focused/bold line. The full list is windowed by
    /// `self.scroll` so a short window scrolls; hit-map and text stay in lockstep
    /// because both project from this one vector.
    fn build_about_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(SettingsPanelLine, RowHit)> {
        let inert = RowHit {
            entry_index: None,
            zone: RowZone::Detail,
        };
        let mut rows: Vec<(SettingsPanelLine, RowHit)> = Vec::new();

        // Informational block (inert). `None` shows a minimal placeholder.
        match &self.about {
            Some(about) => {
                for line in about.info_lines() {
                    rows.push((
                        SettingsPanelLine {
                            text: if line.is_empty() {
                                String::new()
                            } else {
                                format!("  {line}")
                            },
                            focused: false,
                            bold: false,
                        },
                        inert,
                    ));
                }
            }
            None => rows.push((
                SettingsPanelLine {
                    text: "  About information unavailable.".to_owned(),
                    focused: false,
                    bold: false,
                },
                inert,
            )),
        }

        // Separator + actionable rows: project links, then Copy diagnostics.
        rows.push((
            SettingsPanelLine {
                text: String::new(),
                focused: false,
                bold: false,
            },
            inert,
        ));
        for (i, link) in super::ABOUT_LINKS.iter().enumerate() {
            let focused = self.selected == i;
            let marker = if focused { ">" } else { " " };
            rows.push((
                SettingsPanelLine {
                    text: format!("{marker} {}: {}", link.label, link.url),
                    focused,
                    bold: focused,
                },
                RowHit {
                    entry_index: None,
                    zone: RowZone::AboutLink { url: link.url },
                },
            ));
        }
        let copy_focused = self.selected == super::ABOUT_COPY_ROW;
        let copy_marker = if copy_focused { ">" } else { " " };
        rows.push((
            SettingsPanelLine {
                text: format!("{copy_marker} [ Copy diagnostics ]"),
                focused: copy_focused,
                bold: copy_focused,
            },
            RowHit {
                entry_index: None,
                zone: RowZone::AboutCopy,
            },
        ));

        // Transient message (e.g. "Diagnostics copied").
        if let Some(message) = &self.message {
            rows.push((
                SettingsPanelLine {
                    text: format!("  {message}"),
                    focused: false,
                    bold: false,
                },
                RowHit {
                    entry_index: None,
                    zone: RowZone::Message,
                },
            ));
        }

        // Footer hint.
        rows.push((
            SettingsPanelLine {
                text: crate::native::overlay::fit_hint_to_width(
                    "  \u{2191}\u{2193} move  Enter open/copy  Esc back",
                    body_width,
                ),
                focused: false,
                bold: false,
            },
            inert,
        ));

        // Window to the visible height by `self.scroll` so a short overlay can
        // scroll through the (normally short) About body.
        let start = self.scroll.min(rows.len().saturating_sub(1));
        let end = (start + body_height).min(rows.len());
        rows[start..end].to_vec()
    }

    /// Dirty-close prompt body rows.
    fn build_close_prompt_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(SettingsPanelLine, RowHit)> {
        let count = self.edits.changed_count();
        let inert = RowHit {
            entry_index: None,
            zone: RowZone::Message,
        };
        let _ = body_width; // layout is fixed text, width only clips
        let mut rows = vec![
            (
                SettingsPanelLine {
                    text: format!("  You have {count} unsaved setting change(s)."),
                    focused: false,
                    bold: false,
                },
                inert,
            ),
            (
                SettingsPanelLine {
                    text: String::new(),
                    focused: false,
                    bold: false,
                },
                inert,
            ),
        ];
        let actions = [
            "  [S] Save and close",
            "  [D] Discard and close",
            "  [C] Cancel (return to settings)",
        ];
        for text in actions {
            if rows.len() >= body_height {
                break;
            }
            rows.push((
                SettingsPanelLine {
                    text: text.to_owned(),
                    focused: false,
                    bold: false,
                },
                inert,
            ));
        }
        rows
    }

    /// Level-2 (and search-mode) entry rows. This is the body of the former
    /// `build_visible_rows`; renamed so the dispatcher above can call it
    /// explicitly from `selected_in_window` without re-entering the path-picker
    /// and close-prompt branches.
    pub(super) fn build_settings_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(SettingsPanelLine, RowHit)> {
        let mut rows: Vec<(SettingsPanelLine, RowHit)> = Vec::new();
        if body_width == 0 || body_height == 0 {
            return rows;
        }

        // OB-SEARCH: when searching, a fixed filter header sits above the results.
        if self.search_active {
            rows.push((
                SettingsPanelLine {
                    text: format!("  Search: {}|", self.query),
                    focused: true,
                    bold: false,
                },
                RowHit {
                    entry_index: None,
                    zone: RowZone::GroupHeader,
                },
            ));
        }

        let mut current_group = "";
        for (index, entry) in self.entries.iter().enumerate().skip(self.scroll) {
            if rows.len() >= body_height {
                break;
            }
            if entry.group != current_group {
                current_group = entry.group;
                rows.push((
                    SettingsPanelLine {
                        text: format!("  {current_group}"),
                        focused: false,
                        bold: false,
                    },
                    RowHit {
                        entry_index: None,
                        zone: RowZone::GroupHeader,
                    },
                ));
                if rows.len() >= body_height {
                    break;
                }
            }

            let focused = index == self.selected;
            let marker = if focused { ">" } else { " " };
            let editing_this = self
                .editing
                .as_ref()
                .is_some_and(|edit| edit.key == entry.key);
            // Numeric rows render as discrete steppers unless they are being
            // text-edited (then the edit buffer shows in a plain value line) or
            // the panel is too narrow (graceful fallback to click-to-type).
            let stepper = if entry.kind == SettingKind::Number && !editing_this {
                self.stepper_line(entry, marker, body_width)
            } else {
                None
            };
            let (text, zone) = if let Some((text, zone)) = stepper {
                (text, zone)
            } else {
                let mut value = self.display_value(entry);
                let max_value = body_width.saturating_sub(entry.name.chars().count() + 6);
                if value.chars().count() > max_value {
                    value = ellipsize(&value, max_value);
                }
                (format!("{marker} {}: {value}", entry.name), RowZone::Value)
            };
            rows.push((
                SettingsPanelLine {
                    text,
                    focused,
                    bold: true,
                },
                RowHit {
                    entry_index: Some(index),
                    zone,
                },
            ));
            if rows.len() >= body_height {
                break;
            }

            let detail = setting_detail(entry);
            for wrapped in wrap_words(&detail, body_width.saturating_sub(4)) {
                if rows.len() >= body_height {
                    break;
                }
                rows.push((
                    SettingsPanelLine {
                        text: format!("    {wrapped}"),
                        focused: false,
                        bold: false,
                    },
                    RowHit {
                        entry_index: Some(index),
                        zone: RowZone::Detail,
                    },
                ));
            }
            if focused && let Some(message) = self.message.as_deref() {
                for wrapped in wrap_words(message, body_width.saturating_sub(4)) {
                    if rows.len() >= body_height {
                        break;
                    }
                    rows.push((
                        SettingsPanelLine {
                            text: format!("    ! {wrapped}"),
                            focused: false,
                            bold: false,
                        },
                        RowHit {
                            entry_index: Some(index),
                            zone: RowZone::Message,
                        },
                    ));
                }
            }
        }

        // OB-SEARCH: a query that matches nothing shows an explicit notice rather
        // than an empty body, and never closes the overlay (R3).
        if self.search_active && self.entries.is_empty() && rows.len() < body_height {
            rows.push((
                SettingsPanelLine {
                    text: format!("  No settings match \"{}\".", self.query),
                    focused: false,
                    bold: false,
                },
                RowHit {
                    entry_index: None,
                    zone: RowZone::Message,
                },
            ));
        }

        rows
    }

    /// Render a numeric row as a stepper: `"{marker} {name}: [<] {value} [>]"`.
    /// Returns `None` (caller falls back to a plain click-to-type value line)
    /// when the row has no [`crate::settings::NumericSpec`] or the panel is too
    /// narrow for both buttons plus the readout.
    fn stepper_line(
        &self,
        entry: &SettingInfo,
        marker: &str,
        body_width: usize,
    ) -> Option<(String, RowZone)> {
        let spec = entry.numeric?;
        let prefix = format!("{marker} {}: ", entry.name);
        let prefix_w = prefix.chars().count();
        let readout = self.display_value(entry).replace(" *", "*");
        let readout_w = spec.readout_width().max(readout.chars().count());
        let total_w = STEPPER_BUTTON_W + 1 + readout_w + 1 + STEPPER_BUTTON_W;
        if body_width.checked_sub(prefix_w)? < total_w {
            return None;
        }
        let down_x0 = prefix_w;
        let readout_x0 = down_x0 + STEPPER_BUTTON_W + 1;
        let up_x0 = readout_x0 + readout_w + 1;
        let padded_readout = center_text(&readout, readout_w);

        Some((
            format!("{prefix}[<] {padded_readout} [>]"),
            RowZone::Stepper {
                down_x0,
                down_w: STEPPER_BUTTON_W,
                readout_x0,
                readout_w,
                up_x0,
                up_w: STEPPER_BUTTON_W,
            },
        ))
    }

    /// Test seam: the body-row offset and button geometry of the first visible
    /// stepper, so the overlay/App layers can drive real clicks without
    /// widening `build_visible_rows`.
    #[cfg(test)]
    pub(in crate::native) fn first_stepper_zone_for_test(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Option<(usize, usize, usize)> {
        self.build_visible_rows(body_width, body_height)
            .into_iter()
            .enumerate()
            .find_map(|(row, (_, hit))| match hit.zone {
                RowZone::Stepper { down_x0, up_x0, .. } => Some((row, down_x0, up_x0)),
                _ => None,
            })
    }

    /// The cell→row hit-map for the current body geometry, aligned 1:1 with
    /// [`SettingsPanel::visible_lines`] (index = body row offset from the first
    /// body cell).
    pub(super) fn visible_hit_map(&self, body_width: usize, body_height: usize) -> Vec<RowHit> {
        self.build_visible_rows(body_width, body_height)
            .into_iter()
            .map(|(_, hit)| hit)
            .collect()
    }

    /// Free pointer-driven scroll: move the viewport by `delta` rows (positive =
    /// later entries) without moving `selected`. This is independent of the
    /// keyboard `clamp()` keep-selection-visible logic; the next keyboard
    /// navigation will re-clamp scroll to the selection as before.
    /// Free pointer-driven scroll. At Level 1 (section list) scrolls
    /// `section_scroll`; at Level 2 or in search mode scrolls `scroll`.
    /// (T-scroll-per-level: the two offsets are independent.)
    pub(in crate::native) fn scroll_lines(&mut self, delta: isize) {
        // Path picker owns wheel scroll when active.
        if let Some(picker) = self.path_picker.as_mut() {
            picker.scroll_lines(delta);
            return;
        }
        if matches!(self.level, SettingsLevel::SectionList) && !self.search_active {
            // Level 1: scroll the section list.
            let max = SECTIONS.len().saturating_sub(1) as isize;
            self.section_scroll = (self.section_scroll as isize + delta).clamp(0, max) as usize;
            return;
        }
        // Level 2 / search: scroll the entry list.
        if self.entries.is_empty() {
            self.scroll = 0;
            return;
        }
        let max = self.entries.len().saturating_sub(1) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max) as usize;
    }

    /// Handle a left/right press inside the panel body. `row_in_body` /
    /// `col_in_body` are 0-based offsets from the first body cell
    /// (`cell.row - rect.body_top`, `cell.column - rect.body_left`). A click
    /// first focuses the owning row, then acts per zone / setting kind. All
    /// value changes funnel through the existing `commit_value` seam.
    pub(in crate::native) fn handle_pointer_press(
        &mut self,
        body_width: usize,
        body_height: usize,
        row_in_body: usize,
        col_in_body: usize,
        button: PointerButton,
        _x_in_body: Option<f32>,
    ) -> SettingsPanelOutcome {
        let hit_map = self.visible_hit_map(body_width, body_height);
        let Some(hit) = hit_map.get(row_in_body).copied() else {
            return SettingsPanelOutcome::Consumed;
        };

        if self.path_picker.is_some() {
            return self.handle_path_picker_pointer(hit, button);
        }

        // SectionRow: drill into the clicked section (Level 1 only).
        // T-level-hitmap: this zone only appears in Level-1 hit-maps.
        if hit.zone == RowZone::SectionRow {
            if let Some(section_index) = hit.entry_index {
                self.section_selected = section_index;
                self.drill_into_section(section_index);
            }
            return SettingsPanelOutcome::Consumed;
        }

        // About view actionable rows (ABOUT). These carry `entry_index: None`,
        // so they are handled before the entry guard below. A click focuses the
        // row (so keyboard and mouse focus agree) and acts.
        match hit.zone {
            RowZone::AboutLink { url } => {
                if let Some(i) = super::ABOUT_LINKS.iter().position(|l| l.url == url) {
                    self.selected = i;
                }
                if let Some(link) = super::ABOUT_LINKS.iter().find(|l| l.url == url) {
                    self.message = Some(format!("Opening {}.", link.label));
                }
                return SettingsPanelOutcome::OpenUrl(url.to_owned());
            }
            RowZone::AboutCopy => {
                self.selected = super::ABOUT_COPY_ROW;
                let text = self
                    .about
                    .as_ref()
                    .map(super::AboutInfo::diagnostics_block)
                    .unwrap_or_default();
                self.message = Some("Diagnostics copied to clipboard.".to_owned());
                return SettingsPanelOutcome::CopyToClipboard(text);
            }
            _ => {}
        }

        let Some(entry_index) = hit.entry_index else {
            // GroupHeader / Message: inert, no focus change.
            return SettingsPanelOutcome::Consumed;
        };

        // A click anywhere on a real row cancels any in-progress text edit
        // (clicking away is the mouse analogue of Esc), then focuses that row.
        self.editing = None;
        self.set_selection(entry_index);

        match hit.zone {
            RowZone::SectionRow => SettingsPanelOutcome::Consumed, // handled above
            // About zones carry `entry_index: None` and are handled before the
            // entry guard above; unreachable here but kept for exhaustiveness.
            RowZone::AboutLink { .. } | RowZone::AboutCopy => SettingsPanelOutcome::Consumed,
            RowZone::GroupHeader | RowZone::Message => SettingsPanelOutcome::Consumed,
            // A help line only selects its owning row; no value change.
            RowZone::Detail => SettingsPanelOutcome::Consumed,
            RowZone::Value => self.click_action_on_selected(button),
            RowZone::Stepper {
                down_x0,
                down_w,
                readout_x0,
                readout_w,
                up_x0,
                up_w,
            } => self.stepper_press(
                entry_index,
                col_in_body,
                button,
                down_x0,
                down_w,
                readout_x0,
                readout_w,
                up_x0,
                up_w,
            ),
        }
    }

    /// Dispatch a press that landed on a numeric stepper row: a press on `[<]`
    /// decrements once, `[>]` increments once, the readout starts click-to-type
    /// edit, and elsewhere on the row only focuses.
    #[allow(clippy::too_many_arguments)]
    fn stepper_press(
        &mut self,
        entry_index: usize,
        col_in_body: usize,
        button: PointerButton,
        down_x0: usize,
        down_w: usize,
        readout_x0: usize,
        readout_w: usize,
        up_x0: usize,
        up_w: usize,
    ) -> SettingsPanelOutcome {
        if button == PointerButton::Right {
            return SettingsPanelOutcome::Consumed;
        }
        let Some(entry) = self.entries.get(entry_index) else {
            return SettingsPanelOutcome::Consumed;
        };
        let reloadable = entry.reloadable;
        if !reloadable {
            self.message = Some("Startup-only setting; edit odytty.conf and restart.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }

        if col_in_body >= down_x0 && col_in_body < down_x0 + down_w {
            self.step_numeric_entry(entry_index, -1)
        } else if col_in_body >= up_x0 && col_in_body < up_x0 + up_w {
            self.step_numeric_entry(entry_index, 1)
        } else if col_in_body >= readout_x0 && col_in_body < readout_x0 + readout_w {
            self.start_numeric_edit(entry_index)
        } else {
            // The label/prefix area: focus only.
            SettingsPanelOutcome::Consumed
        }
    }

    fn step_numeric_entry(&mut self, entry_index: usize, direction: isize) -> SettingsPanelOutcome {
        let Some(entry) = self.entries.get(entry_index).cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        let Some(spec) = entry.numeric else {
            return SettingsPanelOutcome::Consumed;
        };
        let parsed = entry.value.parse::<f32>().unwrap_or_else(|_| {
            if entry.key == "background_image_scrim" && direction < 0 {
                1.0
            } else {
                0.0
            }
        });
        let next = spec.snap(parsed + spec.step * direction as f32);
        self.commit_value(entry.key, &format!("{next:.3}"))
    }

    /// Settings steppers do not drag. Pointer moves are ignored so a stale
    /// native motion stream cannot keep editing values after the click.
    pub(in crate::native) fn handle_pointer_drag(
        &mut self,
        _body_width: usize,
        _body_height: usize,
        _col_in_body: usize,
        _x_in_body: Option<f32>,
    ) -> SettingsPanelOutcome {
        SettingsPanelOutcome::Consumed
    }

    /// Kept for the shared overlay close/release path. Settings steppers do not
    /// hold drag state.
    pub(in crate::native) fn end_slider_drag(&mut self) {}

    /// Begin a click-to-type numeric edit through the same `RowEdit` path the
    /// keyboard uses (Enter applies, Esc cancels), so the parser/clamp is shared.
    fn start_numeric_edit(&mut self, entry_index: usize) -> SettingsPanelOutcome {
        let Some(entry) = self.entries.get(entry_index) else {
            return SettingsPanelOutcome::Consumed;
        };
        let key = entry.key;
        let value = entry.value.clone();
        self.editing = Some(RowEdit { key, buffer: value });
        self.message = Some("Editing: type a value, Enter applies, Esc cancels.".to_owned());
        SettingsPanelOutcome::Consumed
    }

    /// Act on a click on the focused row's value, mirroring `activate_selected`
    /// (Bool toggle, theme→picker, Enum cycle, text/number→edit) with the one
    /// pointer addition: a right-click cycles an Enum backward.
    fn click_action_on_selected(&mut self, button: PointerButton) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        if !entry.reloadable {
            self.message = Some("Startup-only setting; edit odytty.conf and restart.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }
        // The synthetic "Open Theme Builder" action row opens the builder
        // directly on click (v0.3.1 discoverability), mirroring `activate_selected`.
        if entry.key == super::THEME_BUILDER_ACTION_KEY {
            self.message = Some("Opening theme builder.".to_owned());
            return SettingsPanelOutcome::OpenThemeBuilder;
        }
        match entry.kind {
            // Key-specific overrides run before kind dispatch (theme is Enum,
            // font_family is String — both open pickers not editors).
            _ if entry.key == "theme" => {
                self.message = Some("Opening built-in theme picker.".to_owned());
                SettingsPanelOutcome::OpenThemePicker
            }
            _ if entry.key == "font_family" => {
                self.message = Some("Opening font picker.".to_owned());
                SettingsPanelOutcome::OpenFontPicker
            }
            SettingKind::Bool => {
                let next = if entry.value == "on" { "off" } else { "on" };
                self.commit_value(entry.key, next)
            }
            SettingKind::Enum => {
                let direction = if button == PointerButton::Right {
                    -1
                } else {
                    1
                };
                self.cycle_selected(direction)
            }
            // Path rows open the inline path picker.
            SettingKind::Path => {
                let original = entry.value.clone();
                let start_dir = super::path_picker::resolve_start_dir(&original);
                self.editing = None;
                self.path_picker = Some(super::path_picker::PathPickerState::new(
                    entry.key, start_dir, original,
                ));
                SettingsPanelOutcome::Consumed
            }
            // The `keybinds` row opens the remap editor on click, exactly like
            // the keyboard `activate_selected` (mod.rs) does — without this arm
            // it fell through to the generic List branch below and popped a
            // useless "type a value" prompt, leaving the editor mouse-unreachable
            // (P1-5/P1-6). Must run before the generic List arm.
            SettingKind::List if entry.key == "keybinds" => {
                self.message = Some("Opening keybinding editor.".to_owned());
                SettingsPanelOutcome::OpenKeyBindings
            }
            SettingKind::Number | SettingKind::String | SettingKind::List => {
                self.editing = Some(RowEdit {
                    key: entry.key,
                    buffer: entry.value,
                });
                self.message =
                    Some("Editing: type a value, Enter applies, Esc cancels.".to_owned());
                SettingsPanelOutcome::Consumed
            }
        }
    }

    fn handle_path_picker_pointer(
        &mut self,
        hit: RowHit,
        button: PointerButton,
    ) -> SettingsPanelOutcome {
        let Some(mut picker) = self.path_picker.take() else {
            return SettingsPanelOutcome::Consumed;
        };
        if button == PointerButton::Right {
            self.path_picker = Some(picker);
            return SettingsPanelOutcome::Consumed;
        }
        let Some(entry_index) = hit.entry_index else {
            self.path_picker = Some(picker);
            return SettingsPanelOutcome::Consumed;
        };

        let key = picker.key;
        match picker.activate_index(entry_index) {
            PathPickerOutcome::Selected(path_str) => {
                self.path_picker = None;
                self.commit_value(key, &path_str)
            }
            PathPickerOutcome::Cancelled => {
                self.path_picker = None;
                self.message = Some(format!("Cancelled path selection for {key}."));
                SettingsPanelOutcome::Consumed
            }
            PathPickerOutcome::Consumed => {
                self.path_picker = Some(picker);
                SettingsPanelOutcome::Consumed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn panel() -> SettingsPanel {
        let mut p = SettingsPanel::new(&Settings::default());
        // Use flat-mode so pointer tests see all entries without needing to
        // navigate the two-level section list first (T-level-hitmap fixture).
        p.set_test_flat_mode();
        p
    }

    /// Geometry generous enough that every row is visible.
    const W: usize = 96;
    const H: usize = 400;

    fn entry_index(panel: &SettingsPanel, key: &str) -> usize {
        panel
            .entries
            .iter()
            .position(|entry| entry.key == key)
            .expect("known key")
    }

    /// The body row offset of the line whose hit maps to `key`'s value.
    fn value_row(panel: &SettingsPanel, key: &str) -> usize {
        let want = entry_index(panel, key);
        panel
            .build_visible_rows(W, H)
            .iter()
            .position(|(_, hit)| hit.entry_index == Some(want) && hit.zone == RowZone::Value)
            .expect("value row present")
    }

    /// The body row offset and `Stepper` zone for `key` (numeric rows).
    fn stepper_row(panel: &SettingsPanel, key: &str) -> (usize, RowZone) {
        let want = entry_index(panel, key);
        panel
            .build_visible_rows(W, H)
            .iter()
            .enumerate()
            .find_map(|(row, (_, hit))| {
                matches!(hit.zone, RowZone::Stepper { .. })
                    .then_some(())
                    .filter(|_| hit.entry_index == Some(want))
                    .map(|_| (row, hit.zone))
            })
            .expect("stepper row present")
    }

    #[test]
    fn level1_footer_word_fits_on_a_narrow_panel() {
        // OVERLAY-SMALL-WINDOW: the Level-1 section-list footer must degrade to
        // a whole-word hint on a narrow panel, never a mid-word cut.
        const FULL: &str = "  Enter/\u{2192} open  / search  Ctrl+S save  Esc close";
        // Default panel is Level 1 (section list), so the footer hint is built.
        let p = SettingsPanel::new(&Settings::default());

        // Wide body: byte-identical to the full footer (large-window guard).
        let wide = p.build_visible_rows(W, H);
        assert_eq!(
            wide.last().expect("rows present").0.text,
            FULL,
            "wide panel shows the full footer unchanged"
        );

        // Narrow body: footer fits and breaks only on word boundaries. 21 is
        // chosen because a naive char cut at 21 lands mid-word ("…/ sea"),
        // so this width genuinely distinguishes the word-fit from a char cut.
        let narrow = 21;
        let rows = p.build_visible_rows(narrow, H);
        let footer = &rows.last().expect("rows present").0.text;
        assert!(
            footer.chars().count() <= narrow,
            "footer must fit the narrow body: {footer:?}"
        );
        assert!(footer.len() < FULL.len(), "footer really was shortened");
        for (got, want) in footer.split(' ').zip(FULL.split(' ')) {
            assert_eq!(got, want, "footer trimmed on a word boundary, not mid-word");
        }
    }

    #[test]
    fn hit_map_aligns_one_to_one_with_visible_lines() {
        let p = panel();
        let lines = p.visible_lines(W, H);
        let hits = p.visible_hit_map(W, H);
        assert_eq!(
            lines.len(),
            hits.len(),
            "lines and hit-map must be lockstep"
        );
        for (line, hit) in lines.iter().zip(hits.iter()) {
            if hit.zone == RowZone::GroupHeader {
                assert!(hit.entry_index.is_none());
            }
            if hit.zone == RowZone::Value {
                assert!(line.text.contains(':'), "value line carries 'name: value'");
            }
        }
    }

    #[test]
    fn clicking_a_bool_value_row_toggles_via_commit_seam() {
        let mut p = panel();
        let row = value_row(&p, "synthetic_styles");
        let SettingsPanelOutcome::Apply(settings) =
            p.handle_pointer_press(W, H, row, 0, PointerButton::Left, None)
        else {
            panic!("bool click should apply");
        };
        assert!(!settings.synthetic_styles);
        assert_eq!(p.render_signature().changed_count, 1);
    }

    #[test]
    fn clicking_the_theme_value_row_opens_the_picker() {
        let mut p = panel();
        let row = value_row(&p, "theme");
        assert_eq!(
            p.handle_pointer_press(W, H, row, 0, PointerButton::Left, None),
            SettingsPanelOutcome::OpenThemePicker
        );
    }

    #[test]
    fn clicking_the_keybinds_value_row_opens_the_editor() {
        // P1-5/P1-6: the keybinds row (a List kind) must open the remap editor on
        // CLICK, exactly like the keyboard `activate_selected`. Before the fix the
        // click path fell through to the generic List arm and popped a useless
        // "type a value" prompt, leaving the editor mouse-unreachable.
        let mut p = panel();
        let row = value_row(&p, "keybinds");
        assert_eq!(
            p.handle_pointer_press(W, H, row, 0, PointerButton::Left, None),
            SettingsPanelOutcome::OpenKeyBindings,
            "click on keybinds opens the editor (parity with Enter)"
        );
        assert!(
            p.editing.is_none(),
            "click must NOT start an inline text edit on the keybinds row"
        );
    }

    #[test]
    fn left_and_right_click_cycle_an_enum_in_opposite_directions() {
        let row = value_row(&panel(), "subpixel");
        let mut fwd_panel = panel();
        let SettingsPanelOutcome::Apply(fwd) =
            fwd_panel.handle_pointer_press(W, H, row, 0, PointerButton::Left, None)
        else {
            panic!("enum left-click cycles forward");
        };
        let mut back_panel = panel();
        let SettingsPanelOutcome::Apply(back) =
            back_panel.handle_pointer_press(W, H, row, 0, PointerButton::Right, None)
        else {
            panic!("enum right-click cycles backward");
        };
        assert_ne!(
            fwd.subpixel, back.subpixel,
            "forward and backward land on different values"
        );
    }

    #[test]
    fn numeric_rows_render_as_a_stepper() {
        let p = panel();
        let (_, zone) = stepper_row(&p, "font_size");
        let RowZone::Stepper {
            down_x0,
            down_w,
            readout_x0,
            up_x0,
            up_w,
            ..
        } = zone
        else {
            panic!("font_size is a stepper row");
        };
        let rows = p.build_visible_rows(W, H);
        let line = &rows
            .iter()
            .find(|(_, hit)| matches!(hit.zone, RowZone::Stepper { .. }))
            .unwrap()
            .0
            .text;
        assert!(line.contains("[<]"), "down button visible: {line:?}");
        assert!(line.contains("[>]"), "up button visible: {line:?}");
        assert_eq!(down_w, STEPPER_BUTTON_W);
        assert_eq!(up_w, STEPPER_BUTTON_W);
        assert!(down_x0 < readout_x0 && readout_x0 < up_x0);
    }

    #[test]
    fn stepper_reserves_dirty_marker_column() {
        let mut p = panel();
        let clean_line = p
            .build_visible_rows(W, H)
            .into_iter()
            .find(|(line, _)| line.text.contains("Font size:"))
            .expect("font size row present")
            .0
            .text;
        let (row, zone) = stepper_row(&p, "font_size");
        let RowZone::Stepper { up_x0, .. } = zone else {
            unreachable!()
        };
        let SettingsPanelOutcome::Apply(_) =
            p.handle_pointer_press(W, H, row, up_x0, PointerButton::Left, None)
        else {
            panic!("stepper click applies a value");
        };
        let dirty_line = p
            .build_visible_rows(W, H)
            .into_iter()
            .find(|(line, _)| line.text.contains("Font size:"))
            .expect("font size row present")
            .0
            .text;

        let clean_up = clean_line.find("[>]").expect("clean up button visible");
        let dirty_up = dirty_line.find("[>]").expect("dirty up button visible");
        assert_eq!(
            clean_up, dirty_up,
            "dirty marker must not shift controls: clean={clean_line:?} dirty={dirty_line:?}"
        );
        let default_font_size = crate::settings::DEFAULT_FONT_SIZE_PX as u32;
        let stepped_font_size = default_font_size + 1;
        let clean_expected = format!("[<]  {default_font_size}  [>]");
        let dirty_expected = format!("[<] {stepped_font_size}*  [>]");
        assert!(
            clean_line.contains(&clean_expected),
            "clean centered: {clean_line:?}"
        );
        assert!(
            dirty_line.contains(&dirty_expected),
            "dirty centered: {dirty_line:?}"
        );
    }

    #[test]
    fn clicking_the_stepper_readout_starts_a_text_edit() {
        let mut p = panel();
        let (row, zone) = stepper_row(&p, "font_size");
        let RowZone::Stepper { readout_x0, .. } = zone else {
            unreachable!()
        };
        assert_eq!(
            p.handle_pointer_press(W, H, row, readout_x0, PointerButton::Left, None),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(p.render_signature().editing_key, Some("font_size"));
    }

    #[test]
    fn clicking_stepper_up_increments_once_without_dragging() {
        let mut p = panel();
        let (row, zone) = stepper_row(&p, "font_size");
        let RowZone::Stepper { up_x0, .. } = zone else {
            unreachable!()
        };
        let SettingsPanelOutcome::Apply(settings) =
            p.handle_pointer_press(W, H, row, up_x0, PointerButton::Left, None)
        else {
            panic!("up click applies once");
        };
        assert_eq!(
            settings.font_size_px,
            crate::settings::DEFAULT_FONT_SIZE_PX + 1.0
        );
        assert_eq!(p.render_signature().changed_count, 1);
        assert!(!p.is_dragging(), "stepper click does not arm a drag");
    }

    #[test]
    fn clicking_stepper_down_decrements_once_without_dragging() {
        let mut p = panel();
        let (row, zone) = stepper_row(&p, "font_size");
        let RowZone::Stepper { down_x0, .. } = zone else {
            unreachable!()
        };
        let SettingsPanelOutcome::Apply(settings) =
            p.handle_pointer_press(W, H, row, down_x0, PointerButton::Left, None)
        else {
            panic!("down click applies once");
        };
        assert_eq!(
            settings.font_size_px,
            crate::settings::DEFAULT_FONT_SIZE_PX - 1.0
        );
        assert_eq!(p.render_signature().changed_count, 1);
        assert!(!p.is_dragging(), "stepper click does not arm a drag");
    }

    #[test]
    fn pointer_move_after_stepper_click_is_inert() {
        let mut p = panel();
        let (row, zone) = stepper_row(&p, "font_size");
        let RowZone::Stepper { down_x0, up_x0, .. } = zone else {
            unreachable!()
        };
        let SettingsPanelOutcome::Apply(settings) =
            p.handle_pointer_press(W, H, row, down_x0, PointerButton::Left, None)
        else {
            panic!("stepper click applies a value");
        };
        assert_eq!(
            settings.font_size_px,
            crate::settings::DEFAULT_FONT_SIZE_PX - 1.0
        );
        assert_eq!(
            p.handle_pointer_drag(W, H, up_x0, None),
            SettingsPanelOutcome::Consumed
        );
        let value_after_move = p
            .render_signature()
            .entries
            .iter()
            .find(|entry| entry.key == "font_size")
            .and_then(|entry| entry.value.parse::<f32>().ok())
            .expect("font_size parses");
        assert_eq!(
            value_after_move,
            crate::settings::DEFAULT_FONT_SIZE_PX - 1.0,
            "pointer move after stepper click must not keep editing"
        );
    }

    #[test]
    fn right_clicking_a_stepper_does_not_change_the_value() {
        let mut p = panel();
        let (row, zone) = stepper_row(&p, "font_size");
        let RowZone::Stepper { down_x0, .. } = zone else {
            unreachable!()
        };
        assert_eq!(
            p.handle_pointer_press(W, H, row, down_x0, PointerButton::Right, None),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(
            p.render_signature().changed_count,
            0,
            "right-click is inert"
        );
        assert!(!p.is_dragging(), "right-click does not arm a drag");
    }

    #[test]
    fn a_narrow_panel_falls_back_to_a_click_to_type_value_row() {
        // Too narrow for usable stepper controls: the numeric row is a plain
        // Value line and a click starts a text edit (keyboard parity preserved).
        let mut p = panel();
        let narrow = 24;
        let row = p
            .build_visible_rows(narrow, H)
            .iter()
            .position(|(_, hit)| {
                hit.entry_index == Some(entry_index(&p, "font_size")) && hit.zone == RowZone::Value
            })
            .expect("font_size falls back to a Value row when narrow");
        assert_eq!(
            p.handle_pointer_press(narrow, H, row, 0, PointerButton::Left, None),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(p.render_signature().editing_key, Some("font_size"));
    }

    #[test]
    fn keyboard_step_and_stepper_share_the_same_commit_path() {
        use crate::native::overlay::OverlayInput;
        // Keyboard Right still steps by the folded spec.step (font_size: +1).
        let mut kb = panel();
        kb.set_selection(entry_index(&kb, "font_size"));
        let SettingsPanelOutcome::Apply(stepped) = kb.handle_input(OverlayInput::Right) else {
            panic!("keyboard step applies");
        };
        assert_eq!(
            stepped.font_size_px,
            crate::settings::DEFAULT_FONT_SIZE_PX + 1.0
        );
    }

    #[test]
    fn clicking_a_group_header_is_inert() {
        let mut p = panel();
        let hits = p.visible_hit_map(W, H);
        assert_eq!(hits[0].zone, RowZone::GroupHeader);
        let before = p.render_signature().selected;
        assert_eq!(
            p.handle_pointer_press(W, H, 0, 0, PointerButton::Left, None),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(
            p.render_signature().selected,
            before,
            "header click moves nothing"
        );
    }

    #[test]
    fn clicking_a_detail_line_focuses_its_owner_without_changing_value() {
        let mut p = panel();
        let rows = p.build_visible_rows(W, H);
        let (detail_row, owner) = rows
            .iter()
            .enumerate()
            .find_map(|(i, (_, hit))| {
                (hit.zone == RowZone::Detail).then(|| (i, hit.entry_index.unwrap()))
            })
            .expect("a detail line exists");
        assert_eq!(
            p.handle_pointer_press(W, H, detail_row, 0, PointerButton::Left, None),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(
            p.render_signature().selected,
            owner,
            "detail click focuses owner"
        );
        assert_eq!(
            p.render_signature().changed_count,
            0,
            "no value change from detail"
        );
    }

    #[test]
    fn out_of_range_body_row_is_inert() {
        let mut p = panel();
        assert_eq!(
            p.handle_pointer_press(W, H, 100_000, 0, PointerButton::Left, None),
            SettingsPanelOutcome::Consumed
        );
    }

    #[test]
    fn scroll_lines_moves_view_without_moving_selection() {
        let mut p = panel();
        let before_selected = p.render_signature().selected;
        p.scroll_lines(5);
        assert_eq!(p.render_signature().scroll, 5);
        assert_eq!(
            p.render_signature().selected,
            before_selected,
            "wheel scroll leaves keyboard focus put"
        );
        p.scroll_lines(-100);
        assert_eq!(p.render_signature().scroll, 0);
        p.scroll_lines(1_000_000);
        assert!(p.render_signature().scroll <= p.entries.len().saturating_sub(1));
    }

    #[test]
    fn keyboard_activate_and_pointer_click_toggle_a_bool_identically() {
        // Mouse is purely additive: a left-click on a bool value row must reach
        // the same applied settings as the unchanged keyboard Activate path.
        use crate::native::overlay::OverlayInput;
        let target = "synthetic_styles";

        let mut kb = panel();
        let idx = entry_index(&kb, target);
        kb.set_selection(idx);
        let SettingsPanelOutcome::Apply(via_keyboard) = kb.handle_input(OverlayInput::Activate)
        else {
            panic!("keyboard Activate applies the bool toggle");
        };

        let mut ms = panel();
        let row = value_row(&ms, target);
        let SettingsPanelOutcome::Apply(via_pointer) =
            ms.handle_pointer_press(W, H, row, 0, PointerButton::Left, None)
        else {
            panic!("pointer click applies the bool toggle");
        };

        assert_eq!(
            via_keyboard.synthetic_styles, via_pointer.synthetic_styles,
            "keyboard and pointer reach the same value"
        );
        assert!(!via_pointer.synthetic_styles);
    }
}
