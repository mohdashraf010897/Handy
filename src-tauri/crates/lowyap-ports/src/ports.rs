//! Object-safe interfaces for audio processing, note generation, and event
//! delivery. Callers choose where to run synchronous adapters and how to
//! serialize access to stateful ones.

use serde::{Deserialize, Serialize};

use crate::domain::{
    MeetingTemplate, OutputRouteKind, SpeakerChannel, SpeakerIdentity, TranscriptSegment,
    UtteranceSpan,
};
use crate::session_bus::SessionBusSnapshot;

/// An adapter error message, independent of its underlying implementation.
pub type PortError = String;

/// Suppresses system-audio bleed from the microphone track.
pub trait EchoCanceller: Send {
    /// Stable adapter name.
    fn name(&self) -> &'static str;

    /// Cleans 16 kHz microphone PCM using an aligned system-audio reference.
    /// The reference may be empty when system audio is unavailable.
    fn process_16k(
        &mut self,
        me_16k: &[f32],
        others_ref_16k: &[f32],
        route: OutputRouteKind,
    ) -> Vec<f32>;

    /// Clears state before processing a new session.
    fn reset(&mut self);
}

/// Buffers per-channel PCM and emits complete speech spans.
pub trait UtteranceSegmenter: Send {
    /// Stable adapter name.
    fn name(&self) -> &'static str;

    /// Consumes one 16 kHz chunk and returns any spans completed by it.
    fn push_chunk(
        &mut self,
        channel: SpeakerChannel,
        samples_16k: &[f32],
        chunk_start_ms: u64,
    ) -> Vec<UtteranceSpan>;

    /// Drains pending spans, including a final span without trailing silence.
    fn flush(&mut self) -> Vec<UtteranceSpan>;
}

/// Converts 16 kHz speech to text.
pub trait AsrProvider: Send + Sync {
    /// Stable adapter name.
    fn name(&self) -> &'static str;

    /// Transcribes mono PCM, optionally guided by a language hint.
    ///
    /// # Errors
    /// Returns the adapter's error message if transcription fails.
    fn transcribe_16k(
        &self,
        samples_16k: &[f32],
        language_hint: Option<&str>,
    ) -> Result<AsrResult, PortError>;
}

/// Transcript and optional metadata returned by automatic speech recognition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrResult {
    /// Recognized text.
    pub text: String,
    /// Language detected by the provider, if available.
    pub detected_language: Option<String>,
    /// Provider's estimated probability that the input contains no speech.
    pub no_speech_prob: Option<f32>,
}

/// Assigns speaker identities within a meeting.
pub trait SpeakerDiarizer: Send {
    /// Stable adapter name.
    fn name(&self) -> &'static str;

    /// Provides a clean `ME_ONLY` microphone window for local-speaker enrollment.
    /// Callers should supply these windows only while system audio is silent.
    fn observe_me_clean(&mut self, clean_me_16k: &[f32]);

    /// Attributes an utterance, using its channel when finer attribution is unavailable.
    fn identify(&mut self, span: &UtteranceSpan) -> SpeakerIdentity;

    /// Clears session-local enrollment and speaker assignments.
    fn reset(&mut self);
}

/// Generates rolling meeting summaries and expanded notes.
pub trait NoteSummarizer: Send + Sync {
    /// Stable adapter name.
    fn name(&self) -> &'static str;

    /// Maximum input size in characters, not bytes or model tokens.
    fn max_input_chars(&self) -> usize;

    /// Summarizes one transcript window.
    ///
    /// # Errors
    /// Returns the adapter's error message if the window cannot be summarized.
    fn summarize_window(&self, req: &WindowSummaryRequest) -> Result<SummaryOutput, PortError>;

    /// Combines rough notes, rollups, and recent transcript into expanded notes.
    ///
    /// # Errors
    /// Returns the adapter's error message if note expansion fails.
    fn expand_hybrid_notes(&self, req: &HybridNotesRequest) -> Result<SummaryOutput, PortError>;
}

/// Input for one rolling transcript summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WindowSummaryRequest {
    /// Index of the window in the meeting.
    pub window_index: u32,
    /// Transcribed segments in this window.
    pub segments: Vec<TranscriptSegment>,
    /// Optional instruction for the output language.
    pub language_instruction: Option<String>,
}

/// Input for expanding user notes with transcript-derived context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HybridNotesRequest {
    /// Meeting title.
    pub title: String,
    /// Requested structure of the final notes.
    pub template: MeetingTemplate,
    /// Notes written by the user.
    pub rough_notes: String,
    /// Previously summarized transcript windows.
    pub rollups: Vec<String>,
    /// Recent segments not yet included in rollups.
    pub tail_segments: Vec<TranscriptSegment>,
    /// Optional instruction for the output language.
    pub language_instruction: Option<String>,
}

/// Markdown and provenance from a note summarizer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SummaryOutput {
    /// Generated Markdown.
    pub markdown: String,
    /// Identifier of the source that generated the Markdown.
    pub source_id: String,
}

/// Delivers session snapshots to the host's event transport.
pub trait SessionEventSink: Send + Sync {
    /// Publishes a snapshot without taking ownership.
    fn publish_snapshot(&self, snapshot: &SessionBusSnapshot);
}

#[cfg(test)]
mod tests;
