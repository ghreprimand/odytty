// SPDX-License-Identifier: GPL-3.0-only
//! The security spine of the "Open With…" feature (C3b): turning a `.desktop`
//! `Exec=` string into an argv vector, **never** a shell command.
//!
//! A `.desktop` `Exec` value is NOT a shell command line. It is a token list
//! with Desktop-Entry quoting rules (a deliberately small subset of shell
//! quoting — no `$VAR`, no globbing, no command substitution, no `~`) plus a
//! fixed set of `%`-field codes. [`exec_to_argv`] tokenizes per those rules and
//! substitutes the resolved path as a single inert argv element, exactly the way
//! `editor_argv` (the C3 editor matrix) splits its template before substituting
//! so a path with spaces stays one element. The product flows into the shared
//! `spawn_detached` (argv-only, null stdio) — so a path containing `;`, `$()`,
//! backticks, spaces, or newlines is inert: it is one element handed to
//! `Command::args`, never interpolated into a shell string.
//!
//! Pure and std-only — tested directly by asserting the vector, never spawning.

/// The `file://<abs>` URI used for the `%u`/`%U` field codes. Mirrors
/// `crate::native::app::interactive_paths::file_uri`; duplicated here (one line)
/// rather than imported so this library-side module never reaches into `native/`
/// (the SPEC layering rule: `src/desktop/` imports no windowing/GPU).
fn file_uri(abs: &str) -> String {
    format!("file://{abs}")
}

/// Expand a Desktop-Entry `Exec=` string into an argv vector for opening `abs`.
///
/// Tokenization (Desktop-Entry quoting, NOT shell):
/// * tokens are whitespace-separated outside double quotes;
/// * a double-quoted span groups its contents into the current token, and the
///   reserved escapes `\"`, `` \` ``, `\$`, `\\` unescape to the bare character;
/// * no other interpretation happens — `$VAR`, `~`, `*`, `` ` ``, `;`, `|`, `&`
///   are all literal text.
///
/// Field-code substitution (per token, after tokenizing):
/// * `%f` / `%F` → the bare absolute path (we only ever open one file);
/// * `%u` / `%U` → the `file://` URI of that path;
/// * `%i` `%c` `%k` and the deprecated `%d %D %n %N %v %m` → stripped;
/// * `%%` → a literal `%`;
/// * any other `%x` → stripped (unknown/undefined field code);
/// * a field code that is a *substring* of a token (`--file=%f`) substitutes
///   in place and the token stays ONE argv element;
/// * a token that expands to nothing (a standalone stripped code like `%i`) is
///   dropped from the argv;
/// * if no `%f/%F/%u/%U` appears anywhere, the bare path is appended as a
///   trailing element (matches `xdg-open`/`gio` behaviour for simple entries).
pub fn exec_to_argv(exec: &str, abs: &str) -> Vec<String> {
    let uri = file_uri(abs);
    let mut argv: Vec<String> = Vec::new();
    let mut saw_path = false;

    for token in tokenize(exec) {
        let mut out = String::new();
        let mut chars = token.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => out.push('%'),
                Some('f' | 'F') => {
                    out.push_str(abs);
                    saw_path = true;
                }
                Some('u' | 'U') => {
                    out.push_str(&uri);
                    saw_path = true;
                }
                // Stripped codes (icon / translated-name / desktop-file path and
                // the deprecated set): contribute nothing.
                Some('i' | 'c' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => {}
                // Unknown/undefined field code: strip it (drop the code char).
                Some(_) => {}
                // A trailing bare `%`: drop it.
                None => {}
            }
        }
        // A token that was purely a stripped field code (`%i`) expands to the
        // empty string; drop it rather than passing an empty argument. A
        // genuinely empty quoted token (`""`) is preserved.
        if out.is_empty() && !token.is_empty() {
            continue;
        }
        argv.push(out);
    }

    if !saw_path {
        argv.push(abs.to_owned());
    }
    argv
}

/// Tokenize a Desktop-Entry `Exec` string into raw tokens, honoring double-quote
/// grouping and the four reserved in-quote escapes. Pure; no field-code work.
fn tokenize(exec: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if in_token {
                    tokens.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            '"' => {
                // A quoted span is always part of the current token (even an
                // empty `""` produces an empty token if it stands alone).
                in_token = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            // Inside double quotes only `" \ $ \`` are escapable;
                            // a backslash before anything else is literal.
                            match chars.peek() {
                                Some('"' | '\\' | '$' | '`') => {
                                    cur.push(chars.next().unwrap());
                                }
                                _ => cur.push('\\'),
                            }
                        }
                        other => cur.push(other),
                    }
                }
            }
            other => {
                in_token = true;
                cur.push(other);
            }
        }
    }
    if in_token {
        tokens.push(cur);
    }
    tokens
}

#[cfg(test)]
mod tests {
    //! Pure field-code + quoting tests. NONE spawns a process; every case
    //! asserts the built argv vector. Synthetic paths only — no real filesystem,
    //! no real home paths.
    use super::*;

    #[test]
    fn no_field_code_appends_path() {
        // A simple entry with no field code gets the path appended.
        assert_eq!(
            exec_to_argv("feh", "/img/a.png"),
            vec!["feh".to_owned(), "/img/a.png".to_owned()]
        );
    }

    #[test]
    fn percent_f_is_bare_path() {
        assert_eq!(
            exec_to_argv("eog %f", "/img/a.png"),
            vec!["eog".to_owned(), "/img/a.png".to_owned()]
        );
        // Uppercase %F behaves the same for our single-file open.
        assert_eq!(
            exec_to_argv("eog %F", "/img/a.png"),
            vec!["eog".to_owned(), "/img/a.png".to_owned()]
        );
    }

    #[test]
    fn percent_u_is_file_uri() {
        assert_eq!(
            exec_to_argv("firefox %u", "/docs/a.html"),
            vec!["firefox".to_owned(), "file:///docs/a.html".to_owned()]
        );
        assert_eq!(
            exec_to_argv("firefox %U", "/docs/a.html"),
            vec!["firefox".to_owned(), "file:///docs/a.html".to_owned()]
        );
    }

    #[test]
    fn icon_name_and_deprecated_codes_are_stripped() {
        // %i %c %k and the deprecated set drop out entirely; the path still
        // appends because no file/url code was present.
        assert_eq!(
            exec_to_argv("app %i %c %k %d %D %n %N %v %m", "/x/y.png"),
            vec!["app".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn icon_strip_keeps_the_file_code() {
        assert_eq!(
            exec_to_argv("app %i %f", "/x/y.png"),
            vec!["app".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn double_percent_is_literal() {
        assert_eq!(
            exec_to_argv("app 100%% %f", "/x/y.png"),
            vec!["app".to_owned(), "100%".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn substring_field_code_stays_one_element() {
        // `--file=%f` substitutes in place and remains a single argv element.
        assert_eq!(
            exec_to_argv("app --file=%f --quiet", "/x/y.png"),
            vec![
                "app".to_owned(),
                "--file=/x/y.png".to_owned(),
                "--quiet".to_owned()
            ]
        );
    }

    #[test]
    fn unknown_field_code_is_stripped() {
        // `%z` is undefined → stripped, path appended.
        assert_eq!(
            exec_to_argv("app %z", "/x/y.png"),
            vec!["app".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn quoted_program_with_space_is_one_element() {
        assert_eq!(
            exec_to_argv("\"/opt/My App/run\" %f", "/x/y.png"),
            vec!["/opt/My App/run".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn in_quote_escapes_unescape() {
        // \" \\ \$ \` unescape to the bare character inside double quotes.
        assert_eq!(
            exec_to_argv("app \"a\\\"b\" \"c\\$d\" \"e\\`f\" \"g\\\\h\"", "/x/y"),
            vec![
                "app".to_owned(),
                "a\"b".to_owned(),
                "c$d".to_owned(),
                "e`f".to_owned(),
                "g\\h".to_owned(),
                "/x/y".to_owned(),
            ]
        );
    }

    #[test]
    fn path_with_spaces_is_one_inert_element() {
        // The injected path is never re-tokenized, so spaces do not split it.
        let argv = exec_to_argv("eog %f", "/my pictures/holiday photo.png");
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[1], "/my pictures/holiday photo.png");
    }

    #[test]
    fn hostile_path_metacharacters_stay_inert() {
        // A path full of shell metacharacters is a single, inert argv element —
        // the whole security guarantee. Nothing is interpolated or executed.
        let nasty = "/tmp/$(touch pwned);`id`&& rm -rf ~|evil.png";
        let argv = exec_to_argv("eog %f", nasty);
        assert_eq!(argv, vec!["eog".to_owned(), nasty.to_owned()]);
        // And via the trailing-append path (no field code) it is equally inert.
        let argv2 = exec_to_argv("feh", nasty);
        assert_eq!(argv2, vec!["feh".to_owned(), nasty.to_owned()]);
    }

    #[test]
    fn standalone_strip_code_drops_token_not_the_program() {
        // `%i` standalone disappears; argv[0] and the appended path survive.
        let argv = exec_to_argv("gimp %i", "/x/y.png");
        assert_eq!(argv, vec!["gimp".to_owned(), "/x/y.png".to_owned()]);
    }

    #[test]
    fn dollar_and_tilde_outside_quotes_are_literal() {
        // No shell expansion: `$HOME` and `~` are literal argument text.
        assert_eq!(
            exec_to_argv("app $HOME ~ %f", "/x/y.png"),
            vec![
                "app".to_owned(),
                "$HOME".to_owned(),
                "~".to_owned(),
                "/x/y.png".to_owned()
            ]
        );
    }

    #[test]
    fn leading_and_trailing_whitespace_tolerated() {
        assert_eq!(
            exec_to_argv("   eog    %f   ", "/x/y.png"),
            vec!["eog".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn empty_exec_yields_just_the_path() {
        // Defensive: an empty Exec (filtered out before this in production) does
        // not panic; it degrades to a lone path element.
        assert_eq!(exec_to_argv("", "/x/y.png"), vec!["/x/y.png".to_owned()]);
    }
}
