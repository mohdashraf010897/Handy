//! Empirical real-human-audio WAV benchmark and acoustic-stress suite (`--suite real-voice`).
//!
//! Evaluates real 16 kHz (or resampled) human recordings with both:
//! - **Reference-free gates** (works on unlabeled local dogfood WAVs):
//!   1. Leading/trailing silence & breath hallucination gate (phantom phrases + repeated n-gram loops)
//!   2. `SV-95` brand-token (`LOWYAP`) hallucination gate
//!   3. Three deterministic acoustic perturbations (`+12 dB SNR` pink/fan noise,
//!      `0.25x` quiet-mic `-12 dB` attenuation, `+1.5 s` trailing breath + room-tone tail)
//! - **Labeled corpus metrics** (when `<stem>.txt` sidecars or a JSON/TOML manifest supply references):
//!   - Unicode-safe normalized Word Error Rate (`WER`) and Character Error Rate (`CER`)
//!   - Code-switch script preservation (Latin, Arabic/Urdu, Devanagari, CJK, Cyrillic)
//!   - `p50` and `p95` Real-Time Factor (`RTF`) and end-to-end latency (`ms`)
//!
//! **Privacy guarantee (`D-05`):** Default stdout and JSON reports emit only clip IDs,
//! aggregate metrics, and failure flags — never raw user transcript sentences.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use hound::{SampleFormat, WavSpec, WavWriter};
use serde::{Deserialize, Serialize};

use crate::reference_adapters::{rms, FRAME_SAMPLES_16K, SAMPLE_RATE_16K};
use crate::scorecard::EvaluationTier;
use crate::wav_replay::load_wav_mono_16k;

/// Banner emitted when `--suite real-voice` finds zero real `.wav` inputs.
pub const UNVERIFIED_NO_WAVS_STATUS: &str =
    "UNVERIFIED (0 real human WAVs evaluated — refusing to emit a quality score)";

/// Banner emitted when WAVs were loaded but no ASR backend was supplied.
pub const UNVERIFIED_NO_ASR_STATUS: &str =
    "UNVERIFIED (no --asr-cmd supplied — refusing to emit a quality score)";

/// Default deterministic seed for acoustic stress perturbations.
pub const DEFAULT_PERTURBATION_SEED: u64 = 0x5EED_CAFE_1600_0001;

/// Minimum edge silence/breath duration (ms) worth probing with ASR.
const MIN_SILENCE_SLICE_MS: u64 = 400;

/// RMS ceiling (~`-42.0 dBFS`) treated as silence or soft breath at clip edges,
/// preserving quiet unvoiced consonants (`-40..-36 dBFS`) in real speech.
const EDGE_SILENCE_RMS_THRESHOLD: f32 = 0.008;

/// Pre-onset / post-offset guard frames (`3 * 20 ms = 60 ms`) kept out of isolated
/// edge silence slices so unvoiced consonant aspiration tails are not scored as silence.
const EDGE_SILENCE_GUARD_FRAMES: usize = 3;

/// Default trailing breath + room-tone tail duration in milliseconds.
pub const DEFAULT_TRAILING_BREATH_TAIL_MS: u64 = 1_500;

/// Default quiet-mic attenuation gain (`0.25x` ≈ `-12.04 dB`).
pub const DEFAULT_QUIET_MIC_GAIN: f32 = 0.25;

/// Default pink/fan noise target signal-to-noise ratio in decibels.
pub const DEFAULT_PINK_FAN_SNR_DB: f32 = 12.0;

/// Duration threshold (ms) at or above which live dictation switches to `LiveDecode::Chunked`.
pub const LIVE_CHUNKER_THRESHOLD_MS: u64 = 14_000;

/// Maximum normalized WER allowed on labeled `clean` / `clean_english` clips.
pub const MAX_CLEAN_ENGLISH_WER: f64 = 0.35;

/// Maximum normalized WER allowed on labeled `accented_english` clips.
pub const MAX_ACCENTED_ENGLISH_WER: f64 = 0.55;

/// Maximum normalized WER allowed on labeled `noisy_room` or `unlabeled` clips.
pub const MAX_NOISY_OR_UNLABELED_WER: f64 = 0.60;

/// Maximum normalized WER allowed on labeled `multilingual` / `code_switch` clips.
pub const MAX_MULTILINGUAL_WER: f64 = 0.75;

/// Acoustic / domain category for a real-voice benchmark clip.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ClipCategory {
    /// Clean studio or quiet-room dictation.
    #[default]
    Clean,
    /// Clean studio or quiet-room US / General English human speech.
    CleanEnglish,
    /// Accented English human speech (South Asian/Indian, UK/Scottish/Irish, L2).
    AccentedEnglish,
    /// Multilingual non-English human speech (e.g., Hindi, Urdu, Spanish, German).
    Multilingual,
    /// Code-switched human speech mixing two or more languages/scripts.
    CodeSwitch,
    /// Combined accented or multilingual / code-switched speech tag.
    AccentMultilingual,
    /// Silence, ambient room tone, or breath-only recording.
    SilenceBreath,
    /// Noisy room, fan hum, or café background speech.
    NoisyRoom,
    /// Discovered from `--wav-dir` without a manifest category label.
    Unlabeled,
}

impl ClipCategory {
    fn from_label(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "clean" => Ok(Self::Clean),
            "clean_english" | "clean-english" => Ok(Self::CleanEnglish),
            "accented_english" | "accented-english" => Ok(Self::AccentedEnglish),
            "multilingual" => Ok(Self::Multilingual),
            "code_switch" | "code-switch" => Ok(Self::CodeSwitch),
            "accent_multilingual" | "accent-multilingual" => Ok(Self::AccentMultilingual),
            "silence_breath" | "silence-breath" | "silence" => Ok(Self::SilenceBreath),
            "noisy_room" | "noisy-room" | "noisy" => Ok(Self::NoisyRoom),
            "unlabeled" | "" => Ok(Self::Unlabeled),
            other => Err(format!("unknown clip category '{other}'")),
        }
    }

    /// Stable snake_case identifier for Markdown and JSON summaries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::CleanEnglish => "clean_english",
            Self::AccentedEnglish => "accented_english",
            Self::Multilingual => "multilingual",
            Self::CodeSwitch => "code_switch",
            Self::AccentMultilingual => "accent_multilingual",
            Self::SilenceBreath => "silence_breath",
            Self::NoisyRoom => "noisy_room",
            Self::Unlabeled => "unlabeled",
        }
    }

    /// Canonical summary bucket grouping `clean_english`, `accented_english`, and `multilingual_code_switch`.
    pub const fn summary_bucket(self) -> &'static str {
        match self {
            Self::Clean | Self::CleanEnglish => "clean_english",
            Self::AccentedEnglish => "accented_english",
            Self::Multilingual | Self::CodeSwitch | Self::AccentMultilingual => {
                "multilingual_code_switch"
            }
            Self::SilenceBreath => "silence_breath",
            Self::NoisyRoom => "noisy_room",
            Self::Unlabeled => "unlabeled",
        }
    }

    /// Realistic, honest per-category WER ceiling enforced on labeled speech clips.
    pub const fn max_labeled_wer(self) -> f64 {
        match self {
            Self::Clean | Self::CleanEnglish => MAX_CLEAN_ENGLISH_WER,
            Self::AccentedEnglish => MAX_ACCENTED_ENGLISH_WER,
            Self::NoisyRoom | Self::Unlabeled => MAX_NOISY_OR_UNLABELED_WER,
            Self::Multilingual | Self::CodeSwitch | Self::AccentMultilingual => {
                MAX_MULTILINGUAL_WER
            }
            Self::SilenceBreath => 0.0,
        }
    }
}

/// Returns the bundled real-human corpus manifest path (`fixtures/real-voice/manifest.toml`).
pub fn default_corpus_manifest_path() -> PathBuf {
    let compile_time = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("real-voice")
        .join("manifest.toml");
    if compile_time.exists() {
        return compile_time;
    }
    for rel in [
        "fixtures/real-voice/manifest.toml",
        "crates/lowyap-eval/fixtures/real-voice/manifest.toml",
        "src-tauri/crates/lowyap-eval/fixtures/real-voice/manifest.toml",
    ] {
        let candidate = PathBuf::from(rel);
        if candidate.exists() {
            return candidate;
        }
    }
    compile_time
}

/// One clip entry loaded from a JSON/TOML manifest or discovered in `--wav-dir`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RealVoiceManifestEntry {
    /// Path to the `.wav` audio file.
    pub wav: PathBuf,
    /// Optional ground-truth transcript text.
    #[serde(default)]
    pub reference_text: Option<String>,
    /// Acoustic category tag.
    #[serde(default)]
    pub category: ClipCategory,
    /// Optional stable clip identifier (defaults to file stem).
    #[serde(default)]
    pub clip_id: Option<String>,
    /// Optional BCP-47 language hint.
    #[serde(default)]
    pub language_hint: Option<String>,
}

/// Configuration for executing `--suite real-voice`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealVoiceConfig {
    /// When `true`, loads the bundled real-human corpus (`fixtures/real-voice/manifest.toml`).
    pub use_default_corpus: bool,
    /// Optional directory scanned recursively for `.wav` files and `<stem>.txt` sidecars.
    pub wav_dir: Option<PathBuf>,
    /// Optional JSON or TOML manifest file listing WAVs and optional references.
    pub manifest: Option<PathBuf>,
    /// Optional external ASR command template (supports `{wav}` and `{lang}` placeholders or trailing path).
    pub asr_cmd: Option<String>,
    /// When `true`, emits redacted diagnostic previews; raw sentences are never printed.
    pub verbose_redacted: bool,
    /// Deterministic PRNG seed for acoustic stress perturbations.
    pub perturbation_seed: u64,
}

impl Default for RealVoiceConfig {
    fn default() -> Self {
        Self {
            use_default_corpus: false,
            wav_dir: None,
            manifest: None,
            asr_cmd: None,
            verbose_redacted: false,
            perturbation_seed: DEFAULT_PERTURBATION_SEED,
        }
    }
}

/// Overall empirical verification status of a `real-voice` benchmark run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealVoiceStatus {
    /// At least one real WAV was transcribed and passed all empirical sanity gates.
    Pass,
    /// At least one real WAV failed a hallucination, brand-leak, perturbation, or ASR gate.
    Fail,
    /// Zero real WAVs or no ASR backend were provided; refusing to fabricate a score.
    Unverified,
}

/// Broad Unicode script bucket used to measure code-switch script preservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptKind {
    /// Basic and extended Latin script.
    Latin,
    /// Arabic / Urdu / Persian script.
    ArabicUrdu,
    /// Devanagari (Hindi / Marathi / Sanskrit) script.
    Devanagari,
    /// Han / Kana / Hangul (CJK) scripts.
    Cjk,
    /// Cyrillic script.
    Cyrillic,
    /// Any other alphabetic script.
    Other,
}

/// Exact Levenshtein error-rate metrics for WER or CER.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ErrorRateMetrics {
    /// Normalized edit distance divided by `reference_len`.
    pub rate: f64,
    /// Minimum substitutions + insertions + deletions.
    pub edits: usize,
    /// Token or character count in the normalized reference.
    pub reference_len: usize,
    /// Token or character count in the normalized hypothesis.
    pub hypothesis_len: usize,
}

/// Code-switch and cross-script preservation metrics for a labeled clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ScriptPreservationMetrics {
    /// Scripts present in the ground-truth reference text.
    pub expected_scripts: Vec<ScriptKind>,
    /// Scripts present in the ASR hypothesis text.
    pub observed_scripts: Vec<ScriptKind>,
    /// Scripts appearing in the hypothesis that were absent from the reference.
    pub unexpected_scripts: Vec<ScriptKind>,
    /// Fraction of `expected_scripts` retained in `observed_scripts` in `[0.0, 1.0]`.
    pub preserved_script_ratio: f64,
    /// `true` when the reference contains two or more distinct scripts.
    pub is_code_switched: bool,
}

/// Classification of an `SV-95` brand-token hallucination finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrandLeakReason {
    /// Standalone ALL-CAPS `"LOWYAP"` token leaked from prompt bias.
    AllCapsBrandToken,
    /// Brand token appeared when ground-truth reference did not contain it.
    UnreferencedBrandInjection,
    /// Disproportionate brand-token density without acoustic or reference justification.
    DisproportionateBrandDensity,
}

/// Structured diagnostic for an `SV-95` `LOWYAP` brand-token hallucination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BrandLeakFinding {
    /// Count of standalone ALL-CAPS `"LOWYAP"` tokens.
    pub all_caps_count: usize,
    /// Total case-insensitive `"lowyap"` tokens.
    pub total_brand_count: usize,
    /// Total word tokens in the output.
    pub total_words: usize,
    /// Fraction of output words matching the brand token.
    pub density: f64,
    /// Primary rule that triggered the finding.
    pub reason: BrandLeakReason,
}

/// Classification of a Whisper phantom / silence hallucination flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhantomFlagKind {
    /// Known YouTube/subtitle Whisper hallucination phrase.
    KnownPhantomPhrase,
    /// Repeated n-gram decoder loop (e.g. repeated 3-gram cycles).
    RepeatedNgramLoop,
    /// Non-empty text emitted on a low-RMS silence or breath edge slice.
    SilenceSliceHallucination,
}

/// Single phantom hallucination finding (privacy-safe label, never raw user speech).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PhantomFinding {
    /// Category of hallucination detected.
    pub kind: PhantomFlagKind,
    /// Privacy-safe rule identifier.
    pub label: String,
}

/// Real-world acoustic degradation applied to each real WAV clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerturbationKind {
    /// `+12 dB SNR` deterministic pink + fan room-noise injection.
    PinkFanNoise12DbSnr,
    /// `0.25x` (`-12 dB`) quiet-mic / whisper-level gain attenuation.
    QuietMicMinus12Db,
    /// `+1.5 s` trailing breath puff and room-tone tail.
    TrailingBreathTail1500Ms,
}

impl PerturbationKind {
    /// Short stable label for CLI and Markdown tables.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PinkFanNoise12DbSnr => "pink_fan_12db_snr",
            Self::QuietMicMinus12Db => "quiet_mic_minus12db",
            Self::TrailingBreathTail1500Ms => "trailing_breath_1500ms",
        }
    }
}

/// Result of running ASR on one acoustically perturbed variant of a real WAV clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PerturbationResult {
    /// Perturbation applied.
    pub kind: PerturbationKind,
    /// Duration of the perturbed audio in milliseconds.
    pub duration_ms: u64,
    /// RMS amplitude of the perturbed audio.
    pub rms: f32,
    /// Elapsed ASR latency in milliseconds (when an ASR backend ran).
    pub latency_ms: Option<f64>,
    /// Normalized WER drift relative to the unperturbed clip's ASR output.
    pub wer_drift_vs_clean: Option<f64>,
    /// Normalized WER against ground-truth reference text (if labeled).
    pub wer_vs_reference: Option<f64>,
    /// Phantom hallucination flags observed under this perturbation.
    pub phantom_flags: Vec<PhantomFinding>,
    /// Brand-token leak observed under this perturbation.
    pub brand_leak: Option<BrandLeakFinding>,
    /// External ASR execution error, if any.
    pub asr_error: Option<String>,
    /// `true` when ASR succeeded with zero phantom flags and zero brand leaks.
    pub passed: bool,
}

/// Per-category WER, CER, and pass-rate breakdown inside `RealVoiceReport`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CategoryWerBreakdown {
    /// Canonical summary bucket (`clean_english`, `accented_english`, `multilingual_code_switch`, `silence_breath`, `noisy_room`, `unlabeled`).
    pub category: String,
    /// Number of clips evaluated in this bucket.
    pub clips_evaluated: usize,
    /// Number of clips passing all empirical gates in this bucket.
    pub clips_passed: usize,
    /// Number of clips with ground-truth reference transcripts in this bucket.
    pub labeled_clips: usize,
    /// Corpus-weighted Word Error Rate across labeled clips in this bucket (`None` if 0 labeled clips).
    pub wer: Option<f64>,
    /// Corpus-weighted Character Error Rate across labeled clips in this bucket (`None` if 0 labeled clips).
    pub cer: Option<f64>,
    /// Maximum allowed per-clip WER ceiling for labeled speech clips in this bucket.
    pub max_allowed_wer: Option<f64>,
}

/// Per-clip empirical report (privacy-safe: omits raw transcript sentences).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RealVoiceClipReport {
    /// Stable clip identifier (manifest `clip_id` or WAV filename stem).
    pub clip_id: String,
    /// Acoustic category tag.
    pub category: ClipCategory,
    /// Optional BCP-47 language hint from the manifest (`en`, `hi`, `ur`, `es`, `de`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_hint: Option<String>,
    /// Whether a ground-truth reference transcript was supplied.
    pub has_reference: bool,
    /// Audio duration in milliseconds after 16 kHz mono normalization.
    pub duration_ms: u64,
    /// `true` when `duration_ms >= 14_000`, exercising `LiveDecode::Chunked` live boundary stitching.
    pub is_long_chunked: bool,
    /// Audio RMS amplitude.
    pub rms: f32,
    /// Maximum absolute amplitude.
    pub peak_abs: f32,
    /// Duration of leading low-RMS silence/breath frames in milliseconds.
    pub leading_silence_ms: u64,
    /// Duration of trailing low-RMS silence/breath frames in milliseconds.
    pub trailing_silence_ms: u64,
    /// Clean-clip ASR decode latency in milliseconds (engine `best_ms` / `transcribe_ms` sum, or wall-clock fallback).
    pub latency_ms: Option<f64>,
    /// Real-Time Factor (`decode_s / audio_s`, lower is faster).
    pub rtf: Option<f64>,
    /// Exact normalized Word Error Rate (when labeled).
    pub wer: Option<ErrorRateMetrics>,
    /// Exact normalized Character Error Rate (when labeled).
    pub cer: Option<ErrorRateMetrics>,
    /// Category WER ceiling applied to this clip when labeled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_allowed_wer: Option<f64>,
    /// Whether this clip's labeled WER stayed within `max_allowed_wer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wer_gate_passed: Option<bool>,
    /// Code-switch script preservation metrics (when labeled).
    pub script_preservation: Option<ScriptPreservationMetrics>,
    /// Hallucinations triggered on sliced leading/trailing silence or breath frames.
    pub silence_slice_hallucinations: Vec<PhantomFinding>,
    /// Phantom phrase or repeated n-gram loop flags on the clean clip output.
    pub phantom_flags: Vec<PhantomFinding>,
    /// `SV-95` brand-token (`LOWYAP`) hallucination finding, if any.
    pub brand_leak: Option<BrandLeakFinding>,
    /// Acoustic stress perturbation results (3 degradations per clip).
    pub perturbations: Vec<PerturbationResult>,
    /// ASR execution failure, if any.
    pub asr_error: Option<String>,
    /// Redacted preview emitted only when `--verbose-redacted` is requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_preview: Option<String>,
    /// `true` when all clip-level gates passed.
    pub passed: bool,
}

/// Aggregate empirical benchmark report for `--suite real-voice`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RealVoiceReport {
    /// Always `EvaluationTier::EmpiricalRealAudio` for this suite.
    pub evaluation_tier: EvaluationTier,
    /// `Pass`, `Fail`, or `Unverified`.
    pub status: RealVoiceStatus,
    /// Explicit human-readable status line (never invents synthetic scores).
    pub status_banner: String,
    /// Number of real WAV files evaluated.
    pub clips_evaluated: usize,
    /// Number of clips with ground-truth reference transcripts.
    pub labeled_clips: usize,
    /// Number of clips evaluated via reference-free gates only.
    pub unlabeled_clips: usize,
    /// Number of `>= 14 s` multi-sentence clips exercising `LiveDecode::Chunked`.
    pub long_chunked_clips: usize,
    /// Number of `>= 14 s` clips that passed all empirical gates.
    pub long_chunked_passed: usize,
    /// Total real audio duration evaluated in seconds.
    pub total_audio_duration_s: f64,
    /// Corpus-weighted Word Error Rate across labeled clips (`None` if 0 labeled clips).
    pub aggregate_wer: Option<f64>,
    /// Corpus-weighted Character Error Rate across labeled clips (`None` if 0 labeled clips).
    pub aggregate_cer: Option<f64>,
    /// Corpus-weighted WER across labeled `clean` / `clean_english` clips.
    pub clean_english_wer: Option<f64>,
    /// Corpus-weighted WER across labeled `accented_english` clips.
    pub accented_english_wer: Option<f64>,
    /// Corpus-weighted WER across labeled `multilingual` / `code_switch` clips.
    pub multilingual_wer: Option<f64>,
    /// Per-category WER, CER, and gate pass-rate breakdown.
    pub category_breakdown: Vec<CategoryWerBreakdown>,
    /// Mean script preservation ratio across labeled clips (`None` if 0 labeled clips).
    pub mean_script_preservation: Option<f64>,
    /// Number of clips that failed the silence/breath or phantom loop gate.
    pub silence_hallucination_clips: usize,
    /// Number of clips that triggered the `SV-95` brand-token leak detector.
    pub brand_leak_clips: usize,
    /// Number of labeled clips that exceeded their category WER ceiling.
    pub wer_gate_failed_clips: usize,
    /// Perturbation checks passed across all evaluated clips.
    pub perturbation_passed: usize,
    /// Total perturbation checks executed across all evaluated clips.
    pub perturbation_total: usize,
    /// Median Real-Time Factor across evaluated clips.
    pub rtf_p50: Option<f64>,
    /// 95th-percentile Real-Time Factor across evaluated clips.
    pub rtf_p95: Option<f64>,
    /// Median ASR decode latency in milliseconds.
    pub latency_ms_p50: Option<f64>,
    /// 95th-percentile ASR decode latency in milliseconds.
    pub latency_ms_p95: Option<f64>,
    /// Per-clip metrics in deterministic discovery order.
    pub clips: Vec<RealVoiceClipReport>,
}

// ============================================================================
// Unicode-Safe Normalization, WER, CER, and Code-Switch Script Preservation
// ============================================================================

/// Returns `true` for ASCII and common multilingual punctuation characters that
/// should be stripped during WER/CER normalization while keeping combining marks
/// (e.g., Devanagari matras and Arabic/Urdu diacritics).
pub fn is_eval_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '\u{00A1}'..='\u{00BF}'
                | '\u{060C}'
                | '\u{061B}'
                | '\u{061F}'
                | '\u{06D4}'
                | '\u{0964}'
                | '\u{0965}'
                | '\u{2000}'..='\u{206F}'
                | '\u{3000}'..='\u{303F}'
                | '\u{FF01}'..='\u{FF0F}'
                | '\u{FF1A}'..='\u{FF20}'
                | '\u{FF3B}'..='\u{FF40}'
                | '\u{FF5B}'..='\u{FF65}'
        )
}

/// Normalizes text for WER/CER scoring: Unicode lowercase, strip punctuation
/// (preserving non-Latin combining vowel signs), and collapse whitespace.
pub fn normalize_for_eval(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    for (idx, &ch) in chars.iter().enumerate() {
        // Preserve apostrophes inside contractions like `don't` / `let's`.
        if matches!(ch, '\'' | '\u{2019}') {
            let prev_alnum = idx > 0 && chars[idx - 1].is_alphanumeric();
            let next_alnum = idx + 1 < chars.len() && chars[idx + 1].is_alphanumeric();
            if prev_alnum && next_alnum {
                cleaned.push('\'');
                continue;
            }
        }
        if ch.is_control() || ch.is_whitespace() || is_eval_punctuation(ch) {
            cleaned.push(' ');
        } else {
            for lower in ch.to_lowercase() {
                cleaned.push(lower);
            }
        }
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Computes exact normalized Word Error Rate (`WER`) via Levenshtein distance.
pub fn compute_wer(reference: &str, hypothesis: &str) -> ErrorRateMetrics {
    let norm_ref = normalize_for_eval(reference);
    let norm_hyp = normalize_for_eval(hypothesis);
    let ref_words: Vec<&str> = norm_ref.split_whitespace().collect();
    let hyp_words: Vec<&str> = norm_hyp.split_whitespace().collect();
    let edits = levenshtein_distance(&ref_words, &hyp_words);
    let rate = if ref_words.is_empty() {
        if hyp_words.is_empty() {
            0.0
        } else {
            1.0
        }
    } else {
        edits as f64 / ref_words.len() as f64
    };
    ErrorRateMetrics {
        rate: round4_f64(rate),
        edits,
        reference_len: ref_words.len(),
        hypothesis_len: hyp_words.len(),
    }
}

/// Computes exact normalized Character Error Rate (`CER`) over whitespace-stripped
/// Unicode scalar values (preserving Devanagari/Urdu marks for accurate script scoring).
pub fn compute_cer(reference: &str, hypothesis: &str) -> ErrorRateMetrics {
    let ref_chars: Vec<char> = normalize_for_eval(reference)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let hyp_chars: Vec<char> = normalize_for_eval(hypothesis)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let edits = levenshtein_distance(&ref_chars, &hyp_chars);
    let rate = if ref_chars.is_empty() {
        if hyp_chars.is_empty() {
            0.0
        } else {
            1.0
        }
    } else {
        edits as f64 / ref_chars.len() as f64
    };
    ErrorRateMetrics {
        rate: round4_f64(rate),
        edits,
        reference_len: ref_chars.len(),
        hypothesis_len: hyp_chars.len(),
    }
}

/// Measures whether scripts present in `reference` (including code-switched pairs
/// such as Latin + Urdu or Latin + Devanagari) are preserved without unexpected
/// cross-script drift.
pub fn compute_script_preservation(reference: &str, hypothesis: &str) -> ScriptPreservationMetrics {
    let expected: BTreeSet<ScriptKind> = collect_scripts(reference);
    let observed: BTreeSet<ScriptKind> = collect_scripts(hypothesis);

    let preserved_count = expected.iter().filter(|s| observed.contains(s)).count();
    let preserved_script_ratio = if expected.is_empty() {
        1.0
    } else {
        preserved_count as f64 / expected.len() as f64
    };
    let unexpected_scripts: Vec<ScriptKind> = observed.difference(&expected).copied().collect();

    ScriptPreservationMetrics {
        is_code_switched: expected.len() >= 2,
        expected_scripts: expected.into_iter().collect(),
        observed_scripts: observed.into_iter().collect(),
        unexpected_scripts,
        preserved_script_ratio: round4_f64(preserved_script_ratio),
    }
}

fn collect_scripts(text: &str) -> BTreeSet<ScriptKind> {
    let mut scripts = BTreeSet::new();
    for ch in text.chars() {
        if let Some(kind) = classify_script(ch) {
            scripts.insert(kind);
        }
    }
    scripts
}

fn classify_script(ch: char) -> Option<ScriptKind> {
    if !ch.is_alphabetic() {
        return None;
    }
    let code = ch as u32;
    let kind = match code {
        0x0041..=0x007A | 0x00C0..=0x024F | 0x1E00..=0x1EFF => ScriptKind::Latin,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => {
            ScriptKind::ArabicUrdu
        }
        0x0900..=0x097F | 0xA8E0..=0xA8FF => ScriptKind::Devanagari,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF => ScriptKind::Cjk,
        0x0400..=0x052F => ScriptKind::Cyrillic,
        _ => ScriptKind::Other,
    };
    Some(kind)
}

fn levenshtein_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, lhs) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, rhs) in b.iter().enumerate() {
            let cost = usize::from(lhs != rhs);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

// ============================================================================
// Reference-Free Hallucination & SV-95 Brand-Leak Detectors
// ============================================================================

const PHANTOM_SUBSTRINGS: &[(&str, &str)] = &[
    ("thank you for watching", "phantom_thank_you_for_watching"),
    ("thanks for watching", "phantom_thanks_for_watching"),
    ("subtitles by", "phantom_subtitles_by"),
    ("amara.org", "phantom_amara_org"),
    ("please subscribe", "phantom_please_subscribe"),
    ("like and subscribe", "phantom_like_and_subscribe"),
    ("translated by", "phantom_translated_by"),
    ("mbc news", "phantom_mbc_news"),
    ("ご視聴ありがとうございました", "phantom_ja_thanks"),
    ("субтитры", "phantom_ru_subtitles"),
    ("продолжение следует", "phantom_ru_tbc"),
    ("подписывайтесь", "phantom_ru_subscribe"),
];

const SILENCE_EXACT_PHANTOMS: &[(&str, &str)] = &[
    ("thank you", "silence_phantom_thank_you"),
    ("thanks", "silence_phantom_thanks"),
    ("you", "silence_phantom_you"),
    ("bye", "silence_phantom_bye"),
    ("bye bye", "silence_phantom_bye_bye"),
    ("music", "silence_phantom_music"),
];

/// Detects Whisper phantom phrases, repeated n-gram loops, and non-empty outputs
/// emitted on low-RMS silence/breath audio slices.
pub fn detect_phantom_hallucinations(text: &str, is_silence_slice: bool) -> Vec<PhantomFinding> {
    let mut flags = Vec::new();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return flags;
    }

    let lowered_raw = trimmed.to_lowercase();
    if lowered_raw.contains("[music]")
        || lowered_raw.contains("(music)")
        || lowered_raw.contains('♪')
    {
        flags.push(PhantomFinding {
            kind: PhantomFlagKind::KnownPhantomPhrase,
            label: "phantom_music_tag".to_owned(),
        });
    }

    let normalized = normalize_for_eval(trimmed);

    for &(needle, label) in PHANTOM_SUBSTRINGS {
        let norm_needle = normalize_for_eval(needle);
        if !norm_needle.is_empty() && normalized.contains(&norm_needle) {
            flags.push(PhantomFinding {
                kind: PhantomFlagKind::KnownPhantomPhrase,
                label: label.to_owned(),
            });
        }
    }

    if is_silence_slice {
        for &(exact, label) in SILENCE_EXACT_PHANTOMS {
            if normalized == exact {
                flags.push(PhantomFinding {
                    kind: PhantomFlagKind::KnownPhantomPhrase,
                    label: label.to_owned(),
                });
            }
        }
    }

    let words: Vec<&str> = normalized.split_whitespace().collect();
    if has_repeated_ngram_loop(&words) {
        flags.push(PhantomFinding {
            kind: PhantomFlagKind::RepeatedNgramLoop,
            label: "repeated_ngram_loop".to_owned(),
        });
    }

    if is_silence_slice && !words.is_empty() {
        flags.push(PhantomFinding {
            kind: PhantomFlagKind::SilenceSliceHallucination,
            label: format!("silence_slice_non_empty_{}w", words.len()),
        });
    }

    flags
}

fn has_repeated_ngram_loop(words: &[&str]) -> bool {
    if words.len() < 4 {
        return false;
    }

    // 1-gram consecutive repetition >= 4x (e.g. "you you you you").
    let mut run = 1usize;
    for pair in words.windows(2) {
        if pair[0] == pair[1] {
            run += 1;
            if run >= 4 {
                return true;
            }
        } else {
            run = 1;
        }
    }

    // 2-gram consecutive repetition >= 3x (e.g. "thank you thank you thank you").
    if words.len() >= 6 {
        for i in 0..=words.len() - 6 {
            let g = &words[i..i + 2];
            if &words[i + 2..i + 4] == g && &words[i + 4..i + 6] == g {
                return true;
            }
        }
    }

    // 3-gram consecutive repetition >= 2x OR total occurrences >= 3 anywhere in clip.
    if words.len() >= 6 {
        let mut counts: HashMap<(&str, &str, &str), usize> = HashMap::new();
        for (i, tri) in words.windows(3).enumerate() {
            if i + 6 <= words.len() && &words[i + 3..i + 6] == tri {
                return true;
            }
            let count = counts.entry((tri[0], tri[1], tri[2])).or_insert(0);
            *count += 1;
            if *count >= 3 {
                return true;
            }
        }
    }

    false
}

/// Detects `SV-95` brand-token hallucinations where Whisper leaks standalone
/// ALL-CAPS `"LOWYAP"` or emits the brand token at disproportionate density
/// without acoustic or ground-truth justification.
pub fn detect_brand_token_hallucination(
    text: &str,
    reference_text: Option<&str>,
    duration_s: f64,
) -> Option<BrandLeakFinding> {
    let raw_tokens: Vec<&str> = text
        .split_whitespace()
        .map(|tok| tok.trim_matches(is_eval_punctuation))
        .filter(|tok| !tok.is_empty())
        .collect();
    if raw_tokens.is_empty() {
        return None;
    }

    let all_caps_count = raw_tokens.iter().filter(|&&t| t == "LOWYAP").count();
    let total_brand_count = raw_tokens
        .iter()
        .filter(|&&t| t.eq_ignore_ascii_case("lowyap"))
        .count();
    if total_brand_count == 0 {
        return None;
    }

    let total_words = raw_tokens.len();
    let density = round4_f64(total_brand_count as f64 / total_words as f64);

    let reference_mentions_brand = reference_text.is_some_and(|ref_txt| {
        let norm = normalize_for_eval(ref_txt);
        norm.split_whitespace().any(|w| w == "lowyap") || norm.contains("low yap")
    });
    let reference_has_all_caps = reference_text.is_some_and(|ref_txt| {
        ref_txt
            .split_whitespace()
            .map(|tok| tok.trim_matches(is_eval_punctuation))
            .any(|tok| tok == "LOWYAP")
    });

    if all_caps_count > 0 && !reference_has_all_caps {
        return Some(BrandLeakFinding {
            all_caps_count,
            total_brand_count,
            total_words,
            density,
            reason: BrandLeakReason::AllCapsBrandToken,
        });
    }

    if reference_text.is_some() && !reference_mentions_brand {
        return Some(BrandLeakFinding {
            all_caps_count,
            total_brand_count,
            total_words,
            density,
            reason: BrandLeakReason::UnreferencedBrandInjection,
        });
    }

    if !reference_mentions_brand {
        let rate_per_sec = if duration_s > 0.0 {
            total_brand_count as f64 / duration_s
        } else {
            0.0
        };
        if total_brand_count >= 2 && (density >= 0.25 || rate_per_sec > 0.35) {
            return Some(BrandLeakFinding {
                all_caps_count,
                total_brand_count,
                total_words,
                density,
                reason: BrandLeakReason::DisproportionateBrandDensity,
            });
        }
    }

    None
}

/// Extracts contiguous leading and trailing low-RMS silence/breath edge slices
/// from a 16 kHz mono audio buffer without overlapping when the clip is fully silent.
pub fn extract_edge_silence_slices(samples_16k: &[f32]) -> (Vec<f32>, Vec<f32>) {
    if samples_16k.len() < FRAME_SAMPLES_16K {
        return (Vec::new(), Vec::new());
    }
    let frame_count = samples_16k.len() / FRAME_SAMPLES_16K;
    let mut lead_frames = 0usize;
    while lead_frames < frame_count {
        let start = lead_frames * FRAME_SAMPLES_16K;
        let frame = &samples_16k[start..start + FRAME_SAMPLES_16K];
        if rms(frame) < EDGE_SILENCE_RMS_THRESHOLD {
            lead_frames += 1;
        } else {
            break;
        }
    }

    if lead_frames == frame_count {
        return (
            samples_16k[..lead_frames * FRAME_SAMPLES_16K].to_vec(),
            Vec::new(),
        );
    }
    let safe_lead_frames = lead_frames.saturating_sub(EDGE_SILENCE_GUARD_FRAMES);
    let leading = samples_16k[..safe_lead_frames * FRAME_SAMPLES_16K].to_vec();

    let mut trail_frames = 0usize;
    while trail_frames < frame_count - lead_frames {
        let frame_idx = frame_count - 1 - trail_frames;
        let start = frame_idx * FRAME_SAMPLES_16K;
        let frame = &samples_16k[start..start + FRAME_SAMPLES_16K];
        if rms(frame) < EDGE_SILENCE_RMS_THRESHOLD {
            trail_frames += 1;
        } else {
            break;
        }
    }
    let safe_trail_frames = trail_frames.saturating_sub(EDGE_SILENCE_GUARD_FRAMES);
    let trail_start = (frame_count - safe_trail_frames) * FRAME_SAMPLES_16K;
    let trailing = samples_16k[trail_start..frame_count * FRAME_SAMPLES_16K].to_vec();
    (leading, trailing)
}

// ============================================================================
// Deterministic Acoustic Stress Perturbations
// ============================================================================

#[derive(Clone, Debug)]
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_signed_f32(&mut self) -> f32 {
        let unit = (self.next_u64() >> 40) as f32 / ((1_u64 << 24) as f32);
        unit * 2.0 - 1.0
    }
}

fn generate_pink_fan_noise(len: usize, seed: u64, include_fan_hum: bool) -> Vec<f32> {
    let mut rng = XorShift64::new(seed);
    let mut b0 = 0.0_f32;
    let mut b1 = 0.0_f32;
    let mut b2 = 0.0_f32;
    let mut b3 = 0.0_f32;
    let mut b4 = 0.0_f32;
    let mut b5 = 0.0_f32;
    let mut b6 = 0.0_f32;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let white = rng.next_signed_f32();
        b0 = 0.99886 * b0 + white * 0.0555179;
        b1 = 0.99332 * b1 + white * 0.0750759;
        b2 = 0.96900 * b2 + white * 0.153_852;
        b3 = 0.86650 * b3 + white * 0.3104856;
        b4 = 0.55000 * b4 + white * 0.5329522;
        b5 = -0.7616 * b5 - white * 0.0168980;
        let pink = (b0 + b1 + b2 + b3 + b4 + b5 + b6 + white * 0.5362) * 0.11;
        b6 = white * 0.115926;

        let fan = if include_fan_hum {
            let t = i as f32 / SAMPLE_RATE_16K as f32;
            let hum_120 = (2.0 * std::f32::consts::PI * 120.0 * t).sin();
            let hum_240 = (2.0 * std::f32::consts::PI * 240.0 * t).sin();
            0.05 * hum_120 + 0.025 * hum_240
        } else {
            0.0
        };
        out.push(pink + fan);
    }
    out
}

fn active_speech_rms(samples_16k: &[f32]) -> f32 {
    let mut sum_sq = 0.0_f64;
    let mut count = 0usize;
    for frame in samples_16k.as_chunks::<FRAME_SAMPLES_16K>().0 {
        if rms(frame) >= EDGE_SILENCE_RMS_THRESHOLD {
            sum_sq += frame
                .iter()
                .map(|&s| f64::from(s) * f64::from(s))
                .sum::<f64>();
            count += frame.len();
        }
    }
    if count > 0 {
        (sum_sq / count as f64).sqrt() as f32
    } else {
        rms(samples_16k)
    }
}

/// Perturbation 1: injects deterministic pink + fan room noise calibrated to
/// `target_snr_db` (e.g. `+12 dB SNR`), preserving exact sample length.
pub fn inject_pink_fan_noise(samples_16k: &[f32], target_snr_db: f32, seed: u64) -> Vec<f32> {
    if samples_16k.is_empty() {
        return Vec::new();
    }
    let noise = generate_pink_fan_noise(samples_16k.len(), seed, true);
    let noise_rms = rms(&noise);
    let sig_rms = active_speech_rms(samples_16k).max(1e-4);
    let desired_noise_rms = sig_rms / 10.0_f32.powf(target_snr_db / 20.0);
    let scale = if noise_rms > 1e-9 {
        desired_noise_rms / noise_rms
    } else {
        0.0
    };

    samples_16k
        .iter()
        .zip(noise.iter())
        .map(|(&s, &n)| (s + n * scale).clamp(-1.0, 1.0))
        .collect()
}

/// Perturbation 2: attenuates microphone gain (e.g. `0.25x` / `-12 dB`) to simulate
/// quiet-mic or whisper-level input while preserving exact sample length.
pub fn attenuate_quiet_mic(samples_16k: &[f32], gain: f32) -> Vec<f32> {
    let safe_gain = if gain.is_finite() { gain.max(0.0) } else { 0.0 };
    samples_16k
        .iter()
        .map(|&s| (s * safe_gain).clamp(-1.0, 1.0))
        .collect()
}

/// Perturbation 3: appends a deterministic trailing breath puff + room-tone tail
/// of `tail_ms` milliseconds (default `1500 ms` = `24_000` samples at 16 kHz).
pub fn append_trailing_breath_tail(samples_16k: &[f32], tail_ms: u64, seed: u64) -> Vec<f32> {
    let tail_samples = (tail_ms as usize).saturating_mul(SAMPLE_RATE_16K) / 1_000;
    let mut out = Vec::with_capacity(samples_16k.len() + tail_samples);
    out.extend_from_slice(samples_16k);
    if tail_samples == 0 {
        return out;
    }

    let raw_room = generate_pink_fan_noise(tail_samples, seed, false);
    let raw_rms = rms(&raw_room).max(1e-9);
    // ~-48 dBFS room floor.
    let room_scale = 0.004 / raw_rms;

    let breath_start = (150 * SAMPLE_RATE_16K / 1_000).min(tail_samples);
    let breath_len = (400 * SAMPLE_RATE_16K / 1_000).min(tail_samples.saturating_sub(breath_start));
    let mut rng = XorShift64::new(seed ^ 0xA5A5_5A5A_1234_5678);
    let mut lp = 0.0_f32;

    for (i, &r) in raw_room.iter().enumerate() {
        let mut sample = r * room_scale;
        if breath_len > 0 && i >= breath_start && i < breath_start + breath_len {
            let pos = (i - breath_start) as f32 / breath_len as f32;
            let env = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * pos).cos();
            let white = rng.next_signed_f32();
            lp = 0.72 * lp + 0.28 * white;
            // Soft breath exhalation around -34 dBFS peak.
            sample += lp * env * 0.022;
        }
        out.push(sample.clamp(-1.0, 1.0));
    }
    out
}

// ============================================================================
// Privacy-Safe Redaction
// ============================================================================

/// Produces a redacted diagnostic preview for `--verbose-redacted` without
/// echoing raw private user sentences.
pub fn redact_transcript_preview(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return "[empty]".to_owned();
    }
    let mut parts = Vec::with_capacity(tokens.len().min(12));
    for &tok in tokens.iter().take(12) {
        let core = tok.trim_matches(is_eval_punctuation);
        if core == "LOWYAP" {
            parts.push("[LOWYAP]".to_owned());
        } else {
            let chars: Vec<char> = core.chars().collect();
            if chars.len() <= 2 {
                parts.push("**".to_owned());
            } else {
                parts.push(format!("{}***{}", chars[0], chars[chars.len() - 1]));
            }
        }
    }
    let ellipsis = if tokens.len() > 12 { " ..." } else { "" };
    format!(
        "[redacted {}w: {}{}]",
        tokens.len(),
        parts.join(" "),
        ellipsis
    )
}

// ============================================================================
// Discovery & Manifest Loading (JSON + Zero-Dependency TOML)
// ============================================================================

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonManifestShape {
    List(Vec<RealVoiceManifestEntry>),
    Wrapped { clips: Vec<RealVoiceManifestEntry> },
}

/// Loads a JSON or TOML real-voice manifest and resolves relative `.wav` paths
/// against the manifest's parent directory.
pub fn load_manifest(path: &Path) -> Result<Vec<RealVoiceManifestEntry>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed reading manifest '{}': {e}", path.display()))?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);

    let mut entries = match ext.as_deref() {
        Some("toml") => parse_toml_manifest(&content)?,
        Some("json") => parse_json_manifest(&content)?,
        _ => parse_json_manifest(&content).or_else(|_| parse_toml_manifest(&content))?,
    };

    for entry in &mut entries {
        if entry.wav.is_relative() {
            entry.wav = base_dir.join(&entry.wav);
        }
        if entry.reference_text.is_none() {
            entry.reference_text = read_sidecar_reference(&entry.wav);
        }
    }
    Ok(entries)
}

fn parse_json_manifest(content: &str) -> Result<Vec<RealVoiceManifestEntry>, String> {
    let parsed: JsonManifestShape =
        serde_json::from_str(content).map_err(|e| format!("invalid JSON manifest: {e}"))?;
    Ok(match parsed {
        JsonManifestShape::List(v) => v,
        JsonManifestShape::Wrapped { clips } => clips,
    })
}

fn parse_toml_manifest(content: &str) -> Result<Vec<RealVoiceManifestEntry>, String> {
    let mut entries = Vec::new();
    let mut current: Option<HashMap<String, String>> = None;

    for (line_idx, raw_line) in content.lines().enumerate() {
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[clips]]" || line == "[[clip]]" {
            if let Some(table) = current.take() {
                entries.push(build_entry_from_toml_table(&table, line_idx)?);
            }
            current = Some(HashMap::new());
            continue;
        }
        let Some(table) = current.as_mut() else {
            return Err(format!(
                "TOML manifest line {}: expected `[[clips]]` header before key-value pair",
                line_idx + 1
            ));
        };
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!(
                "TOML manifest line {}: expected `key = \"value\"`",
                line_idx + 1
            ));
        };
        let key = k.trim().to_owned();
        let val = parse_toml_string_value(v.trim(), line_idx + 1)?;
        table.insert(key, val);
    }

    if let Some(table) = current.take() {
        entries.push(build_entry_from_toml_table(
            &table,
            content.lines().count(),
        )?);
    }

    Ok(entries)
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_double => escaped = true,
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '#' if !in_double && !in_single => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn parse_toml_string_value(raw: &str, line_num: usize) -> Result<String, String> {
    if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
        return Ok(raw[1..raw.len() - 1].to_owned());
    }
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        let mut out = String::new();
        let mut chars = raw[1..raw.len() - 1].chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => {
                        return Err(format!(
                            "TOML manifest line {line_num}: trailing escape in string"
                        ))
                    }
                }
            } else {
                out.push(ch);
            }
        }
        return Ok(out);
    }
    Err(format!(
        "TOML manifest line {line_num}: expected quoted string value, got `{raw}`"
    ))
}

fn build_entry_from_toml_table(
    table: &HashMap<String, String>,
    line_num: usize,
) -> Result<RealVoiceManifestEntry, String> {
    let wav = table
        .get("wav")
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("TOML manifest entry near line {line_num} missing `wav`"))?;
    let category = match table.get("category") {
        Some(cat) => ClipCategory::from_label(cat)?,
        None => ClipCategory::Clean,
    };
    let reference_text = table
        .get("reference_text")
        .filter(|v| !v.trim().is_empty())
        .cloned();
    let clip_id = table
        .get("clip_id")
        .filter(|v| !v.trim().is_empty())
        .cloned();
    let language_hint = table
        .get("language_hint")
        .filter(|v| !v.trim().is_empty())
        .cloned();

    Ok(RealVoiceManifestEntry {
        wav: PathBuf::from(wav),
        reference_text,
        category,
        clip_id,
        language_hint,
    })
}

/// Recursively discovers `.wav` files inside `dir`, pairing any `<stem>.txt`
/// sidecar transcript file if present.
pub fn discover_wav_dir(dir: &Path) -> Result<Vec<RealVoiceManifestEntry>, String> {
    if !dir.exists() {
        return Err(format!("WAV directory '{}' does not exist", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!(
            "WAV directory '{}' is not a directory",
            dir.display()
        ));
    }

    let mut wav_paths = Vec::new();
    collect_wav_paths_recursive(dir, &mut wav_paths)?;
    wav_paths.sort();

    let entries = wav_paths
        .into_iter()
        .map(|wav| {
            let reference_text = read_sidecar_reference(&wav);
            let clip_id = derive_clip_id(&wav);
            RealVoiceManifestEntry {
                wav,
                reference_text,
                category: ClipCategory::Unlabeled,
                clip_id: Some(clip_id),
                language_hint: None,
            }
        })
        .collect();
    Ok(entries)
}

fn collect_wav_paths_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let read_dir = fs::read_dir(dir)
        .map_err(|e| format!("failed reading directory '{}': {e}", dir.display()))?;
    for entry_res in read_dir {
        let entry =
            entry_res.map_err(|e| format!("failed reading entry in '{}': {e}", dir.display()))?;
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        if path.is_dir() {
            collect_wav_paths_recursive(&path, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn read_sidecar_reference(wav_path: &Path) -> Option<String> {
    let sidecar = wav_path.with_extension("txt");
    let raw = fs::read_to_string(sidecar).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn derive_clip_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("unnamed_clip")
        .to_owned()
}

/// Discovers all real WAV clips from `--default-corpus`, `config.manifest`, and/or `config.wav_dir`.
pub fn discover_clips(config: &RealVoiceConfig) -> Result<Vec<RealVoiceManifestEntry>, String> {
    let mut clips = Vec::new();
    let mut seen_paths = BTreeSet::new();

    if config.use_default_corpus {
        let default_manifest = default_corpus_manifest_path();
        for entry in load_manifest(&default_manifest)? {
            let key = entry
                .wav
                .canonicalize()
                .unwrap_or_else(|_| entry.wav.clone());
            if seen_paths.insert(key) {
                clips.push(entry);
            }
        }
    }

    if let Some(manifest_path) = config.manifest.as_deref() {
        for entry in load_manifest(manifest_path)? {
            let key = entry
                .wav
                .canonicalize()
                .unwrap_or_else(|_| entry.wav.clone());
            if seen_paths.insert(key) {
                clips.push(entry);
            }
        }
    }

    if let Some(wav_dir) = config.wav_dir.as_deref() {
        for entry in discover_wav_dir(wav_dir)? {
            let key = entry
                .wav
                .canonicalize()
                .unwrap_or_else(|_| entry.wav.clone());
            if seen_paths.insert(key) {
                clips.push(entry);
            }
        }
    }

    Ok(clips)
}

// ============================================================================
// External `--asr-cmd` Runner (Workspace-Relative Scratch, Never `/tmp`)
// ============================================================================

struct ScratchWavGuard {
    dir: PathBuf,
    wav_path: PathBuf,
}

impl ScratchWavGuard {
    fn create(samples_16k: &[f32]) -> Result<Self, String> {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::current_dir()
            .map_err(|e| format!("failed resolving current working directory: {e}"))?;
        let dir = base.join(format!(
            ".lowyap-eval-asr-scratch-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&dir)
            .map_err(|e| format!("failed creating scratch dir '{}': {e}", dir.display()))?;
        let wav_path = dir.join("clip_16k.wav");
        write_mono_16k_wav(&wav_path, samples_16k)?;
        Ok(Self { dir, wav_path })
    }
}

impl Drop for ScratchWavGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.wav_path);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_mono_16k_wav(path: &Path, samples_16k: &[f32]) -> Result<(), String> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE_16K as u32,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)
        .map_err(|e| format!("failed creating WAV '{}': {e}", path.display()))?;
    for &sample in samples_16k {
        let clamped = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let q = (clamped * f32::from(i16::MAX)).round() as i16;
        writer
            .write_sample(q)
            .map_err(|e| format!("failed writing WAV sample: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("failed finalizing WAV '{}': {e}", path.display()))
}

fn split_command_template(template: &str) -> Result<(String, Vec<String>), String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in template.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_single || in_double {
        return Err("unclosed quote in --asr-cmd".to_owned());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.is_empty() {
        return Err("--asr-cmd cannot be empty".to_owned());
    }
    let program = tokens.remove(0);
    Ok((program, tokens))
}

/// Parsed transcript and optional engine decode latency extracted from `--asr-cmd` stdout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AsrStdoutOutput {
    /// Cleaned transcript text (with structured JSON unwrapped or Tauri log lines stripped).
    pub text: String,
    /// Engine decode latency in milliseconds (`best_ms` or `transcribe_ms` sum from JSON), if reported.
    pub decode_ms: Option<f64>,
    /// Whether `--simulate-live` live-chunking path was reported by JSON stdout.
    pub simulate_live: Option<bool>,
    /// Decoded chunk count reported by JSON stdout, if available.
    pub chunk_count: Option<u32>,
}

fn invoke_asr_command(
    cmd_template: &str,
    samples_16k: &[f32],
    language_hint: Option<&str>,
) -> Result<AsrStdoutOutput, String> {
    let (program, raw_args) = split_command_template(cmd_template)?;
    let scratch = ScratchWavGuard::create(samples_16k)?;
    let wav_arg = scratch.wav_path.to_string_lossy().into_owned();
    let lang_arg = language_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("auto");

    let mut replaced_wav_placeholder = false;
    let mut args = Vec::with_capacity(raw_args.len() + 1);
    for arg in raw_args {
        if arg.contains("{wav}") {
            replaced_wav_placeholder = true;
        }
        let expanded = arg.replace("{wav}", &wav_arg).replace("{lang}", lang_arg);
        args.push(expanded);
    }
    if !replaced_wav_placeholder {
        args.push(wav_arg);
    }

    let output = Command::new(&program)
        .args(&args)
        .output()
        .map_err(|e| format!("failed spawning ASR command '{program}': {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!(
            "ASR command '{program}' exited with {}: {}",
            output.status,
            if stderr.is_empty() {
                "no stderr"
            } else {
                &stderr
            }
        ));
    }

    let raw_stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_asr_stdout(&raw_stdout))
}

/// Extracts transcript text and optional engine decode latency (`best_ms` or `transcribe_ms` sum)
/// from `--asr-cmd` stdout, stripping `tauri-plugin-log` lines when plain text is emitted.
pub fn parse_asr_stdout(raw: &str) -> AsrStdoutOutput {
    // 1. Prefer structured JSON output (e.g. `lowyap --json --transcribe-file <wav>`)
    for line in raw.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(text) = val.get("text").and_then(|v| v.as_str()) {
                    let decode_ms = extract_json_decode_ms(&val);
                    let simulate_live = val.get("simulate_live").and_then(|v| v.as_bool());
                    let chunk_count = val
                        .get("chunk_count")
                        .and_then(|v| v.as_u64())
                        .and_then(|n| u32::try_from(n).ok());
                    return AsrStdoutOutput {
                        text: text.trim().to_owned(),
                        decode_ms,
                        simulate_live,
                        chunk_count,
                    };
                }
            }
        }
    }
    // 2. Fallback: strip tauri-plugin-log `[YYYY-MM-DD][HH:MM:SS][...][INFO]` lines
    let filtered: Vec<&str> = raw
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("[20")
                && (t.contains("][INFO]")
                    || t.contains("][DEBUG]")
                    || t.contains("][WARN]")
                    || t.contains("][ERROR]")
                    || t.contains("][TRACE]")))
        })
        .collect();
    AsrStdoutOutput {
        text: filtered.join("\n").trim().to_owned(),
        decode_ms: None,
        simulate_live: None,
        chunk_count: None,
    }
}

fn extract_json_decode_ms(val: &serde_json::Value) -> Option<f64> {
    if let Some(best) = val
        .get("best_ms")
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
    {
        return Some(best);
    }
    let transcribe_val = val.get("transcribe_ms")?;
    if let Some(single) = transcribe_val
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
    {
        return Some(single);
    }
    let arr = transcribe_val.as_array()?;
    let mut sum = 0.0_f64;
    let mut count = 0usize;
    for item in arr {
        if let Some(ms) = item.as_f64().filter(|v| v.is_finite() && *v >= 0.0) {
            sum += ms;
            count += 1;
        }
    }
    (count > 0).then_some(sum)
}

/// Extracts transcript text from `--asr-cmd` stdout (JSON `"text"` field or log-stripped plain text).
pub fn extract_asr_stdout_text(raw: &str) -> String {
    parse_asr_stdout(raw).text
}

// ============================================================================
// Benchmark Runner & Report Formatter
// ============================================================================

/// Type alias for a function-pointer ASR callback used when no backend is supplied.
type TimedAsrCallbackFn = fn(&[f32], Option<&str>) -> Result<AsrStdoutOutput, String>;

/// Executes `--suite real-voice` using `--asr-cmd` (when provided).
///
/// Refuses to emit a quality score when 0 WAVs or no `--asr-cmd` are provided.
pub fn run_real_voice_suite(config: &RealVoiceConfig) -> Result<RealVoiceReport, String> {
    if let Some(cmd) = config.asr_cmd.clone() {
        evaluate_real_voice_with_timed_asr(
            config,
            Some(move |samples: &[f32], lang: Option<&str>| {
                invoke_asr_command(&cmd, samples, lang)
            }),
        )
    } else {
        evaluate_real_voice_with_timed_asr(config, None::<TimedAsrCallbackFn>)
    }
}

/// Evaluates discovered real WAV clips using an optional in-process ASR callback
/// `Fn(&[f32]) -> Result<String, String>`.
pub fn evaluate_real_voice_with_asr<F>(
    config: &RealVoiceConfig,
    asr_fn: Option<F>,
) -> Result<RealVoiceReport, String>
where
    F: Fn(&[f32]) -> Result<String, String>,
{
    let wrapped = asr_fn.map(|f| {
        move |samples: &[f32], _lang: Option<&str>| {
            f(samples).map(|text| AsrStdoutOutput {
                text,
                ..AsrStdoutOutput::default()
            })
        }
    });
    evaluate_real_voice_with_timed_asr(config, wrapped)
}

/// Evaluates discovered real WAV clips using an optional language-aware ASR callback
/// that may also supply engine-reported decode latency (`best_ms` / `transcribe_ms`).
pub fn evaluate_real_voice_with_timed_asr<F>(
    config: &RealVoiceConfig,
    asr_fn: Option<F>,
) -> Result<RealVoiceReport, String>
where
    F: Fn(&[f32], Option<&str>) -> Result<AsrStdoutOutput, String>,
{
    let entries = discover_clips(config)?;
    if entries.is_empty() {
        return Ok(RealVoiceReport {
            evaluation_tier: EvaluationTier::EmpiricalRealAudio,
            status: RealVoiceStatus::Unverified,
            status_banner: UNVERIFIED_NO_WAVS_STATUS.to_owned(),
            clips_evaluated: 0,
            labeled_clips: 0,
            unlabeled_clips: 0,
            long_chunked_clips: 0,
            long_chunked_passed: 0,
            total_audio_duration_s: 0.0,
            aggregate_wer: None,
            aggregate_cer: None,
            clean_english_wer: None,
            accented_english_wer: None,
            multilingual_wer: None,
            category_breakdown: Vec::new(),
            mean_script_preservation: None,
            silence_hallucination_clips: 0,
            brand_leak_clips: 0,
            wer_gate_failed_clips: 0,
            perturbation_passed: 0,
            perturbation_total: 0,
            rtf_p50: None,
            rtf_p95: None,
            latency_ms_p50: None,
            latency_ms_p95: None,
            clips: Vec::new(),
        });
    }

    let mut clip_reports = Vec::with_capacity(entries.len());
    let mut total_audio_duration_s = 0.0_f64;
    let mut labeled_clips = 0usize;
    let mut unlabeled_clips = 0usize;
    let mut long_chunked_clips = 0usize;
    let mut long_chunked_passed = 0usize;
    let mut total_wer_edits = 0usize;
    let mut total_wer_ref_words = 0usize;
    let mut total_cer_edits = 0usize;
    let mut total_cer_ref_chars = 0usize;
    let mut script_ratios = Vec::new();
    let mut silence_hallucination_clips = 0usize;
    let mut brand_leak_clips = 0usize;
    let mut wer_gate_failed_clips = 0usize;
    let mut perturbation_passed = 0usize;
    let mut perturbation_total = 0usize;
    let mut rtf_samples = Vec::new();
    let mut latency_samples = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        let (samples_16k, summary) = load_wav_mono_16k(&entry.wav)?;
        let clip_id = entry
            .clip_id
            .clone()
            .unwrap_or_else(|| derive_clip_id(&entry.wav));
        let duration_s = summary.duration_ms as f64 / 1_000.0;
        let is_long_chunked = summary.duration_ms >= LIVE_CHUNKER_THRESHOLD_MS;
        if is_long_chunked {
            long_chunked_clips += 1;
        }
        total_audio_duration_s += duration_s;

        if entry.reference_text.is_some() {
            labeled_clips += 1;
        } else {
            unlabeled_clips += 1;
        }

        let (lead_silence, trail_silence) = extract_edge_silence_slices(&samples_16k);
        let leading_silence_ms = lead_silence.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
        let trailing_silence_ms = trail_silence.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;

        let clip_seed = config
            .perturbation_seed
            .wrapping_add((idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let perturbed_variants = [
            (
                PerturbationKind::PinkFanNoise12DbSnr,
                inject_pink_fan_noise(&samples_16k, DEFAULT_PINK_FAN_SNR_DB, clip_seed),
            ),
            (
                PerturbationKind::QuietMicMinus12Db,
                attenuate_quiet_mic(&samples_16k, DEFAULT_QUIET_MIC_GAIN),
            ),
            (
                PerturbationKind::TrailingBreathTail1500Ms,
                append_trailing_breath_tail(
                    &samples_16k,
                    DEFAULT_TRAILING_BREATH_TAIL_MS,
                    clip_seed ^ 0xBEEF_F00D,
                ),
            ),
        ];

        let lang_hint = entry.language_hint.as_deref();
        let mut clean_output: Option<String> = None;
        let mut live_chunking_verified = true;
        let mut latency_ms = None;
        let mut rtf = None;
        let mut wer = None;
        let mut cer = None;
        let mut script_preservation = None;
        let mut silence_slice_hallucinations = Vec::new();
        let mut phantom_flags = Vec::new();
        let mut brand_leak = None;
        let mut asr_error = None;
        let mut redacted_preview = None;
        let mut perturbations = Vec::with_capacity(perturbed_variants.len());

        if let Some(ref asr) = asr_fn {
            let started = Instant::now();
            match asr(&samples_16k, lang_hint) {
                Ok(asr_out) => {
                    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
                    let effective_ms = asr_out.decode_ms.unwrap_or(wall_ms);
                    if is_long_chunked && asr_out.simulate_live == Some(false) {
                        live_chunking_verified = asr_out.chunk_count.unwrap_or(0) >= 2;
                    }
                    let text = asr_out.text;
                    let clip_rtf = if duration_s > 1e-6 {
                        (effective_ms / 1_000.0) / duration_s
                    } else {
                        0.0
                    };
                    latency_ms = Some(round2_f64(effective_ms));
                    rtf = Some(round4_f64(clip_rtf));
                    latency_samples.push(effective_ms);
                    rtf_samples.push(clip_rtf);

                    let whole_is_silence = entry.category == ClipCategory::SilenceBreath
                        || summary.rms < EDGE_SILENCE_RMS_THRESHOLD;
                    phantom_flags = detect_phantom_hallucinations(&text, whole_is_silence);
                    brand_leak = detect_brand_token_hallucination(
                        &text,
                        entry.reference_text.as_deref(),
                        duration_s,
                    );

                    if let Some(ref reference) = entry.reference_text {
                        let w = compute_wer(reference, &text);
                        let c = compute_cer(reference, &text);
                        let sp = compute_script_preservation(reference, &text);
                        total_wer_edits += w.edits;
                        total_wer_ref_words += w.reference_len;
                        total_cer_edits += c.edits;
                        total_cer_ref_chars += c.reference_len;
                        script_ratios.push(sp.preserved_script_ratio);
                        wer = Some(w);
                        cer = Some(c);
                        script_preservation = Some(sp);
                    }

                    if config.verbose_redacted {
                        redacted_preview = Some(redact_transcript_preview(&text));
                    }
                    clean_output = Some(text);
                }
                Err(err) => {
                    asr_error = Some(err);
                }
            }

            // Silence / breath edge slice checks.
            for (edge_name, slice, slice_ms) in [
                ("leading", &lead_silence, leading_silence_ms),
                ("trailing", &trail_silence, trailing_silence_ms),
            ] {
                if slice_ms >= MIN_SILENCE_SLICE_MS {
                    match asr(slice, lang_hint) {
                        Ok(slice_out) => {
                            for mut finding in detect_phantom_hallucinations(&slice_out.text, true)
                            {
                                finding.label = format!("{edge_name}:{}", finding.label);
                                silence_slice_hallucinations.push(finding);
                            }
                        }
                        Err(err) => {
                            silence_slice_hallucinations.push(PhantomFinding {
                                kind: PhantomFlagKind::SilenceSliceHallucination,
                                label: format!("{edge_name}:asr_error:{err}"),
                            });
                        }
                    }
                }
            }

            // Three acoustic stress perturbations.
            for (kind, perturbed_audio) in &perturbed_variants {
                let p_duration_ms = perturbed_audio.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
                let p_duration_s = p_duration_ms as f64 / 1_000.0;
                let p_rms = round4_f32(rms(perturbed_audio));
                let p_started = Instant::now();
                let res = match asr(perturbed_audio, lang_hint) {
                    Ok(p_out) => {
                        let p_wall_ms = p_started.elapsed().as_secs_f64() * 1_000.0;
                        let p_lat = round2_f64(p_out.decode_ms.unwrap_or(p_wall_ms));
                        let p_text = p_out.text;
                        let p_is_silence = entry.category == ClipCategory::SilenceBreath
                            || summary.rms < EDGE_SILENCE_RMS_THRESHOLD;
                        let p_phantoms = detect_phantom_hallucinations(&p_text, p_is_silence);
                        let p_brand = detect_brand_token_hallucination(
                            &p_text,
                            entry.reference_text.as_deref(),
                            p_duration_s,
                        );
                        let drift = clean_output
                            .as_deref()
                            .map(|clean| compute_wer(clean, &p_text).rate);
                        let ref_wer = entry
                            .reference_text
                            .as_deref()
                            .map(|r| compute_wer(r, &p_text).rate);
                        let accuracy_ok = if p_is_silence {
                            true
                        } else if let Some(rw) = ref_wer {
                            rw <= (entry.category.max_labeled_wer() + 0.35).min(0.85)
                        } else if let Some(d) = drift {
                            d <= 0.60
                        } else {
                            true
                        };
                        let ok = p_phantoms.is_empty() && p_brand.is_none() && accuracy_ok;
                        PerturbationResult {
                            kind: *kind,
                            duration_ms: p_duration_ms,
                            rms: p_rms,
                            latency_ms: Some(p_lat),
                            wer_drift_vs_clean: drift,
                            wer_vs_reference: ref_wer,
                            phantom_flags: p_phantoms,
                            brand_leak: p_brand,
                            asr_error: None,
                            passed: ok,
                        }
                    }
                    Err(err) => PerturbationResult {
                        kind: *kind,
                        duration_ms: p_duration_ms,
                        rms: p_rms,
                        latency_ms: None,
                        wer_drift_vs_clean: None,
                        wer_vs_reference: None,
                        phantom_flags: Vec::new(),
                        brand_leak: None,
                        asr_error: Some(err),
                        passed: false,
                    },
                };
                perturbation_total += 1;
                if res.passed {
                    perturbation_passed += 1;
                }
                perturbations.push(res);
            }
        } else {
            // Acoustic inspection only; no ASR backend was provided.
            for (kind, perturbed_audio) in &perturbed_variants {
                perturbations.push(PerturbationResult {
                    kind: *kind,
                    duration_ms: perturbed_audio.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64,
                    rms: round4_f32(rms(perturbed_audio)),
                    latency_ms: None,
                    wer_drift_vs_clean: None,
                    wer_vs_reference: None,
                    phantom_flags: Vec::new(),
                    brand_leak: None,
                    asr_error: None,
                    passed: false,
                });
            }
        }

        let has_silence_or_phantom =
            !silence_slice_hallucinations.is_empty() || !phantom_flags.is_empty();
        if has_silence_or_phantom {
            silence_hallucination_clips += 1;
        }
        if brand_leak.is_some() {
            brand_leak_clips += 1;
        }
        let max_allowed_wer = entry
            .reference_text
            .is_some()
            .then(|| entry.category.max_labeled_wer());
        let wer_gate_passed = wer
            .as_ref()
            .zip(max_allowed_wer)
            .map(|(w, ceiling)| w.rate <= ceiling);
        if wer_gate_passed == Some(false) {
            wer_gate_failed_clips += 1;
        }
        let all_perturbations_ok = asr_fn.is_some() && perturbations.iter().all(|pert| pert.passed);
        let clip_passed = asr_fn.is_some()
            && asr_error.is_none()
            && !has_silence_or_phantom
            && brand_leak.is_none()
            && wer_gate_passed.unwrap_or(true)
            && all_perturbations_ok
            && (!is_long_chunked || live_chunking_verified);
        if is_long_chunked && clip_passed {
            long_chunked_passed += 1;
        }

        clip_reports.push(RealVoiceClipReport {
            clip_id,
            category: entry.category,
            language_hint: entry.language_hint.clone(),
            has_reference: entry.reference_text.is_some(),
            duration_ms: summary.duration_ms,
            is_long_chunked,
            rms: summary.rms,
            peak_abs: summary.peak_abs,
            leading_silence_ms,
            trailing_silence_ms,
            latency_ms,
            rtf,
            wer,
            cer,
            max_allowed_wer,
            wer_gate_passed,
            script_preservation,
            silence_slice_hallucinations,
            phantom_flags,
            brand_leak,
            perturbations,
            asr_error,
            redacted_preview,
            passed: clip_passed,
        });
    }

    let aggregate_wer = if total_wer_ref_words > 0 {
        Some(round4_f64(
            total_wer_edits as f64 / total_wer_ref_words as f64,
        ))
    } else {
        None
    };
    let aggregate_cer = if total_cer_ref_chars > 0 {
        Some(round4_f64(
            total_cer_edits as f64 / total_cer_ref_chars as f64,
        ))
    } else {
        None
    };
    let mean_script_preservation = if script_ratios.is_empty() {
        None
    } else {
        Some(round4_f64(
            script_ratios.iter().sum::<f64>() / script_ratios.len() as f64,
        ))
    };

    let category_breakdown = build_category_breakdown(&clip_reports);
    let clean_english_wer = category_breakdown
        .iter()
        .find(|b| b.category == "clean_english")
        .and_then(|b| b.wer);
    let accented_english_wer = category_breakdown
        .iter()
        .find(|b| b.category == "accented_english")
        .and_then(|b| b.wer);
    let multilingual_wer = category_breakdown
        .iter()
        .find(|b| b.category == "multilingual_code_switch")
        .and_then(|b| b.wer);

    rtf_samples.sort_by(f64::total_cmp);
    latency_samples.sort_by(f64::total_cmp);

    let (status, status_banner) = if asr_fn.is_none() {
        (
            RealVoiceStatus::Unverified,
            UNVERIFIED_NO_ASR_STATUS.to_owned(),
        )
    } else if clip_reports.iter().all(|c| c.passed) {
        (
            RealVoiceStatus::Pass,
            format!(
                "EMPIRICAL REAL-AUDIO GATE: PASS ({}/{} real human WAVs passed)",
                clip_reports.len(),
                clip_reports.len()
            ),
        )
    } else {
        let passed_count = clip_reports.iter().filter(|c| c.passed).count();
        (
            RealVoiceStatus::Fail,
            format!(
                "EMPIRICAL REAL-AUDIO GATE: FAIL ({passed_count}/{} real human WAVs passed)",
                clip_reports.len()
            ),
        )
    };

    Ok(RealVoiceReport {
        evaluation_tier: EvaluationTier::EmpiricalRealAudio,
        status,
        status_banner,
        clips_evaluated: clip_reports.len(),
        labeled_clips,
        unlabeled_clips,
        long_chunked_clips,
        long_chunked_passed,
        total_audio_duration_s: round2_f64(total_audio_duration_s),
        aggregate_wer,
        aggregate_cer,
        clean_english_wer,
        accented_english_wer,
        multilingual_wer,
        category_breakdown,
        mean_script_preservation,
        silence_hallucination_clips,
        brand_leak_clips,
        wer_gate_failed_clips,
        perturbation_passed,
        perturbation_total,
        rtf_p50: percentile_f64(&rtf_samples, 0.50).map(round4_f64),
        rtf_p95: percentile_f64(&rtf_samples, 0.95).map(round4_f64),
        latency_ms_p50: percentile_f64(&latency_samples, 0.50).map(round2_f64),
        latency_ms_p95: percentile_f64(&latency_samples, 0.95).map(round2_f64),
        clips: clip_reports,
    })
}

fn build_category_breakdown(clips: &[RealVoiceClipReport]) -> Vec<CategoryWerBreakdown> {
    const BUCKETS: &[(&str, Option<f64>)] = &[
        ("clean_english", Some(MAX_CLEAN_ENGLISH_WER)),
        ("accented_english", Some(MAX_ACCENTED_ENGLISH_WER)),
        ("multilingual_code_switch", Some(MAX_MULTILINGUAL_WER)),
        ("silence_breath", None),
        ("noisy_room", Some(MAX_NOISY_OR_UNLABELED_WER)),
        ("unlabeled", Some(MAX_NOISY_OR_UNLABELED_WER)),
    ];

    let mut out = Vec::new();
    for &(bucket, ceiling) in BUCKETS {
        let matching: Vec<&RealVoiceClipReport> = clips
            .iter()
            .filter(|c| c.category.summary_bucket() == bucket)
            .collect();
        if matching.is_empty() {
            continue;
        }
        let clips_evaluated = matching.len();
        let clips_passed = matching.iter().filter(|c| c.passed).count();
        let labeled_clips = matching.iter().filter(|c| c.has_reference).count();
        let mut wer_edits = 0usize;
        let mut wer_ref = 0usize;
        let mut cer_edits = 0usize;
        let mut cer_ref = 0usize;
        for c in &matching {
            if let Some(ref w) = c.wer {
                wer_edits += w.edits;
                wer_ref += w.reference_len;
            }
            if let Some(ref ch) = c.cer {
                cer_edits += ch.edits;
                cer_ref += ch.reference_len;
            }
        }
        let wer = (wer_ref > 0).then(|| round4_f64(wer_edits as f64 / wer_ref as f64));
        let cer = (cer_ref > 0).then(|| round4_f64(cer_edits as f64 / cer_ref as f64));
        out.push(CategoryWerBreakdown {
            category: bucket.to_owned(),
            clips_evaluated,
            clips_passed,
            labeled_clips,
            wer,
            cer,
            max_allowed_wer: ceiling,
        });
    }
    out
}

/// Formats a `RealVoiceReport` as privacy-safe Markdown.
///
/// Raw user transcript sentences are never printed; redacted diagnostic tokens
/// appear only when `verbose_redacted` is `true`.
pub fn format_real_voice_markdown(report: &RealVoiceReport, verbose_redacted: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## lowyap-eval Real-Voice Benchmark\n");
    let _ = writeln!(out, "**Status:** {}\n", report.status_banner);

    if report.clips_evaluated == 0 {
        let _ = writeln!(
            out,
            "Provide real 16 kHz (or resampled) `.wav` recordings via `--default-corpus`, `--wav-dir <DIR>`, or `--manifest <JSON/TOML>` together with `--asr-cmd <BIN>`."
        );
        return out;
    }

    let _ = writeln!(
        out,
        "- **Clips evaluated:** {} ({} labeled, {} unlabeled, {:.2} s total audio)",
        report.clips_evaluated,
        report.labeled_clips,
        report.unlabeled_clips,
        report.total_audio_duration_s
    );
    if report.long_chunked_clips > 0 {
        let _ = writeln!(
            out,
            "- **Live-chunker (`>= 14 s` `LiveDecode::Chunked`) clips:** {}/{} passed",
            report.long_chunked_passed, report.long_chunked_clips
        );
    }
    let _ = writeln!(
        out,
        "- **Labeled WER / CER (overall):** {} / {}",
        report.aggregate_wer.map_or_else(
            || "n/a (unlabeled)".to_owned(),
            |v| format!("{:.2}%", v * 100.0)
        ),
        report.aggregate_cer.map_or_else(
            || "n/a (unlabeled)".to_owned(),
            |v| format!("{:.2}%", v * 100.0)
        )
    );
    let _ = writeln!(
        out,
        "- **Per-category WER:** `clean_english`={} | `accented_english`={} | `multilingual/code_switch`={}",
        report
            .clean_english_wer
            .map_or_else(|| "n/a".to_owned(), |v| format!("{:.2}%", v * 100.0)),
        report
            .accented_english_wer
            .map_or_else(|| "n/a".to_owned(), |v| format!("{:.2}%", v * 100.0)),
        report
            .multilingual_wer
            .map_or_else(|| "n/a".to_owned(), |v| format!("{:.2}%", v * 100.0)),
    );
    if let Some(sp) = report.mean_script_preservation {
        let _ = writeln!(
            out,
            "- **Code-switch script preservation:** {:.2}%",
            sp * 100.0
        );
    }
    let _ = writeln!(
        out,
        "- **Silence/breath hallucinations:** {} clip(s) | **SV-95 `LOWYAP` brand leaks:** {} clip(s) | **WER ceiling failures:** {} clip(s)",
        report.silence_hallucination_clips, report.brand_leak_clips, report.wer_gate_failed_clips
    );
    let _ = writeln!(
        out,
        "- **Acoustic perturbation stability:** {}/{} checks passed",
        report.perturbation_passed, report.perturbation_total
    );
    let _ = writeln!(
        out,
        "- **Latency p50 / p95:** {} / {} | **RTF p50 / p95:** {} / {}\n",
        report
            .latency_ms_p50
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:.1} ms")),
        report
            .latency_ms_p95
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:.1} ms")),
        report
            .rtf_p50
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:.3}x")),
        report
            .rtf_p95
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:.3}x")),
    );

    if !report.category_breakdown.is_empty() {
        out.push_str("### Category Breakdown\n\n");
        out.push_str(
            "| Category | Clips (Pass/Total) | Labeled WER | Labeled CER | Gate Ceiling |\n",
        );
        out.push_str("|---|---:|---:|---:|---|\n");
        for row in &report.category_breakdown {
            let wer_cell = row
                .wer
                .map_or_else(|| "—".to_owned(), |v| format!("{:.2}%", v * 100.0));
            let cer_cell = row
                .cer
                .map_or_else(|| "—".to_owned(), |v| format!("{:.2}%", v * 100.0));
            let gate_cell = row.max_allowed_wer.map_or_else(
                || "0 hallucinations".to_owned(),
                |m| format!("WER <= {:.0}%", m * 100.0),
            );
            let _ = writeln!(
                out,
                "| `{}` | {}/{} | {} | {} | {} |",
                row.category, row.clips_passed, row.clips_evaluated, wer_cell, cer_cell, gate_cell
            );
        }
        out.push('\n');
    }

    if verbose_redacted {
        out.push_str(
            "| Clip ID | Category | Lang | Dur (s) | WER | CER | Latency / RTF | Silence / Phantom | Brand Leak | Stress (3x) | Gate | Redacted Preview |\n",
        );
        out.push_str("|---|---|---|---:|---:|---:|---|---|---|---|---|---|\n");
    } else {
        out.push_str(
            "| Clip ID | Category | Lang | Dur (s) | WER | CER | Latency / RTF | Silence / Phantom | Brand Leak | Stress (3x) | Gate |\n",
        );
        out.push_str("|---|---|---|---:|---:|---:|---|---|---|---|---|\n");
    }

    for clip in &report.clips {
        let lang_str = clip.language_hint.as_deref().unwrap_or("auto");
        let dur_label = if clip.is_long_chunked {
            format!("{:.2} (chunked)", clip.duration_ms as f64 / 1_000.0)
        } else {
            format!("{:.2}", clip.duration_ms as f64 / 1_000.0)
        };
        let wer_str = clip
            .wer
            .as_ref()
            .map_or_else(|| "—".to_owned(), |w| format!("{:.1}%", w.rate * 100.0));
        let cer_str = clip
            .cer
            .as_ref()
            .map_or_else(|| "—".to_owned(), |c| format!("{:.1}%", c.rate * 100.0));
        let lat_rtf = match (clip.latency_ms, clip.rtf) {
            (Some(ms), Some(r)) => format!("{ms:.1} ms / {r:.3}x"),
            _ => "—".to_owned(),
        };
        let phantom_count = clip.silence_slice_hallucinations.len() + clip.phantom_flags.len();
        let brand_str = clip
            .brand_leak
            .as_ref()
            .map_or_else(|| "0".to_owned(), |b| format!("{:?}", b.reason));
        let stress_ok = clip.perturbations.iter().filter(|p| p.passed).count();
        let stress_str = format!("{stress_ok}/{}", clip.perturbations.len());
        let gate_str = if clip.passed {
            "PASS"
        } else if report.status == RealVoiceStatus::Unverified {
            "UNVERIFIED"
        } else {
            "FAIL"
        };

        if verbose_redacted {
            let preview = clip.redacted_preview.as_deref().unwrap_or("—");
            let _ = writeln!(
                out,
                "| `{}` | `{}` | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                clip.clip_id,
                clip.category.as_str(),
                lang_str,
                dur_label,
                wer_str,
                cer_str,
                lat_rtf,
                phantom_count,
                brand_str,
                stress_str,
                gate_str,
                preview
            );
        } else {
            let _ = writeln!(
                out,
                "| `{}` | `{}` | `{}` | {} | {} | {} | {} | {} | {} | {} | {} |",
                clip.clip_id,
                clip.category.as_str(),
                lang_str,
                dur_label,
                wer_str,
                cer_str,
                lat_rtf,
                phantom_count,
                brand_str,
                stress_str,
                gate_str
            );
        }
    }

    out
}

fn percentile_f64(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 * p).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    Some(sorted[idx])
}

fn round2_f64(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round4_f64(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn round4_f32(v: f32) -> f32 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::{extract_asr_stdout_text, parse_asr_stdout};

    #[test]
    fn extracts_json_text_and_best_ms_after_tauri_plugin_log_lines() {
        let stdout = concat!(
            "[2026-11-23][01:57:15][lowyap_app_lib::settings][INFO] Loaded settings\n",
            "[2026-11-23][01:57:16][lowyap_app_lib::cli][INFO] Transcribe finished [REDACTED]\n",
            "{\"audio_secs\":3.35,\"load_ms\":412,\"transcribe_ms\":[810,787],\"best_ms\":787,\"text\":\"Please send the transcription summary before Friday afternoon.\"}\n"
        );
        let parsed = parse_asr_stdout(stdout);
        assert_eq!(
            parsed.text,
            "Please send the transcription summary before Friday afternoon."
        );
        assert_eq!(parsed.decode_ms, Some(787.0));
        assert_eq!(
            extract_asr_stdout_text(stdout),
            "Please send the transcription summary before Friday afternoon."
        );
    }

    #[test]
    fn falls_back_to_transcribe_ms_sum_when_best_ms_absent() {
        let stdout = "{\"transcribe_ms\":[120.5,279.5],\"text\":\"chunk one chunk two\"}\n";
        let parsed = parse_asr_stdout(stdout);
        assert_eq!(parsed.text, "chunk one chunk two");
        assert_eq!(parsed.decode_ms, Some(400.0));
    }

    #[test]
    fn strips_tauri_plugin_log_lines_from_plain_stdout() {
        let stdout = concat!(
            "[2026-11-23][01:57:15][lowyap_app_lib::settings][INFO] Loaded settings\n",
            "Hello from plain stdout\n"
        );
        let parsed = parse_asr_stdout(stdout);
        assert_eq!(parsed.text, "Hello from plain stdout");
        assert_eq!(parsed.decode_ms, None);
    }
}
