//! Pure-Rust hierarchical map-reduce meeting summarizer, auto-title generator,
//! and local loopback GGUF (`Qwen3-4B-Instruct-2507`) adapter (`SV-90`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod budget;
pub mod local_gguf;
pub mod summarizer;
pub mod templates;
pub mod title;

pub use budget::{
    chunk_rollups_by_budget, chunk_segments_by_budget, contains_apple_intelligence_blocked_script,
    estimate_multilingual_tokens, fold_latin_diacritics_for_apple_intelligence,
    romanize_and_fold_for_apple_intelligence,
};
pub use local_gguf::{
    strip_reasoning_and_fences, validate_meeting_endpoint_url, CompletionBackend,
    LlamaServerLaunchSpec, LoopbackOpenAiBackend, QWEN3_4B_DEFAULT_QUANT, QWEN3_4B_GGUF_REPO,
    QWEN3_4B_OLLAMA_TAG,
};
pub use summarizer::{HybridMapReduceSummarizer, DEFAULT_MAX_INPUT_CHARS, MIN_MAX_INPUT_CHARS};
pub use templates::{
    required_headings, template_label, validate_expanded_notes, validate_rollup_bullets,
};
pub use title::{generate_meeting_title, is_placeholder_meeting_title};
