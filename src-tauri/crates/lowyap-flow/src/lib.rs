//! Pure-Rust bilingual script governance, Roman Hinglish transliteration,
//! and Whisper trailing-silence / repetition hallucination guards (`SV-85`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod accent_asr;
pub mod fast_path;
pub mod guard;
pub mod linguist;
pub mod multilingual;
pub mod spoken_urls;

pub use accent_asr::{
    accent_phonetic_similarity, accent_phonetic_skeleton, inspect_compression_and_confidence,
    is_accent_phonetic_match, is_immune_common_english_word, is_non_speech_energy_floor,
    mask_interior_silence_collars, stitch_overlapping_transcripts, stitch_transcript_boundaries,
    CompressionConfidenceVerdict, CompressionGateReason,
};

pub use fast_path::{
    apply_deterministic_fast_path, apply_deterministic_polish, should_bypass_llm, Tone,
};
pub use guard::{guard_transcript_text, pcm_rms_energy, SilenceGuardContext};
pub use linguist::{
    apply_stage_a_linguist, compose_linguist_prompt_template, compose_prompt_engineer_template,
    merged_asr_bias_terms, merged_lexicon_terms, render_linguist_prompt,
    render_prompt_engineer_prompt, run_plug_and_play_linguist, split_stage_b_chunks,
    structure_zero_loss_prompt, validate_linguist_output, validate_linguist_output_for_mode,
    DeterministicOnlyRewriter, GuardVerdict, BUILTIN_POLISH_PROMPT_ID, BUILTIN_PROMPT_ENGINEER_ID,
    CORE_BRAND_LEXICON, DEFAULT_STAGE_B_CHUNK_MAX_CHARS, STARTER_TECH_VOCABULARY,
};
pub use multilingual::{
    build_initial_prompt, build_initial_prompt_for_model, candidate_languages_for_model,
    candidate_languages_for_spoken, contains_devanagari, contains_perso_arabic,
    contains_urdu_specific_perso_arabic, devanagari_to_roman_hinglish, effective_hindustani_script,
    geometric_mean_token_prob, hindustani_to_roman_hinglish,
    hindustani_to_roman_preserving_arabic, is_lang_compatible_with_spoken,
    is_repetitive_hallucination, llm_script_instruction, needs_spoken_language_disambiguation,
    needs_spoken_language_disambiguation_with_context, normalize_script_output,
    perso_arabic_to_roman_hinglish, read_hindustani_script_from_path,
    restore_english_loanwords_in_perso_arabic, score_candidate_transcript,
    score_candidate_transcript_for_language, should_retry_chunk_as_urdu,
    should_retry_chunk_as_urdu_with_context, split_bilingual_clauses,
    split_bilingual_clauses_with_min_ms, strip_hallucination_loops, HindustaniScript,
};
pub use spoken_urls::format_spoken_urls;
