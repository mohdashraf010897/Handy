//! Fork-owned "Flow" features (see `docs/fork/ARCHITECTURE.md`).
//!
//! Everything here is reached from upstream code through one-line hooks in
//! `actions.rs` and `lib.rs`, so upstream merges stay conflict-free.

pub mod ax_context;
pub mod commands;
pub mod context;
#[cfg(debug_assertions)]
pub mod debug;
pub mod fast_path;
pub mod macos_speech;
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

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock, PoisonError,
};

use lowyap_ports::{FieldContextProvider, FieldContextSnapshot, LinguistStageKind, LinguistTone};
use tauri::AppHandle;

use crate::actions::ProcessedTranscription;
use crate::flow::multilingual::HindustaniScript;

static LAST_FIELD_CONTEXT: Mutex<Option<(u64, FieldContextSnapshot)>> = Mutex::new(None);
static FIELD_CONTEXT_GEN: AtomicU64 = AtomicU64::new(0);

fn clear_field_context() -> u64 {
    let gen = FIELD_CONTEXT_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    *LAST_FIELD_CONTEXT
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
    gen
}

fn store_field_context(gen: u64, snapshot: Option<FieldContextSnapshot>) {
    if FIELD_CONTEXT_GEN.load(Ordering::Acquire) != gen {
        return;
    }
    *LAST_FIELD_CONTEXT
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = snapshot.map(|snap| (gen, snap));
}

fn take_field_context() -> Option<FieldContextSnapshot> {
    let current = FIELD_CONTEXT_GEN.load(Ordering::Acquire);
    LAST_FIELD_CONTEXT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .and_then(|(gen, snap)| (gen == current).then_some(snap))
}

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
    let gen = clear_field_context();
    let context_for_ax = context.clone();
    let _ = std::thread::Builder::new()
        .name("ax-context".into())
        .spawn(move || {
            let snap = ax_context::PlatformFieldContextProvider::new(context_for_ax.as_ref())
                .capture_field_context();
            store_field_context(gen, snap);
        });
    timing::attach_app(context, capture_us);
}

/// This app's bundle id and exe name, so its own window is never offered
/// as a per-app rule.
pub(crate) fn own_app_ids(app: &AppHandle) -> [&'static str; 2] {
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

fn to_linguist_tone(tone: settings::Tone) -> LinguistTone {
    match tone {
        settings::Tone::Default => LinguistTone::Default,
        settings::Tone::Casual => LinguistTone::Casual,
        settings::Tone::Formal => LinguistTone::Formal,
        settings::Tone::Code => LinguistTone::Code,
    }
}

fn has_spoken_self_repair(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "no wait",
        "i mean",
        "scratch that",
        "actually wait",
        "or rather",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
}

fn build_stage_a_result(
    raw_transcription: &str,
    expanded: &snippets::SnippetExpansion,
    tone: settings::Tone,
    lexicon_terms: &[String],
    audio_duration_s: f64,
) -> ProcessedTranscription {
    let final_text = if expanded.full_utterance_match {
        // Never alter a user's exact full-utterance snippet.
        expanded.text.clone()
    } else {
        lowyap_flow::apply_stage_a_linguist(
            &expanded.text,
            to_linguist_tone(tone),
            lexicon_terms,
            audio_duration_s,
        )
    };
    let post_processed_text = (final_text != raw_transcription.trim()).then(|| final_text.clone());
    ProcessedTranscription {
        final_text,
        post_processed_text,
        post_process_prompt: None,
    }
}

#[cfg(test)]
fn build_deterministic_result(
    raw_transcription: &str,
    expanded: &snippets::SnippetExpansion,
    tone: settings::Tone,
    apply_tone_polish: bool,
) -> ProcessedTranscription {
    let final_text = if expanded.full_utterance_match {
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

#[cfg(test)]
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

/// Wall-clock interactive SLA for optional Stage-B LLM polishing.
/// If any local or remote provider takes longer than 3.5s, `lowyap` cancels it
/// and pastes Stage-A's deterministic clean text immediately so dictation never hangs.
const STAGE_B_SLA_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(3_500);

/// Maximum character budget sent to Apple's on-device ~3B FoundationModels.
/// Above ~650 chars (~110 words), local 3B guided JSON generation exceeds 15-55s
/// and risks overflowing its 4k context window, whereas Stage-A deterministic
/// linguist (`apply_stage_a_linguist`) cleans fillers, stutters, lexicon terms,
/// spoken URLs, punctuation, and monologue paragraphs in < 1ms.
const APPLE_INTELLIGENCE_FAST_PATH_MAX_CHARS: usize = 650;

fn should_bypass_stage_b_for_local_capacity(provider_id: &str, stage_a_text: &str) -> bool {
    provider_id == crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID
        && stage_a_text.chars().count() > APPLE_INTELLIGENCE_FAST_PATH_MAX_CHARS
}

/// Runs the 2-stage plug-and-play Linguist pipeline:
/// - Stage A (`lowyap_flow::apply_stage_a_linguist`): `<1ms` pure-Rust stutter/filler
///   cleanup, brand & user lexicon snapping, URL formatting, tone polish, and
///   multi-paragraph structuring on `>=45s` / `>=85`-word monologues.
/// - Stage B (`post_process_transcription` + `validate_linguist_output`): optional
///   model-agnostic rewrite (local or cloud) with cursor-context continuity,
///   hallucination guard, and `[Paste raw now]` escape hatch.
pub(crate) async fn finalize_dictation(
    app: &AppHandle,
    transcription: &str,
    audio_duration_s: f64,
    post_process: bool,
    on_polishing_started: impl FnOnce(&str),
    is_cancelled: impl Fn() -> bool,
) -> Option<ProcessedTranscription> {
    let flow_settings = settings::get(app);
    let app_settings = crate::settings::get_settings(app);
    let field_context = take_field_context();
    let expanded = snippets::expand_snippets(transcription, &flow_settings.snippets);

    let effective_tone = if flow_settings.tone_enabled {
        timing::choose_tone(|ctx| context::tone_for(ctx, &flow_settings))
    } else {
        settings::Tone::Default
    };

    let lexicon_terms = lowyap_flow::merged_lexicon_terms(&app_settings.custom_words);
    let stage_a = build_stage_a_result(
        transcription,
        &expanded,
        effective_tone,
        &lexicon_terms,
        audio_duration_s,
    );

    if stage_a.final_text.trim().is_empty() {
        timing::record_linguist_outcome(LinguistStageKind::Raw, None);
        return Some(stage_a);
    }

    let provider_id = app_settings.post_process_provider_id.clone();

    let bypass_stage_b = !post_process
        || expanded.full_utterance_match
        || should_bypass_stage_b_for_local_capacity(&provider_id, &stage_a.final_text)
        || (fast_path::should_bypass_llm(
            &stage_a.final_text,
            effective_tone,
            flow_settings.fast_path_enabled,
            expanded.full_utterance_match,
        ) && !has_spoken_self_repair(&stage_a.final_text));

    if bypass_stage_b {
        let stage_kind = if stage_a.post_processed_text.is_some() || post_process {
            LinguistStageKind::Deterministic
        } else {
            LinguistStageKind::Raw
        };
        let provider_tag = if post_process
            && should_bypass_stage_b_for_local_capacity(&provider_id, &stage_a.final_text)
        {
            log::info!(
                "flow: long take ({} chars) exceeds '{}' interactive budget; delivering Stage-A deterministic polish immediately",
                stage_a.final_text.chars().count(),
                provider_id
            );
            "stage_a_long_take_fast_path"
        } else {
            "stage_a_deterministic"
        };
        timing::record_linguist_outcome(stage_kind, Some(provider_tag.into()));
        return Some(stage_a);
    }

    on_polishing_started(&stage_a.final_text);

    let script_rule = multilingual::llm_script_instruction(
        flow_settings.hindustani_script,
        &app_settings.spoken_languages,
    )
    .map(str::to_owned);
    let forbid_devanagari = matches!(
        flow_settings.hindustani_script,
        HindustaniScript::RomanHinglish | HindustaniScript::UrduPersoArabic
    );

    let linguist_req = lowyap_ports::LinguistRequest {
        raw_transcript: transcription.to_string(),
        stage_a_text: stage_a.final_text.clone(),
        tone: to_linguist_tone(effective_tone),
        field_context,
        lexicon_terms,
        language_code: None,
        script_rule,
        audio_duration_s,
        custom_prompt_override: None,
    };

    let toned_settings =
        prompt::prepare_linguist_settings(&app_settings, &linguist_req, effective_tone)
            .into_owned();
    let active_prompt = toned_settings
        .active_post_process_prompt()
        .map(|p| p.prompt.clone());
    let (turn_id, skip_notify) = paste_guard::begin_polishing();
    let stage_a_for_llm = stage_a.final_text.clone();

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
                    crate::actions::post_process_transcription(&toned_settings, &stage_a_for_llm)
                        .await
                })));

                let Some(handle) = abort_guard.0.as_mut() else {
                    return None;
                };

                let provider_for_log = provider_id.clone();
                tokio::select! {
                    biased;
                    _ = &mut notified => {
                        preferred = paste_guard::TurnWinner::Raw;
                        // Dropping `abort_guard` aborts cancellable HTTP/Ollama work immediately.
                        None
                    }
                    _ = tokio::time::sleep(STAGE_B_SLA_TIMEOUT) => {
                        log::warn!(
                            "flow: Stage-B provider '{}' exceeded {}ms interactive budget; delivering Stage-A polish immediately",
                            provider_for_log,
                            STAGE_B_SLA_TIMEOUT.as_millis()
                        );
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
        (paste_guard::TurnWinner::Raw, _) => {
            timing::record_linguist_outcome(
                LinguistStageKind::Deterministic,
                Some("stage_a_user_skip".into()),
            );
            Some(stage_a)
        }
        (paste_guard::TurnWinner::Llm, Some(processed_text)) => {
            let verdict = lowyap_flow::validate_linguist_output(
                &stage_a.final_text,
                &processed_text,
                forbid_devanagari,
            );
            if verdict.accepted {
                timing::record_linguist_outcome(
                    LinguistStageKind::ModelPolished,
                    Some(provider_id),
                );
                let post_processed_text =
                    (verdict.text != transcription.trim()).then(|| verdict.text.clone());
                Some(ProcessedTranscription {
                    final_text: verdict.text,
                    post_processed_text,
                    post_process_prompt: active_prompt,
                })
            } else {
                log::warn!(
                    "flow: linguist guard rejected provider '{}' output ({:?}); using Stage-A polish",
                    provider_id,
                    verdict.rejection_reason
                );
                timing::record_linguist_outcome(
                    LinguistStageKind::FallbackDeterministic,
                    Some(provider_id),
                );
                Some(stage_a)
            }
        }
        (paste_guard::TurnWinner::Llm, None) => {
            timing::record_linguist_outcome(
                LinguistStageKind::FallbackDeterministic,
                Some(provider_id),
            );
            Some(stage_a)
        }
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
    fn apple_intelligence_long_monologue_uses_instant_stage_a_fast_path() {
        let short_take = "Please send the updated deck before Friday.";
        let long_take = "a ".repeat(400); // 800 chars > 650-char interactive budget
        assert!(!should_bypass_stage_b_for_local_capacity(
            "apple_intelligence",
            short_take
        ));
        assert!(should_bypass_stage_b_for_local_capacity(
            "apple_intelligence",
            &long_take
        ));
        assert!(!should_bypass_stage_b_for_local_capacity(
            "openai", &long_take
        ));
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

    #[test]
    fn stage_a_result_collapses_stutters_snaps_brands_and_detects_self_repair() {
        let raw = "the and the kind of time low app takes versus whisper flow";
        let expanded = snippets::expand_snippets(raw, &[]);
        let lexicon = lowyap_flow::merged_lexicon_terms(&[]);
        let res = build_stage_a_result(raw, &expanded, Tone::Default, &lexicon, 8.0);
        assert_eq!(
            res.final_text,
            "The kind of time lowyap takes versus Wispr Flow."
        );
        assert_eq!(
            res.post_processed_text.as_deref(),
            Some("The kind of time lowyap takes versus Wispr Flow.")
        );
        assert!(has_spoken_self_repair(
            "Let's ship Tuesday, no wait Wednesday."
        ));
        assert!(!has_spoken_self_repair("Sounds good to me."));
    }
}
