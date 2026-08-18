// SPDX-License-Identifier: GPL-3.0-only
//! Bounded arithmetic tests for the memory-attribution diagnostic.
//!
//! Pins the instrument's own invariants — identity, signed remainder, saturating
//! sums, gate grammar, `/proc/status` parse, closed `rss_source` set, and the
//! report-line column set — so a future field addition cannot drift silently.
//! Constructs reports in memory; never opens a GPU device and never reads the
//! process environment (the gate parse is a pure function of its argument).

use std::time::Duration;

use odytty::memory_report::{
    GpuBytes, HostBytes, MemoryReport, ProcessMemory, ResidentSource, format_report_line,
    parse_gate, parse_status_field_kib,
};

/// Column names from the diagnostic legend, in order. Duplicated here because
/// the legend string is private; a missing or reordered column fails the
/// format test rather than the compile.
const LEGEND_COLUMNS: &[&str] = &[
    "seq",
    "epoch_ms",
    "panes",
    "rss_source",
    "rss_bytes",
    "rss_peak_bytes",
    "host_accounted_bytes",
    "host_unaccounted_bytes",
    "host_glyph_atlas_bitmap",
    "host_color_glyph_atlas_bitmap",
    "host_background_image_buffer",
    "host_grid_cells",
    "host_scrollback_ring",
    "host_scrollback_projection",
    "host_scrollback_ring_slack",
    "host_graphics_image_store",
    "host_vertex_staging",
    "gpu_accounted_bytes",
    "gpu_glyph_atlas_texture",
    "gpu_color_glyph_atlas_texture",
    "gpu_background_image_texture",
    "gpu_post_process_textures",
    "gpu_graphics_textures",
    "gpu_vertex_buffers",
];

fn report(
    resident: Option<u64>,
    peak: Option<u64>,
    source: Option<ResidentSource>,
    host: HostBytes,
    gpu: GpuBytes,
) -> MemoryReport {
    MemoryReport {
        process: ProcessMemory {
            resident_bytes: resident,
            peak_resident_bytes: peak,
            source,
        },
        host,
        gpu,
        panes: 1,
    }
}

/// Fills the **additive** fields positionally. `scrollback_ring_slack` is not
/// among them: it is a breakdown of `scrollback_ring` that `accounted()`
/// deliberately excludes, so feeding it from this helper would make every
/// expected total in this file ambiguous. Tests that exercise slack set it
/// explicitly.
fn host_with(fields: impl IntoIterator<Item = u64>) -> HostBytes {
    let mut vals = fields.into_iter();
    HostBytes {
        glyph_atlas_bitmap: vals.next().unwrap_or(0),
        color_glyph_atlas_bitmap: vals.next().unwrap_or(0),
        background_image_buffer: vals.next().unwrap_or(0),
        grid_cells: vals.next().unwrap_or(0),
        scrollback_ring: vals.next().unwrap_or(0),
        scrollback_projection: vals.next().unwrap_or(0),
        graphics_image_store: vals.next().unwrap_or(0),
        vertex_staging: vals.next().unwrap_or(0),
        scrollback_ring_slack: 0,
    }
}

fn line_fields(line: &str) -> Vec<(&str, &str)> {
    line.split_whitespace()
        .map(|token| token.split_once('=').expect("legend token is key=value"))
        .collect()
}

/// `accounted + unaccounted == resident`, exactly, in both directions.
#[test]
fn accounted_plus_unaccounted_equals_resident() {
    let cases: &[(u64, HostBytes)] = &[
        (0, HostBytes::default()),
        (100, host_with([10, 20, 30, 5, 5, 5, 5])),
        (80, host_with([10, 20, 30, 5, 5, 5, 5])),
        (1, host_with([1, 0, 0, 0, 0, 0, 0])),
        (10_000, host_with([1, 2, 3, 4, 5, 6, 7])),
    ];
    for &(resident, host) in cases {
        let r = report(
            Some(resident),
            Some(resident),
            Some(ResidentSource::ProcStatus),
            host,
            GpuBytes::default(),
        );
        let unaccounted = r
            .unaccounted_host_bytes()
            .expect("measured resident yields a remainder");
        let accounted = i64::try_from(r.host.accounted()).unwrap();
        let resident_i = i64::try_from(resident).unwrap();
        assert_eq!(
            accounted.checked_add(unaccounted),
            Some(resident_i),
            "accounted ({accounted}) + unaccounted ({unaccounted}) == resident ({resident})"
        );
    }
}

/// A report whose attributed total exceeds the resident set yields a negative
/// remainder, not zero and not a saturated value. Reserved-but-unfaulted pages
/// make that a real state.
#[test]
fn remainder_is_signed_when_attributed_exceeds_resident() {
    let host = host_with([40, 40, 40, 10, 10, 10, 10]);
    assert_eq!(host.accounted(), 160);
    let r = report(
        Some(100),
        Some(200),
        Some(ResidentSource::ProcStatus),
        host,
        GpuBytes::default(),
    );
    let remainder = r.unaccounted_host_bytes().unwrap();
    assert!(
        remainder < 0,
        "attributed 160 against resident 100 must be negative, got {remainder}"
    );
    assert_eq!(remainder, -60);
    // Not saturating-sub's zero, and not a wrap into a large unsigned.
    assert_ne!(remainder, 0);
}

/// `resident_bytes == None` yields `unaccounted == None`, which formats as the
/// literal token `unmeasured` — never 0. Same for peak.
#[test]
fn unmeasured_resident_and_peak_format_as_unmeasured_never_zero() {
    let r = report(
        None,
        None,
        Some(ResidentSource::Getrusage),
        host_with([8, 0, 0, 0, 0, 0, 0]),
        GpuBytes::default(),
    );
    assert_eq!(r.unaccounted_host_bytes(), None);
    let line = format_report_line(0, 0, &r);
    let fields: Vec<_> = line_fields(&line);
    let rss = fields
        .iter()
        .find(|(k, _)| *k == "rss_bytes")
        .map(|(_, v)| *v)
        .unwrap();
    let peak = fields
        .iter()
        .find(|(k, _)| *k == "rss_peak_bytes")
        .map(|(_, v)| *v)
        .unwrap();
    let unacc = fields
        .iter()
        .find(|(k, _)| *k == "host_unaccounted_bytes")
        .map(|(_, v)| *v)
        .unwrap();
    assert_eq!(rss, "unmeasured");
    assert_eq!(peak, "unmeasured");
    assert_eq!(unacc, "unmeasured");
    assert_ne!(rss, "0");
    assert_ne!(peak, "0");
    assert_ne!(unacc, "0");

    // A measured zero is the number 0, not the unmeasured token.
    let zero = report(
        Some(0),
        Some(0),
        Some(ResidentSource::ProcStatus),
        HostBytes::default(),
        GpuBytes::default(),
    );
    assert_eq!(zero.unaccounted_host_bytes(), Some(0));
    let zero_line = format_report_line(0, 0, &zero);
    let zero_fields: Vec<_> = line_fields(&zero_line);
    assert_eq!(
        zero_fields
            .iter()
            .find(|(k, _)| *k == "rss_bytes")
            .map(|(_, v)| *v),
        Some("0")
    );
    assert_eq!(
        zero_fields
            .iter()
            .find(|(k, _)| *k == "rss_peak_bytes")
            .map(|(_, v)| *v),
        Some("0")
    );
}

/// `accounted()` is saturating: no field combination overflows the sum into a
/// smaller number.
#[test]
fn accounted_saturates_instead_of_wrapping() {
    // Two fields just over half of u64::MAX wrap under wrapping_add (to 1)
    // and saturate under saturating_add (to u64::MAX).
    let half_plus = (u64::MAX / 2).saturating_add(1);
    let host = HostBytes {
        glyph_atlas_bitmap: half_plus,
        color_glyph_atlas_bitmap: half_plus,
        ..HostBytes::default()
    };
    let wrapping = half_plus.wrapping_add(half_plus);
    assert!(
        wrapping < half_plus,
        "precondition: wrapping_add of two (MAX/2+1) values must wrap"
    );
    assert_eq!(host.accounted(), u64::MAX);
    assert_ne!(host.accounted(), wrapping);

    let gpu = GpuBytes {
        glyph_atlas_texture: half_plus,
        color_glyph_atlas_texture: half_plus,
        ..GpuBytes::default()
    };
    assert_eq!(gpu.accounted(), u64::MAX);
    assert_ne!(gpu.accounted(), wrapping);

    let all_max_host = HostBytes {
        glyph_atlas_bitmap: u64::MAX,
        color_glyph_atlas_bitmap: u64::MAX,
        background_image_buffer: u64::MAX,
        grid_cells: u64::MAX,
        scrollback_ring: u64::MAX,
        scrollback_projection: u64::MAX,
        graphics_image_store: u64::MAX,
        vertex_staging: u64::MAX,
        scrollback_ring_slack: u64::MAX,
    };
    assert_eq!(all_max_host.accounted(), u64::MAX);
}

/// `scrollback_ring_slack` is a non-additive breakdown of `scrollback_ring`.
/// Changing it must not move `accounted()` or the host remainder, or the
/// slack figure would double-count and shrink the unexplained bytes.
#[test]
fn accounted_does_not_move_when_scrollback_ring_slack_changes() {
    let base = host_with([10, 10, 10, 10, 40, 10, 10, 10]);
    assert_eq!(base.scrollback_ring_slack, 0);
    let accounted = base.accounted();
    assert_eq!(accounted, 110);

    let mut with_slack = base;
    with_slack.scrollback_ring_slack = 1_000_000;
    let mut max_slack = base;
    max_slack.scrollback_ring_slack = u64::MAX;

    assert_eq!(with_slack.accounted(), accounted);
    assert_eq!(max_slack.accounted(), accounted);
    assert_ne!(with_slack.scrollback_ring_slack, base.scrollback_ring_slack);

    let without = report(
        Some(200),
        Some(200),
        Some(ResidentSource::ProcStatus),
        base,
        GpuBytes::default(),
    );
    let with = report(
        Some(200),
        Some(200),
        Some(ResidentSource::ProcStatus),
        with_slack,
        GpuBytes::default(),
    );
    assert_eq!(
        without.unaccounted_host_bytes(),
        with.unaccounted_host_bytes()
    );
    assert_eq!(without.unaccounted_host_bytes(), Some(90));

    let line = format_report_line(0, 0, &with);
    let map: std::collections::HashMap<&str, &str> = line_fields(&line).into_iter().collect();
    assert_eq!(map["host_scrollback_ring_slack"], "1000000");
    assert_eq!(map["host_accounted_bytes"], "110");
    assert_eq!(map["host_scrollback_ring"], "40");
}

/// GPU-object bytes sit alongside the resident set and never enter the host
/// remainder. Changing them must not move `unaccounted_host_bytes`.
#[test]
fn gpu_bytes_do_not_enter_host_remainder() {
    let host = host_with([10, 10, 10, 10, 10, 10, 10]);
    let without = report(
        Some(200),
        Some(200),
        Some(ResidentSource::ProcStatus),
        host,
        GpuBytes::default(),
    );
    let with = report(
        Some(200),
        Some(200),
        Some(ResidentSource::ProcStatus),
        host,
        GpuBytes {
            glyph_atlas_texture: 1_000_000,
            post_process_textures: 2_000_000,
            vertex_buffers: 3_000_000,
            ..GpuBytes::default()
        },
    );
    assert_eq!(
        without.unaccounted_host_bytes(),
        with.unaccounted_host_bytes()
    );
    assert_eq!(without.host.accounted(), with.host.accounted());
    assert_ne!(without.gpu.accounted(), with.gpu.accounted());
}

/// Gate grammar: `1`/`true` → 10s; a bare integer 1..=3600 → that many seconds
/// except that the token `1` is the boolean form. 0, >3600, negatives, empty,
/// whitespace-only, and junk → None (off). Case-insensitive; surrounding
/// whitespace is trimmed.
#[test]
fn parse_gate_grammar() {
    assert_eq!(parse_gate(None), None);
    assert_eq!(parse_gate(Some("")), None);
    assert_eq!(parse_gate(Some("   ")), None);
    assert_eq!(parse_gate(Some("\t\n")), None);

    let ten = Duration::from_secs(10);
    assert_eq!(parse_gate(Some("1")), Some(ten));
    assert_eq!(parse_gate(Some("true")), Some(ten));
    assert_eq!(parse_gate(Some("TRUE")), Some(ten));
    assert_eq!(parse_gate(Some("True")), Some(ten));
    assert_eq!(parse_gate(Some("  true  ")), Some(ten));
    assert_eq!(parse_gate(Some("\t1\n")), Some(ten));

    // Bare integers other than the boolean token `1`.
    assert_eq!(parse_gate(Some("2")), Some(Duration::from_secs(2)));
    assert_eq!(parse_gate(Some("  60  ")), Some(Duration::from_secs(60)));
    assert_eq!(parse_gate(Some("3600")), Some(Duration::from_secs(3600)));
    // `10` happens to equal the boolean default, but it is the integer form.
    assert_eq!(parse_gate(Some("10")), Some(ten));

    assert_eq!(parse_gate(Some("0")), None);
    assert_eq!(parse_gate(Some("3601")), None);
    assert_eq!(parse_gate(Some("99999")), None);
    assert_eq!(parse_gate(Some("-1")), None);
    assert_eq!(parse_gate(Some("-10")), None);
    assert_eq!(parse_gate(Some("false")), None);
    assert_eq!(parse_gate(Some("yes")), None);
    assert_eq!(parse_gate(Some("on")), None);
    assert_eq!(parse_gate(Some("off")), None);
    assert_eq!(parse_gate(Some("1.5")), None);
    assert_eq!(parse_gate(Some("1s")), None);
    assert_eq!(parse_gate(Some("true1")), None);
    assert_eq!(parse_gate(Some("junk")), None);
    // `str::parse::<u64>` accepts a leading `+`, so this is the integer form,
    // not junk.
    assert_eq!(parse_gate(Some("+60")), Some(Duration::from_secs(60)));
}

/// kiB → bytes conversion, absent/malformed → None, and no panic on hostile
/// input. Declared unconditionally so this runs on every CI leg.
#[test]
fn parse_status_field_kib_conversion_and_malformed() {
    let sample = "Name:\todytty\nVmRSS:\t   1234 kB\nVmHWM:\t   2000 kB\n";
    assert_eq!(parse_status_field_kib(sample, "VmRSS:"), Some(1234 * 1024));
    assert_eq!(parse_status_field_kib(sample, "VmHWM:"), Some(2000 * 1024));
    assert_eq!(parse_status_field_kib(sample, "VmSize:"), None);

    // First matching line wins.
    let dup = "VmRSS:\t1 kB\nVmRSS:\t9 kB\n";
    assert_eq!(parse_status_field_kib(dup, "VmRSS:"), Some(1024));

    // Measured zero is zero, not None.
    assert_eq!(parse_status_field_kib("VmRSS:\t0 kB\n", "VmRSS:"), Some(0));
    // `str::parse::<u64>` accepts a leading `+`.
    assert_eq!(
        parse_status_field_kib("VmRSS:\t+1 kB\n", "VmRSS:"),
        Some(1024)
    );

    let malformed = [
        "",
        "VmRSS:",
        "VmRSS:\t",
        "VmRSS:\tabc kB\n",
        "VmRSS:\t12.5 kB\n",
        "VmRSS:\t-1 kB\n",
        "not a status file",
        " vmrss:\t1234 kB\n",
        "VmRSS:\t18446744073709551616 kB\n",
        "\0VmRSS:\t1 kB\n",
        &"x".repeat(4096),
    ];
    for input in malformed {
        assert_eq!(
            parse_status_field_kib(input, "VmRSS:"),
            None,
            "malformed {input:?} must yield None, not panic"
        );
    }

    // Overflow of kiB * 1024 is None, not a wrap.
    let too_many_kib = format!("VmRSS:\t{} kB\n", (u64::MAX / 1024) + 1);
    assert_eq!(parse_status_field_kib(&too_many_kib, "VmRSS:"), None);
}

/// `rss_source` is a closed set. The exhaustive match fails to compile if a
/// variant is added without updating the token list.
#[test]
fn rss_source_tokens_are_a_closed_set() {
    fn closed_token(src: ResidentSource) -> &'static str {
        match src {
            ResidentSource::ProcStatus => "proc_status",
            ResidentSource::WindowsPsapi => "windows_psapi",
            ResidentSource::Getrusage => "getrusage",
            ResidentSource::Unmeasured => "unmeasured",
        }
    }
    let variants = [
        ResidentSource::ProcStatus,
        ResidentSource::WindowsPsapi,
        ResidentSource::Getrusage,
        ResidentSource::Unmeasured,
    ];
    for src in variants {
        assert_eq!(src.token(), closed_token(src));
        let line = format_report_line(
            0,
            0,
            &report(
                None,
                None,
                Some(src),
                HostBytes::default(),
                GpuBytes::default(),
            ),
        );
        let token = line_fields(&line)
            .into_iter()
            .find(|(k, _)| *k == "rss_source")
            .map(|(_, v)| v.to_string())
            .unwrap();
        assert_eq!(token, src.token());
        assert!(
            matches!(
                token.as_str(),
                "proc_status" | "windows_psapi" | "getrusage" | "unmeasured"
            ),
            "unexpected rss_source token {token}"
        );
    }
    // No source recorded still emits the unmeasured token, never an empty field.
    let missing = report(None, None, None, HostBytes::default(), GpuBytes::default());
    assert_eq!(missing.process.source_token(), "unmeasured");
}

/// `format_report_line` emits every legend column, in order, with no field
/// missing. Negative remainder and unmeasured peak appear as their formatted
/// forms, not as omitted columns.
#[test]
fn format_report_line_emits_legend_columns_in_order() {
    let r = report(
        Some(100),
        None,
        Some(ResidentSource::WindowsPsapi),
        host_with([40, 40, 40, 10, 10, 10, 10]),
        GpuBytes {
            glyph_atlas_texture: 3,
            vertex_buffers: 4,
            ..GpuBytes::default()
        },
    );
    let line = format_report_line(7, 1_700_000_000_000, &r);
    let fields = line_fields(&line);
    let keys: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, LEGEND_COLUMNS);

    let map: std::collections::HashMap<&str, &str> = fields.into_iter().collect();
    assert_eq!(map["seq"], "7");
    assert_eq!(map["epoch_ms"], "1700000000000");
    assert_eq!(map["panes"], "1");
    assert_eq!(map["rss_source"], "windows_psapi");
    assert_eq!(map["rss_bytes"], "100");
    assert_eq!(map["rss_peak_bytes"], "unmeasured");
    assert_eq!(map["host_accounted_bytes"], "160");
    assert_eq!(map["host_unaccounted_bytes"], "-60");
    assert_eq!(map["host_glyph_atlas_bitmap"], "40");
    assert_eq!(map["gpu_accounted_bytes"], "7");
    assert_eq!(map["gpu_glyph_atlas_texture"], "3");
    assert_eq!(map["gpu_vertex_buffers"], "4");
}
