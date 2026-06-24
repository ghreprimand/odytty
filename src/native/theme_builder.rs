// SPDX-License-Identifier: GPL-3.0-only
use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::color::{linear_to_oklab, oklab_to_oklch, srgb_to_linear};
use crate::palette_gen;
use crate::settings::Settings;
use crate::theme::{Appearance, Srgb, Theme, ThemeSpec, contrast_ratio, relative_luminance};
use crate::theme_author::{self, AUTHORING_CONTRAST_FLOOR, AuthorRole, FloorAgainst};

use super::overlay::{OverlayInput, PointerButton};

#[derive(Debug, Clone)]
pub(super) struct ThemeBuilder {
    original: Theme,
    spec: ThemeSpec,
    selected: usize,
    scroll: usize,
    editing: Option<EditMode>,
    message: Option<String>,
    /// Which OKLCH channel the Left/Right arrows currently drive, and which
    /// channel token a pointer slider drag targets (U2). View state, but it
    /// changes the rendered readout/slider highlight independently of `message`,
    /// so it is carried in [`ThemeBuilderSignature`] to drive repaint.
    channel: OklchChannel,
    /// How many floored roles the last save auto-snapped to AA (D-U2-2), carried
    /// from [`ThemeBuilder::save_request`] into [`ThemeBuilder::save_succeeded`]
    /// so the saved-confirmation message can report the backstop.
    save_snap_count: usize,
    /// Whether a pointer slider drag on the focused-channel track is in progress
    /// (U2 Step 2/3). Internal view state only; the App gates per-move routing on
    /// [`ThemeBuilder::is_dragging`] so ordinary hover stays cheap, exactly like
    /// the settings-panel slider.
    dragging_channel: bool,
    /// How many role rows the last [`ThemeBuilder::build_rows`] render actually
    /// fit below the fixed header/preview block (OVERLAY-SMALL-WINDOW). The role
    /// list is the only windowed region, and the header count is variable (the
    /// hint/message wrap, the conditional preview rows), so this capacity cannot
    /// be re-derived from `body_height` alone. Interior-mutable because only the
    /// render pass knows it, yet `clamp` (keyboard nav, `&mut self`) must follow
    /// the selection through the *real* visible window — replacing the old
    /// hardcoded `slack = 8` that over-scrolled on short overlays. `0` until the
    /// first render; the render pass always runs before `scroll_indicator`.
    last_role_capacity: Cell<usize>,
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

/// The chroma value mapped to the right end of the chroma slider track. OKLCH
/// chroma in the sRGB gamut peaks near ~0.37, so this spans the usable range
/// without a dead zone; values past it gamut-map back down inside
/// [`theme_author::nudge`].
const C_MAX: f32 = 0.37;

/// Slider track geometry (U2 Step 2/3), mirroring the settings panel: the track
/// grows to fill the value area between these widths; below the minimum the
/// channel row falls back to a plain (keyboard-only) readout line.
const MIN_SLIDER_TRACK: usize = 8;
const MAX_SLIDER_TRACK: usize = 24;
/// A fixed readout budget reserved to the right of the track so the track does
/// not jump as the channel value's text width changes during a drag.
const CHANNEL_READOUT_W: usize = 7;
/// Track groove and thumb glyphs — the same family the settings slider uses, so
/// they render reliably in the overlay.
const SLIDER_GROOVE: char = '─';
const SLIDER_THUMB: char = '█';

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

    /// The single-letter label for the compact channel picker tokens.
    fn short(self) -> &'static str {
        match self {
            OklchChannel::Lightness => "L",
            OklchChannel::Chroma => "C",
            OklchChannel::Hue => "H",
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
    /// The focused OKLCH channel's label (U2 Step 2/3). In the signature so the
    /// channel picker and slider repaint when the focus changes even if the
    /// message did not — the keyboard path's repaint no longer rides on always
    /// mutating `message`.
    pub(super) channel: &'static str,
    /// The currently-selected role's color (U2 Step 2/3). In the signature so the
    /// slider thumb, the channel value readout, and the selected field's
    /// swatch/hex repaint as a slider drag (or hex entry) moves the color.
    pub(super) selected_color: Srgb,
}

/// The role of one rendered builder body line, used to dispatch a pointer press
/// (U2 Step 2/3). Produced in lockstep with the rendered text by
/// [`ThemeBuilder::build_rows`] so the drawn geometry and the hit-test geometry
/// can never drift — the same pattern the settings panel uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuilderZone {
    /// A header / message / preview line — no pointer action.
    Inert,
    /// A role row: a click focuses that `FIELDS` index.
    Field(usize),
    /// The compact channel picker: a click on one of the L/C/H tokens focuses
    /// that channel. Columns are body-relative (0 = first body cell).
    ChannelPick {
        l_x0: usize,
        c_x0: usize,
        h_x0: usize,
        tok_w: usize,
    },
    /// The focused-channel slider track: a click/drag sets the channel value.
    /// Columns are body-relative.
    Slider { track_x0: usize, track_w: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ThemeBuilderEditSignature {
    Color { field: &'static str, buffer: String },
    Name { buffer: String },
    Seed { buffer: String },
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
    /// Seed entry for contrast-aware generation (U3): the user types a single
    /// accent hex, and Enter feeds it to [`palette_gen::generate`] to author a
    /// complete, AA-by-construction [`ThemeSpec`] loaded as the live editing
    /// target. Shares the hex-entry keystroke handling with [`EditMode::Color`].
    Seed {
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
            "Clone active theme. Tab or [/] picks L/C/H, Left/Right or drag adjust, F snaps to AA, G generates from a seed, Enter types hex, Ctrl+S saves."
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
            OverlayInput::Tab => self.cycle_channel(true),
            OverlayInput::Char('f') | OverlayInput::Char('F') => return self.snap_selected(),
            OverlayInput::Char('g') | OverlayInput::Char('G') => self.begin_seed_edit(),
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
                EditMode::Seed { buffer } => ThemeBuilderEditSignature::Seed {
                    buffer: buffer.clone(),
                },
            }),
            message: self.message.clone(),
            channel: self.channel.label(),
            selected_color: self.color(FIELDS[self.selected]),
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
        self.build_rows(body_width, body_height)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    /// The cell→row hit-map for the current body geometry (U2 Step 2/3), aligned
    /// 1:1 with [`ThemeBuilder::visible_lines`] (index = body row offset from the
    /// first body cell), so a pointer press resolves to exactly the zone drawn at
    /// that row.
    fn visible_hit_map(&self, body_width: usize, body_height: usize) -> Vec<BuilderZone> {
        self.build_rows(body_width, body_height)
            .into_iter()
            .map(|(_, zone)| zone)
            .collect()
    }

    /// The single source of truth for the builder body: emits each rendered line
    /// paired with its pointer hit zone. Both [`ThemeBuilder::visible_lines`] and
    /// [`ThemeBuilder::visible_hit_map`] project from this, so the rendered
    /// geometry and the hit-test geometry are identical by construction.
    fn build_rows(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<(ThemeBuilderLine, BuilderZone)> {
        let mut rows: Vec<(ThemeBuilderLine, BuilderZone)> = Vec::new();
        // Default to "no role rows fit" so an early return (degenerate geometry,
        // or a message that fills the body) cannot leave a stale capacity behind;
        // the real value is recorded just before the role loop below.
        self.last_role_capacity.set(0);
        if body_width == 0 || body_height == 0 {
            return rows;
        }

        let inert = |text: String, swatch: Option<Srgb>| {
            (
                ThemeBuilderLine {
                    text,
                    focused: false,
                    swatch,
                },
                BuilderZone::Inert,
            )
        };

        rows.push(inert(
            ellipsize(
                "  Theme builder - Tab/[ ] L/C/H, Left/Right or drag adjust, F snap AA, G generate, Ctrl+S save",
                body_width,
            ),
            None,
        ));

        let ratio = contrast_ratio(self.spec.foreground, self.spec.background);
        rows.push(inert(
            ellipsize(
                &format!(
                    "  name={}  fg/bg contrast={ratio:.2}{}",
                    self.spec.name,
                    if ratio < 4.0 { " below 4.0" } else { "" }
                ),
                body_width,
            ),
            None,
        ));

        // Selected-role contrast readout (inert), then the channel picker and the
        // focused-channel slider — the two new mouse-driven controls.
        rows.push(inert(
            ellipsize(&self.contrast_readout_line(), body_width),
            None,
        ));

        let (pick_text, l_x0, c_x0, h_x0, tok_w) = self.channel_picker_line();
        rows.push((
            ThemeBuilderLine {
                text: ellipsize(&pick_text, body_width),
                focused: false,
                swatch: None,
            },
            BuilderZone::ChannelPick {
                l_x0,
                c_x0,
                h_x0,
                tok_w,
            },
        ));

        match self.channel_slider_line(body_width) {
            Some((text, track_x0, track_w)) => rows.push((
                ThemeBuilderLine {
                    text,
                    focused: false,
                    swatch: None,
                },
                BuilderZone::Slider { track_x0, track_w },
            )),
            None => rows.push(inert(
                ellipsize(&self.channel_text_line(), body_width),
                None,
            )),
        }

        if let Some(message) = self.message.as_deref() {
            for wrapped in wrap_words(message, body_width.saturating_sub(4)) {
                if rows.len() >= body_height {
                    rows.truncate(body_height);
                    return rows;
                }
                rows.push(inert(format!("    {wrapped}"), None));
            }
        }

        if rows.len() < body_height {
            rows.push(inert(
                ellipsize(
                    "  Preview: Default  Black Red Green Yellow Blue Magenta Cyan White",
                    body_width,
                ),
                Some(self.spec.foreground),
            ));
        }
        if rows.len() < body_height {
            rows.push(inert(
                ellipsize("  Selection  Cursor  Search  Border  Inactive", body_width),
                Some(self.spec.selection),
            ));
        }

        // Record how many role rows actually fit below the (variable) header /
        // preview block, so keyboard nav's `clamp` follows the selection through
        // the real visible window and `scroll_indicator` can flag overflow. This
        // is the single place that knows the true role-list capacity.
        self.last_role_capacity
            .set(body_height.saturating_sub(rows.len()));

        for (index, field) in FIELDS.iter().enumerate().skip(self.scroll) {
            if rows.len() >= body_height {
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
            rows.push((
                ThemeBuilderLine {
                    text: ellipsize(&text, body_width),
                    focused,
                    swatch: Some(color),
                },
                BuilderZone::Field(index),
            ));
        }

        rows.truncate(body_height);
        rows
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
            dragging_channel: false,
            last_role_capacity: Cell::new(0),
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
                } else if matches!(self.editing, Some(EditMode::Seed { .. })) {
                    self.editing = None;
                    self.message = Some("Cancelled generate.".to_owned());
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
                Some(EditMode::Seed { buffer }) => match parse_hex(&buffer) {
                    Some(seed) => self.generate_from_seed(seed),
                    None => {
                        self.editing = Some(EditMode::Seed { buffer });
                        self.message = Some("Use #rgb or #rrggbb for the seed.".to_owned());
                        ThemeBuilderOutcome::Consumed
                    }
                },
                None => ThemeBuilderOutcome::Consumed,
            },
            OverlayInput::Backspace => {
                match self.editing.as_mut() {
                    Some(EditMode::Color { buffer, .. })
                    | Some(EditMode::Name { buffer })
                    | Some(EditMode::Seed { buffer }) => {
                        buffer.pop();
                    }
                    None => {}
                }
                ThemeBuilderOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                match self.editing.as_mut() {
                    Some(EditMode::Color { buffer, .. }) | Some(EditMode::Seed { buffer }) => {
                        if ch == '#' || ch.is_ascii_hexdigit() {
                            buffer.push(ch.to_ascii_lowercase());
                        }
                    }
                    // A match guard here would make the arm non-exhaustive
                    // (a `Name` with a rejected char would fall through), so
                    // keep the inner filter as an `if`.
                    #[allow(clippy::collapsible_match)]
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

    /// Begin seed-entry for contrast-aware generation (U3): pre-fill the buffer
    /// with the current cursor accent as a sensible default so Enter alone
    /// regenerates around the existing accent.
    fn begin_seed_edit(&mut self) {
        self.editing = Some(EditMode::Seed {
            buffer: hex(self.spec.cursor),
        });
        self.message = Some(
            "Generate from seed: type an accent #rrggbb, Enter authors a readable theme, Esc cancels."
                .to_owned(),
        );
    }

    /// Author a complete theme from a single seed accent (U3): hand the seed,
    /// the current appearance polarity, and the authoring floor to
    /// [`palette_gen::generate`], then load the RV1-validated result as the live
    /// editing target. The generated spec clears the AA floor for every floored
    /// role by construction, so the contrast readout reads PASS (or "not
    /// floored") on every role immediately after; the user then tweaks it with
    /// the same OKLCH sliders/keyboard. The in-progress save name is preserved so
    /// generation does not silently rename the draft.
    fn generate_from_seed(&mut self, seed: Srgb) -> ThemeBuilderOutcome {
        let appearance = self.spec.appearance;
        let name = self.spec.name.clone();
        let mut spec = palette_gen::generate(seed, appearance, AUTHORING_CONTRAST_FLOOR);
        spec.name = name;
        self.spec = spec;
        self.editing = None;
        self.selected = 0;
        self.scroll = 0;
        self.channel = OklchChannel::default();
        self.message = Some(format!(
            "Generated {} theme from seed {}. Every role clears AA by construction; tweak with the sliders, Ctrl+S to save.",
            appearance_label(appearance),
            hex(seed),
        ));
        ThemeBuilderOutcome::Preview(self.preview_theme())
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

    /// `Tab` / `[` / `]` — move the keyboard editor's focus to the previous /
    /// next OKLCH channel.
    fn cycle_channel(&mut self, forward: bool) {
        let next = if forward {
            self.channel.next()
        } else {
            self.channel.prev()
        };
        self.set_channel(next);
    }

    /// Focus a specific OKLCH channel (shared by the keyboard cycle and the
    /// pointer channel-picker click). `channel` is in the render signature, so
    /// this repaints the picker/slider; the message is feedback, not the repaint
    /// trigger.
    fn set_channel(&mut self, channel: OklchChannel) {
        self.channel = channel;
        self.message = Some(format!(
            "Edit channel: {} — Left/Right or drag adjust.",
            channel.label()
        ));
    }

    /// Whether a pointer slider drag is in progress (U2 Step 2/3). The App gates
    /// per-move routing on this so ordinary hover over the open builder stays a
    /// cheap no-op.
    pub(super) fn is_dragging(&self) -> bool {
        self.dragging_channel
    }

    /// End a pointer slider drag (U2 Step 2/3), called on pointer release and on
    /// the focus-loss / close cleanups.
    pub(super) fn end_channel_drag(&mut self) {
        self.dragging_channel = false;
    }

    /// Free pointer-driven scroll of the role list (U2 Step 2/3): move the
    /// viewport by `delta` rows without moving `selected`, mirroring the settings
    /// panel's wheel scroll. The next keyboard navigation re-clamps scroll to the
    /// selection.
    pub(super) fn scroll_lines(&mut self, delta: isize) {
        let max = FIELDS.len().saturating_sub(1) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max) as usize;
    }

    /// Hidden role rows above / below the visible window, for the shared ▲/▼
    /// scroll affordance (OVERLAY-SMALL-WINDOW). The role list is the only
    /// windowed region; its capacity is whatever [`ThemeBuilder::build_rows`]
    /// recorded for the current geometry — the render pass always runs before
    /// this, so the memoized value is fresh. `body_height` is unused because the
    /// header block above the list is variable; the recorded capacity already
    /// accounts for it. `(false, false)` whenever every role fits, so a tall
    /// builder draws no arrows and stays byte-identical.
    pub(super) fn scroll_indicator(&self, _body_height: usize) -> (bool, bool) {
        let capacity = self.last_role_capacity.get();
        if capacity == 0 || FIELDS.len() <= capacity {
            return (false, false);
        }
        (self.scroll > 0, self.scroll + capacity < FIELDS.len())
    }

    /// Handle a left/right press inside the builder body (U2 Step 2/3).
    /// `row_in_body` / `col_in_body` are 0-based offsets from the first body cell.
    /// A click on a role row focuses it; a click on an L/C/H picker token focuses
    /// that channel; a left press/drag on the slider track sets the focused
    /// channel value through the same [`theme_author::nudge`] seam the keyboard
    /// uses. Right-click on the slider is inert (no value verb).
    pub(super) fn handle_pointer_press(
        &mut self,
        body_width: usize,
        body_height: usize,
        row_in_body: usize,
        col_in_body: usize,
        button: PointerButton,
    ) -> ThemeBuilderOutcome {
        self.dragging_channel = false;
        let zones = self.visible_hit_map(body_width, body_height);
        let Some(zone) = zones.get(row_in_body).copied() else {
            return ThemeBuilderOutcome::Consumed;
        };
        match zone {
            BuilderZone::Inert => ThemeBuilderOutcome::Consumed,
            BuilderZone::Field(index) => {
                // A click away from an in-progress hex/name edit abandons it
                // (the mouse analogue of Esc), then focuses the clicked role.
                self.editing = None;
                self.set_selection(index);
                ThemeBuilderOutcome::Consumed
            }
            BuilderZone::ChannelPick {
                l_x0,
                c_x0,
                h_x0,
                tok_w,
            } => {
                if let Some(channel) = channel_at_col(l_x0, c_x0, h_x0, tok_w, col_in_body) {
                    self.editing = None;
                    self.set_channel(channel);
                }
                ThemeBuilderOutcome::Consumed
            }
            BuilderZone::Slider { track_x0, track_w } => {
                if button == PointerButton::Right {
                    return ThemeBuilderOutcome::Consumed;
                }
                self.editing = None;
                self.dragging_channel = true;
                self.set_channel_fraction(fraction_at(track_x0, track_w, col_in_body))
            }
        }
    }

    /// Continue an in-progress slider drag (U2 Step 2/3): map the cursor column
    /// to a channel value. Track geometry is recomputed from the shared row
    /// walker each move, so a resize mid-drag can never desync it.
    pub(super) fn handle_pointer_drag(
        &mut self,
        body_width: usize,
        _body_height: usize,
        col_in_body: usize,
    ) -> ThemeBuilderOutcome {
        if !self.dragging_channel {
            return ThemeBuilderOutcome::Consumed;
        }
        let Some((_, track_x0, track_w)) = self.channel_slider_line(body_width) else {
            return ThemeBuilderOutcome::Consumed;
        };
        self.set_channel_fraction(fraction_at(track_x0, track_w, col_in_body))
    }

    /// Set the selected role's focused OKLCH channel to `fraction` of its range
    /// (`0..=1`), via a delta through [`theme_author::nudge`] so the gamut map and
    /// quantization match the keyboard editor exactly — the slider and the arrows
    /// share one math path.
    fn set_channel_fraction(&mut self, fraction: f32) -> ThemeBuilderOutcome {
        let field = FIELDS[self.selected];
        let color = self.color(field);
        let (l, c, h_deg) = oklch_of(color);
        let fraction = fraction.clamp(0.0, 1.0);
        let (dl, dc, dh) = match self.channel {
            OklchChannel::Lightness => (fraction - l, 0.0, 0.0),
            OklchChannel::Chroma => (0.0, fraction * C_MAX - c, 0.0),
            OklchChannel::Hue => {
                let target_deg = fraction * 360.0;
                (0.0, 0.0, (target_deg - h_deg).to_radians())
            }
        };
        let nudged = theme_author::nudge(color, dl, dc, dh);
        self.set_color(field, nudged);
        self.message = Some(format!(
            "{} {} -> {}",
            field.label(),
            self.channel.label(),
            hex(nudged)
        ));
        ThemeBuilderOutcome::Preview(self.preview_theme())
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
        // Follow the selection through the *real* visible window recorded by the
        // last render, not a fixed slack: on a short overlay the role list fits
        // far fewer than 8 rows, so the old constant scrolled the selection off
        // screen (the ThemeBuilder half of OVERLAY-SMALL-WINDOW). `max(1)` keeps
        // the selection visible before the first render seeds the capacity.
        let visible_slack = self.last_role_capacity.get().max(1);
        if self.selected >= self.scroll + visible_slack {
            self.scroll = self.selected.saturating_sub(visible_slack - 1);
        }
        self.scroll = self.scroll.min(FIELDS.len().saturating_sub(1));
    }

    /// The selected role's live authoring contrast against its floor partner
    /// (PASS/FAIL vs the AA 4.5 authoring floor — explicitly NOT the render-time
    /// `min_contrast`), or a "not floored" note for unfloored roles.
    fn contrast_readout_line(&self) -> String {
        let field = FIELDS[self.selected];
        let role = author_role(field);
        let color = self.color(field);
        match theme_author::partner_color(&self.spec, role) {
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
        }
    }

    /// The plain (keyboard-only) channel readout used when the panel is too
    /// narrow for a usable slider track: the active channel plus the selected
    /// color's L/C/H values.
    fn channel_text_line(&self) -> String {
        let (l, c, h) = oklch_of(self.color(FIELDS[self.selected]));
        format!(
            "  edit {}  L {l:.2} C {c:.3} H {h:.0}",
            self.channel.label()
        )
    }

    /// The two U2 readout strings for the currently-selected role (the contrast
    /// verdict and the channel/LCH values). Retained as the test-facing view of
    /// the readout; the rendered overlay projects the same data through
    /// [`ThemeBuilder::build_rows`].
    #[cfg(test)]
    fn readout_lines(&self) -> Vec<String> {
        vec![self.contrast_readout_line(), self.channel_text_line()]
    }

    /// The compact channel picker line and the body-relative column of each L/C/H
    /// token (`l_x0`, `c_x0`, `h_x0`) for hit-testing. The focused channel renders
    /// bracketed; each token is `tok_w` columns wide.
    fn channel_picker_line(&self) -> (String, usize, usize, usize, usize) {
        const PREFIX: &str = "  Channel: ";
        let tok_w = 3;
        let mut text = String::from(PREFIX);
        let mut offs = [0usize; 3];
        for (i, channel) in [
            OklchChannel::Lightness,
            OklchChannel::Chroma,
            OklchChannel::Hue,
        ]
        .into_iter()
        .enumerate()
        {
            offs[i] = text.chars().count();
            let label = channel.short();
            if self.channel == channel {
                text.push_str(&format!("[{label}]"));
            } else {
                text.push_str(&format!(" {label} "));
            }
            text.push(' ');
        }
        (text, offs[0], offs[1], offs[2], tok_w)
    }

    /// The focused-channel slider line and its body-relative track geometry
    /// (`track_x0`, `track_w`), or `None` when the panel is too narrow for a
    /// usable track (the caller falls back to [`ThemeBuilder::channel_text_line`],
    /// preserving the keyboard path). The readout budget is fixed so the track
    /// does not jump as the value's text width changes during a drag.
    fn channel_slider_line(&self, body_width: usize) -> Option<(String, usize, usize)> {
        let (value_str, fraction) = self.channel_value_and_fraction(FIELDS[self.selected]);
        let prefix = format!("  {}  ", self.channel.label());
        let prefix_w = prefix.chars().count();

        let remaining = body_width.checked_sub(prefix_w)?;
        let track_avail = remaining.checked_sub(1 + CHANNEL_READOUT_W)?;
        if track_avail < MIN_SLIDER_TRACK {
            return None;
        }
        let track_w = track_avail.min(MAX_SLIDER_TRACK);
        let track_x0 = prefix_w;
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
        Some((format!("{prefix}{track} {value_str}"), track_x0, track_w))
    }

    /// The focused channel's display string and its position as a `0..=1`
    /// fraction of the channel's slider range, for the readout and the thumb.
    fn channel_value_and_fraction(&self, field: ThemeField) -> (String, f32) {
        let (l, c, h) = oklch_of(self.color(field));
        match self.channel {
            OklchChannel::Lightness => (format!("L {l:.2}"), l.clamp(0.0, 1.0)),
            OklchChannel::Chroma => (format!("C {c:.3}"), (c / C_MAX).clamp(0.0, 1.0)),
            OklchChannel::Hue => (format!("H {h:.0}"), (h / 360.0).clamp(0.0, 1.0)),
        }
    }
}

/// Resolve a body-relative column on the channel picker line to the channel
/// whose token it falls within, or `None` between tokens.
fn channel_at_col(
    l_x0: usize,
    c_x0: usize,
    h_x0: usize,
    tok_w: usize,
    col: usize,
) -> Option<OklchChannel> {
    if col >= l_x0 && col < l_x0 + tok_w {
        Some(OklchChannel::Lightness)
    } else if col >= c_x0 && col < c_x0 + tok_w {
        Some(OklchChannel::Chroma)
    } else if col >= h_x0 && col < h_x0 + tok_w {
        Some(OklchChannel::Hue)
    } else {
        None
    }
}

/// Map a body-relative column on a slider track to a `0..=1` fraction. Columns
/// left of / right of the track saturate to `0` / `1`, mirroring how the
/// settings slider and the selection drag saturate past an edge.
fn fraction_at(track_x0: usize, track_w: usize, col: usize) -> f32 {
    if track_w <= 1 {
        0.0
    } else {
        ((col as f32 - track_x0 as f32) / (track_w - 1) as f32).clamp(0.0, 1.0)
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

/// The lowercase polarity word used in the generate-confirmation message (U3).
fn appearance_label(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
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
#[path = "theme_builder_tests.rs"]
mod tests;
