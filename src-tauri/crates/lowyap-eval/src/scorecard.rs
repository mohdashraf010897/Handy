//! Scorecard engine, pipeline harness, and report formatters for `lowyap-eval`.

use std::collections::{HashMap, HashSet};

use lowyap_ports::{
    AsrProvider, EchoCanceller, HybridNotesRequest, MeetingTemplate, NoteSummarizer,
    SpeakerChannel, SpeakerDiarizer, UtteranceSegmenter, UtteranceSpan, WindowSummaryRequest,
};
use serde::{Deserialize, Serialize};

use crate::reference_adapters::{
    rms, ChannelOnlyDiarizer, ChannelPlusEnrollmentDiarizer, CorrelationGateEchoCanceller,
    EnergyVadSegmenter, ExtractiveBudgetSummarizer, FixedWindowSegmenter, FlowGuardedAsrAdapter,
    IdealOracleEchoCanceller, NlmsEchoCanceller, SyntheticWhisperAsrSim,
};
use crate::scenarios::{
    double_talk_overlap, long_multilingual_meeting_rollup, long_silence_hallucination_trap,
    me_only_enrollment_and_two_remote_speakers, open_speaker_echo_bleed,
    run_soft_cancel_undo_invariants,
};

/// Honesty banner emitted on all synthetic plumbing reports so synthetic invariants
/// are never mistaken for empirical speech-recognition quality.
pub const SYNTHETIC_HONESTY_BANNER: &str =
    "[SYNTHETIC PLUMBING ONLY — Real-Audio Quality: UNVERIFIED until --suite real-voice runs on real human WAVs]";

/// Distinguishes synthetic plumbing invariant checks from empirical evaluation
/// on real human audio recordings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationTier {
    /// Deterministic synthetic signal and text plumbing checks only.
    SyntheticPlumbingOnly,
    /// Empirical evaluation run against real human WAV recordings.
    EmpiricalRealAudio,
}

impl EvaluationTier {
    pub(crate) const fn synthetic_default() -> Self {
        Self::SyntheticPlumbingOnly
    }
}

/// Manifest of adapter implementations evaluated in a suite run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AdapterManifest {
    /// Name of the `EchoCanceller` adapter.
    pub echo_canceller: String,
    /// Name of the `UtteranceSegmenter` adapter.
    pub segmenter: String,
    /// Name of the `AsrProvider` adapter.
    pub asr: String,
    /// Name of the `SpeakerDiarizer` adapter.
    pub diarizer: String,
    /// Name of the `NoteSummarizer` adapter.
    pub summarizer: String,
    /// Whether rolling window compaction is enabled for long meetings.
    pub rolling_window_summary: bool,
}

/// Raw quantitative metrics collected across the 6 synthetic benchmark scenarios.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EvalMetrics {
    /// Echo leak rate in `[0.0, 1.0]` (lower is better).
    pub echo_leak_rate: f32,
    /// Near-end double-talk survival rate in `[0.0, 1.0]` (higher is better).
    pub double_talk_survival_rate: f32,
    /// Number of phantom ASR outputs emitted on silent spans (`0` target).
    pub silence_hallucination_count: usize,
    /// Recall of clean local `Me` speaker turns in `[0.0, 1.0]`.
    pub speaker_self_recall: f32,
    /// Cluster purity of distinct remote speakers in `[0.0, 1.0]`.
    pub remote_cluster_purity: f32,
    /// Macro speaker attribution F1 across `Me` and remote speakers in `[0.0, 1.0]`.
    pub speaker_attribution_f1: f32,
    /// Budget + multilingual preservation pass rate in `[0.0, 1.0]`.
    pub summary_budget_pass_rate: f32,
    /// Recall of required multilingual markers surviving summarization in `[0.0, 1.0]`.
    pub multilingual_marker_recall: f32,
    /// Number of summarizer context-budget overflows encountered (`0` target).
    pub summary_overflow_count: usize,
    /// Pass rate of `SessionStateMachine` lifecycle invariants in `[0.0, 1.0]`.
    pub session_bus_invariant_pass_rate: f32,
}

/// Per-dimension normalized scores on a `0.0..=10.0` scale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DimensionScores {
    /// `0.0..=10.0` derived from `1.0 - echo_leak_rate`.
    pub echo_suppression: f32,
    /// `0.0..=10.0` derived from `double_talk_survival_rate`.
    pub double_talk_survival: f32,
    /// `0.0..=10.0` derived from `silence_hallucination_count`.
    pub hallucination_resistance: f32,
    /// `0.0..=10.0` derived from `speaker_attribution_f1`.
    pub speaker_attribution: f32,
    /// `0.0..=10.0` derived from `summary_budget_pass_rate`.
    pub summary_budget: f32,
    /// `0.0..=10.0` derived from `session_bus_invariant_pass_rate`.
    pub session_bus_invariants: f32,
}

impl DimensionScores {
    /// Computes `0.0..=10.0` dimension scores from raw metrics.
    pub fn from_metrics(m: &EvalMetrics) -> Self {
        Self {
            echo_suppression: round2((1.0 - m.echo_leak_rate).clamp(0.0, 1.0) * 10.0),
            double_talk_survival: round2(m.double_talk_survival_rate.clamp(0.0, 1.0) * 10.0),
            hallucination_resistance: round2(10.0 / (1.0 + m.silence_hallucination_count as f32)),
            speaker_attribution: round2(m.speaker_attribution_f1.clamp(0.0, 1.0) * 10.0),
            summary_budget: round2(m.summary_budget_pass_rate.clamp(0.0, 1.0) * 10.0),
            session_bus_invariants: round2(
                m.session_bus_invariant_pass_rate.clamp(0.0, 1.0) * 10.0,
            ),
        }
    }

    /// Weighted overall score in `0.0..=10.0`.
    pub fn weighted_overall(&self) -> f32 {
        let raw = self.echo_suppression * 0.25
            + self.double_talk_survival * 0.20
            + self.hallucination_resistance * 0.20
            + self.speaker_attribution * 0.15
            + self.summary_budget * 0.10
            + self.session_bus_invariants * 0.10;
        round2(raw.clamp(0.0, 10.0))
    }
}

/// Summary telemetry for an individual benchmark scenario.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ScenarioReport {
    /// Stable scenario identifier.
    pub id: String,
    /// Score in `0.0..=10.0` for this scenario's primary dimension.
    pub score: f32,
    /// Human-readable metric summary.
    pub summary: String,
}

/// Full structured evaluation scorecard emitted by `PipelineUnderTest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EvalScorecard {
    /// Evaluation tier distinguishing synthetic plumbing checks from real-audio runs.
    #[serde(default = "EvaluationTier::synthetic_default")]
    pub evaluation_tier: EvaluationTier,
    /// Suite profile label (`baseline`, `modular-target`, `ideal-oracle`, etc.).
    pub suite_name: String,
    /// Adapters wired into this run.
    pub adapters: AdapterManifest,
    /// Raw quantitative metrics.
    pub metrics: EvalMetrics,
    /// Normalized `0.0..=10.0` dimension scores.
    pub dimension_scores: DimensionScores,
    /// Weighted overall score in `0.0..=10.0`.
    pub overall_score: f32,
    /// Optional pass/fail gate threshold.
    pub min_score_threshold: Option<f32>,
    /// `true` when `overall_score >= min_score_threshold` (or no threshold set).
    pub passed: bool,
    /// Per-scenario details.
    pub scenario_details: Vec<ScenarioReport>,
}

impl EvalScorecard {
    /// Counts `(passed, total)` deterministic plumbing invariants for this scorecard.
    pub fn plumbing_invariant_counts(&self) -> (usize, usize) {
        let checks = [
            self.metrics.echo_leak_rate <= 0.15,
            self.metrics.double_talk_survival_rate >= 0.85,
            self.metrics.silence_hallucination_count == 0,
            self.metrics.speaker_attribution_f1 >= 0.90,
            self.metrics.summary_overflow_count == 0
                && self.metrics.summary_budget_pass_rate >= 0.95,
            self.metrics.session_bus_invariant_pass_rate >= 1.0,
        ];
        let passed = checks.into_iter().filter(|&ok| ok).count();
        (passed, checks.len())
    }

    /// Returns `true` when all deterministic plumbing invariants pass and any
    /// configured threshold gate also passed.
    pub fn plumbing_gate_passed(&self) -> bool {
        let (passed, total) = self.plumbing_invariant_counts();
        self.passed && passed == total
    }
}

/// Side-by-side comparison payload for `--suite compare` / `--suite all`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ComparisonReport {
    /// Evaluation tier distinguishing synthetic plumbing checks from real-audio runs.
    #[serde(default = "EvaluationTier::synthetic_default")]
    pub evaluation_tier: EvaluationTier,
    /// Ordered scorecards included in the comparison.
    pub scorecards: Vec<EvalScorecard>,
    /// Overall pass/fail verdict against `--min-score` (if specified).
    pub passed: bool,
}

/// Trait-object pipeline runner that executes all 6 synthetic benchmark
/// scenarios against any implementation of `lowyap_ports` traits.
pub struct PipelineUnderTest {
    /// Echo cancellation adapter under evaluation.
    pub echo_canceller: Box<dyn EchoCanceller>,
    /// Utterance segmentation adapter under evaluation.
    pub segmenter: Box<dyn UtteranceSegmenter>,
    /// ASR provider adapter under evaluation.
    pub asr: Box<dyn AsrProvider>,
    /// Speaker diarizer adapter under evaluation.
    pub diarizer: Box<dyn SpeakerDiarizer>,
    /// Note summarizer adapter under evaluation.
    pub summarizer: Box<dyn NoteSummarizer>,
    /// Whether the harness invokes rolling-window batching before hybrid note expansion.
    pub use_rolling_summary: bool,
}

impl PipelineUnderTest {
    /// Wave 3 baseline profile: correlation-gate AEC, fixed 10 s slicing,
    /// channel-only diarization, and single-shot note expansion.
    pub fn baseline() -> Self {
        Self {
            echo_canceller: Box::new(CorrelationGateEchoCanceller::new()),
            segmenter: Box::new(FixedWindowSegmenter::default()),
            asr: Box::new(SyntheticWhisperAsrSim),
            diarizer: Box::new(ChannelOnlyDiarizer),
            summarizer: Box::new(ExtractiveBudgetSummarizer::default()),
            use_rolling_summary: false,
        }
    }

    /// Modular Wave 4 target profile: blind delay-compensated NLMS AEC, energy
    /// plus hangover VAD segmentation, `ME_ONLY` enrollment with remote clustering
    /// diarizer, and rolling-window budgeted summarization.
    pub fn modular_target() -> Self {
        Self {
            echo_canceller: Box::new(NlmsEchoCanceller::new()),
            segmenter: Box::new(EnergyVadSegmenter::default()),
            asr: Box::new(SyntheticWhisperAsrSim),
            diarizer: Box::new(ChannelPlusEnrollmentDiarizer::new()),
            summarizer: Box::new(ExtractiveBudgetSummarizer::default()),
            use_rolling_summary: true,
        }
    }

    /// Wave 4B production echo cancellation and speech segmentation.
    ///
    /// ASR, diarization and summarization retain the modular reference adapters.
    pub fn production_dsp() -> Self {
        Self {
            echo_canceller: Box::new(lowyap_dsp::WebRtcAec3EchoCanceller::new()),
            segmenter: Box::new(lowyap_dsp::VadUtteranceSegmenter::default()),
            ..Self::modular_target()
        }
    }

    /// Full Wave 4C production profile wiring all four production crates:
    /// `lowyap-dsp` (WebRTC AEC3 + Earshot-assisted VAD),
    /// `lowyap-flow` (transcript & silence-phantom guard),
    /// `lowyap-diarize` (hybrid `ME_ONLY` voiceprint + remote clustering), and
    /// `lowyap-summarize` (hierarchical map-reduce summarizer).
    pub fn production() -> Self {
        Self {
            echo_canceller: Box::new(lowyap_dsp::WebRtcAec3EchoCanceller::new()),
            segmenter: Box::new(lowyap_dsp::VadUtteranceSegmenter::default()),
            asr: Box::new(FlowGuardedAsrAdapter::new(Box::new(
                SyntheticWhisperAsrSim::new(),
            ))),
            diarizer: Box::new(lowyap_diarize::HybridSpeakerDiarizer::default()),
            summarizer: Box::new(lowyap_summarize::HybridMapReduceSummarizer::default()),
            use_rolling_summary: true,
        }
    }

    /// Upper-bound reference profile using `IdealOracleEchoCanceller` alongside
    /// the modular VAD, diarizer, and rolling-window summarizer.
    pub fn ideal_oracle() -> Self {
        Self {
            echo_canceller: Box::new(IdealOracleEchoCanceller::default()),
            segmenter: Box::new(EnergyVadSegmenter::default()),
            asr: Box::new(SyntheticWhisperAsrSim),
            diarizer: Box::new(ChannelPlusEnrollmentDiarizer::new()),
            summarizer: Box::new(ExtractiveBudgetSummarizer::default()),
            use_rolling_summary: true,
        }
    }

    /// Runs all 6 benchmark scenarios and returns a complete `EvalScorecard`.
    pub fn run_suite(&mut self, suite_name: &str, min_score: Option<f32>) -> EvalScorecard {
        let (echo_leak_rate, s1_detail) = self.eval_echo_bleed();
        let (double_talk_survival_rate, s2_detail) = self.eval_double_talk();
        let (silence_hallucination_count, s3_detail) = self.eval_silence_hallucinations();
        let (speaker_self_recall, remote_cluster_purity, speaker_attribution_f1, s4_detail) =
            self.eval_diarization();
        let (
            summary_budget_pass_rate,
            multilingual_marker_recall,
            summary_overflow_count,
            s5_detail,
        ) = self.eval_multilingual_rollup();
        let (session_bus_invariant_pass_rate, s6_detail) = self.eval_session_bus();

        let metrics = EvalMetrics {
            echo_leak_rate: round4(echo_leak_rate),
            double_talk_survival_rate: round4(double_talk_survival_rate),
            silence_hallucination_count,
            speaker_self_recall: round4(speaker_self_recall),
            remote_cluster_purity: round4(remote_cluster_purity),
            speaker_attribution_f1: round4(speaker_attribution_f1),
            summary_budget_pass_rate: round4(summary_budget_pass_rate),
            multilingual_marker_recall: round4(multilingual_marker_recall),
            summary_overflow_count,
            session_bus_invariant_pass_rate: round4(session_bus_invariant_pass_rate),
        };

        let dimension_scores = DimensionScores::from_metrics(&metrics);
        let overall_score = dimension_scores.weighted_overall();
        let passed = min_score.is_none_or(|min| overall_score >= min);

        let scenario_details = vec![
            ScenarioReport {
                id: "open_speaker_echo_bleed".to_owned(),
                score: dimension_scores.echo_suppression,
                summary: s1_detail,
            },
            ScenarioReport {
                id: "double_talk_overlap".to_owned(),
                score: dimension_scores.double_talk_survival,
                summary: s2_detail,
            },
            ScenarioReport {
                id: "long_silence_hallucination_trap".to_owned(),
                score: dimension_scores.hallucination_resistance,
                summary: s3_detail,
            },
            ScenarioReport {
                id: "me_only_enrollment_and_two_remote_speakers".to_owned(),
                score: dimension_scores.speaker_attribution,
                summary: s4_detail,
            },
            ScenarioReport {
                id: "long_multilingual_meeting_rollup".to_owned(),
                score: dimension_scores.summary_budget,
                summary: s5_detail,
            },
            ScenarioReport {
                id: "soft_cancel_undo_state_invariants".to_owned(),
                score: dimension_scores.session_bus_invariants,
                summary: s6_detail,
            },
        ];

        EvalScorecard {
            evaluation_tier: EvaluationTier::SyntheticPlumbingOnly,
            suite_name: suite_name.to_owned(),
            adapters: AdapterManifest {
                echo_canceller: self.echo_canceller.name().to_owned(),
                segmenter: self.segmenter.name().to_owned(),
                asr: self.asr.name().to_owned(),
                diarizer: self.diarizer.name().to_owned(),
                summarizer: self.summarizer.name().to_owned(),
                rolling_window_summary: self.use_rolling_summary,
            },
            metrics,
            dimension_scores,
            overall_score,
            min_score_threshold: min_score,
            passed,
            scenario_details,
        }
    }

    fn eval_echo_bleed(&mut self) -> (f32, String) {
        self.echo_canceller.reset();
        let _ = self.segmenter.flush();

        let scenario = open_speaker_echo_bleed();
        let mut post_aec_me = Vec::with_capacity(scenario.me_bleed_16k.len());
        let mut spans = Vec::new();

        for chunk in &scenario.chunks {
            let cleaned = self.echo_canceller.process_16k(
                &chunk.me_16k,
                &chunk.others_16k,
                chunk.output_route,
            );
            post_aec_me.extend_from_slice(&cleaned);
            spans.extend(
                self.segmenter
                    .push_chunk(SpeakerChannel::Me, &cleaned, chunk.start_ms),
            );
            spans.extend(self.segmenter.push_chunk(
                SpeakerChannel::Others,
                &chunk.others_16k,
                chunk.start_ms,
            ));
        }
        spans.extend(self.segmenter.flush());

        let mut leaked_me_ms = 0_u64;
        let mut leaked_me_spans = 0_usize;
        for span in spans.iter().filter(|s| s.channel == SpeakerChannel::Me) {
            if let Ok(res) = self.asr.transcribe_16k(&span.samples_16k, Some("en")) {
                if !res.text.trim().is_empty() {
                    leaked_me_spans += 1;
                    leaked_me_ms =
                        leaked_me_ms.saturating_add(span.end_ms.saturating_sub(span.start_ms));
                }
            }
        }

        let pre_rms = rms(&scenario.me_bleed_16k);
        let post_rms = rms(&post_aec_me);
        let acoustic_leak = if pre_rms > 1e-6 {
            (post_rms / pre_rms).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let span_leak = if scenario.remote_speech_ms > 0 {
            (leaked_me_ms as f32 / scenario.remote_speech_ms as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let echo_leak_rate = (0.5 * acoustic_leak + 0.5 * span_leak).clamp(0.0, 1.0);

        let detail = format!(
            "acoustic_leak={:.1}%, leaked_me_spans={}, leaked_me_ms={}/{}ms, echo_leak_rate={:.3}",
            acoustic_leak * 100.0,
            leaked_me_spans,
            leaked_me_ms,
            scenario.remote_speech_ms,
            echo_leak_rate
        );
        (echo_leak_rate, detail)
    }

    fn eval_double_talk(&mut self) -> (f32, String) {
        self.echo_canceller.reset();
        let _ = self.segmenter.flush();

        let scenario = double_talk_overlap();
        let mut post_aec_me = Vec::with_capacity(scenario.clean_me_16k.len());
        let mut spans = Vec::new();

        for chunk in &scenario.chunks {
            let cleaned = self.echo_canceller.process_16k(
                &chunk.me_16k,
                &chunk.others_16k,
                chunk.output_route,
            );
            post_aec_me.extend_from_slice(&cleaned);
            spans.extend(
                self.segmenter
                    .push_chunk(SpeakerChannel::Me, &cleaned, chunk.start_ms),
            );
            spans.extend(self.segmenter.push_chunk(
                SpeakerChannel::Others,
                &chunk.others_16k,
                chunk.start_ms,
            ));
        }
        spans.extend(self.segmenter.flush());

        let (start_idx, end_idx) = scenario.overlap_sample_range;
        let clean = &scenario.clean_me_16k[start_idx..end_idx];
        let out = &post_aec_me[start_idx..end_idx.min(post_aec_me.len())];

        let mut dot = 0.0_f64;
        let mut clean_sq = 0.0_f64;
        for (&o, &c) in out.iter().zip(clean.iter()) {
            dot += f64::from(o) * f64::from(c);
            clean_sq += f64::from(c) * f64::from(c);
        }
        let projection_survival = if clean_sq > 1e-9 {
            (dot / clean_sq).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let (ov_start, ov_end) = scenario.overlap_ms_range;
        let ov_total = ov_end.saturating_sub(ov_start).max(1);
        let mut covered_ms = 0_u64;
        for span in spans.iter().filter(|s| s.channel == SpeakerChannel::Me) {
            let overlap = interval_overlap(span.start_ms, span.end_ms, ov_start, ov_end);
            if overlap > 0 {
                if let Ok(res) = self.asr.transcribe_16k(&span.samples_16k, Some("en")) {
                    if !res.text.trim().is_empty()
                        && !SyntheticWhisperAsrSim::is_known_hallucination(&res.text)
                    {
                        covered_ms = covered_ms.saturating_add(overlap);
                    }
                }
            }
        }
        let asr_coverage = (covered_ms as f32 / ov_total as f32).clamp(0.0, 1.0);
        let survival_rate = (0.70 * projection_survival + 0.30 * asr_coverage).clamp(0.0, 1.0);

        let detail = format!(
            "near_end_projection={:.1}%, asr_overlap_coverage={:.1}%, survival={:.3}",
            projection_survival * 100.0,
            asr_coverage * 100.0,
            survival_rate
        );
        (survival_rate, detail)
    }

    fn eval_silence_hallucinations(&mut self) -> (usize, String) {
        self.echo_canceller.reset();
        let _ = self.segmenter.flush();

        let scenario = long_silence_hallucination_trap();
        let mut spans = Vec::new();
        for chunk in &scenario.chunks {
            let cleaned = self.echo_canceller.process_16k(
                &chunk.me_16k,
                &chunk.others_16k,
                chunk.output_route,
            );
            spans.extend(
                self.segmenter
                    .push_chunk(SpeakerChannel::Me, &cleaned, chunk.start_ms),
            );
            spans.extend(self.segmenter.push_chunk(
                SpeakerChannel::Others,
                &chunk.others_16k,
                chunk.start_ms,
            ));
        }
        spans.extend(self.segmenter.flush());

        let (speech_start, speech_end) = scenario.speech_window_ms;
        let mut hallucinations = 0usize;
        let mut real_utterance_detected = false;

        for span in &spans {
            let span_dur = span.end_ms.saturating_sub(span.start_ms).max(1);
            let overlap = if span.channel == SpeakerChannel::Me {
                interval_overlap(span.start_ms, span.end_ms, speech_start, speech_end)
            } else {
                0
            };
            let is_silent_span = (overlap as f32 / span_dur as f32) < 0.10;

            if let Ok(res) = self.asr.transcribe_16k(&span.samples_16k, Some("en")) {
                if !res.text.trim().is_empty() {
                    if is_silent_span {
                        hallucinations += 1;
                    } else if !SyntheticWhisperAsrSim::is_known_hallucination(&res.text) {
                        real_utterance_detected = true;
                    }
                }
            }
        }

        let detail = format!(
            "spans_emitted={}, silence_hallucinations={}, speech_preserved={}",
            spans.len(),
            hallucinations,
            real_utterance_detected
        );
        (hallucinations, detail)
    }

    fn eval_diarization(&mut self) -> (f32, f32, f32, String) {
        self.echo_canceller.reset();
        let _ = self.segmenter.flush();
        self.diarizer.reset();

        let scenario = me_only_enrollment_and_two_remote_speakers();
        let mut spans = Vec::new();
        for chunk in &scenario.chunks {
            // Feed clean ME_ONLY windows for local-speaker enrollment.
            if rms(&chunk.me_16k) >= 0.02 && rms(&chunk.others_16k) < 0.005 {
                self.diarizer.observe_me_clean(&chunk.me_16k);
            }
            let cleaned = self.echo_canceller.process_16k(
                &chunk.me_16k,
                &chunk.others_16k,
                chunk.output_route,
            );
            spans.extend(
                self.segmenter
                    .push_chunk(SpeakerChannel::Me, &cleaned, chunk.start_ms),
            );
            spans.extend(self.segmenter.push_chunk(
                SpeakerChannel::Others,
                &chunk.others_16k,
                chunk.start_ms,
            ));
        }
        spans.extend(self.segmenter.flush());

        let attributed: Vec<(&UtteranceSpan, String)> = spans
            .iter()
            .filter(|s| rms(&s.samples_16k) >= 0.01)
            .map(|span| {
                let id = self.diarizer.identify(span);
                (span, id.speaker_id)
            })
            .collect();

        // Assign each ground-truth turn to the maximally overlapping attributed span on its channel.
        let mut turn_predictions: Vec<(&'static str, String)> = Vec::new();
        let mut me_total = 0usize;
        let mut me_correct = 0usize;
        let mut remote_cluster_votes: HashMap<String, HashMap<&'static str, usize>> =
            HashMap::new();
        let mut remote_total = 0usize;

        for turn in &scenario.turns {
            let best_pred = attributed
                .iter()
                .filter(|(span, _)| span.channel == turn.channel)
                .map(|(span, spk_id)| {
                    (
                        interval_overlap(span.start_ms, span.end_ms, turn.start_ms, turn.end_ms),
                        spk_id.clone(),
                    )
                })
                .filter(|(ov, _)| *ov > 0)
                .max_by_key(|(ov, _)| *ov)
                .map(|(_, spk_id)| spk_id)
                .unwrap_or_else(|| "<missed>".to_owned());

            if turn.role == "me" {
                me_total += 1;
                if best_pred == "me" {
                    me_correct += 1;
                }
            } else {
                remote_total += 1;
                *remote_cluster_votes
                    .entry(best_pred.clone())
                    .or_default()
                    .entry(turn.role)
                    .or_insert(0) += 1;
            }
            turn_predictions.push((turn.role, best_pred));
        }

        let speaker_self_recall = if me_total > 0 {
            me_correct as f32 / me_total as f32
        } else {
            0.0
        };

        let pure_remote_hits: usize = remote_cluster_votes
            .values()
            .map(|role_counts| role_counts.values().copied().max().unwrap_or(0))
            .sum();
        let remote_cluster_purity = if remote_total > 0 {
            pure_remote_hits as f32 / remote_total as f32
        } else {
            0.0
        };

        // Optimal 1-to-1 assignment from predicted remote speaker_ids to {remote_a, remote_b}.
        let mut remote_ids: Vec<String> = remote_cluster_votes
            .keys()
            .filter(|k| k.as_str() != "<missed>")
            .cloned()
            .collect();
        remote_ids.sort();

        let (best_a_id, best_b_id) = best_two_role_assignment(&remote_ids, &remote_cluster_votes);

        let mut role_to_pred: HashMap<&'static str, Option<String>> = HashMap::new();
        role_to_pred.insert("me", Some("me".to_owned()));
        role_to_pred.insert("remote_a", best_a_id);
        role_to_pred.insert("remote_b", best_b_id);

        let mut f1_sum = 0.0_f32;
        for role in ["me", "remote_a", "remote_b"] {
            let mapped = role_to_pred.get(role).and_then(|v| v.as_deref());
            let mut tp = 0usize;
            let mut fp = 0usize;
            let mut fn_ = 0usize;
            for (gt_role, pred) in &turn_predictions {
                let is_gt = *gt_role == role;
                let is_pred = mapped.is_some_and(|m| m == pred.as_str());
                match (is_gt, is_pred) {
                    (true, true) => tp += 1,
                    (false, true) => fp += 1,
                    (true, false) => fn_ += 1,
                    (false, false) => {}
                }
            }
            let denom = 2 * tp + fp + fn_;
            if denom > 0 {
                f1_sum += (2 * tp) as f32 / denom as f32;
            }
        }
        let speaker_attribution_f1 = (f1_sum / 3.0).clamp(0.0, 1.0);

        let distinct_remote: HashSet<&str> = remote_ids.iter().map(String::as_str).collect();
        let detail = format!(
            "self_recall={:.2}, remote_purity={:.2}, remote_clusters={}, f1={:.3}",
            speaker_self_recall,
            remote_cluster_purity,
            distinct_remote.len(),
            speaker_attribution_f1
        );
        (
            speaker_self_recall,
            remote_cluster_purity,
            speaker_attribution_f1,
            detail,
        )
    }

    fn eval_multilingual_rollup(&mut self) -> (f32, f32, usize, String) {
        let scenario = long_multilingual_meeting_rollup();
        let max_chars = self.summarizer.max_input_chars();

        if !self.use_rolling_summary {
            // Baseline single-shot prompt over all 65 minutes -> overflows budget.
            let req = HybridNotesRequest {
                title: scenario.title,
                template: MeetingTemplate::General,
                rough_notes: scenario.rough_notes,
                rollups: Vec::new(),
                tail_segments: scenario.segments,
                language_instruction: Some(scenario.language_instruction),
            };
            let input_chars = ExtractiveBudgetSummarizer::hybrid_input_chars(&req);
            return match self.summarizer.expand_hybrid_notes(&req) {
                Ok(out) => {
                    let preserved = scenario
                        .required_markers
                        .iter()
                        .filter(|&&m| out.markdown.contains(m))
                        .count();
                    let rate = preserved as f32 / scenario.required_markers.len() as f32;
                    (
                        rate,
                        rate,
                        0,
                        format!(
                            "single_shot_chars={input_chars}/{max_chars}, overflows=0, markers={preserved}/{}",
                            scenario.required_markers.len()
                        ),
                    )
                }
                Err(err) => (
                    0.0,
                    0.0,
                    1,
                    format!("single_shot_chars={input_chars}/{max_chars}, overflows=1 ({err})"),
                ),
            };
        }

        // Rolling-window + tail summarization staying strictly <= max_input_chars.
        let tail_len = 15usize.min(scenario.segments.len());
        let split_at = scenario.segments.len() - tail_len;
        let earlier = &scenario.segments[..split_at];
        let tail = scenario.segments[split_at..].to_vec();

        let window_budget = max_chars.saturating_sub(1_024).max(512);
        let mut rollups = Vec::new();
        let mut current_window = Vec::new();
        let mut current_chars = scenario.language_instruction.chars().count();
        let mut window_index = 0_u32;
        let mut overflows = 0usize;

        for seg in earlier {
            let seg_chars =
                seg.speaker.display_label.chars().count() + seg.text.chars().count() + 4;
            if !current_window.is_empty() && current_chars + seg_chars > window_budget {
                let win_req = WindowSummaryRequest {
                    window_index,
                    segments: std::mem::take(&mut current_window),
                    language_instruction: Some(scenario.language_instruction.clone()),
                };
                match self.summarizer.summarize_window(&win_req) {
                    Ok(out) => rollups.push(out.markdown),
                    Err(_) => overflows += 1,
                }
                window_index += 1;
                current_chars = scenario.language_instruction.chars().count();
            }
            current_chars += seg_chars;
            current_window.push(seg.clone());
        }
        if !current_window.is_empty() {
            let win_req = WindowSummaryRequest {
                window_index,
                segments: current_window,
                language_instruction: Some(scenario.language_instruction.clone()),
            };
            match self.summarizer.summarize_window(&win_req) {
                Ok(out) => rollups.push(out.markdown),
                Err(_) => overflows += 1,
            }
        }

        // Ensure combined rollups + tail stay within max_input_chars.
        let mut hybrid_req = HybridNotesRequest {
            title: scenario.title,
            template: MeetingTemplate::General,
            rough_notes: scenario.rough_notes,
            rollups,
            tail_segments: tail,
            language_instruction: Some(scenario.language_instruction),
        };
        while ExtractiveBudgetSummarizer::hybrid_input_chars(&hybrid_req) > max_chars
            && hybrid_req.rollups.len() > 1
        {
            hybrid_req.rollups.remove(0);
        }

        let final_chars = ExtractiveBudgetSummarizer::hybrid_input_chars(&hybrid_req);
        let windows_count = hybrid_req.rollups.len();
        match self.summarizer.expand_hybrid_notes(&hybrid_req) {
            Ok(out) => {
                let preserved = scenario
                    .required_markers
                    .iter()
                    .filter(|&&m| out.markdown.contains(m))
                    .count();
                let marker_ratio = preserved as f32 / scenario.required_markers.len() as f32;
                let pass_rate = if overflows == 0 {
                    marker_ratio
                } else {
                    (marker_ratio * 0.5).clamp(0.0, 1.0)
                };
                (
                    pass_rate,
                    marker_ratio,
                    overflows,
                    format!(
                        "windows={windows_count}, final_chars={final_chars}/{max_chars}, overflows={overflows}, markers={preserved}/{}",
                        scenario.required_markers.len()
                    ),
                )
            }
            Err(err) => (
                0.0,
                0.0,
                overflows + 1,
                format!(
                    "windows={windows_count}, final_chars={final_chars}/{max_chars}, overflows={} ({err})",
                    overflows + 1
                ),
            ),
        }
    }

    fn eval_session_bus(&self) -> (f32, String) {
        let res = run_soft_cancel_undo_invariants();
        let rate = if res.total_checks > 0 {
            res.passed_checks as f32 / res.total_checks as f32
        } else {
            0.0
        };
        let detail = if res.failures.is_empty() {
            format!(
                "invariants_passed={}/{}",
                res.passed_checks, res.total_checks
            )
        } else {
            format!(
                "invariants_passed={}/{}, failed=[{}]",
                res.passed_checks,
                res.total_checks,
                res.failures.join(", ")
            )
        };
        (rate, detail)
    }
}

/// Evaluates the side-by-side comparison of `baseline` vs full `production`
/// (all four Wave 4C production crates), enforcing `min_score` against `production`.
pub fn evaluate_comparison(min_score: Option<f32>) -> ComparisonReport {
    let baseline = PipelineUnderTest::baseline().run_suite("baseline", None);
    let production = PipelineUnderTest::production().run_suite("production", min_score);
    let passed = production.passed;
    ComparisonReport {
        evaluation_tier: EvaluationTier::SyntheticPlumbingOnly,
        scorecards: vec![baseline, production],
        passed,
    }
}

/// Evaluates all four suites (`baseline`, `production-dsp`, `production`,
/// `modular-target`) in order, enforcing `min_score` against `production`.
pub fn evaluate_all(min_score: Option<f32>) -> ComparisonReport {
    let baseline = PipelineUnderTest::baseline().run_suite("baseline", None);
    let production_dsp = PipelineUnderTest::production_dsp().run_suite("production-dsp", None);
    let production = PipelineUnderTest::production().run_suite("production", min_score);
    let modular_target = PipelineUnderTest::modular_target().run_suite("modular-target", None);
    let passed = production.passed;
    ComparisonReport {
        evaluation_tier: EvaluationTier::SyntheticPlumbingOnly,
        scorecards: vec![baseline, production_dsp, production, modular_target],
        passed,
    }
}

fn gate_word(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}

/// Renders a single `EvalScorecard` as a concise Markdown table.
///
/// Synthetic suites intentionally omit `/10` quality headlines and instead
/// report deterministic plumbing gate verdicts plus an explicit honesty banner.
pub fn format_scorecard_markdown(card: &EvalScorecard) -> String {
    let (inv_passed, inv_total) = card.plumbing_invariant_counts();
    let gate_ok = card.plumbing_gate_passed();
    let mut out = String::new();
    out.push_str(&format!(
        "## lowyap-eval Scorecard: `{}` — PLUMBING GATE: {} ({}/{} deterministic invariants)\n\n",
        card.suite_name,
        gate_word(gate_ok),
        inv_passed,
        inv_total
    ));
    if card.evaluation_tier == EvaluationTier::SyntheticPlumbingOnly {
        out.push_str(SYNTHETIC_HONESTY_BANNER);
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "Adapters: AEC=`{}`, VAD=`{}`, ASR=`{}`, Diarizer=`{}`, Summarizer=`{}` (rolling=`{}`)\n\n",
        card.adapters.echo_canceller,
        card.adapters.segmenter,
        card.adapters.asr,
        card.adapters.diarizer,
        card.adapters.summarizer,
        card.adapters.rolling_window_summary,
    ));
    out.push_str("| Plumbing Invariant | Raw Metric | Gate |\n");
    out.push_str("|---|---|---|\n");
    out.push_str(&format!(
        "| Echo Suppression | leak_rate={:.3} (<= 0.150) | {} |\n",
        card.metrics.echo_leak_rate,
        gate_word(card.metrics.echo_leak_rate <= 0.15)
    ));
    out.push_str(&format!(
        "| Double-Talk Survival | survival={:.3} (>= 0.850) | {} |\n",
        card.metrics.double_talk_survival_rate,
        gate_word(card.metrics.double_talk_survival_rate >= 0.85)
    ));
    out.push_str(&format!(
        "| Hallucination Resistance | hallucinations={} (== 0) | {} |\n",
        card.metrics.silence_hallucination_count,
        gate_word(card.metrics.silence_hallucination_count == 0)
    ));
    out.push_str(&format!(
        "| Speaker Attribution | f1={:.3} (self={:.2}, purity={:.2}, >= 0.900) | {} |\n",
        card.metrics.speaker_attribution_f1,
        card.metrics.speaker_self_recall,
        card.metrics.remote_cluster_purity,
        gate_word(card.metrics.speaker_attribution_f1 >= 0.90)
    ));
    out.push_str(&format!(
        "| Summary Budget & Multilingual | pass={:.2}, markers={:.2}, overflows={} | {} |\n",
        card.metrics.summary_budget_pass_rate,
        card.metrics.multilingual_marker_recall,
        card.metrics.summary_overflow_count,
        gate_word(
            card.metrics.summary_overflow_count == 0
                && card.metrics.summary_budget_pass_rate >= 0.95
        )
    ));
    out.push_str(&format!(
        "| Session Bus Invariants | pass={:.2} (== 1.00) | {} |\n",
        card.metrics.session_bus_invariant_pass_rate,
        gate_word(card.metrics.session_bus_invariant_pass_rate >= 1.0)
    ));
    out.push_str(&format!(
        "| **Plumbing Gate** | {}/{} deterministic invariants | **{}** |\n",
        inv_passed,
        inv_total,
        gate_word(gate_ok)
    ));
    out
}

/// Renders a side-by-side Markdown comparison table across multiple scorecards.
pub fn format_comparison_markdown(report: &ComparisonReport) -> String {
    if report.scorecards.len() == 1 {
        return format_scorecard_markdown(&report.scorecards[0]);
    }
    let base = &report.scorecards[0];
    let target = report
        .scorecards
        .iter()
        .find(|card| card.suite_name == "production")
        .unwrap_or(&report.scorecards[1]);
    let (target_passed, target_total) = target.plumbing_invariant_counts();
    let gate_ok = report.passed && target.plumbing_gate_passed();

    let mut out = String::new();
    out.push_str(&format!(
        "## lowyap-eval Suite Comparison — PLUMBING GATE: {} ({}/{} deterministic invariants on `{}`)\n\n",
        gate_word(gate_ok),
        target_passed,
        target_total,
        target.suite_name
    ));
    if report.evaluation_tier == EvaluationTier::SyntheticPlumbingOnly {
        out.push_str(SYNTHETIC_HONESTY_BANNER);
        out.push_str("\n\n");
    }

    // Header row
    out.push_str("| Metric / Invariant |");
    for card in &report.scorecards {
        out.push_str(&format!(" {} |", card.suite_name));
    }
    if report.scorecards.len() >= 2 {
        out.push_str(" Delta (Target - Baseline) |");
    }
    out.push('\n');

    out.push_str("|---|");
    for _ in &report.scorecards {
        out.push_str("---|");
    }
    if report.scorecards.len() >= 2 {
        out.push_str("---|");
    }
    out.push('\n');

    let mut add_row = |label: &str, vals: Vec<String>, delta: Option<String>| {
        out.push_str(&format!("| {label} |"));
        for v in vals {
            out.push_str(&format!(" {v} |"));
        }
        if report.scorecards.len() >= 2 {
            out.push_str(&format!(" {} |", delta.unwrap_or_else(|| "—".to_owned())));
        }
        out.push('\n');
    };

    add_row(
        "Echo Leak Rate (lower better)",
        report
            .scorecards
            .iter()
            .map(|c| {
                format!(
                    "{:.3} ({})",
                    c.metrics.echo_leak_rate,
                    gate_word(c.metrics.echo_leak_rate <= 0.15)
                )
            })
            .collect(),
        Some(format!(
            "{:+.3}",
            target.metrics.echo_leak_rate - base.metrics.echo_leak_rate
        )),
    );
    add_row(
        "Double-Talk Survival (higher better)",
        report
            .scorecards
            .iter()
            .map(|c| {
                format!(
                    "{:.3} ({})",
                    c.metrics.double_talk_survival_rate,
                    gate_word(c.metrics.double_talk_survival_rate >= 0.85)
                )
            })
            .collect(),
        Some(format!(
            "{:+.3}",
            target.metrics.double_talk_survival_rate - base.metrics.double_talk_survival_rate
        )),
    );
    add_row(
        "Silence Hallucinations (0 target)",
        report
            .scorecards
            .iter()
            .map(|c| {
                format!(
                    "{} ({})",
                    c.metrics.silence_hallucination_count,
                    gate_word(c.metrics.silence_hallucination_count == 0)
                )
            })
            .collect(),
        Some(format!(
            "{:+}",
            target.metrics.silence_hallucination_count as isize
                - base.metrics.silence_hallucination_count as isize
        )),
    );
    add_row(
        "Speaker Attribution F1",
        report
            .scorecards
            .iter()
            .map(|c| {
                format!(
                    "{:.3} ({})",
                    c.metrics.speaker_attribution_f1,
                    gate_word(c.metrics.speaker_attribution_f1 >= 0.90)
                )
            })
            .collect(),
        Some(format!(
            "{:+.3}",
            target.metrics.speaker_attribution_f1 - base.metrics.speaker_attribution_f1
        )),
    );
    add_row(
        "Summary Budget Pass Rate",
        report
            .scorecards
            .iter()
            .map(|c| {
                format!(
                    "{:.2} / {} ovf ({})",
                    c.metrics.summary_budget_pass_rate,
                    c.metrics.summary_overflow_count,
                    gate_word(
                        c.metrics.summary_overflow_count == 0
                            && c.metrics.summary_budget_pass_rate >= 0.95
                    )
                )
            })
            .collect(),
        Some(format!(
            "{:+.2}",
            target.metrics.summary_budget_pass_rate - base.metrics.summary_budget_pass_rate
        )),
    );
    add_row(
        "Session Bus Invariants",
        report
            .scorecards
            .iter()
            .map(|c| {
                format!(
                    "{:.2} ({})",
                    c.metrics.session_bus_invariant_pass_rate,
                    gate_word(c.metrics.session_bus_invariant_pass_rate >= 1.0)
                )
            })
            .collect(),
        Some(format!(
            "{:+.2}",
            target.metrics.session_bus_invariant_pass_rate
                - base.metrics.session_bus_invariant_pass_rate
        )),
    );
    add_row(
        "**Plumbing Gate**",
        report
            .scorecards
            .iter()
            .map(|c| {
                let (p, t) = c.plumbing_invariant_counts();
                format!("**{} ({}/{})**", gate_word(c.plumbing_gate_passed()), p, t)
            })
            .collect(),
        Some(format!(
            "**{:+} invariants**",
            target_passed as isize - base.plumbing_invariant_counts().0 as isize
        )),
    );

    out
}

fn best_two_role_assignment(
    remote_ids: &[String],
    votes: &HashMap<String, HashMap<&'static str, usize>>,
) -> (Option<String>, Option<String>) {
    let vote = |id: &str, role: &'static str| -> usize {
        votes
            .get(id)
            .and_then(|m| m.get(role))
            .copied()
            .unwrap_or(0)
    };

    let mut best_score = 0usize;
    let mut best_pair: (Option<String>, Option<String>) = (None, None);

    let candidates: Vec<Option<&String>> = std::iter::once(None)
        .chain(remote_ids.iter().map(Some))
        .collect();

    for &a in &candidates {
        for &b in &candidates {
            if a.is_some() && a == b {
                continue;
            }
            let score =
                a.map_or(0, |id| vote(id, "remote_a")) + b.map_or(0, |id| vote(id, "remote_b"));
            if score > best_score {
                best_score = score;
                best_pair = (a.cloned(), b.cloned());
            }
        }
    }

    best_pair
}

fn interval_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> u64 {
    let start = a_start.max(b_start);
    let end = a_end.min(b_end);
    end.saturating_sub(start)
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

fn round4(v: f32) -> f32 {
    (v * 10_000.0).round() / 10_000.0
}
