// SPDX-License-Identifier: GPL-3.0-only
//! App-side named-layout actions.
//!
//! A layout is a saved workspace shape - its tabs, pane split tree, per-pane
//! cwd, and host binding, using the launch-autosave schema. Saving captures
//! either the selected workspace or the whole application.
//! Opening a layout either adds its workspaces beside the current set
//! ([`LayoutPlacement::Add`]) or replaces the whole set
//! ([`LayoutPlacement::Replace`]); a window holding real state asks which
//! through the open-layout mode dialog. Deleting removes the file. The actions
//! are reachable from the command palette and menus, and every failure
//! degrades to a one-line notice rather than disturbing the running window.

use super::*;
use crate::native::persistence::{self, LoadOutcome};
use crate::native::session::RestoreReport;

/// How a saved layout lands on the current window. Chosen by
/// the open-layout mode dialog unless `is_single_pristine_workspace` confirms
/// an idle, writable, local single-pane shell that `Add` can consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutPlacement {
    /// Append the saved workspace(s) beside the current set (the pre-existing
    /// open-append behavior; consumes a single pristine workspace).
    Add,
    /// Tear down every current workspace and install the saved set as the whole
    /// application, honoring the saved active-workspace index.
    Replace,
}

impl App {
    /// Save the ACTIVE workspace as a named layout (8g), using the workspace's
    /// own display name as the layout name. This is the command-palette entry's
    /// prompt-free path; the menu "Save as Layout\u{2026}" surface routes a typed
    /// name through [`Self::save_workspace_as_layout`] instead. A name that
    /// collides with an existing layout raises the overwrite-confirm dialog
    /// rather than silently clobbering, even on this prompt-free
    /// path; otherwise a one-line notice confirms.
    pub(super) fn save_active_workspace_as_layout(&mut self) {
        let active = self.sessions.active_workspace_index();
        self.save_workspace_as_layout(active, None);
    }

    /// Save the workspace at rail index `idx` as a named layout (8g). This backs
    /// the "Save Workspace as Layout\u{2026}" menu surfaces: a
    /// rail slot saves the clicked workspace, the content-grid section the active
    /// one. `name_override` is the typed layout name from the prompt; `None`
    /// falls back to the workspace's own display name (the palette path). A stale
    /// index or an empty resulting name is a no-op. a name that
    /// already exists raises the overwrite-confirm dialog instead of clobbering;
    /// otherwise a write failure degrades to a one-line notice.
    pub(super) fn save_workspace_as_layout(&mut self, idx: usize, name_override: Option<&str>) {
        let Some(name) = self.resolve_layout_name(name_override, idx) else {
            return;
        };
        let kind = self
            .sessions
            .workspace_identity(idx)
            .map(LayoutSaveKind::Workspace);
        self.save_layout_or_confirm(name, kind, |app, name| {
            app.write_workspace_layout(idx, name);
        });
    }

    /// Save the WHOLE application - every workspace, with its tabs, split trees,
    /// per-pane cwd, and host bindings, and the active-workspace index preserved -
    /// as a single named layout. This is the primary "Save as
    /// Layout" surface: a layout means the whole session, not one workspace.
    /// `name_override` is the typed layout name from the prompt; `None` (the
    /// palette path) falls back to the active workspace's own name as a sensible
    /// default. An empty resulting name is a no-op. a name that
    /// already exists raises the overwrite-confirm dialog instead of clobbering;
    /// otherwise a write failure degrades to a one-line notice.
    pub(super) fn save_all_workspaces_as_layout(&mut self, name_override: Option<&str>) {
        let Some(name) =
            self.resolve_layout_name(name_override, self.sessions.active_workspace_index())
        else {
            return;
        };
        self.save_layout_or_confirm(name, Some(LayoutSaveKind::WholeApp), |app, name| {
            app.write_all_workspaces_layout(name);
        });
    }

    /// The collision skeleton both save paths share. A name that already exists
    /// prompts (replace vs. a different name) rather than silently
    /// overwriting, on every save path, prompt-free palette entry included; a
    /// `kind` of `None` (the workspace slot went stale) then saves nothing.
    /// Otherwise `write` runs with the name.
    fn save_layout_or_confirm(
        &mut self,
        name: String,
        kind: Option<LayoutSaveKind>,
        write: impl FnOnce(&mut Self, &str),
    ) {
        if persistence::layout_exists(&name) {
            if let Some(kind) = kind {
                self.overlay.open_confirm_overwrite_layout(name, kind);
                self.request_selection_redraw();
            }
            return;
        }
        write(self, &name);
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
    /// collision check - the confirmed-replace arm of OVERWRITE-WARN and the
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

    /// Force-write every workspace as layout `name`, no collision check - the
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

    /// The confirmed-replace arm of the overwrite-layout dialog:
    /// force-write the layout, clobbering the existing file, routed by `kind`.
    pub(super) fn overwrite_layout_confirmed(&mut self, name: &str, kind: LayoutSaveKind) {
        match kind {
            // The dialog names a workspace, not a rail position: a workspace
            // that closed while it was open is reported, never replaced by
            // whichever workspace now holds its position.
            LayoutSaveKind::Workspace(identity) => match self.sessions.workspace_index_of(identity)
            {
                Some(idx) => self.write_workspace_layout(idx, name),
                None => self.notice_layout_workspace_closed(),
            },
            LayoutSaveKind::WholeApp => self.write_all_workspaces_layout(name),
        }
    }

    /// Report a refused workspace save without naming a different target.
    pub(super) fn notice_layout_workspace_closed(&mut self) {
        self.raise_open_notice("The workspace closed, so the layout was not saved.".to_owned());
    }

    /// Open a saved layout, deciding how it lands on the current window.
    /// `is_single_pristine_workspace` allows an idle, writable, local single-pane
    /// shell to be consumed directly. Other windows ask whether to replace or
    /// append. The confirmation path loads and instantiates at acceptance.
    pub(super) fn open_layout(&mut self, name: &str) {
        if self.sessions.is_single_pristine_workspace() {
            // Pristine launch: the append path consumes the lone default
            // workspace, so the result is exactly the saved set. No prompt.
            self.instantiate_layout(name, LayoutPlacement::Add);
        } else {
            self.settle_pointer_for_modal();
            self.overlay.open_confirm_open_layout(name.to_owned());
            self.request_selection_redraw();
        }
    }

    /// Instantiate a saved layout onto the current window with an explicit
    /// placement: `Add` APPENDS its workspace(s) after the
    /// current list (consuming a single pristine workspace, 8e / PRISTINE-CONSUME),
    /// `Replace` tears down every current workspace and installs the saved set as
    /// the whole application (active workspace honored). A corrupt, version-
    /// skewed, or missing layout degrades to a notice and leaves the current
    /// workspaces untouched. On success a compact notice reports any session
    /// reattachment (8h), stale-directory fallback, or remote fallback.
    pub(super) fn instantiate_layout(&mut self, name: &str, placement: LayoutPlacement) {
        match persistence::load_layout(name) {
            LoadOutcome::Loaded(snapshot) => {
                let home = persistence::restore_home_dir();
                let settings = self.settings.clone();
                let ctx = self.remote_restore_context();
                let grid = self.grid;
                let report = match placement {
                    LayoutPlacement::Add => self.sessions.append_from_snapshot_with(
                        &snapshot,
                        home.as_deref(),
                        |set, leaf| {
                            super::profile_launch::spawn_restored_local_leaf(
                                &settings, grid, set, leaf,
                            )
                        },
                        |set, identity| ctx.spawn(set, grid, identity),
                    ),
                    LayoutPlacement::Replace => self.sessions.restore_from_snapshot_with(
                        &snapshot,
                        home.as_deref(),
                        |set, leaf| {
                            super::profile_launch::spawn_restored_local_leaf(
                                &settings, grid, set, leaf,
                            )
                        },
                        |set, identity| ctx.spawn(set, grid, identity),
                    ),
                };
                match report {
                    RestoreReport::Restored {
                        stale_cwd,
                        reattached,
                        reattach_attempted,
                        remote_fallback,
                        ..
                    } => {
                        // Seed the instantiated sessions with the current theme
                        // palette / cursor defaults / scrollback cap. The rebuild
                        // spawns terminals inside the session arena without
                        // routing them through `initialize_session_with`, so
                        // without this they render menus, overlays and content in
                        // the `DynamicColors::default()` palette instead of the
                        // theme's - a per-workspace presentation divergence.
                        self.apply_model_state_to_all_sessions();
                        self.flash_rail_autohide();
                        self.recompute_grid_for_tab_bar();
                        self.on_active_session_changed();
                        self.raise_open_notice(layout_open_notice(
                            name,
                            reattached,
                            reattach_attempted,
                            stale_cwd,
                            remote_fallback,
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

    /// Delete a saved layout (8e) named by a palette row. The delete happens
    /// only while the file still matches `stamp`, the identity captured when
    /// the row was listed: a layout removed, replaced, or rewritten since then
    /// is left alone and a notice says so. Without a captured stamp, deletion
    /// is refused without claiming the file is absent. A notice reports the result.
    pub(super) fn delete_layout(&mut self, name: &str, stamp: Option<persistence::LayoutStamp>) {
        use persistence::LayoutDeleteOutcome;
        let Some(stamp) = stamp else {
            self.raise_open_notice(format!(
                "Couldn't verify layout \u{201c}{name}\u{201d} when the palette opened; nothing was deleted."
            ));
            return;
        };
        let outcome = persistence::delete_layout_if_unchanged(name, stamp);
        let notice = match outcome {
            Ok(LayoutDeleteOutcome::Deleted) => format!("Deleted layout \u{201c}{name}\u{201d}."),
            Ok(LayoutDeleteOutcome::Gone) => {
                format!("Layout \u{201c}{name}\u{201d} no longer exists; nothing was deleted.")
            }
            Ok(LayoutDeleteOutcome::Changed) => format!(
                "Layout \u{201c}{name}\u{201d} changed after the palette opened; it was not deleted."
            ),
            Err(_) => format!("Couldn't delete layout \u{201c}{name}\u{201d}."),
        };
        self.raise_open_notice(notice);
    }

    /// Open the "Open Layout \u{25b8}" picker, seeded with the
    /// saved layout names. Opens even with no saved layouts (the picker then
    /// shows an explanatory line) so the feature is discoverable from the menu.
    pub(super) fn open_saved_layout_picker(&mut self) {
        let listing = persistence::list_layouts();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_layout_picker(listing.names);
        if listing.truncated {
            self.raise_neutral_notice(format!(
                "Only the first {} entries of the layouts folder were checked.",
                persistence::MAX_LAYOUT_DIR_SCAN_ENTRIES
            ));
        }
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
    remote_fallback: usize,
) -> String {
    let mut notice = format!("Opened layout \u{201c}{name}\u{201d}");
    let mut extras: Vec<String> = Vec::new();
    if reattach_attempted > 0 {
        extras.push(format!(
            "{reattached} of {reattach_attempted} sessions reattached"
        ));
    }
    if remote_fallback > 0 {
        extras.push(format!("{remote_fallback} opened locally"));
    }
    if stale_cwd > 0 {
        extras.push("some panes opened at home".to_owned());
    }
    if extras.is_empty() {
        notice.push('.');
    } else {
        notice.push_str(" - ");
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
            layout_open_notice("work", 0, 0, 0, 0),
            "Opened layout \u{201c}work\u{201d}."
        );
    }

    #[test]
    fn notice_reports_reattach_and_stale() {
        let n = layout_open_notice("remote", 2, 3, 1, 0);
        assert!(n.contains("2 of 3 sessions reattached"), "{n}");
        assert!(n.contains("some panes opened at home"), "{n}");
    }
    #[test]
    fn an_unverifiable_layout_palette_row_reports_refusal_without_claiming_absence() {
        use crate::core::Dimensions;
        use crate::native::options::NativeOptions;
        use crate::native::test_support::headless_app_with;
        use crate::settings::Settings;
        use winit::keyboard::NamedKey;
        let scratch = crate::test_dirs::ScratchDir::new("layout-delete-unverified-");
        crate::native::tests::config_env::with_config_base(scratch.path(), true, || {
            let (mut app, _) = headless_app_with(
                NativeOptions::default(),
                Dimensions::new(80, 24),
                Settings::default(),
            );
            app.save_all_workspaces_as_layout(Some("work"));
            let path = crate::native::persistence::layouts_dir().join("work.json");
            let bytes = std::fs::read(&path).unwrap();
            app.drive_char_with_mods_for_test('p', true, true);
            assert!(app.overlay_open_for_test());
            // Inject only the unavailable metadata captured by discovery. The
            // row, filtering, Enter key and dispatch remain production paths.
            app.capture_palette_targets(Vec::new(), Vec::new(), vec![("work".to_owned(), None)]);
            for ch in "Delete Layout: work".chars() {
                app.drive_char_with_mods_for_test(ch, false, false);
            }
            let before = app.open_notice_overlay_signature();
            app.drive_named_key_for_test(NamedKey::Enter);
            assert!(!app.overlay_open_for_test());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                app.open_notice_message_for_test().as_deref(),
                Some(
                    "Couldn't verify layout \u{201c}work\u{201d} when the palette opened; nothing was deleted."
                )
            );
            assert_ne!(app.open_notice_overlay_signature(), before);
        });
    }
}
