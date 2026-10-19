// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl ContextMenuUi {
    /// The rendered body rows in display order. Each entry is either an
    /// [`ContextMenuRow::Item`] (with label, focus, and enabled state) or
    /// [`ContextMenuRow::Separator`] (the visual divider). The renderer decides
    /// how to paint each row type.
    pub(in crate::native) fn rows(&self) -> Vec<ContextMenuRow> {
        let mut item_index = 0;
        self.body_layout()
            .into_iter()
            .map(|item| {
                let Some(item) = item else {
                    return ContextMenuRow::Separator;
                };
                let focused = item_index == self.focused;
                item_index += 1;
                ContextMenuRow::Item {
                    label: self.item_label(item),
                    accelerator: self.accelerator_for_item(item).map(str::to_owned),
                    focused,
                    enabled: self.item_enabled(item),
                }
            })
            .collect()
    }

    pub(in crate::native) fn render_signature(&self) -> ContextMenuSignature {
        ContextMenuSignature {
            spawn: (self.spawn.row, self.spawn.column),
            focused: self.focused as u8,
            copy_enabled: self.copy_enabled,
            cut_enabled: self.cut_enabled,
            paste_enabled: self.paste_enabled,
            delete_enabled: self.delete_enabled,
            prompt_editing_hint: self.prompt_editing_hint,
            command_actions_enabled: self.command_actions_enabled,
            rename_enabled: self.rename_target.is_some(),
            multi_pane: self.multi_pane,
            multi_tab: self.multi_tab,
            multi_workspace: self.multi_workspace,
            bound_workspace: self.bound_workspace,
            pane_read_only: self.pane_read_only,
            bidi_reorder: self.bidi_reorder,
            bidi_direction: self.bidi_direction,
            pane_broadcast: self.pane_broadcast,
            broadcast_active: self.broadcast_active,
            workspace_count: self.workspace_count,
            workspace_slot: match self.surface {
                ContextMenuSurface::WorkspaceSlot(idx) => idx,
                _ => 0,
            },
            surface: self.surface.discriminant(),
            has_path_target: self.path_target.is_some(),
            is_image_target: self.is_image_target(),
            is_file_target: self.is_file_target(),
            connection_is_odytty: self.connection_is_odytty(),
            navigator_target_kind: self.navigator_target_kind(),
            navigator_detached_available: self.navigator_detached_available,
            window_actions: self.window_actions,
            accelerators: self
                .accelerators
                .iter()
                .enumerate()
                .filter_map(|(index, text)| Some((u8::try_from(index).ok()?, text.clone()?)))
                .collect(),
            reserved_cols: (self.reserved_cols_left, self.reserved_cols_right),
            scroll_anchor: self.scroll_anchor,
        }
    }
}

/// Human-readable accelerator label from the canonical config-token chord
/// string produced by [`crate::settings::format_key_chord`] (Part C). Reuses
/// that formatter for the modifier/key decomposition (no duplication) and only
/// title-cases each `+`-separated token: `ctrl+shift+e` → `Ctrl+Shift+E`.
pub(in crate::native) fn humanize_chord(token: String) -> String {
    token
        .split('+')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}
