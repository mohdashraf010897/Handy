//! Fork-owned "Flow" features (see `docs/fork/ARCHITECTURE.md`).
//!
//! Everything here is reached from upstream code through one-line hooks in
//! `actions.rs` and `lib.rs`, so upstream merges stay conflict-free.

pub mod timing;

use tauri::AppHandle;

/// Hotkey press hook (`TranscribeAction::start`).
pub fn on_press(app: &AppHandle) {
    timing::press(app);
}

/// Hotkey release hook (`TranscribeAction::stop`).
pub fn on_release() {
    timing::release();
}
