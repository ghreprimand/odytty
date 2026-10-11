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
