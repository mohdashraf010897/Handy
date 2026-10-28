//! Fork-owned "Flow" features (see `docs/fork/ARCHITECTURE.md`).
//!
//! Everything here is reached from upstream code through one-line hooks in
//! `actions.rs` and `lib.rs`, so upstream merges stay conflict-free.

pub mod commands;
pub mod context;
pub mod prompt;
pub mod settings;
pub mod timing;

use std::sync::OnceLock;

use tauri::AppHandle;

/// Hotkey press hook (`TranscribeAction::start`): marks t0, then captures
/// the frontmost app synchronously so it is the app the user dictates into.
pub fn on_press(app: &AppHandle) {
    timing::press(app);
    let (context, capture_us) = context::capture_timed();
    context::remember(context.as_ref(), &own_app_ids(app));
    timing::attach_app(context, capture_us);
}

/// This app's bundle id and exe name, so its own window is never offered
/// as a per-app rule.
fn own_app_ids(app: &AppHandle) -> [&'static str; 2] {
    static IDS: OnceLock<(String, String)> = OnceLock::new();
    let (identifier, exe) = IDS.get_or_init(|| {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        (app.config().identifier.clone(), exe)
    });
    [identifier.as_str(), exe.as_str()]
}

/// Hotkey release hook (`TranscribeAction::stop`). Returns the dictation's
/// session id for the ASR and paste hooks.
pub fn on_release() -> u64 {
    timing::release()
}
