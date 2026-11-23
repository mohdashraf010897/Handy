//! Offline deterministic plumbing harness, WAV pair inspector, and real-human-audio
//! empirical benchmark (`--suite real-voice`) for `lowyap`.
//!
//! Synthetic suites verify deterministic pipeline invariants and are explicitly
//! tagged [`EvaluationTier::SyntheticPlumbingOnly`]; real speech-recognition
//! quality is verified exclusively through [`real_voice`] on real human WAVs.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod dictation_bench;
pub mod real_voice;
pub mod reference_adapters;
pub mod scenarios;
pub mod scorecard;
pub mod wav_replay;

pub use dictation_bench::{
    format_dictation_bench_markdown, run_dictation_bench, DictationBenchReport,
    DictationLatencySimulation, LinguistReplay,
};
pub use real_voice::{
    append_trailing_breath_tail, attenuate_quiet_mic, compute_cer, compute_script_preservation,
    compute_wer, detect_brand_token_hallucination, detect_phantom_hallucinations, discover_clips,
    discover_wav_dir, evaluate_real_voice_with_asr, extract_edge_silence_slices,
    format_real_voice_markdown, inject_pink_fan_noise, load_manifest, normalize_for_eval,
    redact_transcript_preview, run_real_voice_suite, BrandLeakFinding, BrandLeakReason,
    ClipCategory, ErrorRateMetrics, PerturbationKind, PerturbationResult, PhantomFinding,
    PhantomFlagKind, RealVoiceClipReport, RealVoiceConfig, RealVoiceManifestEntry, RealVoiceReport,
    RealVoiceStatus, ScriptKind, ScriptPreservationMetrics, DEFAULT_PERTURBATION_SEED,
    DEFAULT_PINK_FAN_SNR_DB, DEFAULT_QUIET_MIC_GAIN, DEFAULT_TRAILING_BREATH_TAIL_MS,
    UNVERIFIED_NO_ASR_STATUS, UNVERIFIED_NO_WAVS_STATUS,
};
pub use reference_adapters::{
    ChannelOnlyDiarizer, ChannelPlusEnrollmentDiarizer, CorrelationGateEchoCanceller,
    EnergyVadSegmenter, ExtractiveBudgetSummarizer, FixedWindowSegmenter, FlowGuardedAsrAdapter,
    IdealOracleEchoCanceller, NlmsEchoCanceller, SyntheticWhisperAsrSim,
};
pub use scorecard::{
    evaluate_all, evaluate_comparison, format_comparison_markdown, format_scorecard_markdown,
    AdapterManifest, ComparisonReport, DimensionScores, EvalMetrics, EvalScorecard, EvaluationTier,
    PipelineUnderTest, ScenarioReport, SYNTHETIC_HONESTY_BANNER,
};
pub use wav_replay::{
    format_wav_report_markdown, inspect_wav_pair, load_wav_mono_16k, WavProfileMetrics,
    WavReplayReport, WavTrackSummary,
};
