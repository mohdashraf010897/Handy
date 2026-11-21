//! Pure-Rust bilingual script governance, Roman Hinglish transliteration,
//! and Whisper trailing-silence / repetition hallucination guards (`SV-85`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod fast_path;
pub mod guard;
pub mod multilingual;
pub mod spoken_urls;

pub use fast_path::{
    apply_deterministic_fast_path, apply_deterministic_polish, should_bypass_llm, Tone,
};
pub use guard::{guard_transcript_text, pcm_rms_energy, SilenceGuardContext};
pub use multilingual::{
    build_initial_prompt, build_initial_prompt_for_model, candidate_languages_for_model,
    candidate_languages_for_spoken, contains_devanagari, contains_perso_arabic,
    devanagari_to_roman_hinglish, effective_hindustani_script, geometric_mean_token_prob,
    is_lang_compatible_with_spoken, is_repetitive_hallucination, llm_script_instruction,
    needs_spoken_language_disambiguation, needs_spoken_language_disambiguation_with_context,
    normalize_script_output, read_hindustani_script_from_path, score_candidate_transcript,
    score_candidate_transcript_for_language, should_retry_chunk_as_urdu,
    should_retry_chunk_as_urdu_with_context, split_bilingual_clauses, strip_hallucination_loops,
    HindustaniScript,
};
pub use spoken_urls::format_spoken_urls;
