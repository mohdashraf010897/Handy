//! Tauri commands for the Flow settings UI.

use serde::Serialize;
use specta::Type;
use tauri::AppHandle;

use crate::flow::context::{builtin_app_tones, last_app, matching_rule, AppContext};
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
    last_flow_app(last_app()?, &settings::get(&app))
}

fn last_flow_app(context: AppContext, settings: &FlowSettings) -> Option<LastFlowApp> {
    let app_id = context.app_id?;
    let matched = matching_rule(&app_id, settings);
    Some(LastFlowApp {
        tone: matched.map(|(_, tone, _)| tone).unwrap_or_default(),
        matched_rule: matched.map(|(pattern, _, _)| pattern.to_string()),
        user_rule: matched.is_some_and(|(_, _, user)| user),
        app_name: context.app_name,
        app_id,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::PoisonError;

    use super::*;
    use crate::flow::context::{remember, LAST_APP_TEST_LOCK};

    const OWN: [&str; 2] = ["com.pais.handy", "handy"];

    fn context(app_id: &str, app_name: &str) -> AppContext {
        AppContext {
            app_id: Some(app_id.into()),
            app_name: Some(app_name.into()),
        }
    }

    /// What `get_flow_last_app` returns for the remembered app.
    fn command_result(settings: &FlowSettings) -> Option<LastFlowApp> {
        last_flow_app(last_app()?, settings)
    }

    #[test]
    fn last_app_reports_the_remembered_app_and_its_rule() {
        let _lock = LAST_APP_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let slack = context("com.tinyspeck.slackmacgap", "Slack");
        remember(Some(&slack), &OWN);
        // Pressing the hotkey while this app is frontmost changes nothing.
        remember(Some(&context("com.pais.handy", "Handy")), &OWN);
        remember(Some(&context("Handy.exe", "Handy")), &OWN);

        let off = FlowSettings::default();
        assert_eq!(
            command_result(&off),
            Some(LastFlowApp {
                app_id: "com.tinyspeck.slackmacgap".into(),
                app_name: Some("Slack".into()),
                tone: Tone::Casual,
                matched_rule: Some("com.tinyspeck.slackmacgap".into()),
                user_rule: false,
            }),
            "built-in rule is reported even while tone is off"
        );

        let user = FlowSettings {
            tone_enabled: true,
            app_tones: vec![AppToneRule {
                app_id: "com.tinyspeck.*".into(),
                tone: Tone::Formal,
            }],
        };
        let result = command_result(&user).expect("remembered");
        assert_eq!(result.tone, Tone::Formal);
        assert_eq!(result.matched_rule.as_deref(), Some("com.tinyspeck.*"));
        assert!(result.user_rule);

        remember(Some(&context("com.example.unknown", "Unknown")), &OWN);
        let result = command_result(&user).expect("remembered");
        assert_eq!(result.app_id, "com.example.unknown");
        assert_eq!((result.tone, result.matched_rule), (Tone::Default, None));
    }

    #[test]
    fn context_without_an_id_is_not_reported() {
        let name_only = AppContext {
            app_id: None,
            app_name: Some("Slack".into()),
        };
        assert_eq!(last_flow_app(name_only, &FlowSettings::default()), None);
    }
}
