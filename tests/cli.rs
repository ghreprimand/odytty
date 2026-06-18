// SPDX-License-Identifier: GPL-3.0-only
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
fn show_config_output_formats_default_settings() {
    let output = cli::show_config_output(&odytty::settings::Settings::default());

    assert_contains_line(&output, "theme=odyssey");
    assert_contains_line(&output, "visual=ambient");
    assert_contains_line(&output, "font_family=JetBrains Mono");
    assert_contains_line(&output, "font_size=22");
    assert_contains_line(&output, "render_quality=balanced");
    assert_contains_line(&output, "retro=off");
    assert_contains_line(&output, "window_padding=4");
    assert_contains_line(&output, "bloom=on");
    assert_contains_line(&output, "crt=on");
    assert_contains_line(&output, "keybinds=");
    assert_contains_line(&output, "synthetic_styles=on");

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
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
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
