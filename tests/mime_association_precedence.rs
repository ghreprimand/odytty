// SPDX-License-Identifier: GPL-3.0-only
// Project-authored maps never read the filesystem or launch an application.
use odytty::desktop::{DesktopEnv, MimeProbe, enumerate_open_with};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Probe;
impl MimeProbe for Probe {
    fn query(&self, _: &str) -> Option<String> {
        Some("image/png".into())
    }
}
#[derive(Default)]
struct Env {
    files: HashMap<PathBuf, String>,
    reads: RefCell<Vec<PathBuf>>,
}
impl DesktopEnv for Env {
    fn config_dirs(&self) -> Vec<PathBuf> {
        vec!["fixture-config-high".into(), "fixture-config-low".into()]
    }
    fn data_dirs(&self) -> Vec<PathBuf> {
        vec!["fixture-data-high".into(), "fixture-data-low".into()]
    }
    fn read_file(&self, path: &Path) -> Option<String> {
        self.reads.borrow_mut().push(path.to_owned());
        self.files.get(path).cloned()
    }
}
impl Env {
    fn put(&mut self, path: &str, text: &str) {
        self.files.insert(path.into(), text.into());
    }
    fn associate(&mut self, id: &str) {
        self.put(
            "fixture-config-high/mimeapps.list",
            &format!("[Added Associations]\nimage/png={id};\n"),
        );
    }
}
const ENTRY: &str = "[Desktop Entry]\nType=Application\nName=Fixture\nExec=fixture-viewer %f\n";

#[test]
fn lower_precedence_removal_keeps_a_higher_added_handler() {
    let mut env = Env::default();
    env.associate("fixture.desktop");
    env.put(
        "fixture-config-low/mimeapps.list",
        "[Removed Associations]\nimage/png=fixture.desktop;\n",
    );
    env.put("fixture-data-high/applications/fixture.desktop", ENTRY);
    let apps = enumerate_open_with(&Probe, &env, "fixture.png");
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].id, "fixture.desktop");
}

#[test]
fn higher_precedence_removal_blocks_a_lower_added_handler() {
    let mut env = Env::default();
    env.put(
        "fixture-config-high/mimeapps.list",
        "[Removed Associations]\nimage/png=fixture.desktop;\n",
    );
    env.put(
        "fixture-config-low/mimeapps.list",
        "[Added Associations]\nimage/png=fixture.desktop;\n",
    );
    env.put("fixture-data-low/applications/fixture.desktop", ENTRY);
    assert!(enumerate_open_with(&Probe, &env, "fixture.png").is_empty());
}

#[test]
fn lower_data_removal_keeps_a_higher_cached_handler() {
    let mut env = Env::default();
    env.put(
        "fixture-data-high/applications/mimeinfo.cache",
        "[MIME Cache]\nimage/png=fixture.desktop;\n",
    );
    env.put(
        "fixture-data-low/applications/mimeapps.list",
        "[Removed Associations]\nimage/png=fixture.desktop;\n",
    );
    env.put("fixture-data-high/applications/fixture.desktop", ENTRY);
    assert_eq!(enumerate_open_with(&Probe, &env, "fixture.png").len(), 1);
}

#[test]
fn higher_removal_blocks_cached_handler_and_same_file_addition() {
    for section in [
        "[Removed Associations]\nimage/png=fixture.desktop;\n",
        "[Added Associations]\nimage/png=fixture.desktop;\n[Removed Associations]\nimage/png=fixture.desktop;\n",
    ] {
        let mut env = Env::default();
        env.put("fixture-config-high/mimeapps.list", section);
        env.put(
            "fixture-data-high/applications/mimeinfo.cache",
            "[MIME Cache]\nimage/png=fixture.desktop;\n",
        );
        env.put("fixture-data-high/applications/fixture.desktop", ENTRY);
        assert!(enumerate_open_with(&Probe, &env, "fixture.png").is_empty());
    }
}

#[test]
fn higher_default_survives_lower_removal_and_stays_before_added_handlers() {
    let mut env = Env::default();
    env.put("fixture-config-high/mimeapps.list",
        "[Default Applications]\nimage/png=fixture.desktop;\n[Added Associations]\nimage/png=other.desktop;\n");
    env.put(
        "fixture-config-low/mimeapps.list",
        "[Removed Associations]\nimage/png=fixture.desktop;\n",
    );
    env.put("fixture-data-high/applications/fixture.desktop", ENTRY);
    env.put("fixture-data-high/applications/other.desktop", ENTRY);
    let apps = enumerate_open_with(&Probe, &env, "fixture.png");
    assert_eq!(
        apps.iter().map(|app| app.id.as_str()).collect::<Vec<_>>(),
        ["fixture.desktop", "other.desktop"]
    );
}
