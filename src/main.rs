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
    println!("OdyTTY {}", env!("CARGO_PKG_VERSION"));
    println!("usage: odytty [OPTION]");
    println!();
    println!("With no option, launch the native terminal.");
    println!();
    println!("Options:");
    println!("  --native        launch the native terminal");
    println!("  -e COMMAND...   execute a command instead of the user's shell");
    println!("  --working-directory DIR");
    println!("                  set the initial working directory");
    println!("  --title TITLE   set the initial window title");
    println!("  --version       print the OdyTTY version and exit");
    println!("  --list-themes   list built-in themes and exit");
    println!("  --list-fonts    list discoverable monospace fonts and exit");
    println!("  --show-config   print the effective configuration and exit");
    println!("  --core-smoke    print a parser/core smoke transcript and exit");
    println!("  -h, --help      print this help");
    println!();
    println!("Session commands:");
    println!("  new --detached [-e COMMAND...]");
    println!("                  start a detached resumable session and print its id");
    println!("  list            list live detached sessions");
    println!("  attach ID       diagnostic attach; native window reattach is pending");
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
