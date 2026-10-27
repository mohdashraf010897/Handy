//! Tauri commands for the Flow settings UI.

use serde::Serialize;
use specta::Type;
use tauri::AppHandle;

use crate::flow::context::{builtin_app_tones, last_app, matching_rule};
use crate::flow::settings::{self, AppToneRule, FlowSettings, Tone};

/// Current Flow settings (defaults if `flow_settings.json` is missing).
#[tauri::command]
#[specta::specta]
pub fn get_flow_settings(app: AppHandle) -> FlowSettings {
    settings::get(&app)
}

/// Validates and saves Flow settings; returns what was stored.
#[tauri::command]
#[specta::specta]
pub fn set_flow_settings(app: AppHandle, settings: FlowSettings) -> Result<FlowSettings, String> {
    settings::set(&app, settings)
}

/// The built-in app → tone presets, applied after the user's rules.
#[tauri::command]
#[specta::specta]
pub fn get_flow_builtin_app_tones() -> Vec<AppToneRule> {
    builtin_app_tones()
}

/// The last app dictated into, and the rule that currently applies to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct LastFlowApp {
    pub app_id: String,
    pub app_name: Option<String>,
    /// Tone the current rules give this app (even while tone is off).
    pub tone: Tone,
    /// Pattern of the matching rule; `None` when no rule matches.
    pub matched_rule: Option<String>,
    /// Whether the matching rule is a user rule rather than a built-in one.
    pub user_rule: bool,
}

/// The app that was frontmost at the last hotkey press (excluding this app),
/// held in memory only. `None` before the first dictation or on Linux.
#[tauri::command]
#[specta::specta]
pub fn get_flow_last_app(app: AppHandle) -> Option<LastFlowApp> {
    let context = last_app()?;
    let app_id = context.app_id?;
    let settings = settings::get(&app);
    let matched = matching_rule(&app_id, &settings);
    Some(LastFlowApp {
        tone: matched.map(|(_, tone, _)| tone).unwrap_or_default(),
        matched_rule: matched.map(|(pattern, _, _)| pattern.to_string()),
        user_rule: matched.is_some_and(|(_, _, user)| user),
        app_name: context.app_name,
        app_id,
    })
}
