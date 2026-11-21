//! Offline deterministic evaluation harness and WAV pair inspector for `lowyap`.
//!
//! Benchmarks baseline, modular reference, production DSP, and full Wave 4C
//! production profiles across six synthetic audio and session scenarios, and
//! inspects dual-stream WAV recordings without mutating the inputs.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod reference_adapters;
pub mod scenarios;
pub mod scorecard;
pub mod wav_replay;

pub use reference_adapters::{
    ChannelOnlyDiarizer, ChannelPlusEnrollmentDiarizer, CorrelationGateEchoCanceller,
    EnergyVadSegmenter, ExtractiveBudgetSummarizer, FixedWindowSegmenter, FlowGuardedAsrAdapter,
    IdealOracleEchoCanceller, NlmsEchoCanceller, SyntheticWhisperAsrSim,
};
pub use scorecard::{
    evaluate_all, evaluate_comparison, format_comparison_markdown, format_scorecard_markdown,
    AdapterManifest, ComparisonReport, DimensionScores, EvalMetrics, EvalScorecard,
    PipelineUnderTest, ScenarioReport,
};
pub use wav_replay::{
    format_wav_report_markdown, inspect_wav_pair, load_wav_mono_16k, WavProfileMetrics,
    WavReplayReport, WavTrackSummary,
};
