// SPDX-License-Identifier: GPL-3.0-only
//! A DECRQM query for grapheme cluster mode 2027 is answered through the real
//! PTY read pump: a child process prints the query and records the reply the
//! terminal writes back to it. Unix only (the child is `/bin/sh`); the core
//! report is covered on every platform by `tests/grapheme_cluster_mode.rs`.

#[cfg(unix)]
#[test]
fn a_session_child_reads_mode_2027_as_permanently_set() {
    use super::ambiguous_width_seeding::{app_with, wait_for_reply, write_script_profile};
    use super::*;
    use crate::settings::Settings;
    const REPLY: &[u8] = b"\x1b[?2027;3$y";
    const SCRIPT: &str = "stty raw -echo; printf '\\033[?2027$p'; \
        dd bs=1 count=11 of=\"$1\" 2>/dev/null";
    let base = crate::test_dirs::ScratchDir::new("gcm");
    let reply = base.path().join("reply");
    write_script_profile(base.path(), "probe", None, SCRIPT, &reply);
    let answer = config_env::with_config_base(base.path(), false, || {
        let mut app = app_with(Settings::default());
        app.new_tab_with_profile_for_test("probe");
        wait_for_reply(&reply, REPLY.len())
    });
    assert_eq!(answer, REPLY);
}
