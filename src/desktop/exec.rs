// SPDX-License-Identifier: GPL-3.0-only
//! The security spine of the "Open With..." feature (C3b): turning a `.desktop`
//! `Exec=` string into an argv vector, **never** a shell command.
//!
//! A `.desktop` `Exec` value is NOT a shell command line. It is a token list
//! with Desktop-Entry quoting rules (a deliberately small subset of shell
//! quoting: no `$VAR`, no globbing, no command substitution, no `~`) plus a
//! fixed set of `%`-field codes. [`exec_to_argv`] tokenizes per those rules,
//! keeps track of which tokens contain quoted text, and substitutes the
//! resolved path as a single argv element, the way `editor_argv` (the C3
//! editor matrix) splits its template before substituting so a path with
//! spaces stays one element. The product flows into the shared
//! `spawn_detached` (argv-only, null stdio), so the path is never interpolated
//! into a shell string by OdyTTY.
//!
//! One argv element is not enough on its own: an entry such as
//! `sh -c "eog %f"` or `sh -c %f` would hand the path to a shell as code.
//! An entry whose file value lands in a recognized interpreter's code argument
//! is refused; `code_args` lists the recognized forms. Field codes inside
//! a quoted argument are undefined by the Desktop Entry specification, and
//! `%F`/`%U` may only stand alone, so an entry using either form is refused
//! before any argv is built. The specification also makes an entry with an
//! unlisted field code, or with a literal `%` not written as `%%`, invalid,
//! and requires balanced quoting; those entries are refused too.
//!
//! Pure and std-only: tested directly by asserting the vector, never spawning.

/// The `file://<abs>` URI used for the `%u`/`%U` field codes. Routes through the
/// shared library-layer encoder (`crate::paths::file_uri`), so a `%u` path with
/// a space, `%`, control byte, or non-ASCII byte is percent-encoded rather than
/// emitted raw (a malformed URI otherwise). Desktop entries are Unix-only, so the
/// Unix convention applies. The encoder lives under `src/paths/` (std-only), so
/// this stays within the SPEC layering rule: `src/desktop/` never reaches into
/// `native/`.
fn file_uri(abs: &str) -> String {
    crate::paths::file_uri::file_uri(abs, crate::paths::file_uri::UriOs::Unix)
}

/// Expand a Desktop-Entry `Exec=` string into an argv vector for opening `abs`,
/// or `None` when the entry uses a field code in a context this mapper refuses.
///
/// Tokenization (Desktop-Entry quoting, NOT shell):
/// * tokens are whitespace-separated outside double quotes;
/// * a double-quoted span groups its contents into the current token, and the
///   reserved escapes `\"`, `` \` ``, `\$`, `\\` unescape to the bare character;
/// * no other interpretation happens: `$VAR`, `~`, `*`, `` ` ``, `;`, `|`, `&`
///   are all literal text.
///
/// Refused entries (`None`):
/// * an unterminated double-quoted span;
/// * an unlisted field code (`%z`) or a bare `%` at the end of a token;
/// * any field code other than `%%` in a token that contains quoted text,
///   such as `sh -c "eog %f"` or `"eog "%f`;
/// * `%F` or `%U` inside a longer token, such as `--files=%F`;
/// * the file value landing in a recognized interpreter's code argument,
///   such as `sh -c %f`, `python3 -c %f`, `perl -e%f`, or an entry without
///   a field code whose appended path would land there (`sh -c`);
/// * no program token, or a program token that is empty or carries any field
///   code other than `%%` (`Exec=%f`, `Exec=%i %f`, `Exec=viewer%f`): the
///   selected file must never become, or replace, the program.
///
/// Field-code substitution (per token, after tokenizing):
/// * `%f` / `%F` -> the bare absolute path (we only ever open one file);
/// * `%u` / `%U` -> the `file://` URI of that path;
/// * `%i` `%c` `%k` and the deprecated `%d %D %n %N %v %m` -> stripped;
/// * `%%` -> a literal `%`, also inside quotes;
/// * `%f` or `%u` inside a longer unquoted token (`--file=%f`) substitutes in
///   place and the token stays ONE argv element;
/// * a token that expands to nothing (a standalone stripped code like `%i`) is
///   dropped from the argv;
/// * if no `%f/%F/%u/%U` appears anywhere, the bare path is appended as a
///   trailing element (matches `xdg-open`/`gio` behaviour for simple entries).
pub fn exec_to_argv(exec: &str, abs: &str) -> Option<Vec<String>> {
    let tokens = tokenize(exec)?;
    if tokens.iter().any(refuses_field_codes) {
        return None;
    }
    // The program is the first token, admitted before any expansion: it must
    // be nonempty and carry no field code other than `%%`.
    let program = program_text(tokens.first()?)?;
    let uri = file_uri(abs);
    let mut argv: Vec<String> = Vec::new();
    // Which argv elements carry the selected file's value.
    let mut carries_path: Vec<bool> = Vec::new();
    let mut saw_path = false;

    for token in &tokens {
        let mut out = String::new();
        let mut carries = false;
        let mut chars = token.text.chars();
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
                    carries = true;
                }
                Some('u' | 'U') => {
                    out.push_str(&uri);
                    saw_path = true;
                    carries = true;
                }
                // Stripped codes (icon / translated-name / desktop-file path and
                // the deprecated set): contribute nothing.
                // Unlisted codes and a trailing bare `%` were refused above.
                Some(_) | None => {}
            }
        }
        // A token that was purely a stripped field code (`%i`) expands to the
        // empty string; drop it rather than passing an empty argument. A
        // genuinely empty quoted token (`""`) is preserved.
        if out.is_empty() && !token.text.is_empty() {
            continue;
        }
        argv.push(out);
        carries_path.push(carries);
    }

    if !saw_path {
        argv.push(abs.to_owned());
        carries_path.push(true);
    }
    // Refuse any mapping that lost or changed the program token.
    if argv.first() != Some(&program) {
        return None;
    }
    // Refuse a mapping that hands the file value to a recognized interpreter
    // as code rather than as a file operand.
    if super::code_args::reaches_code_argument(&argv, &carries_path) {
        return None;
    }
    Some(argv)
}

/// The program token's literal text (`%%` unescaped), or `None` when it is
/// empty or holds any other field code.
fn program_text(token: &Token) -> Option<String> {
    let mut out = String::new();
    let mut chars = token.text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.next() != Some('%') {
            return None;
        }
        out.push('%');
    }
    (!out.is_empty()).then_some(out)
}

/// One `Exec` token and whether any of its text came from a quoted span.
struct Token {
    text: String,
    quoted: bool,
}

/// Whether a token uses a field code the mapper refuses: an unlisted code, a
/// trailing bare `%`, any code other than `%%` in a token containing quoted
/// text, or `%F`/`%U` that is not the whole token.
fn refuses_field_codes(token: &Token) -> bool {
    let mut chars = token.text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        match chars.next() {
            Some('%') => {}
            None => return true,
            Some(code) => {
                if !matches!(
                    code,
                    'f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm'
                ) {
                    return true;
                }
                if token.quoted {
                    return true;
                }
                if matches!(code, 'F' | 'U') && token.text.chars().count() != 2 {
                    return true;
                }
            }
        }
    }
    false
}

/// Tokenize a Desktop-Entry `Exec` string into raw tokens, honoring double-quote
/// grouping and the four reserved in-quote escapes, and recording which tokens
/// contain quoted text. `None` when a double-quoted span is never closed. Pure;
/// no field-code work.
fn tokenize(exec: &str) -> Option<Vec<Token>> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if in_token {
                    tokens.push(Token {
                        text: std::mem::take(&mut cur),
                        quoted,
                    });
                    in_token = false;
                    quoted = false;
                }
            }
            '"' => {
                // A quoted span is always part of the current token (even an
                // empty `""` produces an empty token if it stands alone).
                in_token = true;
                quoted = true;
                let mut closed = false;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => {
                            closed = true;
                            break;
                        }
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
                if !closed {
                    return None;
                }
            }
            other => {
                in_token = true;
                cur.push(other);
            }
        }
    }
    if in_token {
        tokens.push(Token { text: cur, quoted });
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    //! Pure field-code + quoting tests. NONE spawns a process; every case
    //! asserts the built argv vector. Synthetic paths only - no real filesystem,
    //! no real home paths.
    use super::*;

    /// Expansion of an admitted entry.
    fn argv_for(exec: &str, abs: &str) -> Vec<String> {
        exec_to_argv(exec, abs).expect("entry is admitted")
    }

    #[test]
    fn no_field_code_appends_path() {
        // A simple entry with no field code gets the path appended.
        assert_eq!(
            argv_for("feh", "/img/a.png"),
            vec!["feh".to_owned(), "/img/a.png".to_owned()]
        );
    }

    #[test]
    fn percent_f_is_bare_path() {
        assert_eq!(
            argv_for("eog %f", "/img/a.png"),
            vec!["eog".to_owned(), "/img/a.png".to_owned()]
        );
        // Uppercase %F behaves the same for our single-file open.
        assert_eq!(
            argv_for("eog %F", "/img/a.png"),
            vec!["eog".to_owned(), "/img/a.png".to_owned()]
        );
    }

    #[test]
    fn percent_u_is_file_uri() {
        assert_eq!(
            argv_for("firefox %u", "/docs/a.html"),
            vec!["firefox".to_owned(), "file:///docs/a.html".to_owned()]
        );
        assert_eq!(
            argv_for("firefox %U", "/docs/a.html"),
            vec!["firefox".to_owned(), "file:///docs/a.html".to_owned()]
        );
    }

    #[test]
    fn icon_name_and_deprecated_codes_are_stripped() {
        // %i %c %k and the deprecated set drop out entirely; the path still
        // appends because no file/url code was present.
        assert_eq!(
            argv_for("app %i %c %k %d %D %n %N %v %m", "/x/y.png"),
            vec!["app".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn icon_strip_keeps_the_file_code() {
        assert_eq!(
            argv_for("app %i %f", "/x/y.png"),
            vec!["app".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn double_percent_is_literal() {
        assert_eq!(
            argv_for("app 100%% %f", "/x/y.png"),
            vec!["app".to_owned(), "100%".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn substring_field_code_stays_one_element() {
        // `--file=%f` substitutes in place and remains a single argv element.
        assert_eq!(
            argv_for("app --file=%f --quiet", "/x/y.png"),
            vec![
                "app".to_owned(),
                "--file=/x/y.png".to_owned(),
                "--quiet".to_owned()
            ]
        );
    }

    #[test]
    fn unlisted_field_codes_and_a_bare_percent_refuse_the_entry() {
        // The specification makes an entry with an unlisted field code, or a
        // literal `%` not written as `%%`, invalid.
        for exec in ["app %z", "app %z %f", "app %", "app --x=50% %f"] {
            assert_eq!(exec_to_argv(exec, "/x/y.png"), None, "{exec}");
        }
    }

    #[test]
    fn unterminated_quotes_refuse_the_entry() {
        assert_eq!(exec_to_argv("app \"unfinished", "/x/y.png"), None);
        assert_eq!(exec_to_argv("app \"a\\\" %f", "/x/y.png"), None);
    }

    #[test]
    fn quoted_program_with_space_is_one_element() {
        assert_eq!(
            argv_for("\"/opt/My App/run\" %f", "/x/y.png"),
            vec!["/opt/My App/run".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn in_quote_escapes_unescape() {
        // \" \\ \$ \` unescape to the bare character inside double quotes.
        assert_eq!(
            argv_for("app \"a\\\"b\" \"c\\$d\" \"e\\`f\" \"g\\\\h\"", "/x/y"),
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
        let argv = argv_for("eog %f", "/my pictures/holiday photo.png");
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[1], "/my pictures/holiday photo.png");
    }

    #[test]
    fn hostile_path_metacharacters_stay_inert() {
        // A path full of shell metacharacters is a single, inert argv element -
        // the whole security guarantee. Nothing is interpolated or executed.
        let nasty = "/tmp/$(touch pwned);`id`&& rm -rf ~|evil.png";
        let argv = argv_for("eog %f", nasty);
        assert_eq!(argv, vec!["eog".to_owned(), nasty.to_owned()]);
        // And via the trailing-append path (no field code) it is equally inert.
        let argv2 = argv_for("feh", nasty);
        assert_eq!(argv2, vec!["feh".to_owned(), nasty.to_owned()]);
    }

    #[test]
    fn standalone_strip_code_drops_token_not_the_program() {
        // `%i` standalone disappears; argv[0] and the appended path survive.
        let argv = argv_for("gimp %i", "/x/y.png");
        assert_eq!(argv, vec!["gimp".to_owned(), "/x/y.png".to_owned()]);
    }

    #[test]
    fn dollar_and_tilde_outside_quotes_are_literal() {
        // No shell expansion: `$HOME` and `~` are literal argument text.
        assert_eq!(
            argv_for("app $HOME ~ %f", "/x/y.png"),
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
            argv_for("   eog    %f   ", "/x/y.png"),
            vec!["eog".to_owned(), "/x/y.png".to_owned()]
        );
    }

    #[test]
    fn empty_exec_is_refused() {
        // An Exec with no program token (the parser already withholds an empty
        // Exec from enumeration) is refused rather than degrading to a lone
        // path element, which would make the selected file the program.
        assert_eq!(expand("", "/x/y.png"), None);
        assert_eq!(expand("   ", "/x/y.png"), None);
    }

    #[test]
    fn field_codes_in_the_program_position_refuse_the_entry() {
        for exec in [
            "%f",
            "%F",
            "%u",
            "%U",
            "%i %f",
            "%c %f",
            "%k",
            "%i",
            "viewer%f",
            "%d viewer %f",
            "\"\" %f",
        ] {
            assert_eq!(expand(exec, "/x/y.png"), None, "{exec}");
        }
    }

    #[test]
    fn a_literal_percent_in_the_program_is_kept() {
        assert_eq!(
            expand("view%%er %f", "/x/y.png"),
            Some(vec!["view%er".to_owned(), "/x/y.png".to_owned()])
        );
    }

    #[test]
    fn the_program_survives_as_argv0_and_the_file_never_takes_its_place() {
        for exec in ["eog", "eog %f", "eog %i %f", "gimp %U", "\"my viewer\" %u"] {
            let argv = expand(exec, "/x/y.png").expect(exec);
            assert!(argv.len() >= 2, "{exec}");
            assert_ne!(argv[0], "/x/y.png", "{exec}");
            assert_ne!(argv[0], "file:///x/y.png", "{exec}");
        }
    }

    /// Expansion result, `None` when the entry is refused.
    fn expand(exec: &str, abs: &str) -> Option<Vec<String>> {
        exec_to_argv(exec, abs)
    }

    #[test]
    fn field_codes_inside_quotes_refuse_the_entry() {
        // Quoted field-code expansion is undefined in the Desktop Entry
        // specification. A shell code string is the dangerous case: the path
        // would become shell code, not an inert argument.
        for exec in [
            "sh -c \"eog %f\"",
            "app \"%f\"",
            "app \"--file=%u\"",
            "app \"%F\"",
            "app \"%i\"",
            "app \"%z\"",
        ] {
            assert_eq!(expand(exec, "/x/y.png"), None, "{exec}");
        }
    }

    #[test]
    fn a_field_code_beside_a_quoted_span_in_one_token_refuses_the_entry() {
        // `"eog "%f` is one argument whose quoted prefix makes it code-bearing.
        assert_eq!(expand("sh -c \"eog \"%f", "/x/y.png"), None);
        assert_eq!(expand("app %f\" tail\"", "/x/y.png"), None);
    }

    #[test]
    fn embedded_list_codes_refuse_the_entry() {
        // `%F` and `%U` may only stand as an argument on their own.
        assert_eq!(expand("app --files=%F", "/x/y.png"), None);
        assert_eq!(expand("app x%U", "/x/y.png"), None);
    }

    #[test]
    fn ordinary_entries_beside_quoted_arguments_keep_working() {
        assert_eq!(
            expand("\"/opt/My App/run\" --title \"A B\" %F", "/x/y.png"),
            Some(vec![
                "/opt/My App/run".to_owned(),
                "--title".to_owned(),
                "A B".to_owned(),
                "/x/y.png".to_owned(),
            ])
        );
        assert_eq!(
            expand("app \"100%%\" %u", "/x/y.png"),
            Some(vec![
                "app".to_owned(),
                "100%".to_owned(),
                "file:///x/y.png".to_owned(),
            ])
        );
        assert_eq!(
            expand("app --file=%f", "/x/y.png"),
            Some(vec!["app".to_owned(), "--file=/x/y.png".to_owned()])
        );
    }
}
