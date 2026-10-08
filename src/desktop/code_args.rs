// SPDX-License-Identifier: GPL-3.0-only
//! Recognized code arguments for the "Open With..." argv mapping.
//!
//! The selected file reaches a desktop entry's program as one argv element, so
//! no shell ever parses it. That is not enough when the element lands where a
//! program reads its argument as code: `sh -c %f` runs the file name itself as
//! a shell command, and a crafted name would then execute. [`exec_to_argv`]
//! refuses an entry when [`reaches_code_argument`] finds the file value in such
//! a position.
//!
//! The boundary is this list. An interpreter is recognized by the base name of
//! any argv element that does not itself carry the file value (trailing version
//! digits ignored, so `python3.12` is `python`), so wrappers such as `env`,
//! `nice` or `flatpak-spawn --host` do not hide it:
//!
//! * shells (`sh bash dash zsh ksh mksh ash yash posh rbash fish csh tcsh
//!   elvish nu xonsh`): after a flag cluster containing `c`, or `--command`
//!   or `--init-command`, the first operand is code. A file value inside the
//!   shell's own options is refused too. `sh -c CODE sh %f` stays accepted:
//!   the file is a positional parameter there, not code;
//! * option-code interpreters: `python pypy guile` `-c`; `perl` `-e -E`;
//!   `ruby lua luajit Rscript osascript` `-e`; `node nodejs bun` `-e -p
//!   --eval --print`; `julia` `-e -E --eval --print`; `php` `-r -B -R -E`;
//!   `su runuser` `-c --command`. The option's argument is code, attached
//!   (`-e%f`, `--eval=%f`) or as the next element;
//! * `pwsh` and `powershell`: after an option starting `-c` or `-e` (any case),
//!   every later element is code;
//! * `awk gawk mawk nawk sed gsed`: the argument of `-e`, `--expression` or
//!   `--source` is code, and without a program option the first operand is the
//!   program;
//! * `env`: the argument of `-S` or `--split-string` becomes a whole command;
//! * `ssh`: operands after the destination form a remote command, so any file
//!   value after `ssh` is refused.
//!
//! Running the selected file itself (`sh %f`, `python3 %f`) is not a code
//! argument: the file's content runs, which is what such an entry offers, and
//! its name is never parsed. Programs outside this list that read an argument
//! as code are not recognized.
//!
//! [`exec_to_argv`]: super::exec::exec_to_argv

/// Whether any element flagged in `carries_path` lands in a recognized code
/// argument of an interpreter named earlier in `argv`.
pub(super) fn reaches_code_argument(argv: &[String], carries_path: &[bool]) -> bool {
    let carries = |j: usize| carries_path.get(j).copied().unwrap_or(false);
    argv.iter().enumerate().any(|(i, element)| {
        if carries(i) {
            return false;
        }
        let rest = i + 1..argv.len();
        match interpreter(element) {
            Some(Kind::Shell) => shell_code_reached(argv, rest, &carries),
            Some(Kind::OptionCode { short, long }) => {
                option_code_reached(argv, rest, &carries, short, long)
            }
            Some(Kind::PowerShell) => powershell_code_reached(argv, rest, &carries),
            Some(Kind::Program { takes_argument }) => {
                program_code_reached(argv, rest, &carries, takes_argument)
            }
            Some(Kind::SplitString) => {
                option_code_reached(argv, rest, &carries, &['S'], &["split-string"])
            }
            Some(Kind::RemoteCommand) => (i + 1..argv.len()).any(carries),
            None => false,
        }
    })
}

enum Kind {
    Shell,
    OptionCode {
        short: &'static [char],
        long: &'static [&'static str],
    },
    PowerShell,
    /// Short options that take the next element as their argument.
    Program {
        takes_argument: &'static [char],
    },
    SplitString,
    RemoteCommand,
}

fn interpreter(element: &str) -> Option<Kind> {
    let base = element.rsplit('/').next().unwrap_or(element);
    let name = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match name {
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "mksh" | "ash" | "yash" | "posh" | "rbash"
        | "fish" | "csh" | "tcsh" | "elvish" | "nu" | "xonsh" => Kind::Shell,
        "python" | "pypy" | "guile" => Kind::OptionCode {
            short: &['c'],
            long: &[],
        },
        "perl" => Kind::OptionCode {
            short: &['e', 'E'],
            long: &[],
        },
        "ruby" | "lua" | "luajit" | "Rscript" | "osascript" => Kind::OptionCode {
            short: &['e'],
            long: &[],
        },
        "node" | "nodejs" | "bun" => Kind::OptionCode {
            short: &['e', 'p'],
            long: &["eval", "print"],
        },
        "julia" => Kind::OptionCode {
            short: &['e', 'E'],
            long: &["eval", "print"],
        },
        "php" => Kind::OptionCode {
            short: &['r', 'B', 'R', 'E'],
            long: &[],
        },
        "su" | "runuser" => Kind::OptionCode {
            short: &['c'],
            long: &["command"],
        },
        "pwsh" | "powershell" => Kind::PowerShell,
        "awk" | "gawk" | "mawk" | "nawk" => Kind::Program {
            takes_argument: &['e', 'f', 'v', 'F', 'E', 'i', 'l'],
        },
        "sed" | "gsed" => Kind::Program {
            takes_argument: &['e', 'f', 'l'],
        },
        "env" => Kind::SplitString,
        "ssh" => Kind::RemoteCommand,
        _ => return None,
    })
}

fn is_option(element: &str) -> bool {
    element.len() > 1 && (element.starts_with('-') || element.starts_with('+'))
}

/// `sh -c CODE`: a flag cluster containing `c` (or `--command`,
/// `--init-command`) makes the first operand code.
fn shell_code_reached(
    argv: &[String],
    rest: std::ops::Range<usize>,
    carries: &impl Fn(usize) -> bool,
) -> bool {
    let mut command = false;
    let mut operand = None;
    let mut j = rest.start;
    while j < rest.end {
        let element = argv[j].as_str();
        if element == "--" {
            operand = Some(j + 1);
            break;
        }
        if !is_option(element) {
            operand = Some(j);
            break;
        }
        if carries(j) {
            return true;
        }
        let long = element
            .strip_prefix("--")
            .map(|o| o.split('=').next().unwrap_or(o));
        command |= match long {
            Some(name) => matches!(name, "command" | "init-command"),
            None => element[1..].contains('c') || element == "-C",
        };
        j += 1;
    }
    command && operand.is_some_and(carries)
}

/// `perl -e CODE`, `-eCODE`, `--eval=CODE`, `--eval CODE`.
fn option_code_reached(
    argv: &[String],
    rest: std::ops::Range<usize>,
    carries: &impl Fn(usize) -> bool,
    short: &[char],
    long: &[&str],
) -> bool {
    rest.clone().any(|j| {
        let element = argv[j].as_str();
        if let Some(option) = element.strip_prefix("--") {
            let (name, attached) = match option.split_once('=') {
                Some((name, _)) => (name, true),
                None => (option, false),
            };
            if !long.contains(&name) {
                return false;
            }
            return argument_carries(j, attached, rest.end, carries);
        }
        let Some(cluster) = element.strip_prefix('-') else {
            return false;
        };
        let Some(at) = cluster.find(|c| short.contains(&c)) else {
            return false;
        };
        argument_carries(j, at + 1 < cluster.len(), rest.end, carries)
    })
}

/// Whether the argument of the option at `j` carries the file value: the
/// option element itself when the argument is attached, else the next one.
fn argument_carries(
    j: usize,
    attached: bool,
    end: usize,
    carries: &impl Fn(usize) -> bool,
) -> bool {
    if attached {
        carries(j)
    } else {
        j + 1 < end && carries(j + 1)
    }
}

/// `pwsh -c CODE...`: after `-Command`, `-EncodedCommand` or any option a
/// prefix of them could abbreviate, the remaining elements are code.
fn powershell_code_reached(
    argv: &[String],
    rest: std::ops::Range<usize>,
    carries: &impl Fn(usize) -> bool,
) -> bool {
    let mut j = rest.start;
    while j < rest.end {
        let lower = argv[j].to_ascii_lowercase();
        let option = lower.strip_prefix('-').or_else(|| lower.strip_prefix('/'));
        if option.is_some_and(|o| o.starts_with('c') || o.starts_with('e')) {
            return (j..rest.end).any(carries);
        }
        j += 1;
    }
    false
}

/// `awk PROGRAM` and `sed SCRIPT`: `-e`, `--expression` and `--source` take
/// code; without them or a program file (`-f`, `--file`) the first operand is
/// the program.
fn program_code_reached(
    argv: &[String],
    rest: std::ops::Range<usize>,
    carries: &impl Fn(usize) -> bool,
    takes_argument: &[char],
) -> bool {
    let mut has_program = false;
    let mut j = rest.start;
    while j < rest.end {
        let element = argv[j].as_str();
        if element == "--" {
            j += 1;
            break;
        }
        if !is_option(element) {
            break;
        }
        if let Some(option) = element.strip_prefix("--") {
            let (name, attached) = match option.split_once('=') {
                Some((name, _)) => (name, true),
                None => (option, false),
            };
            let code = matches!(name, "expression" | "source");
            let program_file = matches!(name, "file" | "exec");
            if code && argument_carries(j, attached, rest.end, carries) {
                return true;
            }
            has_program |= code || program_file;
            let takes_argument =
                code || program_file || matches!(name, "assign" | "field-separator");
            j += if takes_argument && !attached { 2 } else { 1 };
            continue;
        }
        let cluster = &element[1..];
        let mut skip_next = false;
        for (at, letter) in cluster.char_indices() {
            if !takes_argument.contains(&letter) {
                continue;
            }
            let attached = at + letter.len_utf8() < cluster.len();
            if letter == 'e' && argument_carries(j, attached, rest.end, carries) {
                return true;
            }
            has_program |= matches!(letter, 'e' | 'f' | 'E');
            skip_next = !attached;
            break;
        }
        j += if skip_next { 2 } else { 1 };
    }
    !has_program && j < rest.end && carries(j)
}

#[cfg(test)]
mod tests {
    use super::super::exec::exec_to_argv;

    const PATH: &str = "/tmp/x;touch pwned";

    fn refused(exec: &str) -> bool {
        exec_to_argv(exec, PATH).is_none()
    }

    #[test]
    fn unquoted_shell_command_arguments_refuse_the_entry() {
        for exec in [
            "sh -c %f",
            "bash -c %f",
            "bash -lc %f",
            "/bin/bash -e -c %f",
            "dash -c -- %f",
            "zsh --norc -c %u",
            "fish --command %f",
            "fish -C %f",
            "env sh -c %f",
            "flatpak-spawn --host bash -c %f",
            "sh -c",
            "bash --rcfile=%f",
        ] {
            assert!(refused(exec), "{exec} must be refused");
        }
    }

    #[test]
    fn option_code_arguments_refuse_the_entry() {
        for exec in [
            "python3 -c %f",
            "python3.12 -Bc %f",
            "python -c%f",
            "perl -e %f",
            "perl -le %f",
            "perl -E%f",
            "ruby -e %f",
            "node --eval %f",
            "node --eval=%f",
            "nodejs -p %u",
            "php -r %f",
            "lua5.4 -e %f",
            "julia -e %f",
            "osascript -e %f",
            "su -c %f",
            "su root --command=%f",
            "pwsh -NoProfile -Command %f",
            "powershell -c Write-Output %f",
            "awk %f",
            "gawk -v x=1 %f",
            "sed -n %f",
            "sed -e %f",
            "sed -i %f",
            "gawk -i inc %f",
            "gawk --source=%f",
            "env -S %f",
            "env --split-string %f",
            "ssh host %f",
            "env python3 -c",
        ] {
            assert!(refused(exec), "{exec} must be refused");
        }
    }

    #[test]
    fn file_operands_and_ordinary_entries_keep_working() {
        for exec in [
            "sh %f",
            "python3 %f",
            "perl -w %f",
            "sh -c true sh %f",
            "bash -c true -- %f",
            "awk -f prog.awk %f",
            "sed -e s/a/b/ %f",
            "sed -i -e s/a/b/ %f",
            "gawk -i inc -f prog.awk %f",
            "gawk --file=prog.awk %f",
            "pwsh -File %f",
            "eog %U",
            "vlc --started-from-file %U",
            "code --new-window %F",
            "env FOO=1 eog %f",
            "gimp",
        ] {
            let argv = exec_to_argv(exec, PATH).unwrap_or_else(|| panic!("{exec} offered"));
            let last = argv.last().expect("argv has the file");
            assert!(
                last == PATH || last.starts_with("file://"),
                "{exec}: {argv:?}"
            );
        }
    }

    #[test]
    fn an_interpreter_name_carried_by_the_file_value_is_not_recognized() {
        // The file value is never treated as naming an interpreter; it can
        // only be the code, not select the rules.
        assert!(exec_to_argv("viewer %f -c x", "/usr/bin/sh").is_some());
    }
}
