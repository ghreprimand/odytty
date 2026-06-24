// SPDX-License-Identifier: GPL-3.0-only
use anyhow::Result;
use odytty::app::run_interactive;
use odytty::core::Terminal;
use odytty::native::run_native;
use odytty::pty::PtySession;
use odytty::settings::Settings;

mod cli;

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = std::env::args().skip(1).collect::<Vec<_>>();
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

    if args.first().map(String::as_str) == Some("--dump-command") {
        let command = args
            .get(1)
            .map(String::as_str)
            .unwrap_or("printf 'OdyTTY\\r\\n'");
        return dump_command(command);
    }

    if args.first().map(String::as_str) == Some("session-host") {
        return odytty::session_host::run_internal_host_from_args(&args[1..]);
    }

    if let Some(command) =
        cli::session_command_for_args(&args).map_err(|err| anyhow::anyhow!(err))?
    {
        // Live `odytty attach <id>` opens a native window reattached to the
        // hosted session (the operator-chosen v0.3.0 behavior). Every other
        // session subcommand — including `attach --diagnostic` — stays CLI-only.
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

    if args.first().map(String::as_str) == Some("--interactive") {
        return run_interactive();
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
