// SPDX-License-Identifier: GPL-3.0-only
//! Versioned named launch profile schema and validation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::settings::normalize_name;

use super::json::{self, Json};
use super::limits::*;

/// Supported platform tokens for optional profile applicability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProfilePlatform {
    Linux,
    Macos,
    Windows,
}

impl ProfilePlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match normalize_name(raw).as_str() {
            "linux" => Some(Self::Linux),
            "macos" | "darwin" | "osx" => Some(Self::Macos),
            "windows" | "win32" => Some(Self::Windows),
            _ => None,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn current() -> Self {
        Self::Linux
    }

    #[cfg(target_os = "macos")]
    pub fn current() -> Self {
        Self::Macos
    }

    #[cfg(windows)]
    pub fn current() -> Self {
        Self::Windows
    }
}

/// One explicit command to exec instead of an interactive shell.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileCommand {
    pub program: String,
    pub args: Vec<String>,
    pub(crate) preserved: BTreeMap<String, Json>,
}

/// Launch-time fields: shell, command, cwd, and bounded env overrides.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileLaunch {
    pub shell: Option<String>,
    pub command: Option<ProfileCommand>,
    pub working_directory: Option<String>,
    pub env: BTreeMap<String, String>,
    pub(crate) preserved: BTreeMap<String, Json>,
}

/// Appearance overrides that fall through to global settings when absent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileAppearance {
    pub theme: Option<String>,
    pub visual: Option<String>,
    pub font: Option<String>,
    pub font_family: Option<String>,
    pub font_weight: Option<String>,
    pub font_size_px: Option<f32>,
    pub title: Option<String>,
    pub(crate) preserved: BTreeMap<String, Json>,
}

/// Cursor overrides.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileCursor {
    pub style: Option<String>,
    pub blink: Option<String>,
    pub(crate) preserved: BTreeMap<String, Json>,
}

/// Renderer/effect overrides.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileEffects {
    pub render_quality: Option<String>,
    pub bloom: Option<bool>,
    pub crt: Option<bool>,
    pub retro: Option<bool>,
    pub(crate) preserved: BTreeMap<String, Json>,
}

/// Optional saved-layout reference for future workspace restore.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileLayout {
    pub saved_layout: Option<String>,
    pub(crate) preserved: BTreeMap<String, Json>,
}

/// A versioned, no-secret named launch profile.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchProfile {
    pub schema_version: u32,
    pub name: String,
    pub display_name: Option<String>,
    pub platforms: Option<BTreeSet<ProfilePlatform>>,
    pub launch: ProfileLaunch,
    pub appearance: ProfileAppearance,
    pub cursor: ProfileCursor,
    pub effects: ProfileEffects,
    pub layout: ProfileLayout,
    pub connection: Option<String>,
    /// Unknown keys at this object, preserved verbatim for forward compatibility.
    /// Nested schema objects keep their own `preserved` maps.
    pub(crate) preserved: BTreeMap<String, Json>,
}

/// Outcome of validating or parsing one profile document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    Malformed(String),
    UnsupportedSchemaVersion(u32),
    InvalidName(String),
    RejectedSecret(String),
    LimitExceeded(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(message) => write!(f, "malformed profile: {message}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(f, "unsupported profile schema version {version}")
            }
            Self::InvalidName(message) => write!(f, "invalid profile name: {message}"),
            Self::RejectedSecret(message) => write!(f, "profile stores secrets: {message}"),
            Self::LimitExceeded(message) => write!(f, "profile limit exceeded: {message}"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl LaunchProfile {
    pub fn new(name: impl Into<String>) -> Result<Self, ProfileError> {
        let name = validate_profile_name(&name.into())?;
        Ok(Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            name,
            display_name: None,
            platforms: None,
            launch: ProfileLaunch::default(),
            appearance: ProfileAppearance::default(),
            cursor: ProfileCursor::default(),
            effects: ProfileEffects::default(),
            layout: ProfileLayout::default(),
            connection: None,
            preserved: BTreeMap::new(),
        })
    }

    pub(crate) fn to_json(&self) -> Json {
        let mut entries: Vec<(String, Json)> = vec![
            (
                "schema_version".to_owned(),
                Json::Num(f64::from(self.schema_version)),
            ),
            ("name".to_owned(), Json::Str(self.name.clone())),
        ];
        push_opt_str(&mut entries, "display_name", self.display_name.as_deref());
        if let Some(platforms) = &self.platforms {
            entries.push((
                "platforms".to_owned(),
                Json::Arr(
                    platforms
                        .iter()
                        .map(|platform| Json::Str(platform.as_str().to_owned()))
                        .collect(),
                ),
            ));
        }
        entries.push(("launch".to_owned(), launch_to_json(&self.launch)));
        entries.push((
            "appearance".to_owned(),
            appearance_to_json(&self.appearance),
        ));
        entries.push(("cursor".to_owned(), cursor_to_json(&self.cursor)));
        entries.push(("effects".to_owned(), effects_to_json(&self.effects)));
        entries.push(("layout".to_owned(), layout_to_json(&self.layout)));
        push_opt_str(&mut entries, "connection", self.connection.as_deref());
        for (key, value) in &self.preserved {
            entries.push((key.clone(), value.clone()));
        }
        json::obj_pairs(entries)
    }

    pub fn serialize_pretty(&self) -> String {
        json::to_pretty(&self.to_json())
    }

    /// Parse and validate one profile document.
    ///
    /// This is the single validation authority: writes re-serialize into this
    /// parser so programmatic structs cannot persist secrets or over-limit
    /// fields that a file parse would reject.
    pub fn parse_json(text: &str, expected_name: Option<&str>) -> Result<Self, ProfileError> {
        let root = json::parse(text).map_err(ProfileError::Malformed)?;
        reject_secret_material(&root)?;
        let obj = match &root {
            Json::Obj(entries) => entries,
            _ => {
                return Err(ProfileError::Malformed(
                    "profile root must be an object".to_owned(),
                ));
            }
        };

        let mut known = BTreeSet::new();
        let schema_version = read_u32(obj, "schema_version", &mut known)?.unwrap_or(0);
        if schema_version == 0 {
            return Err(ProfileError::Malformed("missing schema_version".to_owned()));
        }
        if schema_version > PROFILE_SCHEMA_VERSION {
            return Err(ProfileError::UnsupportedSchemaVersion(schema_version));
        }

        let name = read_string(obj, "name", &mut known)?
            .ok_or_else(|| ProfileError::Malformed("missing name".to_owned()))?;
        let name = validate_profile_name(&name)?;
        if let Some(expected) = expected_name
            && expected != name
        {
            return Err(ProfileError::InvalidName(format!(
                "file name {expected:?} does not match document name {name:?}"
            )));
        }

        let display_name =
            read_bounded_string(obj, "display_name", &mut known, MAX_PROFILE_FIELD_CHARS)?;
        let platforms = read_platforms(obj, &mut known)?;
        let launch = read_launch(obj, &mut known)?;
        let appearance = read_appearance(obj, &mut known)?;
        let cursor = read_cursor(obj, &mut known)?;
        let effects = read_effects(obj, &mut known)?;
        let layout = read_layout(obj, &mut known)?;
        let connection =
            read_bounded_string(obj, "connection", &mut known, MAX_PROFILE_FIELD_CHARS)?;

        let preserved = obj
            .iter()
            .filter(|(key, _)| !known.contains(key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        Ok(Self {
            schema_version,
            name,
            display_name,
            platforms,
            launch,
            appearance,
            cursor,
            effects,
            layout,
            connection,
            preserved,
        })
    }

    /// Validate a constructed profile by re-serializing through [`Self::parse_json`].
    pub fn validate(&self) -> Result<(), ProfileError> {
        self.validated_serialization().map(|_| ())
    }

    pub(crate) fn validated_serialization(&self) -> Result<String, ProfileError> {
        let serialized = self.serialize_pretty();
        let reparsed = Self::parse_json(&serialized, Some(&self.name))?;
        if reparsed != *self {
            return Err(ProfileError::Malformed(
                "profile contains a value that cannot round-trip safely".to_owned(),
            ));
        }
        Ok(serialized)
    }

    pub fn applies_on_current_platform(&self) -> bool {
        match &self.platforms {
            None => true,
            Some(set) if set.is_empty() => true,
            Some(set) => set.contains(&ProfilePlatform::current()),
        }
    }
}

pub fn validate_profile_name(raw: &str) -> Result<String, ProfileError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProfileError::InvalidName("empty".to_owned()));
    }
    if trimmed.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(ProfileError::LimitExceeded(format!(
            "name exceeds {MAX_PROFILE_NAME_CHARS} characters"
        )));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ProfileError::InvalidName(
            "use ASCII letters, digits, hyphen, underscore, or dot".to_owned(),
        ));
    }
    Ok(trimmed.to_owned())
}

pub fn profile_file_name(name: &str) -> String {
    format!("{name}{PROFILE_FILE_SUFFIX}")
}

pub fn profile_name_from_path(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    file_name
        .strip_suffix(PROFILE_FILE_SUFFIX)
        .map(str::to_owned)
}

fn push_opt_str(entries: &mut Vec<(String, Json)>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        entries.push((key.to_owned(), Json::Str(value.to_owned())));
    }
}

fn launch_to_json(launch: &ProfileLaunch) -> Json {
    let mut entries = Vec::new();
    push_opt_str(&mut entries, "shell", launch.shell.as_deref());
    if let Some(command) = &launch.command {
        let mut command_entries = vec![
            ("program".to_owned(), Json::Str(command.program.clone())),
            (
                "args".to_owned(),
                Json::Arr(
                    command
                        .args
                        .iter()
                        .map(|arg| Json::Str(arg.clone()))
                        .collect(),
                ),
            ),
        ];
        append_preserved(&mut command_entries, &command.preserved);
        entries.push(("command".to_owned(), json::obj_pairs(command_entries)));
    }
    push_opt_str(
        &mut entries,
        "working_directory",
        launch.working_directory.as_deref(),
    );
    if !launch.env.is_empty() {
        entries.push((
            "env".to_owned(),
            Json::Obj(
                launch
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), Json::Str(value.clone())))
                    .collect(),
            ),
        ));
    }
    append_preserved(&mut entries, &launch.preserved);
    json::obj_pairs(entries)
}

fn appearance_to_json(appearance: &ProfileAppearance) -> Json {
    let mut entries = Vec::new();
    push_opt_str(&mut entries, "theme", appearance.theme.as_deref());
    push_opt_str(&mut entries, "visual", appearance.visual.as_deref());
    push_opt_str(&mut entries, "font", appearance.font.as_deref());
    push_opt_str(
        &mut entries,
        "font_family",
        appearance.font_family.as_deref(),
    );
    push_opt_str(
        &mut entries,
        "font_weight",
        appearance.font_weight.as_deref(),
    );
    if let Some(size) = appearance.font_size_px {
        entries.push(("font_size_px".to_owned(), Json::Num(f64::from(size))));
    }
    push_opt_str(&mut entries, "title", appearance.title.as_deref());
    append_preserved(&mut entries, &appearance.preserved);
    json::obj_pairs(entries)
}

fn cursor_to_json(cursor: &ProfileCursor) -> Json {
    let mut entries = Vec::new();
    push_opt_str(&mut entries, "style", cursor.style.as_deref());
    push_opt_str(&mut entries, "blink", cursor.blink.as_deref());
    append_preserved(&mut entries, &cursor.preserved);
    json::obj_pairs(entries)
}

fn effects_to_json(effects: &ProfileEffects) -> Json {
    let mut entries = Vec::new();
    push_opt_str(
        &mut entries,
        "render_quality",
        effects.render_quality.as_deref(),
    );
    push_bool(&mut entries, "bloom", effects.bloom);
    push_bool(&mut entries, "crt", effects.crt);
    push_bool(&mut entries, "retro", effects.retro);
    append_preserved(&mut entries, &effects.preserved);
    json::obj_pairs(entries)
}

fn layout_to_json(layout: &ProfileLayout) -> Json {
    let mut entries = Vec::new();
    push_opt_str(&mut entries, "saved_layout", layout.saved_layout.as_deref());
    append_preserved(&mut entries, &layout.preserved);
    json::obj_pairs(entries)
}

fn append_preserved(entries: &mut Vec<(String, Json)>, preserved: &BTreeMap<String, Json>) {
    entries.extend(
        preserved
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
}

fn collect_unknown(entries: &[(String, Json)], known: &BTreeSet<String>) -> BTreeMap<String, Json> {
    entries
        .iter()
        .filter(|(key, _)| !known.contains(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn push_bool(entries: &mut Vec<(String, Json)>, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        entries.push((key.to_owned(), Json::Bool(value)));
    }
}

fn read_u32(
    obj: &[(String, Json)],
    key: &str,
    known: &mut BTreeSet<String>,
) -> Result<Option<u32>, ProfileError> {
    known.insert(key.to_owned());
    match obj.iter().find(|(entry_key, _)| entry_key == key) {
        None => Ok(None),
        Some((_, Json::Num(value)))
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= 0.0
                && *value <= u32::MAX as f64 =>
        {
            Ok(Some(*value as u32))
        }
        Some((_, _)) => Err(ProfileError::Malformed(format!(
            "{key} must be a whole number"
        ))),
    }
}

fn read_string(
    obj: &[(String, Json)],
    key: &str,
    known: &mut BTreeSet<String>,
) -> Result<Option<String>, ProfileError> {
    known.insert(key.to_owned());
    Ok(match obj.iter().find(|(entry_key, _)| entry_key == key) {
        None => None,
        Some((_, Json::Str(value))) => Some(value.clone()),
        Some((_, Json::Null)) => None,
        Some((_, _)) => {
            return Err(ProfileError::Malformed(format!("{key} must be a string")));
        }
    })
}

fn read_bounded_string(
    obj: &[(String, Json)],
    key: &str,
    known: &mut BTreeSet<String>,
    max_chars: usize,
) -> Result<Option<String>, ProfileError> {
    let Some(value) = read_string(obj, key, known)? else {
        return Ok(None);
    };
    if value.chars().count() > max_chars {
        return Err(ProfileError::LimitExceeded(format!(
            "{key} exceeds {max_chars} characters"
        )));
    }
    Ok(Some(value))
}

fn read_platforms(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<Option<BTreeSet<ProfilePlatform>>, ProfileError> {
    known.insert("platforms".to_owned());
    let Some((_, value)) = obj.iter().find(|(key, _)| key == "platforms") else {
        return Ok(None);
    };
    let items = match value {
        Json::Arr(items) => items,
        Json::Null => return Ok(None),
        _ => {
            return Err(ProfileError::Malformed(
                "platforms must be an array".to_owned(),
            ));
        }
    };
    let mut set = BTreeSet::new();
    for item in items {
        let Json::Str(raw) = item else {
            return Err(ProfileError::Malformed(
                "platforms entries must be strings".to_owned(),
            ));
        };
        let Some(platform) = ProfilePlatform::parse(raw) else {
            return Err(ProfileError::Malformed(format!("unknown platform {raw:?}")));
        };
        set.insert(platform);
    }
    Ok(Some(set))
}

fn read_launch(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<ProfileLaunch, ProfileError> {
    known.insert("launch".to_owned());
    let Some((_, value)) = obj.iter().find(|(key, _)| key == "launch") else {
        return Ok(ProfileLaunch::default());
    };
    let entries = match value {
        Json::Obj(entries) => entries,
        Json::Null => return Ok(ProfileLaunch::default()),
        _ => {
            return Err(ProfileError::Malformed(
                "launch must be an object".to_owned(),
            ));
        }
    };
    let mut launch_known = BTreeSet::new();
    let shell = read_bounded_string(entries, "shell", &mut launch_known, MAX_PROFILE_FIELD_CHARS)?;
    let working_directory = read_bounded_string(
        entries,
        "working_directory",
        &mut launch_known,
        MAX_PROFILE_FIELD_CHARS,
    )?;
    let command = read_command(entries, &mut launch_known)?;
    let env = read_env(entries, &mut launch_known)?;
    let preserved = collect_unknown(entries, &launch_known);
    Ok(ProfileLaunch {
        shell,
        command,
        working_directory,
        env,
        preserved,
    })
}

fn read_command(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<Option<ProfileCommand>, ProfileError> {
    known.insert("command".to_owned());
    let Some((_, value)) = obj.iter().find(|(key, _)| key == "command") else {
        return Ok(None);
    };
    let entries = match value {
        Json::Obj(entries) => entries,
        Json::Null => return Ok(None),
        _ => {
            return Err(ProfileError::Malformed(
                "command must be an object".to_owned(),
            ));
        }
    };
    let mut command_known = BTreeSet::new();
    let program = read_bounded_string(
        entries,
        "program",
        &mut command_known,
        MAX_PROFILE_FIELD_CHARS,
    )?
    .ok_or_else(|| ProfileError::Malformed("command.program is required".to_owned()))?;
    command_known.insert("args".to_owned());
    let args = match entries.iter().find(|(key, _)| key == "args") {
        None => Vec::new(),
        Some((_, Json::Arr(items))) => {
            if items.len() > MAX_PROFILE_COMMAND_ARGS {
                return Err(ProfileError::LimitExceeded(format!(
                    "command args exceed {MAX_PROFILE_COMMAND_ARGS}"
                )));
            }
            items
                .iter()
                .map(|item| match item {
                    Json::Str(value) => {
                        if value.chars().count() > MAX_PROFILE_FIELD_CHARS {
                            Err(ProfileError::LimitExceeded(
                                "command arg too long".to_owned(),
                            ))
                        } else {
                            Ok(value.clone())
                        }
                    }
                    _ => Err(ProfileError::Malformed(
                        "command args must be strings".to_owned(),
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        Some((_, Json::Null)) => Vec::new(),
        Some((_, _)) => {
            return Err(ProfileError::Malformed(
                "command args must be an array".to_owned(),
            ));
        }
    };
    let preserved = collect_unknown(entries, &command_known);
    Ok(Some(ProfileCommand {
        program,
        args,
        preserved,
    }))
}

fn read_env(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<BTreeMap<String, String>, ProfileError> {
    known.insert("env".to_owned());
    let Some((_, value)) = obj.iter().find(|(key, _)| key == "env") else {
        return Ok(BTreeMap::new());
    };
    let entries = match value {
        Json::Obj(entries) => entries,
        Json::Null => return Ok(BTreeMap::new()),
        _ => return Err(ProfileError::Malformed("env must be an object".to_owned())),
    };
    if entries.len() > MAX_PROFILE_ENV_ENTRIES {
        return Err(ProfileError::LimitExceeded(format!(
            "env exceeds {MAX_PROFILE_ENV_ENTRIES} entries"
        )));
    }
    let mut out = BTreeMap::new();
    for (key, value) in entries {
        if key.chars().count() > MAX_PROFILE_FIELD_CHARS {
            return Err(ProfileError::LimitExceeded("env key too long".to_owned()));
        }
        reject_secret_env_key(key)?;
        let Json::Str(raw) = value else {
            return Err(ProfileError::Malformed(
                "env values must be strings".to_owned(),
            ));
        };
        if raw.chars().count() > MAX_PROFILE_ENV_VALUE_CHARS {
            return Err(ProfileError::LimitExceeded("env value too long".to_owned()));
        }
        reject_secret_env_value(raw)?;
        out.insert(key.clone(), raw.clone());
    }
    Ok(out)
}

fn read_appearance(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<ProfileAppearance, ProfileError> {
    read_nested_object(obj, "appearance", known, |entries, nested_known| {
        let value = ProfileAppearance {
            theme: read_bounded_string(entries, "theme", nested_known, MAX_PROFILE_FIELD_CHARS)?,
            visual: read_bounded_string(entries, "visual", nested_known, MAX_PROFILE_FIELD_CHARS)?,
            font: read_bounded_string(entries, "font", nested_known, MAX_PROFILE_FIELD_CHARS)?,
            font_family: read_bounded_string(
                entries,
                "font_family",
                nested_known,
                MAX_PROFILE_FIELD_CHARS,
            )?,
            font_weight: read_bounded_string(
                entries,
                "font_weight",
                nested_known,
                MAX_PROFILE_FIELD_CHARS,
            )?,
            font_size_px: read_f32(entries, "font_size_px", nested_known)?,
            title: read_bounded_string(entries, "title", nested_known, MAX_PROFILE_FIELD_CHARS)?,
            preserved: BTreeMap::new(),
        };
        Ok(ProfileAppearance {
            preserved: collect_unknown(entries, nested_known),
            ..value
        })
    })
}

fn read_cursor(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<ProfileCursor, ProfileError> {
    read_nested_object(obj, "cursor", known, |entries, nested_known| {
        let value = ProfileCursor {
            style: read_bounded_string(entries, "style", nested_known, MAX_PROFILE_FIELD_CHARS)?,
            blink: read_bounded_string(entries, "blink", nested_known, MAX_PROFILE_FIELD_CHARS)?,
            preserved: BTreeMap::new(),
        };
        Ok(ProfileCursor {
            preserved: collect_unknown(entries, nested_known),
            ..value
        })
    })
}

fn read_effects(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<ProfileEffects, ProfileError> {
    read_nested_object(obj, "effects", known, |entries, nested_known| {
        let value = ProfileEffects {
            render_quality: read_bounded_string(
                entries,
                "render_quality",
                nested_known,
                MAX_PROFILE_FIELD_CHARS,
            )?,
            bloom: read_bool(entries, "bloom", nested_known)?,
            crt: read_bool(entries, "crt", nested_known)?,
            retro: read_bool(entries, "retro", nested_known)?,
            preserved: BTreeMap::new(),
        };
        Ok(ProfileEffects {
            preserved: collect_unknown(entries, nested_known),
            ..value
        })
    })
}

fn read_layout(
    obj: &[(String, Json)],
    known: &mut BTreeSet<String>,
) -> Result<ProfileLayout, ProfileError> {
    read_nested_object(obj, "layout", known, |entries, nested_known| {
        let value = ProfileLayout {
            saved_layout: read_bounded_string(
                entries,
                "saved_layout",
                nested_known,
                MAX_PROFILE_FIELD_CHARS,
            )?,
            preserved: BTreeMap::new(),
        };
        Ok(ProfileLayout {
            preserved: collect_unknown(entries, nested_known),
            ..value
        })
    })
}

fn read_nested_object<T>(
    obj: &[(String, Json)],
    key: &str,
    known: &mut BTreeSet<String>,
    read: impl FnOnce(&[(String, Json)], &mut BTreeSet<String>) -> Result<T, ProfileError>,
) -> Result<T, ProfileError> {
    known.insert(key.to_owned());
    let Some((_, value)) = obj.iter().find(|(entry_key, _)| entry_key == key) else {
        return read(&[], &mut BTreeSet::new());
    };
    match value {
        Json::Obj(entries) => read(entries, &mut BTreeSet::new()),
        Json::Null => read(&[], &mut BTreeSet::new()),
        _ => Err(ProfileError::Malformed(format!("{key} must be an object"))),
    }
}

fn read_f32(
    obj: &[(String, Json)],
    key: &str,
    known: &mut BTreeSet<String>,
) -> Result<Option<f32>, ProfileError> {
    known.insert(key.to_owned());
    match obj.iter().find(|(entry_key, _)| entry_key == key) {
        None => Ok(None),
        Some((_, Json::Num(value))) if value.is_finite() => Ok(Some(*value as f32)),
        Some((_, Json::Null)) => Ok(None),
        Some((_, _)) => Err(ProfileError::Malformed(format!("{key} must be a number"))),
    }
}

fn read_bool(
    obj: &[(String, Json)],
    key: &str,
    known: &mut BTreeSet<String>,
) -> Result<Option<bool>, ProfileError> {
    known.insert(key.to_owned());
    match obj.iter().find(|(entry_key, _)| entry_key == key) {
        None => Ok(None),
        Some((_, Json::Bool(value))) => Ok(Some(*value)),
        Some((_, Json::Null)) => Ok(None),
        Some((_, _)) => Err(ProfileError::Malformed(format!("{key} must be a boolean"))),
    }
}

fn reject_secret_env_key(key: &str) -> Result<(), ProfileError> {
    let normalized = normalize_name(key);
    const BLOCKED: &[&str] = &[
        "password",
        "secret",
        "token",
        "credential",
        "apikey",
        "privatekey",
        "identityfile",
    ];
    if BLOCKED.iter().any(|blocked| normalized.contains(blocked)) {
        return Err(ProfileError::RejectedSecret(format!(
            "environment key {key:?} is not allowed in profiles"
        )));
    }
    Ok(())
}

fn reject_secret_env_value(value: &str) -> Result<(), ProfileError> {
    if value.contains("BEGIN") && value.contains("PRIVATE KEY") {
        return Err(ProfileError::RejectedSecret(
            "private key material is not allowed in profiles".to_owned(),
        ));
    }
    Ok(())
}

fn reject_secret_material(value: &Json) -> Result<(), ProfileError> {
    match value {
        Json::Obj(entries) => {
            for (key, value) in entries {
                reject_secret_env_key(key)?;
                reject_secret_material(value)?;
            }
        }
        Json::Arr(items) => {
            for item in items {
                reject_secret_material(item)?;
            }
        }
        Json::Str(value) => reject_secret_env_value(value)?,
        Json::Null | Json::Bool(_) | Json::Num(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_unknown_keys_at_every_owned_object() {
        let text = r#"{
  "schema_version": 1,
  "name": "dev",
  "future_flag": true,
  "launch": {
    "future_launch": {"enabled": true},
    "command": {"program": "echo", "args": [], "future_command": 7}
  },
  "appearance": {"future_appearance": "kept"},
  "cursor": {"future_cursor": false},
  "effects": {"future_effect": 0.5},
  "layout": {"future_layout": [1, 2]}
}"#;
        let profile = LaunchProfile::parse_json(text, Some("dev")).expect("parse");
        assert_eq!(
            profile.preserved.get("future_flag"),
            Some(&Json::Bool(true))
        );
        assert!(profile.launch.preserved.contains_key("future_launch"));
        assert!(
            profile
                .launch
                .command
                .as_ref()
                .expect("command")
                .preserved
                .contains_key("future_command")
        );
        let reserialized = profile.serialize_pretty();
        for key in [
            "future_flag",
            "future_launch",
            "future_command",
            "future_appearance",
            "future_cursor",
            "future_effect",
            "future_layout",
        ] {
            assert!(reserialized.contains(&format!("\"{key}\"")), "{key}");
        }
        let reparsed = LaunchProfile::parse_json(&reserialized, Some("dev")).expect("reparse");
        assert_eq!(reparsed, profile);
    }

    #[test]
    fn rejects_secret_env_keys_and_values() {
        let text = r#"{
  "schema_version": 1,
  "name": "dev",
  "launch": { "env": { "API_TOKEN": "abc" } }
}"#;
        assert!(matches!(
            LaunchProfile::parse_json(text, Some("dev")),
            Err(ProfileError::RejectedSecret(_))
        ));

        let private_key_marker = ["BEGIN RSA PRIVATE", " KEY"].concat();
        let text = format!(
            r#"{{
  "schema_version": 1,
  "name": "dev",
  "launch": {{ "env": {{ "SAFE": "{private_key_marker}" }} }}
}}"#
        );
        assert!(matches!(
            LaunchProfile::parse_json(&text, Some("dev")),
            Err(ProfileError::RejectedSecret(_))
        ));
    }

    #[test]
    fn rejects_secret_shaped_unknown_fields_and_fractional_versions() {
        let blocked_key = ["pass", "word"].concat();
        let secret = format!(
            r#"{{
  "schema_version": 1,
  "name": "dev",
  "future": {{"{blocked_key}": "synthetic"}}
}}"#
        );
        assert!(matches!(
            LaunchProfile::parse_json(&secret, Some("dev")),
            Err(ProfileError::RejectedSecret(_))
        ));

        let nested_command = format!(
            r#"{{
  "schema_version": 1,
  "name": "dev",
  "launch": {{
    "command": {{"program": "echo", "args": [], "{blocked_key}": "synthetic"}}
  }}
}}"#
        );
        assert!(matches!(
            LaunchProfile::parse_json(&nested_command, Some("dev")),
            Err(ProfileError::RejectedSecret(_))
        ));

        let nested_appearance = r#"{
  "schema_version": 1,
  "name": "dev",
  "appearance": {"future_token": "synthetic"}
}"#;
        assert!(matches!(
            LaunchProfile::parse_json(nested_appearance, Some("dev")),
            Err(ProfileError::RejectedSecret(_))
        ));

        let fractional = r#"{"schema_version": 1.5, "name": "dev"}"#;
        assert!(matches!(
            LaunchProfile::parse_json(fractional, Some("dev")),
            Err(ProfileError::Malformed(_))
        ));
    }
}
