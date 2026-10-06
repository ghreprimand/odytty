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
//! `sh -c "eog %f"` would hand the path to a shell as code. Field codes inside
//! a quoted argument are undefined by the Desktop Entry specification, and
//! `%F`/`%U` may only stand alone, so an entry using either form is refused
//! before any argv is built.
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
/// * any field code other than `%%` in a token that contains quoted text,
///   such as `sh -c "eog %f"` or `"eog "%f`;
/// * `%F` or `%U` inside a longer token, such as `--files=%F`.
///
/// Field-code substitution (per token, after tokenizing):
/// * `%f` / `%F` -> the bare absolute path (we only ever open one file);
/// * `%u` / `%U` -> the `file://` URI of that path;
/// * `%i` `%c` `%k` and the deprecated `%d %D %n %N %v %m` -> stripped;
/// * `%%` -> a literal `%`, also inside quotes;
/// * any other `%x` -> stripped (unknown/undefined field code);
/// * `%f` or `%u` inside a longer unquoted token (`--file=%f`) substitutes in
///   place and the token stays ONE argv element;
/// * a token that expands to nothing (a standalone stripped code like `%i`) is
///   dropped from the argv;
/// * if no `%f/%F/%u/%U` appears anywhere, the bare path is appended as a
///   trailing element (matches `xdg-open`/`gio` behaviour for simple entries).
pub fn exec_to_argv(exec: &str, abs: &str) -> Option<Vec<String>> {
    let tokens = tokenize(exec);
    if tokens.iter().any(refuses_field_codes) {
        return None;
    }
    let uri = file_uri(abs);
    let mut argv: Vec<String> = Vec::new();
    let mut saw_path = false;

    for token in &tokens {
        let mut out = String::new();
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
        if out.is_empty() && !token.text.is_empty() {
            continue;
        }
        argv.push(out);
    }

    if !saw_path {
        argv.push(abs.to_owned());
    }
    Some(argv)
}

/// One `Exec` token and whether any of its text came from a quoted span.
struct Token {
    text: String,
    quoted: bool,
}

/// Whether a token uses a field code in a refused context: any code other
/// than `%%` in a token containing quoted text, or `%F`/`%U` that is not the
/// whole token.
fn refuses_field_codes(token: &Token) -> bool {
    let mut chars = token.text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        match chars.next() {
            Some('%') | None => {}
            Some(code) => {
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
/// contain quoted text. Pure; no field-code work.
fn tokenize(exec: &str) -> Vec<Token> {
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
        tokens.push(Token { text: cur, quoted });
    }
    tokens
}

#[cfg(test)]
mod tests {
    //! Pure field-code + quoting tests. NONE spawns a process; every case
    //! asserts the built argv vector. Synthetic paths only — no real filesystem,
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
    fn unknown_field_code_is_stripped() {
        // `%z` is undefined → stripped, path appended.
        assert_eq!(
            argv_for("app %z", "/x/y.png"),
            vec!["app".to_owned(), "/x/y.png".to_owned()]
        );
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
        // A path full of shell metacharacters is a single, inert argv element —
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
    fn empty_exec_yields_just_the_path() {
        // Defensive: an empty Exec (filtered out before this in production) does
        // not panic; it degrades to a lone path element.
        assert_eq!(argv_for("", "/x/y.png"), vec!["/x/y.png".to_owned()]);
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
