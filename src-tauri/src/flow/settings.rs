//! Flow settings, persisted in `flow_settings.json` next to the app settings.
//!
//! Kept out of `AppSettings` on purpose: upstream's settings store drops keys
//! it does not know, and new fields there would conflict with upstream merges.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use log::warn;
use serde::{Deserialize, Deserializer, Serialize};
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct FlowSettings {
    /// Adjust the cleanup prompt to the frontmost app. Off by default (opt-in,
    /// so upgrading never changes cleanup output); no effect while
    /// post-processing is off.
    pub tone_enabled: bool,
    /// User rules; they take precedence over the built-in app list.
    #[serde(deserialize_with = "lenient_rules")]
    pub app_tones: Vec<AppToneRule>,
}

/// Parses rules one by one so a single bad rule cannot discard the others
/// (e.g. a tone written by a newer build): an unknown tone falls back to
/// `Default` for that rule, and a rule without a usable `app_id` is dropped.
fn lenient_rules<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<AppToneRule>, D::Error> {
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut rules = Vec::with_capacity(raw.len());
    for value in raw {
        let Some(app_id) = value.get("app_id").and_then(|v| v.as_str()) else {
            warn!("flow: skipping an app tone rule without an app id");
            continue;
        };
        let tone = match value.get("tone").map(Tone::deserialize) {
            Some(Ok(tone)) => tone,
            _ => {
                warn!("flow: unknown tone in an app tone rule; using the default tone");
                Tone::Default
            }
        };
        rules.push(AppToneRule {
            app_id: app_id.to_string(),
            tone,
        });
    }
    Ok(rules)
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

/// Loads settings from `path`. A missing file yields defaults; a corrupt file
/// is moved aside to `<file>.corrupt-<unix ms>` and defaults are used. A file
/// that exists but cannot be read is an error, so callers never overwrite it.
pub fn load_from(path: &Path) -> Result<FlowSettings, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(FlowSettings::default()),
        Err(e) => return Err(format!("Cannot read {}: {e}", path.display())),
    };
    Ok(match serde_json::from_str(&contents) {
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
    })
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
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt-{millis}"));
    path.with_file_name(name)
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::portable::app_data_dir(app)
        .map(|dir| dir.join(FILE_NAME))
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))
}

static CACHE: RwLock<Option<FlowSettings>> = RwLock::new(None);

/// Cached settings, loading them on first use. Failed loads are not cached,
/// so a transient read error is retried rather than masked.
fn load_cached(app: &AppHandle) -> Result<FlowSettings, String> {
    if let Some(cached) = CACHE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        return Ok(cached.clone());
    }
    let loaded = load_from(&settings_path(app)?)?;
    let mut cache = CACHE.write().unwrap_or_else(PoisonError::into_inner);
    Ok(cache.get_or_insert(loaded).clone())
}

/// Current settings, read from disk once and cached. Falls back to defaults
/// (uncached) while the file cannot be read.
pub fn get(app: &AppHandle) -> FlowSettings {
    load_cached(app).unwrap_or_else(|e| {
        warn!("flow: {e}; using default settings");
        FlowSettings::default()
    })
}

/// Validates, persists and caches new settings. Refuses to save until the
/// existing file has loaded, so an unreadable file is never overwritten.
pub fn set(app: &AppHandle, settings: FlowSettings) -> Result<FlowSettings, String> {
    let settings = settings.validated()?;
    load_cached(app)?;
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
        let settings = load_from(&dir.path().join(FILE_NAME)).expect("load");
        assert_eq!(settings, FlowSettings::default());
        assert!(!settings.tone_enabled, "per-app tone is opt-in");
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join(FILE_NAME);
        save_to(&path, &sample()).expect("save");
        assert_eq!(load_from(&path).expect("load"), sample());
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn corrupt_file_loads_defaults_and_is_backed_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, "{ not json").expect("write");
        assert_eq!(load_from(&path).expect("load"), FlowSettings::default());
        assert!(!path.exists());
        let backups = backups(dir.path());
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(&backups[0]).expect("backup"),
            "{ not json"
        );
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("flow_settings.json.corrupt-"))
            })
            .collect()
    }

    #[test]
    fn a_second_corrupt_file_keeps_the_first_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, "first").expect("write");
        load_from(&path).expect("load");
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&path, "second").expect("write");
        load_from(&path).expect("load");
        let mut contents: Vec<String> = backups(dir.path())
            .iter()
            .map(|p| fs::read_to_string(p).expect("backup"))
            .collect();
        contents.sort();
        assert_eq!(contents, ["first", "second"]);
    }

    #[test]
    fn unreadable_file_is_an_error_and_left_in_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        // A directory exists at the path but cannot be read as a file.
        fs::create_dir(&path).expect("mkdir");
        assert!(load_from(&path).is_err());
        assert!(path.is_dir());
        assert!(backups(dir.path()).is_empty());
    }

    #[test]
    fn unknown_tone_falls_back_for_that_rule_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        fs::write(
            &path,
            r#"{"tone_enabled":true,"app_tones":[
                {"app_id":"slack","tone":"casual"},
                {"app_id":"notes","tone":"poetic"},
                {"tone":"formal"},
                {"app_id":"code","tone":"code"}
            ]}"#,
        )
        .expect("write");
        let settings = load_from(&path).expect("load");
        assert!(settings.tone_enabled);
        let rules: Vec<(&str, Tone)> = settings
            .app_tones
            .iter()
            .map(|r| (r.app_id.as_str(), r.tone))
            .collect();
        assert_eq!(
            rules,
            [
                ("slack", Tone::Casual),
                ("notes", Tone::Default),
                ("code", Tone::Code)
            ]
        );
        assert!(path.exists(), "a recoverable file is not moved aside");
    }

    #[test]
    fn wrong_shape_rules_still_count_as_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, r#"{"app_tones":"slack"}"#).expect("write");
        assert_eq!(load_from(&path).expect("load"), FlowSettings::default());
        assert_eq!(backups(dir.path()).len(), 1);
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
        let settings = load_from(&path).expect("load");
        assert!(!settings.tone_enabled);
        assert_eq!(settings.app_tones[0].tone, Tone::Casual);
    }

    #[test]
    fn saved_enabled_flag_survives_the_default_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, r#"{"tone_enabled":true,"app_tones":[]}"#).expect("write");
        assert!(load_from(&path).expect("load").tone_enabled);
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
