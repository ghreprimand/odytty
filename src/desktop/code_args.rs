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
//! * shells: options are read with each shell's own arity, so an option
//!   operand (`bash -o posix`, `fish -d all`) cannot hide a later code option.
//!   For `sh bash dash zsh ksh mksh ash yash posh rbash csh tcsh`, after a
//!   flag cluster containing `c` the first operand is code; `-o` takes the
//!   next element, as do `-O` for `bash`, `-R` for `ksh`, `-T` for `mksh`,
//!   `--rcfile` and `--init-file` for `bash`, and `--rcfile` and `--profile`
//!   for `yash`. For `fish`, the argument of `-c`, `-C`, `--command` or
//!   `--init-command` is code. Any shell option outside these tables, an
//!   operand letter that is not last in its cluster, and every option of
//!   `elvish nu xonsh` leave the arity unknown, so a file value anywhere
//!   after such an option is refused. A file value inside the shell's own
//!   options, or as an option's operand, is refused too. `sh -c CODE sh %f`
//!   stays accepted: the file is a positional parameter there, not code;
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
            Some(Kind::Shell(shell)) => shell_code_reached(argv, rest, &carries, shell),
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
    Shell(Shell),
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
        "bash" | "rbash" => Kind::Shell(Shell::Posix {
            operand_letters: &['o', 'O'],
            long_operands: &["rcfile", "init-file"],
        }),
        "ksh" => Kind::Shell(Shell::Posix {
            operand_letters: &['o', 'R'],
            long_operands: &[],
        }),
        "mksh" => Kind::Shell(Shell::Posix {
            operand_letters: &['o', 'T'],
            long_operands: &[],
        }),
        "yash" => Kind::Shell(Shell::Posix {
            operand_letters: &['o'],
            long_operands: &["rcfile", "profile"],
        }),
        "sh" | "dash" | "zsh" | "ash" | "posh" | "csh" | "tcsh" => Kind::Shell(Shell::Posix {
            operand_letters: &['o'],
            long_operands: &[],
        }),
        "fish" => Kind::Shell(Shell::Fish),
        "elvish" | "nu" | "xonsh" => Kind::Shell(Shell::Unparsed),
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

/// How a recognized shell reads its options.
#[derive(Clone, Copy)]
enum Shell {
    /// A POSIX-style shell: a cluster containing `c` makes the first operand
    /// code, and the listed letters and long options take the next element.
    Posix {
        operand_letters: &'static [char],
        long_operands: &'static [&'static str],
    },
    /// `fish`, read like its getopt table: `-c` and `-C` take code.
    Fish,
    /// A shell whose option arity is not modelled: any option is ambiguous.
    Unparsed,
}

/// Letters that take the next element in at least one POSIX-style shell. In a
/// shell that does not list the letter its arity is unknown.
const POSIX_OPERAND_LETTERS: &[char] = &['o', 'O', 'R', 'T'];

/// Long options every POSIX-style shell here reads without an operand.
const POSIX_LONG_FLAGS: &[&str] = &[
    "norc",
    "noprofile",
    "login",
    "posix",
    "restricted",
    "verbose",
    "noediting",
    "debugger",
    "version",
    "help",
];

/// `fish` short options with an argument; the argument of `c` and `C` is code.
const FISH_OPERAND_LETTERS: &[char] = &['c', 'C', 'p', 'd', 'f', 'D', 'o'];
const FISH_FLAG_LETTERS: &[char] = &['h', 'P', 'i', 'l', 'N', 'n', 'v'];
const FISH_LONG_OPERANDS: &[&str] = &[
    "profile",
    "profile-startup",
    "debug",
    "debug-output",
    "debug-stack-frames",
    "features",
];
const FISH_LONG_FLAGS: &[&str] = &[
    "help",
    "interactive",
    "login",
    "no-execute",
    "no-config",
    "private",
    "print-rusage-self",
    "print-debug-categories",
    "version",
];

/// How one shell option element is read.
enum ShellOption {
    /// The option stands alone; `command` when it makes the first operand code.
    Flag { command: bool },
    /// The option also consumes the next element as its operand.
    TakesNext { command: bool },
    /// The option's argument is code: attached, or the next element.
    Code { attached: bool },
    /// The option's arity is unknown.
    Ambiguous,
}

fn shell_option(shell: Shell, element: &str) -> ShellOption {
    let long = element.strip_prefix("--").map(|o| {
        o.split_once('=')
            .map_or((o, false), |(name, _)| (name, true))
    });
    match shell {
        Shell::Posix {
            operand_letters,
            long_operands,
        } => match long {
            Some((_, true)) => ShellOption::Flag { command: false },
            Some((name, false)) if long_operands.contains(&name) => {
                ShellOption::TakesNext { command: false }
            }
            Some((name, false)) if POSIX_LONG_FLAGS.contains(&name) => {
                ShellOption::Flag { command: false }
            }
            Some(_) => ShellOption::Ambiguous,
            None => {
                let cluster = &element[1..];
                let command = cluster.contains('c');
                let mut letters = cluster.chars().peekable();
                while let Some(letter) = letters.next() {
                    if !POSIX_OPERAND_LETTERS.contains(&letter) {
                        continue;
                    }
                    if !operand_letters.contains(&letter) || letters.peek().is_some() {
                        return ShellOption::Ambiguous;
                    }
                    return ShellOption::TakesNext { command };
                }
                ShellOption::Flag { command }
            }
        },
        Shell::Fish => match long {
            Some(("command" | "init-command", attached)) => ShellOption::Code { attached },
            Some((_, true)) => ShellOption::Flag { command: false },
            Some((name, false)) if FISH_LONG_OPERANDS.contains(&name) => {
                ShellOption::TakesNext { command: false }
            }
            Some((name, false)) if FISH_LONG_FLAGS.contains(&name) => {
                ShellOption::Flag { command: false }
            }
            Some(_) => ShellOption::Ambiguous,
            None => {
                let cluster = &element[1..];
                for (at, letter) in cluster.char_indices() {
                    if FISH_FLAG_LETTERS.contains(&letter) {
                        continue;
                    }
                    if !FISH_OPERAND_LETTERS.contains(&letter) {
                        return ShellOption::Ambiguous;
                    }
                    let attached = at + letter.len_utf8() < cluster.len();
                    return match (letter, attached) {
                        ('c' | 'C', attached) => ShellOption::Code { attached },
                        (_, true) => ShellOption::Flag { command: false },
                        (_, false) => ShellOption::TakesNext { command: false },
                    };
                }
                ShellOption::Flag { command: false }
            }
        },
        Shell::Unparsed => ShellOption::Ambiguous,
    }
}

/// `sh -c CODE`, `fish -c CODE`: options are read with the shell's own arity,
/// so an option operand never ends the scan early. A file value inside the
/// options or as an option operand is refused, and so is one anywhere after an
/// option whose arity is unknown.
fn shell_code_reached(
    argv: &[String],
    rest: std::ops::Range<usize>,
    carries: &impl Fn(usize) -> bool,
    shell: Shell,
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
        match shell_option(shell, element) {
            ShellOption::Flag { command: c } => {
                command |= c;
                j += 1;
            }
            ShellOption::Code { attached: true } => j += 1,
            ShellOption::TakesNext { command: c } => {
                command |= c;
                if j + 1 < rest.end && carries(j + 1) {
                    return true;
                }
                j += 2;
            }
            ShellOption::Code { attached: false } => {
                if j + 1 < rest.end && carries(j + 1) {
                    return true;
                }
                j += 2;
            }
            ShellOption::Ambiguous => return (j + 1..rest.end).any(carries),
        }
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
    fn shell_option_operands_do_not_hide_a_later_code_option() {
        for exec in [
            "bash -o posix -c %f",
            "bash +o history -c %f",
            "bash -O extglob -c %f",
            "bash -euo pipefail -c %f",
            "bash -c -o posix %f",
            "bash -co posix %f",
            "bash --rcfile rc -c %f",
            "bash --init-file rc -c %f",
            "zsh -o posix -c %f",
            "dash -o noglob -c %f",
            "ksh -R xref -c %f",
            "mksh -T tty -c %f",
            "yash --profile rc -c %f",
            "env bash -o posix -c %f",
            "fish -d all -c %f",
            "fish --debug all --command %f",
            "fish -p prof -C %f",
            "fish -ic %f",
            "fish -c true -c %f",
            "fish --init-command=true -c %f",
        ] {
            assert!(refused(exec), "{exec} must be refused");
        }
    }

    #[test]
    fn shell_options_of_unknown_arity_refuse_a_later_file_value() {
        for exec in [
            "bash --unknown x -c %f",
            "bash -oc posix %f",
            "sh -T tty -c %f",
            "fish --unknown x -c %f",
            "fish -X x -c %f",
            "nu --config cfg -c %f",
            "elvish -c %f",
            "xonsh -c %f",
            "bash -o %f",
            "bash --rcfile %f",
        ] {
            assert!(refused(exec), "{exec} must be refused");
        }
    }

    #[test]
    fn shell_options_with_operands_still_run_the_selected_file() {
        for exec in [
            "bash -o posix %f",
            "bash -euo pipefail %f",
            "bash -O extglob -x %f",
            "bash --norc %f",
            "bash -o posix -c true sh %f",
            "fish -d all %f",
            "fish -c true %f",
            "fish --command=true %f",
            "nu %f",
            "xonsh %f",
            "elvish %f",
        ] {
            let argv = exec_to_argv(exec, PATH).unwrap_or_else(|| panic!("{exec} offered"));
            assert_eq!(argv.last().map(String::as_str), Some(PATH), "{exec}");
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
