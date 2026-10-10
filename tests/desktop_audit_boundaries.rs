// SPDX-License-Identifier: GPL-3.0-only
// Project-authored maps never read the filesystem or launch an application.
use odytty::desktop::{DesktopEnv, MimeProbe, enumerate_open_with};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

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
fn derived_desktop_candidates_never_read_parent_components() {
    let mut env = Env::default();
    env.associate("..-outside.desktop");
    assert!(enumerate_open_with(&Probe, &env, "fixture.png").is_empty());
    for path in env.reads.borrow().iter() {
        assert!(
            !path
                .components()
                .any(|part| matches!(part, Component::ParentDir)),
            "derived candidate escaped its applications root"
        );
    }
}

#[test]
fn mixed_dash_directory_names_resolve_their_desktop_id() {
    let mut env = Env::default();
    env.associate("foo-bar-editor.desktop");
    env.put(
        "fixture-data-high/applications/foo-bar/editor.desktop",
        ENTRY,
    );
    let apps = enumerate_open_with(&Probe, &env, "fixture.png");
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].id, "foo-bar-editor.desktop");
    assert_eq!(apps[0].argv, ["fixture-viewer", "fixture.png"]);
}

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
fn desktop_type_ignores_whitespace_after_its_separator() {
    let mut env = Env::default();
    env.associate("fixture.desktop");
    env.put(
        "fixture-data-high/applications/fixture.desktop",
        "[Desktop Entry]\nType= Application\nName=Fixture\nExec=fixture-viewer %f\n",
    );
    let apps = enumerate_open_with(&Probe, &env, "fixture.png");
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "Fixture");
}

#[test]
fn desktop_name_decodes_string_escapes_before_display() {
    let mut env = Env::default();
    env.associate("fixture.desktop");
    env.put("fixture-data-high/applications/fixture.desktop",
        "[Desktop Entry]\nType=Application\nName=Fixture\\sViewer\\tPanel\\\\Label\nExec=fixture-viewer %f\n");
    let apps = enumerate_open_with(&Probe, &env, "fixture.png");
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "Fixture Viewer\tPanel\\Label");
}
