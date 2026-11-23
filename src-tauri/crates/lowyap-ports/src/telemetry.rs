//! Engine-agnostic dictation latency telemetry and rolling health summaries.
//!
//! Records wall-clock durations, chunk pre-decoding counts, and stage outcomes
//! without storing transcript text, window titles, or vendor-specific types.

use serde::{Deserialize, Serialize};

/// Which stage produced the final dictated text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinguistStageKind {
    /// Raw ASR output was used because linguistic cleanup was disabled.
    #[default]
    Raw,
    /// Only the deterministic zero-LLM normalizer ran.
    Deterministic,
    /// Both deterministic normalization and a pluggable model rewriter ran.
    ModelPolished,
    /// A model rewriter failed, timed out, or tripped the output guard and
    /// safely fell back to the deterministic stage.
    FallbackDeterministic,
}

impl LinguistStageKind {
    /// Stable snake-case wire value for persistence in SQLite or JSON logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Deterministic => "deterministic",
            Self::ModelPolished => "model_polished",
            Self::FallbackDeterministic => "fallback_deterministic",
        }
    }

    /// Parses a persisted snake-case value, defaulting unknown values to `Raw`.
    pub fn from_wire(value: &str) -> Self {
        match value.trim() {
            "deterministic" => Self::Deterministic,
            "model_polished" => Self::ModelPolished,
            "fallback_deterministic" => Self::FallbackDeterministic,
            _ => Self::Raw,
        }
    }

    /// Returns true when any deterministic or model-backed polish stage ran.
    pub const fn did_polish(self) -> bool {
        !matches!(self, Self::Raw)
    }
}

/// Privacy-safe stage timing and throughput telemetry for a single dictation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DictationTelemetryRecord {
    /// Length of captured audio in seconds.
    pub audio_duration_s: f64,
    /// Milliseconds from shortcut press until microphone capture was ready.
    pub press_to_mic_ready_ms: Option<f64>,
    /// Milliseconds from shortcut release until final ASR work started.
    pub release_to_asr_start_ms: Option<f64>,
    /// Milliseconds spent in ASR after shortcut release.
    pub asr_ms: Option<f64>,
    /// Milliseconds spent in the linguistic polish stage after ASR.
    pub cleanup_ms: Option<f64>,
    /// Milliseconds spent pasting or typing text into the target field.
    pub paste_ms: Option<f64>,
    /// End-to-end user-perceived wait from shortcut release to completion.
    pub release_to_done_ms: Option<f64>,
    /// Effective post-release real-time factor (`audio_duration_s / (asr_ms / 1000)`).
    pub real_time_factor: Option<f64>,
    /// Number of VAD chunks decoded live in the background while still speaking.
    pub chunks_predecoded_live: u32,
    /// Total number of VAD chunks in the dictation (`1` for single-pass takes).
    pub chunks_total: u32,
    /// Opaque identifier of the active speech-to-text adapter/model.
    pub asr_engine_id: Option<String>,
    /// Opaque identifier of the active linguistic rewriter adapter, if any.
    pub linguist_provider_id: Option<String>,
    /// Which linguistic stage produced the final text.
    pub linguist_stage: LinguistStageKind,
}

impl DictationTelemetryRecord {
    /// Computes effective post-release real-time factor from `audio_duration_s` and `asr_ms`.
    pub fn compute_rtf(audio_duration_s: f64, asr_ms: f64) -> Option<f64> {
        if !audio_duration_s.is_finite()
            || !asr_ms.is_finite()
            || audio_duration_s <= 0.0
            || asr_ms <= 0.0
        {
            return None;
        }
        let secs = (asr_ms / 1000.0).max(1e-4);
        Some(audio_duration_s / secs)
    }
}

/// Engine-neutral diagnosis surfaced from observed dictation telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BottleneckKind {
    /// Latency and polish stages are operating within target bounds.
    #[default]
    None,
    /// Long dictations decoded zero chunks live while speaking, causing post-stop wait.
    PostStopBatchDecode,
    /// The currently configured ASR engine has low throughput (`median_rtf < 6x`).
    ComputeBoundAsr,
    /// The configured linguistic rewriter has high tail latency (`p95 > 1200 ms`).
    SlowLinguistProvider,
    /// Recent takes bypassed linguistic polish (`Raw`).
    UnpolishedTakes,
}

/// Aggregated latency and polish health across a window of dictation records.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LatencyHealthSummary {
    /// Number of dictation records included in this summary.
    pub sample_count: usize,
    /// Total spoken audio duration in seconds across all sampled takes.
    pub total_audio_s: f64,
    /// Median (`p50`) wait from key release to paste completion, in milliseconds.
    pub p50_release_to_done_ms: Option<f64>,
    /// 95th-percentile (`p95`) wait from key release to paste completion, in milliseconds.
    pub p95_release_to_done_ms: Option<f64>,
    /// Median (`p50`) post-release ASR time in milliseconds.
    pub p50_asr_ms: Option<f64>,
    /// 95th-percentile (`p95`) linguistic cleanup time in milliseconds.
    pub p95_cleanup_ms: Option<f64>,
    /// Median effective post-release real-time factor (`x real-time`).
    pub median_rtf: Option<f64>,
    /// Fraction of multi-chunk segments decoded live during speech (`0.0..=1.0`).
    pub live_predecoded_ratio: Option<f64>,
    /// Count of takes that ran deterministic or model-backed polish.
    pub polished_count: usize,
    /// Primary engine-neutral bottleneck observed in the sample window.
    pub bottleneck: BottleneckKind,
}

/// Persists or publishes completed dictation telemetry records.
pub trait DictationTelemetrySink: Send + Sync {
    /// Stable sink identifier.
    fn name(&self) -> &'static str;

    /// Records a completed dictation's timing metadata.
    fn record_dictation(&self, record: &DictationTelemetryRecord);
}

fn percentile_sorted(sorted: &[f64], quantile: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let clamped = quantile.clamp(0.0, 1.0);
    let rank = clamped * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        Some(sorted[lower])
    } else {
        let weight = rank - lower as f64;
        Some(sorted[lower] * (1.0 - weight) + sorted[upper] * weight)
    }
}

fn collect_sorted_finite(values: impl Iterator<Item = Option<f64>>) -> Vec<f64> {
    let mut nums: Vec<f64> = values
        .flatten()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .collect();
    nums.sort_by(f64::total_cmp);
    nums
}

/// Computes deterministic latency percentiles and engine-neutral bottleneck signals.
pub fn summarize_telemetry(records: &[DictationTelemetryRecord]) -> LatencyHealthSummary {
    if records.is_empty() {
        return LatencyHealthSummary::default();
    }

    let total_audio_s: f64 = records
        .iter()
        .map(|r| r.audio_duration_s)
        .filter(|v| v.is_finite() && *v > 0.0)
        .sum();

    let done_ms = collect_sorted_finite(records.iter().map(|r| r.release_to_done_ms));
    let asr_ms = collect_sorted_finite(records.iter().map(|r| r.asr_ms));
    let cleanup_ms = collect_sorted_finite(records.iter().map(|r| r.cleanup_ms));
    let rtfs = collect_sorted_finite(records.iter().map(|r| r.real_time_factor));

    let p50_release_to_done_ms = percentile_sorted(&done_ms, 0.50);
    let p95_release_to_done_ms = percentile_sorted(&done_ms, 0.95);
    let p50_asr_ms = percentile_sorted(&asr_ms, 0.50);
    let p95_cleanup_ms = percentile_sorted(&cleanup_ms, 0.95);
    let median_rtf = percentile_sorted(&rtfs, 0.50);

    let (multi_predecoded, multi_total): (u64, u64) = records
        .iter()
        .filter(|r| r.chunks_total > 1)
        .fold((0, 0), |(pre, tot), r| {
            (
                pre + u64::from(r.chunks_predecoded_live.min(r.chunks_total)),
                tot + u64::from(r.chunks_total),
            )
        });
    let live_predecoded_ratio =
        (multi_total > 0).then(|| multi_predecoded as f64 / multi_total as f64);

    let polished_count = records
        .iter()
        .filter(|r| r.linguist_stage.did_polish())
        .count();

    let has_post_stop_batch_stall = records.iter().any(|r| {
        r.audio_duration_s >= 25.0
            && r.chunks_predecoded_live == 0
            && r.asr_ms.unwrap_or(0.0) >= 2_500.0
    });

    let bottleneck = if has_post_stop_batch_stall {
        BottleneckKind::PostStopBatchDecode
    } else if median_rtf.is_some_and(|rtf| rtf < 6.0) {
        BottleneckKind::ComputeBoundAsr
    } else if p95_cleanup_ms.is_some_and(|ms| ms > 3_000.0) {
        BottleneckKind::SlowLinguistProvider
    } else if polished_count == 0 {
        BottleneckKind::UnpolishedTakes
    } else {
        BottleneckKind::None
    };

    LatencyHealthSummary {
        sample_count: records.len(),
        total_audio_s,
        p50_release_to_done_ms,
        p95_release_to_done_ms,
        p50_asr_ms,
        p95_cleanup_ms,
        median_rtf,
        live_predecoded_ratio,
        polished_count,
        bottleneck,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_telemetry_flags_post_stop_batch_stall_on_long_unstreamed_take() {
        let records = vec![
            DictationTelemetryRecord {
                audio_duration_s: 191.62,
                asr_ms: Some(40_125.7),
                paste_ms: Some(233.4),
                release_to_done_ms: Some(40_405.2),
                real_time_factor: DictationTelemetryRecord::compute_rtf(191.62, 40_125.7),
                chunks_predecoded_live: 0,
                chunks_total: 1,
                asr_engine_id: Some("generic-batch-asr".to_string()),
                linguist_provider_id: None,
                linguist_stage: LinguistStageKind::Raw,
                ..DictationTelemetryRecord::default()
            },
            DictationTelemetryRecord {
                audio_duration_s: 15.84,
                asr_ms: Some(3_409.6),
                paste_ms: Some(230.2),
                release_to_done_ms: Some(3_670.8),
                real_time_factor: DictationTelemetryRecord::compute_rtf(15.84, 3_409.6),
                chunks_predecoded_live: 0,
                chunks_total: 1,
                asr_engine_id: Some("generic-batch-asr".to_string()),
                linguist_provider_id: None,
                linguist_stage: LinguistStageKind::Raw,
                ..DictationTelemetryRecord::default()
            },
        ];

        let summary = summarize_telemetry(&records);
        assert_eq!(summary.sample_count, 2);
        assert_eq!(summary.bottleneck, BottleneckKind::PostStopBatchDecode);
        assert_eq!(summary.polished_count, 0);
        assert!(summary.p95_release_to_done_ms.unwrap_or(0.0) > 38_000.0);
    }

    #[test]
    fn summarize_telemetry_reports_clean_health_for_live_predecoded_polished_takes() {
        let records = vec![DictationTelemetryRecord {
            audio_duration_s: 191.6,
            asr_ms: Some(420.0),
            cleanup_ms: Some(290.0),
            paste_ms: Some(180.0),
            release_to_done_ms: Some(890.0),
            real_time_factor: DictationTelemetryRecord::compute_rtf(191.6, 420.0),
            chunks_predecoded_live: 12,
            chunks_total: 13,
            asr_engine_id: Some("any-plugged-asr".to_string()),
            linguist_provider_id: Some("any-plugged-llm".to_string()),
            linguist_stage: LinguistStageKind::ModelPolished,
            ..DictationTelemetryRecord::default()
        }];

        let summary = summarize_telemetry(&records);
        assert_eq!(summary.bottleneck, BottleneckKind::None);
        assert_eq!(summary.polished_count, 1);
        assert!((summary.live_predecoded_ratio.unwrap_or(0.0) - (12.0 / 13.0)).abs() < 1e-6);
    }

    #[test]
    fn summarize_telemetry_allows_normal_2s_cleanup_and_flags_above_3s() {
        let normal = vec![DictationTelemetryRecord {
            audio_duration_s: 40.0,
            asr_ms: Some(900.0),
            cleanup_ms: Some(2_400.0),
            paste_ms: Some(120.0),
            release_to_done_ms: Some(3_420.0),
            real_time_factor: DictationTelemetryRecord::compute_rtf(40.0, 900.0),
            chunks_predecoded_live: 7,
            chunks_total: 8,
            linguist_stage: LinguistStageKind::ModelPolished,
            ..DictationTelemetryRecord::default()
        }];
        assert_eq!(
            summarize_telemetry(&normal).bottleneck,
            BottleneckKind::None
        );

        let slow = vec![DictationTelemetryRecord {
            cleanup_ms: Some(3_400.0),
            ..normal[0].clone()
        }];
        assert_eq!(
            summarize_telemetry(&slow).bottleneck,
            BottleneckKind::SlowLinguistProvider
        );
    }
}
