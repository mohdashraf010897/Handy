//! Read-only local WAV pair inspector powered by `hound`.
//!
//! Loads a pair of `<me.wav>` and `<others.wav>` recordings, normalizes to
//! 16 kHz mono f32 in memory (never mutating files on disk), and compares
//! baseline, modular-target and production DSP metrics.

use std::collections::BTreeMap;
use std::path::Path;

use hound::{SampleFormat, WavReader};
use lowyap_ports::{OutputRouteKind, SpeakerChannel};
use serde::{Deserialize, Serialize};

use crate::reference_adapters::{
    normalized_correlation, peak_abs, rms, SyntheticWhisperAsrSim, FRAME_SAMPLES_16K,
    SAMPLE_RATE_16K,
};
use crate::scenarios::{chunk_dual_stream, STREAM_CHUNK_SAMPLES};
use crate::PipelineUnderTest;

/// Summary statistics for one input WAV file after mono 16 kHz normalization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WavTrackSummary {
    /// Original file path provided by the caller.
    pub path: String,
    /// Source sample rate in Hz.
    pub source_sample_rate: u32,
    /// Source channel count.
    pub source_channels: u16,
    /// Duration in milliseconds after normalization.
    pub duration_ms: u64,
    /// Root-mean-square amplitude in `[0.0, 1.0]`.
    pub rms: f32,
    /// Maximum absolute sample amplitude.
    pub peak_abs: f32,
}

/// Replay metrics for one pipeline profile on a WAV pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WavProfileMetrics {
    /// Profile name (`baseline`, `modular-target` or `production-dsp`).
    pub profile: String,
    /// Echo canceller adapter name.
    pub echo_canceller: String,
    /// Segmenter adapter name.
    pub segmenter: String,
    /// Post-AEC `Me` RMS amplitude.
    pub post_aec_me_rms: f32,
    /// Overall `Me` attenuation in dB (positive means energy was reduced).
    pub me_attenuation_db: f32,
    /// Number of `Me` spans emitted by the segmenter.
    pub emitted_me_spans: usize,
    /// Number of `Others` spans emitted by the segmenter.
    pub emitted_others_spans: usize,
    /// Fraction of total audio dropped as silence in `[0.0, 1.0]`.
    pub dropped_silence_ratio: f32,
    /// Count of known silence hallucinations produced by `SyntheticWhisperAsrSim`.
    pub simulated_hallucination_count: usize,
    /// Speaker label counts emitted by the diarizer.
    pub speaker_turn_counts: BTreeMap<String, usize>,
}

/// Complete read-only inspection report for a `<me.wav, others.wav>` pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WavReplayReport {
    /// Output route assumed during replay.
    pub output_route: OutputRouteKind,
    /// `Me` input track summary.
    pub me_track: WavTrackSummary,
    /// `Others` input track summary.
    pub others_track: WavTrackSummary,
    /// Estimated bulk echo delay in milliseconds (`None` if uncorrelated).
    pub estimated_echo_delay_ms: Option<f32>,
    /// Peak cross-correlation between `Me` and delayed `Others`.
    pub raw_peak_correlation: f32,
    /// Profile metrics in baseline, modular-target and production order.
    pub profiles: Vec<WavProfileMetrics>,
}

/// Loads a WAV file (16/24/32-bit int or 32-bit float), mixes to mono, and
/// resamples linearly to 16 kHz if needed.
pub fn load_wav_mono_16k(path: &Path) -> Result<(Vec<f32>, WavTrackSummary), String> {
    let mut reader = WavReader::open(path)
        .map_err(|e| format!("failed to open WAV '{}': {e}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let interleaved: Vec<f32> = match spec.sample_format {
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|r| {
                r.map(|v| {
                    if v.is_finite() {
                        v.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                format!(
                    "failed reading float samples from '{}': {e}",
                    path.display()
                )
            })?,
        SampleFormat::Int => {
            let bits = spec.bits_per_sample.clamp(1, 32);
            let scale = ((1_i64 << (bits - 1)) as f32).max(1.0);
            reader
                .samples::<i32>()
                .map(|r| r.map(|v| (v as f32 / scale).clamp(-1.0, 1.0)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("failed reading int samples from '{}': {e}", path.display()))?
        }
    };

    let mut mono = Vec::with_capacity(interleaved.len() / channels);
    for frame in interleaved.chunks(channels) {
        let sum: f32 = frame.iter().copied().sum();
        mono.push(sum / frame.len() as f32);
    }

    let samples_16k = if spec.sample_rate == SAMPLE_RATE_16K as u32 {
        mono
    } else {
        resample_linear(&mono, spec.sample_rate, SAMPLE_RATE_16K as u32)
    };

    let summary = WavTrackSummary {
        path: path.display().to_string(),
        source_sample_rate: spec.sample_rate,
        source_channels: spec.channels,
        duration_ms: samples_16k.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64,
        rms: round4(rms(&samples_16k)),
        peak_abs: round4(peak_abs(&samples_16k)),
    };

    Ok((samples_16k, summary))
}

/// Inspects a local `<me.wav, others.wav>` pair across three profiles in read-only mode.
///
/// Attenuation measures the entire microphone track, not ground-truth echo.
pub fn inspect_wav_pair(
    me_path: &Path,
    others_path: &Path,
    output_route: OutputRouteKind,
) -> Result<WavReplayReport, String> {
    let (me_16k, me_track) = load_wav_mono_16k(me_path)?;
    let (others_16k, others_track) = load_wav_mono_16k(others_path)?;

    let (estimated_echo_delay_ms, raw_peak_correlation) =
        estimate_global_echo_delay(&me_16k, &others_16k);

    let chunks = chunk_dual_stream(&me_16k, &others_16k, output_route, STREAM_CHUNK_SAMPLES);
    let profiles = [
        ("baseline", PipelineUnderTest::baseline()),
        ("modular-target", PipelineUnderTest::modular_target()),
        ("production-dsp", PipelineUnderTest::production_dsp()),
    ]
    .into_iter()
    .map(|(name, pipeline)| run_wav_profile(name, &me_16k, others_16k.len(), &chunks, pipeline))
    .collect();

    Ok(WavReplayReport {
        output_route,
        me_track,
        others_track,
        estimated_echo_delay_ms,
        raw_peak_correlation: round4(raw_peak_correlation),
        profiles,
    })
}

/// Formats a `WavReplayReport` as a concise Markdown inspection table.
pub fn format_wav_report_markdown(report: &WavReplayReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## lowyap-eval WAV Pair Inspection (route: `{:?}`)\n\n",
        report.output_route
    ));
    out.push_str(&format!(
        "- **Me**: `{}` ({} ms, {} Hz, {} ch, rms={:.4}, peak={:.4})\n",
        report.me_track.path,
        report.me_track.duration_ms,
        report.me_track.source_sample_rate,
        report.me_track.source_channels,
        report.me_track.rms,
        report.me_track.peak_abs
    ));
    out.push_str(&format!(
        "- **Others**: `{}` ({} ms, {} Hz, {} ch, rms={:.4}, peak={:.4})\n",
        report.others_track.path,
        report.others_track.duration_ms,
        report.others_track.source_sample_rate,
        report.others_track.source_channels,
        report.others_track.rms,
        report.others_track.peak_abs
    ));
    let delay_str = report
        .estimated_echo_delay_ms
        .map_or_else(|| "none detected".to_owned(), |d| format!("{d:.1} ms"));
    out.push_str(&format!(
        "- **Estimated acoustic delay**: {} (peak correlation={:.3})\n\n",
        delay_str, report.raw_peak_correlation
    ));

    out.push_str("| Profile | AEC | Segmenter | Post-AEC Me RMS | Attenuation (dB) | Me / Others Spans | Silence Dropped | Sim Hallucinations |\n");
    out.push_str("|---|---|---|---|---|---|---|---|\n");
    for p in &report.profiles {
        out.push_str(&format!(
            "| {} | `{}` | `{}` | {:.4} | {:.1} dB | {} / {} | {:.1}% | {} |\n",
            p.profile,
            p.echo_canceller,
            p.segmenter,
            p.post_aec_me_rms,
            p.me_attenuation_db,
            p.emitted_me_spans,
            p.emitted_others_spans,
            p.dropped_silence_ratio * 100.0,
            p.simulated_hallucination_count
        ));
    }
    out
}

fn run_wav_profile(
    profile: &str,
    me_16k: &[f32],
    others_len: usize,
    chunks: &[lowyap_ports::DualStreamChunk16k],
    PipelineUnderTest {
        echo_canceller: mut aec,
        mut segmenter,
        mut diarizer,
        asr,
        ..
    }: PipelineUnderTest,
) -> WavProfileMetrics {
    let mut post_me = Vec::with_capacity(me_16k.len());
    let mut spans = Vec::new();
    for chunk in chunks {
        if rms(&chunk.me_16k) >= 0.02 && rms(&chunk.others_16k) < 0.005 {
            diarizer.observe_me_clean(&chunk.me_16k);
        }
        let cleaned = aec.process_16k(&chunk.me_16k, &chunk.others_16k, chunk.output_route);
        post_me.extend_from_slice(&cleaned);
        spans.extend(segmenter.push_chunk(SpeakerChannel::Me, &cleaned, chunk.start_ms));
        spans.extend(segmenter.push_chunk(
            SpeakerChannel::Others,
            &chunk.others_16k,
            chunk.start_ms,
        ));
    }
    spans.extend(segmenter.flush());

    let pre_rms = rms(me_16k);
    // The chunker pads missing microphone blocks when the render lasts longer.
    // Measure both sides over the exact same real microphone samples.
    let measured_len = me_16k.len().min(post_me.len());
    let post_rms = rms(&post_me[..measured_len]);
    let atten_db = if pre_rms > 1e-6 && post_rms > 1e-7 {
        20.0 * (pre_rms / post_rms).log10()
    } else if pre_rms > 1e-6 {
        60.0
    } else {
        0.0
    };

    let mut emitted_me_spans = 0usize;
    let mut emitted_others_spans = 0usize;
    let mut emitted_samples = 0usize;
    let mut hallucinations = 0usize;
    let mut speaker_turn_counts: BTreeMap<String, usize> = BTreeMap::new();

    for span in &spans {
        emitted_samples += span.samples_16k.len();
        match span.channel {
            SpeakerChannel::Me => emitted_me_spans += 1,
            SpeakerChannel::Others => emitted_others_spans += 1,
        }
        if let Ok(res) = asr.transcribe_16k(&span.samples_16k, Some("en")) {
            if SyntheticWhisperAsrSim::is_known_hallucination(&res.text) {
                hallucinations += 1;
            }
        }
        let identity = diarizer.identify(span);
        *speaker_turn_counts
            .entry(identity.display_label)
            .or_insert(0) += 1;
    }

    let total_input_samples = (me_16k.len() + others_len).max(1);
    let dropped_silence_ratio =
        (1.0 - (emitted_samples as f32 / total_input_samples as f32)).clamp(0.0, 1.0);

    WavProfileMetrics {
        profile: profile.to_owned(),
        echo_canceller: aec.name().to_owned(),
        segmenter: segmenter.name().to_owned(),
        post_aec_me_rms: round4(post_rms),
        me_attenuation_db: (atten_db * 10.0).round() / 10.0,
        emitted_me_spans,
        emitted_others_spans,
        dropped_silence_ratio: round4(dropped_silence_ratio),
        simulated_hallucination_count: hallucinations,
        speaker_turn_counts,
    }
}

fn estimate_global_echo_delay(me_16k: &[f32], others_16k: &[f32]) -> (Option<f32>, f32) {
    let n = me_16k.len().min(others_16k.len());
    let max_lag = (180 * SAMPLE_RATE_16K / 1_000).min(n.saturating_sub(FRAME_SAMPLES_16K));
    if n < FRAME_SAMPLES_16K * 4 || max_lag < 16 {
        return (None, 0.0);
    }

    let probe_len = (SAMPLE_RATE_16K * 2).min(n - max_lag);
    let probe_start = max_lag;
    let me_win = &me_16k[probe_start..probe_start + probe_len];
    if rms(me_win) < 0.005 {
        return (None, 0.0);
    }

    let mut best_lag = 0usize;
    let mut best_corr = 0.0_f32;
    for lag in (0..=max_lag).step_by(8) {
        let oth_win = &others_16k[probe_start - lag..probe_start - lag + probe_len];
        let c = normalized_correlation(me_win, oth_win);
        if c > best_corr {
            best_corr = c;
            best_lag = lag;
        }
    }

    let lo = best_lag.saturating_sub(8);
    let hi = (best_lag + 8).min(max_lag);
    for lag in lo..=hi {
        let oth_win = &others_16k[probe_start - lag..probe_start - lag + probe_len];
        let c = normalized_correlation(me_win, oth_win);
        if c > best_corr {
            best_corr = c;
            best_lag = lag;
        }
    }

    if best_corr >= 0.30 {
        let delay_ms = (best_lag as f32 * 1_000.0 / SAMPLE_RATE_16K as f32 * 10.0).round() / 10.0;
        (Some(delay_ms), best_corr)
    } else {
        (None, best_corr)
    }
}

fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if input.is_empty() || src_rate == 0 || dst_rate == 0 {
        return Vec::new();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((input.len() as f64) / ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = (src_pos.floor() as usize).min(input.len() - 1);
        let next = (idx + 1).min(input.len() - 1);
        let frac = (src_pos - idx as f64) as f32;
        out.push(input[idx] * (1.0 - frac) + input[next] * frac);
    }
    out
}

fn round4(v: f32) -> f32 {
    (v * 10_000.0).round() / 10_000.0
}
