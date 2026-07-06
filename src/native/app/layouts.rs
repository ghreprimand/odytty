// SPDX-License-Identifier: GPL-3.0-only
//! App-side named-layout actions (WP3 / 8e + 8g).
//!
//! A layout is a saved workspace shape — its tabs, pane split tree, per-pane
//! cwd, and F6-W5 host binding — reusing the same on-disk schema as the launch
//! autosave. Saving captures the CURRENT workspace as a one-workspace layout;
//! opening a layout APPENDS it as a new workspace (never clobbering the live
//! one, 8e); deleting removes the file. All three are command-palette surfaces
//! with no new chrome, and every failure degrades to a one-line notice rather
//! than disturbing the running window.

use super::*;
use crate::native::persistence::{self, LoadOutcome};
use crate::native::session::RestoreReport;

impl App {
    /// Save the ACTIVE workspace as a named layout (8g), using the workspace's
    /// own display name as the layout name. This is the command-palette entry's
    /// prompt-free path; the menu "Save as Layout\u{2026}" surface routes a typed
    /// name through [`Self::save_workspace_as_layout`] instead. A name that
    /// collides with an existing layout raises the overwrite-confirm dialog
    /// (OVERWRITE-WARN) rather than silently clobbering, even on this prompt-free
    /// path; otherwise a one-line notice confirms.
    pub(super) fn save_active_workspace_as_layout(&mut self) {
        let active = self.sessions.active_workspace_index();
        self.save_workspace_as_layout(active, None);
    }

    /// Save the workspace at rail index `idx` as a named layout (8g). This backs
    /// the "Save Workspace as Layout\u{2026}" menu surfaces (LAYOUT-SURFACE): a
    /// rail slot saves the clicked workspace, the content-grid section the active
    /// one. `name_override` is the typed layout name from the prompt; `None`
    /// falls back to the workspace's own display name (the palette path). A stale
    /// index or an empty resulting name is a no-op. OVERWRITE-WARN: a name that
    /// already exists raises the overwrite-confirm dialog instead of clobbering;
    /// otherwise a write failure degrades to a one-line notice.
    pub(super) fn save_workspace_as_layout(&mut self, idx: usize, name_override: Option<&str>) {
        let Some(name) = self.resolve_layout_name(name_override, idx) else {
            return;
        };
        // OVERWRITE-WARN: a collision prompts (replace vs. a different name)
        // rather than silently overwriting — on every save path, prompt-free
        // palette entry included.
        if persistence::layout_exists(&name) {
            self.overlay
                .open_confirm_overwrite_layout(name, LayoutSaveKind::Workspace(idx));
            self.request_selection_redraw();
            return;
        }
        self.write_workspace_layout(idx, &name);
    }

    /// Save the WHOLE application — every workspace, with its tabs, split trees,
    /// per-pane cwd, and host bindings, and the active-workspace index preserved
    /// — as a single named layout (SAVE-ALL-LAYOUT). This is the primary "Save as
    /// Layout" surface: a layout means the whole session, not one workspace.
    /// `name_override` is the typed layout name from the prompt; `None` (the
    /// palette path) falls back to the active workspace's own name as a sensible
    /// default. An empty resulting name is a no-op. OVERWRITE-WARN: a name that
    /// already exists raises the overwrite-confirm dialog instead of clobbering;
    /// otherwise a write failure degrades to a one-line notice.
    pub(super) fn save_all_workspaces_as_layout(&mut self, name_override: Option<&str>) {
        let Some(name) =
            self.resolve_layout_name(name_override, self.sessions.active_workspace_index())
        else {
            return;
        };
        if persistence::layout_exists(&name) {
            self.overlay
                .open_confirm_overwrite_layout(name, LayoutSaveKind::WholeApp);
            self.request_selection_redraw();
            return;
        }
        self.write_all_workspaces_layout(&name);
    }

    /// Resolve the effective layout name for a save: the trimmed typed name when
    /// present, else the display name of the workspace at `default_idx` (the
    /// prompt-free palette default). `None` when nothing usable remains, so an
    /// empty name is a silent no-op on every path.
    fn resolve_layout_name(
        &self,
        name_override: Option<&str>,
        default_idx: usize,
    ) -> Option<String> {
        let name = match name_override {
            Some(text) => text.trim().to_owned(),
            None => self.sessions.workspace_name(default_idx)?.to_owned(),
        };
        (!name.is_empty()).then_some(name)
    }

    /// Force-write the workspace at rail index `idx` as layout `name`, no
    /// collision check — the confirmed-replace arm of OVERWRITE-WARN and the
    /// no-collision fast path both land here. Re-captures the current state so a
    /// Replace writes what is on screen now. A stale index or write failure
    /// degrades to a one-line notice.
    fn write_workspace_layout(&mut self, idx: usize, name: &str) {
        let full = self.sessions.capture_shape();
        let Some(workspace) = full.workspaces.get(idx).cloned() else {
            return;
        };
        let layout = persistence::ShapeSnapshot {
            version: full.version,
            active_workspace: 0,
            workspaces: vec![workspace],
        };
        match persistence::save_layout(name, &layout) {
            Ok(stem) => self.raise_open_notice(format!("Saved layout \u{201c}{stem}\u{201d}")),
            Err(_) => self.raise_open_notice("Couldn't save the layout.".to_owned()),
        }
    }

    /// Force-write every workspace as layout `name`, no collision check — the
    /// confirmed-replace arm of OVERWRITE-WARN and the no-collision fast path both
    /// land here. Reuses [`persistence::save_layout`] with the full
    /// [`capture_shape`] output (no per-workspace slice); a write failure degrades
    /// to a one-line notice.
    ///
    /// [`capture_shape`]: crate::native::session::SessionSet::capture_shape
    fn write_all_workspaces_layout(&mut self, name: &str) {
        let full = self.sessions.capture_shape();
        let count = full.workspaces.len();
        match persistence::save_layout(name, &full) {
            Ok(stem) => self.raise_open_notice(if count == 1 {
                format!("Saved layout \u{201c}{stem}\u{201d}")
            } else {
                format!("Saved layout \u{201c}{stem}\u{201d} ({count} workspaces)")
            }),
            Err(_) => self.raise_open_notice("Couldn't save the layout.".to_owned()),
        }
    }

    /// The confirmed-replace arm of the overwrite-layout dialog (OVERWRITE-WARN):
    /// force-write the layout, clobbering the existing file, routed by `kind`.
    pub(super) fn overwrite_layout_confirmed(&mut self, name: &str, kind: LayoutSaveKind) {
        match kind {
            LayoutSaveKind::Workspace(idx) => self.write_workspace_layout(idx, name),
            LayoutSaveKind::WholeApp => self.write_all_workspaces_layout(name),
        }
    }

    /// Open (instantiate) a saved layout by APPENDING its workspace(s) after the
    /// current list and switching to the first one (8e). A corrupt, version-
    /// skewed, or missing layout degrades to a notice and leaves the current
    /// workspaces untouched. On success a compact notice reports any session
    /// reattachment (8h) or stale-directory fallback.
    pub(super) fn open_layout(&mut self, name: &str) {
        match persistence::load_layout(name) {
            LoadOutcome::Loaded(snapshot) => {
                let home = persistence::restore_home_dir();
                let report =
                    self.sessions
                        .append_from_snapshot(&snapshot, self.grid, home.as_deref());
                match report {
                    RestoreReport::Restored {
                        stale_cwd,
                        reattached,
                        reattach_attempted,
                        ..
                    } => {
                        // Seed the appended sessions with the current theme
                        // palette / cursor defaults / scrollback cap. Append
                        // spawns terminals inside the session arena without
                        // routing them through `initialize_session_with`, so
                        // without this they render menus, overlays and content in
                        // the `DynamicColors::default()` palette instead of the
                        // theme's — a per-workspace presentation divergence.
                        self.apply_model_state_to_all_sessions();
                        self.flash_rail_autohide();
                        self.recompute_grid_for_tab_bar();
                        self.on_active_session_changed();
                        self.raise_open_notice(layout_open_notice(
                            name,
                            reattached,
                            reattach_attempted,
                            stale_cwd,
                        ));
                    }
                    RestoreReport::Skipped => self
                        .raise_open_notice(format!("Couldn't open layout \u{201c}{name}\u{201d}.")),
                }
            }
            LoadOutcome::Absent => {
                self.raise_open_notice(format!("Layout \u{201c}{name}\u{201d} not found."))
            }
            LoadOutcome::Skew { .. } | LoadOutcome::Corrupt(_) => {
                self.raise_open_notice(format!("Couldn't read layout \u{201c}{name}\u{201d}."))
            }
        }
    }

    /// Delete a saved layout (8e). A missing layout is treated as success (the
    /// end state the user wanted). A one-line notice confirms.
    pub(super) fn delete_layout(&mut self, name: &str) {
        match persistence::delete_layout(name) {
            Ok(()) => self.raise_open_notice(format!("Deleted layout \u{201c}{name}\u{201d}.")),
            Err(_) => {
                self.raise_open_notice(format!("Couldn't delete layout \u{201c}{name}\u{201d}."))
            }
        }
    }

    /// Open the "Open Layout \u{25b8}" picker (LAYOUT-SURFACE), seeded with the
    /// saved layout names. Opens even with no saved layouts (the picker then
    /// shows an explanatory line) so the feature is discoverable from the menu.
    pub(super) fn open_saved_layout_picker(&mut self) {
        let names = persistence::list_layout_names();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_layout_picker(names);
        self.request_selection_redraw();
    }
}

/// Compose the single compact "opened layout" notice (8h): it names the layout
/// and, when relevant, reports how many detached sessions reattached and how
/// many panes fell back to home because their directory is gone.
fn layout_open_notice(
    name: &str,
    reattached: usize,
    reattach_attempted: usize,
    stale_cwd: usize,
) -> String {
    let mut notice = format!("Opened layout \u{201c}{name}\u{201d}");
    let mut extras: Vec<String> = Vec::new();
    if reattach_attempted > 0 {
        extras.push(format!(
            "{reattached} of {reattach_attempted} sessions reattached"
        ));
    }
    if stale_cwd > 0 {
        extras.push("some panes opened at home".to_owned());
    }
    if extras.is_empty() {
        notice.push('.');
    } else {
        notice.push_str(" \u{2014} ");
        notice.push_str(&extras.join("; "));
        notice.push('.');
    }
    notice
}

#[cfg(test)]
mod tests {
    use super::layout_open_notice;

    #[test]
    fn notice_is_bare_when_nothing_special_happened() {
        assert_eq!(
            layout_open_notice("work", 0, 0, 0),
            "Opened layout \u{201c}work\u{201d}."
        );
    }

    #[test]
    fn notice_reports_reattach_and_stale() {
        let n = layout_open_notice("remote", 2, 3, 1);
        assert!(n.contains("2 of 3 sessions reattached"), "{n}");
        assert!(n.contains("some panes opened at home"), "{n}");
    }
}
