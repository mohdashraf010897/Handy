//! Fork-owned "Flow" features (see `docs/fork/ARCHITECTURE.md`).
//!
//! Everything here is reached from upstream code through one-line hooks in
//! `actions.rs` and `lib.rs`, so upstream merges stay conflict-free.

pub mod commands;
pub mod context;
#[cfg(debug_assertions)]
pub mod debug;
pub mod fast_path;
pub mod mic_gain;
pub mod multilingual;
pub mod paste_guard;
pub mod prompt;
pub mod session_wal;
pub mod settings;
pub mod snippets;
pub(crate) mod soft_cancel;
pub mod spoken_urls;
#[cfg(test)]
mod test_log;
pub mod timing;

use std::sync::OnceLock;

use tauri::AppHandle;

use crate::actions::ProcessedTranscription;

/// Hotkey press hook (`TranscribeAction::start`): marks t0, then captures
/// the frontmost app synchronously so it is the app the user dictates into.
pub fn on_press(app: &AppHandle) {
    press_with(app, context::capture_timed);
}

/// Press steps with the frontmost-app capture supplied by the caller.
fn press_with(app: &AppHandle, capture: impl FnOnce() -> (Option<context::AppContext>, u64)) {
    timing::press(app);
    let (context, capture_us) = capture();
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

struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

fn build_deterministic_result(
    raw_transcription: &str,
    expanded: &snippets::SnippetExpansion,
    tone: settings::Tone,
    apply_tone_polish: bool,
) -> ProcessedTranscription {
    let final_text = if expanded.full_utterance_match {
        // Never alter a user's exact full-utterance snippet (e.g. preserve a
        // deliberate trailing period in Casual mode).
        expanded.text.clone()
    } else if apply_tone_polish {
        fast_path::apply_deterministic_polish(&expanded.text, tone)
    } else if expanded.matched_any {
        expanded.text.clone()
    } else {
        expanded.text.trim().to_string()
    };
    let post_processed_text = (final_text != raw_transcription.trim()).then(|| final_text.clone());
    ProcessedTranscription {
        final_text,
        post_processed_text,
        post_process_prompt: None,
    }
}

fn expand_snippets_and_urls(
    transcription: &str,
    snippets: &[settings::VoiceSnippet],
) -> snippets::SnippetExpansion {
    let mut expanded = snippets::expand_snippets(transcription, snippets);
    if !expanded.full_utterance_match {
        let formatted = spoken_urls::format_spoken_urls(&expanded.text);
        if formatted != expanded.text {
            expanded.text = formatted;
        }
    }
    expanded
}

/// Expands voice snippets, applies the sub-350ms deterministic fast-path when
/// eligible, or runs LLM cleanup raced against the `[Paste raw now]` escape hatch.
pub(crate) async fn finalize_dictation(
    app: &AppHandle,
    transcription: &str,
    post_process: bool,
    on_polishing_started: impl FnOnce(),
    is_cancelled: impl Fn() -> bool,
) -> Option<ProcessedTranscription> {
    let flow_settings = settings::get(app);
    let expanded = expand_snippets_and_urls(transcription, &flow_settings.snippets);

    if !post_process {
        return Some(build_deterministic_result(
            transcription,
            &expanded,
            settings::Tone::Default,
            false,
        ));
    }

    let effective_tone = if flow_settings.tone_enabled {
        timing::choose_tone(|ctx| context::tone_for(ctx, &flow_settings))
    } else {
        settings::Tone::Default
    };

    if fast_path::should_bypass_llm(
        &expanded.text,
        effective_tone,
        flow_settings.fast_path_enabled,
        expanded.full_utterance_match,
    ) {
        return Some(build_deterministic_result(
            transcription,
            &expanded,
            effective_tone,
            true,
        ));
    }

    on_polishing_started();

    let app_settings = crate::settings::get_settings(app);
    let toned_settings = prompt::with_tone(app, &app_settings).into_owned();
    let active_prompt = app_settings
        .active_post_process_prompt()
        .map(|p| p.prompt.clone());
    let (turn_id, skip_notify) = paste_guard::begin_polishing();
    let expanded_for_llm = expanded.text.clone();

    let Some((preferred_winner, llm_output)) = crate::actions::complete_unless_cancelled(
        async {
            let mut preferred = paste_guard::TurnWinner::Llm;
            let llm_output = timing::cleanup(async {
                let notified = skip_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if paste_guard::is_skip_requested(turn_id) {
                    preferred = paste_guard::TurnWinner::Raw;
                    return None;
                }

                let mut abort_guard = AbortOnDrop(Some(tokio::spawn(async move {
                    crate::actions::post_process_transcription(&toned_settings, &expanded_for_llm)
                        .await
                })));

                let Some(handle) = abort_guard.0.as_mut() else {
                    return None;
                };

                tokio::select! {
                    biased;
                    _ = &mut notified => {
                        preferred = paste_guard::TurnWinner::Raw;
                        // Dropping `abort_guard` aborts cancellable HTTP/Ollama work immediately.
                        None
                    }
                    res = handle => {
                        let _ = abort_guard.0.take();
                        match res {
                            Ok(output) => output,
                            Err(err) => {
                                log::error!("flow: LLM post-processing task failed: {err}");
                                None
                            }
                        }
                    }
                }
            })
            .await;

            (preferred, llm_output)
        },
        is_cancelled,
    )
    .await
    else {
        paste_guard::cancel_polishing(turn_id);
        return None;
    };

    let winner = paste_guard::resolve_polishing(turn_id, preferred_winner)?;

    match (winner, llm_output) {
        (paste_guard::TurnWinner::Raw, _) => Some(build_deterministic_result(
            transcription,
            &expanded,
            effective_tone,
            true,
        )),
        (paste_guard::TurnWinner::Llm, Some(processed_text)) => Some(ProcessedTranscription {
            final_text: processed_text.clone(),
            post_processed_text: Some(processed_text),
            post_process_prompt: active_prompt,
        }),
        (paste_guard::TurnWinner::Llm, None) => Some(build_deterministic_result(
            transcription,
            &expanded,
            effective_tone,
            false,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::settings::{Tone, VoiceSnippet};

    #[test]
    fn full_utterance_snippet_preserves_trailing_period_in_casual_tone_and_records_history() {
        let snippets = [VoiceSnippet {
            trigger: "sign off".into(),
            expansion: "Best regards, Mohd.".into(),
        }];
        let raw = "Sign off.";
        let expanded = snippets::expand_snippets(raw, &snippets);
        let result = build_deterministic_result(raw, &expanded, Tone::Casual, true);
        assert_eq!(result.final_text, "Best regards, Mohd.");
        assert_eq!(
            result.post_processed_text.as_deref(),
            Some("Best regards, Mohd.")
        );
    }

    #[test]
    fn casual_fast_path_records_transformed_text_while_unchanged_stays_none() {
        let raw = "Sounds good.";
        let expanded = snippets::expand_snippets(raw, &[]);
        let casual = build_deterministic_result(raw, &expanded, Tone::Casual, true);
        assert_eq!(casual.final_text, "Sounds good");
        assert_eq!(casual.post_processed_text.as_deref(), Some("Sounds good"));

        let unchanged = build_deterministic_result(raw, &expanded, Tone::Default, true);
        assert_eq!(unchanged.final_text, "Sounds good.");
        assert_eq!(unchanged.post_processed_text, None);
    }

    #[test]
    fn spoken_urls_format_after_snippets_unless_full_utterance_snippet_matches() {
        let snippets = [VoiceSnippet {
            trigger: "lowyap dot com".into(),
            expansion: "https://lowyap.com/welcome.".into(),
        }];

        // Exact full-utterance snippet wins without URL formatter stripping its deliberate period.
        let full = expand_snippets_and_urls("Lowyap dot com.", &snippets);
        assert!(full.full_utterance_match);
        let full_res = build_deterministic_result("Lowyap dot com.", &full, Tone::Default, false);
        assert_eq!(full_res.final_text, "https://lowyap.com/welcome.");

        // Non-snippet spoken URL formats deterministically in both raw and Code fast-path modes.
        let spoken = expand_snippets_and_urls("github dot com slash lowyap.", &snippets);
        assert!(!spoken.full_utterance_match);
        assert_eq!(spoken.text, "github.com/lowyap");
        assert!(fast_path::should_bypass_llm(
            &spoken.text,
            Tone::Code,
            true,
            spoken.full_utterance_match
        ));
        let raw_res =
            build_deterministic_result("github dot com slash lowyap.", &spoken, Tone::Code, false);
        assert_eq!(raw_res.final_text, "github.com/lowyap");
        assert_eq!(
            raw_res.post_processed_text.as_deref(),
            Some("github.com/lowyap")
        );
    }
}
