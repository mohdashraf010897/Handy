//! Tauri commands for the Flow settings UI.

use tauri::AppHandle;

use crate::flow::context::builtin_app_tones;
use crate::flow::settings::{self, AppToneRule, FlowSettings};

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
