// SPDX-License-Identifier: GPL-3.0-only
//! Pure `file://` URI construction shared by every producer.
//!
//! Two callers build `file://` URIs from an absolute path: the interactive-path
//! "Copy File" action / opener (`native::app::interactive_paths`) and the
//! Desktop-Entry `%u`/`%U` field-code expander (`desktop::exec`). Both used to
//! build the URI inline, and the Unix arms skipped percent-encoding entirely, so
//! a path with a space, `%`, control byte, or non-ASCII byte produced a
//! malformed URI. This module is the single encoder both route through.
//!
//! It lives under `src/paths/` (std-only, no windowing/GPU/native imports) so
//! `src/desktop/` — which by the SPEC layering rule must not reach into
//! `native/` — can use it without crossing that boundary.

/// Which path convention the absolute input follows. Unix paths are already
/// rooted at `/`; Windows paths use `\` separators and a `C:` drive prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UriOs {
    Unix,
    Windows,
}

/// Build the `file://<path>` URI for `abs`, percent-encoding every byte outside
/// the RFC 3986 unreserved set except the path separator `/` and the drive
/// colon `:` (which stay literal so path structure and `C:` survive).
///
/// * Unix: an absolute path already begins with `/`, so `file://` + the encoded
///   path yields the correct three-slash `file:///…` form.
/// * Windows: backslashes become forward slashes and a leading slash is inserted
///   before the drive letter so the drive is part of the path, not parsed as the
///   URL authority (`file://C:/…` would read `C:` as the host).
///
/// A path made only of safe bytes is byte-identical to the pre-encoding output,
/// so ordinary paths are unchanged; only unsafe bytes now encode.
pub fn file_uri(abs: &str, os: UriOs) -> String {
    match os {
        UriOs::Windows => {
            // UNC path `\\host\share\...`: the host is the URI authority, not
            // part of the path, matching PureWindowsPath.as_uri() and Windows'
            // own path-to-URL helpers. The drive-letter transform below would
            // otherwise render it as `file:////host/share/...` (four slashes,
            // empty authority, host folded into the path).
            if let Some(rest) = abs.strip_prefix(r"\\") {
                let slashed = rest.replace('\\', "/");
                let (host, path) = slashed.split_once('/').unwrap_or((slashed.as_str(), ""));
                return format!(
                    "file://{}/{}",
                    percent_encode_uri_path(host),
                    percent_encode_uri_path(path)
                );
            }
            let slashed = abs.replace('\\', "/");
            let rooted = if slashed.starts_with('/') {
                slashed
            } else {
                format!("/{slashed}")
            };
            format!("file://{}", percent_encode_uri_path(&rooted))
        }
        UriOs::Unix => format!("file://{}", percent_encode_uri_path(abs)),
    }
}

/// Percent-encode the bytes of a URI path, preserving the RFC 3986 unreserved
/// set plus the path/drive separators `/` and `:`. Every other byte (space,
/// `%`, control, non-ASCII UTF-8) is emitted as `%XX`. Pure.
pub fn percent_encode_uri_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_safe_path_is_unencoded() {
        assert_eq!(file_uri("/proj/a.rs", UriOs::Unix), "file:///proj/a.rs");
    }

    #[test]
    fn unix_unsafe_bytes_are_encoded() {
        // The pre-fix Unix arm emitted a raw space / `%` / control byte, making
        // the URI malformed. Every unsafe byte now percent-encodes; `/` stays.
        assert_eq!(
            file_uri("/my pictures/a b.png", UriOs::Unix),
            "file:///my%20pictures/a%20b.png"
        );
        assert_eq!(
            file_uri("/50%off/x.txt", UriOs::Unix),
            "file:///50%25off/x.txt"
        );
        // A control byte and a non-ASCII byte both encode.
        assert_eq!(
            file_uri("/a\x07/\u{e9}.txt", UriOs::Unix),
            "file:///a%07/%C3%A9.txt"
        );
    }

    #[test]
    fn windows_roots_drive_and_encodes() {
        assert_eq!(
            file_uri("C:\\proj\\a.rs", UriOs::Windows),
            "file:///C:/proj/a.rs"
        );
        assert_eq!(
            file_uri("C:\\my dir\\a b.rs", UriOs::Windows),
            "file:///C:/my%20dir/a%20b.rs"
        );
        assert_eq!(
            file_uri("C:\\50%off\\x.txt", UriOs::Windows),
            "file:///C:/50%25off/x.txt"
        );
    }

    #[test]
    fn windows_unc_host_becomes_authority() {
        // `\\host\share\file` -> host as authority, two slashes (not four).
        assert_eq!(
            file_uri(r"\\server\share\file.txt", UriOs::Windows),
            "file://server/share/file.txt"
        );
        // Percent-encoding still applies to the share/path segments.
        assert_eq!(
            file_uri(r"\\fileserver\builds\out log.txt", UriOs::Windows),
            "file://fileserver/builds/out%20log.txt"
        );
        // A bare host with no share still yields a two-slash authority form.
        assert_eq!(file_uri(r"\\server", UriOs::Windows), "file://server/");
    }

    #[test]
    fn encoder_preserves_slash_and_colon_only() {
        assert_eq!(percent_encode_uri_path("/a:b/c"), "/a:b/c");
        assert_eq!(percent_encode_uri_path("a#b?c"), "a%23b%3Fc");
    }
}
