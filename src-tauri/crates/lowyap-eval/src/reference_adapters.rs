//! Deterministic reference and baseline implementations of `lowyap_ports` traits.
//!
//! These adapters let `lowyap-eval` benchmark Wave 3 baseline heuristics against
//! modular target architectures (and future OSS adapters such as `aec3`,
//! `SileroVad`, `sherpa-rs`, and `llama-cpp-2`) through one shared runner.

use std::collections::VecDeque;
use std::fmt;

use lowyap_flow::{guard_transcript_text, pcm_rms_energy, HindustaniScript, SilenceGuardContext};
use lowyap_ports::{
    AsrProvider, AsrResult, EchoCanceller, HybridNotesRequest, MeetingTemplate, NoteSummarizer,
    OutputRouteKind, PortError, SpeakerChannel, SpeakerDiarizer, SpeakerIdentity, SummaryOutput,
    TranscriptSegment, UtteranceSegmenter, UtteranceSpan, WindowSummaryRequest,
};

/// Canonical sample rate for all 16 kHz mono audio frames.
pub const SAMPLE_RATE_16K: usize = 16_000;

/// 20 ms frame size at 16 kHz.
pub const FRAME_SAMPLES_16K: usize = 320;

/// Computes the root-mean-square amplitude of a slice.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

/// Computes the maximum absolute sample amplitude of a slice.
pub fn peak_abs(samples: &[f32]) -> f32 {
    samples
        .iter()
        .copied()
        .map(f32::abs)
        .fold(0.0_f32, f32::max)
}

/// Converts a decibel level relative to full scale (`dBFS`) to linear amplitude.
pub fn dbfs_to_linear(dbfs: f32) -> f32 {
    10.0_f32.powf(dbfs / 20.0)
}

/// Normalized Pearson-like correlation magnitude between two equal-length frames.
pub fn normalized_correlation(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut a_sq = 0.0_f64;
    let mut b_sq = 0.0_f64;
    for (&lhs, &rhs) in a.iter().zip(b.iter()) {
        let l = f64::from(lhs);
        let r = f64::from(rhs);
        dot += l * r;
        a_sq += l * l;
        b_sq += r * r;
    }
    if a_sq <= f64::EPSILON || b_sq <= f64::EPSILON {
        return 0.0;
    }
    (dot / (a_sq.sqrt() * b_sq.sqrt())).abs() as f32
}

/// Normalized amplitude-envelope correlation between two equal-length frames.
pub fn normalized_envelope_correlation(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut a_sq = 0.0_f64;
    let mut b_sq = 0.0_f64;
    for (&lhs, &rhs) in a.iter().zip(b.iter()) {
        let l = f64::from(lhs.abs());
        let r = f64::from(rhs.abs());
        dot += l * r;
        a_sq += l * l;
        b_sq += r * r;
    }
    if a_sq <= f64::EPSILON || b_sq <= f64::EPSILON {
        return 0.0;
    }
    (dot / (a_sq.sqrt() * b_sq.sqrt())) as f32
}

/// Estimates fundamental frequency (`F0` in Hz) over active voiced frames via
/// first-peak normalized autocorrelation in `[80 Hz, 320 Hz]`.
pub fn estimate_pitch_hz(samples: &[f32]) -> Option<f32> {
    const WINDOW: usize = 640; // 40 ms
    const MIN_LAG: usize = 50; // 320 Hz
    const MAX_LAG: usize = 200; // 80 Hz

    if samples.len() < WINDOW {
        return None;
    }

    let mut pitches = Vec::new();
    for start in (0..=samples.len() - WINDOW).step_by(WINDOW) {
        let win = &samples[start..start + WINDOW];
        if rms(win) < 0.01 {
            continue;
        }
        let mut corrs = [0.0_f32; MAX_LAG + 1];
        let mut max_corr = 0.0_f32;
        for lag in MIN_LAG..=MAX_LAG {
            let n = WINDOW - lag;
            let mut dot = 0.0_f64;
            let mut e1 = 0.0_f64;
            let mut e2 = 0.0_f64;
            for i in 0..n {
                let a = f64::from(win[i]);
                let b = f64::from(win[i + lag]);
                dot += a * b;
                e1 += a * a;
                e2 += b * b;
            }
            let c = if e1 > 1e-12 && e2 > 1e-12 {
                (dot / (e1.sqrt() * e2.sqrt())) as f32
            } else {
                0.0
            };
            corrs[lag] = c;
            if c > max_corr {
                max_corr = c;
            }
        }
        if max_corr < 0.45 {
            continue;
        }
        // Pick the first strong local peak to avoid octave-halving errors.
        let threshold = max_corr * 0.85;
        let mut chosen_lag = None;
        for lag in (MIN_LAG + 1)..MAX_LAG {
            if corrs[lag] >= threshold
                && corrs[lag] >= corrs[lag - 1]
                && corrs[lag] >= corrs[lag + 1]
            {
                chosen_lag = Some(lag);
                break;
            }
        }
        if let Some(lag) = chosen_lag {
            pitches.push(SAMPLE_RATE_16K as f32 / lag as f32);
        }
    }

    if pitches.is_empty() {
        return None;
    }
    pitches.sort_by(|a, b| a.total_cmp(b));
    Some(pitches[pitches.len() / 2])
}

/// Computes a coarse spectral centroid (Hz) over active frames using a small
/// Goertzel/DFT probe bank between 100 Hz and 3200 Hz.
pub fn estimate_spectral_centroid_hz(samples: &[f32]) -> f32 {
    const WINDOW: usize = 512;
    const BINS: usize = 32;
    if samples.len() < WINDOW {
        return 0.0;
    }

    let mut weighted_sum = 0.0_f64;
    let mut mag_sum = 0.0_f64;

    // Sample up to 6 active windows spread evenly across the span.
    let stride = ((samples.len() - WINDOW) / 6).max(WINDOW);
    let mut count = 0usize;
    for start in (0..=samples.len() - WINDOW).step_by(stride) {
        let win = &samples[start..start + WINDOW];
        if rms(win) < 0.008 {
            continue;
        }
        for bin in 1..=BINS {
            let freq = (bin as f64) * 100.0;
            let omega = 2.0 * std::f64::consts::PI * freq / (SAMPLE_RATE_16K as f64);
            let mut re = 0.0_f64;
            let mut im = 0.0_f64;
            for (i, &s) in win.iter().enumerate() {
                let angle = omega * (i as f64);
                let sample = f64::from(s);
                re += sample * angle.cos();
                im -= sample * angle.sin();
            }
            let mag = (re * re + im * im).sqrt();
            weighted_sum += freq * mag;
            mag_sum += mag;
        }
        count += 1;
        if count >= 6 {
            break;
        }
    }

    if mag_sum <= f64::EPSILON {
        0.0
    } else {
        (weighted_sum / mag_sum) as f32
    }
}

// ============================================================================
// Echo Cancellation Adapters
// ============================================================================

/// Baseline echo canceller ported from Wave 3's `SpeakerEchoGuard`
/// (`src-tauri/src/meeting/aec_guard.rs`).
///
/// Uses 20 ms frame-aligned delay search with linear waveform + envelope
/// correlation gating. Struggles on sub-frame delays (e.g. 45 ms) combined with
/// loudspeaker nonlinearity.
#[derive(Debug, Default)]
pub struct CorrelationGateEchoCanceller {
    others_history: VecDeque<Vec<f32>>,
}

impl CorrelationGateEchoCanceller {
    const MAX_DELAY_FRAMES: usize = 9; // 180 ms
    const ACTIVE_RMS_FLOOR: f32 = 0.008;
    const ACTIVE_PEAK_FLOOR: f32 = 0.03;
    const CORRELATION_MIN: f32 = 0.78;
    const NEAR_END_BURST_MULTIPLIER: f32 = 1.35;
    const BLEED_RATIO_LIMIT: f32 = 0.85;

    /// Creates a new correlation-gate baseline echo canceller.
    pub fn new() -> Self {
        Self {
            others_history: VecDeque::with_capacity(Self::MAX_DELAY_FRAMES + 1),
        }
    }

    fn push_others_history(&mut self, others_16k: &[f32]) {
        for frame_start in (0..others_16k.len()).step_by(FRAME_SAMPLES_16K) {
            let frame_end = (frame_start + FRAME_SAMPLES_16K).min(others_16k.len());
            let mut frame = vec![0.0_f32; FRAME_SAMPLES_16K];
            let len = frame_end - frame_start;
            frame[..len].copy_from_slice(&others_16k[frame_start..frame_end]);
            self.push_history_frame(frame);
        }
    }

    fn push_history_frame(&mut self, frame: Vec<f32>) {
        self.others_history.push_back(frame);
        while self.others_history.len() > (Self::MAX_DELAY_FRAMES + 1) {
            self.others_history.pop_front();
        }
    }

    fn best_delayed_match(&self, me_frame: &[f32]) -> (f32, f32) {
        let mut best_corr = 0.0_f32;
        let mut peak_rms = 0.0_f32;

        for delay in 0..=Self::MAX_DELAY_FRAMES {
            let Some(candidate) = self
                .others_history
                .len()
                .checked_sub(delay + 1)
                .and_then(|idx| self.others_history.get(idx))
            else {
                continue;
            };

            let wave_corr = normalized_correlation(me_frame, candidate);
            let env_corr = normalized_envelope_correlation(me_frame, candidate);
            let combined = (wave_corr * 0.45) + (env_corr * 0.55);
            best_corr = best_corr.max(combined);
            peak_rms = peak_rms.max(rms(candidate));
        }

        (best_corr, peak_rms)
    }
}

impl EchoCanceller for CorrelationGateEchoCanceller {
    fn name(&self) -> &'static str {
        "correlation_gate_baseline"
    }

    fn process_16k(
        &mut self,
        me_16k: &[f32],
        others_ref_16k: &[f32],
        route: OutputRouteKind,
    ) -> Vec<f32> {
        if me_16k.is_empty() {
            self.push_others_history(others_ref_16k);
            return Vec::new();
        }

        if route == OutputRouteKind::Headphones {
            self.push_others_history(others_ref_16k);
            return me_16k.to_vec();
        }

        let target_len = me_16k.len().max(others_ref_16k.len());
        let mut me = me_16k.to_vec();
        me.resize(target_len, 0.0);
        let mut others = others_ref_16k.to_vec();
        others.resize(target_len, 0.0);

        let mut output = vec![0.0_f32; target_len];

        for frame_start in (0..target_len).step_by(FRAME_SAMPLES_16K) {
            let frame_end = (frame_start + FRAME_SAMPLES_16K).min(target_len);
            let mut me_frame = vec![0.0_f32; FRAME_SAMPLES_16K];
            let mut others_frame = vec![0.0_f32; FRAME_SAMPLES_16K];
            let frame_len = frame_end - frame_start;
            me_frame[..frame_len].copy_from_slice(&me[frame_start..frame_end]);
            others_frame[..frame_len].copy_from_slice(&others[frame_start..frame_end]);

            self.push_history_frame(others_frame.clone());

            let me_rms = rms(&me_frame);
            let me_peak = peak_abs(&me_frame);
            let others_active = rms(&others_frame) > Self::ACTIVE_RMS_FLOOR
                || peak_abs(&others_frame) > Self::ACTIVE_PEAK_FLOOR;

            let (best_corr, others_peak_rms) = self.best_delayed_match(&me_frame);
            let expected_bleed = others_peak_rms * Self::BLEED_RATIO_LIMIT;
            let near_end_burst = me_peak > expected_bleed * Self::NEAR_END_BURST_MULTIPLIER;
            let unknown_route_without_strong_others =
                route == OutputRouteKind::Unknown && others_peak_rms < Self::ACTIVE_RMS_FLOOR;

            let should_suppress = !unknown_route_without_strong_others
                && others_active
                && best_corr >= Self::CORRELATION_MIN
                && me_rms < others_peak_rms * Self::BLEED_RATIO_LIMIT
                && !near_end_burst;

            let gain = if should_suppress {
                if best_corr > 0.92 && me_rms < others_peak_rms * 0.65 {
                    0.0
                } else {
                    let knee = ((best_corr - Self::CORRELATION_MIN)
                        / (0.95 - Self::CORRELATION_MIN))
                        .clamp(0.0, 1.0);
                    ((1.0 - knee).powi(2) * 0.35).clamp(0.0, 0.35)
                }
            } else {
                1.0
            };

            for idx in 0..frame_len {
                output[frame_start + idx] = me[frame_start + idx] * gain;
            }
        }

        output.truncate(me_16k.len());
        output
    }

    fn reset(&mut self) {
        self.others_history.clear();
    }
}

/// Delay-compensated adaptive NLMS echo canceller with Volterra nonlinear taps
/// (linear + quadratic + cubic) and a double-talk detector.
///
/// Serves as the modular-target adaptive AEC reference until a native OSS crate
/// such as `aec3` is wired into the desktop app.
#[derive(Debug)]
pub struct NlmsEchoCanceller {
    ref_history: Vec<f32>,
    estimated_delay: Option<usize>,
    fixed_delay: Option<usize>,
    weights: Vec<f32>,
    step_size: f32,
    converged_frames: usize,
    frames_seen: usize,
    oracle_mode: bool,
}

impl Default for NlmsEchoCanceller {
    fn default() -> Self {
        Self::new()
    }
}

impl NlmsEchoCanceller {
    const MAX_SEARCH_DELAY: usize = 2_880; // 180 ms
    const MAX_HISTORY: usize = 4_096; // 256 ms
    const LINEAR_TAPS: usize = 80; // 5 ms span covering direct + early reflections
    const LINEAR_LEAD: usize = 8; // start 8 samples before bulk delay
    const QUAD_TAPS: usize = 16; // 2nd-harmonic loudspeaker distortion
    const CUBIC_TAPS: usize = 16; // odd-order clipping / tanh distortion
    const TOTAL_TAPS: usize = Self::LINEAR_TAPS + Self::QUAD_TAPS + Self::CUBIC_TAPS;

    /// Creates a blind adaptive NLMS echo canceller (estimates room delay online).
    pub fn new() -> Self {
        Self {
            ref_history: vec![0.0; Self::MAX_HISTORY],
            estimated_delay: None,
            fixed_delay: None,
            weights: vec![0.0; Self::TOTAL_TAPS],
            step_size: 0.45,
            converged_frames: 0,
            frames_seen: 0,
            oracle_mode: false,
        }
    }

    fn new_oracle(delay_samples: usize) -> Self {
        Self {
            ref_history: vec![0.0; Self::MAX_HISTORY],
            estimated_delay: Some(delay_samples),
            fixed_delay: Some(delay_samples),
            weights: vec![0.0; Self::TOTAL_TAPS],
            step_size: 0.65,
            converged_frames: 0,
            frames_seen: 0,
            oracle_mode: true,
        }
    }

    fn estimate_delay(
        &self,
        me_frame: &[f32],
        ref_stream: &[f32],
        frame_base: usize,
    ) -> Option<usize> {
        let max_lag = Self::MAX_SEARCH_DELAY.min(frame_base);
        if max_lag < 16 || rms(me_frame) < 0.008 {
            return None;
        }

        let eval_lag = |lag: usize| -> f32 {
            let start = frame_base - lag;
            let candidate = &ref_stream[start..start + me_frame.len()];
            if rms(candidate) < 0.008 {
                return 0.0;
            }
            let wave = normalized_correlation(me_frame, candidate);
            let env = normalized_envelope_correlation(me_frame, candidate);
            0.70 * wave + 0.30 * env
        };

        // Coarse pass (step 4 samples = 0.25 ms).
        let mut best_lag = 0usize;
        let mut best_score = 0.0_f32;
        for lag in (0..=max_lag).step_by(4) {
            let score = eval_lag(lag);
            if score > best_score {
                best_score = score;
                best_lag = lag;
            }
        }

        // Fine pass around best coarse lag.
        let lo = best_lag.saturating_sub(4);
        let hi = (best_lag + 4).min(max_lag);
        for lag in lo..=hi {
            let score = eval_lag(lag);
            if score > best_score {
                best_score = score;
                best_lag = lag;
            }
        }

        if best_score >= 0.45 {
            Some(best_lag)
        } else {
            None
        }
    }

    fn build_feature_vec(ref_stream: &[f32], sample_idx: usize, delay: usize, out: &mut [f32]) {
        let base_lag = delay.saturating_sub(Self::LINEAR_LEAD);
        for (k, slot) in out.iter_mut().take(Self::LINEAR_TAPS).enumerate() {
            let lag = base_lag + k;
            *slot = if sample_idx >= lag {
                ref_stream[sample_idx - lag]
            } else {
                0.0
            };
        }

        let nl_base = delay.saturating_sub(2);
        let q_offset = Self::LINEAR_TAPS;
        for k in 0..Self::QUAD_TAPS {
            let lag = nl_base + k;
            let x = if sample_idx >= lag {
                ref_stream[sample_idx - lag]
            } else {
                0.0
            };
            out[q_offset + k] = x * x;
        }

        let c_offset = q_offset + Self::QUAD_TAPS;
        for k in 0..Self::CUBIC_TAPS {
            let lag = nl_base + k;
            let x = if sample_idx >= lag {
                ref_stream[sample_idx - lag]
            } else {
                0.0
            };
            out[c_offset + k] = x * x * x;
        }
    }
}

impl EchoCanceller for NlmsEchoCanceller {
    fn name(&self) -> &'static str {
        if self.oracle_mode {
            "ideal_oracle_echo_canceller"
        } else {
            "nlms_adaptive_aec"
        }
    }

    fn process_16k(
        &mut self,
        me_16k: &[f32],
        others_ref_16k: &[f32],
        route: OutputRouteKind,
    ) -> Vec<f32> {
        let len = me_16k.len();
        if len == 0 {
            if !others_ref_16k.is_empty() {
                self.ref_history.extend_from_slice(others_ref_16k);
                if self.ref_history.len() > Self::MAX_HISTORY {
                    let drop_n = self.ref_history.len() - Self::MAX_HISTORY;
                    self.ref_history.drain(..drop_n);
                }
            }
            return Vec::new();
        }

        // Build contiguous reference stream = history + aligned current others chunk.
        let hist_len = self.ref_history.len();
        let mut ref_stream = Vec::with_capacity(hist_len + len);
        ref_stream.extend_from_slice(&self.ref_history);
        let copy_n = others_ref_16k.len().min(len);
        ref_stream.extend_from_slice(&others_ref_16k[..copy_n]);
        if copy_n < len {
            ref_stream.resize(hist_len + len, 0.0);
        }

        let result = if route == OutputRouteKind::Headphones || others_ref_16k.is_empty() {
            me_16k.to_vec()
        } else {
            let mut output = vec![0.0_f32; len];
            let mut feat = [0.0_f32; Self::TOTAL_TAPS];
            let mut pred_buf = [0.0_f32; FRAME_SAMPLES_16K];
            let mut err_buf = [0.0_f32; FRAME_SAMPLES_16K];

            for frame_start in (0..len).step_by(FRAME_SAMPLES_16K) {
                let frame_end = (frame_start + FRAME_SAMPLES_16K).min(len);
                let frame_len = frame_end - frame_start;
                let me_frame = &me_16k[frame_start..frame_end];
                let ref_frame_base = hist_len + frame_start;
                self.frames_seen += 1;

                // Estimate bulk delay on startup or periodically while unconverged.
                if self.fixed_delay.is_none()
                    && (self.estimated_delay.is_none()
                        || (self.converged_frames < 4 && self.frames_seen.is_multiple_of(2)))
                {
                    if let Some(lag) = self.estimate_delay(me_frame, &ref_stream, ref_frame_base) {
                        if self.estimated_delay != Some(lag) {
                            self.estimated_delay = Some(lag);
                            self.weights.fill(0.0);
                            self.converged_frames = 0;
                        }
                    }
                }

                let Some(delay) = self.estimated_delay else {
                    output[frame_start..frame_end].copy_from_slice(me_frame);
                    continue;
                };

                let delayed_start = ref_frame_base.saturating_sub(delay);
                let delayed_end = (delayed_start + frame_len).min(ref_stream.len());
                let ref_rms = rms(&ref_stream[delayed_start..delayed_end]);
                let me_rms = rms(me_frame);

                if ref_rms < 0.005 {
                    output[frame_start..frame_end].copy_from_slice(me_frame);
                    continue;
                }

                // Dry-run prediction with frozen weights to detect double-talk cleanly.
                for i in 0..frame_len {
                    Self::build_feature_vec(&ref_stream, ref_frame_base + i, delay, &mut feat);
                    let mut y_hat = 0.0_f32;
                    for (w, x) in self.weights.iter().zip(feat.iter()) {
                        y_hat += w * x;
                    }
                    pred_buf[i] = y_hat;
                    err_buf[i] = me_frame[i] - y_hat;
                }

                let dry_err_rms = rms(&err_buf[..frame_len]);
                let dry_pred_rms = rms(&pred_buf[..frame_len]);
                let near_end_present = if self.converged_frames >= 4 {
                    dry_err_rms > 0.32 * me_rms && dry_err_rms > 0.025
                } else {
                    me_rms > ref_rms * 1.15
                };

                if !near_end_present {
                    // Single-talk remote echo: adapt NLMS weights and apply residual post-filter.
                    let passes = if self.oracle_mode && self.converged_frames < 3 {
                        3
                    } else {
                        1
                    };
                    for _ in 0..passes {
                        for i in 0..frame_len {
                            Self::build_feature_vec(
                                &ref_stream,
                                ref_frame_base + i,
                                delay,
                                &mut feat,
                            );
                            let mut y_hat = 0.0_f32;
                            let mut norm_sq = 1e-4_f32;
                            for (w, x) in self.weights.iter().zip(feat.iter()) {
                                y_hat += w * x;
                                norm_sq += x * x;
                            }
                            let err = me_frame[i] - y_hat;
                            let scale = self.step_size * err / norm_sq;
                            for (w, x) in self.weights.iter_mut().zip(feat.iter()) {
                                *w = (*w + scale * x).clamp(-4.0, 4.0);
                            }
                            err_buf[i] = err;
                        }
                    }

                    let post_err_rms = rms(&err_buf[..frame_len]);
                    if post_err_rms < 0.35 * me_rms {
                        self.converged_frames = self.converged_frames.saturating_add(1);
                    }

                    // Residual echo suppressor only engages when single-talk echo cancellation
                    // already achieved significant ERLE.
                    let res_gain = if post_err_rms < 0.25 * me_rms {
                        let ratio = (post_err_rms / (me_rms + 1e-6)).clamp(0.0, 1.0);
                        (ratio * ratio * 0.5).clamp(0.0, 0.25)
                    } else if self.oracle_mode && dry_pred_rms > 0.005 {
                        0.02
                    } else {
                        1.0
                    };

                    for i in 0..frame_len {
                        output[frame_start + i] = err_buf[i] * res_gain;
                    }
                } else {
                    // Double-talk: freeze weights and emit subtracted near-end signal intact.
                    output[frame_start..frame_end].copy_from_slice(&err_buf[..frame_len]);
                }
            }
            output
        };

        // Update trailing reference history.
        let keep_start = ref_stream.len().saturating_sub(Self::MAX_HISTORY);
        self.ref_history.clear();
        self.ref_history
            .extend_from_slice(&ref_stream[keep_start..]);

        result
    }

    fn reset(&mut self) {
        self.ref_history.fill(0.0);
        self.estimated_delay = self.fixed_delay;
        self.weights.fill(0.0);
        self.converged_frames = 0;
        self.frames_seen = 0;
    }
}

/// Oracle echo canceller seeded with known room delay (`45 ms` by default) to
/// establish an upper-bound benchmark reference alongside `CorrelationGateEchoCanceller`
/// and blind `NlmsEchoCanceller`.
#[derive(Debug)]
pub struct IdealOracleEchoCanceller {
    inner: NlmsEchoCanceller,
}

impl Default for IdealOracleEchoCanceller {
    fn default() -> Self {
        Self::with_delay_ms(45)
    }
}

impl IdealOracleEchoCanceller {
    /// Creates an oracle echo canceller seeded with the exact acoustic delay in milliseconds.
    pub fn with_delay_ms(delay_ms: usize) -> Self {
        let delay_samples = delay_ms * SAMPLE_RATE_16K / 1_000;
        Self {
            inner: NlmsEchoCanceller::new_oracle(delay_samples),
        }
    }
}

impl EchoCanceller for IdealOracleEchoCanceller {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn process_16k(
        &mut self,
        me_16k: &[f32],
        others_ref_16k: &[f32],
        route: OutputRouteKind,
    ) -> Vec<f32> {
        self.inner.process_16k(me_16k, others_ref_16k, route)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

// ============================================================================
// Utterance Segmenter Adapters
// ============================================================================

#[derive(Debug, Default)]
struct FixedChannelBuffer {
    samples: Vec<f32>,
    start_ms: Option<u64>,
}

/// Baseline fixed-window segmenter (`10 s` = `160_000` samples per span).
///
/// Reproduces Wave 3's fixed slicing behavior, emitting silent windows verbatim
/// into ASR.
#[derive(Debug)]
pub struct FixedWindowSegmenter {
    window_samples: usize,
    me: FixedChannelBuffer,
    others: FixedChannelBuffer,
}

impl Default for FixedWindowSegmenter {
    fn default() -> Self {
        Self::new(10 * SAMPLE_RATE_16K)
    }
}

impl FixedWindowSegmenter {
    /// Creates a fixed-window segmenter with the given window length in samples.
    pub fn new(window_samples: usize) -> Self {
        Self {
            window_samples: window_samples.max(FRAME_SAMPLES_16K),
            me: FixedChannelBuffer::default(),
            others: FixedChannelBuffer::default(),
        }
    }

    fn buffer_mut(&mut self, channel: SpeakerChannel) -> &mut FixedChannelBuffer {
        match channel {
            SpeakerChannel::Me => &mut self.me,
            SpeakerChannel::Others => &mut self.others,
        }
    }
}

impl UtteranceSegmenter for FixedWindowSegmenter {
    fn name(&self) -> &'static str {
        "fixed_10s_segmenter"
    }

    fn push_chunk(
        &mut self,
        channel: SpeakerChannel,
        samples_16k: &[f32],
        chunk_start_ms: u64,
    ) -> Vec<UtteranceSpan> {
        if samples_16k.is_empty() {
            return Vec::new();
        }
        let window_samples = self.window_samples;
        let buf = self.buffer_mut(channel);
        if buf.start_ms.is_none() {
            buf.start_ms = Some(chunk_start_ms);
        }
        buf.samples.extend_from_slice(samples_16k);

        let mut spans = Vec::new();
        while buf.samples.len() >= window_samples {
            let chunk: Vec<f32> = buf.samples.drain(..window_samples).collect();
            let start_ms = buf.start_ms.unwrap_or(chunk_start_ms);
            let duration_ms = chunk.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
            let end_ms = start_ms.saturating_add(duration_ms);
            buf.start_ms = if buf.samples.is_empty() {
                None
            } else {
                Some(end_ms)
            };
            spans.push(UtteranceSpan {
                channel,
                samples_16k: chunk,
                start_ms,
                end_ms,
            });
        }
        spans
    }

    fn flush(&mut self) -> Vec<UtteranceSpan> {
        let mut spans = Vec::new();
        for channel in [SpeakerChannel::Me, SpeakerChannel::Others] {
            let buf = self.buffer_mut(channel);
            if !buf.samples.is_empty() {
                let start_ms = buf.start_ms.unwrap_or(0);
                let duration_ms = buf.samples.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
                let samples_16k = std::mem::take(&mut buf.samples);
                spans.push(UtteranceSpan {
                    channel,
                    samples_16k,
                    start_ms,
                    end_ms: start_ms.saturating_add(duration_ms),
                });
            }
            buf.start_ms = None;
        }
        spans
    }
}

#[derive(Debug, Default)]
struct VadChannelState {
    in_speech: bool,
    consecutive_active_frames: usize,
    silent_hangover_frames: usize,
    pending_onset: Vec<f32>,
    pending_onset_start_ms: Option<u64>,
    active_samples: Vec<f32>,
    active_start_ms: u64,
    frame_remainder: Vec<f32>,
    remainder_start_ms: Option<u64>,
}

/// Energy + hangover speech-turn segmenter that drops silent frames below
/// `-45 dBFS` so ASR never receives empty 10-second silence windows.
#[derive(Debug)]
pub struct EnergyVadSegmenter {
    threshold_rms: f32,
    onset_frames: usize,
    hangover_frames: usize,
    max_utterance_samples: usize,
    me: VadChannelState,
    others: VadChannelState,
}

impl Default for EnergyVadSegmenter {
    fn default() -> Self {
        Self::new(-45.0, 3, 20, 15 * SAMPLE_RATE_16K)
    }
}

impl EnergyVadSegmenter {
    /// Creates an energy-based VAD segmenter.
    ///
    /// - `threshold_dbfs`: frames with RMS below this level are treated as silence (default `-45.0`).
    /// - `onset_frames`: consecutive active 20 ms frames required to open an utterance.
    /// - `hangover_frames`: trailing silent 20 ms frames before closing an utterance.
    /// - `max_utterance_samples`: cap for long monologues.
    pub fn new(
        threshold_dbfs: f32,
        onset_frames: usize,
        hangover_frames: usize,
        max_utterance_samples: usize,
    ) -> Self {
        Self {
            threshold_rms: dbfs_to_linear(threshold_dbfs),
            onset_frames: onset_frames.max(1),
            hangover_frames: hangover_frames.max(1),
            max_utterance_samples: max_utterance_samples.max(FRAME_SAMPLES_16K * 5),
            me: VadChannelState::default(),
            others: VadChannelState::default(),
        }
    }

    fn state_mut(&mut self, channel: SpeakerChannel) -> &mut VadChannelState {
        match channel {
            SpeakerChannel::Me => &mut self.me,
            SpeakerChannel::Others => &mut self.others,
        }
    }

    fn emit_active(channel: SpeakerChannel, state: &mut VadChannelState) -> Option<UtteranceSpan> {
        if state.active_samples.is_empty() {
            state.in_speech = false;
            return None;
        }
        let samples_16k = std::mem::take(&mut state.active_samples);
        let start_ms = state.active_start_ms;
        let duration_ms = samples_16k.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
        state.in_speech = false;
        state.consecutive_active_frames = 0;
        state.silent_hangover_frames = 0;
        Some(UtteranceSpan {
            channel,
            samples_16k,
            start_ms,
            end_ms: start_ms.saturating_add(duration_ms),
        })
    }
}

impl UtteranceSegmenter for EnergyVadSegmenter {
    fn name(&self) -> &'static str {
        "energy_vad_segmenter"
    }

    fn push_chunk(
        &mut self,
        channel: SpeakerChannel,
        samples_16k: &[f32],
        chunk_start_ms: u64,
    ) -> Vec<UtteranceSpan> {
        if samples_16k.is_empty() {
            return Vec::new();
        }
        let threshold_rms = self.threshold_rms;
        let onset_frames = self.onset_frames;
        let hangover_frames = self.hangover_frames;
        let max_utterance_samples = self.max_utterance_samples;
        let state = self.state_mut(channel);

        if state.remainder_start_ms.is_none() {
            state.remainder_start_ms = Some(chunk_start_ms);
        }
        state.frame_remainder.extend_from_slice(samples_16k);

        let mut spans = Vec::new();
        let mut cursor_ms = state.remainder_start_ms.unwrap_or(chunk_start_ms);

        while state.frame_remainder.len() >= FRAME_SAMPLES_16K {
            let frame: Vec<f32> = state.frame_remainder.drain(..FRAME_SAMPLES_16K).collect();
            let frame_ms = cursor_ms;
            cursor_ms = cursor_ms.saturating_add(20);
            let is_active = rms(&frame) >= threshold_rms;

            if !state.in_speech {
                if is_active {
                    if state.pending_onset_start_ms.is_none() {
                        state.pending_onset_start_ms = Some(frame_ms);
                    }
                    state.pending_onset.extend_from_slice(&frame);
                    state.consecutive_active_frames += 1;
                    if state.consecutive_active_frames >= onset_frames {
                        state.in_speech = true;
                        state.active_start_ms = state.pending_onset_start_ms.unwrap_or(frame_ms);
                        state.active_samples.append(&mut state.pending_onset);
                        state.pending_onset_start_ms = None;
                        state.silent_hangover_frames = 0;
                    }
                } else {
                    state.pending_onset.clear();
                    state.pending_onset_start_ms = None;
                    state.consecutive_active_frames = 0;
                }
            } else {
                state.active_samples.extend_from_slice(&frame);
                if is_active {
                    state.silent_hangover_frames = 0;
                } else {
                    state.silent_hangover_frames += 1;
                }

                if state.silent_hangover_frames >= hangover_frames
                    || state.active_samples.len() >= max_utterance_samples
                {
                    if let Some(span) = Self::emit_active(channel, state) {
                        spans.push(span);
                    }
                }
            }
        }

        state.remainder_start_ms = if state.frame_remainder.is_empty() {
            None
        } else {
            Some(cursor_ms)
        };

        spans
    }

    fn flush(&mut self) -> Vec<UtteranceSpan> {
        let mut spans = Vec::new();
        let threshold_rms = self.threshold_rms;
        for channel in [SpeakerChannel::Me, SpeakerChannel::Others] {
            let state = self.state_mut(channel);
            if state.in_speech {
                if !state.frame_remainder.is_empty() && rms(&state.frame_remainder) >= threshold_rms
                {
                    state
                        .active_samples
                        .extend_from_slice(&state.frame_remainder);
                }
                if let Some(span) = Self::emit_active(channel, state) {
                    spans.push(span);
                }
            }
            state.frame_remainder.clear();
            state.remainder_start_ms = None;
            state.pending_onset.clear();
            state.pending_onset_start_ms = None;
            state.consecutive_active_frames = 0;
        }
        spans
    }
}

// ============================================================================
// ASR Provider Adapter
// ============================================================================

/// Deterministic Whisper-like ASR simulator.
///
/// Emits structured speech tokens for voiced spans and reproduces Whisper's
/// `"Thank you."` / cross-script hallucination when fed `>= 5 s` of near-silent
/// (`< 0.002` RMS) audio.
#[derive(Debug, Default)]
pub struct SyntheticWhisperAsrSim;

impl SyntheticWhisperAsrSim {
    const SILENCE_RMS_LIMIT: f32 = 0.002;
    const HALLUCINATION_MIN_SAMPLES: usize = 5 * SAMPLE_RATE_16K; // 5 s
    const SPEECH_FRAME_RMS: f32 = 0.008;

    /// Creates a new deterministic Whisper ASR simulator.
    pub fn new() -> Self {
        Self
    }

    /// Returns `true` when `text` matches a known Whisper silence hallucination.
    pub fn is_known_hallucination(text: &str) -> bool {
        let normalized = text.trim();
        matches!(
            normalized,
            "Thank you." | "Thank you for watching." | "Спасибо." | "شکریہ۔"
        )
    }
}

impl AsrProvider for SyntheticWhisperAsrSim {
    fn name(&self) -> &'static str {
        "synthetic_whisper_asr_sim"
    }

    fn transcribe_16k(
        &self,
        samples_16k: &[f32],
        language_hint: Option<&str>,
    ) -> Result<AsrResult, PortError> {
        if samples_16k.is_empty() {
            return Ok(AsrResult {
                text: String::new(),
                detected_language: language_hint.map(str::to_owned),
                no_speech_prob: Some(1.0),
            });
        }

        let span_rms = rms(samples_16k);
        let mut total_frames = 0usize;
        let mut active_frames = 0usize;
        let mut max_contiguous_silent_samples = 0usize;
        let mut cur_silent_samples = 0usize;

        for chunk in samples_16k.chunks(FRAME_SAMPLES_16K) {
            total_frames += 1;
            let frame_rms = rms(chunk);
            if frame_rms >= Self::SPEECH_FRAME_RMS {
                active_frames += 1;
                cur_silent_samples = 0;
            } else if frame_rms < Self::SILENCE_RMS_LIMIT {
                cur_silent_samples += chunk.len();
                max_contiguous_silent_samples =
                    max_contiguous_silent_samples.max(cur_silent_samples);
            } else {
                cur_silent_samples = 0;
            }
        }

        let active_ratio = if total_frames == 0 {
            0.0
        } else {
            active_frames as f32 / total_frames as f32
        };

        // Reproduce Whisper's silence hallucination trap on >= 5s near-silent audio.
        let near_silent_span = (span_rms < Self::SILENCE_RMS_LIMIT
            && samples_16k.len() >= Self::HALLUCINATION_MIN_SAMPLES)
            || (active_ratio < 0.03
                && max_contiguous_silent_samples >= Self::HALLUCINATION_MIN_SAMPLES);

        if near_silent_span {
            let text = match language_hint {
                Some("ru") => "Спасибо.",
                Some("ur") => "شکریہ۔",
                _ => "Thank you.",
            };
            return Ok(AsrResult {
                text: text.to_owned(),
                detected_language: Some(language_hint.unwrap_or("en").to_owned()),
                no_speech_prob: Some(0.89),
            });
        }

        if active_ratio < 0.03 || span_rms < 0.005 {
            return Ok(AsrResult {
                text: String::new(),
                detected_language: language_hint.map(str::to_owned),
                no_speech_prob: Some(0.95),
            });
        }

        let pitch_hz = estimate_pitch_hz(samples_16k).unwrap_or(160.0);
        let duration_ms = samples_16k.len() as u64 * 1_000 / SAMPLE_RATE_16K as u64;
        let band = if pitch_hz < 150.0 {
            "low_voice"
        } else if pitch_hz < 190.0 {
            "mid_voice"
        } else {
            "high_voice"
        };

        Ok(AsrResult {
            text: format!("speech_turn_{band}_{duration_ms}ms"),
            detected_language: Some(language_hint.unwrap_or("en").to_owned()),
            no_speech_prob: Some(0.04),
        })
    }
}

/// Production-guarded ASR adapter wrapping any [`AsrProvider`] with
/// [`lowyap_flow::guard_transcript_text`] using audio RMS energy and decoder
/// no-speech probability to strip silence, music, and cross-script phantoms.
pub struct FlowGuardedAsrAdapter {
    inner: Box<dyn AsrProvider>,
    spoken_languages: Vec<String>,
    script_pref: HindustaniScript,
}

impl fmt::Debug for FlowGuardedAsrAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlowGuardedAsrAdapter")
            .field("inner", &self.inner.name())
            .field("spoken_languages", &self.spoken_languages)
            .field("script_pref", &self.script_pref)
            .finish()
    }
}

impl Default for FlowGuardedAsrAdapter {
    fn default() -> Self {
        Self::new(Box::new(SyntheticWhisperAsrSim::new()))
    }
}

impl FlowGuardedAsrAdapter {
    /// Wraps `inner` with `lowyap-flow` transcript guards configured for
    /// English, Hindi, and Urdu (`HindustaniScript::Auto`).
    pub fn new(inner: Box<dyn AsrProvider>) -> Self {
        Self::with_profile(
            inner,
            vec!["en".to_owned(), "hi".to_owned(), "ur".to_owned()],
            HindustaniScript::Auto,
        )
    }

    /// Wraps `inner` with an explicit spoken-language profile and script preference.
    pub fn with_profile(
        inner: Box<dyn AsrProvider>,
        spoken_languages: Vec<String>,
        script_pref: HindustaniScript,
    ) -> Self {
        Self {
            inner,
            spoken_languages,
            script_pref,
        }
    }
}

impl AsrProvider for FlowGuardedAsrAdapter {
    fn name(&self) -> &'static str {
        "flow_guarded_asr"
    }

    fn transcribe_16k(
        &self,
        samples_16k: &[f32],
        language_hint: Option<&str>,
    ) -> Result<AsrResult, PortError> {
        let mut result = self.inner.transcribe_16k(samples_16k, language_hint)?;
        let ctx = SilenceGuardContext {
            rms_energy: Some(pcm_rms_energy(samples_16k)),
            no_speech_prob: result.no_speech_prob,
            mean_token_prob: None,
            output_was_translated: false,
        };
        result.text =
            guard_transcript_text(&result.text, &self.spoken_languages, self.script_pref, ctx);
        Ok(result)
    }
}

// ============================================================================
// Speaker Diarizer Adapters
// ============================================================================

/// Baseline channel-only diarizer (Wave 3 behavior): labels mic as `Me` and all
/// system-audio turns as a single undifferentiated `Others` speaker.
#[derive(Debug, Default)]
pub struct ChannelOnlyDiarizer;

impl SpeakerDiarizer for ChannelOnlyDiarizer {
    fn name(&self) -> &'static str {
        "channel_only_baseline"
    }

    fn observe_me_clean(&mut self, _clean_me_16k: &[f32]) {}

    fn identify(&mut self, span: &UtteranceSpan) -> SpeakerIdentity {
        SpeakerIdentity {
            channel: span.channel,
            speaker_id: span.channel.as_str().to_owned(),
            display_label: span.channel.display_label().to_owned(),
            confidence: Some(0.5),
        }
    }

    fn reset(&mut self) {}
}

#[derive(Clone, Debug)]
struct VoiceProfile {
    pitch_hz: f32,
    centroid_hz: f32,
    count: usize,
}

impl VoiceProfile {
    fn distance(&self, pitch_hz: f32, centroid_hz: f32) -> f32 {
        let pitch_diff = (self.pitch_hz - pitch_hz).abs() / 35.0;
        let centroid_diff = (self.centroid_hz - centroid_hz).abs() / 450.0;
        0.75 * pitch_diff + 0.25 * centroid_diff
    }

    fn update(&mut self, pitch_hz: f32, centroid_hz: f32) {
        let n = self.count as f32;
        self.pitch_hz = (self.pitch_hz * n + pitch_hz) / (n + 1.0);
        self.centroid_hz = (self.centroid_hz * n + centroid_hz) / (n + 1.0);
        self.count += 1;
    }
}

/// Deterministic channel + `ME_ONLY` enrollment diarizer.
///
/// Learns the local speaker's pitch/spectral centroid from clean `ME_ONLY`
/// windows and clusters distinct remote speakers on `Others` (`Speaker 1`,
/// `Speaker 2`, ...).
#[derive(Debug, Default)]
pub struct ChannelPlusEnrollmentDiarizer {
    me_profile: Option<VoiceProfile>,
    remote_clusters: Vec<VoiceProfile>,
}

impl ChannelPlusEnrollmentDiarizer {
    const CLUSTER_DISTANCE_THRESHOLD: f32 = 1.0;

    /// Creates a new enrollment + remote-clustering diarizer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether a local `ME_ONLY` voice profile has been enrolled.
    pub fn is_me_enrolled(&self) -> bool {
        self.me_profile.is_some()
    }

    /// Returns the number of discovered remote speaker clusters.
    pub fn remote_cluster_count(&self) -> usize {
        self.remote_clusters.len()
    }

    fn extract_features(samples: &[f32]) -> Option<(f32, f32)> {
        let pitch = estimate_pitch_hz(samples)?;
        let centroid = estimate_spectral_centroid_hz(samples);
        Some((pitch, centroid))
    }
}

impl SpeakerDiarizer for ChannelPlusEnrollmentDiarizer {
    fn name(&self) -> &'static str {
        "channel_plus_enrollment_diarizer"
    }

    fn observe_me_clean(&mut self, clean_me_16k: &[f32]) {
        if rms(clean_me_16k) < 0.01 {
            return;
        }
        let Some((pitch, centroid)) = Self::extract_features(clean_me_16k) else {
            return;
        };
        match &mut self.me_profile {
            Some(profile) => profile.update(pitch, centroid),
            None => {
                self.me_profile = Some(VoiceProfile {
                    pitch_hz: pitch,
                    centroid_hz: centroid,
                    count: 1,
                });
            }
        }
    }

    fn identify(&mut self, span: &UtteranceSpan) -> SpeakerIdentity {
        let features = Self::extract_features(&span.samples_16k);

        match span.channel {
            SpeakerChannel::Me => {
                let confidence = match (&self.me_profile, features) {
                    (Some(profile), Some((pitch, centroid))) => {
                        let dist = profile.distance(pitch, centroid);
                        (1.0 - dist * 0.25).clamp(0.55, 0.98)
                    }
                    (Some(_), None) => 0.75,
                    (None, _) => 0.65,
                };
                SpeakerIdentity {
                    channel: SpeakerChannel::Me,
                    speaker_id: "me".to_owned(),
                    display_label: "Me".to_owned(),
                    confidence: Some(confidence),
                }
            }
            SpeakerChannel::Others => {
                let Some((pitch, centroid)) = features else {
                    return SpeakerIdentity {
                        channel: SpeakerChannel::Others,
                        speaker_id: "others".to_owned(),
                        display_label: "Others".to_owned(),
                        confidence: Some(0.5),
                    };
                };

                let mut best_idx = None;
                let mut best_dist = f32::MAX;
                for (idx, cluster) in self.remote_clusters.iter().enumerate() {
                    let dist = cluster.distance(pitch, centroid);
                    if dist < best_dist {
                        best_dist = dist;
                        best_idx = Some(idx);
                    }
                }

                let (cluster_idx, confidence) = if let (Some(idx), true) =
                    (best_idx, best_dist <= Self::CLUSTER_DISTANCE_THRESHOLD)
                {
                    self.remote_clusters[idx].update(pitch, centroid);
                    (idx, (1.0 - best_dist * 0.35).clamp(0.6, 0.97))
                } else {
                    self.remote_clusters.push(VoiceProfile {
                        pitch_hz: pitch,
                        centroid_hz: centroid,
                        count: 1,
                    });
                    (self.remote_clusters.len() - 1, 0.85)
                };

                let number = cluster_idx + 1;
                SpeakerIdentity {
                    channel: SpeakerChannel::Others,
                    speaker_id: format!("speaker_{number}"),
                    display_label: format!("Speaker {number}"),
                    confidence: Some(confidence),
                }
            }
        }
    }

    fn reset(&mut self) {
        self.me_profile = None;
        self.remote_clusters.clear();
    }
}

// ============================================================================
// Note Summarizer Adapter
// ============================================================================

/// Deterministic extractive summarizer enforcing a strict `max_input_chars`
/// budget and preserving multi-script terms across English, Roman Hindi/Urdu,
/// and Arabic/Urdu script.
#[derive(Debug, Clone)]
pub struct ExtractiveBudgetSummarizer {
    max_chars: usize,
}

impl Default for ExtractiveBudgetSummarizer {
    fn default() -> Self {
        Self::new(12_000)
    }
}

impl ExtractiveBudgetSummarizer {
    /// Creates an extractive summarizer with the given character budget.
    pub fn new(max_chars: usize) -> Self {
        Self {
            max_chars: max_chars.max(512),
        }
    }

    /// Computes total input character count (Unicode scalar values) for a window request.
    pub fn window_input_chars(req: &WindowSummaryRequest) -> usize {
        let mut total = req
            .language_instruction
            .as_deref()
            .map_or(0, |s| s.chars().count());
        for seg in &req.segments {
            total += seg.speaker.display_label.chars().count() + seg.text.chars().count() + 4;
        }
        total
    }

    /// Computes total input character count (Unicode scalar values) for a hybrid notes request.
    pub fn hybrid_input_chars(req: &HybridNotesRequest) -> usize {
        let mut total = req.title.chars().count()
            + req.rough_notes.chars().count()
            + req
                .language_instruction
                .as_deref()
                .map_or(0, |s| s.chars().count());
        for rollup in &req.rollups {
            total += rollup.chars().count() + 2;
        }
        for seg in &req.tail_segments {
            total += seg.speaker.display_label.chars().count() + seg.text.chars().count() + 4;
        }
        total
    }

    fn truncate_chars(text: &str, max_chars: usize) -> String {
        let mut out = String::new();
        for (idx, ch) in text.chars().enumerate() {
            if idx >= max_chars {
                out.push('…');
                break;
            }
            out.push(ch);
        }
        out
    }

    fn has_arabic_urdu_script(text: &str) -> bool {
        text.chars().any(|c| {
            ('\u{0600}'..='\u{06FF}').contains(&c) || ('\u{0750}'..='\u{077F}').contains(&c)
        })
    }

    fn is_salient(seg: &TranscriptSegment) -> bool {
        let lower = seg.text.to_lowercase();
        lower.contains("action")
            || lower.contains("ship")
            || lower.contains("decision")
            || lower.contains("demo")
            || lower.contains("blocker")
            || lower.contains("target")
            || Self::has_arabic_urdu_script(&seg.text)
    }

    fn template_heading(template: MeetingTemplate) -> &'static str {
        match template {
            MeetingTemplate::General => "General Notes",
            MeetingTemplate::OneOnOne => "1:1 Notes",
            MeetingTemplate::Standup => "Standup Notes",
            MeetingTemplate::LanguageLesson => "Language Lesson Notes",
            MeetingTemplate::ClientCall => "Client Call Notes",
        }
    }
}

impl NoteSummarizer for ExtractiveBudgetSummarizer {
    fn name(&self) -> &'static str {
        "extractive_budget_summarizer"
    }

    fn max_input_chars(&self) -> usize {
        self.max_chars
    }

    fn summarize_window(&self, req: &WindowSummaryRequest) -> Result<SummaryOutput, PortError> {
        let input_chars = Self::window_input_chars(req);
        if input_chars > self.max_chars {
            return Err(format!(
                "window {} exceeded max_input_chars ({input_chars} > {})",
                req.window_index, self.max_chars
            ));
        }

        let mut selected: Vec<&TranscriptSegment> = req
            .segments
            .iter()
            .filter(|s| Self::is_salient(s))
            .collect();
        if selected.is_empty() {
            selected = req.segments.iter().take(3).collect();
        }

        // Preserve at most 4 concise bullets per window while ensuring multi-script
        // lines (English, Roman Hindi/Urdu, Arabic/Urdu) remain represented.
        let mut lines = Vec::new();
        lines.push(format!("### Window {}", req.window_index + 1));

        let mut picked_arabic = false;
        for seg in selected.iter().take(3) {
            if Self::has_arabic_urdu_script(&seg.text) {
                picked_arabic = true;
            }
            lines.push(format!(
                "- **{}**: {}",
                seg.speaker.display_label,
                Self::truncate_chars(&seg.text, 120)
            ));
        }
        if !picked_arabic {
            if let Some(ar_seg) = req
                .segments
                .iter()
                .find(|s| Self::has_arabic_urdu_script(&s.text))
            {
                lines.push(format!(
                    "- **{}**: {}",
                    ar_seg.speaker.display_label,
                    Self::truncate_chars(&ar_seg.text, 120)
                ));
            }
        }

        Ok(SummaryOutput {
            markdown: lines.join("\n"),
            source_id: self.name().to_owned(),
        })
    }

    fn expand_hybrid_notes(&self, req: &HybridNotesRequest) -> Result<SummaryOutput, PortError> {
        let input_chars = Self::hybrid_input_chars(req);
        if input_chars > self.max_chars {
            return Err(format!(
                "hybrid notes request exceeded max_input_chars ({input_chars} > {})",
                self.max_chars
            ));
        }

        let mut md = String::new();
        md.push_str(&format!(
            "# {} ({})\n\n",
            req.title,
            Self::template_heading(req.template)
        ));

        if let Some(instr) = &req.language_instruction {
            md.push_str(&format!("_Language policy: {instr}_\n\n"));
        }

        if !req.rough_notes.trim().is_empty() {
            md.push_str("## User Rough Notes\n");
            md.push_str(req.rough_notes.trim());
            md.push_str("\n\n");
        }

        if !req.rollups.is_empty() {
            md.push_str("## Rolling Window Highlights\n");
            for rollup in &req.rollups {
                md.push_str(rollup.trim());
                md.push('\n');
            }
            md.push('\n');
        }

        if !req.tail_segments.is_empty() {
            md.push_str("## Recent Tail Highlights\n");
            for seg in req.tail_segments.iter().take(6) {
                md.push_str(&format!(
                    "- **{}**: {}\n",
                    seg.speaker.display_label,
                    Self::truncate_chars(&seg.text, 140)
                ));
            }
        }

        Ok(SummaryOutput {
            markdown: md,
            source_id: self.name().to_owned(),
        })
    }
}
