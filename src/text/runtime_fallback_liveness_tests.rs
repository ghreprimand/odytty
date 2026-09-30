// SPDX-License-Identifier: GPL-3.0-only
//! Runtime glyph fallback must never execute fontconfig on the render caller.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::FontHandle;
use super::super::symbols::FontconfigStalled;
use super::*;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn slow_none(_ch: char) -> Result<Option<Arc<FontHandle>>, FontconfigStalled> {
    std::thread::sleep(Duration::from_millis(300));
    Ok(None)
}

fn fast_none(_ch: char) -> Result<Option<Arc<FontHandle>>, FontconfigStalled> {
    Ok(None)
}

fn stalled(_ch: char) -> Result<Option<Arc<FontHandle>>, FontconfigStalled> {
    Err(FontconfigStalled)
}

fn wait_idle(deadline: Duration) {
    let until = Instant::now() + deadline;
    while Instant::now() < until {
        if idle_for_test() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("runtime glyph fallback worker did not settle within {deadline:?}");
}

#[test]
fn glyph_fallback_queues_without_blocking_and_disables_after_helper_stall() {
    let _guard = TEST_LOCK.lock().expect("runtime fallback test lock");

    reset_for_test(Some(slow_none));
    let start = Instant::now();
    assert!(matches!(request('\u{0378}'), RuntimeSymbol::Pending));
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "a slow fontconfig resolver blocked the glyph request"
    );
    wait_idle(Duration::from_secs(2));
    assert!(matches!(request('\u{0378}'), RuntimeSymbol::Ready(None)));

    reset_for_test(Some(fast_none));
    let start = Instant::now();
    for offset in 0..200_u32 {
        let ch = char::from_u32(0x4000 + offset).expect("synthetic codepoint");
        assert!(matches!(request(ch), RuntimeSymbol::Pending));
    }
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "a page of distinct glyph requests blocked on fallback work"
    );
    wait_idle(Duration::from_secs(3));

    reset_for_test(Some(stalled));
    assert!(matches!(request('\u{0379}'), RuntimeSymbol::Pending));
    wait_idle(Duration::from_secs(2));
    assert!(
        disabled_for_test(),
        "a stalled helper disables future queries"
    );
    assert!(matches!(request('\u{037A}'), RuntimeSymbol::Ready(None)));

    reset_for_test(None);
}
