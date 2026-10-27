//! Frontmost-app capture at hotkey press, and the app → tone mapping.
//!
//! Only the app identity is read (macOS bundle id and name, Windows exe name);
//! window titles are never read because they routinely contain private data.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use crate::flow::settings::{AppToneRule, FlowSettings, Tone};

/// The app that was frontmost when dictation started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppContext {
    /// Bundle id on macOS (`com.tinyspeck.slackmacgap`), exe name on Windows
    /// (`slack.exe`).
    pub app_id: Option<String>,
    /// Display name, for diagnostics only.
    pub app_name: Option<String>,
}

/// Captures the frontmost app and how long the capture took, in microseconds.
pub fn capture_timed() -> (Option<AppContext>, u64) {
    let started = Instant::now();
    let context = capture();
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    (context, micros)
}

#[cfg(target_os = "macos")]
fn capture() -> Option<AppContext> {
    use objc2_app_kit::NSWorkspace;

    objc2::rc::autoreleasepool(|_| {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let context = AppContext {
            app_id: app.bundleIdentifier().map(|id| id.to_string()),
            app_name: app.localizedName().map(|name| name.to_string()),
        };
        non_empty(context)
    })
}

#[cfg(target_os = "windows")]
fn capture() -> Option<AppContext> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    // SAFETY: GetForegroundWindow takes no arguments and may return null.
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `window` is a window handle and `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid as *mut u32)) };
    if pid == 0 {
        return None;
    }
    // SAFETY: limited query access needs no privileges; failure is an Err.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` holds `len` u16s and `process` is an open handle.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `process` was opened above and is closed exactly once.
    if let Err(e) = unsafe { CloseHandle(process) } {
        log::debug!("flow: CloseHandle failed: {e}");
    }
    queried.ok()?;
    let path = String::from_utf16_lossy(buffer.get(..len as usize)?);
    let exe = std::path::Path::new(&path)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    non_empty(AppContext {
        app_id: Some(exe.clone()),
        app_name: Some(exe),
    })
}

/// Linux has no portable frontmost-app API (X11 vs. Wayland compositors).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture() -> Option<AppContext> {
    None
}

#[cfg_attr(
    all(not(test), not(any(target_os = "macos", target_os = "windows"))),
    expect(dead_code, reason = "Linux capture always returns None")
)]
fn non_empty(context: AppContext) -> Option<AppContext> {
    let has = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.trim().is_empty());
    (has(&context.app_id) || has(&context.app_name)).then_some(context)
}

/// Built-in app → tone presets. User rules in `flow_settings.json` win.
const BUILTIN_APP_TONES: &[(&str, Tone)] = &[
    // macOS chat
    ("com.tinyspeck.slackmacgap", Tone::Casual),
    ("com.apple.MobileSMS", Tone::Casual),
    ("net.whatsapp.WhatsApp", Tone::Casual),
    ("ru.keepcoder.Telegram", Tone::Casual),
    ("com.hnc.Discord", Tone::Casual),
    ("org.whispersystems.signal-desktop", Tone::Casual),
    ("com.microsoft.teams2", Tone::Casual),
    // macOS email and documents
    ("com.apple.mail", Tone::Formal),
    ("com.microsoft.Outlook", Tone::Formal),
    ("com.microsoft.Word", Tone::Formal),
    ("com.apple.iWork.Pages", Tone::Formal),
    // macOS editors and terminals
    ("com.microsoft.VSCode", Tone::Code),
    ("com.microsoft.VSCodeInsiders", Tone::Code),
    ("com.todesktop.230313mzl4w4u92", Tone::Code), // Cursor
    ("dev.zed.Zed", Tone::Code),
    ("com.apple.dt.Xcode", Tone::Code),
    ("com.jetbrains.*", Tone::Code),
    ("com.google.android.studio", Tone::Code),
    ("com.apple.Terminal", Tone::Code),
    ("com.googlecode.iterm2", Tone::Code),
    ("dev.warp.Warp-Stable", Tone::Code),
    ("com.mitchellh.ghostty", Tone::Code),
    ("net.kovidgoyal.kitty", Tone::Code),
    ("org.alacritty", Tone::Code),
    ("com.github.wez.wezterm", Tone::Code),
    // Windows chat
    ("slack", Tone::Casual),
    ("discord", Tone::Casual),
    ("telegram", Tone::Casual),
    ("whatsapp", Tone::Casual),
    ("signal", Tone::Casual),
    ("ms-teams", Tone::Casual),
    // Windows email and documents
    ("outlook", Tone::Formal),
    ("olk", Tone::Formal),
    ("winword", Tone::Formal),
    ("thunderbird", Tone::Formal),
    // Windows editors and terminals
    ("code", Tone::Code),
    ("code - insiders", Tone::Code),
    ("cursor", Tone::Code),
    ("zed", Tone::Code),
    ("devenv", Tone::Code),
    ("idea64", Tone::Code),
    ("pycharm64", Tone::Code),
    ("webstorm64", Tone::Code),
    ("windowsterminal", Tone::Code),
    ("wt", Tone::Code),
    ("powershell", Tone::Code),
    ("pwsh", Tone::Code),
    ("cmd", Tone::Code),
];

/// The built-in presets, as rules the settings UI can display.
pub fn builtin_app_tones() -> Vec<AppToneRule> {
    BUILTIN_APP_TONES
        .iter()
        .map(|&(app_id, tone)| AppToneRule {
            app_id: app_id.to_string(),
            tone,
        })
        .collect()
}

/// Picks the tone for the captured app: user rules first, then built-ins.
pub fn tone_for(context: Option<&AppContext>, settings: &FlowSettings) -> Tone {
    if !settings.tone_enabled {
        return Tone::Default;
    }
    let Some(app_id) = context.and_then(|c| c.app_id.as_deref()) else {
        return Tone::Default;
    };
    matching_rule(app_id, settings)
        .map(|(_, tone, _)| tone)
        .unwrap_or_default()
}

/// The first rule matching `app_id` as `(pattern, tone, is_user_rule)`,
/// ignoring whether tone is enabled.
pub fn matching_rule<'a>(
    app_id: &str,
    settings: &'a FlowSettings,
) -> Option<(&'a str, Tone, bool)> {
    settings
        .app_tones
        .iter()
        .map(|rule| (rule.app_id.as_str(), rule.tone, true))
        .chain(
            BUILTIN_APP_TONES
                .iter()
                .map(|&(pattern, tone)| (pattern, tone, false)),
        )
        .find(|(pattern, _, _)| app_matches(pattern, app_id))
}

/// The last app other than this one that was frontmost at a hotkey press.
/// Kept in memory only so the settings UI can offer it as a rule.
static LAST_APP: Mutex<Option<AppContext>> = Mutex::new(None);

/// Remembers a captured app unless it is this app's own window.
pub fn remember(context: Option<&AppContext>, own_ids: &[&str]) {
    let Some(context) = context else { return };
    let Some(app_id) = context.app_id.as_deref() else {
        return;
    };
    if own_ids.iter().any(|own| app_matches(own, app_id)) {
        return;
    }
    *LAST_APP.lock().unwrap_or_else(PoisonError::into_inner) = Some(context.clone());
}

/// The app remembered by [`remember`], if any.
pub fn last_app() -> Option<AppContext> {
    LAST_APP
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn normalize(id: &str) -> String {
    let id = id.trim().to_lowercase();
    match id.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => id,
    }
}

fn app_matches(pattern: &str, app_id: &str) -> bool {
    let pattern = normalize(pattern);
    let app_id = normalize(app_id);
    match pattern.strip_suffix('*') {
        Some(prefix) => !prefix.is_empty() && app_id.starts_with(prefix),
        None => pattern == app_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(app_id: &str) -> AppContext {
        AppContext {
            app_id: Some(app_id.into()),
            app_name: None,
        }
    }

    fn rule(app_id: &str, tone: Tone) -> AppToneRule {
        AppToneRule {
            app_id: app_id.into(),
            tone,
        }
    }

    #[test]
    fn builtin_apps_map_to_their_presets() {
        let settings = FlowSettings::default();
        let cases = [
            ("com.tinyspeck.slackmacgap", Tone::Casual),
            ("com.apple.mail", Tone::Formal),
            ("com.microsoft.VSCode", Tone::Code),
            ("Slack.exe", Tone::Casual),
            ("OUTLOOK.EXE", Tone::Formal),
            ("WindowsTerminal.exe", Tone::Code),
            ("com.apple.Safari", Tone::Default),
        ];
        for (app_id, expected) in cases {
            assert_eq!(
                tone_for(Some(&ctx(app_id)), &settings),
                expected,
                "{app_id}"
            );
        }
    }

    #[test]
    fn wildcard_matches_prefix_only() {
        let settings = FlowSettings::default();
        assert_eq!(
            tone_for(Some(&ctx("com.jetbrains.intellij")), &settings),
            Tone::Code
        );
        assert_eq!(
            tone_for(Some(&ctx("com.jetbrainsx")), &settings),
            Tone::Default
        );
        assert!(!app_matches("*", "anything"));
    }

    #[test]
    fn user_rules_override_builtins() {
        let settings = FlowSettings {
            tone_enabled: true,
            app_tones: vec![
                rule("com.tinyspeck.slackmacgap", Tone::Formal),
                rule("com.apple.*", Tone::Casual),
            ],
        };
        assert_eq!(
            tone_for(Some(&ctx("com.tinyspeck.slackmacgap")), &settings),
            Tone::Formal
        );
        // The user wildcard is checked before the built-in Mail preset.
        assert_eq!(
            tone_for(Some(&ctx("com.apple.mail")), &settings),
            Tone::Casual
        );
        let reset = FlowSettings {
            tone_enabled: true,
            app_tones: vec![rule("slack.exe", Tone::Default)],
        };
        assert_eq!(tone_for(Some(&ctx("slack.exe")), &reset), Tone::Default);
    }

    #[test]
    fn disabled_or_unknown_context_uses_default_tone() {
        let disabled = FlowSettings {
            tone_enabled: false,
            app_tones: Vec::new(),
        };
        assert_eq!(
            tone_for(Some(&ctx("com.tinyspeck.slackmacgap")), &disabled),
            Tone::Default
        );
        let settings = FlowSettings::default();
        assert_eq!(tone_for(None, &settings), Tone::Default);
        let name_only = AppContext {
            app_id: None,
            app_name: Some("Slack".into()),
        };
        assert_eq!(tone_for(Some(&name_only), &settings), Tone::Default);
    }

    #[test]
    fn builtin_rules_are_valid_settings() {
        let settings = FlowSettings {
            tone_enabled: true,
            app_tones: builtin_app_tones(),
        };
        assert!(settings.validated().is_ok());
    }

    #[test]
    fn empty_context_is_treated_as_not_captured() {
        assert_eq!(non_empty(AppContext::default()), None);
        let blank = AppContext {
            app_id: Some("  ".into()),
            app_name: None,
        };
        assert_eq!(non_empty(blank), None);
        let named = AppContext {
            app_id: None,
            app_name: Some("Slack".into()),
        };
        assert!(non_empty(named).is_some());
    }

    #[test]
    fn matching_rule_reports_source_and_ignores_enabled_flag() {
        let settings = FlowSettings {
            tone_enabled: false,
            app_tones: vec![rule("com.apple.*", Tone::Code)],
        };
        assert_eq!(
            matching_rule("com.apple.mail", &settings),
            Some(("com.apple.*", Tone::Code, true))
        );
        assert_eq!(
            matching_rule("Slack.exe", &settings),
            Some(("slack", Tone::Casual, false))
        );
        assert_eq!(matching_rule("com.example.unknown", &settings), None);
    }

    #[test]
    fn remember_skips_own_app_and_missing_ids() {
        remember(Some(&ctx("com.tinyspeck.slackmacgap")), &["com.pais.handy"]);
        remember(Some(&ctx("com.pais.handy")), &["com.pais.handy"]);
        remember(Some(&ctx("Handy.exe")), &["", "handy.exe"]);
        remember(None, &[]);
        let name_only = AppContext {
            app_id: None,
            app_name: Some("Unknown".into()),
        };
        remember(Some(&name_only), &[]);
        assert_eq!(last_app(), Some(ctx("com.tinyspeck.slackmacgap")));
    }

    #[test]
    fn capture_is_fast_and_does_not_panic() {
        let (_, micros) = capture_timed();
        assert!(micros < 1_000_000, "capture took {micros} us");
    }
}
