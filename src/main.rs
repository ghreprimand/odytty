// SPDX-License-Identifier: GPL-3.0-only
//
// On Windows, OdyTTY is built as a GUI-subsystem binary (`windows_subsystem =
// "windows"`) — the canonical posture for a windowed terminal (Windows
// Terminal, Alacritty, WezTerm all do this). No console is allocated or
// inherited at launch, which removes the console-vs-ConPTY interaction the
// earlier `FreeConsole` workaround papered over and stops a console window
// flashing on an Explorer double-click. The tradeoff: the CLI/introspection
// paths (`--version`/`--help`/…) have no stdout by default, so
// `attach_parent_console_for_cli` reattaches to the launching console when a
// CLI argument is present. The Unix build is unaffected.
#![cfg_attr(windows, windows_subsystem = "windows")]
use anyhow::Result;
#[cfg(unix)]
use odytty::app::run_interactive;
use odytty::core::Terminal;
use odytty::native::run_native;
use odytty::pty::PtySession;
use odytty::settings::Settings;

mod cli;

fn main() -> Result<()> {
    // FREEZE-HARDEN (c): default logging tees WARN+ records to stderr AND a
    // size-capped rotated file at `$XDG_STATE_HOME/odytty/odytty.log`, so a
    // launcher that redirects stderr to /dev/null no longer discards the only
    // evidence of a crash or stall. Lazy: the file is only created when a
    // record is actually emitted, so CLI invocations stay disk-silent.
    odytty::logging::init();

    let args = std::env::args().skip(1).collect::<Vec<_>>();

    // Windows GUI-subsystem builds start with no console, so a CLI invocation
    // run from a shell would otherwise print nothing (stdout is null and Rust
    // silently swallows the writes). When a CLI argument is present, reattach to
    // the parent process's console BEFORE any output so `--version`/`--help`/
    // `--dump-command`/`session-host`/`--core-smoke` print from a console;
    // launched from Explorer there is no parent console and this is a harmless
    // no-op. No-op on Unix.
    #[cfg(windows)]
    attach_parent_console_for_cli(&args);
    if let Some(output) = cli::output_for_args(&args) {
        print!("{output}");
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("--version") {
        println!("odytty {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        print_usage();
        return Ok(());
    }

    if let Some(output) =
        cli::shell_integration_output(&args).map_err(|err| anyhow::anyhow!(err))?
    {
        print!("{output}");
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("--dump-command") {
        let command = args
            .get(1)
            .map(String::as_str)
            .unwrap_or("printf 'OdyTTY\\r\\n'");
        return dump_command(command);
    }

    if args.first().map(String::as_str) == Some("session-host") {
        #[cfg(unix)]
        {
            return odytty::session_host::run_internal_host_from_args(&args[1..]);
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("session-host is not supported on Windows yet");
        }
    }

    // Session subcommand PARSING is cross-platform (stable `--help`/usage), but
    // the detached-session EXECUTION path (host/attach over Unix sockets) is
    // Unix-only — on Windows the parsed command is rejected with a clean message
    // rather than panicking or silently no-opping.
    if let Some(command) =
        cli::session_command_for_args(&args).map_err(|err| anyhow::anyhow!(err))?
    {
        #[cfg(unix)]
        {
            // Live `odytty attach <id>` opens a native window reattached to the
            // hosted session (the operator-chosen v0.3.0 behavior). Every other
            // session subcommand — including `attach --diagnostic` — stays
            // CLI-only.
            if let Some(session_id) = command.live_attach_id() {
                let settings = Settings::from_env();
                let options = cli::native_attach_options(session_id, &settings);
                run_native(options, settings)?;
                return Ok(());
            }
            if let cli::SessionCliCommand::Attach(options) = &command {
                match cli::resolve_attach(options)? {
                    cli::AttachAction::LiveWindow(session_id) => {
                        let settings = Settings::from_env();
                        let options = cli::native_attach_options(&session_id, &settings);
                        run_native(options, settings)?;
                    }
                    cli::AttachAction::PrintCli(output) => {
                        print!("{output}");
                    }
                }
                return Ok(());
            }
            print!("{}", cli::run_session_command(command)?);
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            let _ = command;
            anyhow::bail!("resumable sessions are not supported on Windows yet");
        }
    }

    if args.first().map(String::as_str) == Some("--interactive") {
        #[cfg(unix)]
        {
            return run_interactive();
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("--interactive is not supported on Windows yet");
        }
    }

    let settings = Settings::from_env();
    if let Some(options) =
        cli::native_options_for_args(&args, &settings).map_err(|err| anyhow::anyhow!(err))?
    {
        // Opens a real native window and runs the event loop until the window is
        // closed, with runtime settings loaded once for the native session.
        run_native(options, settings)?;
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("--core-smoke") {
        return core_smoke();
    }

    eprintln!("unknown argument: {}", args[0]);
    eprintln!(
        "usage: odytty [--native] [--title TITLE] [--working-directory DIR] [-e COMMAND [ARGS...]]"
    );
    std::process::exit(2);
}

fn print_usage() {
    // The usage text lives in `cli` so it is unit-tested and cannot drift from
    // the real CLI behavior (notably the live `odytty attach` verb).
    print!("{}", cli::usage_text());
}

fn core_smoke() -> Result<()> {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b[1;36mOdyTTY\x1b[0m core skeleton\r\n");
    terminal.advance(b"owned grid + owned parser are online\r\n");

    println!("{}", terminal.screen().plain_text());
    Ok(())
}

fn dump_command(command: &str) -> Result<()> {
    let mut terminal = Terminal::new(80, 24);
    let mut session = PtySession::spawn_shell_command(terminal.screen().dimensions(), command)?;
    let output = session.read_to_end()?;
    terminal.advance(&output);
    session.wait()?;

    println!("{}", terminal.screen().plain_text());
    Ok(())
}

/// Reattach a Windows GUI-subsystem build to its launching console for CLI
/// invocations, so introspection output reaches the shell that ran it.
///
/// Under `windows_subsystem = "windows"` the process starts with no console, so
/// any `println!` goes to a null handle and is silently dropped. When the
/// command line carries an argument (every CLI/print path —
/// `--version`/`--help`/`--dump-command`/`session-host`/the session verbs/
/// `--core-smoke`/`--list-*`/`--show-config` — takes one), reattach to the
/// parent process's console here, before `main` produces any output.
///
/// `AttachConsole(ATTACH_PARENT_PROCESS)` succeeds when the launcher owns a
/// console (a shell) and the subsequent prints land there; it fails when there
/// is no parent console (an Explorer double-click), in which case the prints are
/// harmlessly swallowed — an Explorer launch of `--version` has no reader anyway.
/// The no-argument GUI launch is intentionally left detached, so a console never
/// flashes when opening the terminal window. The result is ignored either way.
#[cfg(windows)]
fn attach_parent_console_for_cli(args: &[String]) {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    if args.is_empty() {
        return;
    }
    // SAFETY: `AttachConsole` takes a process-id argument and is safe to call in
    // any console state; a failure (no parent console) is expected and ignored.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
