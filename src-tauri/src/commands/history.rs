use crate::managers::{
    history::{HistoryEntry, HistoryManager, PaginatedHistory},
    transcription_jobs::TranscriptionJobManager,
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
#[specta::specta]
pub async fn get_history_entries(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<PaginatedHistory, String> {
    history_manager
        .get_history_entries(cursor, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_history_entry_saved(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .toggle_saved_status(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_audio_file_path(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    file_name: String,
) -> Result<String, String> {
    let path = history_manager.get_audio_file_path(&file_name);
    path.to_str()
        .ok_or_else(|| "Invalid file path".to_string())
        .map(|s| s.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn delete_history_entry(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    id: i64,
) -> Result<(), String> {
    jobs.cancel(id);
    history_manager
        .delete_entry(id)
        .await
        .map_err(|e| e.to_string())
}

/// Queues the entry to be transcribed again with the current model. Its
/// progress and outcome arrive as `TranscriptionJob` events.
#[tauri::command]
#[specta::specta]
pub fn retry_history_entry_transcription(jobs: State<'_, Arc<TranscriptionJobManager>>, id: i64) {
    jobs.retry(id);
}

/// Stops the entry's background transcription: a queued one is dropped, a
/// running one stops after the chunk it is on. The entry keeps the text it
/// had before.
#[tauri::command]
#[specta::specta]
pub fn cancel_history_entry_transcription(jobs: State<'_, Arc<TranscriptionJobManager>>, id: i64) {
    jobs.cancel(id);
}

/// Adds an audio or video file to History and queues its transcription.
/// Fails without adding anything if the file has no audio Handy can read.
#[tauri::command]
#[specta::specta]
pub async fn transcribe_file(
    jobs: State<'_, Arc<TranscriptionJobManager>>,
    path: String,
) -> Result<HistoryEntry, String> {
    let jobs = Arc::clone(&jobs);
    tauri::async_runtime::spawn_blocking(move || jobs.transcribe_file(PathBuf::from(path)))
        .await
        .map_err(|e| format!("Failed to read the file: {}", e))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn update_history_limit(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    limit: usize,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.history_limit = limit;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn update_recording_retention_period(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    period: String,
) -> Result<(), String> {
    use crate::settings::RecordingRetentionPeriod;

    let retention_period = match period.as_str() {
        "never" => RecordingRetentionPeriod::Never,
        "preserve_limit" => RecordingRetentionPeriod::PreserveLimit,
        "days3" => RecordingRetentionPeriod::Days3,
        "weeks2" => RecordingRetentionPeriod::Weeks2,
        "months3" => RecordingRetentionPeriod::Months3,
        _ => return Err(format!("Invalid retention period: {}", period)),
    };

    let mut settings = crate::settings::get_settings(&app);
    settings.recording_retention_period = retention_period;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn normalize_dictionary_word(raw: &str) -> Result<String, String> {
    let stripped: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | '"' | '\''))
        .collect();
    let normalized = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_len = normalized.chars().count();
    if !(1..=50).contains(&char_len) {
        return Err("Dictionary word must be between 1 and 50 characters".to_string());
    }
    Ok(normalized)
}

fn upsert_custom_word(custom_words: &mut Vec<String>, normalized: String) -> bool {
    let lower = normalized.to_lowercase();
    if let Some(existing) = custom_words.iter_mut().find(|w| w.to_lowercase() == lower) {
        if *existing != normalized {
            *existing = normalized;
            true
        } else {
            false
        }
    } else {
        custom_words.push(normalized);
        true
    }
}

#[tauri::command]
#[specta::specta]
pub async fn correct_history_entry_and_save_word(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    updated_text: String,
    dictionary_word: Option<String>,
) -> Result<HistoryEntry, String> {
    let trimmed_text = updated_text.trim();
    if trimmed_text.is_empty() {
        return Err("Updated transcription cannot be empty".to_string());
    }

    let normalized_word = match dictionary_word {
        Some(raw) if !raw.trim().is_empty() => Some(normalize_dictionary_word(&raw)?),
        Some(raw) if !raw.is_empty() => return Err(normalize_dictionary_word(&raw).unwrap_err()),
        _ => None,
    };

    let entry = history_manager
        .update_entry_transcription(id, trimmed_text.to_string())
        .map_err(|e| e.to_string())?;

    if let Some(word) = normalized_word {
        let mut settings = crate::settings::get_settings(&app);
        if upsert_custom_word(&mut settings.custom_words, word) {
            let updated_words = settings.custom_words.clone();
            crate::settings::write_settings(&app, settings);
            let _ = app.emit(
                "settings-changed",
                serde_json::json!({
                    "setting": "custom_words",
                    "value": updated_words,
                }),
            );
        }
    }

    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_dictionary_word_strips_quotes_angles_and_collapses_whitespace() {
        assert_eq!(
            normalize_dictionary_word("  \"low<yap>\"   'desktop'  ").unwrap(),
            "lowyap desktop"
        );
        assert_eq!(
            normalize_dictionary_word("low\u{0000}\u{0007}yap\u{001b}").unwrap(),
            "lowyap"
        );
        assert!(normalize_dictionary_word("\u{0000}\u{001f}").is_err());
        assert!(normalize_dictionary_word("   ").is_err());
        assert!(normalize_dictionary_word("\"<>\"").is_err());
        assert!(normalize_dictionary_word(&"a".repeat(50)).is_ok());
        assert!(normalize_dictionary_word(&"a".repeat(51)).is_err());
    }

    #[test]
    fn upsert_custom_word_deduplicates_case_insensitively_and_replaces_casing() {
        let mut words = vec!["Handy".to_string(), "LowYap".to_string()];

        assert!(!upsert_custom_word(&mut words, "LowYap".to_string()));
        assert_eq!(words, vec!["Handy", "LowYap"]);

        assert!(upsert_custom_word(&mut words, "lowyap".to_string()));
        assert_eq!(words, vec!["Handy", "lowyap"]);

        assert!(upsert_custom_word(&mut words, "Wispr".to_string()));
        assert_eq!(words, vec!["Handy", "lowyap", "Wispr"]);
    }
}
