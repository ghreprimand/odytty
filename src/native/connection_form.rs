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
            FormField::Save => "Save",
            FormField::Cancel => "Cancel",
        }
    }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConnectionFormLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
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
    advanced: bool,
    focus: FormField,
    error: Option<String>,
    /// Saved aliases that would collide on save (Add: all; Edit: all but this
    /// block's own alias). Checked inline so a collision never writes.
    existing_aliases: Vec<String>,
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
                }
                ConnectionFormOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                if self.focus.is_tristate() && ch == ' ' {
                    self.cycle_tristate(self.focus, true);
                } else if let Some(buf) = self.text_buf_mut(self.focus) {
                    buf.push(ch);
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
            // Enter on a text field advances to the next field, a familiar form
            // convenience that never submits by accident.
            _ => {
                self.move_focus(1);
                ConnectionFormOutcome::Consumed
            }
        }
    }

    /// Map a left-click on body row `row` to a focus change or an action.
    pub(super) fn handle_pointer_press(&mut self, row: usize) -> ConnectionFormOutcome {
        let Some(field) = self.field_at_row(row) else {
            return ConnectionFormOutcome::Consumed;
        };
        self.focus = field;
        // Clicking an action row (or the disclosure) acts immediately; clicking
        // a data field just focuses it.
        match field {
            FormField::Save | FormField::Cancel | FormField::Advanced => self.activate_focus(),
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
        match self.validate() {
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

    fn validate(&mut self) -> Result<ConnectionHost, String> {
        let alias = self.alias.trim().to_owned();
        if alias.is_empty() {
            self.focus = FormField::Alias;
            return Err("Alias is required".to_owned());
        }
        if !is_valid_adhoc_part(&alias) {
            self.focus = FormField::Alias;
            return Err("Alias: letters, digits, . - _ only; no leading -".to_owned());
        }
        // Alias-collision guard: never clobber a different saved host.
        if self.existing_aliases.contains(&alias) {
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
        rows.push(FormRow::Field(FormField::Save));
        rows.push(FormRow::Field(FormField::Cancel));
        rows.push(FormRow::Spacer);
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
            bold: matches!(field, FormField::Save | FormField::Cancel),
        }
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ConnectionFormLine> {
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
                }),
                FormRow::Text(text, bold) => lines.push(ConnectionFormLine {
                    text: truncate_for_width(&text, body_width),
                    focused: false,
                    bold,
                }),
            }
        }
        lines
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
        }
    }
}

/// A rendered row: a focusable field, a blank spacer, or static text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FormRow {
    Field(FormField),
    Spacer,
    Text(String, bool),
}

fn truncate_for_width(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    text.chars().take(max_chars).collect()
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
    fn visible_lines_are_bounded_by_body_height() {
        let mut form = ConnectionForm::new();
        form.open_add(Vec::new());
        assert!(form.visible_lines(72, 5).len() <= 5);
    }
}
