use crate::managers::audio::AudioRecordingManager;
use crate::meeting::macos_bridge::{
    active_calendar_meeting, calendar_access_status, request_calendar_access,
    ActiveCalendarMeeting, CalendarAccessStatus,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{
    MeetingDetail, MeetingLiveState, MeetingSummaryRecord, MeetingTemplate,
};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

fn meeting_manager(app: &AppHandle) -> Result<Arc<MeetingSessionManager>, String> {
    app.try_state::<Arc<MeetingSessionManager>>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| "Meeting manager is not initialized yet".to_string())
}

#[tauri::command]
#[specta::specta]
pub fn get_meeting_live_state(app: AppHandle) -> MeetingLiveState {
    match meeting_manager(&app) {
        Ok(manager) => manager.get_live_state(),
        Err(_) => MeetingSessionManager::default_live_state(),
    }
}

#[tauri::command]
#[specta::specta]
pub fn start_meeting_notes(
    app: AppHandle,
    title: Option<String>,
    template: Option<MeetingTemplate>,
    open_workspace: Option<bool>,
) -> Result<MeetingLiveState, String> {
    if app
        .try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|audio_manager| audio_manager.is_recording())
    {
        return Err(
            "Cannot start meeting notes while a dictation recording is in progress. Please stop dictation first."
                .to_string(),
        );
    }

    let state = meeting_manager(&app)?.start_meeting(title, template)?;

    if open_workspace.unwrap_or(false) {
        crate::session_bus::open_meetings_workspace(app);
    }

    Ok(state)
}

#[tauri::command]
#[specta::specta]
pub fn pause_meeting_notes(app: AppHandle) -> Result<MeetingLiveState, String> {
    meeting_manager(&app)?.pause_meeting()
}

#[tauri::command]
#[specta::specta]
pub fn resume_meeting_notes(app: AppHandle) -> Result<MeetingLiveState, String> {
    meeting_manager(&app)?.resume_meeting()
}

#[tauri::command]
#[specta::specta]
pub async fn stop_meeting_notes(app: AppHandle) -> Result<MeetingDetail, String> {
    let manager = meeting_manager(&app)?;
    tokio::task::spawn_blocking(move || manager.stop_meeting())
        .await
        .map_err(|err| format!("Failed to stop meeting task: {err}"))?
}

#[tauri::command]
#[specta::specta]
pub fn update_live_rough_notes(app: AppHandle, rough_notes: String) -> Result<(), String> {
    meeting_manager(&app)?.update_live_rough_notes(rough_notes)
}

#[tauri::command]
#[specta::specta]
pub fn set_meeting_auto_detect(app: AppHandle, enabled: bool) -> Result<bool, String> {
    meeting_manager(&app)?.set_auto_detect_enabled(enabled)
}

#[tauri::command]
#[specta::specta]
pub fn dismiss_detected_call(app: AppHandle, call_id: Option<String>) -> Result<(), String> {
    meeting_manager(&app)?.dismiss_detected_call(call_id)
}

#[tauri::command]
#[specta::specta]
pub fn list_meetings(
    app: AppHandle,
    query: Option<String>,
) -> Result<Vec<MeetingSummaryRecord>, String> {
    meeting_manager(&app)?.list_meetings(query.as_deref())
}

#[tauri::command]
#[specta::specta]
pub fn get_meeting_detail(app: AppHandle, meeting_id: String) -> Result<MeetingDetail, String> {
    meeting_manager(&app)?.get_meeting_detail(&meeting_id)
}

#[tauri::command]
#[specta::specta]
pub fn update_meeting_meta(
    app: AppHandle,
    meeting_id: String,
    title: Option<String>,
    template: Option<MeetingTemplate>,
    rough_notes: Option<String>,
) -> Result<MeetingDetail, String> {
    meeting_manager(&app)?.update_meeting_meta(&meeting_id, title, template, rough_notes)
}

#[tauri::command]
#[specta::specta]
pub async fn regenerate_meeting_notes(
    app: AppHandle,
    meeting_id: String,
    template: Option<MeetingTemplate>,
) -> Result<MeetingDetail, String> {
    let manager = meeting_manager(&app)?;
    tokio::task::spawn_blocking(move || manager.regenerate_meeting_notes(&meeting_id, template))
        .await
        .map_err(|err| format!("Failed to regenerate meeting notes task: {err}"))?
}

#[tauri::command]
#[specta::specta]
pub fn export_meeting_markdown(app: AppHandle, meeting_id: String) -> Result<String, String> {
    meeting_manager(&app)?.export_meeting_markdown(&meeting_id)
}

#[tauri::command]
#[specta::specta]
pub fn copy_meeting_markdown(app: AppHandle, meeting_id: String) -> Result<bool, String> {
    let markdown = meeting_manager(&app)?.export_meeting_markdown(&meeting_id)?;
    app.clipboard()
        .write_text(markdown)
        .map_err(|err| format!("Failed to copy meeting markdown: {err}"))?;
    Ok(true)
}

#[tauri::command]
#[specta::specta]
pub fn delete_meeting(app: AppHandle, meeting_id: String) -> Result<bool, String> {
    meeting_manager(&app)?.delete_meeting(&meeting_id)
}

#[tauri::command]
#[specta::specta]
pub fn get_calendar_access_status() -> CalendarAccessStatus {
    calendar_access_status()
}

#[tauri::command]
#[specta::specta]
pub async fn connect_calendar_access() -> Result<CalendarAccessStatus, String> {
    tokio::task::spawn_blocking(request_calendar_access)
        .await
        .map_err(|err| format!("Failed to request calendar access: {err}"))
}

#[tauri::command]
#[specta::specta]
pub fn get_active_calendar_meeting() -> Option<ActiveCalendarMeeting> {
    active_calendar_meeting()
}
