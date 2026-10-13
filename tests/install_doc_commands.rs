// SPDX-License-Identifier: GPL-3.0-only
//! The shell commands in `docs/install.md` must work on a fresh profile.

/// Every fenced block that links a launcher into `$HOME/.local/bin` creates
/// that directory first: `ln` does not, and `install -D` only creates the
/// parents of the file it installs. A block that did not would fail on an
/// account with no `~/.local/bin`.
#[test]
fn launcher_links_into_local_bin_create_the_directory_first() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/install.md"))
        .expect("read docs/install.md");
    let mut checked = 0;
    for block in text.split("```").skip(1).step_by(2) {
        let lines: Vec<&str> = block.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let continued = line.trim_end().ends_with('\\');
            let command = if continued {
                format!("{} {}", line, lines.get(index + 1).copied().unwrap_or(""))
            } else {
                (*line).to_owned()
            };
            if !command.trim_start().starts_with("ln -s") || !command.contains(".local/bin/") {
                continue;
            }
            checked += 1;
            let created = lines[..index].iter().any(|earlier| {
                (earlier.contains("install -d") || earlier.contains("mkdir -p"))
                    && earlier.contains("$HOME/.local/bin\"")
            });
            assert!(
                created,
                "this block links into ~/.local/bin without creating it first:\n{block}"
            );
        }
    }
    assert!(checked >= 2, "expected the two documented launcher links");
}
