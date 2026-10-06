// SPDX-License-Identifier: GPL-3.0-only
// Project-authored bounded protocol fixtures.
use odytty::core::Terminal;

fn stored_wide_image() -> Terminal {
    let mut terminal = Terminal::new(20, 4);
    let encoded = "A".repeat(128);
    terminal.advance(format!("\x1b_Ga=t,f=32,s=24,v=1,i=7,q=2;{encoded}\x1b\\").as_bytes());
    assert_eq!(terminal.graphics().store().len(), 1);
    terminal
}

#[test]
fn natural_crop_extent_uses_the_remaining_source_width() {
    let mut terminal = stored_wide_image();
    terminal.advance(b"\x1b_Ga=p,i=7,x=16,q=2\x1b\\");
    assert_eq!(terminal.graphics().placements()[0].display_columns, 1);
}

#[test]
fn explicit_zero_crop_width_means_the_rest_of_the_image() {
    let mut terminal = stored_wide_image();
    terminal.advance(b"\x1b_Ga=p,i=7,w=0,q=2\x1b\\");
    assert_eq!(terminal.graphics().placements()[0].display_columns, 3);
}

#[test]
fn aborted_transfer_does_not_set_an_unrelated_commands_quiet_level() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(b"\x1b_Ga=t,f=32,s=1,v=1,q=2,m=1;AA\x1b\\");
    assert!(terminal.take_host_output().is_empty());
    terminal.advance(b"\x1b_Ga=Z,f=32,s=1,v=1;AAAAAA==\x1b\\");
    assert!(
        String::from_utf8(terminal.take_host_output())
            .unwrap()
            .contains("unsupported-action")
    );
}

#[test]
fn malformed_action_is_rejected_instead_of_becoming_transmit_only() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(b"\x1b_Ga=TT,f=32,s=1,v=1;AAAAAA==\x1b\\");
    assert_eq!(terminal.graphics().store().len(), 0);
}

#[test]
fn malformed_compression_value_is_rejected_instead_of_becoming_unset() {
    let mut terminal = Terminal::new(20, 4);
    terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,o=zz;AAAAAA==\x1b\\");
    assert_eq!(terminal.graphics().store().len(), 0);
}

#[test]
fn rgb_expansion_is_refused_at_decode_budget_before_store_insertion() {
    let mut terminal = Terminal::new(8, 3);
    *terminal.graphics_mut() =
        odytty::graphics::ImageScene::new(odytty::graphics::ImageStoreLimits {
            max_decoded_bytes: 3,
            max_images: 2,
        });
    terminal.advance(b"\x1b_Ga=t,f=24,s=1,v=1;////\x1b\\");
    let reply = String::from_utf8(terminal.take_host_output()).unwrap();
    assert!(
        reply.contains("payload-too-large"),
        "expansion must be rejected during decode: {reply}"
    );
    assert!(terminal.graphics().store().is_empty());
}

#[test]
fn named_query_does_not_bypass_default_transport_permission() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"\x1b_Ga=q,t=f,f=32,s=1,v=1,i=7;AAAAAA==\x1b\\");
    let reply = terminal.take_host_output();
    assert!(!reply.is_empty(), "an ordinary query must answer");
    assert!(
        !String::from_utf8_lossy(&reply).contains(";OK"),
        "named media cannot be interpreted as direct bytes"
    );
    assert!(terminal.graphics().store().is_empty());
}

#[test]
fn malformed_medium_is_not_defaulted_to_direct_bytes() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"\x1b_Ga=t,t=dd,f=32,s=1,v=1;AAAAAA==\x1b\\");
    assert!(terminal.graphics().store().is_empty());
}

#[test]
fn malformed_dimensions_are_not_treated_as_omitted_crop() {
    let mut terminal = stored_wide_image();
    terminal.advance(b"\x1b_Ga=p,i=7,w=invalid\x1b\\");
    assert!(terminal.graphics().placements().is_empty());
}

#[test]
fn crop_height_uses_remaining_source_height() {
    let mut terminal = Terminal::new(20, 10);
    let encoded = "A".repeat(256); // 1x48 RGBA, project-authored zero pixels.
    terminal.advance(format!("\x1b_Ga=t,f=32,s=1,v=48,i=7,q=2;{encoded}\x1b\\").as_bytes());
    assert_eq!(terminal.graphics().store().len(), 1);
    terminal.advance(b"\x1b_Ga=p,i=7,y=32,q=2\x1b\\");
    assert_eq!(terminal.graphics().placements()[0].display_rows, 1);
}

#[test]
fn explicit_zero_crop_height_means_remaining_height() {
    let mut terminal = Terminal::new(20, 10);
    let encoded = "A".repeat(256);
    terminal.advance(format!("\x1b_Ga=t,f=32,s=1,v=48,i=7,q=2;{encoded}\x1b\\").as_bytes());
    assert_eq!(terminal.graphics().store().len(), 1);
    terminal.advance(b"\x1b_Ga=p,i=7,h=0,q=2\x1b\\");
    assert_eq!(terminal.graphics().placements()[0].display_rows, 3);
}
