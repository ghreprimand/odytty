// SPDX-License-Identifier: GPL-3.0-only
//! Explicit availability policy for optional native validation.

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
