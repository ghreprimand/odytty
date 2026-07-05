// SPDX-License-Identifier: GPL-3.0-only
//! Add / Edit connection form overlay (REMOTE-UX P4, ODP-3).
//!
//! Presentation + input state for the in-app form that creates or edits an
//! OdyTTY-owned `hosts.conf` block. The form owns only text buffers, tri-state
//! toggles, a focus cursor, and an inline validation message. It never writes to
//! the file itself: accepting the form emits a [`ConnectionFormOutcome::Save`]
//! carrying a fully-built [`ConnectionHost`] plus the edit target (the original
//! alias for an in-place edit, or `None` for a fresh append) for the App to
//! persist through the byte-splice writer.
//!
//! Only OdyTTY-owned hosts are editable; an `ssh-config`-imported row is
//! read-only (never written back to `~/.ssh/config`), so the App never opens the
//! form for one. The `Protocol` field is reserved (config-only) and is carried
//! through an edit opaquely rather than surfaced as a control.

use crate::connection_hosts::{ConnectionHost, ConnectionHostSource, is_valid_adhoc_part};
use crate::ssh_connect::ProbeClass;
use crate::theme::Srgb;

use super::overlay::OverlayInput;

/// One focusable control in the form, in top-to-bottom order within each group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FormField {
    #[default]
    Alias,
    HostName,
    User,
    Port,
    /// The Advanced disclosure toggle. Activating it reveals the override /
    /// profile / identity fields below.
    Advanced,
    IdentityFile,
    Integration,
    Reuse,
    Tmux,
    Theme,
    Font,
    Title,
    Test,
    Save,
    Cancel,
}

impl FormField {
    fn is_tristate(self) -> bool {
        matches!(
            self,
            FormField::Integration | FormField::Reuse | FormField::Tmux
        )
    }

    /// The left-column label for a field row.
    fn label(self) -> &'static str {
        match self {
            FormField::Alias => "Alias",
            FormField::HostName => "HostName",
            FormField::User => "User",
            FormField::Port => "Port",
            FormField::Advanced => "Advanced",
            FormField::IdentityFile => "IdentityFile",
            FormField::Integration => "Integration",
            FormField::Reuse => "Reuse",
            FormField::Tmux => "Tmux",
            FormField::Theme => "Theme",
            FormField::Font => "Font",
            FormField::Title => "Title",
            FormField::Test => "Test",
            FormField::Save => "Save",
            FormField::Cancel => "Cancel",
        }
    }

    /// One- or two-line help for the focused field, shown in the form's footer
    /// (FORM-UX, the SETTINGS-COMPACT focused-row pattern). Facts mirror the
    /// `hosts.conf` reference in `docs/runtime-knobs.md` — no new claims.
    fn help(self) -> &'static str {
        match self {
            FormField::Alias => {
                "Quick-connect name for this host. Letters, digits, . - _ (no leading -)."
            }
            FormField::HostName => "Host to connect to (defaults to the alias if left empty).",
            FormField::User => "Login user (ssh user@host). Empty uses your ssh default.",
            FormField::Port => "TCP port (empty = 22).",
            FormField::Advanced => {
                "Enter toggles optional per-host overrides: identity key, integration/reuse/tmux, theme/font/title."
            }
            FormField::IdentityFile => {
                "Path to an existing private key; passed as ssh -i. Empty uses the agent/ssh defaults. Enter browses ~/.ssh; ssh-copy-id installs a key on the host. Only the path is stored, never key material."
            }
            FormField::Integration => {
                "Remote shell integration. inherit = follow the global setting; on/off override it for this host."
            }
            FormField::Reuse => {
                "Connection reuse (ControlMaster). inherit = follow the global setting; on/off override it for this host."
            }
            FormField::Tmux => {
                "tmux session persistence. inherit = follow the global setting; on/off override it for this host."
            }
            FormField::Theme => "Per-host theme override. Empty uses the app default.",
            FormField::Font => "Per-host font override. Empty uses the app default.",
            FormField::Title => "Per-host tab title. Empty falls back to user@host.",
            FormField::Test => {
                "Run a non-interactive reachability + key/agent probe. Never sends a password."
            }
            FormField::Save => "Write this connection to hosts.conf and close the form.",
            FormField::Cancel => "Discard changes and close the form.",
        }
    }
}

/// The live state of a Test Connection probe (ODP-8).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum TestState {
    /// No probe has run for the current field values.
    #[default]
    Idle,
    /// A probe is in flight (the App is running it on a background thread).
    Running,
    /// A probe finished with this tri-state classification.
    Done(ProbeClass),
    /// The probe could not be spawned / built (e.g. invalid host); the message
    /// is a short, non-credential reason.
    Error(String),
}

/// Whether the form creates a new block or edits an existing one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum FormMode {
    /// A fresh host appended to `hosts.conf`.
    #[default]
    Add,
    /// An in-place edit of the block owning `original_alias`; Save splices over
    /// that block's byte span.
    Edit { original_alias: String },
}

/// What accepting or dismissing the form asks the App to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ConnectionFormOutcome {
    /// A key was handled but nothing else changed.
    Consumed,
    /// Cancel: close the form without writing.
    Close,
    /// Persist the built host. `edit_target` is the original alias for an
    /// in-place block edit (byte splice), or `None` to append a new block.
    Save {
        host: Box<ConnectionHost>,
        edit_target: Option<String>,
    },
    /// Run a background Test Connection probe against the built host (ODP-8).
    /// The form stays open and shows the tri-state result when it lands.
    Test(Box<ConnectionHost>),
    /// Open the IdentityFile key browser (FORM-UX). The form stays open; the App
    /// scans `~/.ssh` for candidate private keys (filename heuristics only —
    /// never key contents) and seeds the in-form browser through
    /// [`ConnectionForm::open_key_browse`]. Manual typing stays available, so
    /// this only fires when the field is empty.
    BrowseIdentityKeys,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConnectionFormLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
    /// A small color swatch drawn at the row start — the tri-state Test result
    /// indicator (green ok / amber interactive-auth / red failure). `None` for
    /// every ordinary field row.
    pub(super) swatch: Option<Srgb>,
}

/// Render-cache signature: any field, toggle, focus, or error change repaints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ConnectionFormSignature {
    is_edit: bool,
    advanced: bool,
    focus: usize,
    fields: Vec<String>,
    tristates: Vec<Option<bool>>,
    error: Option<String>,
    test: TestState,
    /// The IdentityFile browser state (FORM-UX): `None` when the form shows its
    /// fields, else the browser's query + selection so a repaint tracks it.
    browse: Option<(String, usize, usize)>,
}

/// The in-form IdentityFile key browser (FORM-UX). A seeded, type-to-filter
/// list of candidate private-key PATHS discovered under `~/.ssh` by the App —
/// filename heuristics only, never key contents. Accepting a row fills the
/// IdentityFile field with the chosen path; Esc returns to the form. Manual
/// typing is always available, so an empty scan just shows a hint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct KeyBrowse {
    /// Candidate key paths, seeded by the App at open (already sorted). Display
    /// shows the file name; accepting fills the field with the full path.
    candidates: Vec<String>,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
}

impl KeyBrowse {
    fn open(candidates: Vec<String>) -> Self {
        let mut browse = KeyBrowse {
            candidates,
            ..KeyBrowse::default()
        };
        browse.recompute();
        browse
    }

    fn recompute(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.candidates.len()).collect();
        } else {
            let needle = self.query.to_ascii_lowercase();
            self.filtered = (0..self.candidates.len())
                .filter(|&i| {
                    basename(&self.candidates[i])
                        .to_ascii_lowercase()
                        .contains(&needle)
                })
                .collect();
        }
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    /// The path the current selection would fill in, if any.
    fn selected_path(&self) -> Option<&str> {
        let idx = *self.filtered.get(self.selected)?;
        self.candidates.get(idx).map(String::as_str)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ConnectionForm {
    mode: FormMode,
    alias: String,
    host_name: String,
    user: String,
    port: String,
    identity_file: String,
    theme: String,
    font: String,
    title: String,
    integration: Option<bool>,
    reuse: Option<bool>,
    tmux: Option<bool>,
    /// Reserved protocol carried opaquely across an edit so it is not dropped
    /// (it has no form control).
    protocol: Option<String>,
    /// Per-host `Persist` (ControlPersist) override carried opaquely across an
    /// edit so it is not dropped (the global setting is the editor; there is no
    /// per-host form control).
    persist: Option<String>,
    advanced: bool,
    focus: FormField,
    error: Option<String>,
    /// The Test Connection probe state (ODP-8).
    test: TestState,
    /// Saved aliases that would collide on save (Add: all; Edit: all but this
    /// block's own alias). Checked inline so a collision never writes.
    existing_aliases: Vec<String>,
    /// When `Some`, the IdentityFile key browser is active over the form
    /// (FORM-UX). Input/render route to the browser until a pick or Esc.
    browse: Option<KeyBrowse>,
}

impl ConnectionForm {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Open a blank form to add a new host. `existing_aliases` are the saved
    /// aliases an added block must not collide with.
    pub(super) fn open_add(&mut self, existing_aliases: Vec<String>) {
        *self = Self {
            mode: FormMode::Add,
            existing_aliases,
            ..Self::default()
        };
    }

    /// Open a form pre-filled from an existing OdyTTY-owned host. Save splices
    /// over the original block. `existing_aliases` should exclude this host's
    /// own alias so keeping it is not a self-collision.
    pub(super) fn open_edit(&mut self, host: &ConnectionHost, existing_aliases: Vec<String>) {
        *self = Self {
            mode: FormMode::Edit {
                original_alias: host.alias.clone(),
            },
            alias: host.alias.clone(),
            host_name: host.host_name.clone().unwrap_or_default(),
            user: host.user.clone().unwrap_or_default(),
            port: host.port.map(|p| p.to_string()).unwrap_or_default(),
            identity_file: host.identity_file.clone().unwrap_or_default(),
            theme: host.theme.clone().unwrap_or_default(),
            font: host.font.clone().unwrap_or_default(),
            title: host.title.clone().unwrap_or_default(),
            integration: host.integration,
            reuse: host.reuse,
            tmux: host.tmux,
            protocol: host.protocol.clone(),
            persist: host.persist.clone(),
            existing_aliases,
            ..Self::default()
        };
    }

    fn is_edit(&self) -> bool {
        matches!(self.mode, FormMode::Edit { .. })
    }

    pub(super) fn title(&self) -> String {
        // A leading `←` marks the shared back-arrow affordance (the picker
        // title-hit test keys off it).
        if self.is_edit() {
            "\u{2190} Edit Connection  (Esc = cancel)".to_owned()
        } else {
            "\u{2190} Add Connection  (Esc = cancel)".to_owned()
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        columns.min(72)
    }

    /// The focusable fields in render order, honoring the Advanced disclosure.
    fn fields(&self) -> Vec<FormField> {
        let mut fields = vec![
            FormField::Alias,
            FormField::HostName,
            FormField::User,
            FormField::Port,
            FormField::Advanced,
        ];
        if self.advanced {
            fields.extend([
                FormField::IdentityFile,
                FormField::Integration,
                FormField::Reuse,
                FormField::Tmux,
                FormField::Theme,
                FormField::Font,
                FormField::Title,
            ]);
        }
        fields.push(FormField::Test);
        fields.push(FormField::Save);
        fields.push(FormField::Cancel);
        fields
    }

    fn focus_index(&self) -> usize {
        self.fields()
            .iter()
            .position(|f| *f == self.focus)
            .unwrap_or(0)
    }

    fn move_focus(&mut self, delta: isize) {
        let fields = self.fields();
        if fields.is_empty() {
            return;
        }
        let len = fields.len() as isize;
        let current = self.focus_index() as isize;
        let next = (current + delta).rem_euclid(len);
        self.focus = fields[next as usize];
    }

    fn text_buf_mut(&mut self, field: FormField) -> Option<&mut String> {
        match field {
            FormField::Alias => Some(&mut self.alias),
            FormField::HostName => Some(&mut self.host_name),
            FormField::User => Some(&mut self.user),
            FormField::Port => Some(&mut self.port),
            FormField::IdentityFile => Some(&mut self.identity_file),
            FormField::Theme => Some(&mut self.theme),
            FormField::Font => Some(&mut self.font),
            FormField::Title => Some(&mut self.title),
            _ => None,
        }
    }

    fn tristate_mut(&mut self, field: FormField) -> Option<&mut Option<bool>> {
        match field {
            FormField::Integration => Some(&mut self.integration),
            FormField::Reuse => Some(&mut self.reuse),
            FormField::Tmux => Some(&mut self.tmux),
            _ => None,
        }
    }

    /// Cycle a tri-state control: inherit → on → off → inherit (or the reverse
    /// when `forward` is false).
    fn cycle_tristate(&mut self, field: FormField, forward: bool) {
        self.invalidate_test();
        if let Some(slot) = self.tristate_mut(field) {
            *slot = if forward {
                match *slot {
                    None => Some(true),
                    Some(true) => Some(false),
                    Some(false) => None,
                }
            } else {
                match *slot {
                    None => Some(false),
                    Some(false) => Some(true),
                    Some(true) => None,
                }
            };
        }
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ConnectionFormOutcome {
        // While the IdentityFile browser is open it captures every key.
        if self.browse.is_some() && self.handle_browse_input(input) {
            return ConnectionFormOutcome::Consumed;
        }
        match input {
            OverlayInput::Close => ConnectionFormOutcome::Close,
            OverlayInput::Save => self.try_save(),
            OverlayInput::Up => {
                self.move_focus(-1);
                ConnectionFormOutcome::Consumed
            }
            OverlayInput::Down | OverlayInput::Tab => {
                self.move_focus(1);
                ConnectionFormOutcome::Consumed
            }
            OverlayInput::Left => {
                if self.focus.is_tristate() {
                    self.cycle_tristate(self.focus, false);
                }
                ConnectionFormOutcome::Consumed
            }
            OverlayInput::Right => {
                if self.focus.is_tristate() {
                    self.cycle_tristate(self.focus, true);
                }
                ConnectionFormOutcome::Consumed
            }
            OverlayInput::Activate | OverlayInput::ActivateAlt => self.activate_focus(),
            OverlayInput::Backspace => {
                if let Some(buf) = self.text_buf_mut(self.focus) {
                    buf.pop();
                    self.invalidate_test();
                }
                ConnectionFormOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                if self.focus.is_tristate() && ch == ' ' {
                    self.cycle_tristate(self.focus, true);
                } else if let Some(buf) = self.text_buf_mut(self.focus) {
                    buf.push(ch);
                    self.invalidate_test();
                }
                ConnectionFormOutcome::Consumed
            }
            OverlayInput::Char(_)
            | OverlayInput::PageUp
            | OverlayInput::PageDown
            | OverlayInput::Home
            | OverlayInput::End => ConnectionFormOutcome::Consumed,
        }
    }

    /// Perform the focused field's Enter action.
    fn activate_focus(&mut self) -> ConnectionFormOutcome {
        match self.focus {
            FormField::Save => self.try_save(),
            FormField::Test => self.try_test(),
            FormField::Cancel => ConnectionFormOutcome::Close,
            FormField::Advanced => {
                self.advanced = !self.advanced;
                if !self.advanced && !self.fields().contains(&self.focus) {
                    self.focus = FormField::Advanced;
                }
                ConnectionFormOutcome::Consumed
            }
            field if field.is_tristate() => {
                self.cycle_tristate(field, true);
                ConnectionFormOutcome::Consumed
            }
            // Enter on an EMPTY IdentityFile field opens the ~/.ssh key browser
            // (FORM-UX). A non-empty field advances instead so a typed path is
            // never interrupted — manual entry stays fully supported.
            FormField::IdentityFile if self.identity_file.trim().is_empty() => {
                ConnectionFormOutcome::BrowseIdentityKeys
            }
            // Enter on a text field advances to the next field, a familiar form
            // convenience that never submits by accident.
            _ => {
                self.move_focus(1);
                ConnectionFormOutcome::Consumed
            }
        }
    }

    /// Seed and open the IdentityFile key browser over the form (FORM-UX). The
    /// App discovered `candidates` by scanning `~/.ssh` (filename heuristics
    /// only), so an empty list simply shows a "type a path manually" hint.
    pub(super) fn open_key_browse(&mut self, candidates: Vec<String>) {
        self.focus = FormField::IdentityFile;
        self.browse = Some(KeyBrowse::open(candidates));
    }

    #[cfg(test)]
    fn browsing(&self) -> bool {
        self.browse.is_some()
    }

    /// Route a key while the IdentityFile browser is open. Esc/Close returns to
    /// the form; Enter (or ActivateAlt) fills the field with the selected path;
    /// typing filters. Returns `true` when the browser handled the key.
    fn handle_browse_input(&mut self, input: OverlayInput) -> bool {
        let Some(browse) = self.browse.as_mut() else {
            return false;
        };
        match input {
            OverlayInput::Close => {
                self.browse = None;
            }
            OverlayInput::Up => browse.move_selection(-1),
            OverlayInput::Down | OverlayInput::Tab => browse.move_selection(1),
            OverlayInput::PageUp | OverlayInput::Home => browse.move_selection(isize::MIN / 2),
            OverlayInput::PageDown | OverlayInput::End => browse.move_selection(isize::MAX / 2),
            OverlayInput::Backspace => {
                browse.query.pop();
                browse.recompute();
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                browse.query.push(ch);
                browse.recompute();
            }
            OverlayInput::Activate | OverlayInput::ActivateAlt => {
                if let Some(path) = browse.selected_path() {
                    self.identity_file = path.to_owned();
                    self.invalidate_test();
                }
                self.browse = None;
            }
            OverlayInput::Char(_)
            | OverlayInput::Left
            | OverlayInput::Right
            | OverlayInput::Save => {}
        }
        true
    }

    /// Map a left-click on body row `row` to a focus change or an action.
    pub(super) fn handle_pointer_press(&mut self, row: usize) -> ConnectionFormOutcome {
        // While the key browser is open a click selects (and accepts) a row.
        if let Some(browse) = self.browse.as_mut() {
            // Row 0 is the browser prompt; candidate rows follow it.
            if row >= 1 {
                let cursor = row - 1;
                if cursor < browse.filtered.len() {
                    browse.selected = cursor;
                    if let Some(path) = browse.selected_path() {
                        self.identity_file = path.to_owned();
                        self.invalidate_test();
                    }
                    self.browse = None;
                }
            }
            return ConnectionFormOutcome::Consumed;
        }
        let Some(field) = self.field_at_row(row) else {
            return ConnectionFormOutcome::Consumed;
        };
        self.focus = field;
        // Clicking an action row (or the disclosure) acts immediately; clicking
        // a data field just focuses it.
        match field {
            FormField::Save | FormField::Test | FormField::Cancel | FormField::Advanced => {
                self.activate_focus()
            }
            _ => ConnectionFormOutcome::Consumed,
        }
    }

    /// The field rendered on body row `row`, if any (spacer / header / error
    /// rows return `None`).
    fn field_at_row(&self, row: usize) -> Option<FormField> {
        self.rows().into_iter().nth(row).and_then(|r| match r {
            FormRow::Field(field) => Some(field),
            _ => None,
        })
    }

    /// Trim and normalize a text buffer into an optional field value.
    fn opt(value: &str) -> Option<String> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    /// Validate the current buffers and either build the host to save or set an
    /// inline error and stay open.
    fn try_save(&mut self) -> ConnectionFormOutcome {
        match self.validate(true) {
            Ok(host) => {
                let edit_target = match &self.mode {
                    FormMode::Add => None,
                    FormMode::Edit { original_alias } => Some(original_alias.clone()),
                };
                ConnectionFormOutcome::Save {
                    host: Box::new(host),
                    edit_target,
                }
            }
            Err(message) => {
                self.error = Some(message);
                ConnectionFormOutcome::Consumed
            }
        }
    }

    /// Validate just enough to build a host to probe (no alias-collision check —
    /// a Test only asks whether the destination is reachable), mark the form as
    /// testing, and emit the probe request. The App runs it on a background
    /// thread and feeds the result back through [`Self::set_test_result`].
    fn try_test(&mut self) -> ConnectionFormOutcome {
        match self.validate(false) {
            Ok(host) => {
                self.test = TestState::Running;
                ConnectionFormOutcome::Test(Box::new(host))
            }
            Err(message) => {
                self.error = Some(message);
                ConnectionFormOutcome::Consumed
            }
        }
    }

    /// Record the outcome of a background Test Connection probe (ODP-8).
    pub(super) fn set_test_result(&mut self, result: Result<ProbeClass, String>) {
        self.test = match result {
            Ok(class) => TestState::Done(class),
            Err(message) => TestState::Error(message),
        };
    }

    /// A form edit invalidates any prior probe result (it no longer describes
    /// the current fields).
    fn invalidate_test(&mut self) {
        self.test = TestState::Idle;
    }

    fn validate(&mut self, check_collision: bool) -> Result<ConnectionHost, String> {
        let alias = self.alias.trim().to_owned();
        if alias.is_empty() {
            self.focus = FormField::Alias;
            return Err("Alias is required".to_owned());
        }
        if !is_valid_adhoc_part(&alias) {
            self.focus = FormField::Alias;
            return Err("Alias: letters, digits, . - _ only; no leading -".to_owned());
        }
        // Alias-collision guard: never clobber a different saved host (skipped
        // for a Test, which only probes reachability).
        if check_collision && self.existing_aliases.contains(&alias) {
            self.focus = FormField::Alias;
            return Err(format!("A host named \"{alias}\" already exists"));
        }

        let host_name = Self::opt(&self.host_name);
        if let Some(name) = host_name.as_deref()
            && !is_valid_adhoc_part(name)
        {
            self.focus = FormField::HostName;
            return Err("HostName: letters, digits, . - _ only; no leading -".to_owned());
        }

        let user = Self::opt(&self.user);
        if let Some(user) = user.as_deref()
            && !is_valid_adhoc_part(user)
        {
            self.focus = FormField::User;
            return Err("User: letters, digits, . - _ only; no @ or leading -".to_owned());
        }

        let port = match self.port.trim() {
            "" => None,
            digits => match digits.parse::<u16>() {
                Ok(port) if port >= 1 => Some(port),
                _ => {
                    self.focus = FormField::Port;
                    return Err("Port must be a number in 1..=65535".to_owned());
                }
            },
        };

        // Free-text profile / identity fields: reject only control characters;
        // the writer quotes whitespace so multi-word values round-trip.
        for (value, field, name) in [
            (&self.identity_file, FormField::IdentityFile, "IdentityFile"),
            (&self.theme, FormField::Theme, "Theme"),
            (&self.font, FormField::Font, "Font"),
            (&self.title, FormField::Title, "Title"),
        ] {
            if value.chars().any(char::is_control) {
                self.focus = field;
                return Err(format!("{name} contains a control character"));
            }
        }

        self.error = None;
        Ok(ConnectionHost {
            alias,
            host_name,
            user,
            port,
            theme: Self::opt(&self.theme),
            font: Self::opt(&self.font),
            title: Self::opt(&self.title),
            integration: self.integration,
            reuse: self.reuse,
            tmux: self.tmux,
            protocol: self.protocol.clone(),
            persist: self.persist.clone(),
            identity_file: Self::opt(&self.identity_file),
            source: ConnectionHostSource::Odytty,
        })
    }

    /// Ordered rows the form renders (and the inverse map for click hit-testing).
    fn rows(&self) -> Vec<FormRow> {
        let mut rows = vec![
            FormRow::Field(FormField::Alias),
            FormRow::Field(FormField::HostName),
            FormRow::Field(FormField::User),
            FormRow::Field(FormField::Port),
            FormRow::Spacer,
            FormRow::Field(FormField::Advanced),
        ];
        if self.advanced {
            rows.extend([
                FormRow::Field(FormField::IdentityFile),
                FormRow::Field(FormField::Integration),
                FormRow::Field(FormField::Reuse),
                FormRow::Field(FormField::Tmux),
                FormRow::Field(FormField::Theme),
                FormRow::Field(FormField::Font),
                FormRow::Field(FormField::Title),
            ]);
        }
        rows.push(FormRow::Spacer);
        if let Some(error) = &self.error {
            rows.push(FormRow::Text(format!("! {error}"), false));
        }
        rows.push(FormRow::Field(FormField::Test));
        if let Some((text, swatch)) = self.test_status() {
            rows.push(FormRow::Status(text, swatch));
        }
        rows.push(FormRow::Field(FormField::Save));
        rows.push(FormRow::Field(FormField::Cancel));
        rows.push(FormRow::Spacer);
        // Focused-field help footer (FORM-UX): follows keyboard focus + click,
        // wrapped to the body width in `visible_lines`. Collapses off first on a
        // short window, exactly like the connection-manager footer.
        rows.push(FormRow::Help(self.focus));
        rows.push(FormRow::Text(
            "[Tab/\u{2191}\u{2193}] move   [Enter] act   [Ctrl+S] save   [Esc] cancel".to_owned(),
            false,
        ));
        rows
    }

    /// Render one field row: `Label   value` (or the tri-state / action form).
    fn render_field(&self, field: FormField, body_width: usize) -> ConnectionFormLine {
        let focused = field == self.focus;
        let text = match field {
            FormField::Advanced => {
                let marker = if self.advanced {
                    '\u{25be}'
                } else {
                    '\u{25b8}'
                };
                format!("{marker} Advanced")
            }
            FormField::Save => {
                if self.is_edit() {
                    "[ Save changes ]".to_owned()
                } else {
                    "[ Save connection ]".to_owned()
                }
            }
            FormField::Test => "[ Test connection ]".to_owned(),
            FormField::Cancel => "[ Cancel ]".to_owned(),
            _ if field.is_tristate() => {
                let slot = match field {
                    FormField::Integration => self.integration,
                    FormField::Reuse => self.reuse,
                    FormField::Tmux => self.tmux,
                    _ => None,
                };
                let value = match slot {
                    None => "inherit",
                    Some(true) => "on",
                    Some(false) => "off",
                };
                format!("{:<13}< {value} >", field.label())
            }
            _ => {
                let value = match field {
                    FormField::Alias => &self.alias,
                    FormField::HostName => &self.host_name,
                    FormField::User => &self.user,
                    FormField::Port => &self.port,
                    FormField::IdentityFile => &self.identity_file,
                    FormField::Theme => &self.theme,
                    FormField::Font => &self.font,
                    FormField::Title => &self.title,
                    _ => "",
                };
                // A trailing caret marks the edit cursor on the focused field.
                let caret = if focused { "\u{2588}" } else { "" };
                format!("{:<13}{value}{caret}", field.label())
            }
        };
        ConnectionFormLine {
            text: truncate_for_width(&text, body_width),
            focused,
            bold: matches!(field, FormField::Save | FormField::Test | FormField::Cancel),
            swatch: None,
        }
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ConnectionFormLine> {
        // The IdentityFile key browser owns the whole body while it is open.
        if let Some(browse) = self.browse.as_ref() {
            return self.browse_lines(browse, body_width, body_height);
        }
        let mut lines = Vec::new();
        for row in self.rows() {
            if lines.len() >= body_height {
                break;
            }
            match row {
                FormRow::Field(field) => lines.push(self.render_field(field, body_width)),
                FormRow::Spacer => lines.push(ConnectionFormLine {
                    text: String::new(),
                    focused: false,
                    bold: false,
                    swatch: None,
                }),
                FormRow::Text(text, bold) => lines.push(ConnectionFormLine {
                    text: truncate_for_width(&text, body_width),
                    focused: false,
                    bold,
                    swatch: None,
                }),
                // A swatch consumes three leading columns, so its text budget is
                // three narrower.
                FormRow::Status(text, swatch) => lines.push(ConnectionFormLine {
                    text: truncate_for_width(&text, body_width.saturating_sub(3)),
                    focused: false,
                    bold: true,
                    swatch,
                }),
                FormRow::Help(field) => {
                    for wrapped in wrap_for_width(field.help(), body_width) {
                        if lines.len() >= body_height {
                            break;
                        }
                        lines.push(ConnectionFormLine {
                            text: wrapped,
                            focused: false,
                            bold: false,
                            swatch: None,
                        });
                    }
                }
            }
        }
        lines
    }

    /// Render the IdentityFile key browser body (FORM-UX): a `> query` prompt,
    /// then candidate file NAMES (never contents), or a "type a path manually"
    /// hint when the scan found nothing, then a key-hint footer.
    fn browse_lines(
        &self,
        browse: &KeyBrowse,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ConnectionFormLine> {
        let mut lines = Vec::with_capacity(body_height.min(browse.filtered.len() + 3));
        lines.push(ConnectionFormLine {
            text: truncate_for_width(
                &format!("Browse ~/.ssh keys  > {}", browse.query),
                body_width,
            ),
            focused: false,
            bold: true,
            swatch: None,
        });
        if browse.filtered.is_empty() {
            if lines.len() < body_height {
                let hint = if browse.candidates.is_empty() {
                    "No keys found under ~/.ssh \u{2014} Esc, then type a path manually"
                } else {
                    "No matches \u{2014} Esc to go back"
                };
                lines.push(ConnectionFormLine {
                    text: truncate_for_width(hint, body_width),
                    focused: false,
                    bold: false,
                    swatch: None,
                });
            }
        } else {
            // Reserve the last row for the footer hint when there is room.
            let footer_reserve = usize::from(body_height > 3);
            let room = body_height.saturating_sub(1 + footer_reserve);
            for (row, &idx) in browse.filtered.iter().take(room).enumerate() {
                let name = basename(&browse.candidates[idx]);
                lines.push(ConnectionFormLine {
                    text: truncate_for_width(name, body_width),
                    focused: row == browse.selected,
                    bold: false,
                    swatch: None,
                });
            }
        }
        if lines.len() < body_height {
            lines.push(ConnectionFormLine {
                text: truncate_for_width(
                    "[\u{2191}\u{2193}] pick   [Enter] use   [Esc] back",
                    body_width,
                ),
                focused: false,
                bold: false,
                swatch: None,
            });
        }
        lines
    }

    /// The Test-result status line (text + optional swatch), or `None` when no
    /// probe has run for the current fields. The amber copy states outright that
    /// an interactive-auth result is EXPECTED for a password host and the connect
    /// will still work, so amber never reads as "broken".
    fn test_status(&self) -> Option<(String, Option<Srgb>)> {
        match &self.test {
            TestState::Idle => None,
            TestState::Running => Some((
                "Testing\u{2026} reachability + key/agent auth".to_owned(),
                None,
            )),
            TestState::Error(message) => {
                Some((format!("Test could not run: {message}"), Some(TEST_RED)))
            }
            TestState::Done(class) => Some(match class {
                ProbeClass::AuthOk => (
                    "Reachable \u{2014} key/agent auth OK".to_owned(),
                    Some(TEST_GREEN),
                ),
                ProbeClass::InteractiveAuth => (
                    "Reachable \u{2014} will prompt for a password when you connect (expected)"
                        .to_owned(),
                    Some(TEST_AMBER),
                ),
                ProbeClass::HostKeyMismatch => (
                    "Host key mismatch \u{2014} verify the host before connecting".to_owned(),
                    Some(TEST_RED),
                ),
                ProbeClass::Unreachable => (
                    "Unreachable \u{2014} check the host, port, and network".to_owned(),
                    Some(TEST_RED),
                ),
            }),
        }
    }

    pub(super) fn render_signature(&self) -> ConnectionFormSignature {
        ConnectionFormSignature {
            is_edit: self.is_edit(),
            advanced: self.advanced,
            focus: self.focus_index(),
            fields: vec![
                self.alias.clone(),
                self.host_name.clone(),
                self.user.clone(),
                self.port.clone(),
                self.identity_file.clone(),
                self.theme.clone(),
                self.font.clone(),
                self.title.clone(),
            ],
            tristates: vec![self.integration, self.reuse, self.tmux],
            error: self.error.clone(),
            test: self.test.clone(),
            browse: self
                .browse
                .as_ref()
                .map(|b| (b.query.clone(), b.selected, b.filtered.len())),
        }
    }
}

/// A rendered row: a focusable field, a blank spacer, static text, or the
/// swatched Test-result status line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FormRow {
    Field(FormField),
    Spacer,
    Text(String, bool),
    Status(String, Option<Srgb>),
    /// The focused-field help footer (FORM-UX). Rendered as one or more wrapped
    /// lines at the body width by [`ConnectionForm::visible_lines`].
    Help(FormField),
}

/// Fixed status colors for the tri-state Test result. These are semantic status
/// indicators (ok / caution / failure), not theme chrome, so they are fixed
/// rather than derived from a theme role.
const TEST_GREEN: Srgb = (0x3f, 0xc0, 0x60);
const TEST_AMBER: Srgb = (0xd0, 0x94, 0x20);
const TEST_RED: Srgb = (0xd0, 0x44, 0x44);

fn truncate_for_width(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    text.chars().take(max_chars).collect()
}

/// The final path component, for browser display. Handles both `/` and `\`
/// separators so a Windows `%USERPROFILE%\.ssh\id_ed25519` path shows its name.
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Greedy word-wrap for the help footer: split on spaces, packing whole words up
/// to `width` columns per line. A single word longer than `width` is hard-split
/// so it never overflows. `width == 0` yields nothing.
fn wrap_for_width(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let cur_len = current.chars().count();
        if current.is_empty() {
            // A word wider than the line is hard-split across lines.
            if word_len > width {
                let mut chars = word.chars().peekable();
                while chars.peek().is_some() {
                    let chunk: String = chars.by_ref().take(width).collect();
                    lines.push(chunk);
                }
            } else {
                current = word.to_owned();
            }
        } else if cur_len + 1 + word_len <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            if word_len > width {
                let mut chars = word.chars().peekable();
                while chars.peek().is_some() {
                    let chunk: String = chars.by_ref().take(width).collect();
                    lines.push(chunk);
                }
            } else {
                current = word.to_owned();
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(form: &mut ConnectionForm, text: &str) {
        for ch in text.chars() {
            form.handle_input(OverlayInput::Char(ch));
        }
    }

    fn saved(form: &mut ConnectionForm) -> Option<(ConnectionHost, Option<String>)> {
        match form.handle_input(OverlayInput::Save) {
            ConnectionFormOutcome::Save { host, edit_target } => Some((*host, edit_target)),
            _ => None,
        }
    }

    #[test]
    fn add_form_builds_a_host_and_appends() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        // Focus starts on Alias.
        typed(&mut form, "web1");
        form.handle_input(OverlayInput::Down); // HostName
        typed(&mut form, "gateway.example.invalid");
        form.handle_input(OverlayInput::Down); // User
        typed(&mut form, "deploy");
        form.handle_input(OverlayInput::Down); // Port
        typed(&mut form, "2222");
        let (host, edit_target) = saved(&mut form).expect("save");
        assert_eq!(edit_target, None, "Add appends");
        assert_eq!(host.alias, "web1");
        assert_eq!(host.host_name.as_deref(), Some("gateway.example.invalid"));
        assert_eq!(host.user.as_deref(), Some("deploy"));
        assert_eq!(host.port, Some(2222));
    }

    #[test]
    fn empty_alias_blocks_save_with_an_inline_error() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        assert_eq!(
            form.handle_input(OverlayInput::Save),
            ConnectionFormOutcome::Consumed
        );
        let lines = form.visible_lines(72, 40);
        assert!(lines.iter().any(|l| l.text.contains("Alias is required")));
    }

    #[test]
    fn bad_port_blocks_save() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        typed(&mut form, "h");
        form.handle_input(OverlayInput::Down); // HostName
        form.handle_input(OverlayInput::Down); // User
        form.handle_input(OverlayInput::Down); // Port
        typed(&mut form, "99999");
        assert_eq!(
            form.handle_input(OverlayInput::Save),
            ConnectionFormOutcome::Consumed
        );
        assert!(form.error.as_deref().unwrap().contains("Port"));
    }

    #[test]
    fn alias_collision_blocks_add() {
        let mut form = ConnectionForm::new();
        form.open_add(vec!["taken".to_owned()]);
        typed(&mut form, "taken");
        assert_eq!(
            form.handle_input(OverlayInput::Save),
            ConnectionFormOutcome::Consumed
        );
        assert!(form.error.as_deref().unwrap().contains("already exists"));
    }

    #[test]
    fn leading_dash_alias_is_rejected() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        typed(&mut form, "-bad");
        assert_eq!(
            form.handle_input(OverlayInput::Save),
            ConnectionFormOutcome::Consumed
        );
        assert!(form.error.is_some());
    }

    #[test]
    fn edit_prefills_and_saves_with_the_original_alias() {
        let host = ConnectionHost {
            alias: "alpha".to_owned(),
            host_name: Some("alpha.example.invalid".to_owned()),
            user: Some("alice".to_owned()),
            port: Some(2200),
            theme: None,
            font: None,
            title: None,
            integration: Some(false),
            reuse: None,
            tmux: None,
            protocol: Some("ssh".to_owned()),
            persist: None,
            identity_file: Some("/home/user/.ssh/alpha.example".to_owned()),
            source: ConnectionHostSource::Odytty,
        };
        let mut form = ConnectionForm::new();
        form.open_edit(&host, Vec::new());
        // Rename alpha -> alpha2 (append a "2").
        typed(&mut form, "2");
        let (built, edit_target) = saved(&mut form).expect("save");
        assert_eq!(
            edit_target.as_deref(),
            Some("alpha"),
            "Edit targets the original block"
        );
        assert_eq!(built.alias, "alpha2");
        // Pre-filled fields survive.
        assert_eq!(built.user.as_deref(), Some("alice"));
        assert_eq!(built.port, Some(2200));
        assert_eq!(built.integration, Some(false));
        // Reserved protocol is carried through opaquely, not dropped.
        assert_eq!(built.protocol.as_deref(), Some("ssh"));
        // IdentityFile is pre-filled and preserved.
        assert_eq!(
            built.identity_file.as_deref(),
            Some("/home/user/.ssh/alpha.example")
        );
    }

    #[test]
    fn advanced_toggle_reveals_and_hides_override_fields() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        assert!(!form.advanced);
        assert!(!form.fields().contains(&FormField::Integration));
        form.focus = FormField::Advanced;
        form.handle_input(OverlayInput::Activate);
        assert!(form.advanced);
        assert!(form.fields().contains(&FormField::Integration));
    }

    #[test]
    fn tristate_cycles_inherit_on_off() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.advanced = true;
        form.focus = FormField::Reuse;
        assert_eq!(form.reuse, None);
        form.handle_input(OverlayInput::Right);
        assert_eq!(form.reuse, Some(true));
        form.handle_input(OverlayInput::Right);
        assert_eq!(form.reuse, Some(false));
        form.handle_input(OverlayInput::Right);
        assert_eq!(form.reuse, None);
        // Left cycles the other way.
        form.handle_input(OverlayInput::Left);
        assert_eq!(form.reuse, Some(false));
        // Space also cycles a tri-state (there is no text to type there).
        form.reuse = None;
        form.handle_input(OverlayInput::Char(' '));
        assert_eq!(form.reuse, Some(true));
    }

    #[test]
    fn identity_file_field_round_trips_into_the_saved_host() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        typed(&mut form, "k");
        form.advanced = true;
        form.focus = FormField::IdentityFile;
        typed(&mut form, "/home/user/.ssh/id_ed25519.example");
        let (host, _) = saved(&mut form).expect("save");
        assert_eq!(
            host.identity_file.as_deref(),
            Some("/home/user/.ssh/id_ed25519.example")
        );
    }

    #[test]
    fn cancel_closes_without_saving() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.focus = FormField::Cancel;
        assert_eq!(
            form.handle_input(OverlayInput::Activate),
            ConnectionFormOutcome::Close
        );
    }

    #[test]
    fn esc_closes() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        assert_eq!(
            form.handle_input(OverlayInput::Close),
            ConnectionFormOutcome::Close
        );
    }

    #[test]
    fn click_focuses_a_field_and_acts_on_a_button() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        // Row 1 is HostName (row 0 = Alias). A click focuses it.
        assert_eq!(
            form.handle_pointer_press(1),
            ConnectionFormOutcome::Consumed
        );
        assert_eq!(form.focus, FormField::HostName);
        // Find the Cancel row and click it → Close.
        let cancel_row = form
            .rows()
            .iter()
            .position(|r| matches!(r, FormRow::Field(FormField::Cancel)))
            .expect("cancel row");
        assert_eq!(
            form.handle_pointer_press(cancel_row),
            ConnectionFormOutcome::Close
        );
    }

    #[test]
    fn test_action_validates_and_emits_a_probe_request() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        typed(&mut form, "reachable-host");
        form.focus = FormField::Test;
        match form.handle_input(OverlayInput::Activate) {
            ConnectionFormOutcome::Test(host) => assert_eq!(host.alias, "reachable-host"),
            other => panic!("expected Test, got {other:?}"),
        }
        assert_eq!(form.test, TestState::Running);
        // The Running status renders (no swatch while pending).
        let lines = form.visible_lines(72, 40);
        assert!(lines.iter().any(|l| l.text.contains("Testing")));
    }

    #[test]
    fn test_action_on_an_invalid_host_shows_an_error_not_a_probe() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        // Empty alias: Test cannot build a host.
        form.focus = FormField::Test;
        assert_eq!(
            form.handle_input(OverlayInput::Activate),
            ConnectionFormOutcome::Consumed
        );
        assert!(form.error.is_some());
        assert_eq!(form.test, TestState::Idle);
    }

    #[test]
    fn set_test_result_renders_the_tri_state_with_a_swatch() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        // Amber = reachable but interactive auth; the copy must say it is
        // expected and the connect still works.
        form.set_test_result(Ok(ProbeClass::InteractiveAuth));
        let lines = form.visible_lines(72, 40);
        let status = lines
            .iter()
            .find(|l| l.swatch.is_some())
            .expect("a swatched status row");
        assert_eq!(status.swatch, Some(TEST_AMBER));
        assert!(status.text.contains("expected"));
        // A failure is red.
        form.set_test_result(Ok(ProbeClass::Unreachable));
        let red = form
            .visible_lines(72, 40)
            .into_iter()
            .find(|l| l.swatch == Some(TEST_RED));
        assert!(red.is_some());
    }

    #[test]
    fn editing_a_field_invalidates_a_prior_test_result() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.set_test_result(Ok(ProbeClass::AuthOk));
        assert!(matches!(form.test, TestState::Done(_)));
        // Typing into the focused field clears the stale result.
        typed(&mut form, "x");
        assert_eq!(form.test, TestState::Idle);
    }

    #[test]
    fn visible_lines_are_bounded_by_body_height() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        assert!(form.visible_lines(72, 5).len() <= 5);
    }

    #[test]
    fn enter_on_empty_identity_file_requests_the_key_browser() {
        // FORM-UX IDENTITY-BROWSE: Enter on an EMPTY IdentityFile field asks the
        // App to scan ~/.ssh; a non-empty field advances instead (manual typing
        // is never interrupted).
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.advanced = true;
        form.focus = FormField::IdentityFile;
        assert_eq!(
            form.handle_input(OverlayInput::Activate),
            ConnectionFormOutcome::BrowseIdentityKeys
        );
        // With a typed path, Enter advances focus rather than browsing.
        typed(&mut form, "/keys/mine");
        assert_eq!(
            form.handle_input(OverlayInput::Activate),
            ConnectionFormOutcome::Consumed
        );
        assert_ne!(form.focus, FormField::IdentityFile);
    }

    #[test]
    fn key_browser_pick_fills_the_field_and_closes() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.advanced = true;
        form.focus = FormField::IdentityFile;
        form.open_key_browse(vec![
            "/home/u/.ssh/id_ed25519".to_owned(),
            "/home/u/.ssh/work.pem".to_owned(),
        ]);
        assert!(form.browsing());
        // Browser rows render by file NAME, never the path's directory.
        let lines = form.visible_lines(72, 10);
        assert!(lines.iter().any(|l| l.text == "id_ed25519"));
        assert!(lines.iter().any(|l| l.text == "work.pem"));
        // Down then Enter fills the field with the second candidate's full path.
        form.handle_input(OverlayInput::Down);
        assert_eq!(
            form.handle_input(OverlayInput::Activate),
            ConnectionFormOutcome::Consumed
        );
        assert!(!form.browsing());
        assert_eq!(form.identity_file, "/home/u/.ssh/work.pem");
    }

    #[test]
    fn key_browser_filters_by_name_and_esc_returns_without_change() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.open_key_browse(vec![
            "/k/id_ed25519".to_owned(),
            "/k/id_rsa".to_owned(),
            "/k/work.pem".to_owned(),
        ]);
        typed(&mut form, "rsa");
        let lines = form.visible_lines(72, 10);
        assert!(lines.iter().any(|l| l.text == "id_rsa"));
        assert!(!lines.iter().any(|l| l.text == "work.pem"));
        // Esc backs out of the browser without touching the field.
        assert_eq!(
            form.handle_input(OverlayInput::Close),
            ConnectionFormOutcome::Consumed
        );
        assert!(!form.browsing());
        assert!(form.identity_file.is_empty());
    }

    #[test]
    fn empty_key_scan_shows_a_type_a_path_hint() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.open_key_browse(Vec::new());
        let lines = form.visible_lines(72, 10);
        assert!(
            lines
                .iter()
                .any(|l| l.text.contains("type a path manually")),
            "empty scan surfaces the manual-entry hint"
        );
        // Enter on the empty browser is inert (no path to fill) and closes it.
        assert_eq!(
            form.handle_input(OverlayInput::Activate),
            ConnectionFormOutcome::Consumed
        );
        assert!(!form.browsing());
        assert!(form.identity_file.is_empty());
    }

    #[test]
    fn key_browser_click_selects_and_fills() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.open_key_browse(vec!["/k/id_ed25519".to_owned(), "/k/id_rsa".to_owned()]);
        // Row 0 is the prompt; row 2 is the second candidate.
        assert_eq!(
            form.handle_pointer_press(2),
            ConnectionFormOutcome::Consumed
        );
        assert!(!form.browsing());
        assert_eq!(form.identity_file, "/k/id_rsa");
    }

    #[test]
    fn focused_field_help_footer_follows_focus() {
        // FORM-UX FIELD-HELP: the footer shows the FOCUSED field's help.
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.focus = FormField::Alias;
        let alias_help = form.visible_lines(72, 40);
        assert!(
            alias_help
                .iter()
                .any(|l| l.text.contains("Quick-connect name")),
            "Alias help shows when Alias is focused"
        );
        // Focus IdentityFile: the footer switches to its help (mentions ssh -i).
        form.advanced = true;
        form.focus = FormField::IdentityFile;
        let id_help = form.visible_lines(72, 40);
        assert!(
            id_help.iter().any(|l| l.text.contains("ssh -i")),
            "IdentityFile help mentions ssh -i"
        );
        assert!(
            !id_help
                .iter()
                .any(|l| l.text.contains("Quick-connect name")),
            "only the focused field's help renders"
        );
    }

    #[test]
    fn help_footer_wraps_within_the_body_width() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        form.advanced = true;
        form.focus = FormField::IdentityFile;
        // Narrow body: every emitted line stays within the width (footer wraps
        // rather than truncating the long help).
        let width = 30;
        for line in form.visible_lines(width, 40) {
            assert!(
                line.text.chars().count() <= width,
                "line within width: {:?}",
                line.text
            );
        }
    }

    #[test]
    fn wrap_for_width_hard_splits_overlong_words() {
        let out = wrap_for_width("short verylongwordthatexceeds", 8);
        assert!(out.iter().all(|l| l.chars().count() <= 8));
        assert_eq!(out[0], "short");
        assert!(wrap_for_width("anything", 0).is_empty());
    }

    #[test]
    fn basename_handles_both_separators() {
        assert_eq!(basename("/home/u/.ssh/id_ed25519"), "id_ed25519");
        assert_eq!(basename(r"C:\Users\u\.ssh\id_rsa"), "id_rsa");
        assert_eq!(basename("bare"), "bare");
    }
}
