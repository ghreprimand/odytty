// SPDX-License-Identifier: GPL-3.0-only
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use odytty::core::Dimensions;
use odytty::native::NativeCommand;
use odytty::session_host::{
    HostCommand, HostConfig, HostExitReason, ListedSession, SessionMetadata, now_unix_ms, run_host,
    write_session_metadata,
};
use odytty::text::FontInventoryEntry;

#[path = "../src/cli.rs"]
mod cli;

#[test]
fn list_themes_output_contains_every_builtin_theme() {
    let output = cli::list_themes_output();
    assert_eq!(
        cli::output_for_args(&["--list-themes".to_owned()]).as_deref(),
        Some(output.as_str())
    );
    let lines = output.lines().collect::<Vec<_>>();

    for name in odytty::theme::names() {
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&format!("name={name}\t"))),
            "--list-themes output missing {name}"
        );
    }
    assert!(lines.iter().any(|line| line.contains("appearance=light")));
    assert!(lines.iter().any(|line| line.contains("appearance=dark")));
    assert!(lines.iter().any(|line| line.contains("family=odyssey")));
    assert!(lines.iter().any(|line| line.contains("family=community")));

    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted, "--list-themes output must be sorted");
}

#[test]
fn list_fonts_output_formats_inventory_rows() {
    let output = cli::list_fonts_output_for_entries(vec![
        FontInventoryEntry {
            name: "AlphaMono".to_owned(),
            path: PathBuf::from("fixtures/fonts/AlphaMono.ttf"),
            monospace: true,
        },
        FontInventoryEntry {
            name: "PosterSans".to_owned(),
            path: PathBuf::from("fixtures/fonts/PosterSans.otf"),
            monospace: false,
        },
    ]);

    assert_contains_line(
        &output,
        "path=fixtures/fonts/AlphaMono.ttf\tname=AlphaMono\tmonospace=on",
    );
    assert_contains_line(
        &output,
        "path=fixtures/fonts/PosterSans.otf\tname=PosterSans\tmonospace=off",
    );
}

#[test]
fn output_for_args_supports_list_fonts() {
    assert!(cli::output_for_args(&["--list-fonts".to_owned()]).is_some());
}

#[test]
fn session_command_parser_accepts_new_default_detached_with_exec_options() {
    let command = cli::session_command_for_args(&[
        "new".to_owned(),
        "--title".to_owned(),
        "Demo".to_owned(),
        "--working-directory=/tmp".to_owned(),
        "-e".to_owned(),
        "bash".to_owned(),
        "-lc".to_owned(),
        "printf ok".to_owned(),
    ])
    .expect("parse")
    .expect("session command");

    let cli::SessionCliCommand::NewDetached(options) = command else {
        panic!("expected new detached command");
    };
    assert_eq!(options.title.as_deref(), Some("Demo"));
    assert_eq!(
        options.working_directory.as_deref(),
        Some(Path::new("/tmp"))
    );
    assert_eq!(
        options.command,
        cli::DetachedSessionCommand::Exec(NativeCommand {
            program: "bash".into(),
            args: vec!["-lc".into(), "printf ok".into()],
        })
    );
}

#[test]
fn session_command_parser_accepts_new_detached_alias() {
    let default = cli::session_command_for_args(&["new".to_owned()])
        .expect("parse default")
        .expect("default new command");
    let alias = cli::session_command_for_args(&["new".to_owned(), "--detached".to_owned()])
        .expect("parse alias")
        .expect("alias new command");

    assert_eq!(alias, default);
    assert!(matches!(default, cli::SessionCliCommand::NewDetached(_)));
}

#[test]
fn session_command_parser_accepts_list_and_attach() {
    assert!(matches!(
        cli::session_command_for_args(&["list".to_owned()]).expect("parse"),
        Some(cli::SessionCliCommand::List(_))
    ));
    assert!(matches!(
        cli::session_command_for_args(&["attach".to_owned(), "s-1".to_owned()]).expect("parse"),
        Some(cli::SessionCliCommand::Attach(cli::SessionAttachOptions { id, .. })) if id == "s-1"
    ));
    assert!(matches!(
        cli::session_command_for_args(&["attach".to_owned()]).expect("parse"),
        Some(cli::SessionCliCommand::Attach(cli::SessionAttachOptions { id, diagnostic, .. }))
            if id.is_empty() && !diagnostic
    ));
}

#[test]
fn attach_without_flag_is_live_and_carries_the_session_id() {
    // `odytty attach <id>` (no flag) is a LIVE attach: it must route to the
    // native window path with the session id intact.
    let command = cli::session_command_for_args(&["attach".to_owned(), "s-live".to_owned()])
        .expect("parse")
        .expect("attach command");
    assert!(matches!(
        &command,
        cli::SessionCliCommand::Attach(cli::SessionAttachOptions { id, diagnostic, .. })
            if id == "s-live" && !*diagnostic
    ));
    assert_eq!(
        command.live_attach_id(),
        Some("s-live"),
        "no-flag attach must route to the native window"
    );
}

#[test]
fn attach_diagnostic_flag_stays_cli_only() {
    // `--diagnostic` preserves the script-friendly dump path: it must NOT route
    // to the native window. Flag order is irrelevant.
    for args in [
        vec![
            "attach".to_owned(),
            "--diagnostic".to_owned(),
            "s-1".to_owned(),
        ],
        vec![
            "attach".to_owned(),
            "s-1".to_owned(),
            "--diagnostic".to_owned(),
        ],
    ] {
        let command = cli::session_command_for_args(&args)
            .expect("parse")
            .expect("attach command");
        assert!(matches!(
            &command,
            cli::SessionCliCommand::Attach(cli::SessionAttachOptions { id, diagnostic, .. })
                if id == "s-1" && *diagnostic
        ));
        assert_eq!(
            command.live_attach_id(),
            None,
            "diagnostic attach must stay CLI-only"
        );
    }
}

#[test]
fn attach_rejects_unknown_flag_and_extra_id() {
    assert_eq!(
        cli::session_command_for_args(&["attach".to_owned(), "--bogus".to_owned()]).unwrap_err(),
        "unknown odytty attach argument: --bogus"
    );
    assert_eq!(
        cli::session_command_for_args(&["attach".to_owned(), "s-1".to_owned(), "s-2".to_owned(),])
            .unwrap_err(),
        "odytty attach takes exactly one session id"
    );
}

#[test]
fn live_attach_id_is_none_for_list_and_new_detached() {
    // list and new must never route to the native window.
    let list = cli::session_command_for_args(&["list".to_owned()])
        .expect("parse")
        .expect("list command");
    assert_eq!(list.live_attach_id(), None);

    let new = cli::session_command_for_args(&["new".to_owned()])
        .expect("parse")
        .expect("new command");
    assert_eq!(new.live_attach_id(), None);
}

#[test]
fn native_attach_options_set_attach_session_and_nothing_else() {
    // The CLI arg must flow into NativeOptions.attach_session, and every other
    // field must equal the normal settings-derived launch — so the attach
    // launch differs from a normal launch only by the attach target.
    let settings = odytty::settings::Settings::default();
    let baseline = odytty::native::NativeOptions::from_settings(&settings);
    let attach = cli::native_attach_options("s-1", &settings);

    assert_eq!(attach.attach_session.as_deref(), Some("s-1"));
    assert_eq!(
        odytty::native::NativeOptions {
            attach_session: None,
            ..attach
        },
        baseline,
        "attach options must match a normal launch apart from attach_session"
    );
}

#[test]
fn usage_text_documents_live_attach_and_drops_pending_wording() {
    let usage = cli::usage_text();
    assert!(
        usage.contains("attach [ID]"),
        "usage must document optional attach id: {usage}"
    );
    assert!(
        usage.contains("reattach a detached session in a live native window"),
        "usage must describe the live attach behavior: {usage}"
    );
    assert!(
        usage.contains("without ID: attach the only live session or list choices"),
        "usage must document no-id attach behavior: {usage}"
    );
    assert!(
        usage.contains("attach --diagnostic ID"),
        "usage must document diagnostic attach id: {usage}"
    );
    assert!(
        !usage.contains("pending"),
        "stale 'reattach is pending' wording must be gone: {usage}"
    );
    // The other documented verbs are unchanged.
    assert!(usage.contains("list            list live detached sessions"));
    assert!(usage.contains("new [--detached] [-e COMMAND...]"));
}

#[test]
fn session_command_parser_rejects_incomplete_session_commands() {
    assert_eq!(
        cli::session_command_for_args(&["attach".to_owned(), "--diagnostic".to_owned()])
            .unwrap_err(),
        "odytty attach --diagnostic requires a session id"
    );
}

#[test]
fn list_sessions_output_is_readable_and_name_first() {
    let output = cli::list_sessions_output(&[
        ListedSession {
            id: "s-0001-aaaa".to_owned(),
            name: "build".to_owned(),
            state: "running",
            age_ms: 42,
            pane_count: 1,
        },
        ListedSession {
            id: "s-0002-bbbb".to_owned(),
            name: "s-0002-bbbb".to_owned(),
            state: "running",
            age_ms: 65_000,
            pane_count: 2,
        },
    ]);

    assert_eq!(
        output,
        "build\t1 pane\t42ms\t(s-0001-aaaa)\ns-0002-bbbb\t2 panes\t1m\n"
    );
}

#[test]
fn attach_no_id_resolver_errors_when_no_sessions_exist() {
    let options = cli::SessionAttachOptions {
        id: String::new(),
        runtime_base: None,
        diagnostic: false,
    };
    let error = cli::resolve_attach_from_sessions(&options, &[]).unwrap_err();
    assert_eq!(error.to_string(), "no live sessions to attach");
}

#[test]
fn attach_no_id_resolver_attaches_single_session() {
    let options = cli::SessionAttachOptions {
        id: String::new(),
        runtime_base: None,
        diagnostic: false,
    };
    let action = cli::resolve_attach_from_sessions(
        &options,
        &[ListedSession {
            id: "s-0001-aaaa".to_owned(),
            name: "build".to_owned(),
            state: "running",
            age_ms: 1_000,
            pane_count: 1,
        }],
    )
    .expect("resolve attach");
    assert_eq!(
        action,
        cli::AttachAction::LiveWindow("s-0001-aaaa".to_owned())
    );
}

#[test]
fn attach_resolver_keeps_explicit_id_live_without_enumerating() {
    let action = cli::resolve_attach(&cli::SessionAttachOptions {
        id: "s-0001-aaaa".to_owned(),
        runtime_base: None,
        diagnostic: false,
    })
    .expect("resolve attach");

    assert_eq!(
        action,
        cli::AttachAction::LiveWindow("s-0001-aaaa".to_owned())
    );
}

#[test]
fn attach_no_id_resolver_lists_multiple_sessions_and_requires_choice() {
    let options = cli::SessionAttachOptions {
        id: String::new(),
        runtime_base: None,
        diagnostic: false,
    };
    let action = cli::resolve_attach_from_sessions(
        &options,
        &[
            ListedSession {
                id: "s-0001-aaaa".to_owned(),
                name: "build".to_owned(),
                state: "running",
                age_ms: 1_000,
                pane_count: 1,
            },
            ListedSession {
                id: "s-0002-bbbb".to_owned(),
                name: "web".to_owned(),
                state: "running",
                age_ms: 2_000,
                pane_count: 2,
            },
        ],
    )
    .expect("resolve attach");

    let cli::AttachAction::PrintCli(output) = action else {
        panic!("multiple sessions must print choices");
    };
    assert!(output.contains("build\t1 pane\t1s\t(s-0001-aaaa)"));
    assert!(output.contains("web\t2 panes\t2s\t(s-0002-bbbb)"));
    assert!(output.contains("multiple live sessions; specify an id: odytty attach <id>"));
}

#[test]
fn new_detached_prints_session_id_without_spawning_in_tests() {
    // Short prefix: this test binds a real session-host socket, so the runtime
    // base must stay well under the macOS 104-byte AF_UNIX `sun_path` limit.
    let temp = TempDir::new("cli-nd");
    let options = cli::DetachedSessionOptions {
        id: Some("s-test".to_owned()),
        title: Some("Test Session".to_owned()),
        runtime_base: Some(temp.path().to_owned()),
        ..cli::DetachedSessionOptions::default()
    };
    let output = cli::run_new_detached_with_spawner(options.clone(), |config| {
        assert_eq!(config.session_id, "s-test");
        Ok(())
    })
    .expect("new detached output");

    assert_eq!(output, "id=s-test\n");
}

#[test]
fn attach_reports_unknown_session_without_creating_daemons() {
    let temp = TempDir::new("odytty-cli-attach-missing");
    let error =
        cli::run_session_command(cli::SessionCliCommand::Attach(cli::SessionAttachOptions {
            id: "missing".to_owned(),
            runtime_base: Some(temp.path().to_owned()),
            diagnostic: true,
        }))
        .unwrap_err();

    assert!(
        error.to_string().contains("session not found: missing"),
        "unexpected error: {error}"
    );
}

#[test]
fn list_and_attach_use_live_session_host_without_scrollback_dump() {
    // Short prefix: this test binds a real session-host socket, so the runtime
    // base must stay well under the macOS 104-byte AF_UNIX `sun_path` limit.
    let temp = TempDir::new("cli-ls");
    let mut config = HostConfig::new("s-live");
    config.runtime_base = Some(temp.path().to_owned());
    config.command = HostCommand::ShellCommand("printf private-output; sleep 5".to_owned());
    config.dimensions = Dimensions::new(80, 24);
    config.detached_idle_timeout = Duration::from_millis(1500);
    let paths = config.runtime_paths().expect("runtime paths");
    write_session_metadata(
        &paths.dir,
        &SessionMetadata {
            id: "s-live".to_owned(),
            name: "Demo".to_owned(),
            created_unix_ms: now_unix_ms(),
            pane_count: 1,
        },
    )
    .expect("write metadata");

    let socket = paths.socket.clone();
    let host = thread::spawn(move || run_host(config));
    wait_for_socket(&socket);

    let list_output =
        cli::run_session_command(cli::SessionCliCommand::List(cli::SessionListOptions {
            runtime_base: Some(temp.path().to_owned()),
        }))
        .expect("list sessions");
    assert!(
        list_output
            .lines()
            .any(|line| line.starts_with("Demo\t1 pane\t")),
        "s-live not listed: {list_output}"
    );
    assert!(list_output.contains("(s-live)"));
    assert!(
        !list_output.contains("private-output"),
        "list output must not include scrollback"
    );

    let attach_output =
        cli::run_session_command(cli::SessionCliCommand::Attach(cli::SessionAttachOptions {
            id: "s-live".to_owned(),
            runtime_base: Some(temp.path().to_owned()),
            diagnostic: true,
        }))
        .expect("diagnostic attach");
    assert_eq!(
        attach_output,
        "id=s-live\tstate=attached\tmode=diagnostic\tcolumns=80\trows=24\tpanes=1\n"
    );

    let exit = host
        .join()
        .expect("host thread")
        .expect("host exits cleanly");
    assert_eq!(exit.reason, HostExitReason::DetachedIdleTimeout);
}

#[test]
fn show_config_output_formats_default_settings() {
    let output = cli::show_config_output(&odytty::settings::Settings::default());

    assert_contains_line(&output, "theme=odyssey-default");
    assert_contains_line(&output, "visual=ambient");
    assert_contains_line(
        &output,
        &format!("font_family={}", odytty::text::BUNDLED_FONT_FAMILY),
    );
    assert_contains_line(
        &output,
        &format!(
            "font_size={}",
            odytty::settings::DEFAULT_FONT_SIZE_PX as usize
        ),
    );
    assert_contains_line(&output, "render_quality=high");
    assert_contains_line(&output, "retro=off");
    assert_contains_line(&output, "window_padding=4");
    assert_contains_line(&output, "bloom=on");
    assert_contains_line(&output, "crt=on");
    assert_contains_line(&output, "keybinds=");
    assert_contains_line(&output, "synthetic_styles=on");
    // Symbol-fallback diagnostics: on by default, backed by the bundled chain.
    // The chain leads with both bundled faces (v3, then v2); a host Nerd font,
    // if present, appends a machine-specific `host:<path>`, so assert the
    // deterministic bundled prefix rather than an exact line.
    assert_contains_line(&output, "symbol_fallback=on");
    assert!(
        output
            .lines()
            .any(|line| line.starts_with("symbol_font_source=bundled > bundled")),
        "output missing symbol_font_source bundled v3+v2 chain prefix:\n{output}"
    );

    let lines = output.lines().collect::<Vec<_>>();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted, "show_config_output must be sorted");
}

#[test]
fn show_config_reads_temp_config_and_applies_env_override() {
    let temp = TempDir::new("odytty-cli-show-config");
    let config_dir = temp.path().join("odytty");
    fs::create_dir_all(&config_dir).expect("create config dir");
    fs::write(
        config_dir.join("odytty.conf"),
        "theme = odyssey\nfont_size = 16\ncursor_blink = off\nsubpixel = rgb\n",
    )
    .expect("write temp config");

    let output = Command::new(odytty_bin())
        .arg("--show-config")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", temp.path())
        .env("HOME", temp.path())
        .env("ODYTTY_FONT_SIZE", "21")
        .env("ODYTTY_RENDER_QUALITY", "plain")
        .output()
        .expect("run odytty --show-config");

    assert!(
        output.status.success(),
        "--show-config failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert_contains_line(&stdout, "theme=odyssey");
    assert_contains_line(&stdout, "font_size=21");
    assert_contains_line(&stdout, "render_quality=plain");
    assert_contains_line(&stdout, "window_padding=4");
    assert_contains_line(&stdout, "cursor_blink=off");
    assert_contains_line(&stdout, "subpixel=rgb");
    assert_contains_line(&stdout, "visual=ambient");

    let lines = stdout.lines().collect::<Vec<_>>();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted, "--show-config output must be sorted");
}

fn odytty_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_odytty")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_odytty")))
}

fn assert_contains_line(output: &str, expected: &str) {
    assert!(
        output.lines().any(|line| line == expected),
        "output missing {expected:?}:\n{output}"
    );
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        // Keep this directory name SHORT (pid + a process-global counter, no nanos
        // timestamp). The CLI session-host binds `<base>/odytty/session-<id>.sock`
        // and, on macOS, `std::env::temp_dir()` is a long `/var/folders/.../T/`
        // path; a verbose, nanos-timestamped base overflows the 104-byte AF_UNIX
        // `sun_path` limit and the host refuses to bind. pid disambiguates across
        // processes and the counter within one, matching the lib-test and e2e
        // helpers. Pass a short `prefix`.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{seq}", std::process::id()));
        fs::create_dir(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn wait_for_socket(socket_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match UnixStream::connect(socket_path) {
            Ok(_) => return,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("socket did not become ready: {error}"),
        }
    }
}
