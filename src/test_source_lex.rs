// SPDX-License-Identifier: GPL-3.0-only
//! A small Rust lexer for source-scanning guard tests.
//!
//! A guard that searches source text for a name can be satisfied or tripped by
//! the same name in a comment or a string literal. Guards built on this lexer
//! see only code tokens: comments never become tokens, and the contents of
//! string and character literals are kept apart from identifiers, so text in
//! either place cannot stand in for a call or hide one.
//!
//! This is not a parser. It splits a file into identifiers, punctuation, and
//! literals, and the helpers answer the structural questions the guards ask
//! (does this function body call that name, which test functions exist, what
//! string does this constant hold). Test-only.

use std::fs;
use std::path::{Path, PathBuf};

/// One lexical token of a Rust source file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Token {
    Ident(String),
    Punct(&'static str),
    /// A string literal (plain, byte, C, or raw) with its source text between
    /// the quotes, escapes left as written.
    Str(String),
    /// A number or a character literal.
    Literal,
}

const OPERATORS: [&str; 24] = [
    "<<=", ">>=", "..=", "==", "=>", "!=", "<=", ">=", "+=", "-=", "*=", "/=", "%=", "^=", "&=",
    "|=", "::", "->", "..", "&&", "||", "<<", ">>", "=",
];
const SINGLE_PUNCT: [&str; 18] = [
    "{", "}", "(", ")", "[", "]", ";", ",", ".", ":", "<", ">", "&", "|", "!", "#", "?", "@",
];

/// Split `source` into code tokens.
pub(crate) fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 1;
            i += 2;
            while i < chars.len() && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if let Some((text_start, text_end, end)) = string_literal_span(&chars, i) {
            tokens.push(Token::Str(chars[text_start..text_end].iter().collect()));
            i = end;
        } else if c == '\'' {
            // A character literal closes within a few characters; otherwise
            // this is a lifetime or loop label.
            let closes = if next == Some('\\') {
                chars[i + 2..].iter().take(10).position(|&x| x == '\'')
            } else if chars.get(i + 2) == Some(&'\'') {
                Some(0)
            } else {
                None
            };
            if let Some(offset) = closes {
                tokens.push(Token::Literal);
                i += if next == Some('\\') { 3 + offset } else { 3 };
            } else {
                i += 1;
            }
        } else if c.is_alphabetic() || c == '_' {
            let start = if c == 'r'
                && next == Some('#')
                && chars
                    .get(i + 2)
                    .is_some_and(|x| x.is_alphabetic() || *x == '_')
            {
                i + 2
            } else {
                i
            };
            let mut end = start;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            tokens.push(Token::Ident(chars[start..end].iter().collect()));
            i = end;
        } else if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            tokens.push(Token::Literal);
        } else if let Some(operator) = OPERATORS.iter().find(|operator| {
            chars[i..]
                .iter()
                .take(3)
                .collect::<String>()
                .starts_with(**operator)
        }) {
            tokens.push(Token::Punct(operator));
            i += operator.len();
        } else {
            if let Some(single) = SINGLE_PUNCT.iter().find(|p| p.starts_with(c)) {
                tokens.push(Token::Punct(single));
            }
            i += 1;
        }
    }
    tokens
}

/// Where a string literal starting at `start` keeps its text and where it
/// ends: `(text start, text end, end)`, the end exclusive. Plain, byte, C,
/// and raw literals with any number of `#` are recognised; `None` when no
/// string starts here.
fn string_literal_span(chars: &[char], start: usize) -> Option<(usize, usize, usize)> {
    let mut i = start;
    while matches!(chars.get(i), Some('b' | 'c')) && i < start + 1 {
        i += 1;
    }
    let raw = chars.get(i) == Some(&'r');
    if raw {
        i += 1;
    }
    let mut hashes = 0;
    while raw && chars.get(i) == Some(&'#') {
        hashes += 1;
        i += 1;
    }
    if chars.get(i) != Some(&'"') {
        return None;
    }
    i += 1;
    let text_start = i;
    while i < chars.len() {
        if !raw && chars[i] == '\\' {
            i += 2;
        } else if chars[i] == '"'
            && chars[i + 1..]
                .iter()
                .take(hashes)
                .filter(|c| **c == '#')
                .count()
                == hashes
        {
            return Some((text_start, i, i + 1 + hashes));
        } else {
            i += 1;
        }
    }
    Some((text_start.min(chars.len()), chars.len(), chars.len()))
}

/// Index of the token that closes the bracket opened at `open`.
pub(crate) fn matching_close(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::Punct("{" | "(" | "[") => depth += 1,
            Token::Punct("}" | ")" | "]") => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `tokens` starts with a call to `name`.
pub(crate) fn starts_with_call(tokens: &[Token], name: &str) -> bool {
    matches!(
        tokens,
        [Token::Ident(first), Token::Punct("("), ..] if first == name
    )
}

/// Whether `token` is the identifier or punctuation `word`.
fn is_word(token: &Token, word: &str) -> bool {
    match token {
        Token::Ident(name) => name == word,
        Token::Punct(punct) => *punct == word,
        Token::Str(_) | Token::Literal => false,
    }
}

/// Whether `tokens` holds the consecutive identifiers and punctuation `words`,
/// for example `["Command", "::", "new", "("]`.
pub(crate) fn has_sequence(tokens: &[Token], words: &[&str]) -> bool {
    !words.is_empty()
        && tokens.windows(words.len()).any(|window| {
            window
                .iter()
                .zip(words)
                .all(|(token, word)| is_word(token, word))
        })
}

/// Whether `tokens` calls `name` as a function, method, or path tail.
pub(crate) fn calls(tokens: &[Token], name: &str) -> bool {
    has_sequence(tokens, &[name, "("])
}

/// The body tokens, braces excluded, of the first `fn <name>` in `tokens`.
pub(crate) fn fn_body<'a>(tokens: &'a [Token], name: &str) -> Option<&'a [Token]> {
    let at = tokens
        .windows(2)
        .position(|pair| is_word(&pair[0], "fn") && is_word(&pair[1], name))?;
    body_after(tokens, at + 2)
}

/// The body of the function whose name sits just before `from`: the tokens
/// between the next `{` and its closing brace.
fn body_after(tokens: &[Token], from: usize) -> Option<&[Token]> {
    let open = from
        + tokens[from..]
            .iter()
            .position(|t| *t == Token::Punct("{"))?;
    let close = matching_close(tokens, open)?;
    Some(&tokens[open + 1..close])
}

/// Every function marked `#[test]`, as its name and body tokens.
pub(crate) fn test_functions(tokens: &[Token]) -> Vec<(&str, &[Token])> {
    let mut found = Vec::new();
    for at in 0..tokens.len() {
        let marked = matches!(
            &tokens[at..],
            [Token::Punct("#"), Token::Punct("["), Token::Ident(test), Token::Punct("]"), ..]
                if test == "test"
        );
        if !marked {
            continue;
        }
        // Other attributes may sit between the marker and the `fn`.
        let Some(offset) = tokens[at + 4..]
            .iter()
            .position(|token| is_word(token, "fn"))
        else {
            continue;
        };
        let name_at = at + 4 + offset + 1;
        let Some(Token::Ident(name)) = tokens.get(name_at) else {
            continue;
        };
        if let Some(body) = body_after(tokens, name_at + 1) {
            found.push((name.as_str(), body));
        }
    }
    found
}

/// The `mod <name>;` declarations in `tokens`.
pub(crate) fn declared_modules(tokens: &[Token]) -> Vec<&str> {
    tokens
        .windows(3)
        .filter_map(|window| match window {
            [Token::Ident(keyword), Token::Ident(name), Token::Punct(";")] if keyword == "mod" => {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect()
}

/// The `const <NAME>: <type> = "<text>";` items in `tokens`, as name and the
/// text between the quotes.
pub(crate) fn string_consts(tokens: &[Token]) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    for at in 0..tokens.len() {
        let [
            Token::Ident(keyword),
            Token::Ident(name),
            Token::Punct(":"),
            rest @ ..,
        ] = &tokens[at..]
        else {
            continue;
        };
        if keyword != "const" {
            continue;
        }
        let Some(equals) = rest.iter().position(|t| *t == Token::Punct("=")) else {
            continue;
        };
        if let [Token::Str(text), Token::Punct(";"), ..] = &rest[equals + 1..] {
            found.push((name.as_str(), text.as_str()));
        }
    }
    found
}

/// Every `.rs` file under `directory`, recursively.
pub(crate) fn rust_sources_recursively(directory: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed reading {}: {error}", directory.display()));
    let mut sources = Vec::new();
    for entry in entries {
        let entry = entry.expect("source directory entry");
        let path = entry.path();
        if path.is_dir() {
            sources.extend(rust_sources_recursively(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idents(tokens: &[Token]) -> Vec<&str> {
        tokens
            .iter()
            .filter_map(|token| match token {
                Token::Ident(name) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn comments_never_become_tokens() {
        let tokens = tokenize("a // b\n/* c /* d */ e */ f /// g\n h");
        assert_eq!(idents(&tokens), ["a", "f", "h"]);
    }

    #[test]
    fn string_text_stays_in_the_string_token() {
        let tokens = tokenize(r###"x "call(" r#"raw "quoted" call("# b"bytes" y"###);
        assert_eq!(idents(&tokens), ["x", "y"]);
        let texts: Vec<&str> = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Str(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["call(", r#"raw "quoted" call("#, "bytes"]);
    }

    #[test]
    fn lifetimes_and_character_literals_are_told_apart() {
        let tokens = tokenize("fn f<'a>(x: &'a str) { let c = '}'; let d = '\\n'; }");
        let open = tokens
            .iter()
            .position(|t| *t == Token::Punct("{"))
            .expect("body opens");
        assert_eq!(matching_close(&tokens, open), Some(tokens.len() - 1));
    }

    #[test]
    fn a_closing_bracket_first_has_no_match() {
        assert_eq!(matching_close(&tokenize(") ("), 0), None);
    }

    #[test]
    fn sequences_and_calls_match_code_only() {
        let tokens = tokenize(
            r#"fn f() { // Command::new(
            let s = "Command::new("; Command :: new("x"); helper(1); }"#,
        );
        assert!(has_sequence(&tokens, &["Command", "::", "new", "("]));
        assert!(calls(&tokens, "helper"));
        assert!(!calls(&tokens, "missing"));
        let quiet = tokenize(
            r#"fn f() { // Command::new(
            let s = "Command::new("; }"#,
        );
        assert!(!has_sequence(&quiet, &["Command", "::", "new", "("]));
    }

    #[test]
    fn function_bodies_are_found_by_name_with_nesting() {
        let tokens =
            tokenize("fn one() { if x { y() } } fn two<T>(a: T) -> Out where T: Copy { inner(a) }");
        let one = fn_body(&tokens, "one").expect("one");
        assert!(calls(one, "y"));
        let two = fn_body(&tokens, "two").expect("two");
        assert!(calls(two, "inner"));
        assert!(!calls(two, "y"));
        assert!(fn_body(&tokens, "three").is_none());
    }

    #[test]
    fn test_functions_are_listed_with_their_own_bodies() {
        let tokens = tokenize(
            r##"
            #[test]
            fn first() { a(); }
            // #[test] fn commented() { b(); }
            const NOTE: &str = "#[test] fn quoted() { c(); }";
            #[test]
            #[cfg(unix)]
            fn second() { d(); }
            fn helper() { e(); }
            "##,
        );
        let tests = test_functions(&tokens);
        let names: Vec<&str> = tests.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["first", "second"]);
        assert!(calls(tests[0].1, "a") && !calls(tests[0].1, "e"));
        assert!(calls(tests[1].1, "d") && !calls(tests[1].1, "e"));
    }

    #[test]
    fn modules_and_string_consts_are_read_from_code() {
        let tokens = tokenize(
            r#"
            mod alpha;
            // mod hidden;
            mod inline { fn x() {} }
            pub const A_ENV: &str = "ODYTTY_A";
            pub const B_ENV: &str =
                "ODYTTY_B";
            // pub const C_ENV: &str = "ODYTTY_C";
            const COUNT: usize = 3;
            const D_ENV: &str = concat!("ODYTTY_", "D");
            "#,
        );
        assert_eq!(declared_modules(&tokens), ["alpha"]);
        assert_eq!(
            string_consts(&tokens),
            [("A_ENV", "ODYTTY_A"), ("B_ENV", "ODYTTY_B")]
        );
    }
}
