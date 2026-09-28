// SPDX-License-Identifier: GPL-3.0-only
//! Scrollback export sanitizer and shared-writer contract tests.

use super::super::command_export::write_plain_text;
use super::super::scrollback_export::{
    DocumentBuilder, ExportLine, ExportPalette, ExportSpan, ScrollbackFormat, SpanStyle,
    html_document, plain_text, safe_http_href,
};
use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn lines_with_text(text: &str, link: Option<&str>) -> Vec<ExportLine> {
    vec![ExportLine::Text(vec![ExportSpan {
        text: text.to_owned(),
        style: SpanStyle::default(),
        link: link.map(str::to_owned),
    }])]
}

fn palette() -> ExportPalette {
    ExportPalette::from_theme(&Theme::default())
}

#[test]
fn html_escapes_text_and_emits_only_safe_http_links() {
    let dangerous = lines_with_text("<script>& \"quoted\" 'text'", None);
    let html = html_document(&dangerous, &palette());
    assert!(html.contains("&lt;script&gt;&amp; &quot;quoted&quot; &#39;text&#39;"));
    for active in [
        "<script", "<iframe", "<object", "<embed", "<img", "onload=", "onclick=",
    ] {
        assert!(
            !html.to_ascii_lowercase().contains(active),
            "active markup: {active}"
        );
    }
    assert_eq!(html.matches("<style>").count(), 1);
    assert!(html.contains("default-src 'none'"));

    let safe = html_document(
        &lines_with_text("safe", Some("HTTPS://example.com/a")),
        &palette(),
    );
    assert!(safe.contains("<a href=\"HTTPS://example.com/a\" rel="));
    assert!(safe.contains(">safe</a>"));

    for target in [
        "javascript:alert(1)",
        "data:text/html,boom",
        "file:///secret",
        "https:///missing-host",
        "https://user@example.com/private",
        "https://example.com/\" onmouseover=\"alert(1)",
    ] {
        assert_eq!(
            safe_http_href(target),
            None,
            "unsafe URL accepted: {target}"
        );
        let html = html_document(&lines_with_text("label", Some(target)), &palette());
        assert!(
            !html.contains("<a "),
            "unsafe URL rendered as link: {target}"
        );
        assert!(
            !html.contains(target),
            "unsafe target leaked into HTML: {target}"
        );
    }
}

#[test]
fn plain_text_contains_only_visible_labels_not_link_targets() {
    let lines = lines_with_text("click me", Some("https://example.com/private"));
    assert_eq!(plain_text(&lines), "click me\n");
    assert!(!plain_text(&lines).contains("example.com"));
}

#[test]
fn document_builder_joins_wrapped_rows_and_marks_images_as_text() {
    let mut builder = DocumentBuilder::default();
    let mut no_link = |_| None;
    builder.push_row(&[Cell::new('a', Attrs::default())], true, &mut no_link);
    builder.push_row(
        &[
            Cell::new('b', Attrs::default()),
            Cell::new(' ', Attrs::default()),
        ],
        false,
        &mut no_link,
    );
    builder.mark_image();
    builder.push_row(&[Cell::new('x', Attrs::default())], false, &mut no_link);
    let lines = builder.finish();

    assert_eq!(plain_text(&lines), "ab\n[image]\nx\n");
    assert!(matches!(lines[1], ExportLine::Image));
}

#[test]
fn scrollback_plain_text_is_written_by_the_shared_private_export_writer() {
    let dimensions = Dimensions::new(20, 4);
    let (app, terminal) =
        headless_app_with(NativeOptions::default(), dimensions, Settings::default());
    terminal.lock().expect("terminal").advance(b"alpha\r\nbeta");
    let contents = app
        .capture_scrollback_export(ScrollbackFormat::PlainText)
        .expect("capture");
    let directory = std::env::temp_dir().join(format!(
        "odytty-scrollback-export-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir(&directory).expect("temp directory");
    let path = directory.join("scrollback.txt");

    write_plain_text(&path, &contents).expect("shared writer accepts captured text");

    assert_eq!(fs::read_to_string(&path).expect("read export"), contents);
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn safe_href_requires_http_scheme_and_nonempty_authority() {
    for accepted in [
        "http://example.com",
        "https://example.com/path?q=1#frag",
        "HtTpS://[2001:db8::1]/",
    ] {
        assert_eq!(safe_http_href(accepted), Some(accepted));
    }
    for rejected in [
        "http:example.com",
        "https://",
        "ftp://example.com",
        "//example.com",
    ] {
        assert_eq!(safe_http_href(rejected), None, "accepted {rejected}");
    }
}
