// SPDX-License-Identifier: GPL-3.0-only
//! Tests for hover resolution and interactive-path open classification.

use super::*;

fn resolved_path(abs: &str) -> crate::paths::Resolved {
    crate::paths::Resolved {
        abs: abs.to_owned(),
        kind: crate::paths::FsKind::File,
        line: None,
        col: None,
    }
}

#[test]
fn image_open_kind_uses_inline_for_images_when_enabled() {
    let settings = crate::settings::Settings {
        interactive_paths_image_inline: true,
        ..crate::settings::Settings::default()
    };
    assert_eq!(
        interactive_path_open_kind(&settings, &resolved_path("/home/user/carpet1.jpg")),
        InteractivePathOpenKind::InlineImage
    );
}

#[test]
fn image_open_kind_uses_external_for_images_when_disabled() {
    let settings = crate::settings::Settings {
        interactive_paths_image_inline: false,
        ..crate::settings::Settings::default()
    };
    assert_eq!(
        interactive_path_open_kind(&settings, &resolved_path("/home/user/carpet1.jpg")),
        InteractivePathOpenKind::External
    );
}

#[test]
fn image_open_kind_uses_external_for_non_images() {
    let settings = crate::settings::Settings {
        interactive_paths_image_inline: true,
        ..crate::settings::Settings::default()
    };
    assert_eq!(
        interactive_path_open_kind(&settings, &resolved_path("/home/user/notes.txt")),
        InteractivePathOpenKind::External
    );
}
