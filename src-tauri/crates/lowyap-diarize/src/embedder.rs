//! Acoustic speaker voiceprint embeddings and the [`VoiceEmbedder`] hexagonal seam.

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use crate::SAMPLE_RATE_16K;

const FRAME_SAMPLES: usize = 400; // 25 ms at 16 kHz
const HOP_SAMPLES: usize = 160; // 10 ms at 16 kHz
const FFT_SIZE: usize = 512;
const FFT_BINS: usize = FFT_SIZE / 2 + 1; // 257 bins (0..=8000 Hz, 31.25 Hz spacing)

const MIN_ACTIVE_FRAMES: usize = 4; // ~55 ms minimum active speech
const ACTIVE_FRAME_RMS_FLOOR: f32 = 0.008;

const MIN_PITCH_HZ: f32 = 70.0;
const MAX_PITCH_HZ: f32 = 380.0;
const MIN_PITCH_LAG: usize = 42; // ~380.95 Hz at 16 kHz
const MAX_PITCH_LAG: usize = 228; // ~70.18 Hz at 16 kHz
const VOICED_CORR_THRESHOLD: f32 = 0.32;

const MEL_BANDS: usize = 24;
const MFCC_COEFFS: usize = 13; // c1..=c13
const CMN_MFCC_COEFFS: usize = 12; // c1..=c12
const GROUPED_MEL_BANDS: usize = 5;
const MEL_MIN_HZ: f32 = 80.0;
const MEL_MAX_HZ: f32 = 7_600.0;

const PITCH_DIM: usize = 10;
const SPECTRAL_SHAPE_DIM: usize = 8;
const EMBEDDING_DIM: usize =
    PITCH_DIM + SPECTRAL_SHAPE_DIM + MFCC_COEFFS + GROUPED_MEL_BANDS + CMN_MFCC_COEFFS; // 48

// Calibrated L2 block weights (squares sum to 1.0: 0.40 + 0.24 + 0.22 + 0.09 + 0.05 = 1.00).
const PITCH_BLOCK_WEIGHT: f32 = 0.632_455_5; // sqrt(0.40)
const SPECTRAL_BLOCK_WEIGHT: f32 = 0.489_897_94; // sqrt(0.24)
const MFCC_MEAN_WEIGHT: f32 = 0.469_041_6; // sqrt(0.22)
const MEL_GROUP_WEIGHT: f32 = 0.30; // sqrt(0.09)
const CMN_STD_WEIGHT: f32 = 0.223_606_8; // sqrt(0.05)

/// Unit L2-normalized speaker voiceprint extracted from 16 kHz mono audio.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoiceEmbedding {
    /// Unit L2-normalized embedding vector.
    pub vector: Vec<f32>,
    /// Count of voiced frames (`normalized autocorrelation peak >= 0.32`).
    pub voiced_frames: usize,
    /// Median fundamental frequency (`F0`) in Hz across voiced frames, when voiced speech is present.
    pub median_f0_hz: Option<f32>,
}

impl VoiceEmbedding {
    /// Computes cosine similarity in `[-1.0, 1.0]` between this voiceprint and `other`.
    ///
    /// Returns `0.0` if dimensions mismatch or either vector has near-zero norm.
    pub fn cosine_similarity(&self, other: &[f32]) -> f32 {
        if self.vector.len() != other.len() || self.vector.is_empty() {
            return 0.0;
        }
        let mut dot = 0.0_f64;
        let mut a_sq = 0.0_f64;
        let mut b_sq = 0.0_f64;
        for (&lhs, &rhs) in self.vector.iter().zip(other.iter()) {
            let l = f64::from(lhs);
            let r = f64::from(rhs);
            dot += l * r;
            a_sq += l * l;
            b_sq += r * r;
        }
        if a_sq <= 1e-24 || b_sq <= 1e-24 {
            return 0.0;
        }
        (dot / (a_sq.sqrt() * b_sq.sqrt())).clamp(-1.0, 1.0) as f32
    }
}

/// Hexagonal seam for extracting fixed-dimension speaker voiceprints from 16 kHz mono PCM.
///
/// Implementations may use pure-Rust DSP ([`AcousticMelVoiceEmbedder`]) or plug in
/// neural speaker encoders (`ECAPA-TDNN`, `WeSpeaker`, `CAM++`) behind the same trait.
pub trait VoiceEmbedder: Send + Sync {
    /// Stable adapter identifier.
    fn name(&self) -> &'static str;

    /// Dimensionality of [`VoiceEmbedding::vector`] emitted by this embedder.
    fn embedding_dim(&self) -> usize;

    /// Extracts a unit L2-normalized [`VoiceEmbedding`] from 16 kHz mono audio,
    /// returning `None` if fewer than 4 active frames (`~55 ms`) exceed the energy floor.
    fn embed_16k(&mut self, samples_16k: &[f32]) -> Option<VoiceEmbedding>;
}

/// Pure-Rust zero-download speaker voiceprint extractor combining autocorrelation pitch/harmonicity,
/// unnormalized spectral shape, and 24-band log-Mel / 13-MFCC vocal-tract statistics.
pub struct AcousticMelVoiceEmbedder {
    fft: Arc<dyn Fft<f32>>,
    hann_window: [f32; FRAME_SAMPLES],
    mel_filters: Vec<Vec<(usize, f32)>>,
    bin_glottal_lift: [f32; FFT_BINS],
    dct_matrix: [[f32; MEL_BANDS]; MFCC_COEFFS],
    lifter_weights: [f32; MFCC_COEFFS],
    fft_buf: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
}

impl std::fmt::Debug for AcousticMelVoiceEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcousticMelVoiceEmbedder")
            .field("embedding_dim", &EMBEDDING_DIM)
            .finish()
    }
}

impl Default for AcousticMelVoiceEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl AcousticMelVoiceEmbedder {
    /// Constructs a new 16 kHz acoustic Mel/MFCC/pitch voiceprint extractor.
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let scratch_len = fft.get_inplace_scratch_len();

        let mut hann_window = [0.0_f32; FRAME_SAMPLES];
        for (i, w) in hann_window.iter_mut().enumerate() {
            *w = 0.5 * (1.0 - (2.0 * PI * (i as f32) / (FRAME_SAMPLES as f32)).cos());
        }

        let df = SAMPLE_RATE_16K as f32 / FFT_SIZE as f32;
        let mut bin_glottal_lift = [1.0_f32; FFT_BINS];
        for (bin, lift) in bin_glottal_lift.iter_mut().enumerate() {
            let freq = bin as f32 * df;
            // Equivalent to first-order pre-emphasis flattening the ~-6 dB/octave glottal rolloff.
            let ratio = freq / 650.0;
            *lift = 1.0 + ratio * ratio;
        }

        let mel_filters = build_mel_filterbank(df);

        let mut dct_matrix = [[0.0_f32; MEL_BANDS]; MFCC_COEFFS];
        let scale = (2.0 / MEL_BANDS as f32).sqrt();
        for (k_idx, row) in dct_matrix.iter_mut().enumerate() {
            let k = (k_idx + 1) as f32; // c1..=c13
            for (m, coeff) in row.iter_mut().enumerate() {
                *coeff = scale * (PI * k * (m as f32 + 0.5) / MEL_BANDS as f32).cos();
            }
        }

        let mut lifter_weights = [1.0_f32; MFCC_COEFFS];
        let lifter_l = 16.0_f32;
        for (k_idx, w) in lifter_weights.iter_mut().enumerate() {
            let k = (k_idx + 1) as f32;
            *w = 1.0 + 0.5 * lifter_l * (PI * k / lifter_l).sin();
        }
        // Mildly temper c1 relative to formant-resolving c2..c13 so generic voiced tilt never dominates.
        lifter_weights[0] *= 0.55;

        Self {
            fft,
            hann_window,
            mel_filters,
            bin_glottal_lift,
            dct_matrix,
            lifter_weights,
            fft_buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            fft_scratch: vec![Complex::new(0.0, 0.0); scratch_len],
        }
    }

    fn extract_pitch_and_harmonicity(zero_mean_frame: &[f32; FRAME_SAMPLES]) -> (Option<f32>, f32) {
        let mut corrs = [0.0_f32; MAX_PITCH_LAG + 2];
        let mut max_corr = 0.0_f32;

        for lag in (MIN_PITCH_LAG - 1)..=(MAX_PITCH_LAG + 1) {
            let n = FRAME_SAMPLES - lag;
            let mut dot = 0.0_f64;
            let mut e1 = 0.0_f64;
            let mut e2 = 0.0_f64;
            for i in 0..n {
                let a = f64::from(zero_mean_frame[i]);
                let b = f64::from(zero_mean_frame[i + lag]);
                dot += a * b;
                e1 += a * a;
                e2 += b * b;
            }
            let c = if e1 > 1e-12 && e2 > 1e-12 {
                (dot / (e1.sqrt() * e2.sqrt())).clamp(-1.0, 1.0) as f32
            } else {
                0.0
            };
            corrs[lag] = c;
            if (MIN_PITCH_LAG..=MAX_PITCH_LAG).contains(&lag) && c > max_corr {
                max_corr = c;
            }
        }

        if max_corr < VOICED_CORR_THRESHOLD {
            return (None, max_corr.max(0.0));
        }

        // Select the earliest local peak within 88% of global maximum to prevent subharmonic octave halving.
        let peak_floor = (max_corr * 0.88).max(VOICED_CORR_THRESHOLD);
        let mut chosen_lag = None;
        for lag in MIN_PITCH_LAG..=MAX_PITCH_LAG {
            if corrs[lag] >= peak_floor
                && corrs[lag] >= corrs[lag - 1]
                && corrs[lag] >= corrs[lag + 1]
            {
                chosen_lag = Some(lag);
                break;
            }
        }
        let lag = chosen_lag.unwrap_or_else(|| {
            let mut best_l = MIN_PITCH_LAG;
            let mut best_v = corrs[MIN_PITCH_LAG];
            for (l, &v) in corrs
                .iter()
                .enumerate()
                .take(MAX_PITCH_LAG + 1)
                .skip(MIN_PITCH_LAG + 1)
            {
                if v > best_v {
                    best_v = v;
                    best_l = l;
                }
            }
            best_l
        });

        // Parabolic peak interpolation over (lag - 1, lag, lag + 1).
        let y_prev = corrs[lag - 1];
        let y_curr = corrs[lag];
        let y_next = corrs[lag + 1];
        let denom = y_prev - 2.0 * y_curr + y_next;
        let (delta, refined_peak) = if denom < -1e-6 {
            let d = (0.5 * (y_prev - y_next) / denom).clamp(-0.5, 0.5);
            let p = (y_curr - 0.25 * (y_prev - y_next) * d).clamp(0.0, 1.0);
            (d, p)
        } else {
            (0.0, y_curr.clamp(0.0, 1.0))
        };

        let refined_lag = (lag as f32 + delta).max(1.0);
        let f0_hz = (SAMPLE_RATE_16K as f32 / refined_lag).clamp(MIN_PITCH_HZ, MAX_PITCH_HZ);
        (Some(f0_hz), refined_peak)
    }
}

impl VoiceEmbedder for AcousticMelVoiceEmbedder {
    fn name(&self) -> &'static str {
        "acoustic_mel_mfcc_f0_v1"
    }

    fn embedding_dim(&self) -> usize {
        EMBEDDING_DIM
    }

    fn embed_16k(&mut self, samples_16k: &[f32]) -> Option<VoiceEmbedding> {
        if samples_16k.len() < FRAME_SAMPLES {
            return None;
        }

        let df = SAMPLE_RATE_16K as f32 / FFT_SIZE as f32;
        let min_spec_bin = ((MEL_MIN_HZ / df).ceil() as usize).max(1);
        let max_spec_bin = ((MEL_MAX_HZ / df).floor() as usize).min(FFT_BINS - 1);

        let mut active_frames = 0usize;
        let mut voiced_log_f0s: Vec<f32> = Vec::new();
        let mut voiced_f0s_hz: Vec<f32> = Vec::new();
        let mut harmonicity_sum = 0.0_f32;

        let mut centroid_sum = 0.0_f32;
        let mut rolloff_sum = 0.0_f32;
        let mut low_ratio_sum = 0.0_f32;
        let mut mid_ratio_sum = 0.0_f32;
        let mut high_ratio_sum = 0.0_f32;

        let mut frame_mfccs: Vec<[f32; MFCC_COEFFS]> = Vec::new();
        let mut mel_norm_sum = [0.0_f32; MEL_BANDS];

        let mut zero_mean = [0.0_f32; FRAME_SAMPLES];
        let mut power_spec = [0.0_f32; FFT_BINS];

        for start in (0..=samples_16k.len() - FRAME_SAMPLES).step_by(HOP_SAMPLES) {
            let raw = &samples_16k[start..start + FRAME_SAMPLES];
            let mut sum = 0.0_f64;
            for &s in raw {
                sum += f64::from(sanitize_sample(s));
            }
            let mean = (sum / FRAME_SAMPLES as f64) as f32;

            let mut energy_sq = 0.0_f64;
            for (dst, &s) in zero_mean.iter_mut().zip(raw.iter()) {
                let centered = sanitize_sample(s) - mean;
                *dst = centered;
                energy_sq += f64::from(centered) * f64::from(centered);
            }
            let frame_rms = (energy_sq / FRAME_SAMPLES as f64).sqrt() as f32;
            if frame_rms < ACTIVE_FRAME_RMS_FLOOR {
                continue;
            }
            active_frames += 1;

            // 1. Pitch (F0) & normalized autocorrelation harmonicity.
            let (maybe_f0, harm_peak) = Self::extract_pitch_and_harmonicity(&zero_mean);
            harmonicity_sum += harm_peak;
            if let Some(f0_hz) = maybe_f0 {
                voiced_f0s_hz.push(f0_hz);
                voiced_log_f0s.push(f0_hz.ln());
            }

            // 2. 512-point Hann-windowed FFT power spectrum.
            for (i, (&s, &w)) in zero_mean.iter().zip(self.hann_window.iter()).enumerate() {
                self.fft_buf[i] = Complex::new(s * w, 0.0);
            }
            for slot in &mut self.fft_buf[FRAME_SAMPLES..] {
                *slot = Complex::new(0.0, 0.0);
            }
            self.fft
                .process_with_scratch(&mut self.fft_buf, &mut self.fft_scratch);

            for (bin, slot) in power_spec.iter_mut().enumerate() {
                let c = self.fft_buf[bin];
                *slot = c.re * c.re + c.im * c.im;
            }

            // Unnormalized spectral shape over 80..=7600 Hz.
            let mut mag_weighted_freq = 0.0_f32;
            let mut mag_total = 0.0_f32;
            let mut total_power = 0.0_f32;
            let mut low_power = 0.0_f32;
            let mut mid_power = 0.0_f32;
            let mut high_power = 0.0_f32;

            for (bin, &pwr) in power_spec
                .iter()
                .enumerate()
                .take(max_spec_bin + 1)
                .skip(min_spec_bin)
            {
                let freq = bin as f32 * df;
                let mag = pwr.sqrt();
                mag_weighted_freq += freq * mag;
                mag_total += mag;
                total_power += pwr;

                if freq <= 400.0 {
                    low_power += pwr;
                } else if freq <= 1_800.0 {
                    mid_power += pwr;
                } else {
                    high_power += pwr;
                }
            }

            let centroid_hz = if mag_total > 1e-12 {
                mag_weighted_freq / mag_total
            } else {
                500.0
            };
            centroid_sum += centroid_hz;

            let rolloff_target = 0.85 * total_power;
            let mut accum_power = 0.0_f32;
            let mut rolloff_hz = MEL_MAX_HZ;
            if total_power > 1e-12 {
                for (bin, &pwr) in power_spec
                    .iter()
                    .enumerate()
                    .take(max_spec_bin + 1)
                    .skip(min_spec_bin)
                {
                    accum_power += pwr;
                    if accum_power >= rolloff_target {
                        rolloff_hz = bin as f32 * df;
                        break;
                    }
                }
            }
            rolloff_sum += rolloff_hz;

            let p_denom = total_power.max(1e-12);
            low_ratio_sum += low_power / p_denom;
            mid_ratio_sum += mid_power / p_denom;
            high_ratio_sum += high_power / p_denom;

            // 3. 24-band log-Mel filterbank + 13 MFCCs (c1..c13).
            let mut mel_energies = [0.0_f32; MEL_BANDS];
            let mut max_mel = 1e-12_f32;
            for (m, weights) in self.mel_filters.iter().enumerate() {
                let mut band_energy = 0.0_f32;
                for &(bin, w) in weights {
                    band_energy += power_spec[bin] * self.bin_glottal_lift[bin] * w;
                }
                mel_energies[m] = band_energy;
                if band_energy > max_mel {
                    max_mel = band_energy;
                }
            }

            // Clamp stopband floor at -40 dB below frame peak for room-noise immunity + gain invariance.
            let mel_floor = max_mel * 1e-4;
            let mut log_mel = [0.0_f32; MEL_BANDS];
            let mut log_mel_mean = 0.0_f32;
            for (m, &e) in mel_energies.iter().enumerate() {
                let lm = e.max(mel_floor).ln();
                log_mel[m] = lm;
                log_mel_mean += lm;
            }
            log_mel_mean /= MEL_BANDS as f32;

            for (m, lm) in log_mel.iter_mut().enumerate() {
                *lm -= log_mel_mean;
                mel_norm_sum[m] += *lm;
            }

            let mut mfcc = [0.0_f32; MFCC_COEFFS];
            for (k_idx, row) in self.dct_matrix.iter().enumerate() {
                let mut sum_c = 0.0_f32;
                for (&lm, &d) in log_mel.iter().zip(row.iter()) {
                    sum_c += lm * d;
                }
                mfcc[k_idx] = sum_c * self.lifter_weights[k_idx];
            }
            frame_mfccs.push(mfcc);
        }

        if active_frames < MIN_ACTIVE_FRAMES {
            return None;
        }

        let inv_active = 1.0 / active_frames as f32;
        let voiced_frames = voiced_f0s_hz.len();
        let median_f0_hz = median_in_place(&mut voiced_f0s_hz);

        // --- Block 1: Pitch & Harmonicity (10 dims) ---
        let mut pitch_block = [0.0_f32; PITCH_DIM];
        if let Some(med_f0) = median_f0_hz {
            voiced_log_f0s.sort_by(f32::total_cmp);
            let u = ((med_f0.ln() - 4.2) / 1.7).clamp(0.0, 1.0);
            let iqr = interquartile_range_sorted(&voiced_log_f0s);
            let std_lf0 = std_dev(&voiced_log_f0s);

            pitch_block[0] = 2.0 * u - 1.0;
            pitch_block[1] = (PI * u).cos();
            pitch_block[2] = (PI * u).sin() - 0.65;
            pitch_block[3] = (2.0 * PI * u).cos();
            pitch_block[4] = (2.0 * PI * u).sin();
            pitch_block[5] = (3.0 * PI * u).cos();
            pitch_block[6] = (3.0 * PI * u).sin();
            pitch_block[7] = ((iqr - 0.05) * 3.0).clamp(-0.75, 0.75) * 0.35;
            pitch_block[8] = ((std_lf0 - 0.04) * 3.0).clamp(-0.75, 0.75) * 0.35;
        }
        let mean_harm = harmonicity_sum * inv_active;
        pitch_block[9] = ((mean_harm - 0.60) * 2.0).clamp(-1.0, 1.0) * 0.55;
        normalize_and_scale(&mut pitch_block, PITCH_BLOCK_WEIGHT);

        // --- Block 2: Unnormalized Spectral Shape (8 dims) ---
        let mean_centroid = centroid_sum * inv_active;
        let mean_rolloff = rolloff_sum * inv_active;
        let mean_low = low_ratio_sum * inv_active;
        let mean_mid = mid_ratio_sum * inv_active;
        let mean_high = high_ratio_sum * inv_active;

        let c_u = ((mean_centroid.max(MEL_MIN_HZ).ln() - 5.1) / 2.6).clamp(0.0, 1.0);
        let r_u = ((mean_rolloff.max(MEL_MIN_HZ).ln() - 5.5) / 2.8).clamp(0.0, 1.0);

        // Centered log-ratio (CLR) across low (80-400), mid (400-1800), high (1800-7600) bands,
        // offset by typical voiced-speech baseline so distinct formant balances point in opposite directions.
        let l_low = (mean_low + 0.015).ln();
        let l_mid = (mean_mid + 0.015).ln();
        let l_high = (mean_high + 0.015).ln();
        let l_mean = (l_low + l_mid + l_high) / 3.0;

        let mut spectral_block = [
            2.0 * c_u - 1.0,
            (PI * c_u).cos(),
            (PI * c_u).sin() - 0.65,
            2.0 * r_u - 1.0,
            (PI * r_u).cos(),
            (l_low - l_mean) - 0.75,
            (l_mid - l_mean) - 0.55,
            (l_high - l_mean) + 1.30,
        ];
        normalize_and_scale(&mut spectral_block, SPECTRAL_BLOCK_WEIGHT);

        // --- Block 3: Utterance-Mean Liftered MFCCs c1..c13 (13 dims) ---
        let mut mfcc_mean = [0.0_f32; MFCC_COEFFS];
        for frame_c in &frame_mfccs {
            for (acc, &v) in mfcc_mean.iter_mut().zip(frame_c.iter()) {
                *acc += v;
            }
        }
        for acc in &mut mfcc_mean {
            *acc *= inv_active;
        }
        // Center c1 around neutral residual voiced slope after glottal lift.
        mfcc_mean[0] -= 3.2;
        let mut mfcc_mean_block = mfcc_mean;
        normalize_and_scale(&mut mfcc_mean_block, MFCC_MEAN_WEIGHT);

        // --- Block 4: 5 Coarse Formant-Region Log-Mel Deviations (5 dims) ---
        let mel_ranges: [(usize, usize, f32); GROUPED_MEL_BANDS] = [
            (0, 4, 1.2),    // ~80-380 Hz (F1 low / chest)
            (4, 9, 1.5),    // ~380-950 Hz (F1 high / F2 low)
            (9, 14, 0.2),   // ~950-2050 Hz (F2 mid-high)
            (14, 19, -1.1), // ~2050-4000 Hz (F3 / speaker presence)
            (19, 24, -1.8), // ~4000-7600 Hz (fricative / brilliance)
        ];
        let mut mel_group_block = [0.0_f32; GROUPED_MEL_BANDS];
        for (g_idx, &(lo, hi, prior)) in mel_ranges.iter().enumerate() {
            let mut sum_g = 0.0_f32;
            for &v in &mel_norm_sum[lo..hi] {
                sum_g += v * inv_active;
            }
            mel_group_block[g_idx] = (sum_g / (hi - lo) as f32) - prior;
        }
        normalize_and_scale(&mut mel_group_block, MEL_GROUP_WEIGHT);

        // --- Block 5: Cepstral-Mean-Normalized (CMN) MFCC Temporal Std c1..c12 (12 dims) ---
        // CMN subtracts the utterance cepstral mean `c_k - mean_t(c_k)` for linear channel/EQ invariance.
        let mut cmn_std_block = [0.0_f32; CMN_MFCC_COEFFS];
        for (k_idx, dst) in cmn_std_block.iter_mut().enumerate() {
            let mean_k = if k_idx == 0 {
                mfcc_mean[0] + 3.2
            } else {
                mfcc_mean[k_idx]
            };
            let mut var_sum = 0.0_f32;
            for frame_c in &frame_mfccs {
                let cmn_val = frame_c[k_idx] - mean_k;
                var_sum += cmn_val * cmn_val;
            }
            *dst = (var_sum * inv_active).sqrt() - 0.8;
        }
        let cmn_reliability = ((active_frames as f32 - 4.0) / 14.0).clamp(0.25, 1.0);
        normalize_and_scale(&mut cmn_std_block, CMN_STD_WEIGHT * cmn_reliability);

        let mut vector = Vec::with_capacity(EMBEDDING_DIM);
        vector.extend_from_slice(&pitch_block);
        vector.extend_from_slice(&spectral_block);
        vector.extend_from_slice(&mfcc_mean_block);
        vector.extend_from_slice(&mel_group_block);
        vector.extend_from_slice(&cmn_std_block);
        l2_normalize(&mut vector);

        Some(VoiceEmbedding {
            vector,
            voiced_frames,
            median_f0_hz,
        })
    }
}

fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() && sample.abs() >= 1e-20 {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn hz_to_mel(hz: f32) -> f32 {
    2_595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0_f32.powf(mel / 2_595.0) - 1.0)
}

fn build_mel_filterbank(df: f32) -> Vec<Vec<(usize, f32)>> {
    let mel_min = hz_to_mel(MEL_MIN_HZ);
    let mel_max = hz_to_mel(MEL_MAX_HZ);
    let step = (mel_max - mel_min) / (MEL_BANDS as f32 + 1.0);

    let mut hz_points = [0.0_f32; MEL_BANDS + 2];
    for (i, slot) in hz_points.iter_mut().enumerate() {
        *slot = mel_to_hz(mel_min + (i as f32) * step);
    }

    let mut filters = Vec::with_capacity(MEL_BANDS);
    for m in 0..MEL_BANDS {
        let f_left = hz_points[m];
        let f_center = hz_points[m + 1];
        let f_right = hz_points[m + 2];

        let mut weights = Vec::new();
        let mut weight_sum = 0.0_f32;
        for bin in 1..FFT_BINS {
            let freq = bin as f32 * df;
            if freq <= f_left || freq >= f_right {
                continue;
            }
            let w = if freq <= f_center {
                (freq - f_left) / (f_center - f_left).max(1e-6)
            } else {
                (f_right - freq) / (f_right - f_center).max(1e-6)
            };
            if w > 0.0 {
                weights.push((bin, w));
                weight_sum += w;
            }
        }
        if weight_sum > 1e-12 {
            for (_, w) in &mut weights {
                *w /= weight_sum;
            }
        }
        filters.push(weights);
    }
    filters
}

fn median_in_place(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[mid])
    } else {
        Some(0.5 * (values[mid - 1] + values[mid]))
    }
}

fn interquartile_range_sorted(sorted: &[f32]) -> f32 {
    if sorted.len() < 2 {
        return 0.0;
    }
    let q25 = percentile_sorted(sorted, 0.25);
    let q75 = percentile_sorted(sorted, 0.75);
    (q75 - q25).max(0.0)
}

fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let pos = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f32;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f32;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

fn std_dev(values: &[f32]) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().copied().sum::<f32>() / values.len() as f32;
    let var = values
        .iter()
        .map(|&v| {
            let d = v - mean;
            d * d
        })
        .sum::<f32>()
        / values.len() as f32;
    var.sqrt()
}

fn normalize_and_scale(slice: &mut [f32], weight: f32) {
    let norm_sq: f64 = slice.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    if norm_sq <= 1e-20 {
        slice.fill(0.0);
        return;
    }
    let scale = (f64::from(weight) / norm_sq.sqrt()) as f32;
    for v in slice.iter_mut() {
        *v *= scale;
    }
}

pub(crate) fn l2_normalize(slice: &mut [f32]) {
    let norm_sq: f64 = slice.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    if norm_sq <= 1e-24 {
        return;
    }
    let inv = (1.0 / norm_sq.sqrt()) as f32;
    for v in slice.iter_mut() {
        *v *= inv;
    }
}
