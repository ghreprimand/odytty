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
//! existing `commit_value`/`apply_raw` seam — no new write path. UX4-P2 adds the
//! numeric slider (`Slider` zone): a draggable track plus a click-to-type
//! readout, both committing through that same seam.

use super::{RowEdit, SettingKind, SettingsPanel, SettingsPanelLine, SettingsPanelOutcome};
use super::{SettingInfo, ellipsize, setting_detail, wrap_words};
use crate::native::overlay::PointerButton;

/// Slider track geometry bounds (UX4-P2): the track grows to fill the value
/// area between these widths; below the minimum the row falls back to a plain
/// click-to-type value line.
const MIN_SLIDER_TRACK: usize = 8;
const MAX_SLIDER_TRACK: usize = 24;
/// Track groove and thumb glyphs. Box-drawing/full-block render reliably in the
/// overlay (same family as the panel border).
const SLIDER_GROOVE: char = '─';
const SLIDER_THUMB: char = '█';

/// The role of one rendered body line, used to dispatch a click. Produced in
/// lockstep with the rendered text by [`SettingsPanel::build_visible_rows`] so
/// the two views can never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum RowZone {
    /// A group label line — inert.
    GroupHeader,
    /// The `"name: value"` line — the primary action zone.
    Value,
    /// A numeric row rendered as a slider (UX4-P2): a click/drag on the track
    /// sets the value; a click on the readout starts a click-to-type edit. All
    /// columns are body-relative (0 = first body cell).
    Slider {
        track_x0: usize,
        track_w: usize,
        readout_x0: usize,
        readout_w: usize,
    },
    /// A wrapped help line — selects its owning row only, no value change.
    Detail,
    /// A `"! ..."` notice line — inert.
    Message,
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
    /// geometry are identical.
    pub(super) fn build_visible_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(SettingsPanelLine, RowHit)> {
        let mut rows: Vec<(SettingsPanelLine, RowHit)> = Vec::new();
        if body_width == 0 || body_height == 0 {
            return rows;
        }

        // OB-SEARCH: when searching, a fixed filter header sits above the
        // results. It is inert (entry_index None) — the keyboard owns the query
        // — so the pointer hit-map stays in lockstep with the rendered lines.
        if self.search_active {
            rows.push((
                SettingsPanelLine {
                    text: format!("  Search: {}|", self.query),
                    focused: true,
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
            // Numeric rows render as a slider unless they are being text-edited
            // (then the edit buffer shows in a plain value line) or the panel is
            // too narrow for a usable track (graceful fallback to click-to-type).
            let slider = if entry.kind == SettingKind::Number && !editing_this {
                self.slider_line(entry, marker, body_width)
            } else {
                None
            };
            let (text, zone) = if let Some((text, zone)) = slider {
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
                SettingsPanelLine { text, focused },
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
                },
                RowHit {
                    entry_index: None,
                    zone: RowZone::Message,
                },
            ));
        }

        rows
    }

    /// Render a numeric row as a slider (UX4-P2): `"{marker} {name}: ───█── {value}"`.
    /// Returns `None` (caller falls back to a plain click-to-type value line)
    /// when the row has no [`crate::settings::NumericSpec`], the value cannot be
    /// parsed, or the panel is too narrow for a usable track. The readout column
    /// budget is reserved from the spec (not the live value) so the track does
    /// not jump as the value or its changed marker grows during a drag.
    fn slider_line(
        &self,
        entry: &SettingInfo,
        marker: &str,
        body_width: usize,
    ) -> Option<(String, RowZone)> {
        let spec = entry.numeric?;
        let prefix = format!("{marker} {}: ", entry.name);
        let prefix_w = prefix.chars().count();
        let readout = self.display_value(entry);
        let readout_budget = spec.readout_width();

        let remaining = body_width.checked_sub(prefix_w)?;
        // Need at least the track, one separating space, and the readout budget.
        let track_avail = remaining.checked_sub(1 + readout_budget)?;
        if track_avail < MIN_SLIDER_TRACK {
            return None;
        }
        let track_w = track_avail.min(MAX_SLIDER_TRACK);
        let track_x0 = prefix_w;
        let readout_x0 = track_x0 + track_w + 1;

        let value = entry.value.parse::<f32>().ok()?;
        let fraction = spec.fraction_of(value);
        let last = track_w.saturating_sub(1);
        let thumb = ((fraction * last as f32).round() as usize).min(last);

        let mut track = String::with_capacity(track_w);
        for column in 0..track_w {
            track.push(if column == thumb {
                SLIDER_THUMB
            } else {
                SLIDER_GROOVE
            });
        }

        Some((
            format!("{prefix}{track} {readout}"),
            RowZone::Slider {
                track_x0,
                track_w,
                readout_x0,
                readout_w: readout_budget,
            },
        ))
    }

    /// Test seam (UX4-P2): the body-row offset and track geometry
    /// (`track_x0`, `track_w`) of the first visible slider, so the overlay/App
    /// layers can drive a real drag without widening `build_visible_rows`.
    #[cfg(test)]
    pub(in crate::native) fn first_slider_zone_for_test(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Option<(usize, usize, usize)> {
        self.build_visible_rows(body_width, body_height)
            .into_iter()
            .enumerate()
            .find_map(|(row, (_, hit))| match hit.zone {
                RowZone::Slider {
                    track_x0, track_w, ..
                } => Some((row, track_x0, track_w)),
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
    pub(in crate::native) fn scroll_lines(&mut self, delta: isize) {
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
    ) -> SettingsPanelOutcome {
        // A fresh press ends any stale drag before dispatching.
        self.dragging = None;
        let hit_map = self.visible_hit_map(body_width, body_height);
        let Some(hit) = hit_map.get(row_in_body).copied() else {
            return SettingsPanelOutcome::Consumed;
        };
        let Some(entry_index) = hit.entry_index else {
            // GroupHeader / Message: inert, no focus change.
            return SettingsPanelOutcome::Consumed;
        };

        // A click anywhere on a real row cancels any in-progress text edit
        // (clicking away is the mouse analogue of Esc), then focuses that row.
        self.editing = None;
        self.set_selection(entry_index);

        match hit.zone {
            RowZone::GroupHeader | RowZone::Message => SettingsPanelOutcome::Consumed,
            // A help line only selects its owning row; no value change.
            RowZone::Detail => SettingsPanelOutcome::Consumed,
            RowZone::Value => self.click_action_on_selected(button),
            RowZone::Slider {
                track_x0,
                track_w,
                readout_x0,
                readout_w,
            } => self.slider_press(
                entry_index,
                col_in_body,
                button,
                track_x0,
                track_w,
                readout_x0,
                readout_w,
            ),
        }
    }

    /// Dispatch a press that landed on a numeric slider row (UX4-P2): a press on
    /// the track sets the value and arms a drag; a press on the readout starts a
    /// click-to-type edit; elsewhere on the row it only focuses. Right-click on
    /// a slider has no value verb (it just focuses), keeping reverse-cycle an
    /// enum-only gesture.
    #[allow(clippy::too_many_arguments)]
    fn slider_press(
        &mut self,
        entry_index: usize,
        col_in_body: usize,
        button: PointerButton,
        track_x0: usize,
        track_w: usize,
        readout_x0: usize,
        readout_w: usize,
    ) -> SettingsPanelOutcome {
        if button == PointerButton::Right {
            return SettingsPanelOutcome::Consumed;
        }
        let Some(entry) = self.entries.get(entry_index) else {
            return SettingsPanelOutcome::Consumed;
        };
        let key = entry.key;
        let reloadable = entry.reloadable;
        let spec = entry.numeric;
        if !reloadable {
            self.message = Some("Startup-only setting; edit odytty.conf and restart.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }

        if col_in_body >= track_x0 && col_in_body < track_x0 + track_w {
            let Some(spec) = spec else {
                return SettingsPanelOutcome::Consumed;
            };
            self.dragging = Some(key);
            self.set_value_from_slider(key, spec, track_x0, track_w, col_in_body)
        } else if col_in_body >= readout_x0 && col_in_body < readout_x0 + readout_w {
            self.start_numeric_edit(entry_index)
        } else {
            // The label/prefix area: focus only.
            SettingsPanelOutcome::Consumed
        }
    }

    /// Continue an in-progress slider drag (UX4-P2): map the current cursor
    /// column to a value for the dragged row. Geometry is recomputed from the
    /// shared row walker each move, so a resize mid-drag can never desync it.
    pub(in crate::native) fn handle_pointer_drag(
        &mut self,
        body_width: usize,
        body_height: usize,
        col_in_body: usize,
    ) -> SettingsPanelOutcome {
        let Some(key) = self.dragging else {
            return SettingsPanelOutcome::Consumed;
        };
        let Some(index) = self.entries.iter().position(|entry| entry.key == key) else {
            return SettingsPanelOutcome::Consumed;
        };
        let Some(spec) = self.entries[index].numeric else {
            return SettingsPanelOutcome::Consumed;
        };
        let Some((track_x0, track_w)) = self.slider_zone_for(index, body_width, body_height) else {
            return SettingsPanelOutcome::Consumed;
        };
        self.set_value_from_slider(key, spec, track_x0, track_w, col_in_body)
    }

    /// End a slider drag (UX4-P2), called on pointer release.
    pub(in crate::native) fn end_slider_drag(&mut self) {
        self.dragging = None;
    }

    /// Map a body-relative column on a slider track to a committed value. Columns
    /// left of / right of the track saturate to min / max, mirroring how the
    /// selection path saturates a drag past an edge. Commit flows through the
    /// existing `commit_value` → `apply_raw` clamp/parse seam.
    fn set_value_from_slider(
        &mut self,
        key: &'static str,
        spec: crate::settings::NumericSpec,
        track_x0: usize,
        track_w: usize,
        col_in_body: usize,
    ) -> SettingsPanelOutcome {
        let fraction = if track_w <= 1 {
            0.0
        } else {
            ((col_in_body as f32 - track_x0 as f32) / (track_w - 1) as f32).clamp(0.0, 1.0)
        };
        let value = spec.value_at_fraction(fraction);
        self.commit_value(key, &format!("{value:.3}"))
    }

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

    /// The track geometry of the slider currently rendered for entry `index`, by
    /// scanning the shared row walker — guarantees drag geometry matches what is
    /// drawn. `None` if that row is not currently a visible slider.
    fn slider_zone_for(
        &self,
        index: usize,
        body_width: usize,
        body_height: usize,
    ) -> Option<(usize, usize)> {
        self.build_visible_rows(body_width, body_height)
            .into_iter()
            .find_map(|(_, hit)| match hit.zone {
                RowZone::Slider {
                    track_x0, track_w, ..
                } if hit.entry_index == Some(index) => Some((track_x0, track_w)),
                _ => None,
            })
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
        match entry.kind {
            SettingKind::Bool => {
                let next = if entry.value == "on" { "off" } else { "on" };
                self.commit_value(entry.key, next)
            }
            // Theme row click opens the built-in picker, matching the keyboard
            // Left/Right behavior (not the Enter custom-path text edit).
            SettingKind::Enum if entry.key == "theme" => {
                self.message = Some("Opening built-in theme picker.".to_owned());
                SettingsPanelOutcome::OpenThemePicker
            }
            SettingKind::Enum => {
                let direction = if button == PointerButton::Right {
                    -1
                } else {
                    1
                };
                self.cycle_selected(direction)
            }
            SettingKind::Number | SettingKind::String | SettingKind::Path | SettingKind::List => {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn panel() -> SettingsPanel {
        SettingsPanel::new(&Settings::default())
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

    /// The body row offset and `Slider` zone for `key` (numeric rows, UX4-P2).
    fn slider_row(panel: &SettingsPanel, key: &str) -> (usize, RowZone) {
        let want = entry_index(panel, key);
        panel
            .build_visible_rows(W, H)
            .iter()
            .enumerate()
            .find_map(|(row, (_, hit))| {
                matches!(hit.zone, RowZone::Slider { .. })
                    .then_some(())
                    .filter(|_| hit.entry_index == Some(want))
                    .map(|_| (row, hit.zone))
            })
            .expect("slider row present")
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
            p.handle_pointer_press(W, H, row, 0, PointerButton::Left)
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
            p.handle_pointer_press(W, H, row, 0, PointerButton::Left),
            SettingsPanelOutcome::OpenThemePicker
        );
    }

    #[test]
    fn left_and_right_click_cycle_an_enum_in_opposite_directions() {
        let row = value_row(&panel(), "subpixel");
        let mut fwd_panel = panel();
        let SettingsPanelOutcome::Apply(fwd) =
            fwd_panel.handle_pointer_press(W, H, row, 0, PointerButton::Left)
        else {
            panic!("enum left-click cycles forward");
        };
        let mut back_panel = panel();
        let SettingsPanelOutcome::Apply(back) =
            back_panel.handle_pointer_press(W, H, row, 0, PointerButton::Right)
        else {
            panic!("enum right-click cycles backward");
        };
        assert_ne!(
            fwd.subpixel, back.subpixel,
            "forward and backward land on different values"
        );
    }

    #[test]
    fn numeric_rows_render_as_a_slider_with_a_thumb() {
        let p = panel();
        let (_, zone) = slider_row(&p, "font_size");
        let RowZone::Slider {
            track_x0, track_w, ..
        } = zone
        else {
            panic!("font_size is a slider row");
        };
        assert!(track_w >= MIN_SLIDER_TRACK && track_w <= MAX_SLIDER_TRACK);
        // The rendered track contains exactly one thumb glyph.
        let rows = p.build_visible_rows(W, H);
        let line = &rows
            .iter()
            .find(|(_, hit)| matches!(hit.zone, RowZone::Slider { .. }))
            .unwrap()
            .0
            .text;
        assert_eq!(
            line.chars().filter(|&c| c == SLIDER_THUMB).count(),
            1,
            "one thumb: {line:?}"
        );
        assert!(track_x0 > 0, "track starts after the name prefix");
    }

    #[test]
    fn clicking_the_slider_readout_starts_a_text_edit() {
        let mut p = panel();
        let (row, zone) = slider_row(&p, "font_size");
        let RowZone::Slider { readout_x0, .. } = zone else {
            unreachable!()
        };
        assert_eq!(
            p.handle_pointer_press(W, H, row, readout_x0, PointerButton::Left),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(p.render_signature().editing_key, Some("font_size"));
    }

    #[test]
    fn clicking_the_slider_track_sets_the_value_through_the_commit_seam() {
        let mut p = panel();
        let (row, zone) = slider_row(&p, "font_size");
        let RowZone::Slider {
            track_x0, track_w, ..
        } = zone
        else {
            unreachable!()
        };
        // Click the far right of the track → saturates to the max (72 px).
        let SettingsPanelOutcome::Apply(settings) =
            p.handle_pointer_press(W, H, row, track_x0 + track_w - 1, PointerButton::Left)
        else {
            panic!("track click applies a value");
        };
        assert_eq!(settings.font_size_px, crate::settings::MAX_FONT_SIZE_PX);
        assert_eq!(p.render_signature().changed_count, 1);
        // The press also armed a drag.
        assert!(p.is_dragging(), "track press arms a drag");
    }

    #[test]
    fn clicking_the_far_left_of_the_track_sets_the_minimum() {
        let mut p = panel();
        let (row, zone) = slider_row(&p, "font_size");
        let RowZone::Slider { track_x0, .. } = zone else {
            unreachable!()
        };
        let SettingsPanelOutcome::Apply(settings) =
            p.handle_pointer_press(W, H, row, track_x0, PointerButton::Left)
        else {
            panic!("track click applies a value");
        };
        assert_eq!(settings.font_size_px, crate::settings::MIN_FONT_SIZE_PX);
    }

    #[test]
    fn dragging_the_slider_updates_live_and_release_ends_the_drag() {
        let mut p = panel();
        let (row, zone) = slider_row(&p, "font_size");
        let RowZone::Slider {
            track_x0, track_w, ..
        } = zone
        else {
            unreachable!()
        };
        // Press mid-track to start a drag.
        let _ = p.handle_pointer_press(W, H, row, track_x0 + track_w / 2, PointerButton::Left);
        assert!(p.is_dragging());
        // Drag to the right end → max; drag past the left end → min (saturates).
        let SettingsPanelOutcome::Apply(hi) = p.handle_pointer_drag(W, H, track_x0 + track_w + 50)
        else {
            panic!("drag right applies");
        };
        assert_eq!(hi.font_size_px, crate::settings::MAX_FONT_SIZE_PX);
        let SettingsPanelOutcome::Apply(lo) = p.handle_pointer_drag(W, H, 0) else {
            panic!("drag left applies");
        };
        assert_eq!(lo.font_size_px, crate::settings::MIN_FONT_SIZE_PX);
        // Release ends the drag; a subsequent move is inert.
        p.end_slider_drag();
        assert!(!p.is_dragging());
        assert_eq!(
            p.handle_pointer_drag(W, H, track_x0 + track_w - 1),
            SettingsPanelOutcome::Consumed,
            "no drag after release"
        );
    }

    #[test]
    fn right_clicking_a_slider_does_not_change_the_value() {
        let mut p = panel();
        let (row, zone) = slider_row(&p, "font_size");
        let RowZone::Slider { track_x0, .. } = zone else {
            unreachable!()
        };
        assert_eq!(
            p.handle_pointer_press(W, H, row, track_x0, PointerButton::Right),
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
        // Too narrow for a usable track: the numeric row is a plain Value line
        // and a click starts a text edit (keyboard parity preserved).
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
            p.handle_pointer_press(narrow, H, row, 0, PointerButton::Left),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(p.render_signature().editing_key, Some("font_size"));
    }

    #[test]
    fn keyboard_step_and_slider_share_the_same_commit_path() {
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
            p.handle_pointer_press(W, H, 0, 0, PointerButton::Left),
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
            p.handle_pointer_press(W, H, detail_row, 0, PointerButton::Left),
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
            p.handle_pointer_press(W, H, 100_000, 0, PointerButton::Left),
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
            ms.handle_pointer_press(W, H, row, 0, PointerButton::Left)
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
