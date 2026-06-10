use anyhow::Result;
use odytty::app::run_interactive;
use odytty::core::Terminal;
use odytty::native::{NativeOptions, run_native};
use odytty::pty::PtySession;
use odytty::settings::Settings;

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--dump-command") {
        let command = args
            .get(1)
            .map(String::as_str)
            .unwrap_or("printf 'OdyTTY\\r\\n'");
        return dump_command(command);
    }

    if args.first().map(String::as_str) == Some("--interactive") {
        return run_interactive();
    }

    if args.first().map(String::as_str) == Some("--native") {
        // Opens a real native window and runs the event loop until the window is
        // closed, with runtime settings loaded once for the native session.
        let settings = Settings::from_env();
        run_native(NativeOptions::from_settings(&settings), settings)?;
        return Ok(());
    }

    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b[1;36mOdyTTY\x1b[0m core skeleton\r\n");
    terminal.advance(b"owned grid + vte parser are online\r\n");

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
