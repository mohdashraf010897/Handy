//! Flow settings, persisted in `flow_settings.json` next to the app settings.
//!
//! Kept out of `AppSettings` on purpose: upstream's settings store drops keys
//! it does not know, and new fields there would conflict with upstream merges.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use log::warn;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;

const FILE_NAME: &str = "flow_settings.json";

/// Writing style asked of LLM cleanup for a given app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Leave the user's cleanup prompt unchanged.
    #[default]
    Default,
    /// Chat apps: conversational, light punctuation.
    Casual,
    /// Email and documents: complete sentences, professional register.
    Formal,
    /// Editors and terminals: keep identifiers, commands and paths verbatim.
    Code,
}

/// Maps an app (macOS bundle id or Windows exe name) to a tone. A trailing
/// `*` matches any suffix; matching ignores case and a `.exe` suffix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AppToneRule {
    pub app_id: String,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct FlowSettings {
    /// Adjust the cleanup prompt to the frontmost app. Has no effect while
    /// post-processing is off.
    pub tone_enabled: bool,
    /// User rules; they take precedence over the built-in app list.
    pub app_tones: Vec<AppToneRule>,
}

impl Default for FlowSettings {
    fn default() -> Self {
        Self {
            tone_enabled: true,
            app_tones: Vec::new(),
        }
    }
}

impl FlowSettings {
    /// Trims rule ids and rejects empty ones.
    pub fn validated(mut self) -> Result<Self, String> {
        for rule in &mut self.app_tones {
            let trimmed = rule.app_id.trim();
            if trimmed.is_empty() || trimmed == "*" {
                return Err("App tone rules need a non-empty app id".to_string());
            }
            rule.app_id = trimmed.to_string();
        }
        Ok(self)
    }
}

/// Loads settings from `path`. A missing file yields defaults; an unreadable
/// or corrupt file is moved aside to `<file>.corrupt` and defaults are used.
pub fn load_from(path: &Path) -> FlowSettings {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == ErrorKind::NotFound => return FlowSettings::default(),
        Err(e) => {
            warn!("flow: cannot read {}: {e}; using defaults", path.display());
            return FlowSettings::default();
        }
    };
    match serde_json::from_str(&contents) {
        Ok(settings) => settings,
        Err(e) => {
            let backup = corrupt_backup_path(path);
            warn!(
                "flow: {} is invalid ({e}); moved to {} and using defaults",
                path.display(),
                backup.display()
            );
            if let Err(e) = fs::rename(path, &backup) {
                warn!("flow: failed to back up corrupt settings: {e}");
            }
            FlowSettings::default()
        }
    }
}

/// Writes settings atomically (temp file + rename).
pub fn save_to(path: &Path, settings: &FlowSettings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize flow settings: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("Failed to replace {}: {e}", path.display()))
}

fn corrupt_backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".corrupt");
    path.with_file_name(name)
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::portable::app_data_dir(app)
        .map(|dir| dir.join(FILE_NAME))
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))
}

static CACHE: RwLock<Option<FlowSettings>> = RwLock::new(None);

/// Current settings, read from disk once and cached.
pub fn get(app: &AppHandle) -> FlowSettings {
    if let Some(cached) = CACHE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        return cached.clone();
    }
    let loaded = match settings_path(app) {
        Ok(path) => load_from(&path),
        Err(e) => {
            warn!("flow: {e}; using default settings");
            FlowSettings::default()
        }
    };
    let mut cache = CACHE.write().unwrap_or_else(PoisonError::into_inner);
    cache.get_or_insert(loaded).clone()
}

/// Validates, persists and caches new settings.
pub fn set(app: &AppHandle, settings: FlowSettings) -> Result<FlowSettings, String> {
    let settings = settings.validated()?;
    save_to(&settings_path(app)?, &settings)?;
    *CACHE.write().unwrap_or_else(PoisonError::into_inner) = Some(settings.clone());
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FlowSettings {
        FlowSettings {
            tone_enabled: false,
            app_tones: vec![AppToneRule {
                app_id: "com.example.app".into(),
                tone: Tone::Formal,
            }],
        }
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = load_from(&dir.path().join(FILE_NAME));
        assert_eq!(settings, FlowSettings::default());
        assert!(settings.tone_enabled);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join(FILE_NAME);
        save_to(&path, &sample()).expect("save");
        assert_eq!(load_from(&path), sample());
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn corrupt_file_loads_defaults_and_is_backed_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, "{ not json").expect("write");
        assert_eq!(load_from(&path), FlowSettings::default());
        assert!(!path.exists());
        let backup = dir.path().join("flow_settings.json.corrupt");
        assert_eq!(fs::read_to_string(backup).expect("backup"), "{ not json");
    }

    #[test]
    fn partial_file_fills_missing_fields_with_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        fs::write(
            &path,
            r#"{"app_tones":[{"app_id":"slack","tone":"casual"}],"future":1}"#,
        )
        .expect("write");
        let settings = load_from(&path);
        assert!(settings.tone_enabled);
        assert_eq!(settings.app_tones[0].tone, Tone::Casual);
    }

    #[test]
    fn validation_trims_ids_and_rejects_empty_ones() {
        let mut settings = sample();
        settings.app_tones[0].app_id = "  slack.exe ".into();
        let validated = settings.validated().expect("valid");
        assert_eq!(validated.app_tones[0].app_id, "slack.exe");

        let mut empty = sample();
        empty.app_tones[0].app_id = " ".into();
        assert!(empty.validated().is_err());
    }
}
