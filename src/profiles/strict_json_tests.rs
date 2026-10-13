// SPDX-License-Identifier: GPL-3.0-only
//! Profile documents are read through the strict JSON boundary: damaged
//! hand-edited files are refused with a precise error instead of being
//! normalized on the next save, and a save cannot produce a file the capped
//! reader would refuse.

use super::json;
use super::limits::MAX_PROFILE_FILE_BYTES;
use super::schema::{LaunchProfile, ProfileError};

fn profile_text(extra: &str) -> String {
    format!("{{\"schema_version\": 1, \"name\": \"dev\"{extra}}}")
}

fn malformed(text: &str) -> String {
    match LaunchProfile::parse_json(text, Some("dev")) {
        Err(ProfileError::Malformed(message)) => message,
        other => panic!("expected a malformed document, got {other:?}"),
    }
}

#[test]
fn a_raw_control_character_in_a_string_is_refused() {
    let text = profile_text(", \"display_name\": \"a\tb\"");
    let message = malformed(&text);
    assert!(message.contains("control character"), "{message}");
    // The escaped form stays valid.
    let escaped = profile_text(", \"display_name\": \"a\\tb\"");
    assert!(LaunchProfile::parse_json(&escaped, Some("dev")).is_ok());
}

#[test]
fn the_lenient_reader_keeps_accepting_raw_controls_for_tool_output() {
    // Hyprland replies and palette files go through `parse`, which is
    // deliberately unchanged.
    assert!(json::parse("{\"title\": \"a\tb\"}").is_ok());
    assert!(json::parse("{\"a\": 1, \"a\": 2}").is_ok());
    assert!(json::parse("[01]").is_ok());
}

#[test]
fn numbers_outside_the_json_grammar_are_refused() {
    for bad in ["01", "1.", "-", "1e", "1e+", "--1", "1.5.2", "+1"] {
        let text = profile_text(&format!(", \"future\": {bad}"));
        let result = LaunchProfile::parse_json(&text, Some("dev"));
        assert!(
            matches!(result, Err(ProfileError::Malformed(_))),
            "{bad} -> {result:?}"
        );
    }
    for good in ["0", "-0", "0.5", "10", "1E+2", "2.5e-3", "123456789012"] {
        let text = profile_text(&format!(", \"future\": {good}"));
        assert!(
            LaunchProfile::parse_json(&text, Some("dev")).is_ok(),
            "{good}"
        );
    }
}

#[test]
fn a_number_beyond_f64_is_refused_instead_of_saved_as_zero() {
    let message = malformed(&profile_text(", \"future\": 1e999"));
    assert!(message.contains("out of range"), "{message}");
}

/// Save a parsed profile and parse it again, as the writer and a later load do.
fn saved(text: &str) -> (String, LaunchProfile) {
    let profile = LaunchProfile::parse_json(text, Some("dev")).expect("parse");
    let written = profile.validated_serialization().expect("a valid save");
    let reloaded = LaunchProfile::parse_json(&written, Some("dev")).expect("reload");
    assert_eq!(reloaded, profile, "the saved text loads back unchanged");
    (written, profile)
}

#[test]
fn unknown_numbers_beyond_f64_precision_keep_their_literal_on_save() {
    let literals = [
        "12345678901234567890",
        "-9007199254740993",
        "0.30000000000000004441",
        "1.2345678901234567e-7",
        "340282366920938463463374607431768211455",
    ];
    let text = profile_text(&format!(
        ", \"future\": {}, \"future_object\": {{\"n\": [{}, {{\"deep\": {}}}]}}, \
         \"launch\": {{\"future_launch\": {}}}, \"switch\": {{\"future_switch\": {}}}",
        literals[0], literals[1], literals[2], literals[3], literals[4]
    ));
    let (written, _) = saved(&text);
    for literal in literals {
        assert!(written.contains(literal), "{literal} in {written}");
    }
}

#[test]
fn short_numbers_are_written_as_before() {
    let text = profile_text(", \"future\": [3, 1.5, 1E+2, 0.1, 1000000000000000]");
    let (written, _) = saved(&text);
    // Up to 15 significant digits the value survives the trip through `f64`,
    // and the writer prints its shortest form; the spelling of an exponent is
    // not preserved.
    assert!(written.contains("3,"), "{written}");
    assert!(written.contains("1.5,"), "{written}");
    assert!(written.contains("100,"), "{written}");
    assert!(written.contains("0.1,"), "{written}");
    // From 1e15 up an integer takes the shorter exponent form.
    assert!(written.contains("1e15\n"), "{written}");
}

#[test]
fn exponent_literals_are_saved_in_their_short_form() {
    let text = profile_text(
        ", \"future\": [1e300, 1e-300, -2.5E-200, 1.5e20, 123456789012345e3, 0.000001, 0.01, 0.25]",
    );
    let (written, _) = saved(&text);
    for short in ["1e300,", "1e-300,", "-2.5e-200,", "1.5e20,", "1e-6,"] {
        assert!(written.contains(short), "{short} in {written}");
    }
    // A plain form no longer than the exponent form stays plain.
    assert!(written.contains("123456789012345000,"), "{written}");
    assert!(written.contains("0.01,"), "{written}");
    assert!(written.contains("0.25\n"), "{written}");
}

#[test]
fn many_large_unknown_numbers_still_fit_the_file_cap_on_save() {
    // 4,000 copies of `1e300` read in about 28 KiB; written as 301-digit
    // integers they would exceed the 1 MiB cap and the save would be refused.
    let values = vec!["1e300"; 4000].join(", ");
    let text = profile_text(&format!(", \"future\": [{values}]"));
    let (written, _) = saved(&text);
    assert!(
        (written.len() as u64) < MAX_PROFILE_FILE_BYTES,
        "{} bytes",
        written.len()
    );
}

#[test]
fn a_known_number_with_many_digits_still_loads_and_saves_normalized() {
    let text = profile_text(", \"appearance\": {\"font_size_px\": 12.345678901234567890}");
    let (written, profile) = saved(&text);
    let size = profile.appearance.font_size_px.expect("font size");
    assert!((size - 12.345_679).abs() < 1e-5, "{size}");
    // Owned fields are written from their typed value, never from the source
    // literal.
    assert!(!written.contains("12.345678901234567890"), "{written}");
}

#[test]
fn the_lenient_reader_still_holds_every_number_as_f64() {
    assert_eq!(
        json::parse("12345678901234567890").expect("number"),
        json::Json::Num(12_345_678_901_234_567_890_f64)
    );
}

#[test]
fn duplicate_object_keys_are_refused_at_every_depth() {
    for extra in [
        ", \"display_name\": \"a\", \"display_name\": \"b\"",
        ", \"launch\": {\"shell\": \"a\", \"shell\": \"b\"}",
        ", \"launch\": {\"env\": {\"A\": \"1\", \"A\": \"2\"}}",
        ", \"future\": {\"x\": 1, \"x\": 2}",
    ] {
        let message = malformed(&profile_text(extra));
        assert!(
            message.contains("duplicate object key"),
            "{extra}: {message}"
        );
    }
    // Distinct keys that merely look alike stay valid.
    let text = profile_text(", \"future\": {\"x\": 1, \"X\": 2}");
    assert!(LaunchProfile::parse_json(&text, Some("dev")).is_ok());
}

#[test]
fn a_profile_whose_pretty_form_exceeds_the_file_budget_is_not_saved() {
    // A compact document inside the budget whose pretty-printed form is not:
    // the writer indents every array element.
    let items = vec!["1"; 200_000].join(",");
    let compact = profile_text(&format!(", \"future\": [{items}]"));
    assert!((compact.len() as u64) < MAX_PROFILE_FILE_BYTES);
    let profile = LaunchProfile::parse_json(&compact, Some("dev")).expect("compact fits");
    assert!(profile.serialize_pretty().len() as u64 > MAX_PROFILE_FILE_BYTES);
    assert!(matches!(
        profile.validated_serialization(),
        Err(ProfileError::LimitExceeded(_))
    ));
    assert!(matches!(
        profile.validate(),
        Err(ProfileError::LimitExceeded(_))
    ));
}

#[test]
fn an_in_memory_document_over_the_file_budget_is_refused() {
    let padding = "x".repeat(MAX_PROFILE_FILE_BYTES as usize);
    let text = profile_text(&format!(", \"future\": \"{padding}\""));
    assert!(matches!(
        LaunchProfile::parse_json(&text, Some("dev")),
        Err(ProfileError::LimitExceeded(_))
    ));
}

#[test]
fn strict_objects_refuse_excessive_members_at_every_depth() {
    let members = (0..20_000)
        .map(|n| format!("\"member{n}\":0"))
        .collect::<Vec<_>>()
        .join(",");
    for text in [
        format!("{{{members}}}"),
        profile_text(&format!(", \"future\": {{{members}}}")),
    ] {
        assert!((text.len() as u64) < MAX_PROFILE_FILE_BYTES);
        let result = json::parse_strict(&text);
        assert!(result.is_err(), "object member budget");
        assert!(result.unwrap_err().contains("object member limit"));
    }
}

#[test]
fn escaped_duplicate_keys_and_unknown_members_keep_their_contract() {
    let error = json::parse_strict(r#"{"x":1,"\u0078":2}"#).expect_err("decoded duplicate");
    assert!(error.contains("duplicate object key"));
    let text = profile_text(r#", "future": {"x":1,"X":2}"#);
    let profile = LaunchProfile::parse_json(&text, Some("dev")).expect("unknown members");
    let saved = profile
        .validated_serialization()
        .expect("save unknown members");
    let document = json::parse_strict(&saved).expect("saved document");
    let future = document.get("future").expect("unknown object retained");
    assert_eq!(future.get("x").and_then(json::Json::as_f64), Some(1.0));
    assert_eq!(future.get("X").and_then(json::Json::as_f64), Some(2.0));
}

#[test]
fn strict_object_member_bound_accepts_exact_limit_and_lenient_sibling_stays_open() {
    let members = (0..super::MAX_PROFILE_JSON_OBJECT_MEMBERS)
        .map(|n| format!("\"member{n}\":0"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(json::parse_strict(&format!("{{{members}}}")).is_ok());
    let beyond = format!("{{{members},\"extra\":0}}");
    assert!(json::parse_strict(&beyond).is_err());
    assert!(json::parse(&beyond).is_ok(), "tool-output reader contract");
}
