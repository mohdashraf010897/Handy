//! Shared audio and meeting values. PCM buffers hold mono samples at 16 kHz.

use serde::{Deserialize, Serialize};

/// The capture channel, independent of the identity inferred from a span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerChannel {
    /// Local microphone.
    Me,
    /// System audio.
    Others,
}

impl SpeakerChannel {
    /// Returns the snake-case wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Me => "me",
            Self::Others => "others",
        }
    }

    /// Returns the default display label for an unidentified speaker.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Me => "Me",
            Self::Others => "Others",
        }
    }
}

/// A speaker attribution that can refine a generic channel label.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SpeakerIdentity {
    /// Source audio channel, even if attribution identifies the local speaker.
    pub channel: SpeakerChannel,
    /// Stable identifier for this speaker within the diarizer's session.
    pub speaker_id: String,
    /// User-facing label, such as “Others”, “Speaker 1”, or enrolled “Me”.
    pub display_label: String,
    /// Confidence in the attribution, when supplied by the adapter.
    pub confidence: Option<f32>,
}

/// The output route, which can inform echo cancellation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputRouteKind {
    /// Headphones or a headset.
    Headphones,
    /// Built-in or external loudspeakers.
    Speakers,
    /// The output route is not known.
    #[default]
    Unknown,
}

/// Which audio streams are available for the meeting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCaptureMode {
    /// Microphone and system audio.
    DualStream,
    /// Microphone only; no system audio is available.
    #[default]
    MicOnlyFallback,
}

/// The coarse lifecycle state of a meeting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    /// No meeting in progress.
    #[default]
    Idle,
    /// Capturing meeting audio.
    Recording,
    /// Capture is suspended and can resume.
    Paused,
    /// Capture ended; notes are being generated.
    Summarizing,
}

/// Structure requested for expanded meeting notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTemplate {
    /// General-purpose meeting notes.
    #[default]
    General,
    /// A one-on-one meeting.
    OneOnOne,
    /// A team standup.
    Standup,
    /// A language lesson.
    LanguageLesson,
    /// A client call.
    ClientCall,
}

/// A timestamped frame of 16 kHz mono PCM.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PcmFrame16k {
    /// Audio samples, normally finite and normalized to `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    /// Time of the first sample, in milliseconds on the caller's timeline.
    pub timestamp_ms: u64,
}

impl PcmFrame16k {
    /// Returns the root mean square amplitude, or zero for an empty frame.
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }

        let energy: f64 = self
            .samples
            .iter()
            .map(|&sample| f64::from(sample).powi(2))
            .sum();
        (energy / self.samples.len() as f64).sqrt() as f32
    }

    /// Returns the largest absolute amplitude, or zero for an empty frame.
    pub fn peak_abs(&self) -> f32 {
        self.samples
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0, f32::max)
    }

    /// Returns the number of whole milliseconds represented by the samples.
    pub fn duration_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / 16_000
    }
}

/// A batch of time-aligned microphone and system-audio PCM.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DualStreamChunk16k {
    /// Microphone samples.
    pub me_16k: Vec<f32>,
    /// System-audio samples, empty when only the microphone is captured.
    pub others_16k: Vec<f32>,
    /// Detected output route during the chunk.
    pub output_route: OutputRouteKind,
    /// Chunk start in milliseconds on the caller's timeline.
    pub start_ms: u64,
}

/// A segment of speech extracted from one capture channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UtteranceSpan {
    /// Channel from which the span was extracted.
    pub channel: SpeakerChannel,
    /// PCM at 16 kHz.
    pub samples_16k: Vec<f32>,
    /// Start in milliseconds on the caller's timeline.
    pub start_ms: u64,
    /// End in milliseconds on the caller's timeline.
    pub end_ms: u64,
}

/// Transcribed speech with speaker attribution and timing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TranscriptSegment {
    /// Speaker attribution for this segment.
    pub speaker: SpeakerIdentity,
    /// Start in milliseconds on the caller's timeline.
    pub start_ms: u64,
    /// End in milliseconds on the caller's timeline.
    pub end_ms: u64,
    /// Recognized text.
    pub text: String,
    /// Detected language, if available.
    pub language: Option<String>,
}

#[cfg(test)]
mod tests;
