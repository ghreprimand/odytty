// SPDX-License-Identifier: GPL-3.0-only
//! Quarantine keeps every earlier copy of a malformed profile file.

use std::fs;

use super::quarantine_malformed_file;

#[test]
fn a_second_quarantine_of_the_same_profile_keeps_the_first_copy() {
    let dir = crate::test_dirs::fresh_temp_dir("odytty-profile-quarantine-");
    let path = dir.join("dev.profile.json");

    fs::write(&path, "first hand-edited text").expect("write first");
    assert!(quarantine_malformed_file(&path).expect("first quarantine"));
    fs::write(&path, "second hand-edited text").expect("write second");
    assert!(quarantine_malformed_file(&path).expect("second quarantine"));
    fs::write(&path, "third hand-edited text").expect("write third");
    assert!(quarantine_malformed_file(&path).expect("third quarantine"));

    assert_eq!(
        fs::read_to_string(dir.join("dev.profile.json.bad")).expect("first copy"),
        "first hand-edited text"
    );
    assert_eq!(
        fs::read_to_string(dir.join("dev.profile.json.bad.1")).expect("second copy"),
        "second hand-edited text"
    );
    assert_eq!(
        fs::read_to_string(dir.join("dev.profile.json.bad.2")).expect("third copy"),
        "third hand-edited text"
    );
    assert!(!path.exists(), "the original is moved each time");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_file_stays_in_place_once_every_quarantine_name_is_taken() {
    let dir = crate::test_dirs::fresh_temp_dir("odytty-profile-quarantine-");
    let path = dir.join("dev.profile.json");
    fs::write(dir.join("dev.profile.json.bad"), "kept 0").expect("seed");
    for index in 1..super::store::MAX_QUARANTINE_COPIES {
        fs::write(dir.join(format!("dev.profile.json.bad.{index}")), "kept").expect("seed");
    }
    fs::write(&path, "latest").expect("write latest");

    let error = quarantine_malformed_file(&path).expect_err("no free name");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&path).expect("original kept"), "latest");
    assert_eq!(
        fs::read_to_string(dir.join("dev.profile.json.bad")).expect("first kept"),
        "kept 0"
    );
    let _ = fs::remove_dir_all(dir);
}
