// SPDX-License-Identifier: GPL-3.0-only
//! Headless CLI introspection helpers.
//!
//! These commands print stable, script-friendly snapshots and exit before the
//! native window or PTY paths start.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result as AnyResult, bail};
use odytty::atlas::SubpixelMode;
use odytty::core::{CursorStyle, SnapshotEnvelope, SnapshotEnvelopeCaps};
use odytty::native::{NativeCommand, NativeOptions};
use odytty::session_host::protocol::HostFrame;
use odytty::session_host::{
    HostCommand, HostConfig, ListedSession, SessionHostClient, SessionMetadata,
    existing_runtime_dir, list_live_sessions, now_unix_ms, session_socket_path,
    spawn_host_on_demand, write_session_metadata,
};
use odytty::settings::{
    BindableAction, KeyBindingKey, KeyBindingNamedKey, KeyBindingOverride, KeyChord, Settings,
};
use odytty::text::{self, FontInventoryEntry};
use odytty::theme::{self, Theme, VisualEffect, relative_luminance};

/// Return stdout for a supported CLI introspection flag.
pub fn output_for_args(args: &[String]) -> Option<String> {
    match args.first().map(String::as_str) {
        Some("--list-fonts") => Some(list_fonts_output()),
        Some("--list-themes") => Some(list_themes_output()),
        Some("--show-config") => Some(show_config_output(&Settings::from_env())),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCliCommand {
    NewDetached(DetachedSessionOptions),
    List(SessionListOptions),
    Attach(SessionAttachOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedSessionOptions {
    pub id: Option<String>,
    pub title: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub command: DetachedSessionCommand,
    pub runtime_base: Option<PathBuf>,
}

impl Default for DetachedSessionOptions {
    fn default() -> Self {
        Self {
            id: None,
            title: None,
            working_directory: None,
            command: DetachedSessionCommand::DefaultShell,
            runtime_base: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachedSessionCommand {
    DefaultShell,
    Exec(NativeCommand),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionListOptions {
    pub runtime_base: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAttachOptions {
    pub id: String,
    pub runtime_base: Option<PathBuf>,
    /// When `true` (`odytty attach --diagnostic <id>`), print a one-line status
    /// snapshot and exit instead of opening a window. When `false` (the default
    /// `odytty attach <id>`), the verb opens a live native window reattached to
    /// the hosted session — see [`SessionCliCommand::live_attach_id`].
    pub diagnostic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachAction {
    LiveWindow(String),
    PrintCli(String),
}

impl SessionCliCommand {
    /// The session id to reattach in a **live native window**, or `None` for
    /// every command that stays CLI-only (list, new, and diagnostic attach).
    ///
    /// Live attach is the one session subcommand that launches the native window
    /// instead of printing a script-friendly string, so `main` uses this to
    /// route `odytty attach <id>` to [`native_attach_options`] + the native run
    /// path rather than [`run_session_command`].
    pub fn live_attach_id(&self) -> Option<&str> {
        match self {
            SessionCliCommand::Attach(options) if !options.diagnostic && !options.id.is_empty() => {
                Some(&options.id)
            }
            _ => None,
        }
    }
}

/// Parse public resumable-session subcommands.
pub fn session_command_for_args(args: &[String]) -> Result<Option<SessionCliCommand>, String> {
    match args.first().map(String::as_str) {
        Some("new") => parse_session_new(&args[1..])
            .map(|options| Some(SessionCliCommand::NewDetached(options))),
        Some("list") => {
            if args.len() != 1 {
                return Err("odytty list takes no arguments".to_owned());
            }
            Ok(Some(SessionCliCommand::List(SessionListOptions::default())))
        }
        Some("attach") => {
            let mut id: Option<String> = None;
            let mut diagnostic = false;
            for arg in &args[1..] {
                match arg.as_str() {
                    "--diagnostic" => {
                        if diagnostic {
                            return Err("odytty attach: --diagnostic specified twice".to_owned());
                        }
                        diagnostic = true;
                    }
                    other if other.starts_with("--") => {
                        return Err(format!("unknown odytty attach argument: {other}"));
                    }
                    other => {
                        if id.is_some() {
                            return Err("odytty attach takes exactly one session id".to_owned());
                        }
                        id = Some(other.to_owned());
                    }
                }
            }
            if diagnostic && id.is_none() {
                return Err("odytty attach --diagnostic requires a session id".to_owned());
            }
            let id = id.unwrap_or_default();
            Ok(Some(SessionCliCommand::Attach(SessionAttachOptions {
                id,
                runtime_base: None,
                diagnostic,
            })))
        }
        _ => Ok(None),
    }
}

pub fn resolve_attach(options: &SessionAttachOptions) -> AnyResult<AttachAction> {
    if options.diagnostic {
        return run_attach_diagnostic(options.clone()).map(AttachAction::PrintCli);
    }
    if !options.id.is_empty() {
        return Ok(AttachAction::LiveWindow(options.id.clone()));
    }
    let sessions = list_live_sessions(options.runtime_base.as_deref())?;
    resolve_attach_from_sessions(options, &sessions)
}

pub fn resolve_attach_from_sessions(
    options: &SessionAttachOptions,
    sessions: &[ListedSession],
) -> AnyResult<AttachAction> {
    if options.diagnostic {
        return Ok(AttachAction::PrintCli(String::new()));
    }
    if !options.id.is_empty() {
        return Ok(AttachAction::LiveWindow(options.id.clone()));
    }
    match sessions {
        [] => bail!("no live sessions to attach"),
        [session] => Ok(AttachAction::LiveWindow(session.id.clone())),
        _ => {
            let mut out = list_sessions_output(sessions);
            out.push_str("multiple live sessions; specify an id: odytty attach <id>\n");
            Ok(AttachAction::PrintCli(out))
        }
    }
}

pub fn run_session_command(command: SessionCliCommand) -> AnyResult<String> {
    match command {
        SessionCliCommand::NewDetached(options) => run_new_detached(options),
        SessionCliCommand::List(options) => list_live_sessions(options.runtime_base.as_deref())
            .map(|sessions| list_sessions_output(&sessions)),
        SessionCliCommand::Attach(options) => run_attach_diagnostic(options),
    }
}

pub fn list_sessions_output(sessions: &[ListedSession]) -> String {
    let mut out = String::new();
    for session in sessions {
        out.push_str(&format_listed_session(session));
        out.push('\n');
    }
    out
}

fn format_listed_session(session: &ListedSession) -> String {
    let label = display_field_value(if session.name == session.id {
        &session.id
    } else {
        &session.name
    });
    let pane_label = if session.pane_count == 1 {
        "1 pane".to_owned()
    } else {
        format!("{} panes", session.pane_count)
    };
    let age = humanize_age_ms(session.age_ms);
    if session.name == session.id {
        format!("{label}\t{pane_label}\t{age}")
    } else {
        format!(
            "{label}\t{pane_label}\t{age}\t({})",
            display_field_value(&session.id)
        )
    }
}

fn humanize_age_ms(age_ms: u128) -> String {
    if age_ms < 1_000 {
        return format!("{age_ms}ms");
    }
    let seconds = age_ms / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

fn display_field_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn parse_session_new(args: &[String]) -> Result<DetachedSessionOptions, String> {
    let mut options = DetachedSessionOptions::default();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--detached" => {
                index += 1;
            }
            "--title" => {
                index += 1;
                options.title = Some(
                    args.get(index)
                        .ok_or_else(|| "--title requires a value".to_owned())?
                        .clone(),
                );
                index += 1;
            }
            "--working-directory" | "--working-dir" => {
                index += 1;
                options.working_directory = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| format!("{arg} requires a value"))?,
                ));
                index += 1;
            }
            "-e" | "--execute" => {
                options.command =
                    DetachedSessionCommand::Exec(command_from_rest(&args[index + 1..])?);
                index = args.len();
            }
            _ => {
                if let Some(title) = arg.strip_prefix("--title=") {
                    options.title = Some(title.to_owned());
                    index += 1;
                } else if let Some(path) = arg.strip_prefix("--working-directory=") {
                    options.working_directory = Some(PathBuf::from(path));
                    index += 1;
                } else if let Some(path) = arg.strip_prefix("--working-dir=") {
                    options.working_directory = Some(PathBuf::from(path));
                    index += 1;
                } else {
                    return Err(format!("unknown odytty new argument: {arg}"));
                }
            }
        }
    }

    Ok(options)
}

fn run_new_detached(options: DetachedSessionOptions) -> AnyResult<String> {
    run_new_detached_with_spawner(options, |config| spawn_host_on_demand(config).map(|_| ()))
}

pub fn run_new_detached_with_spawner(
    options: DetachedSessionOptions,
    spawner: impl FnOnce(&HostConfig) -> AnyResult<()>,
) -> AnyResult<String> {
    let session_id = options.id.unwrap_or_else(generate_session_id);
    let mut config = HostConfig::new(session_id.clone());
    config.runtime_base = options.runtime_base;
    config.command = match options.command {
        DetachedSessionCommand::DefaultShell => HostCommand::DefaultShell {
            working_directory: options.working_directory,
        },
        DetachedSessionCommand::Exec(command) => HostCommand::Exec {
            program: command.program,
            args: command.args,
            working_directory: options.working_directory,
        },
    };
    let paths = config.runtime_paths()?;
    let metadata = SessionMetadata {
        id: session_id.clone(),
        name: options.title.unwrap_or_else(|| session_id.clone()),
        created_unix_ms: now_unix_ms(),
        pane_count: 1,
    };
    write_session_metadata(&paths.dir, &metadata)?;
    spawner(&config)?;
    Ok(format!("id={session_id}\n"))
}

fn run_attach_diagnostic(options: SessionAttachOptions) -> AnyResult<String> {
    let Some(runtime_dir) = existing_runtime_dir(options.runtime_base.as_deref())? else {
        bail!("session not found: {}", options.id);
    };
    let socket_path = session_socket_path(&runtime_dir, &options.id)?;
    if !socket_path.exists() {
        bail!("session not found: {}", options.id);
    }
    let mut client = SessionHostClient::connect(&socket_path, &options.id)
        .with_context(|| format!("attach session {}", options.id))?;
    let snapshot = read_attach_snapshot(&mut client)?;
    let envelope = SnapshotEnvelope::decode(&snapshot, SnapshotEnvelopeCaps::default())
        .context("decode session snapshot")?;
    let _ = client.detach();
    Ok(format!(
        "id={}\tstate=attached\tmode=diagnostic\tcolumns={}\trows={}\tpanes=1\n",
        cli_field_value(&options.id),
        envelope.terminal.dimensions.columns,
        envelope.terminal.dimensions.rows
    ))
}

fn read_attach_snapshot(client: &mut SessionHostClient) -> AnyResult<Vec<u8>> {
    for _ in 0..40 {
        if let Some(frame) = client.read_frame(Duration::from_millis(50))? {
            match frame {
                HostFrame::Snapshot(bytes) => return Ok(bytes),
                HostFrame::SessionExit { .. } => bail!("session exited before snapshot"),
                HostFrame::Error(message) => bail!("session-host error before snapshot: {message}"),
                HostFrame::Output(_) | HostFrame::Invalidate { .. } => {}
            }
        }
    }
    bail!("session attach timed out before snapshot")
}

fn generate_session_id() -> String {
    format!("s-{}-{}", std::process::id(), now_unix_ms())
}

/// Native window options for a **live** `odytty attach <id>`.
///
/// Exactly [`NativeOptions::from_settings`] with `attach_session` set, so the
/// attach launch differs from a normal launch by the attach target alone: the
/// window opens its normal initial local session and then reattaches the hosted
/// session as a live, focused tab (the wiring proven by the session-host e2e).
pub fn native_attach_options(id: &str, settings: &Settings) -> NativeOptions {
    NativeOptions {
        attach_session: Some(id.to_owned()),
        ..NativeOptions::from_settings(settings)
    }
}

/// The full `--help` / `-h` usage text.
///
/// Lives in the library (not `main`) so the documented CLI surface is covered by
/// a test and cannot drift from the real behavior — in particular the
/// `odytty attach` verb, which now opens a live attached window by default.
pub fn usage_text() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut out = String::new();
    out.push_str(&format!("OdyTTY {version}\n"));
    out.push_str("usage: odytty [OPTION]\n\n");
    out.push_str("With no option, launch the native terminal.\n\n");
    out.push_str("Options:\n");
    out.push_str("  --native        launch the native terminal\n");
    out.push_str("  -e COMMAND...   execute a command instead of the user's shell\n");
    out.push_str("  --working-directory DIR\n");
    out.push_str("                  set the initial working directory\n");
    out.push_str("  --title TITLE   set the initial window title\n");
    out.push_str("  --version       print the OdyTTY version and exit\n");
    out.push_str("  --list-themes   list built-in themes and exit\n");
    out.push_str("  --list-fonts    list discoverable monospace fonts and exit\n");
    out.push_str("  --show-config   print the effective configuration and exit\n");
    out.push_str("  --core-smoke    print a parser/core smoke transcript and exit\n");
    out.push_str("  -h, --help      print this help\n");
    out.push('\n');
    out.push_str("Session commands:\n");
    out.push_str("  new [--detached] [-e COMMAND...]\n");
    out.push_str("                  start a detached resumable session and print its id\n");
    out.push_str("  list            list live detached sessions\n");
    out.push_str("  attach [ID]\n");
    out.push_str("                  reattach a detached session in a live native window;\n");
    out.push_str("                  without ID: attach the only live session or list choices\n");
    out.push_str("  attach --diagnostic ID\n");
    out.push_str("                  --diagnostic prints a one-line status and exits\n");
    out
}

/// Parse arguments that launch the native terminal.
///
/// `-e` consumes the rest of the command line as the command argv, matching the
/// convention used by terminal-emulator desktop integration. Options after
/// `-e` are passed to the child command unchanged.
pub fn native_options_for_args(
    args: &[String],
    settings: &Settings,
) -> Result<Option<NativeOptions>, String> {
    let mut options = NativeOptions::from_settings(settings);
    let mut launch_native = args.is_empty();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--native" => {
                launch_native = true;
                index += 1;
            }
            "--title" => {
                index += 1;
                let title = args
                    .get(index)
                    .ok_or_else(|| "--title requires a value".to_owned())?;
                options.title = title.clone();
                launch_native = true;
                index += 1;
            }
            "--working-directory" | "--working-dir" => {
                index += 1;
                let path = args
                    .get(index)
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                options.working_directory = Some(PathBuf::from(path));
                launch_native = true;
                index += 1;
            }
            "-e" | "--execute" => {
                let command = command_from_rest(&args[index + 1..])?;
                options.command = Some(command);
                return Ok(Some(options));
            }
            "--" => {
                let command = command_from_rest(&args[index + 1..])?;
                options.command = Some(command);
                return Ok(Some(options));
            }
            // Prefix-form flags. Written as a plain `if let` chain rather than
            // `if let` match guards (an unstable feature on older toolchains) so
            // the crate builds on a wider range of Rust versions — important for
            // the recommended from-source install path.
            _ => {
                if let Some(title) = arg.strip_prefix("--title=") {
                    options.title = title.to_owned();
                    launch_native = true;
                    index += 1;
                } else if let Some(path) = arg.strip_prefix("--working-directory=") {
                    options.working_directory = Some(PathBuf::from(path));
                    launch_native = true;
                    index += 1;
                } else if let Some(path) = arg.strip_prefix("--working-dir=") {
                    options.working_directory = Some(PathBuf::from(path));
                    launch_native = true;
                    index += 1;
                } else {
                    return Ok(None);
                }
            }
        }
    }

    Ok(launch_native.then_some(options))
}

fn command_from_rest(rest: &[String]) -> Result<NativeCommand, String> {
    let program = rest
        .first()
        .ok_or_else(|| "-e requires a command".to_owned())?;
    Ok(NativeCommand {
        program: OsString::from(program),
        args: rest[1..].iter().map(OsString::from).collect(),
    })
}

/// Machine-friendly system font inventory.
pub fn list_fonts_output() -> String {
    list_fonts_output_for_entries(text::font_inventory())
}

/// Machine-friendly font inventory for an explicit entry set.
pub fn list_fonts_output_for_entries(entries: Vec<FontInventoryEntry>) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!(
            "path={}\tname={}\tmonospace={}\n",
            path_value(&entry.path),
            entry.name,
            bool_value(entry.monospace)
        ));
    }
    out
}

/// Machine-friendly built-in theme inventory.
pub fn list_themes_output() -> String {
    let mut themes = theme::all().to_vec();
    themes.sort_by_key(|theme| theme.name);

    let mut out = String::new();
    for theme in themes {
        out.push_str(&format!(
            "name={}\tappearance={}\tfamily={}\n",
            theme.name,
            appearance(theme),
            theme_family(theme.name)
        ));
    }
    out
}

/// Stable effective settings dump.
pub fn show_config_output(settings: &Settings) -> String {
    let mut rows = vec![
        ("bell", settings.bell.as_str().to_owned()),
        ("bloom", bool_value(settings.bloom).to_owned()),
        ("bloom_intensity", float_value(settings.bloom_intensity)),
        ("bloom_radius", float_value(settings.bloom_radius)),
        ("bloom_threshold", float_value(settings.bloom_threshold)),
        ("crt", bool_value(settings.crt).to_owned()),
        (
            "crt_scanline_intensity",
            float_value(settings.crt_scanline_intensity),
        ),
        (
            "crt_scanline_period",
            float_value(settings.crt_scanline_period),
        ),
        (
            "crt_vignette_strength",
            float_value(settings.crt_vignette_strength),
        ),
        ("crt_curvature", float_value(settings.crt_curvature)),
        ("cursor_blink", settings.cursor_blink.as_str().to_owned()),
        (
            "cursor_style",
            cursor_style_value(settings.cursor_style).to_owned(),
        ),
        (
            "font",
            settings
                .font_path
                .as_deref()
                .map(path_value)
                .unwrap_or_default(),
        ),
        (
            "font_family",
            settings.font_family.clone().unwrap_or_default(),
        ),
        ("font_size", float_value(settings.font_size_px)),
        (
            "interactive_paths",
            bool_value(settings.interactive_paths).to_owned(),
        ),
        (
            "interactive_paths_barewords",
            bool_value(settings.interactive_paths_barewords).to_owned(),
        ),
        (
            "interactive_paths_click_hint",
            bool_value(settings.interactive_paths_click_hint).to_owned(),
        ),
        (
            "interactive_paths_image_inline",
            bool_value(settings.interactive_paths_image_inline).to_owned(),
        ),
        (
            "interactive_paths_editor",
            settings.interactive_paths_editor.clone(),
        ),
        ("keybinds", key_bindings_value(&settings.key_bindings)),
        (
            "native_autoclose_ms",
            settings
                .native_autoclose
                .map(duration_millis_value)
                .unwrap_or_default(),
        ),
        ("osc52_read", bool_value(settings.osc52_read).to_owned()),
        (
            "render_quality",
            settings.render_quality.as_str().to_owned(),
        ),
        ("retro", bool_value(settings.retro).to_owned()),
        ("stem_darken", float_value(settings.stem_darken)),
        (
            "symbol_fallback",
            bool_value(settings.symbol_fallback).to_owned(),
        ),
        ("symbol_font_source", symbol_font_source_value(settings)),
        ("subpixel", subpixel_value(settings.subpixel).to_owned()),
        (
            "synthetic_styles",
            bool_value(settings.synthetic_styles).to_owned(),
        ),
        ("text_gamma", float_value(settings.text_gamma)),
        ("theme", settings.theme.name.to_owned()),
        ("visual", visual_value(settings.visual).to_owned()),
        ("window_padding", float_value(settings.window_padding_px)),
    ];
    rows.sort_by_key(|(key, _)| *key);

    let mut out = String::new();
    for (key, value) in rows {
        out.push_str(key);
        out.push('=');
        out.push_str(&value);
        out.push('\n');
    }
    out
}

/// The symbol / Nerd-font fallback source for `--show-config` diagnostics.
///
/// Reports `disabled` when the fallback is off; otherwise the **chain** the
/// renderer would install, in order, under the precedence explicit > bundled
/// (v3, then v2) > host — joined with ` > ` (e.g. `bundled > bundled > host`,
/// or `none` when no face resolved). The atlas walks this chain per glyph, so
/// coverage is the union of all listed faces. This is exactly the diagnostic
/// that makes "why is my prompt icon tofu / which fonts are in play" answerable
/// without source diving.
fn symbol_font_source_value(settings: &Settings) -> String {
    if !settings.symbol_fallback {
        return "disabled".to_owned();
    }
    let (sources, _) = text::resolve_symbol_fonts_with_source(
        settings.symbol_font.as_deref(),
        &text::font_search_dirs(),
    );
    if sources.is_empty() {
        return "none".to_owned();
    }
    sources
        .iter()
        .map(|s| s.describe())
        .collect::<Vec<_>>()
        .join(" > ")
}

fn appearance(theme: Theme) -> &'static str {
    if relative_luminance(theme.background) > 0.18 {
        "light"
    } else {
        "dark"
    }
}

fn theme_family(name: &str) -> &'static str {
    if name == "plain" {
        "baseline"
    } else if name.starts_with("odyssey") {
        "odyssey"
    } else {
        "community"
    }
}

fn float_value(value: f32) -> String {
    let formatted = format!("{value:.2}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn bool_value(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn cli_field_value(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ch if ch.is_control() => out.push(' '),
            ch => out.push(ch),
        }
    }
    out
}

fn path_value(path: &Path) -> String {
    path.display().to_string()
}

fn duration_millis_value(duration: Duration) -> String {
    duration.as_millis().to_string()
}

fn visual_value(value: VisualEffect) -> &'static str {
    value.as_str()
}

fn subpixel_value(value: SubpixelMode) -> &'static str {
    match value {
        SubpixelMode::Off => "off",
        SubpixelMode::Rgb => "rgb",
        SubpixelMode::Bgr => "bgr",
    }
}

fn cursor_style_value(value: CursorStyle) -> &'static str {
    match value {
        CursorStyle::Block => "block",
        CursorStyle::Underline => "underline",
        CursorStyle::Bar => "bar",
    }
}

fn key_bindings_value(bindings: &[KeyBindingOverride]) -> String {
    bindings
        .iter()
        .map(key_binding_value)
        .collect::<Vec<_>>()
        .join(";")
}

fn key_binding_value(binding: &KeyBindingOverride) -> String {
    format!(
        "{}={}",
        chord_value(binding.chord),
        action_value(binding.action)
    )
}

fn chord_value(chord: KeyChord) -> String {
    let mut parts = Vec::new();
    if chord.modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if chord.modifiers.shift {
        parts.push("shift".to_owned());
    }
    if chord.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if chord.modifiers.super_key {
        parts.push("super".to_owned());
    }
    parts.push(key_value(chord.key));
    parts.join("+")
}

fn key_value(key: KeyBindingKey) -> String {
    match key {
        KeyBindingKey::Character(',') => "comma".to_owned(),
        KeyBindingKey::Character(ch) => ch.to_string(),
        KeyBindingKey::Named(named) => match named {
            KeyBindingNamedKey::Enter => "enter".to_owned(),
            KeyBindingNamedKey::Backspace => "backspace".to_owned(),
            KeyBindingNamedKey::Escape => "esc".to_owned(),
            KeyBindingNamedKey::Tab => "tab".to_owned(),
            KeyBindingNamedKey::Space => "space".to_owned(),
            KeyBindingNamedKey::PageUp => "pageup".to_owned(),
            KeyBindingNamedKey::PageDown => "pagedown".to_owned(),
            KeyBindingNamedKey::Home => "home".to_owned(),
            KeyBindingNamedKey::End => "end".to_owned(),
            KeyBindingNamedKey::Delete => "delete".to_owned(),
            KeyBindingNamedKey::Insert => "insert".to_owned(),
            KeyBindingNamedKey::ArrowUp => "up".to_owned(),
            KeyBindingNamedKey::ArrowDown => "down".to_owned(),
            KeyBindingNamedKey::ArrowLeft => "left".to_owned(),
            KeyBindingNamedKey::ArrowRight => "right".to_owned(),
            KeyBindingNamedKey::F(number) => format!("f{number}"),
        },
    }
}

fn action_value(action: BindableAction) -> &'static str {
    match action {
        BindableAction::Search => "search",
        BindableAction::SettingsPanel => "settings",
        BindableAction::ThemePicker => "theme-picker",
        BindableAction::Copy => "copy",
        BindableAction::Paste => "paste",
        BindableAction::ScrollPageUp => "scroll-up",
        BindableAction::ScrollPageDown => "scroll-down",
        BindableAction::JumpPromptPrev => "jump-prompt-prev",
        BindableAction::JumpPromptNext => "jump-prompt-next",
        BindableAction::CopyMode => "copy-mode",
        BindableAction::Hints => "hints",
        BindableAction::ClearInput => "clear-input",
        BindableAction::CommandPalette => "command-palette",
        BindableAction::SessionReplay => "session-replay",
        BindableAction::ConnectionManager => "connection-manager",
        BindableAction::ThemeBuilder => "theme-builder",
        BindableAction::SessionAttach => "session-attach",
        BindableAction::NewTab => "new-tab",
        BindableAction::NextTab => "next-tab",
        BindableAction::PrevTab => "prev-tab",
        BindableAction::CloseTab => "close-tab",
        BindableAction::SplitColumns => "split-columns",
        BindableAction::SplitRows => "split-rows",
        BindableAction::FocusPaneLeft => "focus-pane-left",
        BindableAction::FocusPaneRight => "focus-pane-right",
        BindableAction::FocusPaneUp => "focus-pane-up",
        BindableAction::FocusPaneDown => "focus-pane-down",
        BindableAction::FocusPaneNext => "focus-pane-next",
        BindableAction::ClosePane => "close-pane",
        BindableAction::ZoomPane => "zoom-pane",
        BindableAction::EqualizePanes => "equalize-panes",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn native_parse_exec_consumes_rest_as_child_argv() {
        let options = native_options_for_args(
            &strings(&[
                "--title",
                "Monitor",
                "--working-directory",
                "/tmp",
                "-e",
                "btop",
                "--utf-force",
            ]),
            &Settings::default(),
        )
        .expect("parse")
        .expect("native options");

        assert_eq!(options.title, "Monitor");
        assert_eq!(options.working_directory, Some(PathBuf::from("/tmp")));
        let command = options.command.expect("command");
        assert_eq!(command.program, OsString::from("btop"));
        assert_eq!(command.args, vec![OsString::from("--utf-force")]);
    }

    #[test]
    fn native_parse_rejects_empty_exec() {
        let err = native_options_for_args(&strings(&["-e"]), &Settings::default())
            .expect_err("empty -e should fail");
        assert_eq!(err, "-e requires a command");
    }

    #[test]
    fn native_parse_unknown_argument_is_not_native() {
        assert!(
            native_options_for_args(&strings(&["--bogus"]), &Settings::default())
                .expect("parse")
                .is_none()
        );
    }

    #[test]
    fn show_config_output_includes_interactive_path_knobs() {
        let settings = Settings {
            interactive_paths: true,
            interactive_paths_barewords: false,
            interactive_paths_click_hint: false,
            interactive_paths_image_inline: false,
            interactive_paths_editor: "code --goto {file}:{line}:{col}".to_owned(),
            ..Settings::default()
        };

        let output = show_config_output(&settings);

        assert!(output.lines().any(|line| line == "interactive_paths=on"));
        assert!(
            output
                .lines()
                .any(|line| line == "interactive_paths_barewords=off")
        );
        assert!(
            output
                .lines()
                .any(|line| line == "interactive_paths_click_hint=off")
        );
        assert!(
            output
                .lines()
                .any(|line| line == "interactive_paths_image_inline=off")
        );
        assert!(
            output
                .lines()
                .any(|line| line == "interactive_paths_editor=code --goto {file}:{line}:{col}")
        );
    }
}
