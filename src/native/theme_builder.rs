// SPDX-License-Identifier: GPL-3.0-only
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::color::{linear_to_oklab, oklab_to_oklch, srgb_to_linear};
use crate::settings::Settings;
use crate::theme::{Appearance, Srgb, Theme, ThemeSpec, contrast_ratio, relative_luminance};
use crate::theme_author::{self, AUTHORING_CONTRAST_FLOOR, AuthorRole, FloorAgainst};

use super::overlay::OverlayInput;

#[derive(Debug, Clone)]
pub(super) struct ThemeBuilder {
    original: Theme,
    spec: ThemeSpec,
    selected: usize,
    scroll: usize,
    editing: Option<EditMode>,
    message: Option<String>,
    /// Which OKLCH channel the Left/Right arrows currently drive (U2). Internal
    /// view state only — it is surfaced through `message`/the readout lines (both
    /// already in the render signature), so it deliberately stays out of
    /// [`ThemeBuilderSignature`].
    channel: OklchChannel,
    /// How many floored roles the last save auto-snapped to AA (D-U2-2), carried
    /// from [`ThemeBuilder::save_request`] into [`ThemeBuilder::save_succeeded`]
    /// so the saved-confirmation message can report the backstop.
    save_snap_count: usize,
}

/// The OKLCH channel the keyboard editor is focused on (U2). Lightness and
/// chroma are delta-driven (±step per keypress, gamut-stable); hue is rotated by
/// a fixed step per keypress (the keyboard analog of the absolute hue dial,
/// D-U2-1). Cycled with `[` / `]` (the in-app channel-focus control).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OklchChannel {
    #[default]
    Lightness,
    Chroma,
    Hue,
}

/// Per-keypress OKLCH step sizes for the keyboard editor. Lightness and chroma
/// are additive deltas; hue is a fixed rotation. Kept conservative so a keypress
/// is a fine nudge, not a jump.
const L_STEP: f32 = 0.02;
const C_STEP: f32 = 0.01;
const H_STEP_DEG: f32 = 5.0;

impl OklchChannel {
    fn next(self) -> Self {
        match self {
            OklchChannel::Lightness => OklchChannel::Chroma,
            OklchChannel::Chroma => OklchChannel::Hue,
            OklchChannel::Hue => OklchChannel::Lightness,
        }
    }

    fn prev(self) -> Self {
        match self {
            OklchChannel::Lightness => OklchChannel::Hue,
            OklchChannel::Chroma => OklchChannel::Lightness,
            OklchChannel::Hue => OklchChannel::Chroma,
        }
    }

    fn label(self) -> &'static str {
        match self {
            OklchChannel::Lightness => "L (lightness)",
            OklchChannel::Chroma => "C (chroma)",
            OklchChannel::Hue => "H (hue)",
        }
    }

    /// The single non-zero delta this channel contributes for a `direction`
    /// (`-1` / `+1`) keypress, ready for [`theme_author::nudge`].
    fn deltas(self, direction: i16) -> (f32, f32, f32) {
        let sign = f32::from(direction);
        match self {
            OklchChannel::Lightness => (sign * L_STEP, 0.0, 0.0),
            OklchChannel::Chroma => (0.0, sign * C_STEP, 0.0),
            OklchChannel::Hue => (0.0, 0.0, sign * H_STEP_DEG.to_radians()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ThemeBuilderOutcome {
    Consumed,
    Preview(Theme),
    Save(ThemeBuilderSaveRequest),
    Cancel(Theme),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ThemeBuilderSaveRequest {
    pub(super) name: String,
    pub(super) spec: ThemeSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThemeBuilderSignature {
    pub(super) original: &'static str,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) editing: Option<ThemeBuilderEditSignature>,
    pub(super) message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ThemeBuilderEditSignature {
    Color { field: &'static str, buffer: String },
    Name { buffer: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThemeBuilderLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) swatch: Option<Srgb>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditMode {
    Color {
        field: ThemeField,
        previous: Srgb,
        buffer: String,
    },
    Name {
        buffer: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeField {
    Foreground,
    Background,
    Clear,
    Cursor,
    Selection,
    Search,
    Border,
    Inactive,
    Palette(usize),
}

const FIELDS: [ThemeField; 24] = [
    ThemeField::Foreground,
    ThemeField::Background,
    ThemeField::Clear,
    ThemeField::Cursor,
    ThemeField::Selection,
    ThemeField::Search,
    ThemeField::Border,
    ThemeField::Inactive,
    ThemeField::Palette(0),
    ThemeField::Palette(1),
    ThemeField::Palette(2),
    ThemeField::Palette(3),
    ThemeField::Palette(4),
    ThemeField::Palette(5),
    ThemeField::Palette(6),
    ThemeField::Palette(7),
    ThemeField::Palette(8),
    ThemeField::Palette(9),
    ThemeField::Palette(10),
    ThemeField::Palette(11),
    ThemeField::Palette(12),
    ThemeField::Palette(13),
    ThemeField::Palette(14),
    ThemeField::Palette(15),
];

impl ThemeBuilder {
    pub(super) fn new(settings: &Settings) -> Self {
        Self::from_theme(settings.theme)
    }

    pub(super) fn open(&mut self, settings: &Settings) {
        *self = Self::from_theme(settings.theme);
        self.message = Some(
            "Clone active theme. [/] picks L/C/H, Left/Right adjust, F snaps to AA, Enter types hex, Ctrl+S saves."
                .to_owned(),
        );
    }

    pub(super) fn refresh(&mut self, settings: &Settings) {
        if self.editing.is_none() {
            *self = Self::from_theme(settings.theme);
        }
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ThemeBuilderOutcome {
        if self.editing.is_some() {
            return self.handle_editing_input(input);
        }

        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-6),
            OverlayInput::PageDown => self.move_selection(6),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => self.set_selection(FIELDS.len().saturating_sub(1)),
            OverlayInput::Left => return self.nudge_selected(-1),
            OverlayInput::Right => return self.nudge_selected(1),
            OverlayInput::Char('[') => self.cycle_channel(false),
            OverlayInput::Char(']') => self.cycle_channel(true),
            OverlayInput::Char('f') | OverlayInput::Char('F') => return self.snap_selected(),
            OverlayInput::Activate => self.begin_color_edit(),
            OverlayInput::Save => self.begin_name_edit(),
            OverlayInput::Char('n') | OverlayInput::Char('N') => self.begin_name_edit(),
            OverlayInput::Close => return ThemeBuilderOutcome::Cancel(self.original),
            _ => {}
        }

        ThemeBuilderOutcome::Consumed
    }

    pub(super) fn save_succeeded(&mut self, saved_name: &str, path: &Path, changed: usize) {
        self.spec.name = saved_name.to_owned();
        self.original = self.preview_theme();
        let snap_note = match self.save_snap_count {
            0 => String::new(),
            1 => " Snapped 1 role to AA on save.".to_owned(),
            n => format!(" Snapped {n} roles to AA on save."),
        };
        self.save_snap_count = 0;
        self.message = Some(format!(
            "Saved {saved_name} to {} and odytty.conf ({changed} setting change).{snap_note}",
            path.display()
        ));
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.save_snap_count = 0;
        self.message = Some(format!("Save failed: {message}"));
    }

    pub(super) fn render_signature(&self) -> ThemeBuilderSignature {
        ThemeBuilderSignature {
            original: self.original.name,
            selected: self.selected,
            scroll: self.scroll,
            editing: self.editing.as_ref().map(|editing| match editing {
                EditMode::Color { field, buffer, .. } => ThemeBuilderEditSignature::Color {
                    field: field.key(),
                    buffer: buffer.clone(),
                },
                EditMode::Name { buffer } => ThemeBuilderEditSignature::Name {
                    buffer: buffer.clone(),
                },
            }),
            message: self.message.clone(),
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        72.min(columns)
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ThemeBuilderLine> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        lines.push(ThemeBuilderLine {
            text: ellipsize(
                "  Theme builder - [/] L/C/H, Left/Right adjust, F snap AA, Enter hex, Ctrl+S save",
                body_width,
            ),
            focused: false,
            swatch: None,
        });

        let ratio = contrast_ratio(self.spec.foreground, self.spec.background);
        lines.push(ThemeBuilderLine {
            text: ellipsize(
                &format!(
                    "  name={}  fg/bg contrast={ratio:.2}{}",
                    self.spec.name,
                    if ratio < 4.0 { " below 4.0" } else { "" }
                ),
                body_width,
            ),
            focused: false,
            swatch: None,
        });

        for text in self.readout_lines() {
            lines.push(ThemeBuilderLine {
                text: ellipsize(&text, body_width),
                focused: false,
                swatch: None,
            });
        }

        if let Some(message) = self.message.as_deref() {
            for wrapped in wrap_words(message, body_width.saturating_sub(4)) {
                if lines.len() >= body_height {
                    return lines;
                }
                lines.push(ThemeBuilderLine {
                    text: format!("    {wrapped}"),
                    focused: false,
                    swatch: None,
                });
            }
        }

        self.push_preview_lines(&mut lines, body_width, body_height);

        for (index, field) in FIELDS.iter().enumerate().skip(self.scroll) {
            if lines.len() >= body_height {
                break;
            }
            let focused = index == self.selected;
            let marker = if focused { ">" } else { " " };
            let color = self.color(*field);
            let mut value = hex(color);
            if let Some(EditMode::Color {
                field: editing_field,
                buffer,
                ..
            }) = self.editing.as_ref()
                && editing_field == field
            {
                value = format!("[{buffer}]");
            }
            let text = format!("{marker} {:<12} {value}", field.label());
            lines.push(ThemeBuilderLine {
                text: ellipsize(&text, body_width),
                focused,
                swatch: Some(color),
            });
        }

        lines.truncate(body_height);
        lines
    }

    fn from_theme(theme: Theme) -> Self {
        let mut spec = ThemeSpec::from_theme(&theme);
        spec.name = suggested_name(theme.name);
        spec.appearance = if relative_luminance(theme.background) > 0.18 {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        Self {
            original: theme,
            spec,
            selected: 0,
            scroll: 0,
            editing: None,
            message: None,
            channel: OklchChannel::Lightness,
            save_snap_count: 0,
        }
    }

    fn handle_editing_input(&mut self, input: OverlayInput) -> ThemeBuilderOutcome {
        match input {
            OverlayInput::Close => {
                if let Some(EditMode::Color {
                    field, previous, ..
                }) = self.editing.take()
                {
                    self.set_color(field, previous);
                    self.message = Some(format!("Cancelled edit for {}.", field.label()));
                    return ThemeBuilderOutcome::Preview(self.preview_theme());
                }
                if matches!(self.editing, Some(EditMode::Name { .. })) {
                    self.editing = None;
                    self.message = Some("Cancelled save name.".to_owned());
                }
                ThemeBuilderOutcome::Consumed
            }
            OverlayInput::Activate | OverlayInput::Save => match self.editing.take() {
                Some(EditMode::Color { field, buffer, .. }) => match parse_hex(&buffer) {
                    Some(color) => {
                        self.set_color(field, color);
                        self.message = Some(format!("Applied {} = {}.", field.label(), hex(color)));
                        ThemeBuilderOutcome::Preview(self.preview_theme())
                    }
                    None => {
                        self.editing = Some(EditMode::Color {
                            field,
                            previous: self.color(field),
                            buffer,
                        });
                        self.message = Some("Use #rgb or #rrggbb.".to_owned());
                        ThemeBuilderOutcome::Consumed
                    }
                },
                Some(EditMode::Name { buffer }) => self.save_request(buffer),
                None => ThemeBuilderOutcome::Consumed,
            },
            OverlayInput::Backspace => {
                match self.editing.as_mut() {
                    Some(EditMode::Color { buffer, .. }) | Some(EditMode::Name { buffer }) => {
                        buffer.pop();
                    }
                    None => {}
                }
                ThemeBuilderOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                match self.editing.as_mut() {
                    Some(EditMode::Color { buffer, .. }) => {
                        if ch == '#' || ch.is_ascii_hexdigit() {
                            buffer.push(ch.to_ascii_lowercase());
                        }
                    }
                    Some(EditMode::Name { buffer }) => {
                        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                            buffer.push(ch.to_ascii_lowercase());
                        }
                    }
                    None => {}
                }
                ThemeBuilderOutcome::Consumed
            }
            _ => ThemeBuilderOutcome::Consumed,
        }
    }

    fn begin_color_edit(&mut self) {
        let field = FIELDS[self.selected];
        let color = self.color(field);
        self.editing = Some(EditMode::Color {
            field,
            previous: color,
            buffer: hex(color),
        });
        self.message = Some(format!(
            "Editing {}: type #rrggbb, Enter applies, Esc cancels.",
            field.label()
        ));
    }

    fn begin_name_edit(&mut self) {
        self.editing = Some(EditMode::Name {
            buffer: self.spec.name.clone(),
        });
        self.message =
            Some("Save as: type a theme name, Enter writes .theme, Esc cancels.".to_owned());
    }

    fn save_request(&mut self, name: String) -> ThemeBuilderOutcome {
        let name = name.trim().to_ascii_lowercase();
        if !valid_theme_name(&name) {
            self.editing = Some(EditMode::Name { buffer: name });
            self.message =
                Some("Use letters, numbers, dashes, or underscores; no paths.".to_owned());
            return ThemeBuilderOutcome::Consumed;
        }
        self.spec.name = name.clone();
        self.spec.appearance = if relative_luminance(self.spec.background) > 0.18 {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        // D-U2-2: mandatory save-time backstop — snap every floored role to AA so
        // the written .theme clears the authoring floor by construction, even if
        // the operator never pressed F. The count is reported in save_succeeded.
        self.save_snap_count = self.auto_snap_floored();
        let spec = self.spec.clone();
        ThemeBuilderOutcome::Save(ThemeBuilderSaveRequest { name, spec })
    }

    fn nudge_selected(&mut self, direction: i16) -> ThemeBuilderOutcome {
        let field = FIELDS[self.selected];
        let color = self.color(field);
        let (dl, dc, dh) = self.channel.deltas(direction);
        let nudged = theme_author::nudge(color, dl, dc, dh);
        self.set_color(field, nudged);
        let arrow = if direction < 0 { "-" } else { "+" };
        self.message = Some(format!(
            "{} {} {arrow} -> {}",
            field.label(),
            self.channel.label(),
            hex(nudged)
        ));
        ThemeBuilderOutcome::Preview(self.preview_theme())
    }

    /// `[` / `]` — move the keyboard editor's focus to the previous / next OKLCH
    /// channel. Pure view-state change; the new focus is surfaced via `message`
    /// (in the render signature) so the readout repaints.
    fn cycle_channel(&mut self, forward: bool) {
        self.channel = if forward {
            self.channel.next()
        } else {
            self.channel.prev()
        };
        self.message = Some(format!(
            "Edit channel: {} — Left/Right adjust.",
            self.channel.label()
        ));
    }

    /// `F` — snap the selected role up to the authoring floor (AA 4.5) against
    /// its [`theme_author::floor_partner`]-resolved surface. Inert (and says so)
    /// for roles that are not floored (background, chrome, the background-side
    /// neutral ramp pair); idempotent on roles already clearing the floor.
    fn snap_selected(&mut self) -> ThemeBuilderOutcome {
        let field = FIELDS[self.selected];
        let role = author_role(field);
        match theme_author::partner_color(&self.spec, role) {
            Some(partner) => {
                let snapped = theme_author::snap_to_floor(
                    self.color(field),
                    partner,
                    AUTHORING_CONTRAST_FLOOR,
                );
                self.set_color(field, snapped);
                let ratio = theme_author::authoring_contrast(snapped, partner);
                let surface = floor_surface_label(role, self.spec.appearance).unwrap_or("?");
                self.message = Some(format!(
                    "Snapped {} to AA {AUTHORING_CONTRAST_FLOOR:.1} ({ratio:.2} vs {surface}).",
                    field.label()
                ));
                ThemeBuilderOutcome::Preview(self.preview_theme())
            }
            None => {
                self.message = Some(format!("{} is not floored (no AA target).", field.label()));
                ThemeBuilderOutcome::Consumed
            }
        }
    }

    /// Auto-snap every floored role in the spec up to the authoring floor (the
    /// D-U2-2 save-time backstop), returning how many roles actually moved.
    /// Partners are resolved fresh from the in-progress spec each iteration, so a
    /// foreground-floored fill (selection/search) snaps against the already
    /// floored foreground — `FIELDS` lists foreground before the fills, so the
    /// dependency resolves in one pass.
    fn auto_snap_floored(&mut self) -> usize {
        let mut changed = 0;
        for field in FIELDS {
            let role = author_role(field);
            if let Some(partner) = theme_author::partner_color(&self.spec, role) {
                let before = self.color(field);
                let after = theme_author::snap_to_floor(before, partner, AUTHORING_CONTRAST_FLOOR);
                if after != before {
                    self.set_color(field, after);
                    changed += 1;
                }
            }
        }
        changed
    }

    fn preview_theme(&self) -> Theme {
        self.spec.to_theme_with_name("custom")
    }

    fn color(&self, field: ThemeField) -> Srgb {
        match field {
            ThemeField::Foreground => self.spec.foreground,
            ThemeField::Background => self.spec.background,
            ThemeField::Clear => self.spec.clear,
            ThemeField::Cursor => self.spec.cursor,
            ThemeField::Selection => self.spec.selection,
            ThemeField::Search => self.spec.search,
            ThemeField::Border => self.spec.border,
            ThemeField::Inactive => self.spec.inactive,
            ThemeField::Palette(index) => self.spec.palette[index],
        }
    }

    fn set_color(&mut self, field: ThemeField, color: Srgb) {
        match field {
            ThemeField::Foreground => self.spec.foreground = color,
            ThemeField::Background => self.spec.background = color,
            ThemeField::Clear => self.spec.clear = color,
            ThemeField::Cursor => self.spec.cursor = color,
            ThemeField::Selection => self.spec.selection = color,
            ThemeField::Search => self.spec.search = color,
            ThemeField::Border => self.spec.border = color,
            ThemeField::Inactive => self.spec.inactive = color,
            ThemeField::Palette(index) => self.spec.palette[index] = color,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let next = self.selected as isize + delta;
        self.set_selection(next.clamp(0, FIELDS.len().saturating_sub(1) as isize) as usize);
    }

    fn set_selection(&mut self, selected: usize) {
        self.selected = selected.min(FIELDS.len().saturating_sub(1));
        self.clamp();
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(FIELDS.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let visible_slack = 8;
        if self.selected >= self.scroll + visible_slack {
            self.scroll = self.selected.saturating_sub(visible_slack - 1);
        }
        self.scroll = self.scroll.min(FIELDS.len().saturating_sub(1));
    }

    /// The two U2 readout lines for the currently-selected role: its live
    /// authoring contrast against the floor partner (PASS/FAIL vs the AA 4.5
    /// authoring floor — explicitly NOT the render-time `min_contrast`), and the
    /// active OKLCH editing channel with the selected color's L/C/H values.
    fn readout_lines(&self) -> Vec<String> {
        let field = FIELDS[self.selected];
        let role = author_role(field);
        let color = self.color(field);

        let contrast_line = match theme_author::partner_color(&self.spec, role) {
            Some(partner) => {
                let ratio = theme_author::authoring_contrast(color, partner);
                let surface = floor_surface_label(role, self.spec.appearance).unwrap_or("?");
                let verdict = if ratio >= AUTHORING_CONTRAST_FLOOR {
                    "PASS"
                } else {
                    "FAIL"
                };
                format!(
                    "  {} vs {surface}: {ratio:.2} {verdict} AA {AUTHORING_CONTRAST_FLOOR:.1}",
                    field.label()
                )
            }
            None => format!("  {} not floored (no AA target)", field.label()),
        };

        let (l, c, h) = oklch_of(color);
        let channel_line = format!(
            "  edit {}  L {l:.2} C {c:.3} H {h:.0}",
            self.channel.label()
        );

        vec![contrast_line, channel_line]
    }

    fn push_preview_lines(
        &self,
        lines: &mut Vec<ThemeBuilderLine>,
        body_width: usize,
        body_height: usize,
    ) {
        if lines.len() >= body_height {
            return;
        }
        lines.push(ThemeBuilderLine {
            text: ellipsize(
                "  Preview: Default  Black Red Green Yellow Blue Magenta Cyan White",
                body_width,
            ),
            focused: false,
            swatch: Some(self.spec.foreground),
        });
        if lines.len() < body_height {
            lines.push(ThemeBuilderLine {
                text: ellipsize("  Selection  Cursor  Search  Border  Inactive", body_width),
                focused: false,
                swatch: Some(self.spec.selection),
            });
        }
    }
}

impl ThemeField {
    fn key(self) -> &'static str {
        match self {
            ThemeField::Foreground => "foreground",
            ThemeField::Background => "background",
            ThemeField::Clear => "clear",
            ThemeField::Cursor => "cursor",
            ThemeField::Selection => "selection",
            ThemeField::Search => "search",
            ThemeField::Border => "border",
            ThemeField::Inactive => "inactive",
            ThemeField::Palette(0) => "color0",
            ThemeField::Palette(1) => "color1",
            ThemeField::Palette(2) => "color2",
            ThemeField::Palette(3) => "color3",
            ThemeField::Palette(4) => "color4",
            ThemeField::Palette(5) => "color5",
            ThemeField::Palette(6) => "color6",
            ThemeField::Palette(7) => "color7",
            ThemeField::Palette(8) => "color8",
            ThemeField::Palette(9) => "color9",
            ThemeField::Palette(10) => "color10",
            ThemeField::Palette(11) => "color11",
            ThemeField::Palette(12) => "color12",
            ThemeField::Palette(13) => "color13",
            ThemeField::Palette(14) => "color14",
            ThemeField::Palette(15) => "color15",
            ThemeField::Palette(_) => "color",
        }
    }

    fn label(self) -> &'static str {
        self.key()
    }
}

pub(super) fn save_theme_to_dir(
    theme_dir: &Path,
    request: &ThemeBuilderSaveRequest,
) -> io::Result<PathBuf> {
    fs::create_dir_all(theme_dir)?;
    let path = theme_dir.join(format!("{}.theme", request.name));
    fs::write(&path, request.spec.serialize())?;
    Ok(path)
}

pub(super) fn user_theme_dir_for_config(config_path: &Path) -> Option<PathBuf> {
    config_path.parent().map(|parent| parent.join("themes"))
}

fn suggested_name(name: &str) -> String {
    let base = name.trim().strip_suffix(".theme").unwrap_or(name.trim());
    let mut out = String::new();
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | ' ') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "custom-theme".to_owned()
    } else if out.ends_with("-custom") {
        out.to_owned()
    } else {
        format!("{out}-custom")
    }
}

fn valid_theme_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('.')
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn parse_hex(value: &str) -> Option<Srgb> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.is_empty() || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => Some((
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        )),
        3 => {
            let expand = |slice: &str| u8::from_str_radix(slice, 16).ok().map(|v| v * 17);
            Some((
                expand(&hex[0..1])?,
                expand(&hex[1..2])?,
                expand(&hex[2..3])?,
            ))
        }
        _ => None,
    }
}

fn hex((r, g, b): Srgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Map a builder [`ThemeField`] to the U2 [`AuthorRole`] used to resolve its
/// floor partner. The palette index carries through unchanged so the
/// appearance-dependent neutral-pair exemption in
/// [`theme_author::floor_partner`] applies to the right slots.
fn author_role(field: ThemeField) -> AuthorRole {
    match field {
        ThemeField::Foreground => AuthorRole::Foreground,
        ThemeField::Background => AuthorRole::Background,
        ThemeField::Clear => AuthorRole::Clear,
        ThemeField::Cursor => AuthorRole::Cursor,
        ThemeField::Selection => AuthorRole::Selection,
        ThemeField::Search => AuthorRole::Search,
        ThemeField::Border => AuthorRole::Border,
        ThemeField::Inactive => AuthorRole::Inactive,
        ThemeField::Palette(index) => AuthorRole::Palette(index),
    }
}

/// A short label ("fg" / "bg") for the surface a role floors against, or `None`
/// when the role is not floored.
fn floor_surface_label(role: AuthorRole, appearance: Appearance) -> Option<&'static str> {
    theme_author::floor_partner(role, appearance).map(|against| match against {
        FloorAgainst::Background => "bg",
        FloorAgainst::Foreground => "fg",
    })
}

/// Decode an sRGB byte triple to OKLCH for the readout: lightness `[0,1]`,
/// chroma, and hue in degrees `[0,360)`. Read-only display helper over the
/// shared `color` conversions — the editing math always routes through
/// [`theme_author::nudge`], never this.
fn oklch_of((r, g, b): Srgb) -> (f32, f32, f32) {
    let lch = oklab_to_oklch(linear_to_oklab([
        srgb_to_linear(r),
        srgb_to_linear(g),
        srgb_to_linear(b),
    ]));
    (lch.l, lch.c, lch.h.to_degrees().rem_euclid(360.0))
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(12);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let separator = usize::from(!current.is_empty());
        if current.chars().count() + separator + word.chars().count() > width {
            if !current.is_empty() {
                lines.push(current);
                current = String::new();
            }
            if word.chars().count() > width {
                lines.push(ellipsize(word, width));
                continue;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "~".to_owned();
    }
    let mut out = text.chars().take(width - 1).collect::<String>();
    out.push('~');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-theme-builder-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn edit_state_machine_previews_color_changes() {
        let mut builder = ThemeBuilder::new(&Settings::default());
        assert_eq!(
            builder.handle_input(OverlayInput::Activate),
            ThemeBuilderOutcome::Consumed
        );
        for _ in 0..7 {
            builder.handle_input(OverlayInput::Backspace);
        }
        for ch in "#123456".chars() {
            builder.handle_input(OverlayInput::Char(ch));
        }

        let ThemeBuilderOutcome::Preview(theme) = builder.handle_input(OverlayInput::Activate)
        else {
            panic!("expected preview");
        };

        assert_eq!(theme.foreground, (0x12, 0x34, 0x56));
        assert_eq!(builder.render_signature().editing, None);
    }

    #[test]
    fn serialize_round_trips_to_valid_theme() {
        let mut builder = ThemeBuilder::new(&Settings::default());
        builder.handle_input(OverlayInput::Save);
        for _ in 0..builder
            .render_signature()
            .editing
            .as_ref()
            .and_then(|edit| match edit {
                ThemeBuilderEditSignature::Name { buffer } => Some(buffer.len()),
                _ => None,
            })
            .unwrap()
        {
            builder.handle_input(OverlayInput::Backspace);
        }
        for ch in "my-theme".chars() {
            builder.handle_input(OverlayInput::Char(ch));
        }
        let ThemeBuilderOutcome::Save(request) = builder.handle_input(OverlayInput::Activate)
        else {
            panic!("expected save request");
        };

        let reparsed = ThemeSpec::parse(&request.spec.serialize(), |m| panic!("warn: {m}"));
        assert_eq!(reparsed, request.spec);
        assert_eq!(request.name, "my-theme");
    }

    #[test]
    fn save_writes_to_injected_temp_dir() {
        let dir = temp_dir("save");
        let mut spec = ThemeSpec::from_theme(&Theme::ODYSSEY);
        spec.name = "test-theme".to_owned();
        let request = ThemeBuilderSaveRequest {
            name: "test-theme".to_owned(),
            spec: spec.clone(),
        };

        let path = save_theme_to_dir(&dir, &request).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let reparsed = ThemeSpec::parse(&contents, |m| panic!("warn: {m}"));

        assert_eq!(path, dir.join("test-theme.theme"));
        assert_eq!(reparsed, spec);
        assert!(!contents.contains("/home/"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancel_restores_original_theme() {
        let settings = Settings {
            theme: Theme::ODYSSEY,
            ..Settings::default()
        };
        let mut builder = ThemeBuilder::new(&settings);
        let ThemeBuilderOutcome::Preview(_) = builder.handle_input(OverlayInput::Right) else {
            panic!("expected preview");
        };

        let ThemeBuilderOutcome::Cancel(theme) = builder.handle_input(OverlayInput::Close) else {
            panic!("expected cancel");
        };

        assert_eq!(theme, Theme::ODYSSEY);
    }

    #[test]
    fn arrows_drive_the_focused_oklch_channel_via_core_nudge() {
        let mut b = ThemeBuilder::new(&Settings::default());
        let field = FIELDS[b.selected];

        // Default channel = Lightness: Right == core nudge with +L_STEP only.
        let start = b.color(field);
        b.handle_input(OverlayInput::Right);
        assert_eq!(b.color(field), theme_author::nudge(start, L_STEP, 0.0, 0.0));

        // `]` cycles to Chroma: Right == +C_STEP only.
        b.handle_input(OverlayInput::Char(']'));
        let before_c = b.color(field);
        b.handle_input(OverlayInput::Right);
        assert_eq!(
            b.color(field),
            theme_author::nudge(before_c, 0.0, C_STEP, 0.0)
        );

        // `]` cycles to Hue: Right == +H_STEP rotation only.
        b.handle_input(OverlayInput::Char(']'));
        let before_h = b.color(field);
        b.handle_input(OverlayInput::Right);
        assert_eq!(
            b.color(field),
            theme_author::nudge(before_h, 0.0, 0.0, H_STEP_DEG.to_radians())
        );

        // Left is the negative delta of the focused channel.
        let before_neg = b.color(field);
        b.handle_input(OverlayInput::Left);
        assert_eq!(
            b.color(field),
            theme_author::nudge(before_neg, 0.0, 0.0, -H_STEP_DEG.to_radians())
        );
    }

    #[test]
    fn channel_cycles_both_directions() {
        let mut b = ThemeBuilder::new(&Settings::default());
        assert_eq!(b.channel, OklchChannel::Lightness);
        b.handle_input(OverlayInput::Char(']'));
        assert_eq!(b.channel, OklchChannel::Chroma);
        b.handle_input(OverlayInput::Char(']'));
        assert_eq!(b.channel, OklchChannel::Hue);
        b.handle_input(OverlayInput::Char(']'));
        assert_eq!(b.channel, OklchChannel::Lightness); // wraps
        b.handle_input(OverlayInput::Char('['));
        assert_eq!(b.channel, OklchChannel::Hue); // wraps backward
    }

    #[test]
    fn snap_lifts_failing_floored_role_to_aa_and_is_idempotent() {
        let mut b = ThemeBuilder::new(&Settings::default());
        // Sabotage foreground to equal background (contrast ~1, fails AA).
        let bg = b.spec.background;
        b.set_color(ThemeField::Foreground, bg);
        b.selected = 0; // foreground

        let out = b.snap_selected();
        assert!(matches!(out, ThemeBuilderOutcome::Preview(_)));

        let partner = theme_author::partner_color(&b.spec, AuthorRole::Foreground).unwrap();
        let lifted = b.color(ThemeField::Foreground);
        assert!(theme_author::authoring_contrast(lifted, partner) >= AUTHORING_CONTRAST_FLOOR);

        // Idempotent: a second snap is a byte no-op.
        b.snap_selected();
        assert_eq!(b.color(ThemeField::Foreground), lifted);
    }

    #[test]
    fn snap_is_inert_on_not_floored_roles() {
        let mut b = ThemeBuilder::new(&Settings::default());
        // Background is never floored.
        b.selected = FIELDS
            .iter()
            .position(|f| matches!(f, ThemeField::Background))
            .unwrap();
        let before = b.color(ThemeField::Background);
        let out = b.snap_selected();
        assert_eq!(out, ThemeBuilderOutcome::Consumed);
        assert_eq!(b.color(ThemeField::Background), before);
    }

    #[test]
    fn readout_reflects_pass_fail_and_not_floored() {
        let mut b = ThemeBuilder::new(&Settings::default());

        // Foreground floored against bg: force a fail, then snap to pass.
        let bg = b.spec.background;
        b.set_color(ThemeField::Foreground, bg);
        b.selected = 0;
        assert!(b.readout_lines()[0].contains("FAIL"));
        b.snap_selected();
        assert!(b.readout_lines()[0].contains("PASS"));

        // Border is not floored.
        b.selected = FIELDS
            .iter()
            .position(|f| matches!(f, ThemeField::Border))
            .unwrap();
        assert!(b.readout_lines()[0].contains("not floored"));
    }

    #[test]
    fn save_auto_snaps_every_floored_role_to_aa() {
        let mut b = ThemeBuilder::new(&Settings::default());
        // Sabotage a chromatic palette slot to fail AA against the background.
        let bg = b.spec.background;
        b.set_color(ThemeField::Palette(1), bg);

        let ThemeBuilderOutcome::Save(req) = b.save_request("mytheme".to_owned()) else {
            panic!("expected save request");
        };
        assert_eq!(req.name, "mytheme");
        assert!(b.save_snap_count >= 1);

        // Every floored role in the saved spec now clears the authoring floor.
        for field in FIELDS {
            let role = author_role(field);
            if let Some(partner) = theme_author::partner_color(&b.spec, role) {
                let ratio = theme_author::authoring_contrast(b.color(field), partner);
                assert!(
                    ratio >= AUTHORING_CONTRAST_FLOOR,
                    "{field:?} only reached {ratio:.2}"
                );
            }
        }

        // The save confirmation reports the backstop.
        b.save_succeeded("mytheme", Path::new("/tmp/mytheme.theme"), 1);
        assert!(b.message.as_deref().unwrap().contains("Snapped"));
    }

    #[test]
    fn hex_entry_still_applies_verbatim_without_snap() {
        let mut b = ThemeBuilder::new(&Settings::default());
        b.selected = 0; // foreground
        let bg = b.spec.background;
        // Type the background hex into the foreground: a deliberate low-contrast
        // expert choice that must be applied verbatim (no auto-snap on entry).
        b.handle_input(OverlayInput::Activate);
        for _ in 0..8 {
            b.handle_input(OverlayInput::Backspace);
        }
        for ch in hex(bg).chars() {
            b.handle_input(OverlayInput::Char(ch));
        }
        let ThemeBuilderOutcome::Preview(_) = b.handle_input(OverlayInput::Activate) else {
            panic!("expected preview");
        };
        assert_eq!(b.color(ThemeField::Foreground), bg); // verbatim, not snapped
    }
}
