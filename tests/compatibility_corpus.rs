// SPDX-License-Identifier: GPL-3.0-only
//! Compatibility corpus replay harness.
//!
//! Executes the tracked corpus under `tests/fixtures/compatibility/cases`
//! through the public `Terminal` API and asserts the expectations each case
//! declares. This is the executing half of the corpus contract in
//! `docs/compatibility/corpus.md`; the validating half is
//! `scripts/compatibility-corpus.py`, which enforces provenance, privacy
//! guards, resource caps, and manifest correspondence. This harness
//! deliberately re-checks the structural rules it depends on (chunk sums,
//! id/stem equality, the blanket at-sign ban, in-geometry expectations) so a
//! malformed corpus fails here with a clear message even when the validator
//! has not been run.
//!
//! The case file grammar is mirrored exactly from the validator: `#`
//! comment/directive lines, a closed directive vocabulary in the header,
//! content lines assembled with the `\e \r \n \t \\ \xNN` escape notation and
//! no implicit line terminators. Two grammars would be one too many.
//!
//! Replay is deterministic and host-independent: fixed geometry, explicit
//! chunk sizes that must sum to the payload length, no PTY, no external
//! commands, no timing. Every case runs in the default `cargo test` tier.

use odytty::core::Terminal;
use std::fs;
use std::path::{Path, PathBuf};

const SPDX_LINE: &str = "# SPDX-License-Identifier: GPL-3.0-only";

#[derive(Debug)]
enum Expectation {
    Line(usize, String),
    Contains(String),
    NotContains(String),
    Cursor(usize, usize),
    ScrollbackLen(usize),
    HostOutput(Vec<u8>),
    Cwd(String),
    CwdUnix(String),
    CwdWindows(String),
    CwdNone,
}

#[derive(Debug)]
struct Case {
    id: String,
    columns: usize,
    rows: usize,
    chunks: Vec<usize>,
    expectations: Vec<Expectation>,
    payload: Vec<u8>,
}

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compatibility/cases")
}

fn corpus_files() -> Vec<PathBuf> {
    let dir = cases_dir();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("vtseq"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "{}: no corpus cases found; an empty corpus is a harness error, not a clean sheet",
        dir.display()
    );
    files
}

/// Assemble one content line or expectation value into bytes: `\e`, `\r`,
/// `\n`, `\t`, `\\`, `\xNN`; every other character emits its UTF-8 encoding.
fn assemble(text: &str) -> Result<Vec<u8>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            index += 1;
            continue;
        }
        index += 1;
        if index >= chars.len() {
            return Err("dangling backslash".to_string());
        }
        let marker = chars[index];
        if marker == 'x' {
            if index + 2 >= chars.len() {
                return Err("truncated hex escape".to_string());
            }
            let digits: String = chars[index + 1..=index + 2].iter().collect();
            let byte = u8::from_str_radix(&digits, 16)
                .map_err(|_| format!("malformed hex escape `\\x{digits}`"))?;
            out.push(byte);
            index += 3;
            continue;
        }
        let escaped: &[u8] = match marker {
            'e' => b"\x1b",
            'r' => b"\r",
            'n' => b"\n",
            't' => b"\t",
            '\\' => b"\\",
            other => return Err(format!("unknown escape `\\{other}`")),
        };
        out.extend_from_slice(escaped);
        index += 1;
    }
    Ok(out)
}

fn decode_text(value: &str) -> Result<String, String> {
    if value != value.trim_end() {
        return Err(
            "expectation text has trailing whitespace; trimmed rows never carry any".to_string(),
        );
    }
    let bytes = assemble(value)?;
    String::from_utf8(bytes).map_err(|_| "expectation text is not valid UTF-8".to_string())
}

fn parse_usize(value: &str, what: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("{what} must be a non-negative integer, got `{value}`"))
}

/// Parse the value of a `= <text>` directive (expect-contains and friends).
fn text_after_equals(value: &str, directive: &str) -> Result<String, String> {
    match value.strip_prefix("= ") {
        Some(body) => decode_text(body),
        None => Err(format!("{directive} must take the form `= <text>`")),
    }
}

fn is_directive_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

#[derive(Debug, Default)]
struct CaseBuilder {
    id: Option<String>,
    geometry: Option<(usize, usize)>,
    chunks: Option<Vec<usize>>,
    expectations: Vec<Expectation>,
    seen_singletons: Vec<String>,
    expect_line_rows: Vec<usize>,
}

impl CaseBuilder {
    fn parse_directive(
        &mut self,
        key: &str,
        value: &str,
        number: usize,
        seen_content: bool,
    ) -> Result<(), String> {
        let known = matches!(
            key,
            "id" | "geometry"
                | "chunks"
                | "expect-cursor"
                | "expect-scrollback-len"
                | "expect-host-output-hex"
                | "expect-cwd"
                | "expect-cwd-unix"
                | "expect-cwd-windows"
                | "expect-cwd-none"
                | "expect-line"
                | "expect-contains"
                | "expect-not-contains"
        );
        if !known {
            return Err(format!(
                "line {number} looks like a directive but `{key}` is not a known directive"
            ));
        }
        if seen_content {
            return Err(format!(
                "line {number} directive `{key}` appears after payload content"
            ));
        }
        let singleton = !matches!(
            key,
            "expect-line" | "expect-contains" | "expect-not-contains"
        );
        if singleton && self.seen_singletons.iter().any(|seen| seen == key) {
            return Err(format!("duplicate directive `{key}`"));
        }
        if singleton {
            self.seen_singletons.push(key.to_string());
        }
        match key {
            "id" => self.id = Some(value.to_string()),
            "geometry" => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() != 2 {
                    return Err("geometry must be `<columns> <rows>`".to_string());
                }
                self.geometry = Some((
                    parse_usize(parts[0], "geometry columns")?,
                    parse_usize(parts[1], "geometry rows")?,
                ));
            }
            "chunks" => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.is_empty() {
                    return Err("chunks must be one or more positive integers".to_string());
                }
                let mut sizes = Vec::new();
                for part in parts {
                    let size = parse_usize(part, "chunk size")?;
                    if size == 0 {
                        return Err("chunk sizes must be positive".to_string());
                    }
                    sizes.push(size);
                }
                self.chunks = Some(sizes);
            }
            "expect-cursor" => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() != 2 {
                    return Err("expect-cursor must be `<row> <column>` (0-indexed)".to_string());
                }
                self.expectations.push(Expectation::Cursor(
                    parse_usize(parts[0], "expect-cursor row")?,
                    parse_usize(parts[1], "expect-cursor column")?,
                ));
            }
            "expect-scrollback-len" => {
                self.expectations
                    .push(Expectation::ScrollbackLen(parse_usize(
                        value,
                        "expect-scrollback-len",
                    )?));
            }
            "expect-host-output-hex" => {
                if !value.len().is_multiple_of(2)
                    || !value
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                {
                    return Err("expect-host-output-hex must be lowercase hex pairs".to_string());
                }
                let mut bytes = Vec::new();
                for pair in 0..value.len() / 2 {
                    bytes.push(
                        u8::from_str_radix(&value[pair * 2..pair * 2 + 2], 16)
                            .map_err(|err| err.to_string())?,
                    );
                }
                self.expectations.push(Expectation::HostOutput(bytes));
            }
            "expect-cwd-none" => {
                if !value.is_empty() {
                    return Err("expect-cwd-none takes no value".to_string());
                }
                self.expectations.push(Expectation::CwdNone);
            }
            "expect-cwd" => self
                .expectations
                .push(Expectation::Cwd(text_after_equals(value, key)?)),
            "expect-cwd-unix" => self
                .expectations
                .push(Expectation::CwdUnix(text_after_equals(value, key)?)),
            "expect-cwd-windows" => self
                .expectations
                .push(Expectation::CwdWindows(text_after_equals(value, key)?)),
            "expect-line" => {
                let (row, rest) = value
                    .split_once(' ')
                    .ok_or_else(|| "expect-line must be `<row> = <text>`".to_string())?;
                let row = parse_usize(row, "expect-line row")?;
                if self.expect_line_rows.contains(&row) {
                    return Err(format!("duplicate expect-line for row {row}"));
                }
                self.expect_line_rows.push(row);
                let body = if rest == "=" {
                    ""
                } else {
                    rest.strip_prefix("= ")
                        .ok_or_else(|| "expect-line must be `<row> = <text>`".to_string())?
                };
                self.expectations
                    .push(Expectation::Line(row, decode_text(body)?));
            }
            "expect-contains" => {
                self.expectations
                    .push(Expectation::Contains(text_after_equals(value, key)?));
            }
            "expect-not-contains" => {
                self.expectations
                    .push(Expectation::NotContains(text_after_equals(value, key)?));
            }
            other => return Err(format!("unhandled directive `{other}`")),
        }
        Ok(())
    }
}

fn load_case(path: &Path) -> Result<Case, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("cannot read: {err}"))?;
    if text.starts_with('\u{feff}') {
        return Err("UTF-8 BOM present".to_string());
    }
    if text.contains('\r') {
        return Err("carriage-return byte in file; line endings must be LF".to_string());
    }
    if text.contains('@') {
        return Err(
            "literal at-sign found; the ban is blanket across every tracked corpus file"
                .to_string(),
        );
    }
    let mut lines = text.split('\n');
    if lines.next() != Some(SPDX_LINE) {
        return Err(format!("line 1 must be `{SPDX_LINE}`"));
    }

    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "file name is not valid UTF-8".to_string())?;
    let mut builder = CaseBuilder::default();
    let mut payload = Vec::new();
    let mut seen_content = false;

    for (number, line) in lines.enumerate() {
        let number = number + 2;
        if line != line.trim_end() {
            return Err(format!("line {number} has trailing whitespace"));
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('#') {
            // A comment line: prose, or a `# key: value` directive with a
            // lowercase hyphenated key. Anything else is ignored here.
            if let Some(comment) = line.strip_prefix("# ")
                && let Some((key, raw_value)) = comment.split_once(':')
                && is_directive_key(key)
            {
                let value = raw_value.strip_prefix(' ').unwrap_or("");
                builder.parse_directive(key, value, number, seen_content)?;
            }
            continue;
        }
        seen_content = true;
        payload.extend(assemble(line).map_err(|err| format!("line {number}: {err}"))?);
    }

    let id = builder
        .id
        .ok_or_else(|| "missing `# id:` directive".to_string())?;
    if id != stem {
        return Err(format!("id `{id}` does not match file stem `{stem}`"));
    }
    let (columns, rows) = builder
        .geometry
        .ok_or_else(|| "missing `# geometry:` directive".to_string())?;
    if payload.is_empty() {
        return Err("empty payload; a case must feed at least one byte".to_string());
    }
    if builder.expectations.is_empty() {
        return Err("no expectations; a case without one is not a regression test".to_string());
    }
    let has_cwd = builder
        .expectations
        .iter()
        .any(|expectation| matches!(expectation, Expectation::Cwd(_) | Expectation::CwdNone));
    let has_cwd_unix = builder
        .expectations
        .iter()
        .any(|expectation| matches!(expectation, Expectation::CwdUnix(_)));
    let has_cwd_windows = builder
        .expectations
        .iter()
        .any(|expectation| matches!(expectation, Expectation::CwdWindows(_)));
    if has_cwd_unix != has_cwd_windows {
        return Err("expect-cwd-unix and expect-cwd-windows must be declared together".to_string());
    }
    if has_cwd && has_cwd_unix {
        return Err(
            "universal and platform-specific working-directory expectations cannot be mixed"
                .to_string(),
        );
    }
    let chunks = builder.chunks.unwrap_or_else(|| vec![payload.len()]);
    let total: usize = chunks.iter().sum();
    if total != payload.len() {
        return Err(format!(
            "chunk sizes sum to {total} but the payload is {} bytes; replay must be exact",
            payload.len()
        ));
    }
    for expectation in &builder.expectations {
        match expectation {
            Expectation::Cursor(row, column) if *row >= rows || *column >= columns => {
                return Err(format!(
                    "expect-cursor ({row}, {column}) lies outside the declared geometry"
                ));
            }
            Expectation::Line(row, _) if *row >= rows => {
                return Err(format!(
                    "expect-line row {row} lies outside the declared geometry"
                ));
            }
            _ => {}
        }
    }
    Ok(Case {
        id,
        columns,
        rows,
        chunks,
        expectations: builder.expectations,
        payload,
    })
}

#[test]
fn corpus_case_files_are_self_consistent() {
    // Structural gate: every file parses under the same rules the Python
    // validator enforces. A failure here means the corpus is malformed, not
    // that the product regressed.
    for path in corpus_files() {
        load_case(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    }
}

#[test]
fn corpus_cases_replay_as_declared() {
    for path in corpus_files() {
        let case = load_case(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let mut terminal = Terminal::new(case.columns, case.rows);
        let mut offset = 0;
        for size in &case.chunks {
            let end = offset + size;
            terminal.advance(&case.payload[offset..end]);
            offset = end;
        }
        assert_eq!(
            offset,
            case.payload.len(),
            "{}: chunks did not cover the payload exactly",
            case.id
        );

        let text = terminal.screen().plain_text();
        let lines: Vec<&str> = text.split('\n').collect();
        let host_output = terminal.take_host_output();
        for expectation in &case.expectations {
            match expectation {
                Expectation::Line(row, expected) => {
                    let actual = lines.get(*row).copied().unwrap_or("<missing row>");
                    assert_eq!(
                        actual,
                        expected.as_str(),
                        "{}: visible row {row} disagrees",
                        case.id
                    );
                }
                Expectation::Contains(needle) => assert!(
                    text.contains(needle.as_str()),
                    "{}: screen should contain {needle:?}, got {text:?}",
                    case.id
                ),
                Expectation::NotContains(needle) => assert!(
                    !text.contains(needle.as_str()),
                    "{}: screen should not contain {needle:?}, got {text:?}",
                    case.id
                ),
                Expectation::Cursor(row, column) => {
                    let cursor = terminal.screen().cursor();
                    assert_eq!(
                        (cursor.row, cursor.column),
                        (*row, *column),
                        "{}: cursor position disagrees",
                        case.id
                    );
                }
                Expectation::ScrollbackLen(expected) => assert_eq!(
                    terminal.screen().scrollback_len(),
                    *expected,
                    "{}: scrollback length disagrees",
                    case.id
                ),
                Expectation::HostOutput(expected) => assert_eq!(
                    &host_output, expected,
                    "{}: host-bound reply disagrees",
                    case.id
                ),
                Expectation::Cwd(expected) => assert_eq!(
                    terminal.screen().current_working_directory(),
                    Some(expected.as_str()),
                    "{}: reported working directory disagrees",
                    case.id
                ),
                Expectation::CwdUnix(expected) => {
                    if cfg!(unix) {
                        assert_eq!(
                            terminal.screen().current_working_directory(),
                            Some(expected.as_str()),
                            "{}: Unix reported working directory disagrees",
                            case.id
                        );
                    }
                }
                Expectation::CwdWindows(expected) => {
                    if cfg!(windows) {
                        assert_eq!(
                            terminal.screen().current_working_directory(),
                            Some(expected.as_str()),
                            "{}: Windows reported working directory disagrees",
                            case.id
                        );
                    }
                }
                Expectation::CwdNone => assert_eq!(
                    terminal.screen().current_working_directory(),
                    None,
                    "{}: no working directory should be reported",
                    case.id
                ),
            }
        }
    }
}
