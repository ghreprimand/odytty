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
//! existing `commit_value`/`apply_raw` seam — no new write path. The slider
//! (`SliderTrack`/`Readout`) zones are UX4-P2 and intentionally absent here.

use super::{RowEdit, SettingKind, SettingsPanel, SettingsPanelLine, SettingsPanelOutcome};
use super::{ellipsize, setting_detail, wrap_words};
use crate::native::overlay::PointerButton;

/// The role of one rendered body line, used to dispatch a click. Produced in
/// lockstep with the rendered text by [`SettingsPanel::build_visible_rows`] so
/// the two views can never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum RowZone {
    /// A group label line — inert.
    GroupHeader,
    /// The `"name: value"` line — the primary action zone.
    Value,
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
            let mut value = self.display_value(entry);
            let max_value = body_width.saturating_sub(entry.name.chars().count() + 6);
            if value.chars().count() > max_value {
                value = ellipsize(&value, max_value);
            }
            rows.push((
                SettingsPanelLine {
                    text: format!("{marker} {}: {value}", entry.name),
                    focused,
                },
                RowHit {
                    entry_index: Some(index),
                    zone: RowZone::Value,
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

        rows
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

    /// Handle a left/right press inside the panel body. `row_in_body` is the
    /// 0-based row offset from the first body cell (`cell.row - rect.body_top`).
    /// A click first focuses the owning row, then acts per zone / setting kind.
    /// All value changes funnel through the existing `commit_value` seam.
    pub(in crate::native) fn handle_pointer_press(
        &mut self,
        body_width: usize,
        body_height: usize,
        row_in_body: usize,
        button: PointerButton,
    ) -> SettingsPanelOutcome {
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
        }
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
            p.handle_pointer_press(W, H, row, PointerButton::Left)
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
            p.handle_pointer_press(W, H, row, PointerButton::Left),
            SettingsPanelOutcome::OpenThemePicker
        );
    }

    #[test]
    fn left_and_right_click_cycle_an_enum_in_opposite_directions() {
        let row = value_row(&panel(), "subpixel");
        let mut fwd_panel = panel();
        let SettingsPanelOutcome::Apply(fwd) =
            fwd_panel.handle_pointer_press(W, H, row, PointerButton::Left)
        else {
            panic!("enum left-click cycles forward");
        };
        let mut back_panel = panel();
        let SettingsPanelOutcome::Apply(back) =
            back_panel.handle_pointer_press(W, H, row, PointerButton::Right)
        else {
            panic!("enum right-click cycles backward");
        };
        assert_ne!(
            fwd.subpixel, back.subpixel,
            "forward and backward land on different values"
        );
    }

    #[test]
    fn clicking_a_number_value_row_starts_a_text_edit() {
        let mut p = panel();
        let row = value_row(&p, "font_size");
        assert_eq!(
            p.handle_pointer_press(W, H, row, PointerButton::Left),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(p.render_signature().editing_key, Some("font_size"));
    }

    #[test]
    fn clicking_a_group_header_is_inert() {
        let mut p = panel();
        let hits = p.visible_hit_map(W, H);
        assert_eq!(hits[0].zone, RowZone::GroupHeader);
        let before = p.render_signature().selected;
        assert_eq!(
            p.handle_pointer_press(W, H, 0, PointerButton::Left),
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
            p.handle_pointer_press(W, H, detail_row, PointerButton::Left),
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
            p.handle_pointer_press(W, H, 100_000, PointerButton::Left),
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
            ms.handle_pointer_press(W, H, row, PointerButton::Left)
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
