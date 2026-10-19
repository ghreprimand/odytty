// SPDX-License-Identifier: GPL-3.0-only
//! Present-mode policy regressions for Wayland surfaces.

use crate::native::gpu::present_mode::{revalidate_present_mode, select_present_mode};
use crate::test_source_lex::{
    Token, matching_close, rust_sources_recursively, starts_with_call, tokenize,
};
use std::fs;
use std::path::Path;
use wgpu::PresentMode;

fn select(offered: &[PresentMode], wayland: bool) -> PresentMode {
    select_present_mode(offered, wayland)
}

#[test]
fn wayland_uses_mailbox_when_offered() {
    assert_eq!(
        select(&[PresentMode::Fifo, PresentMode::Mailbox], true),
        PresentMode::Mailbox,
    );
}

#[test]
fn wayland_falls_back_to_fifo_without_mailbox() {
    assert_eq!(
        select(&[PresentMode::Fifo, PresentMode::Immediate], true),
        PresentMode::Fifo,
    );
}

#[test]
fn non_wayland_keeps_fifo_when_mailbox_is_offered() {
    assert_eq!(
        select(&[PresentMode::Mailbox, PresentMode::Fifo], false),
        PresentMode::Fifo,
    );
}

#[test]
fn empty_wayland_offers_fall_back_to_fifo() {
    assert_eq!(select(&[], true), PresentMode::Fifo);
}

#[test]
fn offered_mode_order_does_not_change_the_choice() {
    let offered = [
        [PresentMode::Mailbox, PresentMode::Fifo],
        [PresentMode::Fifo, PresentMode::Mailbox],
    ];
    for modes in offered {
        assert_eq!(select(&modes, true), PresentMode::Mailbox);
    }
}

#[test]
fn revalidation_keeps_mailbox_when_offered_and_falls_back_when_removed() {
    assert_eq!(
        revalidate_present_mode(
            PresentMode::Mailbox,
            &[PresentMode::Fifo, PresentMode::Mailbox]
        ),
        PresentMode::Mailbox,
    );
    assert_eq!(
        revalidate_present_mode(PresentMode::Mailbox, &[PresentMode::Fifo]),
        PresentMode::Fifo,
    );
}

#[test]
fn gpu_surface_present_modes_are_selected_by_the_policy_helper() {
    let gpu_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/gpu");
    let files = rust_sources_recursively(&gpu_dir);
    assert!(!files.is_empty(), "GPU source scan found no Rust files");

    let mut configurations = 0;
    for path in files {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed reading {}: {error}", path.display()));
        configurations += scan_present_mode_policy(&source)
            .unwrap_or_else(|violation| panic!("{}: {violation}", path.display()));
    }
    assert!(
        configurations > 0,
        "GPU source scan found no SurfaceConfiguration literals"
    );
}

/// Check one source file against the present-mode policy and return how many
/// `SurfaceConfiguration` literals it holds: every literal takes its
/// `present_mode` from `select_present_mode`, directly or through a local
/// binding in the same function, and every later assignment to a
/// `present_mode` field goes through `revalidate_present_mode`.
fn scan_present_mode_policy(source: &str) -> Result<usize, String> {
    let tokens = tokenize(source);
    let mut configurations = 0;
    for (start, token) in tokens.iter().enumerate() {
        let is_literal = matches!(token, Token::Ident(name) if name == "SurfaceConfiguration")
            && tokens.get(start + 1) == Some(&Token::Punct("{"))
            && !matches!(
                start.checked_sub(1).and_then(|before| tokens.get(before)),
                Some(Token::Ident(keyword)) if matches!(keyword.as_str(), "struct" | "for" | "impl" | "enum" | "trait")
            );
        if is_literal {
            configurations += 1;
            let close = matching_close(&tokens, start + 1)
                .ok_or("unterminated SurfaceConfiguration literal")?;
            check_configuration(&tokens, start, &tokens[start + 2..close])?;
        }
        if matches!(token, Token::Punct("."))
            && matches!(tokens.get(start + 1), Some(Token::Ident(name)) if name == "present_mode")
            && tokens.get(start + 2) == Some(&Token::Punct("="))
        {
            let end = tokens[start + 3..]
                .iter()
                .position(|token| *token == Token::Punct(";"))
                .map_or(tokens.len(), |offset| start + 3 + offset);
            let revalidated = tokens[start + 3..end].windows(2).any(|pair| {
                pair[1] == Token::Punct("(")
                    && pair[0] == Token::Ident("revalidate_present_mode".into())
            });
            if !revalidated {
                return Err(
                    "mutates a surface present_mode outside revalidate_present_mode".into(),
                );
            }
        }
    }
    Ok(configurations)
}

/// Check the fields of one literal whose type name sits at `start`.
fn check_configuration(tokens: &[Token], start: usize, fields: &[Token]) -> Result<(), String> {
    // Split the literal into top-level `name: value` fields.
    let mut value: Option<&[Token]> = None;
    let mut field_start = 0;
    let mut depth = 0usize;
    for index in 0..=fields.len() {
        let at_end = index == fields.len();
        match fields.get(index) {
            Some(Token::Punct("{" | "(" | "[")) => depth += 1,
            Some(Token::Punct("}" | ")" | "]")) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if at_end || (depth == 0 && fields[index] == Token::Punct(",")) {
            let field = &fields[field_start..index];
            if let [Token::Ident(name), rest @ ..] = field
                && name == "present_mode"
            {
                value = Some(match rest {
                    [Token::Punct(":"), initializer @ ..] => initializer,
                    // Field init shorthand: the binding has the field's name.
                    _ => &field[..1],
                });
            }
            field_start = index + 1;
        }
    }
    let value = value.ok_or("SurfaceConfiguration has no present_mode")?;
    if starts_with_call(value, "select_present_mode") {
        return Ok(());
    }
    let [Token::Ident(binding)] = value else {
        return Err("configures present_mode with an expression, not select_present_mode".into());
    };
    // Find `let <binding> = <initializer>;` earlier in the same function.
    let function_start = tokens[..start]
        .iter()
        .rposition(|token| *token == Token::Ident("fn".into()))
        .unwrap_or(0);
    let declaration = tokens[function_start..start]
        .windows(3)
        .rposition(|window| {
            window[0] == Token::Ident("let".into())
                && window[1] == Token::Ident(binding.clone())
                && window[2] == Token::Punct("=")
        })
        .ok_or_else(|| format!("no local {binding} present-mode binding"))?;
    let initializer = &tokens[function_start + declaration + 3..start];
    if starts_with_call(initializer, "select_present_mode") {
        Ok(())
    } else {
        Err(format!(
            "configures present_mode through {binding:?}, not select_present_mode"
        ))
    }
}

#[cfg(test)]
mod policy_scan_tests {
    use super::scan_present_mode_policy;

    #[test]
    fn inline_and_bound_selection_pass_and_count_each_literal() {
        let source = r#"
            fn a() {
                let config = wgpu::SurfaceConfiguration {
                    usage: U::X,
                    present_mode: select_present_mode(&offered, wayland),
                };
            }
            fn b() {
                let present_mode = select_present_mode(&offered, wayland);
                let config = wgpu::SurfaceConfiguration { present_mode, size: Extent { w: 1, h: 2 } };
            }
        "#;
        assert_eq!(scan_present_mode_policy(source), Ok(2));
    }

    #[test]
    fn comments_and_strings_cannot_fool_or_trip_the_scan() {
        let source = r##"
            // cfg.present_mode = PresentMode::Fifo;
            /* nested /* SurfaceConfiguration { */ present_mode: Fifo, */
            fn a() {
                let note = "SurfaceConfiguration { present_mode: Fifo }; .present_mode = x;";
                let raw = r#"cfg.present_mode = bad;"#;
                let config = SurfaceConfiguration {
                    // present_mode: PresentMode::Fifo,
                    present_mode: select_present_mode(&offered, w),
                    label: "};",
                };
            }
        "##;
        assert_eq!(scan_present_mode_policy(source), Ok(1));
    }

    #[test]
    fn a_comment_does_not_stand_in_for_the_policy_call() {
        let source = r#"
            fn a() {
                let mode = PresentMode::Fifo; // select_present_mode(
                let config = SurfaceConfiguration { present_mode: mode };
            }
        "#;
        assert!(scan_present_mode_policy(source).is_err());
    }

    #[test]
    fn bypasses_are_rejected_whatever_the_spacing() {
        for source in [
            "fn a() { let c = SurfaceConfiguration { present_mode: PresentMode::Fifo }; }",
            "fn a() { let c = SurfaceConfiguration{present_mode:PresentMode::Fifo,}; }",
            "fn a() { let c = SurfaceConfiguration { usage: U::X }; }",
            "fn a() { let m = fallback(); let c = SurfaceConfiguration { present_mode: m }; }",
            "fn a() { c . present_mode = PresentMode::Fifo; }",
            "fn a() { c.present_mode=PresentMode::Fifo; }",
            "fn a() { c.present_mode = other(select_present_mode); }",
        ] {
            assert!(scan_present_mode_policy(source).is_err(), "{source}");
        }
    }

    #[test]
    fn revalidated_assignments_and_comparisons_pass() {
        let source = r#"
            fn a() {
                self.config.present_mode =
                    revalidate_present_mode(self.config.present_mode, &caps.present_modes);
                if self.config.present_mode == PresentMode::Fifo {}
                let same = self.config.present_mode >= x;
            }
        "#;
        assert_eq!(scan_present_mode_policy(source), Ok(0));
    }

    #[test]
    fn declarations_and_impls_of_the_type_are_not_literals() {
        let source = "struct SurfaceConfiguration { present_mode: u8 } \
                      impl Default for SurfaceConfiguration { fn default() -> Self { todo!() } }";
        assert_eq!(scan_present_mode_policy(source), Ok(0));
    }
}
