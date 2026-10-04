// SPDX-License-Identifier: GPL-3.0-only
//! Scrollback export document: a pane's scrollback plus live screen as logical
//! lines, rendered either as plain UTF-8 text or as one sanitized,
//! self-contained HTML file.
//!
//! The document is built from terminal cells only. It never reads the working
//! directory, host, user, profile, title, environment, or any other terminal
//! metadata, so an export carries only text that was in the grid. Inline
//! images (Kitty, iTerm2, and sixel placements and Kitty Unicode placeholder
//! cells) become one `[image]` line; image bytes are never embedded.
//!
//! The HTML form holds one `<style>` block, a Content-Security-Policy that
//! forbids every fetch and script, escaped text, and a bounded inline style per
//! span: a color from the theme's 16 ANSI colors, the xterm 256-color table,
//! or an explicit RGB cell color, plus bold, dim, italic, underline, and
//! strike classes. It has no `<script>`, event handler, `<iframe>`, `<object>`,
//! `<embed>`, `<img>`, external stylesheet, or font. OSC 8 hyperlinks become
//! `<a href>` only for `http` and `https` URLs with a host and no embedded
//! credentials; every other target, including `file:` and `javascript:`,
//! stays plain text.
//!
//! Both forms are encoded line by line as rows arrive, under a byte cap that
//! the output never passes: an export over the cap is refused whole, never
//! truncated, and never materialized first as a full line list or string.
//! Writing is not done here: both forms go through the shared private atomic
//! writer in [`super::command_export`] under its 32 MiB cap.

use super::command_export::CommandExportError;
use crate::core::{Cell, Color, LinkId, PLACEHOLDER_CHAR};
use crate::theme::Srgb;

/// The line that stands in for an inline image.
pub(super) const IMAGE_PLACEHOLDER: &str = "[image]";

/// Longest hyperlink target emitted as an `href`; longer targets stay text.
const MAX_HREF_BYTES: usize = 2048;

/// Title of the generated HTML page. Fixed, so no terminal title or path
/// reaches the file.
const HTML_TITLE: &str = "OdyTTY scrollback";

/// The two export forms offered in the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollbackFormat {
    PlainText,
    Html,
}

impl ScrollbackFormat {
    /// Application-owned neutral filename suggested by the save dialog.
    pub(super) fn suggested_filename(self) -> &'static str {
        match self {
            Self::PlainText => "scrollback.txt",
            Self::Html => "scrollback.html",
        }
    }

    /// Save-dialog filter label and extensions.
    pub(super) fn filter(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::PlainText => ("Plain text", &["txt"]),
            Self::Html => ("HTML", &["html", "htm"]),
        }
    }
}

/// One logical line of the export.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ExportLine {
    /// An inline image anchored in this part of the scrollback.
    Image,
    /// Text runs with trailing blanks trimmed.
    Text(Vec<ExportSpan>),
}

/// A run of text sharing one style and one hyperlink target.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ExportSpan {
    pub(super) text: String,
    pub(super) style: SpanStyle,
    /// Raw OSC 8 target. Only the HTML form reads it, and only after
    /// [`safe_http_href`] accepts it.
    pub(super) link: Option<String>,
}

/// The subset of cell attributes the HTML form renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct SpanStyle {
    pub(super) foreground: Color,
    pub(super) background: Color,
    pub(super) bold: bool,
    pub(super) dim: bool,
    pub(super) italic: bool,
    pub(super) underline: bool,
    pub(super) strike: bool,
    pub(super) inverse: bool,
}

impl SpanStyle {
    fn from_cell(cell: &Cell) -> Self {
        let attrs = &cell.attrs;
        Self {
            foreground: attrs.foreground,
            background: attrs.background,
            bold: attrs.bold(),
            dim: attrs.dim(),
            italic: attrs.italic(),
            underline: attrs.underline(),
            strike: attrs.strikethrough(),
            inverse: attrs.inverse(),
        }
    }
}

/// The colors the HTML form resolves `Color::Default` and the 16 ANSI
/// indices against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExportPalette {
    pub(super) foreground: Srgb,
    pub(super) background: Srgb,
    pub(super) ansi: [Srgb; 16],
}

impl ExportPalette {
    pub(super) fn from_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            foreground: theme.foreground,
            background: theme.background,
            ansi: theme.palette,
        }
    }

    fn resolve(&self, color: Color, default: Srgb) -> Srgb {
        match color {
            Color::Default => default,
            Color::Indexed(index) if index < 16 => self.ansi[usize::from(index)],
            Color::Indexed(index) => crate::text::indexed_srgb(index),
            Color::Rgb(red, green, blue) => (red, green, blue),
        }
    }
}

/// Receives finished logical lines from a [`LineAssembler`].
trait LineSink {
    /// Largest number of text bytes the next line may hold before the export
    /// certainly exceeds its cap. Text bytes never exceed encoded bytes, so a
    /// line over this budget is refused without being encoded.
    fn line_budget(&self) -> usize;
    fn emit(&mut self, line: ExportLine) -> Result<(), CommandExportError>;
}

/// Turns physical rows, in reading order, into logical lines, trimming each
/// line's trailing blanks. Trailing whitespace is held apart from the
/// committed text until a later non-blank character proves it interior, so a
/// line that would exceed the sink's budget is refused as soon as its
/// committed text does, and a long blank tail never grows past that budget.
#[derive(Debug, Default)]
struct LineAssembler {
    current: Vec<ExportSpan>,
    committed_bytes: usize,
    pending: Vec<ExportSpan>,
    pending_bytes: usize,
    /// The blank tail outgrew the budget and was dropped: any later
    /// non-blank character on this line makes the export too large.
    pending_overflow: bool,
    image_in_line: bool,
}

impl LineAssembler {
    fn push_row(
        &mut self,
        cells: &[Cell],
        wrapped: bool,
        link_uri: &mut impl FnMut(LinkId) -> Option<String>,
        sink: &mut impl LineSink,
    ) -> Result<(), CommandExportError> {
        let budget = sink.line_budget();
        for cell in cells
            .iter()
            .filter(|cell| !cell.wide_continuation && !cell.layout_padding)
        {
            if cell.ch == PLACEHOLDER_CHAR {
                self.image_in_line = true;
                continue;
            }
            let style = SpanStyle::from_cell(cell);
            let link = cell.attrs.hyperlink.and_then(&mut *link_uri);
            for ch in crate::selection::cell_grapheme_chars(cell) {
                self.push_char(ch, style, &link, budget)?;
            }
        }
        if !wrapped {
            self.end_line(sink)?;
        }
        Ok(())
    }

    fn push_char(
        &mut self,
        ch: char,
        style: SpanStyle,
        link: &Option<String>,
        budget: usize,
    ) -> Result<(), CommandExportError> {
        if ch.is_whitespace() {
            if self.pending_overflow {
                return Ok(());
            }
            self.pending_bytes = self.pending_bytes.saturating_add(ch.len_utf8());
            if self.committed_bytes.saturating_add(self.pending_bytes) > budget {
                self.pending = Vec::new();
                self.pending_bytes = 0;
                self.pending_overflow = true;
            } else {
                append_char(&mut self.pending, ch, style, link);
            }
            return Ok(());
        }
        let incoming = self.pending_bytes.saturating_add(ch.len_utf8());
        if self.pending_overflow || self.committed_bytes.saturating_add(incoming) > budget {
            return Err(CommandExportError::TooLarge);
        }
        for span in self.pending.drain(..) {
            append_span(&mut self.current, span);
        }
        self.committed_bytes += incoming;
        self.pending_bytes = 0;
        append_char(&mut self.current, ch, style, link);
        Ok(())
    }

    fn end_line(&mut self, sink: &mut impl LineSink) -> Result<(), CommandExportError> {
        self.pending.clear();
        self.pending_bytes = 0;
        self.pending_overflow = false;
        self.committed_bytes = 0;
        let spans = std::mem::take(&mut self.current);
        if std::mem::take(&mut self.image_in_line) {
            sink.emit(ExportLine::Image)?;
            if spans.is_empty() {
                return Ok(());
            }
        }
        sink.emit(ExportLine::Text(spans))
    }

    /// Close a line left open by a final wrapped row.
    fn finish(&mut self, sink: &mut impl LineSink) -> Result<(), CommandExportError> {
        if !self.current.is_empty()
            || !self.pending.is_empty()
            || self.pending_overflow
            || self.image_in_line
        {
            self.end_line(sink)?;
        }
        Ok(())
    }
}

fn append_char(spans: &mut Vec<ExportSpan>, ch: char, style: SpanStyle, link: &Option<String>) {
    match spans.last_mut() {
        Some(span) if span.style == style && span.link == *link => span.text.push(ch),
        _ => spans.push(ExportSpan {
            text: ch.to_string(),
            style,
            link: link.clone(),
        }),
    }
}

fn append_span(spans: &mut Vec<ExportSpan>, span: ExportSpan) {
    match spans.last_mut() {
        Some(last) if last.style == span.style && last.link == span.link => {
            last.text.push_str(&span.text);
        }
        _ => spans.push(span),
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct CollectLines(Vec<ExportLine>);

#[cfg(test)]
impl LineSink for CollectLines {
    fn line_budget(&self) -> usize {
        usize::MAX
    }

    fn emit(&mut self, line: ExportLine) -> Result<(), CommandExportError> {
        self.0.push(line);
        Ok(())
    }
}

#[cfg(test)]
/// Accumulates physical rows, in reading order, into an unbounded list of
/// logical lines. The export itself uses [`BoundedDocument`], which encodes
/// each line as it closes; this test-only form, with [`plain_text`] and
/// [`html_document`], exposes the line model through the same assembler and
/// encoder.
#[derive(Debug, Default)]
pub(super) struct DocumentBuilder {
    assembler: LineAssembler,
    lines: CollectLines,
}

#[cfg(test)]
impl DocumentBuilder {
    /// Mark the logical line that holds the next pushed row as containing an
    /// image anchor (a Kitty, iTerm2, or sixel placement).
    pub(super) fn mark_image(&mut self) {
        self.assembler.image_in_line = true;
    }

    /// Append one physical row. `wrapped` continues the logical line onto the
    /// next row; otherwise the line ends here. `link_uri` resolves an OSC 8
    /// link id to its target.
    pub(super) fn push_row(
        &mut self,
        cells: &[Cell],
        wrapped: bool,
        link_uri: &mut impl FnMut(LinkId) -> Option<String>,
    ) {
        // An unbounded sink never refuses.
        let _ = self
            .assembler
            .push_row(cells, wrapped, link_uri, &mut self.lines);
    }

    /// Close the last line and drop the blank rows below the last content
    /// (the unused part of the live screen).
    pub(super) fn finish(mut self) -> Vec<ExportLine> {
        let _ = self.assembler.finish(&mut self.lines);
        let mut lines = self.lines.0;
        while matches!(lines.last(), Some(ExportLine::Text(spans)) if spans.is_empty()) {
            lines.pop();
        }
        lines
    }
}

/// Builds one export file from physical rows under a byte cap. Each logical
/// line is encoded as soon as it closes, so the document is never held as a
/// line list, and the output never grows past the cap: an export that would
/// exceed it is refused whole with [`CommandExportError::TooLarge`], never
/// truncated.
pub(super) struct BoundedDocument {
    assembler: LineAssembler,
    encoder: Encoder,
}

impl BoundedDocument {
    /// Start a document in `format` whose encoded size may not exceed `limit`
    /// bytes. `palette` is read only by the HTML form.
    pub(super) fn new(
        format: ScrollbackFormat,
        palette: &ExportPalette,
        limit: usize,
    ) -> Result<Self, CommandExportError> {
        let html = (format == ScrollbackFormat::Html).then_some(*palette);
        Ok(Self {
            assembler: LineAssembler::default(),
            encoder: Encoder::new(html, limit)?,
        })
    }

    /// Mark the logical line that holds the next pushed row as containing an
    /// image anchor (a Kitty, iTerm2, or sixel placement).
    pub(super) fn mark_image(&mut self) {
        self.assembler.image_in_line = true;
    }

    /// Append one physical row. `wrapped` continues the logical line onto the
    /// next row; otherwise the line ends here. `link_uri` resolves an OSC 8
    /// link id to its target. Fails with `TooLarge` as soon as the export
    /// certainly exceeds the cap.
    pub(super) fn push_row(
        &mut self,
        cells: &[Cell],
        wrapped: bool,
        link_uri: &mut impl FnMut(LinkId) -> Option<String>,
    ) -> Result<(), CommandExportError> {
        self.assembler
            .push_row(cells, wrapped, link_uri, &mut self.encoder)
    }

    /// Close the last line, drop trailing blank lines, and return the file.
    pub(super) fn finish(mut self) -> Result<String, CommandExportError> {
        self.assembler.finish(&mut self.encoder)?;
        self.encoder.finish()
    }
}

/// Streams logical lines into plain text or sanitized HTML, never letting the
/// output exceed `limit` bytes. Blank lines are deferred until a later
/// non-blank line proves them interior, so trailing blank rows cost nothing.
struct Encoder {
    /// `Some` for the HTML form.
    html: Option<ExportPalette>,
    out: String,
    limit: usize,
    blank_lines: usize,
}

impl Encoder {
    fn new(html: Option<ExportPalette>, limit: usize) -> Result<Self, CommandExportError> {
        let mut encoder = Self {
            html,
            out: String::new(),
            limit,
            blank_lines: 0,
        };
        if let Some(palette) = html {
            encoder.html_header(&palette)?;
        }
        Ok(encoder)
    }

    /// Make room for `extra` more bytes, or refuse when they would pass the
    /// cap. Capacity grows geometrically but never past the cap.
    fn reserve(&mut self, extra: usize) -> Result<(), CommandExportError> {
        let needed = self
            .out
            .len()
            .checked_add(extra)
            .filter(|needed| *needed <= self.limit)
            .ok_or(CommandExportError::TooLarge)?;
        if needed > self.out.capacity() {
            let target = needed
                .max(self.out.capacity().saturating_mul(2))
                .max(4096)
                .min(self.limit);
            self.out.reserve_exact(target - self.out.len());
        }
        Ok(())
    }

    fn put(&mut self, text: &str) -> Result<(), CommandExportError> {
        self.reserve(text.len())?;
        self.out.push_str(text);
        Ok(())
    }

    fn put_char(&mut self, ch: char) -> Result<(), CommandExportError> {
        self.reserve(ch.len_utf8())?;
        self.out.push(ch);
        Ok(())
    }

    /// Escape text for HTML element and attribute content. Control
    /// characters, which HTML does not allow, become U+FFFD.
    fn put_escaped(&mut self, text: &str) -> Result<(), CommandExportError> {
        for ch in text.chars() {
            match ch {
                '&' => self.put("&amp;")?,
                '<' => self.put("&lt;")?,
                '>' => self.put("&gt;")?,
                '"' => self.put("&quot;")?,
                '\'' => self.put("&#39;")?,
                ch if ch.is_control() => self.put_char('\u{FFFD}')?,
                ch => self.put_char(ch)?,
            }
        }
        Ok(())
    }

    fn flush_blank_lines(&mut self) -> Result<(), CommandExportError> {
        let count = std::mem::take(&mut self.blank_lines);
        self.reserve(count)?;
        self.out.extend(std::iter::repeat_n('\n', count));
        Ok(())
    }

    fn encode(&mut self, line: &ExportLine) -> Result<(), CommandExportError> {
        if matches!(line, ExportLine::Text(spans) if spans.is_empty()) {
            self.blank_lines = self.blank_lines.saturating_add(1);
            return Ok(());
        }
        self.flush_blank_lines()?;
        match (line, self.html) {
            (ExportLine::Image, None) => self.put(IMAGE_PLACEHOLDER)?,
            (ExportLine::Image, Some(_)) => {
                self.put("<span class=\"img\">")?;
                self.put(IMAGE_PLACEHOLDER)?;
                self.put("</span>")?;
            }
            (ExportLine::Text(spans), None) => {
                for span in spans {
                    self.put(&span.text)?;
                }
            }
            (ExportLine::Text(spans), Some(palette)) => {
                for span in spans {
                    self.html_span(span, &palette)?;
                }
            }
        }
        self.put_char('\n')
    }

    /// Drop deferred trailing blank lines and close the document.
    fn finish(mut self) -> Result<String, CommandExportError> {
        self.blank_lines = 0;
        if self.html.is_some() {
            self.put("</pre>\n</body>\n</html>\n")?;
        }
        Ok(self.out)
    }

    fn html_header(&mut self, palette: &ExportPalette) -> Result<(), CommandExportError> {
        self.put("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n")?;
        self.put(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
             style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\n",
        )?;
        self.put("<meta name=\"referrer\" content=\"no-referrer\">\n<title>")?;
        self.put(HTML_TITLE)?;
        self.put("</title>\n<style>\n")?;
        self.put(&format!(
            "body{{margin:0;background:{};color:{}}}\n",
            hex(palette.background),
            hex(palette.foreground)
        ))?;
        self.put(
            "pre{margin:0;padding:1em;font-family:monospace;white-space:pre-wrap;\
             overflow-wrap:anywhere}\n\
             a{color:inherit}\n.b{font-weight:bold}\n.d{opacity:.6}\n.i{font-style:italic}\n\
             .u{text-decoration:underline}\n.s{text-decoration:line-through}\n\
             .u.s{text-decoration:underline line-through}\n.img{opacity:.6}\n",
        )?;
        self.put("</style>\n</head>\n<body>\n<pre>")
    }

    fn html_span(
        &mut self,
        span: &ExportSpan,
        palette: &ExportPalette,
    ) -> Result<(), CommandExportError> {
        let style = span.style;
        let mut foreground = palette.resolve(style.foreground, palette.foreground);
        let mut background = palette.resolve(style.background, palette.background);
        if style.inverse {
            std::mem::swap(&mut foreground, &mut background);
        }
        let classes: Vec<&str> = [
            (style.bold, "b"),
            (style.dim, "d"),
            (style.italic, "i"),
            (style.underline, "u"),
            (style.strike, "s"),
        ]
        .into_iter()
        .filter_map(|(on, class)| on.then_some(class))
        .collect();
        let mut css = Vec::new();
        if foreground != palette.foreground {
            css.push(format!("color:{}", hex(foreground)));
        }
        if background != palette.background {
            css.push(format!("background:{}", hex(background)));
        }
        let href = span.link.as_deref().and_then(safe_http_href);
        if let Some(href) = href {
            self.put("<a href=\"")?;
            self.put_escaped(href)?;
            self.put("\" rel=\"noopener noreferrer nofollow\">")?;
        }
        let styled = !classes.is_empty() || !css.is_empty();
        if styled {
            self.put("<span")?;
            if !classes.is_empty() {
                self.put(" class=\"")?;
                self.put(&classes.join(" "))?;
                self.put("\"")?;
            }
            if !css.is_empty() {
                self.put(" style=\"")?;
                self.put(&css.join(";"))?;
                self.put("\"")?;
            }
            self.put(">")?;
        }
        self.put_escaped(&span.text)?;
        if styled {
            self.put("</span>")?;
        }
        if href.is_some() {
            self.put("</a>")?;
        }
        Ok(())
    }
}

impl LineSink for Encoder {
    fn line_budget(&self) -> usize {
        self.limit
            .saturating_sub(self.out.len())
            .saturating_sub(self.blank_lines)
    }

    fn emit(&mut self, line: ExportLine) -> Result<(), CommandExportError> {
        self.encode(&line)
    }
}

#[cfg(test)]
/// Encode an already-built line list, keeping every given line (including
/// trailing blank ones).
fn encode_lines(lines: &[ExportLine], html: Option<ExportPalette>) -> String {
    let encode = || -> Result<String, CommandExportError> {
        let mut encoder = Encoder::new(html, usize::MAX)?;
        for line in lines {
            encoder.encode(line)?;
        }
        encoder.flush_blank_lines()?;
        encoder.finish()
    };
    // An unbounded encoder never refuses.
    encode().unwrap_or_default()
}

#[cfg(test)]
/// The plain-text form: one line per logical line, `\n` endings, images as
/// `[image]`. An empty document yields an empty string.
pub(super) fn plain_text(lines: &[ExportLine]) -> String {
    encode_lines(lines, None)
}

#[cfg(test)]
/// The sanitized self-contained HTML form.
pub(super) fn html_document(lines: &[ExportLine], palette: &ExportPalette) -> String {
    encode_lines(lines, Some(*palette))
}

fn hex((red, green, blue): Srgb) -> String {
    format!("#{red:02x}{green:02x}{blue:02x}")
}

/// Accept a hyperlink target for an `href` only when it is an `http` or
/// `https` URL with a non-empty host, no userinfo (credentials), no
/// whitespace, control, quote, backslash, backtick, or angle-bracket
/// characters, and at most [`MAX_HREF_BYTES`] bytes. Every other scheme,
/// including `file` and `javascript`, is refused and stays plain text.
pub(super) fn safe_http_href(uri: &str) -> Option<&str> {
    if uri.is_empty() || uri.len() > MAX_HREF_BYTES {
        return None;
    }
    if uri.chars().any(|ch| {
        ch.is_control() || ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '\\' | '`')
    }) {
        return None;
    }
    let (scheme, rest) = uri.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest
        .strip_prefix("//")?
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') {
        return None;
    }
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    (!host.is_empty()).then_some(uri)
}

#[cfg(test)]
#[path = "scrollback_export_tests.rs"]
mod tests;
