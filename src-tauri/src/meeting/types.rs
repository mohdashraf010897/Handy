//! Shared domain types and traits for Wave 3 (`SV-78`) Bot-Free Meeting Notetaker.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Which audio channel produced a meeting transcript segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerChannel {
    /// Local microphone (`Me`).
    Me,
    /// Remote participants captured from macOS system audio (`Others`).
    Others,
}

impl SpeakerChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Me => "me",
            Self::Others => "others",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Me => "Me",
            Self::Others => "Others",
        }
    }
}

/// Active output route detected on the host so speaker bleed can be suppressed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum OutputRouteKind {
    /// Wired headphones, AirPods, Bluetooth headset, or USB headset (`Mic = Me` cleanly).
    Headphones,
    /// Built-in speakers or external speakers (`SpeakerEchoGuard` active on `Me`).
    Speakers,
    #[default]
    Unknown,
}

impl OutputRouteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headphones => "headphones",
            Self::Speakers => "speakers",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether the meeting is capturing both system audio + mic or fell back to mic-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AudioCaptureMode {
    /// Bot-free dual-stream capture (`Me` mic + `Others` macOS `CATapDescription` tap).
    DualStream,
    /// Graceful fallback when System Audio permission is not granted or unsupported OS.
    #[default]
    MicOnlyFallback,
}

impl AudioCaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DualStream => "dual_stream",
            Self::MicOnlyFallback => "mic_only_fallback",
        }
    }
}

/// Lifecycle state of the meeting notetaker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    #[default]
    Idle,
    Recording,
    Paused,
    Summarizing,
}

/// Structured note templates for Granola-style hybrid note expansion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTemplate {
    #[default]
    General,
    OneOnOne,
    Standup,
    LanguageLesson,
    ClientCall,
}

impl MeetingTemplate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::OneOnOne => "one_on_one",
            Self::Standup => "standup",
            Self::LanguageLesson => "language_lesson",
            Self::ClientCall => "client_call",
        }
    }

    pub fn from_db_str(value: &str) -> Self {
        match value {
            "one_on_one" => Self::OneOnOne,
            "standup" => Self::Standup,
            "language_lesson" => Self::LanguageLesson,
            "client_call" => Self::ClientCall,
            _ => Self::General,
        }
    }
}

/// Active call detected on the system (microphone running in another app + call app open).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DetectedCallInfo {
    /// Stable identifier for this active call episode so "Not now" snoozes it cleanly.
    pub call_id: String,
    pub app_bundle_id: String,
    pub app_name: String,
    pub output_route: OutputRouteKind,
    pub detected_at_ms: i64,
}

/// One transcribed utterance or pause marker inside a meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MeetingSegment {
    pub id: i64,
    pub meeting_id: String,
    pub speaker: SpeakerChannel,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub language: Option<String>,
    #[serde(default)]
    pub speaker_id: Option<String>,
    #[serde(default)]
    pub speaker_label: Option<String>,
}

impl MeetingSegment {
    pub fn effective_speaker_label(&self) -> &str {
        self.speaker_label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.speaker.display_label())
    }
}

/// Rolling 5-minute mini-summary persisted to SQLite so 60+ min meetings keep flat RAM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MeetingRollup {
    pub id: i64,
    pub meeting_id: String,
    pub window_index: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub summary_bullets: String,
}

/// Compact row for the searchable Meetings archive list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MeetingSummaryRecord {
    pub id: String,
    pub title: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub duration_ms: u64,
    pub template: MeetingTemplate,
    pub capture_mode: AudioCaptureMode,
    pub output_route: OutputRouteKind,
    pub app_name: Option<String>,
    pub segment_count: u32,
    pub preview_snippet: String,
}

/// Complete persisted meeting record including rough bullets, expanded Markdown notes, and transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MeetingDetail {
    pub summary: MeetingSummaryRecord,
    pub rough_notes: String,
    pub expanded_notes_md: String,
    /// `"llm"`, `"apple_intelligence"`, or `"local_extractive"` so source is always transparent.
    pub summary_source: String,
    pub segments: Vec<MeetingSegment>,
    pub rollups: Vec<MeetingRollup>,
}

/// Real-time snapshot polled/subscribed by the Meetings workspace and Call-Detected pill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MeetingLiveState {
    pub status: MeetingStatus,
    pub active_meeting_id: Option<String>,
    pub title: String,
    pub template: MeetingTemplate,
    pub elapsed_ms: u64,
    pub capture_mode: AudioCaptureMode,
    pub output_route: OutputRouteKind,
    pub app_name: Option<String>,
    pub rough_notes: String,
    pub recent_segments: Vec<MeetingSegment>,
    pub rollup_count: u32,
    pub detected_call: Option<DetectedCallInfo>,
    pub auto_detect_enabled: bool,
}

/// Time-aligned 16 kHz mono PCM frames drained from the active capture backend.
#[derive(Debug, Clone, Default)]
pub struct DualPcmChunk {
    /// Local microphone samples at 16_000 Hz mono (`f32` in `-1.0..=1.0`), already echo-guarded if on speakers.
    pub me_16k: Vec<f32>,
    /// System audio (`Others`) samples at 16_000 Hz mono (`f32` in `-1.0..=1.0`). Empty in `MicOnlyFallback`.
    pub others_16k: Vec<f32>,
    pub output_route: OutputRouteKind,
}

/// Active dual-stream capture handle owned by a running meeting session.
pub trait ActiveDualCapture: Send {
    fn capture_mode(&self) -> AudioCaptureMode;
    fn output_route(&self) -> OutputRouteKind;
    /// Drains newly captured 16 kHz mono samples since the previous call.
    fn drain_16k_frames(&mut self) -> DualPcmChunk;
    /// Cleanly stops hardware taps/streams and releases OS resources.
    fn stop(&mut self);
}

/// Factory trait so `MeetingSessionManager` works identically with real macOS CoreAudio taps and deterministic unit-test mocks.
pub trait DualAudioCaptureBackend: Send + Sync {
    fn start_capture(&self) -> Result<Box<dyn ActiveDualCapture>, String>;
}
