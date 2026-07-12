// SPDX-License-Identifier: GPL-3.0-only
//! Section-list model for the two-level settings overlay (SETTINGS-REDESIGN).
//!
//! Level 1 is a compact list of human-friendly sections; Level 2 drills into
//! the filtered entry list for one section. This module owns the section table
//! and the `SettingsLevel` state enum that drives the split. All group-to-section
//! mapping is derived from `SECTIONS` — never duplicated per section.

/// One display section in the Level-1 section list.
pub(super) struct Section {
    /// Display name shown in the Level-1 section list.
    pub(super) name: &'static str,
    /// Raw `group` strings from `SettingInfo` that belong to this section.
    /// All settings whose `group` matches any entry here appear at Level 2.
    pub(super) groups: &'static [&'static str],
}

/// Compile-time section table. Maps 15 raw groups → 10 display sections.
/// Both the Level-1 section list and the Level-2 entry filter are derived
/// from this table; filter logic is never duplicated per section.
pub(super) const SECTIONS: &[Section] = &[
    Section {
        name: "Themes",
        groups: &["Theme"],
    },
    Section {
        name: "Fonts",
        groups: &["Font"],
    },
    Section {
        name: "Rendering",
        groups: &["Rendering"],
    },
    // Tabs, the workspace rail, the tab panel, and panes share this one
    // discoverable "Layout" section. The group order here matches the group rank
    // in `setting_group_rank` so the Level-2 rows read Tabs, Workspace rail,
    // Panel, Panes top to bottom.
    Section {
        name: "Layout",
        groups: &["Tabs", "Workspace rail", "Panel", "Panes"],
    },
    Section {
        name: "Effects",
        groups: &["Post-process"],
    },
    Section {
        name: "Cursor",
        groups: &["Cursor"],
    },
    Section {
        name: "Input",
        groups: &["Input", "Clipboard"],
    },
    Section {
        name: "Sessions",
        groups: &["Sessions"],
    },
    Section {
        name: "Connections",
        groups: &["Connections"],
    },
    Section {
        name: "Advanced",
        groups: &["Accessibility", "Development"],
    },
];
