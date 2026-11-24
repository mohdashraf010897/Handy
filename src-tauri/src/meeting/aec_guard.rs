use crate::meeting::types::OutputRouteKind;
use std::collections::VecDeque;

const SAMPLE_RATE_16K: usize = 16_000;
const FRAME_MS: usize = 20;
pub const FRAME_SAMPLES_16K: usize = SAMPLE_RATE_16K * FRAME_MS / 1_000;
const MAX_DELAY_MS: usize = 340;
const MAX_DELAY_FRAMES: usize = MAX_DELAY_MS / FRAME_MS;
const ACTIVE_RMS_FLOOR: f32 = 0.008;
const ACTIVE_PEAK_FLOOR: f32 = 0.03;
const CORRELATION_MIN: f32 = 0.78;
const NEAR_END_BURST_MULTIPLIER: f32 = 1.35;
const BLEED_RATIO_LIMIT: f32 = 0.85;

#[derive(Debug, Default)]
pub struct SpeakerEchoGuard {
    others_history: VecDeque<Vec<f32>>,
}

impl SpeakerEchoGuard {
    pub fn new() -> Self {
        Self {
            others_history: VecDeque::with_capacity(MAX_DELAY_FRAMES + 1),
        }
    }

    pub fn process(
        &mut self,
        me_16k: &[f32],
        others_16k: &[f32],
        output_route: OutputRouteKind,
    ) -> Vec<f32> {
        if me_16k.is_empty() {
            self.push_others_history(others_16k);
            return Vec::new();
        }

        if output_route == OutputRouteKind::Headphones {
            self.push_others_history(others_16k);
            return me_16k.to_vec();
        }

        let target_len = me_16k.len().max(others_16k.len());
        let mut me = me_16k.to_vec();
        me.resize(target_len, 0.0);
        let mut others = others_16k.to_vec();
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
            let others_active = rms(&others_frame) > ACTIVE_RMS_FLOOR
                || peak_abs(&others_frame) > ACTIVE_PEAK_FLOOR;

            let (best_corr, others_peak_rms) = self.best_delayed_match(&me_frame);
            let expected_bleed = others_peak_rms * BLEED_RATIO_LIMIT;
            let near_end_burst = me_peak > expected_bleed * NEAR_END_BURST_MULTIPLIER;
            let unknown_route_without_strong_others =
                output_route == OutputRouteKind::Unknown && others_peak_rms < ACTIVE_RMS_FLOOR;

            let should_suppress = !unknown_route_without_strong_others
                && others_active
                && best_corr >= CORRELATION_MIN
                && me_rms < others_peak_rms * BLEED_RATIO_LIMIT
                && !near_end_burst;

            let gain = if should_suppress {
                if best_corr > 0.92 && me_rms < others_peak_rms * 0.65 {
                    0.0
                } else {
                    let knee =
                        ((best_corr - CORRELATION_MIN) / (0.95 - CORRELATION_MIN)).clamp(0.0, 1.0);
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
        while self.others_history.len() > (MAX_DELAY_FRAMES + 1) {
            self.others_history.pop_front();
        }
    }

    fn best_delayed_match(&self, me_frame: &[f32]) -> (f32, f32) {
        let mut best_corr = 0.0_f32;
        let mut peak_rms = 0.0_f32;

        for delay in 0..=MAX_DELAY_FRAMES {
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

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq = samples.iter().map(|s| s * s).sum::<f32>();
    (sum_sq / samples.len() as f32).sqrt()
}

fn peak_abs(samples: &[f32]) -> f32 {
    samples.iter().map(|s| s.abs()).fold(0.0_f32, f32::max)
}

fn normalized_correlation(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut a_sq = 0.0_f32;
    let mut b_sq = 0.0_f32;
    for (lhs, rhs) in a.iter().zip(b.iter()) {
        dot += lhs * rhs;
        a_sq += lhs * lhs;
        b_sq += rhs * rhs;
    }
    if a_sq <= f32::EPSILON || b_sq <= f32::EPSILON {
        return 0.0;
    }
    (dot / (a_sq.sqrt() * b_sq.sqrt())).abs()
}

fn normalized_envelope_correlation(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut a_sq = 0.0_f32;
    let mut b_sq = 0.0_f32;
    for (lhs, rhs) in a.iter().zip(b.iter()) {
        let lhs_abs = lhs.abs();
        let rhs_abs = rhs.abs();
        dot += lhs_abs * rhs_abs;
        a_sq += lhs_abs * lhs_abs;
        b_sq += rhs_abs * rhs_abs;
    }
    if a_sq <= f32::EPSILON || b_sq <= f32::EPSILON {
        return 0.0;
    }
    dot / (a_sq.sqrt() * b_sq.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_sine(frames: usize, amp: f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * FRAME_SAMPLES_16K);
        for i in 0..(frames * FRAME_SAMPLES_16K) {
            let phase = (i as f32 / 14.0).sin();
            out.push(phase * amp);
        }
        out
    }

    fn delayed_bleed(input: &[f32], delay_frames: usize, gain: f32) -> Vec<f32> {
        let delay = delay_frames * FRAME_SAMPLES_16K;
        let mut out = vec![0.0_f32; input.len()];
        for idx in delay..input.len() {
            out[idx] = input[idx - delay] * gain;
        }
        out
    }

    #[test]
    fn headphones_pass_through_me_unchanged() {
        let mut guard = SpeakerEchoGuard::new();
        let me = synth_sine(4, 0.5);
        let others = synth_sine(4, 0.7);
        let out = guard.process(&me, &others, OutputRouteKind::Headphones);
        assert_eq!(out, me);
    }

    #[test]
    fn suppresses_delayed_speaker_bleed() {
        let mut guard = SpeakerEchoGuard::new();
        let others = synth_sine(12, 0.6);
        let me_bleed = delayed_bleed(&others, 2, 0.5);
        let out = guard.process(&me_bleed, &others, OutputRouteKind::Speakers);
        assert!(rms(&out) < rms(&me_bleed) * 0.2);
    }

    #[test]
    fn preserves_near_end_double_talk() {
        let mut guard = SpeakerEchoGuard::new();
        let others = synth_sine(12, 0.5);
        let mut me = delayed_bleed(&others, 2, 0.45);
        let burst_start = 5 * FRAME_SAMPLES_16K;
        let burst_end = burst_start + FRAME_SAMPLES_16K;
        for sample in &mut me[burst_start..burst_end] {
            *sample += 0.75;
        }

        let out = guard.process(&me, &others, OutputRouteKind::Speakers);
        let in_burst_rms = rms(&me[burst_start..burst_end]);
        let out_burst_rms = rms(&out[burst_start..burst_end]);
        assert!(out_burst_rms > in_burst_rms * 0.55);
    }

    #[test]
    fn silence_is_stable_and_stays_silent() {
        let mut guard = SpeakerEchoGuard::new();
        let me = vec![0.0_f32; FRAME_SAMPLES_16K * 3];
        let others = vec![0.0_f32; FRAME_SAMPLES_16K * 3];
        let out = guard.process(&me, &others, OutputRouteKind::Speakers);
        assert!(out.iter().all(|sample| *sample == 0.0));
    }
}
