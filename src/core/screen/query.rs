//! DCS query/reporting helpers for XTGETTCAP and DECRQSS.
//!
//! These protocols ride the same parser hook/put/unhook seam as graphics DCS
//! payloads but are terminal-core queries, so they live beside the screen state
//! machine instead of in the graphics router.

use super::*;

const MAX_DCS_QUERY_BYTES: usize = 4096;
const XTGETTCAP_TERM_NAME: &[u8] = b"xterm-256color";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DcsQueryCapture {
    XtGetTcap { payload: Vec<u8>, overflowed: bool },
    Decrqss { payload: Vec<u8>, overflowed: bool },
}

pub(super) fn dcs_query_hook(
    intermediates: &[u8],
    ignore: bool,
    action: char,
) -> Option<DcsQueryCapture> {
    if ignore || action != 'q' {
        return None;
    }
    match intermediates {
        b"+" => Some(DcsQueryCapture::XtGetTcap {
            payload: Vec::new(),
            overflowed: false,
        }),
        b"$" => Some(DcsQueryCapture::Decrqss {
            payload: Vec::new(),
            overflowed: false,
        }),
        _ => None,
    }
}

pub(super) fn dcs_query_put(capture: &mut DcsQueryCapture, byte: u8) {
    let (payload, overflowed) = match capture {
        DcsQueryCapture::XtGetTcap {
            payload,
            overflowed,
        }
        | DcsQueryCapture::Decrqss {
            payload,
            overflowed,
        } => (payload, overflowed),
    };

    if payload.len() < MAX_DCS_QUERY_BYTES {
        payload.push(byte);
    } else {
        *overflowed = true;
    }
}

impl Screen {
    pub(super) fn dispatch_dcs_query(&mut self, capture: DcsQueryCapture) {
        match capture {
            DcsQueryCapture::XtGetTcap {
                payload,
                overflowed,
            } => {
                if !overflowed {
                    self.xtgettcap_report(&payload);
                }
            }
            DcsQueryCapture::Decrqss {
                payload,
                overflowed,
            } => {
                if !overflowed {
                    self.decrqss_report(&payload);
                }
            }
        }
    }

    fn xtgettcap_report(&mut self, payload: &[u8]) {
        for name_hex in payload.split(|byte| *byte == b';') {
            let Some(name) = decode_ascii_hex(name_hex) else {
                continue;
            };
            let Some(value) = xtgettcap_value(&name) else {
                self.host_output.extend_from_slice(b"\x1bP0+r\x1b\\");
                continue;
            };

            self.host_output.extend_from_slice(b"\x1bP1+r");
            self.host_output.extend_from_slice(name_hex);
            self.host_output.push(b'=');
            encode_ascii_hex(value, &mut self.host_output);
            self.host_output.extend_from_slice(b"\x1b\\");
        }
    }

    fn decrqss_report(&mut self, selector: &[u8]) {
        let Some(value) = self.decrqss_value(selector) else {
            self.host_output.extend_from_slice(b"\x1bP0$r\x1b\\");
            return;
        };

        self.host_output.extend_from_slice(b"\x1bP1$r");
        self.host_output.extend_from_slice(value.as_bytes());
        self.host_output.extend_from_slice(b"\x1b\\");
    }

    fn decrqss_value(&self, selector: &[u8]) -> Option<String> {
        match selector {
            b"m" => Some(format!("{}m", sgr_state_params(self.current_attrs))),
            b" q" => Some(format!(
                "{} q",
                cursor_style_code(self.cursor_style, self.cursor_blink)
            )),
            b"\"q" => Some(format!("{}\"q", if self.current_protected { 1 } else { 0 })),
            b"r" => {
                let (top, bottom) = self.effective_region();
                Some(format!("{};{}r", top + 1, bottom + 1))
            }
            _ => None,
        }
    }
}

fn xtgettcap_value(name: &[u8]) -> Option<&'static [u8]> {
    match name {
        // Conservative truth set: term-name compatibility, 256-color indexed
        // palette, and direct RGB color. OdyTTY does not claim a full terminfo
        // database through XTGETTCAP.
        b"TN" => Some(XTGETTCAP_TERM_NAME),
        b"Co" => Some(b"256"),
        b"RGB" => Some(b"1"),
        _ => None,
    }
}

fn sgr_state_params(attrs: Attrs) -> String {
    let mut params = Vec::new();

    if attrs.bold() {
        params.push("1".to_string());
    }
    if attrs.dim() {
        params.push("2".to_string());
    }
    if attrs.italic() {
        params.push("3".to_string());
    }
    match attrs.effective_underline_style() {
        UnderlineStyle::None => {}
        UnderlineStyle::Straight => params.push("4".to_string()),
        UnderlineStyle::Double => params.push("4:2".to_string()),
        UnderlineStyle::Curly => params.push("4:3".to_string()),
        UnderlineStyle::Dotted => params.push("4:4".to_string()),
        UnderlineStyle::Dashed => params.push("4:5".to_string()),
    }
    if attrs.blink() {
        params.push("5".to_string());
    }
    if attrs.inverse() {
        params.push("7".to_string());
    }
    if attrs.hidden() {
        params.push("8".to_string());
    }
    if attrs.strikethrough() {
        params.push("9".to_string());
    }
    push_color_params(&mut params, 38, attrs.foreground);
    push_color_params(&mut params, 48, attrs.background);
    if let Some(color) = attrs.underline_color {
        push_color_params(&mut params, 58, color);
    }

    if params.is_empty() {
        "0".to_string()
    } else {
        params.join(";")
    }
}

fn push_color_params(params: &mut Vec<String>, selector: u16, color: Color) {
    match color {
        Color::Default => {}
        Color::Indexed(index) => {
            if selector == 38 && index < 8 {
                params.push((30 + index).to_string());
            } else if selector == 48 && index < 8 {
                params.push((40 + index).to_string());
            } else if selector == 38 && index < 16 {
                params.push((90 + index - 8).to_string());
            } else if selector == 48 && index < 16 {
                params.push((100 + index - 8).to_string());
            } else {
                params.push(format!("{selector}:5:{index}"));
            }
        }
        Color::Rgb(red, green, blue) => {
            params.push(format!("{selector}:2::{red}:{green}:{blue}"));
        }
    }
}

fn cursor_style_code(style: CursorStyle, blink: bool) -> u8 {
    match (style, blink) {
        (CursorStyle::Block, true) => 1,
        (CursorStyle::Block, false) => 2,
        (CursorStyle::Underline, true) => 3,
        (CursorStyle::Underline, false) => 4,
        (CursorStyle::Bar, true) => 5,
        (CursorStyle::Bar, false) => 6,
    }
}

fn decode_ascii_hex(hex: &[u8]) -> Option<Vec<u8>> {
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }

    let mut decoded = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn encode_ascii_hex(bytes: &[u8], out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_hex_names_are_dropped() {
        assert_eq!(decode_ascii_hex(b"544e").as_deref(), Some(b"TN".as_slice()));
        assert!(decode_ascii_hex(b"544").is_none());
        assert!(decode_ascii_hex(b"54xx").is_none());
    }

    #[test]
    fn sgr_state_defaults_to_reset() {
        assert_eq!(sgr_state_params(Attrs::default()), "0");
    }
}
