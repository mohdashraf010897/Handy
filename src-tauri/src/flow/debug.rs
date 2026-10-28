//! Debug builds only: runs a headless `--transcribe-file` through the Flow
//! dictation path as if a given app were frontmost, for QA without a GUI.
//!
//! With `SEALVOICE_FLOW_AS_APP=<bundle id>` set, [`run_headless`] presses,
//! releases, transcribes and cleans up like a hotkey dictation into that app
//! (no mic, no paste), then prints one `flow_debug {json}` line to stdout.
//! `SEALVOICE_FLOW_TONE_ENABLED=1` turns per-app tone on for this process
//! only; it is not saved. Cleanup runs when post-processing is enabled in the
//! settings, as with the post-process hotkey.

use std::sync::Arc;
use std::time::Instant;

use tauri::{AppHandle, Manager};
use tauri_specta::Event;

use crate::actions::{
    process_transcription_output, TranscriptionCompleteEvent, TranscriptionOutcome,
};
use crate::flow::context::AppContext;
use crate::flow::{settings, timing};
use crate::managers::transcription::TranscriptionManager;

const AS_APP_ENV: &str = "SEALVOICE_FLOW_AS_APP";
const TONE_ENABLED_ENV: &str = "SEALVOICE_FLOW_TONE_ENABLED";

fn env_value(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Runs the Flow path for `samples` when `SEALVOICE_FLOW_AS_APP` is set.
pub fn run_headless(app: &AppHandle, model_id: &str, samples: &[f32]) {
    let Some(app_id) = env_value(AS_APP_ENV) else {
        return;
    };
    if env_value(TONE_ENABLED_ENV).as_deref() == Some("1") {
        settings::override_in_memory(settings::FlowSettings {
            tone_enabled: true,
            ..settings::get(app)
        });
    }
    let tm = Arc::clone(app.state::<Arc<TranscriptionManager>>().inner());
    if !tm.is_model_loaded() {
        if let Err(e) = tm.load_model_with_device(model_id, None) {
            eprintln!("error: flow_debug: load_model('{model_id}') failed: {e}");
            return;
        }
    }

    let context = AppContext {
        app_id: Some(app_id),
        app_name: None,
    };
    super::press_with(app, || (Some(context), 0));
    let session = super::on_release();
    let stop_time = Instant::now();

    let post_process = crate::settings::get_settings(app).post_process_enabled;
    let handle = app.clone();
    let audio = samples.to_vec();
    // A spawned task, as in the app: the cleanup tone is tied to the task
    // that ran ASR, and `block_on` alone has no task id.
    let result = tauri::async_runtime::block_on(tauri::async_runtime::spawn(async move {
        let text = timing::asr(session, || tm.transcribe(audio))?;
        let processed = process_transcription_output(&handle, &text, post_process).await;
        Ok::<_, anyhow::Error>(processed.final_text)
    }));
    let text = match result {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => {
            eprintln!("error: flow_debug: transcribe failed: {e}");
            return;
        }
        Err(e) => {
            eprintln!("error: flow_debug: task failed: {e}");
            return;
        }
    };

    let summary = timing::summary(session);
    let outcome = if text.is_empty() {
        TranscriptionOutcome::Empty
    } else {
        TranscriptionOutcome::Typed
    };
    // Logs this session's `flow_timing` line (not pasted, so no paste times).
    let event = TranscriptionCompleteEvent {
        outcome,
        audio_seconds: samples.len() as f64
            / crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE as f64,
        wait_seconds: stop_time.elapsed().as_secs_f64(),
    };
    if let Err(e) = event.emit(app) {
        eprintln!("error: flow_debug: failed to emit completion: {e}");
    }

    println!(
        "flow_debug {}",
        serde_json::json!({
            "tone": summary.and_then(|s| s.tone),
            "cleanup_ran": summary.is_some_and(|s| s.cleanup_ran),
            "app_captured": summary.is_some_and(|s| s.app_captured),
            "text": text,
        })
    );
}
