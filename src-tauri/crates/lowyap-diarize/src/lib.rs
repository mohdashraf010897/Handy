//! Pure-Rust speaker diarization, `ME_ONLY` local voiceprint enrollment,
//! acoustic speaker embeddings, and conversational name-cue resolution (`SV-89`).
//!
//! # Example
//! ```
//! use lowyap_diarize::{DiarizerConfig, HybridSpeakerDiarizer};
//! use lowyap_ports::{SpeakerChannel, SpeakerDiarizer, UtteranceSpan};
//!
//! let mut diarizer = HybridSpeakerDiarizer::new(DiarizerConfig::default());
//! let span = UtteranceSpan {
//!     channel: SpeakerChannel::Me,
//!     samples_16k: vec![0.0; 1_600],
//!     start_ms: 0,
//!     end_ms: 100,
//! };
//! let identity = diarizer.identify(&span);
//! assert_eq!(identity.speaker_id, "me");
//! assert_eq!(identity.display_label, "Me");
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod diarizer;
pub mod embedder;
pub mod name_cues;

/// Canonical 16 kHz mono PCM sample rate used across `lowyap` diarization adapters.
pub const SAMPLE_RATE_16K: usize = 16_000;

pub use diarizer::{DiarizerConfig, HybridSpeakerDiarizer};
pub use embedder::{AcousticMelVoiceEmbedder, VoiceEmbedder, VoiceEmbedding};
pub use name_cues::NameCueResolver;

#[cfg(test)]
mod tests;
