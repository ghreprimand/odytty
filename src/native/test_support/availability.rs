// SPDX-License-Identifier: GPL-3.0-only
//! Explicit availability policy for optional native validation.

/// Whether the required-validation switch `name` is on. Only the value `1`
/// turns it on, the value `ci-validation.sh` exports, so `0` or an empty value
/// leaves the requirement off. `tests/gpu_composite_smoke.rs` is a separate
/// crate and applies the same rule in its own `gpu_required`.
pub(in crate::native) fn required_by_env(name: &str) -> bool {
    requirement_value_is_on(std::env::var_os(name).as_deref())
}

fn requirement_value_is_on(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

pub(in crate::native) fn available(
    test: &str,
    resource: &str,
    present: bool,
    required: bool,
) -> bool {
    if present {
        return true;
    }
    assert!(
        !required,
        "VALIDATION_REQUIRED test={test} resource={resource} unavailable"
    );
    eprintln!("VALIDATION_SKIP test={test} resource={resource} unavailable");
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_value_one_turns_a_requirement_on() {
        use std::ffi::OsStr;
        assert!(requirement_value_is_on(Some(OsStr::new("1"))));
        for off in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("0")),
            Some(OsStr::new("true")),
        ] {
            assert!(!requirement_value_is_on(off), "{off:?}");
        }
    }

    #[test]
    fn present_resources_execute_in_both_modes() {
        assert!(available("fixture", "adapter", true, false));
        assert!(available("fixture", "adapter", true, true));
    }

    #[test]
    fn optional_missing_resource_skips() {
        assert!(!available("fixture", "adapter", false, false));
    }

    #[test]
    #[should_panic(expected = "VALIDATION_REQUIRED test=fixture resource=adapter unavailable")]
    fn required_missing_resource_fails() {
        available("fixture", "adapter", false, true);
    }
}
