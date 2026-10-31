// SPDX-License-Identifier: GPL-3.0-only
//! DECXCPR (`CSI ? 6 n`) and an image-number display (`a=p,I=`) are answered
//! through the real PTY read pump: a child process prints both requests and
//! records the reply the terminal writes back, then anything that arrives
//! shortly afterwards, so a duplicate answer would be visible. Unix only (the
//! child is `/bin/sh`); the core replies are covered on every platform.

#[cfg(unix)]
#[test]
fn a_session_child_reads_one_cursor_position_and_image_number_reply() {
    use super::ambiguous_width_seeding::{app_with, write_script_profile};
    use super::*;
    use crate::settings::Settings;
    const REPLY: &[u8] = b"\x1b[?3;5R\x1b_GI=7,p=2;OK\x1b\\|end";
    const SCRIPT: &str = "stty raw -echo; \
        printf '\\033_Ga=t,f=32,s=1,v=1,I=7,q=2;AAAA/w==\\033\\\\'; \
        printf '\\033[3;5H\\033[?6n\\033_Ga=p,I=7,p=2\\033\\\\'; \
        { dd bs=1 count=22 2>/dev/null; sleep 0.3; stty min 0 time 0; \
        dd bs=64 count=1 2>/dev/null; printf '|end'; } > \"$1.tmp\"; \
        mv \"$1.tmp\" \"$1\"";
    let base = crate::test_dirs::ScratchDir::new("xcpr");
    let reply = base.path().join("reply");
    write_script_profile(base.path(), "probe", None, SCRIPT, &reply);
    let answer = config_env::with_config_base(base.path(), false, || {
        let mut app = app_with(Settings::default());
        app.new_tab_with_profile_for_test("probe");
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Ok(bytes) = std::fs::read(&reply) {
                break bytes;
            }
            assert!(Instant::now() < deadline, "the child recorded no reply");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
    assert_eq!(
        answer,
        REPLY,
        "{}",
        String::from_utf8_lossy(&answer).escape_debug()
    );
}
