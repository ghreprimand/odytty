// SPDX-License-Identifier: GPL-3.0-only
//! OSC (Operating System Command) payload parsing helpers, split out of the
//! parent module for the modularity cap: OSC string reassembly, OSC 7
//! working-directory URL decoding, OSC 52 clipboard selection parsing, xterm
//! `rgb:` color parse/format, and the indexed-color sRGB table. These are pure
//! free functions moved verbatim from `super` (private `fn` widened to
//! `pub(super)` so the parent module can still call them).

use super::*;

/// Reassemble an OSC string payload (everything after the numeric selector)
/// into text. The parser splits OSC on `;`, so a title containing a semicolon
/// arrives as multiple parts; rejoin them with `;` to recover it. Invalid UTF-8
/// is replaced rather than rejected so a malformed title can never panic or
/// desync the parser. An empty payload yields an empty string.
pub(super) fn osc_string(parts: &[&[u8]]) -> String {
    let mut bytes = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            bytes.push(b';');
        }
        bytes.extend_from_slice(part);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Parse an OSC 7 payload (`file://host/path`) into a working-directory path.
///
/// Returns the percent-decoded path on success, or `None` when the OSC 7 should
/// be ignored (the working directory is then left unchanged). The parser never
/// panics, never emits a response, and never touches the filesystem.
///
/// ## Hostname policy
///
/// An empty host or `localhost` (ASCII case-insensitive) is always accepted. A
/// front end may also inject the local hostname; when present, an OSC 7 host
/// matching that name is accepted too. Matching is case-insensitive and tolerant
/// of short-vs-FQDN forms by comparing both full names and the leading label
/// before the first `.`. Any other host causes the OSC 7 to be ignored.
/// Rationale: a `file://` URL with a foreign host names a path on *another*
/// machine. The core cannot resolve hostnames itself (it stays deterministic
/// and filesystem-free), so the local name is live front-end config, not
/// serialized terminal state.
///
/// ## Robustness policy
///
/// - Non-`file://` URLs are ignored (OdyTTY tracks file URLs only).
/// - A missing path (no `/` after the authority) is ignored.
/// - A malformed percent-escape (`%` without two following hex digits, or a
///   trailing/truncated `%`) ignores the whole OSC 7 rather than guessing.
/// - A decoded NUL byte (`%00`) is rejected: NUL can never appear in a valid
///   path and accepting it risks truncation bugs downstream.
/// - Surviving non-UTF-8 bytes are replaced lossily so a malformed path can
///   never desync the parser.
pub(super) fn parse_osc7_cwd(parts: &[&[u8]], local_hostname: Option<&str>) -> Option<String> {
    // Rejoin on ';' to recover a URL whose path contains a semicolon (the OSC
    // parser splits payloads on ';'). Work on raw bytes so percent-decoding
    // sees the exact wire form before any UTF-8 interpretation.
    let mut raw = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            raw.push(b';');
        }
        raw.extend_from_slice(part);
    }

    // Require the file:// scheme (scheme is case-insensitive per RFC 3986).
    const SCHEME: &[u8] = b"file://";
    if raw.len() < SCHEME.len() || !raw[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let after_scheme = &raw[SCHEME.len()..];

    // Authority runs up to the first '/', which also begins the path. A URL
    // with no path component carries no directory and is ignored.
    let slash = after_scheme.iter().position(|&b| b == b'/')?;
    let host = &after_scheme[..slash];
    let path = &after_scheme[slash..];

    if !osc7_host_is_local(host, local_hostname) {
        return None;
    }

    let decoded = percent_decode_path(path)?;
    let cwd = String::from_utf8_lossy(&decoded).into_owned();
    #[cfg(windows)]
    {
        return Some(strip_leading_drive_slash(cwd));
    }
    Some(cwd)
}

#[cfg(any(windows, test))]
fn strip_leading_drive_slash(path: String) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'\\' || bytes[3] == b'/')
    {
        return path[1..].to_owned();
    }
    path
}

fn osc7_host_is_local(host: &[u8], local_hostname: Option<&str>) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case(b"localhost") {
        return true;
    }
    let Ok(host) = std::str::from_utf8(host) else {
        return false;
    };
    let Some(local_hostname) = local_hostname.filter(|hostname| !hostname.is_empty()) else {
        return false;
    };
    hostname_matches(host, local_hostname)
}

fn hostname_matches(host: &str, local_hostname: &str) -> bool {
    if host.eq_ignore_ascii_case(local_hostname) {
        return true;
    }
    let host_label = leading_label(host);
    let local_label = leading_label(local_hostname);
    !host_label.is_empty()
        && !local_label.is_empty()
        && host_label.eq_ignore_ascii_case(local_label)
}

fn leading_label(host: &str) -> &str {
    host.split_once('.').map(|(label, _)| label).unwrap_or(host)
}

/// Percent-decode a path's bytes. Returns `None` on a malformed escape or a
/// decoded NUL byte (see [`parse_osc7_cwd`] robustness policy).
pub(super) fn percent_decode_path(path: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        let byte = path[i];
        if byte == b'%' {
            // Need exactly two hex digits following the '%'.
            let hi = path.get(i + 1).and_then(hex_value)?;
            let lo = path.get(i + 2).and_then(hex_value)?;
            let decoded = (hi << 4) | lo;
            if decoded == 0 {
                return None;
            }
            out.push(decoded);
            i += 3;
        } else {
            out.push(byte);
            i += 1;
        }
    }
    Some(out)
}

pub(super) fn hex_value(byte: &u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(super) fn osc52_selections(raw: &[u8]) -> Option<Vec<ClipboardSelection>> {
    if raw.is_empty() {
        return Some(vec![ClipboardSelection::Clipboard]);
    }
    let mut selections = Vec::new();
    for &byte in raw {
        let selection = match byte {
            b'c' => ClipboardSelection::Clipboard,
            b'p' => ClipboardSelection::Primary,
            _ => continue,
        };
        if !selections.contains(&selection) {
            selections.push(selection);
        }
    }
    (!selections.is_empty()).then_some(selections)
}

pub(super) fn osc52_selection_bytes(selection: ClipboardSelection) -> &'static [u8] {
    match selection {
        ClipboardSelection::Clipboard => b"c",
        ClipboardSelection::Primary => b"p",
    }
}

pub(super) fn parse_xterm_rgb(raw: &[u8]) -> Option<RgbColor> {
    let raw = std::str::from_utf8(raw).ok()?;
    let components = raw.strip_prefix("rgb:")?;
    let mut parts = components.split('/');
    let red = parse_xterm_rgb_component(parts.next()?)?;
    let green = parse_xterm_rgb_component(parts.next()?)?;
    let blue = parse_xterm_rgb_component(parts.next()?)?;
    parts
        .next()
        .is_none()
        .then(|| RgbColor::new(red, green, blue))
}

pub(super) fn parse_xterm_rgb_component(component: &str) -> Option<u8> {
    if component.is_empty() || component.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(component, 16).ok()?;
    let max = (1u32 << (component.len() * 4)) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

pub(super) fn format_xterm_rgb(color: RgbColor) -> String {
    format!(
        "rgb:{:04x}/{:04x}/{:04x}",
        color.red as u16 * 257,
        color.green as u16 * 257,
        color.blue as u16 * 257
    )
}

pub(super) fn default_color_osc_code(slot: DefaultColorSlot) -> &'static [u8] {
    match slot {
        DefaultColorSlot::Foreground => b"10",
        DefaultColorSlot::Background => b"11",
        DefaultColorSlot::Cursor => b"12",
    }
}

pub(super) fn indexed_srgb(index: u8) -> RgbColor {
    let (red, green, blue) = match index {
        0 => (0x00, 0x00, 0x00),
        1 => (0xCD, 0x00, 0x00),
        2 => (0x00, 0xCD, 0x00),
        3 => (0xCD, 0xCD, 0x00),
        4 => (0x00, 0x00, 0xEE),
        5 => (0xCD, 0x00, 0xCD),
        6 => (0x00, 0xCD, 0xCD),
        7 => (0xE5, 0xE5, 0xE5),
        8 => (0x7F, 0x7F, 0x7F),
        9 => (0xFF, 0x00, 0x00),
        10 => (0x00, 0xFF, 0x00),
        11 => (0xFF, 0xFF, 0x00),
        12 => (0x5C, 0x5C, 0xFF),
        13 => (0xFF, 0x00, 0xFF),
        14 => (0x00, 0xFF, 0xFF),
        15 => (0xFF, 0xFF, 0xFF),
        16..=231 => {
            let i = index - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let level = |v: u8| -> u8 { if v == 0 { 0 } else { 55 + v * 40 } };
            (level(r), level(g), level(b))
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    };
    RgbColor::new(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::strip_leading_drive_slash;

    #[test]
    fn strip_leading_drive_slash_normalizes_file_url_drive_paths() {
        for (input, expected) in [
            ("/C:/Users/x", "C:/Users/x"),
            ("/c:/x", "c:/x"),
            ("/C:\\x", "C:\\x"),
            ("/C:", "C:"),
            ("C:/x", "C:/x"),
            ("/tmp/x", "/tmp/x"),
            ("/", "/"),
            ("", ""),
        ] {
            assert_eq!(
                strip_leading_drive_slash(input.to_owned()),
                expected,
                "{input}"
            );
        }
    }
}
