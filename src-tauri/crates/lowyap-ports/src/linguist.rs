//! Engine-agnostic contracts for incremental speech chunking, active text-field
//! context capture, and pluggable linguistic rewriting.
//!
//! Callers can combine any speech-to-text backend, any OS text-field context
//! adapter, and any deterministic or model-backed rewriter without coupling
//! core logic to a specific vendor, prompt format, or runtime.

use serde::{Deserialize, Serialize};

use crate::ports::PortError;

/// Policy governing how speech is split at natural pauses for incremental or
/// batch decoding across any non-streaming speech-to-text backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChunkingPolicy {
    /// Preferred target chunk duration in milliseconds (`e.g. 14_000` live, `30_000` batch).
    pub target_chunk_ms: u64,
    /// Maximum audio duration in milliseconds decoded as a single chunk before cutting.
    pub max_chunk_ms: u64,
    /// Minimum chunk duration in milliseconds so tiny fragments are never emitted alone.
    pub min_chunk_ms: u64,
    /// Lookahead past `max_chunk_ms` in milliseconds required before finalizing a live cut.
    pub lookahead_ms: u64,
    /// Minimum coalesced clause duration in milliseconds for bilingual disambiguation.
    pub min_bilingual_clause_ms: u64,
}

impl ChunkingPolicy {
    /// Tuned for live push-to-talk or toggle dictation: commits completed pauses
    /// every ~8.0 s (`4.5 s` min, `12.0 s` max) while the user is speaking so
    /// mid-sentence thinking pauses stay anchored inside surrounding speech context
    /// while key-release still decodes only a short final tail (`< 0.9 s` on Metal),
    /// regardless of the underlying ASR engine.
    pub const fn live_dictation() -> Self {
        Self {
            target_chunk_ms: 8_000,
            max_chunk_ms: 12_000,
            min_chunk_ms: 4_500,
            lookahead_ms: 700,
            min_bilingual_clause_ms: 11_000,
        }
    }

    /// Tuned for offline file transcription and historical batch clips, preserving
    /// upstream 30 s / 40 s / 5 s cut boundaries.
    pub const fn batch_clip() -> Self {
        Self {
            target_chunk_ms: 30_000,
            max_chunk_ms: 40_000,
            min_chunk_ms: 5_000,
            lookahead_ms: 5_000,
            min_bilingual_clause_ms: 12_000,
        }
    }
}

impl Default for ChunkingPolicy {
    fn default() -> Self {
        Self::batch_clip()
    }
}

/// Broad surface category of the target application, independent of OS bundle IDs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppSurfaceKind {
    /// General text input when no narrower category applies.
    #[default]
    General,
    /// Instant messaging or team chat.
    Chat,
    /// Email client, document editor, or long-form notes.
    EmailOrDoc,
    /// IDE, code editor, or terminal emulator.
    CodeOrTerminal,
    /// Web browser surface.
    Browser,
}

/// Target register requested for linguistic polishing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinguistTone {
    /// Natural written prose preserving the speaker's register.
    #[default]
    Default,
    /// Concise, conversational chat style.
    Casual,
    /// Complete, professional prose for email or documents.
    Formal,
    /// Technical/code literal style preserving identifiers, flags, and paths.
    Code,
}

/// Privacy-bounded snapshot of text immediately surrounding the insertion cursor.
///
/// Window titles, URLs, and app bundle identifiers are intentionally excluded
/// so adapters can supply local syntactic/casing continuity without leaking
/// identity metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FieldContextSnapshot {
    /// Bounded text immediately preceding the cursor or selection (e.g. `<= 160` chars).
    pub before_cursor: Option<String>,
    /// Bounded text immediately following the cursor or selection (e.g. `<= 80` chars).
    pub after_cursor: Option<String>,
    /// Whether the target text field currently has a non-empty selection.
    pub has_selection: bool,
    /// Broad surface category of the target field.
    pub surface: AppSurfaceKind,
    /// Time spent capturing the snapshot in microseconds.
    pub capture_us: u64,
}

/// OS-independent port for reading bounded cursor context from the focused text field.
pub trait FieldContextProvider: Send + Sync {
    /// Stable adapter identifier (e.g. `"macos_ax"`, `"noop"`).
    fn name(&self) -> &'static str;

    /// Captures bounded surrounding text if permitted and supported by the focused control.
    fn capture_field_context(&self) -> Option<FieldContextSnapshot>;
}

/// Vendor-neutral request passed to any linguistic rewriter adapter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LinguistRequest {
    /// Raw ASR output before deterministic cleanup.
    pub raw_transcript: String,
    /// Output of Stage-A deterministic normalization (stutter/lexicon/URL pass).
    pub stage_a_text: String,
    /// Requested writing register.
    pub tone: LinguistTone,
    /// Spoken audio duration in seconds (used for paragraphing heuristics).
    pub audio_duration_s: f64,
    /// Detected or selected ISO language code, if known.
    pub language_code: Option<String>,
    /// Optional script constraint (e.g. Roman Hinglish vs Nastaliq Urdu).
    pub script_rule: Option<String>,
    /// Domain/brand vocabulary terms to preserve with exact spelling.
    pub lexicon_terms: Vec<String>,
    /// Optional bounded text around the active insertion cursor.
    pub field_context: Option<FieldContextSnapshot>,
    /// Optional user-configured custom base prompt template.
    pub custom_prompt_override: Option<String>,
}

/// Output returned by a [`LinguistRewriter`] adapter prior to output-guard validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LinguistRewrite {
    /// Rewritten transcript candidate.
    pub text: String,
    /// Opaque identifier of the provider/model that produced the candidate.
    pub provider_id: String,
}

/// Object-safe port for any synchronous or bridged linguistic rewriter
/// (deterministic-only, Apple Intelligence, local GGUF/llama-server, Ollama,
/// ONNX GEC, or OpenAI-compatible HTTP endpoint).
pub trait LinguistRewriter: Send + Sync {
    /// Stable adapter identifier.
    fn name(&self) -> &'static str;

    /// Rewrites `req` into clean written prose while preserving speaker intent.
    ///
    /// # Errors
    /// Returns [`PortError`] if the underlying provider is unavailable or fails.
    fn rewrite(&self, req: &LinguistRequest) -> Result<LinguistRewrite, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_dictation_policy_commits_earlier_than_batch_policy() {
        let live = ChunkingPolicy::live_dictation();
        let batch = ChunkingPolicy::batch_clip();
        assert!(live.target_chunk_ms < batch.target_chunk_ms);
        assert!(live.max_chunk_ms < batch.max_chunk_ms);
        assert!(live.lookahead_ms < batch.lookahead_ms);
        assert!(live.min_chunk_ms > 0);
    }
}
