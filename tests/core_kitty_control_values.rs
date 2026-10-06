// SPDX-License-Identifier: GPL-3.0-only
// Project-authored bounded protocol fixtures: control values that do not parse
// refuse the command, while unknown keys and valid wide readings still work.
use odytty::core::Terminal;

/// One opaque red 1x1 RGBA pixel, base64.
const PIXEL: &str = "/wAA/w==";

fn stores(control: &str) -> usize {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(format!("\x1b_G{control};{PIXEL}\x1b\\").as_bytes());
    terminal.graphics().store().len()
}

#[test]
fn malformed_values_of_every_reading_refuse_the_command() {
    for control in [
        "a=T,f=32,s=1,v=1,m=x",
        "a=T,f=32,s=1,v=1,i=4294967296",
        "a=T,f=32,s=1,v=1,X=left",
        "a=T,f=32,s=1,v=1,Y=",
        "a=T,f=32,s=1,v=1,z=top",
        "a=T,f=32,s=1,v=1,c=-1",
        "a=T,f=32,s=1,v=1,q=quiet",
        "a=T,f=3x,s=1,v=1",
    ] {
        assert_eq!(stores(control), 0, "{control}");
    }
}

#[test]
fn unknown_keys_and_unsigned_offset_readings_are_still_accepted() {
    assert_eq!(stores("a=T,f=32,s=1,v=1,K=zz"), 1);
    // `Y=` is a signed pixel offset on a placement and an unsigned color on a
    // frame; a value only the unsigned reading accepts is not malformed.
    assert_eq!(stores("a=T,f=32,s=1,v=1,Y=4278190335"), 1);
}

#[test]
fn an_aborting_command_keeps_its_own_quiet_level() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"\x1b_Ga=t,f=32,s=1,v=1,m=1;AA\x1b\\");
    let _ = terminal.take_host_output();
    terminal.advance(b"\x1b_Ga=Z,f=32,s=1,v=1,q=2;AAAAAA==\x1b\\");
    assert!(terminal.take_host_output().is_empty());
}
