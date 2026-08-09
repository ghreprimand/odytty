// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl ContextMenuUi {
    /// The rendered body rows in display order. Each entry is either an
    /// [`ContextMenuRow::Item`] (with label, focus, and enabled state) or
    /// [`ContextMenuRow::Separator`] (the visual divider). The renderer decides
    /// how to paint each row type.
    pub(in crate::native) fn rows(&self) -> Vec<ContextMenuRow> {
        let items = self.visible_items();
        let mut out = Vec::with_capacity(self.body_row_count());
        let mut prev_section: Option<u8> = None;
        for (item_index, item) in items.iter().enumerate() {
            // Insert a separator wherever consecutive visible items cross a
            // section boundary, so the layout reflows when Close Pane appears.
            let section = self.section_of(*item);
            if prev_section.is_some_and(|p| p != section) {
                out.push(ContextMenuRow::Separator);
            }
            prev_section = Some(section);
            out.push(ContextMenuRow::Item {
                label: item.label(),
                accelerator: self.accelerator_for_item(*item).map(str::to_owned),
                focused: item_index == self.focused,
                enabled: self.item_enabled(*item),
            });
        }
        out
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
            rename_enabled: self.rename_target.is_some(),
            multi_pane: self.multi_pane,
            multi_tab: self.multi_tab,
            multi_workspace: self.multi_workspace,
            bound_workspace: self.bound_workspace,
            workspace_count: self.workspace_count,
            surface: self.surface.discriminant(),
            has_path_target: self.path_target.is_some(),
            is_image_target: self.is_image_target(),
            is_file_target: self.is_file_target(),
            connection_is_odytty: self.connection_is_odytty(),
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
