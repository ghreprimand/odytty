// SPDX-License-Identifier: GPL-3.0-only
//! Quoting and batch regressions for native file-drop path insertion.
//!
//! App-route coverage (accumulate / cancel / stale / remote / overflow)
//! lives in `native::tests::file_drop_app` via the Headless shell override.
use super::*;

#[test]
fn powershell_drive_unc_quotes_and_unicode_are_literal() {
    for path in [
        r"C:\work dir\report.txt",
        r"\\server\share\file.txt",
        "C:\\雪\\a'b",
    ] {
        let quoted = quote_powershell(path).unwrap();
        assert_eq!(quoted, format!("'{}'", path.replace('\'', "''")));
        assert!(!quoted.contains(['\r', '\n']));
    }
    assert_eq!(quote_powershell("a‘b’c").unwrap(), "'a‘‘b’’c'");
    for path in ["a\nb", "a\rb", "a\u{1b}b", "a\0b"] {
        assert_eq!(quote_powershell(path), Err(DropError::UnsupportedPath));
    }
}

#[test]
fn powershell_smart_quote_grammar_doubles_every_delimiter() {
    // PowerShell treats typographic singles as string delimiters. Each must
    // be doubled inside a single-quoted token; ASCII apostrophe too.
    let cases = [
        ("a'b", "'a''b'"),
        ("a‘b", "'a‘‘b'"),
        ("a’b", "'a’’b'"),
        ("a‚b", "'a‚‚b'"), // U+201A
        ("a‛b", "'a‛‛b'"), // U+201B
        (
            "C:\\dir with ‘mixed’ and 'ascii' quotes",
            "'C:\\dir with ‘‘mixed’’ and ''ascii'' quotes'",
        ),
    ];
    for (path, expected) in cases {
        assert_eq!(quote_powershell(path).unwrap(), expected, "path={path:?}");
        assert!(
            !quote_powershell(path).unwrap().contains(['\r', '\n', '\0']),
            "controls must stay out of the PowerShell token"
        );
    }
}

#[test]
fn unix_quotes_spaces_apostrophes_and_shell_metacharacters() {
    let source = b"/tmp/a'b $(echo bad);`bad`\\z";
    assert_eq!(
        quote_unix(source, ShellKind::Bash).unwrap(),
        "'/tmp/a'\\''b $(echo bad);`bad`\\z'"
    );
    assert_eq!(
        quote_unix(source, ShellKind::Fish).unwrap(),
        "'/tmp/a\\'b $(echo bad);`bad`\\\\z'"
    );
    assert_eq!(
        quote_unix(source, ShellKind::Zsh),
        quote_unix(source, ShellKind::Bash)
    );
}

#[test]
fn unix_controls_and_invalid_utf8_use_byte_escapes() {
    for source in [
        &b"/tmp/a\nb"[..],
        &b"/tmp/\xff\x1b'b"[..],
        &b"/tmp/a\rb"[..],
    ] {
        for shell in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
            let text = quote_unix(source, shell).unwrap();
            assert!(text.is_ascii());
            assert!(!text.chars().any(char::is_control));
        }
    }
    assert_eq!(
        quote_unix(b"/a\0b", ShellKind::Bash),
        Err(DropError::UnsupportedPath)
    );
}

#[cfg(unix)]
#[test]
fn batch_preserves_order_and_rejects_whole_oversized_drop() {
    let mut batch = FileDropBatch::default();
    batch.push("/tmp/first file".into());
    batch.push("/tmp/second/".into());
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)).unwrap(),
        "'/tmp/first file' '/tmp/second/'"
    );
    let mut batch = FileDropBatch::default();
    for _ in 0..=MAX_DROP_FILES {
        batch.push("/tmp/file".into());
    }
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)),
        Err(DropError::TooLarge)
    );
    let mut batch = FileDropBatch::default();
    batch.push(PathBuf::from(format!("/{}", "x".repeat(MAX_DROP_BYTES))));
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)),
        Err(DropError::TooLarge)
    );
}

#[cfg(unix)]
#[test]
fn overflow_rejects_whole_batch_not_a_kept_prefix() {
    // One accepted path, then a byte overflow: the whole batch is cleared.
    let mut batch = FileDropBatch::default();
    batch.push("/tmp/kept-first".into());
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)).unwrap(),
        "'/tmp/kept-first'"
    );
    let oversized = PathBuf::from(format!("/{}", "y".repeat(MAX_DROP_BYTES)));
    batch.push(oversized);
    assert!(batch.rejected);
    assert!(batch.paths.is_empty());
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)),
        Err(DropError::TooLarge)
    );
    // Further events stay rejected; there is no silent prefix recovery.
    batch.push("/tmp/after-overflow".into());
    assert!(batch.rejected);
    assert!(batch.paths.is_empty());
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)),
        Err(DropError::TooLarge)
    );
}

#[cfg(unix)]
#[test]
fn file_count_overflow_clears_prior_paths_and_stays_rejected() {
    let mut batch = FileDropBatch::default();
    for i in 0..MAX_DROP_FILES {
        batch.push(PathBuf::from(format!("/tmp/n{i}")));
    }
    assert!(!batch.rejected);
    assert_eq!(batch.paths.len(), MAX_DROP_FILES);
    batch.push("/tmp/one-too-many".into());
    assert!(batch.rejected);
    assert!(batch.paths.is_empty());
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)),
        Err(DropError::TooLarge)
    );
}

#[test]
fn uri_schemes_and_relative_paths_are_not_file_events() {
    for value in [
        "https://example.invalid/file",
        "file:///tmp/file",
        "javascript:bad",
        "relative.txt",
    ] {
        assert_eq!(
            quote_path(Path::new(value), ShellKind::PowerShell),
            Err(DropError::UnsupportedPath)
        );
    }
}

#[cfg(unix)]
#[test]
fn unknown_shell_and_non_unicode_powershell_fail_closed() {
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/\xff".to_vec()));
    assert_eq!(
        quote_path(&path, ShellKind::PowerShell),
        Err(DropError::UnsupportedPath)
    );
    let mut batch = FileDropBatch::default();
    batch.push(path);
    assert_eq!(batch.insertion(None), Err(DropError::UnknownShell));
}

#[cfg(windows)]
#[test]
fn windows_unpaired_utf16_and_non_powershell_fail_closed() {
    use std::os::windows::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_wide(&[67, 58, 92, 0xd800]));
    assert_eq!(
        quote_path(&path, ShellKind::PowerShell),
        Err(DropError::UnsupportedPath)
    );
    assert_eq!(
        quote_path(Path::new(r"C:\file"), ShellKind::Bash),
        Err(DropError::UnsupportedPath)
    );
}

#[cfg(unix)]
fn shell_available(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("-c")
        .arg("true")
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Expand one or more already-quoted shell words and capture NUL-separated argv.
#[cfg(unix)]
fn shell_expand_argv(bin: &str, quoted_words: &str) -> Vec<u8> {
    // `printf %s\\0 WORD...` keeps each quoted token as a separate argv element.
    // Four backslashes: Rust -> `\\0` in the -c string -> printf receives `\0`.
    let script = format!("printf %s\\\\0 {quoted_words}");
    let output = std::process::Command::new(bin)
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap_or_else(|err| panic!("{bin} -c failed to spawn: {err}"));
    assert!(
        output.status.success(),
        "{bin} exit {:?}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[cfg(unix)]
fn assert_unix_roundtrip(shell: ShellKind, bin: &str, path_bytes: &[u8]) {
    if !shell_available(bin) {
        eprintln!("skip {bin} argv roundtrip: binary not installed");
        return;
    }
    let quoted = quote_unix(path_bytes, shell)
        .unwrap_or_else(|err| panic!("quote_unix({shell:?}) rejected {:?}: {err}", path_bytes));
    let got = shell_expand_argv(bin, &quoted);
    let mut expected = path_bytes.to_vec();
    expected.push(0);
    assert_eq!(
        got, expected,
        "{bin} roundtrip mismatch for path_bytes={path_bytes:?}; quoted={quoted}"
    );
}

#[cfg(unix)]
#[test]
fn installed_shells_roundtrip_hostile_utf8_path_bytes() {
    // Synthetic absolute names only; no host usernames, homes, or secrets.
    let cases: &[&[u8]] = &[
        b"/tmp/plain",
        b"/tmp/with space/name",
        b"/tmp/a'b",
        b"/tmp/a'b $(echo bad);`bad`\\z",
        b"/tmp/unicode-\xE9\x9B\xAA", // UTF-8 雪
        b"/tmp/dir/",
    ];
    for path in cases {
        assert_unix_roundtrip(ShellKind::Bash, "bash", path);
        assert_unix_roundtrip(ShellKind::Zsh, "zsh", path);
        assert_unix_roundtrip(ShellKind::Fish, "fish", path);
    }
}

#[cfg(unix)]
#[test]
fn installed_shells_roundtrip_non_utf8_and_control_bytes() {
    let cases: &[&[u8]] = &[
        b"/tmp/\xff",
        b"/tmp/\xff\xfe name",
        b"/tmp/a\nb",
        b"/tmp/\xff\x1b'b",
        b"/tmp/a\rb",
    ];
    for path in cases {
        assert_unix_roundtrip(ShellKind::Bash, "bash", path);
        assert_unix_roundtrip(ShellKind::Zsh, "zsh", path);
        assert_unix_roundtrip(ShellKind::Fish, "fish", path);
    }
}

#[cfg(unix)]
#[test]
fn installed_shells_roundtrip_multi_path_batch_order() {
    let paths: &[&[u8]] = &[b"/tmp/first file", b"/tmp/second'", b"/tmp/\xff-third"];
    for (shell, bin) in [
        (ShellKind::Bash, "bash"),
        (ShellKind::Zsh, "zsh"),
        (ShellKind::Fish, "fish"),
    ] {
        if !shell_available(bin) {
            eprintln!("skip {bin} multi-path roundtrip: binary not installed");
            continue;
        }
        let mut words = String::new();
        let mut expected = Vec::new();
        for path in paths {
            if !words.is_empty() {
                words.push(' ');
            }
            words.push_str(&quote_unix(path, shell).unwrap());
            expected.extend_from_slice(path);
            expected.push(0);
        }
        let got = shell_expand_argv(bin, &words);
        assert_eq!(got, expected, "{bin} multi-path order/bytes");
    }
}

#[cfg(unix)]
#[test]
fn nul_path_never_reaches_a_shell_word() {
    assert_eq!(
        quote_unix(b"/tmp/a\0b", ShellKind::Bash),
        Err(DropError::UnsupportedPath)
    );
    assert_eq!(
        quote_unix(b"/tmp/a\0b", ShellKind::Fish),
        Err(DropError::UnsupportedPath)
    );
}

#[test]
fn unix_quotes_spaces_as_exact_single_quoted_bytes() {
    let path = b"/tmp/with space/name";
    assert_eq!(
        quote_unix(path, ShellKind::Bash).unwrap(),
        "'/tmp/with space/name'"
    );
    assert_eq!(
        quote_unix(path, ShellKind::Zsh).unwrap(),
        "'/tmp/with space/name'"
    );
    assert_eq!(
        quote_unix(path, ShellKind::Fish).unwrap(),
        "'/tmp/with space/name'"
    );
}

#[test]
fn unix_quotes_single_and_double_quotes_exactly() {
    let path = b"/tmp/a'b\"c";
    assert_eq!(
        quote_unix(path, ShellKind::Bash).unwrap(),
        "'/tmp/a'\\''b\"c'"
    );
    assert_eq!(
        quote_unix(path, ShellKind::Zsh).unwrap(),
        "'/tmp/a'\\''b\"c'"
    );
    assert_eq!(
        quote_unix(path, ShellKind::Fish).unwrap(),
        "'/tmp/a\\'b\"c'"
    );
}

#[test]
fn unix_quotes_backslashes_exactly_per_shell_family() {
    let path = b"/tmp/a\\b\\\\c";
    assert_eq!(
        quote_unix(path, ShellKind::Bash).unwrap(),
        "'/tmp/a\\b\\\\c'"
    );
    assert_eq!(
        quote_unix(path, ShellKind::Zsh).unwrap(),
        "'/tmp/a\\b\\\\c'"
    );
    // Fish single-quoted grammar escapes backslash.
    assert_eq!(
        quote_unix(path, ShellKind::Fish).unwrap(),
        "'/tmp/a\\\\b\\\\\\\\c'"
    );
}

#[test]
fn unix_quotes_unicode_combining_rtl_and_emoji_as_literal_utf8() {
    // Combining acute on e, RTL override, grinning face.
    let path = "/tmp/e\u{0301}-\u{202E}name-\u{1F600}";
    let expected_bash = format!("'{path}'");
    assert_eq!(
        quote_unix(path.as_bytes(), ShellKind::Bash).unwrap(),
        expected_bash
    );
    assert_eq!(
        quote_unix(path.as_bytes(), ShellKind::Zsh).unwrap(),
        expected_bash
    );
    assert_eq!(
        quote_unix(path.as_bytes(), ShellKind::Fish).unwrap(),
        expected_bash
    );
    assert_eq!(
        quote_powershell(&format!("C:\\{path}")).unwrap(),
        format!("'C:\\{path}'")
    );
}

#[test]
fn unix_keeps_u2028_out_of_the_insertion_stream() {
    // U+2028 is a line separator: it must not reach the PTY as a raw scalar
    // even though Rust's char::is_control is false for it.
    let path = "/tmp/before\u{2028}after";
    for shell in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
        let quoted = quote_unix(path.as_bytes(), shell)
            .unwrap_or_else(|err| panic!("unix quoter refused U+2028 under {shell:?}: {err}"));
        assert!(
            !quoted.contains('\u{2028}'),
            "{shell:?} emitted raw U+2028 into insertion bytes: {quoted:?}"
        );
        assert!(
            !quoted
                .chars()
                .any(|ch| ch == '\u{2028}' || ch == '\u{2029}'),
            "{shell:?} must not emit line/paragraph separators"
        );
    }
}

#[test]
fn powershell_refuses_u2028_line_separator() {
    let path = "/tmp/before\u{2028}after";
    assert_eq!(
        quote_powershell(path),
        Err(DropError::UnsupportedPath),
        "PowerShell must refuse line separators rather than embed them"
    );
}

#[test]
fn unix_tab_uses_byte_escapes_not_raw_control() {
    let path = b"/tmp/a\tb";
    assert_eq!(
        quote_unix(path, ShellKind::Bash).unwrap(),
        "$'\\x2f\\x74\\x6d\\x70\\x2f\\x61\\x09\\x62'"
    );
    assert_eq!(
        quote_unix(path, ShellKind::Zsh).unwrap(),
        "$'\\x2f\\x74\\x6d\\x70\\x2f\\x61\\x09\\x62'"
    );
    assert_eq!(
        quote_unix(path, ShellKind::Fish).unwrap(),
        "\\X2f\\X74\\X6d\\X70\\X2f\\X61\\X09\\X62"
    );
    assert_eq!(
        quote_powershell("/tmp/a\tb"),
        Err(DropError::UnsupportedPath)
    );
}

#[test]
fn unix_non_utf8_0x80_0xff_and_invalid_continuation_use_byte_escapes() {
    let cases: &[(&[u8], &str, &str)] = &[
        (
            b"/tmp/\x80",
            "$'\\x2f\\x74\\x6d\\x70\\x2f\\x80'",
            "\\X2f\\X74\\X6d\\X70\\X2f\\X80",
        ),
        (
            b"/tmp/\xff",
            "$'\\x2f\\x74\\x6d\\x70\\x2f\\xff'",
            "\\X2f\\X74\\X6d\\X70\\X2f\\Xff",
        ),
        (
            // Invalid continuation: lead byte without trailers.
            b"/tmp/\xc3\x28",
            "$'\\x2f\\x74\\x6d\\x70\\x2f\\xc3\\x28'",
            "\\X2f\\X74\\X6d\\X70\\X2f\\Xc3\\X28",
        ),
    ];
    for (path, bash, fish) in cases {
        assert_eq!(
            quote_unix(path, ShellKind::Bash).unwrap(),
            *bash,
            "bash {path:?}"
        );
        assert_eq!(
            quote_unix(path, ShellKind::Zsh).unwrap(),
            *bash,
            "zsh {path:?}"
        );
        assert_eq!(
            quote_unix(path, ShellKind::Fish).unwrap(),
            *fish,
            "fish {path:?}"
        );
    }
}

#[test]
fn unix_quotes_leading_dash_tilde_dollar_backticks_and_command_subst() {
    let path = b"/tmp/-rf/~/$HOME/`id`/$(echo x)/!";
    let bash = quote_unix(path, ShellKind::Bash).unwrap();
    let fish = quote_unix(path, ShellKind::Fish).unwrap();
    assert_eq!(bash, "'/tmp/-rf/~/$HOME/`id`/$(echo x)/!'");
    assert_eq!(quote_unix(path, ShellKind::Zsh).unwrap(), bash);
    assert_eq!(fish, "'/tmp/-rf/~/$HOME/`id`/$(echo x)/!'");
    assert!(bash.starts_with('\'') && bash.ends_with('\''));
}

#[test]
fn unix_quotes_history_fish_comment_brace_and_glob_metacharacters() {
    // bash/zsh history `!`, fish `%`/`#`, brace `{}`, glob `*?[`.
    let path = b"/tmp/!hist/%var/#comment/{a,b}/file*?[x]";
    let bash = quote_unix(path, ShellKind::Bash).unwrap();
    let fish = quote_unix(path, ShellKind::Fish).unwrap();
    assert_eq!(bash, "'/tmp/!hist/%var/#comment/{a,b}/file*?[x]'");
    assert_eq!(quote_unix(path, ShellKind::Zsh).unwrap(), bash);
    assert_eq!(fish, "'/tmp/!hist/%var/#comment/{a,b}/file*?[x]'");
}

#[test]
fn powershell_quotes_drive_unc_and_device_namespace_paths() {
    for path in [
        r"C:\a b\report.txt",
        r"\\server\share\x",
        r"\\?\C:\Program Files\x",
        r"\\?\UNC\server\share\x",
    ] {
        let quoted = quote_powershell(path).unwrap();
        assert_eq!(quoted, format!("'{path}'"));
        assert_eq!(quoted.as_bytes().last().copied(), Some(b'\''));
        assert!(!quoted.as_bytes().contains(&b'\n'));
        assert!(!quoted.as_bytes().contains(&b'\r'));
    }
}

#[cfg(windows)]
#[test]
fn windows_pathbuf_embedded_nul_is_refused() {
    use std::os::windows::ffi::OsStringExt;
    // Wide NUL inside an otherwise absolute drive path must fail closed.
    let path = PathBuf::from(std::ffi::OsString::from_wide(&[
        0x0043, 0x003a, 0x005c, 0x0000, 0x0078,
    ]));
    assert_eq!(
        quote_path(&path, ShellKind::PowerShell),
        Err(DropError::UnsupportedPath)
    );
}

#[cfg(unix)]
#[test]
fn absolute_uri_shaped_names_quote_as_literal_filenames() {
    // OS path events can legally contain these bytes as a filename. They must
    // never be opened, fetched, or rewritten into a different scheme.
    for path in [
        "/tmp/file:///etc/passwd",
        "/tmp/http://example.invalid/x",
        "/tmp/javascript:void",
    ] {
        let bash = quote_unix(path.as_bytes(), ShellKind::Bash).unwrap();
        assert_eq!(bash, format!("'{path}'"));
        assert_eq!(quote_path(Path::new(path), ShellKind::Bash).unwrap(), bash);
        assert!(
            bash.contains("file://") || bash.contains("http://") || bash.contains("javascript:")
        );
    }
}

#[cfg(unix)]
#[test]
fn ten_thousand_path_batch_rejects_whole_collection() {
    let mut batch = FileDropBatch::default();
    for i in 0..10_000 {
        batch.push(PathBuf::from(format!("/tmp/n{i}")));
        if i >= MAX_DROP_FILES {
            assert!(
                batch.rejected,
                "batch must stay rejected after the {MAX_DROP_FILES}-file cap"
            );
            assert!(batch.paths.is_empty());
        }
    }
    assert!(batch.rejected);
    assert!(batch.paths.is_empty());
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)),
        Err(DropError::TooLarge)
    );
}

#[cfg(unix)]
#[test]
fn directory_trailing_separator_bytes_are_preserved() {
    let path = b"/tmp/dir/";
    assert_eq!(quote_unix(path, ShellKind::Bash).unwrap(), "'/tmp/dir/'");
    assert_eq!(quote_unix(path, ShellKind::Fish).unwrap(), "'/tmp/dir/'");
    let mut batch = FileDropBatch::default();
    batch.push(PathBuf::from("/tmp/dir/"));
    assert_eq!(
        batch.insertion(Some(ShellKind::Bash)).unwrap(),
        "'/tmp/dir/'"
    );
}

#[cfg(unix)]
#[test]
fn installed_shells_roundtrip_plan_named_metacharacter_paths() {
    let cases: &[&[u8]] = &[
        b"/tmp/with space/name",
        b"/tmp/a'b\"c",
        b"/tmp/a\\b\\\\c",
        b"/tmp/-rf",
        b"/tmp/~/$HOME/`id`/$(echo x)/!",
        b"/tmp/!hist/%var/#comment/{a,b}/file*?[x]",
        b"/tmp/file:///etc/passwd",
        b"/tmp/http://example.invalid/x",
        b"/tmp/javascript:void",
        b"/tmp/e\xcc\x81", // combining acute as UTF-8 bytes
        "/tmp/\u{1F600}".as_bytes(),
        b"/tmp/a\tb",
        b"/tmp/\x80",
        b"/tmp/\xff",
        b"/tmp/\xc3\x28",
        b"/tmp/dir/",
    ];
    for path in cases {
        assert_unix_roundtrip(ShellKind::Bash, "bash", path);
        assert_unix_roundtrip(ShellKind::Zsh, "zsh", path);
        assert_unix_roundtrip(ShellKind::Fish, "fish", path);
    }
}

#[cfg(unix)]
#[test]
fn installed_shells_roundtrip_via_positional_parameter() {
    // `$1`-style echo: the quoted token is expanded by the shell grammar, then
    // passed through "$1" so the reconstructed argv bytes must match.
    let cases: &[&[u8]] = &[
        b"/tmp/plain",
        b"/tmp/with space/name",
        b"/tmp/a'b\"c",
        b"/tmp/-rf/~/$HOME/`id`",
        b"/tmp/!hist/%var/#c/{a,b}/x*?[0]",
        b"/tmp/\xff",
    ];
    for (shell, bin) in [
        (ShellKind::Bash, "bash"),
        (ShellKind::Zsh, "zsh"),
        (ShellKind::Fish, "fish"),
    ] {
        if !shell_available(bin) {
            eprintln!("skip {bin} $1 roundtrip: binary not installed");
            continue;
        }
        for path in cases {
            let quoted = quote_unix(path, shell).unwrap();
            // Insert the already-quoted token as a shell word, then echo via $1
            // (or fish argv) so reconstruction proves the grammar, not Rust argv.
            let script = if bin == "fish" {
                format!("printf '%s\\0' {quoted}")
            } else {
                format!("set -- {quoted}; printf '%s\\0' \"$1\"")
            };
            let output = std::process::Command::new(bin)
                .arg("-c")
                .arg(&script)
                .output()
                .unwrap_or_else(|err| panic!("{bin} $1 roundtrip spawn failed: {err}"));
            assert!(
                output.status.success(),
                "{bin} $1 roundtrip exit {:?}; stderr={}; script={script}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
            let mut expected = path.to_vec();
            expected.push(0);
            assert_eq!(
                output.stdout, expected,
                "{bin} $1 roundtrip mismatch path={path:?} quoted={quoted}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn mixed_shell_powershell_payload_stays_one_bash_literal_argument() {
    // Documentation: Bash quoting keeps a PowerShell-shaped payload as one
    // argv element. Cross-family safety belongs to the authority layer, not
    // the quoter. The path contains an apostrophe, so Bash uses '\'' splitting.
    let path = b"/tmp/a';Write-Output PWNED;#";
    let quoted = quote_unix(path, ShellKind::Bash).unwrap();
    let expected = format!(
        "'{}'",
        std::str::from_utf8(path).unwrap().replace('\'', "'\\''")
    );
    assert_eq!(quoted, expected);
    // Byte form of the bash '\'' split around the apostrophe after `a`.
    assert_eq!(
        quoted.as_bytes(),
        [
            39, 47, 116, 109, 112, 47, 97, 39, 92, 39, 39, 59, 87, 114, 105, 116, 101, 45, 79, 117,
            116, 112, 117, 116, 32, 80, 87, 78, 69, 68, 59, 35, 39
        ]
    );
    assert_unix_roundtrip(ShellKind::Bash, "bash", path);
    if shell_available("bash") {
        let got = shell_expand_argv("bash", &quoted);
        let mut expected = path.to_vec();
        expected.push(0);
        assert_eq!(
            got, expected,
            "bash must keep the PowerShell payload as one word"
        );
        assert_eq!(
            got.split(|&b| b == 0).filter(|s| !s.is_empty()).count(),
            1,
            "exactly one argv element, not a Write-Output command"
        );
    }
}

#[cfg(unix)]
#[test]
fn mixed_shell_non_utf8_fish_escape_and_bash_byte_roundtrip() {
    // Documentation: Fish emits \\Xhh for non-UTF-8; Bash $'\\xhh' round-trips
    // the same bytes under bash. Each family's token is only claimed for itself.
    let path = b"/tmp/\xff";
    let fish = quote_unix(path, ShellKind::Fish).unwrap();
    assert!(
        fish.contains("\\Xff") || fish.contains("\\XFF"),
        "fish quoting must be \\Xff-style; got {fish}"
    );
    assert_eq!(fish, "\\X2f\\X74\\X6d\\X70\\X2f\\Xff");
    let bash = quote_unix(path, ShellKind::Bash).unwrap();
    assert_eq!(bash, "$'\\x2f\\x74\\x6d\\x70\\x2f\\xff'");
    assert_unix_roundtrip(ShellKind::Bash, "bash", path);
    assert_unix_roundtrip(ShellKind::Fish, "fish", path);
}

#[cfg(windows)]
fn windows_powershell_command() -> std::process::Command {
    use std::process::Command;
    for bin in ["powershell.exe", "pwsh.exe", "pwsh"] {
        let mut probe = Command::new(bin);
        crate::native::app::win_spawn::apply_no_console_window(&mut probe);
        let ok = probe
            .args(["-NoProfile", "-NonInteractive", "-Command", "$null"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if ok {
            let mut cmd = Command::new(bin);
            crate::native::app::win_spawn::apply_no_console_window(&mut cmd);
            return cmd;
        }
    }
    panic!("powershell.exe / pwsh not available on Windows CI host");
}

#[cfg(windows)]
fn powershell_roundtrip_code_units(path: &str) -> Vec<u16> {
    let quoted = quote_powershell(path).unwrap_or_else(|err| {
        panic!("quote_powershell refused {path:?}: {err}");
    });
    // Assign the already-quoted token, then emit UTF-16 code units as decimals.
    let script = format!("$x = {quoted}; ([int[]][char[]]$x) -join ','");
    let output = windows_powershell_command()
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .unwrap_or_else(|err| panic!("PowerShell spawn failed for {path:?}: {err}"));
    assert!(
        output.status.success(),
        "PowerShell exit {:?} path={path:?} stderr={} script={script}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    text.split(',')
        .map(|part| {
            part.trim()
                .parse::<u16>()
                .unwrap_or_else(|err| panic!("bad code unit {part:?} from {text:?}: {err}"))
        })
        .collect()
}

#[cfg(windows)]
#[test]
fn powershell_parser_roundtrips_drive_unc_device_and_quote_codepoints() {
    let cases = [
        r"C:\a b\c.txt",
        r"\\server\share\x y",
        r"\\?\C:\x",
        r"C:\o'brien.txt",
        "C:\\q\u{0027}end",
        "C:\\q\u{2018}end",
        "C:\\q\u{2019}end",
        "C:\\q\u{201A}end",
        "C:\\q\u{201B}end",
    ];
    for path in cases {
        let got = powershell_roundtrip_code_units(path);
        let expected: Vec<u16> = path.encode_utf16().collect();
        assert_eq!(
            got,
            expected,
            "PowerShell parser round-trip mismatch for {path:?}; quoted={:?}",
            quote_powershell(path).unwrap()
        );
    }
}
