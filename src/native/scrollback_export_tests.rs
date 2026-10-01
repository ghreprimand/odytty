// SPDX-License-Identifier: GPL-3.0-only
//! Scrollback export document, plain-text, and HTML sanitizer unit tests.

use super::*;
use crate::core::Attrs;

/// Mutation: bypass `safe_http_href` or `put_escaped` in `html_span`.
/// Every rejected scheme must remain visible text without active markup.
#[test]
fn phase9_mixed_image_and_link_document_stays_inert() {
    let mut lines = vec![text_line("header"), ExportLine::Image];
    for target in [
        "javascript:alert(1)",
        "data:text/html,<script>run()</script>",
        "file:///synthetic/archive",
        "https://example.invalid/\u{1b}payload",
        "https://example.invalid/\"onclick=\"run()",
    ] {
        lines.push(ExportLine::Text(vec![ExportSpan {
            text: "<&\"\u{0}\u{7}\u{1b}>".to_owned(),
            style: SpanStyle::default(),
            link: Some(target.to_owned()),
        }]));
    }
    lines.push(ExportLine::Text(vec![ExportSpan {
        text: "safe label".to_owned(),
        style: SpanStyle::default(),
        link: Some("https://example.invalid/a?x=1&y=2".to_owned()),
    }]));
    let html = html_document(&lines, &palette());
    assert_eq!(html.matches("<a href=").count(), 1);
    assert!(html.contains("https://example.invalid/a?x=1&amp;y=2"));
    assert_eq!(html.matches("[image]").count(), 1);
    assert_eq!(html.matches('\u{FFFD}').count(), 15);
    assert!(html.contains("&lt;&amp;&quot;"));
    for forbidden in ["<img", "<script", "javascript:", "data:text", "file:"] {
        assert!(!html.contains(forbidden), "active content: {forbidden}");
    }
    assert!(!html.chars().any(|ch| ch.is_control() && ch != '\n'));
    let plain = plain_text(&lines);
    assert_eq!(plain.matches("[image]").count(), 1);
    assert!(!plain.contains("example.invalid"));
}

/// Mutation: change the encoder's byte check to character count, or omit
/// HTML suffix bytes from `reserve`. Refusal must include the whole document.
#[test]
fn phase9_multibyte_and_image_markup_use_encoded_byte_limits() {
    let lines = vec![text_line("\u{1f680}<&"), ExportLine::Image];
    for html in [None, Some(palette())] {
        let expected = encode_lines(&lines, html);
        for limit in [expected.len(), expected.len() - 1] {
            let result = (|| {
                let mut encoder = Encoder::new(html, limit)?;
                for line in &lines {
                    encoder.encode(line)?;
                }
                encoder.finish()
            })();
            if limit == expected.len() {
                assert_eq!(result, Ok(expected.clone()));
            } else {
                assert_eq!(result, Err(CommandExportError::TooLarge));
            }
        }
    }
}

/// Mutation: remove `validate_text` from the shared atomic writer. An
/// oversized export must never replace an existing destination on any OS.
#[test]
fn phase9_32_mib_refusal_keeps_the_existing_export_unchanged() {
    use super::super::command_export::{MAX_COMMAND_EXPORT_BYTES, write_plain_text};
    use std::time::{SystemTime, UNIX_EPOCH};
    assert_eq!(MAX_COMMAND_EXPORT_BYTES, 32 * 1024 * 1024);
    let directory = std::env::temp_dir().join(format!(
        "odytty-export-cap-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir(&directory).expect("owned directory");
    let path = directory.join("saved.txt");
    std::fs::write(&path, b"previous export\n").expect("existing export");
    let oversized = "x".repeat(MAX_COMMAND_EXPORT_BYTES + 1);
    let result = write_plain_text(&path, &oversized);
    let previous = std::fs::read(&path).expect("existing destination remains");
    let entries = std::fs::read_dir(&directory).expect("directory").count();
    std::fs::remove_dir_all(&directory).expect("cleanup owned directory");
    assert_eq!(result, Err(CommandExportError::TooLarge));
    assert_eq!(previous, b"previous export\n");
    assert_eq!(entries, 1, "no temporary export is left on refusal");
}

fn row(text: &str, columns: usize) -> Vec<Cell> {
    let mut cells: Vec<Cell> = text
        .chars()
        .map(|ch| Cell::new(ch, Attrs::default()))
        .collect();
    cells.resize(columns, Cell::new(' ', Attrs::default()));
    cells
}

fn no_links(_: LinkId) -> Option<String> {
    None
}

fn build(rows: &[(Vec<Cell>, bool)]) -> Vec<ExportLine> {
    let mut builder = DocumentBuilder::default();
    for (cells, wrapped) in rows {
        builder.push_row(cells, *wrapped, &mut no_links);
    }
    builder.finish()
}

fn palette() -> ExportPalette {
    ExportPalette::from_theme(&crate::theme::Theme::PLAIN)
}

fn text_line(text: &str) -> ExportLine {
    ExportLine::Text(vec![ExportSpan {
        text: text.to_owned(),
        style: SpanStyle::default(),
        link: None,
    }])
}

fn bounded(
    format: ScrollbackFormat,
    limit: usize,
    rows: &[(Vec<Cell>, bool)],
) -> Result<String, CommandExportError> {
    let colors = palette();
    let mut document = BoundedDocument::new(format, &colors, limit)?;
    for (cells, wrapped) in rows {
        document.push_row(cells, *wrapped, &mut no_links)?;
    }
    document.finish()
}

#[test]
fn wrapped_rows_join_into_one_logical_line_and_trailing_blanks_trim() {
    let lines = build(&[
        (row("abcde", 5), true),
        (row("fg", 5), false),
        (row("next  ", 6), false),
        (row("", 5), false),
        (row("", 5), false),
    ]);
    assert_eq!(lines, vec![text_line("abcdefg"), text_line("next")]);
    assert_eq!(plain_text(&lines), "abcdefg\nnext\n");
}

#[test]
fn inner_blank_lines_are_kept_and_trailing_screen_rows_dropped() {
    let lines = build(&[
        (row("one", 5), false),
        (row("", 5), false),
        (row("two", 5), false),
        (row("", 5), false),
    ]);
    assert_eq!(plain_text(&lines), "one\n\ntwo\n");
    assert_eq!(plain_text(&[]), "");
}

#[test]
fn wide_glyph_spacers_are_skipped() {
    let mut cells = row("", 4);
    cells[0] = Cell::new('\u{4e2d}', Attrs::default());
    cells[1].wide_continuation = true;
    cells[2] = Cell::new('x', Attrs::default());
    assert_eq!(plain_text(&build(&[(cells, false)])), "\u{4e2d}x\n");
}

#[test]
fn image_anchor_becomes_one_placeholder_line() {
    let mut builder = DocumentBuilder::default();
    builder.push_row(&row("before", 8), false, &mut no_links);
    builder.mark_image();
    builder.push_row(&row("", 8), false, &mut no_links);
    builder.push_row(&row("after", 8), false, &mut no_links);
    let lines = builder.finish();
    assert_eq!(plain_text(&lines), "before\n[image]\nafter\n");
}

#[test]
fn unicode_placeholder_cells_become_the_image_line() {
    let mut cells = row("", 4);
    cells[0] = Cell::new(PLACEHOLDER_CHAR, Attrs::default());
    cells[1] = Cell::new(PLACEHOLDER_CHAR, Attrs::default());
    let lines = build(&[(cells, false), (row("tail", 4), false)]);
    assert_eq!(plain_text(&lines), "[image]\ntail\n");
    assert!(!plain_text(&lines).contains(PLACEHOLDER_CHAR));
}

#[test]
fn html_escapes_markup_and_contains_no_active_content() {
    let lines = build(&[(row("<script>alert('x')</script> & \"q\"", 40), false)]);
    let html = html_document(&lines, &palette());
    assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;q&quot;"));
    let lower = html.to_ascii_lowercase();
    for forbidden in [
        "<script",
        "<iframe",
        "<object",
        "<embed",
        "<img",
        "<link",
        "onload",
        "onerror",
        "javascript:",
        "@import",
        "url(",
    ] {
        assert!(!lower.contains(forbidden), "{forbidden} must not appear");
    }
    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("default-src 'none'"));
    assert_eq!(html.matches("<style>").count(), 1);
}

fn link_id() -> LinkId {
    LinkId::new(std::num::NonZeroU32::new(7).expect("nonzero"))
}

fn linked_row(text: &str, uri: &str) -> Vec<ExportLine> {
    let mut attrs = Attrs::default();
    attrs.hyperlink = Some(link_id());
    let cells: Vec<Cell> = text.chars().map(|ch| Cell::new(ch, attrs)).collect();
    let mut builder = DocumentBuilder::default();
    let owned = uri.to_owned();
    builder.push_row(&cells, false, &mut |id: LinkId| {
        (id == link_id()).then(|| owned.clone())
    });
    builder.finish()
}

#[test]
fn only_http_and_https_links_become_hrefs() {
    let html = html_document(
        &linked_row("site", "https://example.com/a?b=1#c"),
        &palette(),
    );
    assert!(html.contains(
        "<a href=\"https://example.com/a?b=1#c\" rel=\"noopener noreferrer nofollow\">site</a>"
    ));
    for refused in [
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        "file:///etc/passwd",
        "data:text/html,hi",
        "ftp://example.com/",
        "https://",
        "https:///path",
        "http://user:secret@example.com/",
        "https://exa mple.com/",
        "https://example.com/\"onmouseover=\"x",
        "//example.com/",
        "example.com",
    ] {
        let html = html_document(&linked_row("t", refused), &palette());
        assert!(!html.contains("<a "), "{refused} must stay text");
        assert!(
            html.contains(">t\n") || html.contains("\nt\n"),
            "{refused}: text kept"
        );
    }
    assert!(
        safe_http_href(&format!(
            "https://example.com/{}",
            "a".repeat(MAX_HREF_BYTES)
        ))
        .is_none()
    );
    assert_eq!(
        safe_http_href("HTTP://[::1]:8080/x"),
        Some("HTTP://[::1]:8080/x")
    );
}

#[test]
fn plain_text_never_carries_link_targets() {
    let text = plain_text(&linked_row("label", "https://example.com/secret-target"));
    assert_eq!(text, "label\n");
}

#[test]
fn html_styles_are_bounded_to_palette_colors_and_classes() {
    let mut red_bold = Attrs::default();
    red_bold.foreground = Color::Indexed(1);
    red_bold.set_bold(true);
    let mut rgb_bg = Attrs::default();
    rgb_bg.background = Color::Rgb(1, 2, 3);
    let cells = vec![
        Cell::new('R', red_bold),
        Cell::new('g', rgb_bg),
        Cell::new('n', Attrs::default()),
    ];
    let theme = palette();
    let html = html_document(&build(&[(cells, false)]), &theme);
    let red = theme.ansi[1];
    assert!(html.contains(&format!(
        "<span class=\"b\" style=\"color:#{:02x}{:02x}{:02x}\">R</span>",
        red.0, red.1, red.2
    )));
    assert!(html.contains("<span style=\"background:#010203\">g</span>n"));
}

#[test]
fn inverse_swaps_resolved_colors() {
    let mut inverse = Attrs::default();
    inverse.set_inverse(true);
    let theme = palette();
    let html = html_document(&build(&[(vec![Cell::new('v', inverse)], false)]), &theme);
    let fg = theme.foreground;
    let bg = theme.background;
    assert!(html.contains(&format!(
        "style=\"color:#{:02x}{:02x}{:02x};background:#{:02x}{:02x}{:02x}\">v<",
        bg.0, bg.1, bg.2, fg.0, fg.1, fg.2
    )));
}

#[test]
fn html_image_line_is_text_not_an_img_element() {
    let mut builder = DocumentBuilder::default();
    builder.mark_image();
    builder.push_row(&row("", 3), false, &mut no_links);
    let html = html_document(&builder.finish(), &palette());
    assert!(html.contains("<span class=\"img\">[image]</span>"));
    assert!(!html.to_ascii_lowercase().contains("<img"));
}

#[test]
fn export_chunks_match_the_previous_viewport_walk_for_one_three_and_large_chunks() {
    let mut terminal = crate::core::Terminal::new(12, 4);
    for index in 0..48 {
        let line = match index % 3 {
            0 => format!("row-{index:02}-abcdefghijklmnop\r\n"),
            1 => format!("\x1b[1;34mrow-{index:02}\x1b[0m\r\n"),
            _ => format!(
                "\x1b]8;;https://example.invalid/{index}\x07row-{index:02}-漢字\x1b]8;;\x07\r\n"
            ),
        };
        terminal.advance(line.as_bytes());
    }
    terminal.advance(b"\x1b]133;A\x07prompt$ ");

    let screen = terminal.screen();
    let live_rows = screen.visible_search_rows(0).len();
    let total_rows = screen.export_row_count();
    let scrollback_rows = total_rows - live_rows;
    let mut previous_walk = Vec::with_capacity(total_rows);
    let mut start = 0;
    while start < scrollback_rows {
        let take = live_rows.min(scrollback_rows - start);
        let window = screen.visible_search_rows(scrollback_rows - start);
        previous_walk.extend(window.into_iter().take(take));
        start += take;
    }
    previous_walk.extend(screen.visible_search_rows(0));
    assert_eq!(previous_walk.len(), total_rows);

    for chunk_size in [1, 3, 512] {
        let mut rows = Vec::with_capacity(total_rows);
        let mut start = 0;
        while start < total_rows {
            let chunk = screen.export_chunk(start, chunk_size);
            assert!(!chunk.rows.is_empty(), "nonempty range at {start}");
            start += chunk.rows.len();
            rows.extend(chunk.rows);
        }
        assert_eq!(rows, previous_walk, "chunk size {chunk_size}");
    }
    assert!(screen.export_chunk(total_rows, 3).rows.is_empty());
    assert!(screen.export_chunk(0, 0).rows.is_empty());
}

#[test]
fn one_row_export_chunks_emit_one_image_for_a_placement_across_row_511() {
    let mut terminal = crate::core::Terminal::new(40, 8);
    for index in 0..513 {
        terminal.advance(format!("row-{index:03}\r\n").as_bytes());
    }
    terminal.advance(b"\x1b[6;1H");
    terminal.advance(b"\x1b_Ga=T,f=32,t=d,s=2,v=2,i=1,c=2,r=2;AAAA/wAAAP8AAAD/AAAA/w==\x1b\\");

    let screen = terminal.screen();
    let total_rows = screen.export_row_count();
    let mut builder = DocumentBuilder::default();
    let mut seen = std::collections::HashSet::new();
    let mut image_rows_seen = 0;
    let mut first_image_row = None;
    let mut no_link = |_| None;

    for start in 0..total_rows {
        let chunk = screen.export_chunk(start, 1);
        let mut image_rows = std::collections::BTreeSet::new();
        for (id, row) in chunk.placements {
            if seen.insert(id) {
                image_rows.insert(row);
                image_rows_seen += 1;
                first_image_row = Some(start + row);
            }
        }
        for (row, visible) in chunk.rows.iter().enumerate() {
            if image_rows.contains(&row) {
                builder.mark_image();
            }
            builder.push_row(&visible.cells, visible.wrapped, &mut no_link);
        }
    }

    assert_eq!(image_rows_seen, 1, "one graphics placement is anchored");
    assert_eq!(
        first_image_row,
        Some(511),
        "fixture places at the chunk edge"
    );
    assert_eq!(plain_text(&builder.finish()).matches("[image]").count(), 1);
}

#[test]
fn html_replaces_control_characters() {
    let lines = vec![text_line("a\u{7}b\u{1b}[31m")];
    let html = html_document(&lines, &palette());
    assert!(html.contains("a\u{FFFD}b\u{FFFD}[31m"));
    assert!(!html.contains('\u{1b}'));
}

#[test]
fn bounded_capture_matches_unbounded_output_at_exact_cap_and_refuses_cap_minus_one() {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Rgb(120, 40, 180);
    attrs.set_bold(true);
    let cells = "linked <& text"
        .chars()
        .map(|ch| Cell::new(ch, attrs))
        .collect::<Vec<_>>();
    let rows = [(cells.clone(), false)];
    let mut builder = DocumentBuilder::default();
    builder.push_row(&cells, false, &mut no_links);
    let lines = builder.finish();
    let colors = palette();

    for (format, expected) in [
        (ScrollbackFormat::PlainText, plain_text(&lines)),
        (ScrollbackFormat::Html, html_document(&lines, &colors)),
    ] {
        assert_eq!(bounded(format, expected.len(), &rows), Ok(expected.clone()));
        assert_eq!(
            bounded(format, expected.len() - 1, &rows),
            Err(CommandExportError::TooLarge),
            "the final encoded byte length, not source cell count, sets the cap"
        );
    }
}

#[test]
fn html_escape_link_and_style_expansion_counts_against_the_final_byte_cap() {
    let mut attrs = Attrs::default();
    attrs.hyperlink = Some(link_id());
    attrs.foreground = Color::Rgb(1, 2, 3);
    attrs.set_underline(true);
    let source = "<".repeat(96);
    let cells = source
        .chars()
        .map(|ch| Cell::new(ch, attrs))
        .collect::<Vec<_>>();
    let uri = format!("https://example.com/{}", "p".repeat(96));
    let mut builder = DocumentBuilder::default();
    builder.push_row(&cells, false, &mut |id| {
        (id == link_id()).then(|| uri.clone())
    });
    let lines = builder.finish();
    let colors = palette();
    let html = html_document(&lines, &colors);
    let plain = plain_text(&lines);
    let cap_without_expansion = html_document(&[], &colors).len() + plain.len();

    assert!(html.len() > cap_without_expansion);
    assert_eq!(
        bounded(
            ScrollbackFormat::Html,
            cap_without_expansion,
            &[(cells, false)]
        ),
        Err(CommandExportError::TooLarge),
        "escaped text and hyperlink/style markup must be included in the cap"
    );
}

#[test]
fn overflow_after_a_valid_prefix_refuses_the_entire_document() {
    let limit = 8;
    let rows = [(row("ok", 2), false), (row("too long", 8), false)];
    let colors = palette();
    let mut document = BoundedDocument::new(ScrollbackFormat::PlainText, &colors, limit)
        .expect("small plain-text header fits");
    assert!(
        document
            .push_row(&rows[0].0, rows[0].1, &mut no_links)
            .is_ok()
    );
    assert_eq!(
        document.push_row(&rows[1].0, rows[1].1, &mut no_links),
        Err(CommandExportError::TooLarge)
    );
}

#[test]
fn oversized_trailing_blank_tail_is_ignored_but_interior_overflow_is_refused() {
    let colors = palette();
    let spaces = vec![Cell::new(' ', Attrs::default()); 1_000];
    let mut blank = BoundedDocument::new(ScrollbackFormat::PlainText, &colors, 16)
        .expect("plain-text document");
    blank
        .push_row(&spaces, false, &mut no_links)
        .expect("trailing whitespace does not count as exported bytes");
    assert_eq!(blank.finish(), Ok(String::new()));

    let mut content = BoundedDocument::new(ScrollbackFormat::PlainText, &colors, 16)
        .expect("plain-text document");
    content
        .push_row(&spaces, true, &mut no_links)
        .expect("wrapped blank tail is held without unbounded output storage");
    assert_eq!(
        content.push_row(&[Cell::new('z', Attrs::default())], false, &mut no_links),
        Err(CommandExportError::TooLarge),
        "a later non-blank character makes the oversized tail part of the output"
    );
}
