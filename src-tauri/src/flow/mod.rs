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

/// Non-destructively reads structured code/product identifiers from the current
/// dictation turn's captured cursor context so ASR vocabulary biasing (`run_setup`)
/// can boost active symbols while leaving `LAST_FIELD_CONTEXT` intact for Stage-B.
pub(crate) fn peek_field_context_identifiers() -> Vec<String> {
    let current = FIELD_CONTEXT_GEN.load(Ordering::Acquire);
    let guard = LAST_FIELD_CONTEXT
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let snap = guard
        .as_ref()
        .and_then(|(gen, snap)| (*gen == current).then_some(snap));
    ax_context::extract_context_identifiers(snap)
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
        "you know",
        "scratch that",
        "actually wait",
        "or rather",
        "i'm saying",
        "i am saying",
        "sort of",
        "kind of",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
}

fn should_bypass_clean_medium_prose(
    text: &str,
    tone: settings::Tone,
    fast_path_enabled: bool,
) -> bool {
    if !fast_path_enabled || !matches!(tone, settings::Tone::Default | settings::Tone::Casual) {
        return false;
    }
    let word_count = text.split_whitespace().count();
    // Only bypass Stage-B on crisp 11..=14 word single-clause sentences; 15+ word
    // spoken thoughts go through Light-Fluency Polish so phrasing smooths naturally.
    if !(11..=14).contains(&word_count) {
        return false;
    }
    let trimmed = text.trim();
    if trimmed.matches(',').count() > 1 {
        return false;
    }
    if !trimmed.ends_with(['.', '?', '!']) && tone != settings::Tone::Casual {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    // Keep Stage-B engaged whenever spoken hedging, backtracking, formatting cues, or phonetic ASR slips appear.
    ![
        "no wait",
        "i mean",
        "scratch that",
        "actually",
        "or rather",
        "you know",
        "kind of",
        "sort of",
        "basically",
        "or something",
        "i feel",
        "i'm saying",
        "upto",
        "white line",
        "bullet point",
        "numbered list",
        "new line",
        "new paragraph",
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

/// Wall-clock interactive SLA for single-pass Stage-B LLM polishing.
/// Aligned 600ms above Apple Intelligence's 3.8s FFI semaphore so Swift cancels
/// cleanly and releases its serial lock before Rust's outer guard fires.
const STAGE_B_SLA_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(4_400);

/// Wall-clock budget for multi-line transforms (`Prompt Engineer` / `Custom`)
/// and longer single-pass monologues.
const STAGE_B_EXTENDED_SLA_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(6_200);

/// Per-chunk character ceiling for Apple Intelligence greedy plain-text Polish.
/// Keeping chunks at <=650 chars (~110 words, 2-4 sentences) keeps 3B under its
/// 3.8s per-chunk FFI SLA and prevents synonym-drift paraphrasing on 90s monologues.
const APPLE_INTELLIGENCE_PLAIN_CHUNK_MAX_CHARS: usize = 650;

/// Maximum character budget for clause-chunked local 3B Stage-B passes (~4 chunks).
/// Above ~2,600 chars (~440 words), Stage-A deterministic linguist (`<1ms`) handles
/// cleanup and paragraphing directly so multi-minute monologues paste immediately.
const APPLE_INTELLIGENCE_FAST_PATH_MAX_CHARS: usize = 2_600;

fn should_bypass_stage_b_for_local_capacity(provider_id: &str, stage_a_text: &str) -> bool {
    provider_id == crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID
        && stage_a_text.chars().count() > APPLE_INTELLIGENCE_FAST_PATH_MAX_CHARS
}

/// Apple Intelligence on-device FoundationModels only supports English/supported major
/// locales; sending Perso-Arabic Urdu/Arabic, Devanagari, or heavy Romanized Urdu/Hindi
/// triggers `"An unsupported language or locale was used"` or multi-second fallbacks.
/// Bypass immediately in `0ms` with deterministic Stage-A output.
fn looks_like_heavy_roman_hindustani(text: &str) -> bool {
    // Exclude words that collide with common English tokens ("the", "hum", "main", "is", "us").
    const MARKERS: &[&str] = &[
        "hai", "hain", "hoon", "nahi", "nahin", "kya", "kyun", "kaise", "aur", "lekin", "magar",
        "agar", "toh", "bhi", "isko", "usko", "iske", "uske", "karein", "karenge", "karna",
        "karta", "karti", "hoga", "hogi", "honge", "tha", "thi", "mein", "tum", "aap", "yahan",
        "wahan", "achha", "acha", "matlab", "samajh", "dekho", "batao", "bohot", "bahut", "kabhi",
        "abhi", "phir", "vala", "wala", "wali",
    ];
    let mut hits = 0usize;
    for raw in text.split_whitespace() {
        let tok = raw
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .to_ascii_lowercase();
        if tok.len() >= 3 && MARKERS.contains(&tok.as_str()) {
            hits += 1;
            if hits >= 3 {
                return true;
            }
        }
    }
    false
}

fn should_bypass_stage_b_for_unsupported_script(provider_id: &str, stage_a_text: &str) -> bool {
    provider_id == crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID
        && (lowyap_flow::contains_perso_arabic(stage_a_text)
            || lowyap_flow::contains_devanagari(stage_a_text)
            || looks_like_heavy_roman_hindustani(stage_a_text))
}

/// Runs the 2-stage plug-and-play Linguist pipeline:
/// - Stage A (`lowyap_flow::apply_stage_a_linguist`): `<1ms` pure-Rust stutter/filler
///   cleanup, brand & user lexicon snapping, URL formatting, tone polish, and
///   multi-paragraph structuring on `>=45s` / `>=85`-word monologues.
/// - Stage B (`post_process_transcription` + `validate_linguist_output_for_mode`):
///   optional model-agnostic rewrite (local or cloud) with clause-chunked 3B safety,
///   `Prompt Engineer` mode, cursor-context continuity, hallucination guard, and
///   `[Paste raw now]` escape hatch.
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

    let ax_identifiers = ax_context::extract_context_identifiers(field_context.as_ref());
    let base_lexicon = lowyap_flow::merged_lexicon_terms(&app_settings.custom_words);
    let lexicon_terms = lowyap_flow::merged_asr_bias_terms(&base_lexicon, &ax_identifiers, 48);
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
    let linguist_mode = prompt::linguist_mode_for_settings(&app_settings);

    let bypass_non_latin =
        should_bypass_stage_b_for_unsupported_script(&provider_id, &stage_a.final_text);
    let bypass_stage_b = !post_process
        || expanded.full_utterance_match
        || bypass_non_latin
        || should_bypass_stage_b_for_local_capacity(&provider_id, &stage_a.final_text)
        || (linguist_mode == lowyap_ports::LinguistMode::Polish
            && (fast_path::should_bypass_llm(
                &stage_a.final_text,
                effective_tone,
                flow_settings.fast_path_enabled,
                expanded.full_utterance_match,
            ) || should_bypass_clean_medium_prose(
                &stage_a.final_text,
                effective_tone,
                flow_settings.fast_path_enabled,
            ))
            && !has_spoken_self_repair(transcription)
            && !has_spoken_self_repair(&stage_a.final_text));

    if bypass_stage_b {
        let stage_kind = if stage_a.post_processed_text.is_some() || post_process {
            LinguistStageKind::Deterministic
        } else {
            LinguistStageKind::Raw
        };
        let provider_tag = if post_process && bypass_non_latin {
            "stage_a_non_latin_fast_path"
        } else if post_process
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
        mode: linguist_mode,
    };

    let toned_settings =
        prompt::prepare_linguist_settings(&app_settings, &linguist_req, effective_tone)
            .into_owned();
    let active_prompt = toned_settings
        .active_post_process_prompt()
        .map(|p| p.prompt.clone());
    let (turn_id, skip_notify) = paste_guard::begin_polishing();

    let use_clause_chunks = provider_id == crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID
        && linguist_mode == lowyap_ports::LinguistMode::Polish
        && stage_a.final_text.chars().count() > APPLE_INTELLIGENCE_PLAIN_CHUNK_MAX_CHARS;
    let chunks = if use_clause_chunks {
        lowyap_flow::split_stage_b_chunks(
            &stage_a.final_text,
            APPLE_INTELLIGENCE_PLAIN_CHUNK_MAX_CHARS,
        )
    } else {
        vec![stage_a.final_text.clone()]
    };
    let sla_timeout = if chunks.len() > 1 {
        let scaled_ms = 4_400u64.saturating_add((chunks.len() as u64 - 1).saturating_mul(3_600));
        std::time::Duration::from_millis(scaled_ms.min(12_000))
    } else if linguist_mode != lowyap_ports::LinguistMode::Polish
        || stage_a.final_text.chars().count() > 500
    {
        STAGE_B_EXTENDED_SLA_TIMEOUT
    } else {
        STAGE_B_SLA_TIMEOUT
    };

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

                let is_multi_chunk = chunks.len() > 1;
                let mut abort_guard = AbortOnDrop(Some(tokio::spawn(async move {
                    if !is_multi_chunk {
                        let single = chunks.into_iter().next()?;
                        return crate::actions::post_process_transcription(&toned_settings, &single)
                            .await;
                    }
                    let mut polished_parts = Vec::with_capacity(chunks.len());
                    let mut accepted_any = false;
                    for chunk in chunks {
                        if paste_guard::is_skip_requested(turn_id) {
                            return None;
                        }
                        match crate::actions::post_process_transcription(&toned_settings, &chunk)
                            .await
                        {
                            Some(candidate) => {
                                let part_verdict = lowyap_flow::validate_linguist_output_for_mode(
                                    &chunk,
                                    &candidate,
                                    forbid_devanagari,
                                    linguist_mode,
                                );
                                if part_verdict.accepted {
                                    accepted_any = true;
                                }
                                polished_parts.push(part_verdict.text);
                            }
                            None => {
                                polished_parts.push(chunk);
                            }
                        }
                    }
                    accepted_any.then(|| polished_parts.join(" "))
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
                    _ = tokio::time::sleep(sla_timeout) => {
                        log::warn!(
                            "flow: Stage-B provider '{}' exceeded {}ms interactive budget; delivering Stage-A polish immediately",
                            provider_for_log,
                            sla_timeout.as_millis()
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
            let verdict = lowyap_flow::validate_linguist_output_for_mode(
                &stage_a.final_text,
                &processed_text,
                forbid_devanagari,
                linguist_mode,
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

/// Outcome of an on-demand Transform invocation (`⌥1` or overlay/Studio menu).
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct TransformOutcome {
    /// Transformed output text.
    pub text: String,
    /// `"replaced_selection"` when highlighted text was replaced in-place, or
    /// `"copied_last_dictation"` when transformed onto the clipboard.
    pub target: String,
    /// Active preset ID used for the transform.
    pub preset_id: String,
}

/// Transforms an arbitrary input string on-demand using `prompt_id_override`
/// (or the currently selected preset) and the configured local/cloud provider.
pub(crate) async fn transform_text_now(
    app: &AppHandle,
    source_text: &str,
    prompt_id_override: Option<&str>,
) -> Result<(String, String), String> {
    let trimmed = source_text.trim();
    if trimmed.is_empty() {
        return Err("Nothing to transform".into());
    }

    let flow_settings = settings::get(app);
    let mut app_settings = crate::settings::get_settings(app);
    if let Some(override_id) = prompt_id_override {
        if app_settings
            .post_process_prompts
            .iter()
            .any(|p| p.id == override_id)
        {
            app_settings.post_process_selected_prompt_id = Some(override_id.to_owned());
        }
    }

    // Zero-key local fallback on Apple Silicon if the user hasn't entered a cloud key.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let current_provider = app_settings.post_process_provider_id.as_str();
        let missing_cloud_key = current_provider != crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID
            && current_provider != "custom"
            && app_settings
                .post_process_api_keys
                .get(current_provider)
                .is_none_or(|k| k.trim().is_empty());
        if missing_cloud_key {
            app_settings.post_process_provider_id =
                crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID.to_string();
        }
    }

    let preset_id = app_settings
        .active_post_process_prompt()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| lowyap_flow::BUILTIN_POLISH_PROMPT_ID.to_string());
    let linguist_mode = prompt::linguist_mode_for_settings(&app_settings);

    let (app_ctx, _) = context::capture_timed();
    let effective_tone = if flow_settings.tone_enabled {
        context::tone_for(app_ctx.as_ref(), &flow_settings)
    } else {
        settings::Tone::Default
    };
    let lexicon_terms = lowyap_flow::merged_lexicon_terms(&app_settings.custom_words);
    let stage_a = lowyap_flow::apply_stage_a_linguist(
        trimmed,
        to_linguist_tone(effective_tone),
        &lexicon_terms,
        (trimmed.split_whitespace().count() as f64 / 2.5).max(1.0),
    );

    let script_rule = multilingual::llm_script_instruction(
        flow_settings.hindustani_script,
        &app_settings.spoken_languages,
    )
    .map(str::to_owned);
    let forbid_devanagari = matches!(
        flow_settings.hindustani_script,
        HindustaniScript::RomanHinglish | HindustaniScript::UrduPersoArabic
    );

    let surface_ctx = ax_context::PlatformFieldContextProvider::new(app_ctx.as_ref())
        .capture_field_context();

    let linguist_req = lowyap_ports::LinguistRequest {
        raw_transcript: trimmed.to_string(),
        stage_a_text: stage_a.clone(),
        tone: to_linguist_tone(effective_tone),
        field_context: surface_ctx,
        lexicon_terms,
        language_code: None,
        script_rule,
        audio_duration_s: 5.0,
        custom_prompt_override: None,
        mode: linguist_mode,
    };

    let toned_settings =
        prompt::prepare_linguist_settings(&app_settings, &linguist_req, effective_tone)
            .into_owned();

    let use_clause_chunks = app_settings.post_process_provider_id
        == crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID
        && linguist_mode == lowyap_ports::LinguistMode::Polish
        && stage_a.chars().count() > APPLE_INTELLIGENCE_PLAIN_CHUNK_MAX_CHARS;

    let llm_result = if use_clause_chunks {
        let chunks =
            lowyap_flow::split_stage_b_chunks(&stage_a, APPLE_INTELLIGENCE_PLAIN_CHUNK_MAX_CHARS);
        let mut out = Vec::with_capacity(chunks.len());
        let mut any_accepted = false;
        for chunk in chunks {
            match crate::actions::post_process_transcription(&toned_settings, &chunk).await {
                Some(candidate) => {
                    let v = lowyap_flow::validate_linguist_output_for_mode(
                        &chunk,
                        &candidate,
                        forbid_devanagari,
                        linguist_mode,
                    );
                    if v.accepted {
                        any_accepted = true;
                    }
                    out.push(v.text);
                }
                None => out.push(chunk),
            }
        }
        any_accepted.then(|| out.join(" "))
    } else {
        tokio::time::timeout(
            STAGE_B_EXTENDED_SLA_TIMEOUT,
            crate::actions::post_process_transcription(&toned_settings, &stage_a),
        )
        .await
        .ok()
        .flatten()
    };

    let final_text = match llm_result {
        Some(candidate) => {
            let verdict = lowyap_flow::validate_linguist_output_for_mode(
                &stage_a,
                &candidate,
                forbid_devanagari,
                linguist_mode,
            );
            verdict.text
        }
        None if linguist_mode == lowyap_ports::LinguistMode::PromptEngineer => {
            lowyap_flow::structure_zero_loss_prompt(&stage_a)
        }
        None => stage_a,
    };

    Ok((final_text, preset_id))
}

/// Runs the global Transform action (`⌥1` or overlay button):
/// 1. If text is highlighted in the focused app (`AXSelectedText`), transforms it
///    and replaces the selection in-place via paste.
/// 2. Otherwise transforms the most recent dictation (`paste_guard` or latest
///    history entry) onto the clipboard and flashes `"transformed_copied"` so
///    unhighlighted editor text is never double-pasted.
pub(crate) async fn run_transform_action(
    app: AppHandle,
    prompt_id_override: Option<String>,
) -> Result<TransformOutcome, String> {
    use tauri::Manager;
    use tauri_plugin_clipboard_manager::ClipboardExt;

    if !matches!(
        crate::session_bus::bus().snapshot().dictation,
        lowyap_ports::DictationPhase::Idle
    ) {
        return Err("Dictation is currently active".into());
    }

    let (front_ctx, _) = context::capture_timed();
    let own_id = app.config().identifier.as_str();
    context::remember(front_ctx.as_ref(), &[own_id, "lowyap", "Handy"]);
    let selected = ax_context::capture_selected_text();
    let (source_text, replace_in_place) = if let Some(sel) = selected {
        (sel, true)
    } else if let Some(last) = paste_guard::get_last_dictation() {
        (last, false)
    } else if let Some(hm) = app.try_state::< std::sync::Arc<crate::managers::history::HistoryManager> >() {
        let latest = hm
            .get_history_entries(None, Some(1))
            .await
            .ok()
            .and_then(|page| page.entries.into_iter().next())
            .map(|entry| entry.transcription_text)
            .filter(|t| !t.trim().is_empty());
        match latest {
            Some(text) => (text, false),
            None => {
                crate::overlay::show_overlay_notice(&app, "transform_empty");
                return Err("No selected text or recent dictation to transform".into());
            }
        }
    } else {
        crate::overlay::show_overlay_notice(&app, "transform_empty");
        return Err("No selected text or recent dictation to transform".into());
    };

    // Recover the original raw dictation/selection if `source_text` was produced by an
    // earlier Transform style (e.g. switching back from `Prompt Engineer` to `Polish`).
    let effective_source = paste_guard::resolve_transform_source(&source_text);

    crate::overlay::show_processing_overlay(&app);
    let result = transform_text_now(&app, &effective_source, prompt_id_override.as_deref()).await;

    let (transformed, preset_id) = match result {
        Ok(pair) => pair,
        Err(err) => {
            crate::overlay::hide_recording_overlay(&app);
            return Err(err);
        }
    };

    paste_guard::record_transform_output(&effective_source, &transformed);

    if replace_in_place {
        let app_for_paste = app.clone();
        let text_for_paste = transformed.clone();
        let paste_res = tokio::task::spawn_blocking(move || {
            crate::utils::paste(text_for_paste, app_for_paste)
        })
        .await
        .map_err(|e| e.to_string())?;
        crate::overlay::hide_recording_overlay(&app);
        if let Err(err) = paste_res {
            app.clipboard()
                .write_text(transformed.clone())
                .map_err(|e| e.to_string())?;
            crate::overlay::show_overlay_notice(&app, "transformed_copied");
            log::warn!("flow: in-place selection paste failed ({err}); copied transformed text");
            return Ok(TransformOutcome {
                text: transformed,
                target: "copied_last_dictation".into(),
                preset_id,
            });
        }
        Ok(TransformOutcome {
            text: transformed,
            target: "replaced_selection".into(),
            preset_id,
        })
    } else {
        app.clipboard()
            .write_text(transformed.clone())
            .map_err(|e| e.to_string())?;
        crate::overlay::show_overlay_notice(&app, "transformed_copied");
        Ok(TransformOutcome {
            text: transformed,
            target: "copied_last_dictation".into(),
            preset_id,
        })
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
    fn apple_intelligence_chunked_budget_accepts_927_char_take_and_caps_runaway_monologue() {
        let take_89_len = "a ".repeat(464); // 928 chars: now handled via 3x <=380-char chunks!
        let marathon_take = "a ".repeat(1_400); // 2,800 chars > 2,600-char chunked interactive cap
        assert!(!should_bypass_stage_b_for_local_capacity(
            "apple_intelligence",
            &take_89_len
        ));
        assert!(should_bypass_stage_b_for_local_capacity(
            "apple_intelligence",
            &marathon_take
        ));
        assert!(!should_bypass_stage_b_for_local_capacity(
            "openai",
            &marathon_take
        ));
    }

    #[test]
    fn roman_hindustani_detector_ignores_english_definite_articles_and_catches_real_hinglish() {
        let english = "The architecture of the system and the UI pipeline are working smoothly, hum along nicely.";
        assert!(!looks_like_heavy_roman_hindustani(english));

        let roman_hinglish = "Isko apply karein toh kya hoga aur batao?";
        assert!(looks_like_heavy_roman_hindustani(roman_hinglish));
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "Requires live macOS 26+ Apple Intelligence FoundationModels runtime"]
    async fn live_wave6_apple_intelligence_benchmark() {
        if !crate::apple_intelligence::check_apple_intelligence_availability() {
            println!("Apple Intelligence unavailable on this runner; skipping live probe.");
            return;
        }

        let cases = [
            (
                89,
                "One more thing that you could add is the buy me a coffee icon. It is catchy and you know fetches the attention of the people now let's get back to the quite build a user's plan and along with that let's have a round table again let's discuss on where we are what we need to do in terms of engineering efforts in terms of matching the feature parity with the competitors who already does the job well and let's see what we do and i also see that the post processing by whisper like it has got a third transform button i will show you the screenshot pasting it in and it polishes very well although the polish that we do its not upto the marks, yeah it does removes some filler words and gaps But the kind of polish that we promised to our website, it is not yet achieved that I feel. when I am writing prompts like are we not doing that can we check in a code whether it silently dropped somewhere in the white line or something",
            ),
            (
                90,
                "There is one gap here. It did not use the correct icon of the LOWYAP. that's one concern then in the meeting recording I don't know whether English is the right thing to use there because Not everyone understands, I mean it has to be a global product, right?",
            ),
            (
                91,
                "But remember we don't regress on the other fixes that we have done it has to be a progressive Iteration and no regression is allowed here",
            ),
            (
                93,
                "Okay, I see that the last response actually took like 724. tokens and that's too much. I think why did it take too much of it and there is one more review which I like about Wispr Flow is that they let me Let me show you the video actually. I am going to record it so there is a side. There is a slight element that gives me certain options and they are not covering my screen at all. They require the least space and then they give me the placement option of it and only the icon is visible so that I can place it wherever on the screen I want. It is like on three places as of now, so it's quite cool.",
            ),
            (
                95,
                "Please do wave 7. Go on wave 7. Take a look at the polishing change you did, to see how much time it takes.",
            ),
            (
                96,
                "Did you even compare the user experience that Your edge dock provides us with its snow. It's so snappier and not smooth at all. and it sticks to It sticks to like ever showing up versus this per flows doc takes only Minimal space If you want that, I can provide that to you",
            ),
        ];

        let mut base_settings = crate::settings::get_default_settings();
        base_settings.post_process_enabled = true;
        base_settings.post_process_provider_id =
            crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID.to_string();
        base_settings.custom_words = vec!["lowyap".to_string()];

        let lexicon = lowyap_flow::merged_lexicon_terms(&base_settings.custom_words);

        for (id, raw) in cases {
            let expanded = expand_snippets_and_urls(raw, &[]);
            let stage_a =
                build_stage_a_result(raw, &expanded, Tone::Default, &lexicon, 12.0).final_text;

            // 1. Polish mode (clause-chunked <= 380 chars, @Generable CleanedTranscript)
            let mut polish_settings = base_settings.clone();
            polish_settings.post_process_selected_prompt_id =
                Some(lowyap_flow::BUILTIN_POLISH_PROMPT_ID.to_string());
            let polish_req = lowyap_ports::LinguistRequest {
                raw_transcript: raw.to_string(),
                stage_a_text: stage_a.clone(),
                tone: lowyap_ports::LinguistTone::Default,
                field_context: None,
                lexicon_terms: lexicon.clone(),
                language_code: None,
                script_rule: None,
                audio_duration_s: 12.0,
                custom_prompt_override: None,
                mode: lowyap_ports::LinguistMode::Polish,
            };
            let toned_polish =
                prompt::prepare_linguist_settings(&polish_settings, &polish_req, Tone::Default)
                    .into_owned();

            let t0 = std::time::Instant::now();
            let chunks = if stage_a.chars().count() > APPLE_INTELLIGENCE_PLAIN_CHUNK_MAX_CHARS {
                lowyap_flow::split_stage_b_chunks(&stage_a, APPLE_INTELLIGENCE_PLAIN_CHUNK_MAX_CHARS)
            } else {
                vec![stage_a.clone()]
            };
            let mut polished_parts = Vec::with_capacity(chunks.len());
            for chunk in &chunks {
                let out = crate::actions::post_process_transcription(&toned_polish, chunk)
                    .await
                    .expect("Polish Apple Intelligence call should succeed");
                let verdict = lowyap_flow::validate_linguist_output_for_mode(
                    chunk,
                    &out,
                    false,
                    lowyap_ports::LinguistMode::Polish,
                );
                assert!(
                    verdict.accepted,
                    "Polish chunk rejected ({:?}): {out}",
                    verdict.rejection_reason
                );
                polished_parts.push(verdict.text);
            }
            let polished = polished_parts.join(" ");
            let polish_ms = t0.elapsed().as_millis();

            // 2. Prompt Engineer mode (plain-text mode token_limit = -1, 5800ms budget)
            let mut pe_settings = base_settings.clone();
            pe_settings.post_process_selected_prompt_id =
                Some(lowyap_flow::BUILTIN_PROMPT_ENGINEER_ID.to_string());
            let pe_req = lowyap_ports::LinguistRequest {
                raw_transcript: raw.to_string(),
                stage_a_text: stage_a.clone(),
                tone: lowyap_ports::LinguistTone::Default,
                field_context: None,
                lexicon_terms: lexicon.clone(),
                language_code: None,
                script_rule: None,
                audio_duration_s: 12.0,
                custom_prompt_override: None,
                mode: lowyap_ports::LinguistMode::PromptEngineer,
            };
            let toned_pe =
                prompt::prepare_linguist_settings(&pe_settings, &pe_req, Tone::Default)
                    .into_owned();

            let t1 = std::time::Instant::now();
            let pe_raw = crate::actions::post_process_transcription(&toned_pe, &stage_a)
                .await
                .expect("Prompt Engineer Apple Intelligence call should succeed");
            let pe_verdict = lowyap_flow::validate_linguist_output_for_mode(
                &stage_a,
                &pe_raw,
                false,
                lowyap_ports::LinguistMode::PromptEngineer,
            );
            assert!(
                pe_verdict.accepted,
                "Prompt Engineer output rejected ({:?}): {pe_raw}",
                pe_verdict.rejection_reason
            );
            let pe_out = pe_verdict.text;
            let pe_ms = t1.elapsed().as_millis();

            println!(
                "\n================ TAKE #{id} ({} chars, {} chunks) ================",
                raw.len(),
                chunks.len()
            );
            println!("[STAGE-A]: {stage_a}");
            println!("[POLISH {polish_ms}ms]: {polished}");
            println!("[PROMPT ENGINEER {pe_ms}ms]:\n{pe_out}");

            assert!(!polished.trim().is_empty());
            assert!(!pe_out.trim().is_empty());
            assert!(!polished.contains("<before_cursor>"));
            assert!(!polished.contains("<after_cursor>"));
            assert!(!pe_out.contains("<before_cursor>"));
            assert!(!pe_out.contains("<after_cursor>"));
            if id == 90 {
                assert!(
                    !polished.contains("LOWYAP"),
                    "Polish output for #90 must preserve lowercase 'lowyap': {polished}"
                );
            }
            if id == 96 {
                assert!(
                    polished.to_ascii_lowercase().contains("not"),
                    "Polish output for #96 must preserve negation ('not'): {polished}"
                );
                assert!(
                    polished.contains("Wispr Flow"),
                    "Polish output for #96 must preserve 'Wispr Flow': {polished}"
                );
            }
        }
    }
}
