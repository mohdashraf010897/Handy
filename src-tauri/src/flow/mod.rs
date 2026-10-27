//! Fork-owned "Flow" features (see `docs/fork/ARCHITECTURE.md`).
//!
//! Everything here is reached from upstream code through one-line hooks in
//! `actions.rs` and `lib.rs`, so upstream merges stay conflict-free.

pub mod context;
pub mod timing;

use tauri::AppHandle;

/// Hotkey press hook (`TranscribeAction::start`): marks t0, then captures
/// the frontmost app synchronously so it is the app the user dictates into.
pub fn on_press(app: &AppHandle) {
    timing::press(app);
    let (context, capture_us) = context::capture_timed();
    timing::attach_app(context, capture_us);
}

/// Hotkey release hook (`TranscribeAction::stop`).
pub fn on_release() {
    timing::release();
}
