//! Software Noise-Gated RMS Auto-Gain Control (`NoiseGatedAgc`) ahead of VAD
//! and ASR.
//!
//! Boosts quiet speech (`-48 dBFS .. -22 dBFS`, such as low OS mic sliders or
//! fixed-gain Bluetooth/AirPods microphones) into Silero VAD and Whisper's
//! reliable operating band while gating dead silence / laptop fan hiss below
//! `-52 dBFS` (`rms < 0.0025`) and soft-limiting transients below `±0.98`.

use anyhow::Result;

use super::{check_whole_frames, SpeechScorer, VadFrame, VadTailReport, VoiceActivityDetector};

/// Target speech RMS (`~ -21.9 dBFS`).
pub const DEFAULT_TARGET_RMS: f32 = 0.08;
/// Noise-gate RMS floor (`~ -48.4 dBFS`). Frames quieter than this decay toward
/// unity gain (`1.0x`) so laptop room/fan hiss (`~0.0024` RMS) is never amplified
/// into VAD triggers while quiet speech (`>= 0.005` RMS) still boosts cleanly.
pub const DEFAULT_NOISE_GATE_RMS: f32 = 0.0038;
/// Minimum gain (`0 dB`): normal/loud speech is never attenuated.
pub const DEFAULT_MIN_GAIN: f32 = 1.0;
/// Maximum boost (`+15.56 dB`), lifting `-45.4 dBFS` (`rms ~ 0.0054`) to
/// `rms ~ 0.032` (`-29.8 dBFS`).
pub const DEFAULT_MAX_GAIN: f32 = 6.0;
/// Per-frame one-pole smoothing factor when gain is increasing.
pub const DEFAULT_ATTACK_COEFF: f32 = 0.35;
/// Per-frame one-pole smoothing factor when gain is decaying back toward `1.0`.
pub const DEFAULT_RELEASE_COEFF: f32 = 0.08;
/// Linear threshold above which soft-knee peak limiting engages.
pub const SOFT_LIMIT_KNEE: f32 = 0.85;
/// Asymptotic output ceiling so boosted plosives never hard-clip.
pub const SOFT_LIMIT_CEILING: f32 = 0.98;

/// Tunable parameters for [`NoiseGatedAgc`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgcConfig {
    pub target_rms: f32,
    pub noise_gate_rms: f32,
    pub min_gain: f32,
    pub max_gain: f32,
    pub attack_coeff: f32,
    pub release_coeff: f32,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            target_rms: DEFAULT_TARGET_RMS,
            noise_gate_rms: DEFAULT_NOISE_GATE_RMS,
            min_gain: DEFAULT_MIN_GAIN,
            max_gain: DEFAULT_MAX_GAIN,
            attack_coeff: DEFAULT_ATTACK_COEFF,
            release_coeff: DEFAULT_RELEASE_COEFF,
        }
    }
}

/// Stateful per-frame noise-gated RMS automatic gain controller with sample-level
/// gain interpolation and $C^1$-smooth `tanh` peak limiting.
#[derive(Debug, Clone)]
pub struct NoiseGatedAgc {
    target_rms: f32,
    noise_gate_rms: f32,
    min_gain: f32,
    max_gain: f32,
    attack_coeff: f32,
    release_coeff: f32,
    current_gain: f32,
}

impl Default for NoiseGatedAgc {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseGatedAgc {
    pub const fn new() -> Self {
        Self {
            target_rms: DEFAULT_TARGET_RMS,
            noise_gate_rms: DEFAULT_NOISE_GATE_RMS,
            min_gain: DEFAULT_MIN_GAIN,
            max_gain: DEFAULT_MAX_GAIN,
            attack_coeff: DEFAULT_ATTACK_COEFF,
            release_coeff: DEFAULT_RELEASE_COEFF,
            current_gain: DEFAULT_MIN_GAIN,
        }
    }

    pub fn with_config(config: AgcConfig) -> Self {
        let min_gain = if config.min_gain.is_finite() && config.min_gain > 0.0 {
            config.min_gain
        } else {
            DEFAULT_MIN_GAIN
        };
        let max_gain = if config.max_gain.is_finite() && config.max_gain >= min_gain {
            config.max_gain
        } else {
            DEFAULT_MAX_GAIN.max(min_gain)
        };
        let target_rms = if config.target_rms.is_finite() && config.target_rms > 0.0 {
            config.target_rms
        } else {
            DEFAULT_TARGET_RMS
        };
        let noise_gate_rms = if config.noise_gate_rms.is_finite() && config.noise_gate_rms >= 0.0 {
            config.noise_gate_rms
        } else {
            DEFAULT_NOISE_GATE_RMS
        };
        let attack_coeff = if config.attack_coeff.is_finite() {
            config.attack_coeff.clamp(0.0, 1.0)
        } else {
            DEFAULT_ATTACK_COEFF
        };
        let release_coeff = if config.release_coeff.is_finite() {
            config.release_coeff.clamp(0.0, 1.0)
        } else {
            DEFAULT_RELEASE_COEFF
        };

        Self {
            target_rms,
            noise_gate_rms,
            min_gain,
            max_gain,
            attack_coeff,
            release_coeff,
            current_gain: min_gain,
        }
    }

    /// Current smoothed linear gain factor (`min_gain..=max_gain`).
    #[inline]
    pub fn current_gain(&self) -> f32 {
        self.current_gain
    }

    /// Resets smoothed gain state back to unity (`min_gain`) between recordings.
    #[inline]
    pub fn reset(&mut self) {
        self.current_gain = self.min_gain;
    }

    /// Applies noise-gated RMS auto-gain and soft-knee peak limiting in place.
    ///
    /// Gain ramps linearly across the frame from the previous frame's gain to
    /// the new target-smoothed gain so frame boundaries remain click-free.
    pub fn process_in_place(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }

        let rms = frame_rms(samples);
        let desired_gain = if rms < self.noise_gate_rms {
            self.min_gain
        } else {
            (self.target_rms / rms).clamp(self.min_gain, self.max_gain)
        };

        let coeff = if desired_gain > self.current_gain {
            self.attack_coeff
        } else {
            self.release_coeff
        };

        let start_gain = self.current_gain;
        let end_gain =
            (start_gain + coeff * (desired_gain - start_gain)).clamp(self.min_gain, self.max_gain);
        self.current_gain = end_gain;

        let inv_len = 1.0 / (samples.len() as f32);
        let delta_gain = end_gain - start_gain;

        for (idx, sample) in samples.iter_mut().enumerate() {
            let clean = if sample.is_finite() { *sample } else { 0.0 };
            let gain = start_gain + delta_gain * ((idx + 1) as f32 * inv_len);
            *sample = soft_limit(clean * gain);
        }
    }
}

/// Computes root-mean-square amplitude using an `f64` accumulator to avoid
/// `f32` underflow on quiet (`< -60 dBFS`) frames. Non-finite samples are treated as `0.0`.
pub fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sum_sq = 0.0_f64;
    for &sample in samples {
        if sample.is_finite() {
            let value = f64::from(sample);
            sum_sq += value * value;
        }
    }
    ((sum_sq / (samples.len() as f64)).sqrt()) as f32
}

/// $C^1$-continuous soft-knee peak limiter: linear up to `±0.85` and
/// asymptotically bounded at `±0.98` via `tanh`.
#[inline]
pub fn soft_limit(sample: f32) -> f32 {
    if sample.is_nan() {
        return 0.0;
    }
    let abs = sample.abs();
    if abs <= SOFT_LIMIT_KNEE {
        return sample;
    }
    let headroom = SOFT_LIMIT_CEILING - SOFT_LIMIT_KNEE;
    let excess = abs - SOFT_LIMIT_KNEE;
    let compressed = SOFT_LIMIT_KNEE + headroom * (excess / headroom).tanh();
    compressed.min(SOFT_LIMIT_CEILING).copysign(sample)
}

/// [`VoiceActivityDetector`] (and [`SpeechScorer`]) adapter that runs
/// [`NoiseGatedAgc`] ahead of an inner detector so both VAD decisions and
/// downstream ASR frames receive cleanly boosted audio.
pub struct AgcVad<V> {
    inner: V,
    agc: NoiseGatedAgc,
    scratch: Vec<f32>,
}

impl<V> AgcVad<V> {
    pub fn new(inner: V) -> Self {
        Self::with_agc(inner, NoiseGatedAgc::new())
    }

    pub fn with_agc(inner: V, agc: NoiseGatedAgc) -> Self {
        Self {
            inner,
            agc,
            scratch: Vec::new(),
        }
    }

    pub fn agc(&self) -> &NoiseGatedAgc {
        &self.agc
    }

    pub fn agc_mut(&mut self) -> &mut NoiseGatedAgc {
        &mut self.agc
    }

    pub fn inner(&self) -> &V {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut V {
        &mut self.inner
    }

    pub fn into_inner(self) -> V {
        self.inner
    }
}

impl<V: VoiceActivityDetector> VoiceActivityDetector for AgcVad<V> {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        let Self {
            inner,
            agc,
            scratch,
        } = self;
        scratch.clear();
        scratch.extend_from_slice(frame);
        agc.process_in_place(scratch);
        inner.push_frame(scratch)
    }

    fn frame_samples(&self) -> usize {
        self.inner.frame_samples()
    }

    fn set_hangover_frames(&mut self, frames: usize) {
        self.inner.set_hangover_frames(frames);
    }

    fn tail_report(&self) -> Option<VadTailReport> {
        self.inner.tail_report()
    }

    fn reset(&mut self) {
        self.agc.reset();
        self.inner.reset();
        self.scratch.clear();
    }
}

impl<V: SpeechScorer> SpeechScorer for AgcVad<V> {
    fn score(&mut self, frames: &[f32]) -> Result<Vec<f32>> {
        let frame_len = self.inner.frame_samples();
        check_whole_frames(frames, frame_len)?;

        let Self {
            inner,
            agc,
            scratch,
        } = self;
        scratch.clear();
        scratch.extend_from_slice(frames);
        for chunk in scratch.chunks_exact_mut(frame_len) {
            agc.process_in_place(chunk);
        }
        inner.score(scratch)
    }
}

impl VoiceActivityDetector for Box<dyn VoiceActivityDetector> {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        (**self).push_frame(frame)
    }

    fn frame_samples(&self) -> usize {
        (**self).frame_samples()
    }

    fn is_voice(&mut self, frame: &[f32]) -> Result<bool> {
        (**self).is_voice(frame)
    }

    fn set_hangover_frames(&mut self, frames: usize) {
        (**self).set_hangover_frames(frames);
    }

    fn tail_report(&self) -> Option<VadTailReport> {
        (**self).tail_report()
    }

    fn reset(&mut self) {
        (**self).reset();
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;

    const SILERO_FRAME_SAMPLES: usize = 512;
    const SAMPLE_RATE_HZ: f32 = 16_000.0;

    fn sine_frame(amplitude: f32, freq_hz: f32, frame_idx: usize, len: usize) -> Vec<f32> {
        let offset = frame_idx * len;
        (0..len)
            .map(|i| {
                let t = ((offset + i) as f32) / SAMPLE_RATE_HZ;
                amplitude * (TAU * freq_hz * t).sin()
            })
            .collect()
    }

    #[test]
    fn quiet_speech_minus_45_4_dbfs_boosts_over_4x_within_few_frames_without_clipping() {
        let mut agc = NoiseGatedAgc::new();
        // Peak 0.0076 -> RMS ~ 0.00537 (-45.4 dBFS).
        let amplitude = 0.0076_f32;
        let mut latest_ratio = 1.0_f32;

        for frame_idx in 0..5 {
            let raw = sine_frame(amplitude, 220.0, frame_idx, SILERO_FRAME_SAMPLES);
            let input_rms = frame_rms(&raw);
            let mut processed = raw;
            agc.process_in_place(&mut processed);
            let output_rms = frame_rms(&processed);
            latest_ratio = output_rms / input_rms;

            assert!(
                processed
                    .iter()
                    .all(|s| s.is_finite() && s.abs() <= SOFT_LIMIT_CEILING),
                "frame {frame_idx} exceeded soft limit ceiling"
            );

            // By frame index 2 (the 3rd 512-sample frame = 96 ms), effective gain exceeds 4x.
            if frame_idx == 2 {
                assert!(
                    latest_ratio > 4.0,
                    "expected > 4x boost by 3rd frame, got {latest_ratio:.3}x"
                );
            }
        }

        assert!(
            latest_ratio > 5.0 && latest_ratio <= DEFAULT_MAX_GAIN + 1e-4,
            "expected steady climb toward 6x, got {latest_ratio:.3}x"
        );
        assert!(agc.current_gain() > 5.0 && agc.current_gain() <= DEFAULT_MAX_GAIN);
    }

    #[test]
    fn quiet_noise_floor_minus_60_dbfs_stays_gated_near_unity() {
        let mut agc = NoiseGatedAgc::new();
        // Peak 0.001 -> RMS ~ 0.000707 (< 0.0025 gate).
        for frame_idx in 0..20 {
            let raw = sine_frame(0.001, 120.0, frame_idx, SILERO_FRAME_SAMPLES);
            let input_rms = frame_rms(&raw);
            let mut processed = raw;
            agc.process_in_place(&mut processed);
            let ratio = frame_rms(&processed) / input_rms;
            assert!(
                ratio < 1.15,
                "noise floor must stay gated near 1.0x (< 1.15x), got {ratio:.4}x"
            );
            assert!(agc.current_gain() < 1.15);
        }

        // Also verify decay back below 1.15x after prior quiet-speech boost stops.
        for frame_idx in 0..8 {
            let mut speech = sine_frame(0.0076, 220.0, frame_idx, SILERO_FRAME_SAMPLES);
            agc.process_in_place(&mut speech);
        }
        assert!(agc.current_gain() > 5.0);

        for frame_idx in 0..50 {
            let mut hiss = sine_frame(0.001, 120.0, frame_idx, SILERO_FRAME_SAMPLES);
            agc.process_in_place(&mut hiss);
        }
        assert!(
            agc.current_gain() < 1.15,
            "gain after silence/hiss tail should decay below 1.15x, got {:.4}x",
            agc.current_gain()
        );
    }

    #[test]
    fn normal_loud_speech_minus_12_dbfs_is_not_overboosted_and_transients_soft_limit() {
        let mut agc = NoiseGatedAgc::new();
        // Peak 0.35 -> RMS ~ 0.2475 (-12.1 dBFS).
        for frame_idx in 0..10 {
            let raw = sine_frame(0.35, 220.0, frame_idx, SILERO_FRAME_SAMPLES);
            let input_rms = frame_rms(&raw);
            let mut processed = raw.clone();
            agc.process_in_place(&mut processed);
            let ratio = frame_rms(&processed) / input_rms;

            assert!(
                (ratio - 1.0).abs() < 1e-5,
                "normal loud speech should stay at unity gain, got {ratio:.5}x"
            );
            assert!(processed
                .iter()
                .all(|s| s.is_finite() && s.abs() <= SOFT_LIMIT_CEILING));
        }

        // Sudden loud transient while gain is elevated at ~6x must never exceed ±0.98.
        for frame_idx in 0..10 {
            let mut quiet = sine_frame(0.0076, 220.0, frame_idx, SILERO_FRAME_SAMPLES);
            agc.process_in_place(&mut quiet);
        }
        assert!(agc.current_gain() > 5.5);

        let mut sudden_loud = sine_frame(0.35, 220.0, 10, SILERO_FRAME_SAMPLES);
        agc.process_in_place(&mut sudden_loud);
        let peak = sudden_loud.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        assert!(
            peak <= SOFT_LIMIT_CEILING,
            "soft limiter must bound transient at <= {SOFT_LIMIT_CEILING}, got {peak:.5}"
        );
        assert!(peak > SOFT_LIMIT_KNEE);
    }

    struct RmsThresholdVad {
        min_rms: f32,
    }

    impl VoiceActivityDetector for RmsThresholdVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            if frame_rms(frame) >= self.min_rms {
                Ok(VadFrame::Speech(frame))
            } else {
                Ok(VadFrame::Noise)
            }
        }

        fn frame_samples(&self) -> usize {
            SILERO_FRAME_SAMPLES
        }
    }

    #[test]
    fn agc_vad_adapter_rescues_quiet_speech_while_rejecting_noise_floor() {
        let mut raw_vad = RmsThresholdVad { min_rms: 0.02 };
        let mut agc_vad = AgcVad::new(RmsThresholdVad { min_rms: 0.02 });

        let quiet_frame = sine_frame(0.0076, 220.0, 0, SILERO_FRAME_SAMPLES);
        assert!(!raw_vad.push_frame(&quiet_frame).unwrap().is_speech());

        let mut detected_speech = false;
        for frame_idx in 0..5 {
            let f = sine_frame(0.0076, 220.0, frame_idx, SILERO_FRAME_SAMPLES);
            if let VadFrame::Speech(emitted) = agc_vad.push_frame(&f).unwrap() {
                detected_speech = true;
                assert!(frame_rms(emitted) >= 0.02);
            }
        }
        assert!(
            detected_speech,
            "AgcVad should lift -45.4 dBFS speech above VAD threshold"
        );

        agc_vad.reset();
        for frame_idx in 0..10 {
            let noise = sine_frame(0.001, 120.0, frame_idx, SILERO_FRAME_SAMPLES);
            assert!(!agc_vad.push_frame(&noise).unwrap().is_speech());
        }
    }
}
