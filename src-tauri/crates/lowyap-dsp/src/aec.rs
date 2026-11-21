use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear;
use lowyap_ports::{EchoCanceller, OutputRouteKind};

use crate::normalized_pcm;

const FRAME: usize = 160;
const MAX_DELAY: usize = 2_880; // 180 ms
const HISTORY: usize = 4_096;
const LINEAR_TAPS: usize = 52;
const NONLINEAR_TAPS: usize = 7;
const FEATURE_COUNT: usize = LINEAR_TAPS + 2 * NONLINEAR_TAPS;
const ACTIVE_RENDER: f32 = 0.003;

/// WebRTC AEC3 with a fast delay-aligned adaptive filter for speaker echo.
///
/// The returned samples stay on the original microphone clock, including on
/// partial calls. Render-only calls advance the render clock with zero capture;
/// otherwise only the render prefix aligned to capture is used, with missing
/// reference samples treated as silence.
pub struct WebRtcAec3EchoCanceller {
    worker: Option<Aec3Worker>,
    capture: [f32; FRAME],
    render: [f32; FRAME],
    filled: usize,
    residual_ratio: f32,
    route: Option<OutputRouteKind>,
    fast: FastEchoFilter,
}

impl WebRtcAec3EchoCanceller {
    /// Creates a fresh mono 16 kHz canceller. If the graph cannot initialize,
    /// the in-clock adaptive filter remains available.
    pub fn new() -> Self {
        let format = AudioFormat::ten_ms(16_000, 1);
        Self {
            worker: Aec3Worker::spawn(format),
            capture: [0.0; FRAME],
            render: [0.0; FRAME],
            filled: 0,
            residual_ratio: 1.0,
            route: None,
            fast: FastEchoFilter::default(),
        }
    }

    fn run_webrtc(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        let power = worker.process(self.render, self.capture);
        if let Some(output_rms) = power.filter(|rms| rms.is_finite()) {
            // AEC3's block framer delays its PCM. Use its residual power as a
            // gain estimate, not the samples themselves, to avoid moving speech
            // on the input timeline or splicing delayed PCM into partial calls.
            let input = rms(&self.capture);
            if input > ACTIVE_RENDER {
                let ratio = (output_rms / input).clamp(0.0, 1.0);
                self.residual_ratio = 0.85 * self.residual_ratio + 0.15 * ratio;
            }
        } else {
            self.residual_ratio = 1.0;
            self.worker = None;
        }
    }
}

struct Aec3Frame {
    render: [f32; FRAME],
    capture: [f32; FRAME],
}

// The graph's Rc-backed runtime is !Send. Only frames and scalar results cross
// the thread boundary, so the public EchoCanceller stays Send without unsafe.
struct Aec3Worker {
    request: Option<SyncSender<Aec3Frame>>,
    result: Receiver<f32>,
    thread: Option<JoinHandle<()>>,
}

impl Aec3Worker {
    fn spawn(format: AudioFormat) -> Option<Self> {
        let (request, incoming) = mpsc::sync_channel::<Aec3Frame>(0);
        // At most one frame is in flight. Buffer its reply so Drop can join
        // the worker even if the caller unwinds before receiving it.
        let (outgoing, result) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("lowyap-aec3".into())
            .spawn(move || {
                let mut pipeline = match linear::builder(format, format)
                    .enable_high_pass_filter(true)
                    .enable_gain_controller2(false)
                    .enable_post_filter(false)
                    .enable_noise_suppression(false)
                    .build()
                {
                    Ok(pipeline) => pipeline,
                    Err(error) => {
                        eprintln!("lowyap AEC3 unavailable: {error}");
                        return;
                    }
                };
                let mut output = [0.0; FRAME];
                while let Ok(frame) = incoming.recv() {
                    let processed = pipeline
                        .handle_render_frame(&frame.render)
                        .and_then(|()| pipeline.process_capture_frame(&frame.capture, &mut output));
                    let power = match processed {
                        Ok(true) => rms(&output),
                        Ok(false) => rms(&frame.capture),
                        Err(error) => {
                            eprintln!("lowyap AEC3 disabled after processing error: {error}");
                            break;
                        }
                    };
                    if outgoing.send(power).is_err() {
                        break;
                    }
                }
            });
        match thread {
            Ok(thread) => Some(Self {
                request: Some(request),
                result,
                thread: Some(thread),
            }),
            Err(error) => {
                eprintln!("lowyap AEC3 worker unavailable: {error}");
                None
            }
        }
    }

    fn process(&self, render: [f32; FRAME], capture: [f32; FRAME]) -> Option<f32> {
        self.request
            .as_ref()?
            .send(Aec3Frame { render, capture })
            .ok()?;
        self.result.recv().ok()
    }
}

impl Drop for Aec3Worker {
    fn drop(&mut self) {
        drop(self.request.take());
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("lowyap AEC3 worker stopped unexpectedly");
            }
        }
    }
}

impl Default for WebRtcAec3EchoCanceller {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoCanceller for WebRtcAec3EchoCanceller {
    fn name(&self) -> &'static str {
        "webrtc_aec3"
    }

    fn process_16k(
        &mut self,
        me_16k: &[f32],
        others_ref_16k: &[f32],
        route: OutputRouteKind,
    ) -> Vec<f32> {
        let mut output = Vec::with_capacity(me_16k.len());
        let render_only = me_16k.is_empty();
        let count = if render_only {
            others_ref_16k.len()
        } else {
            me_16k.len()
        };
        if count == 0 {
            return output;
        }
        if self.route == Some(OutputRouteKind::Headphones) && route != OutputRouteKind::Headphones {
            self.fast.clear_path();
            self.residual_ratio = 1.0;
        }
        self.route = Some(route);

        let mut offset = 0;
        while offset < count {
            let begin = self.filled;
            let n = (count - offset).min(FRAME - begin);
            let end = begin + n;
            for i in 0..n {
                self.capture[begin + i] =
                    normalized_pcm(me_16k.get(offset + i).copied().unwrap_or(0.0));
                self.render[begin + i] =
                    normalized_pcm(others_ref_16k.get(offset + i).copied().unwrap_or(0.0));
            }
            self.fast.push_reference(&self.render[begin..end]);
            if end == FRAME {
                self.run_webrtc();
            }

            if !render_only {
                if route == OutputRouteKind::Headphones {
                    output.extend_from_slice(&self.capture[begin..end]);
                } else {
                    self.fast.clean(
                        &self.capture[..end],
                        begin,
                        self.residual_ratio,
                        &mut output,
                    );
                }
            }
            self.filled = end % FRAME;
            offset += n;
        }
        output
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

struct FastEchoFilter {
    render: [f32; HISTORY],
    written: usize,
    weights: [f32; FEATURE_COUNT],
    delay: Option<f32>,
    trained: usize,
    next_search: usize,
    quiet_samples: usize,
    last_active_render: Option<usize>,
}

impl Default for FastEchoFilter {
    fn default() -> Self {
        Self {
            render: [0.0; HISTORY],
            written: 0,
            weights: [0.0; FEATURE_COUNT],
            delay: None,
            trained: 0,
            next_search: 0,
            quiet_samples: 0,
            last_active_render: None,
        }
    }
}

impl FastEchoFilter {
    fn clear_path(&mut self) {
        self.weights.fill(0.0);
        self.delay = None;
        self.trained = 0;
        self.next_search = 0;
        self.quiet_samples = 0;
    }

    fn push_reference(&mut self, render: &[f32]) {
        for &sample in render {
            self.render[self.written % HISTORY] = sample;
            if sample.abs() >= ACTIVE_RENDER {
                self.last_active_render = Some(self.written);
            }
            self.written += 1;
        }
    }

    fn at(&self, index: usize, lag: usize) -> f32 {
        index
            .checked_sub(lag)
            .map_or(0.0, |i| self.render[i % HISTORY])
    }

    fn find_delay(&self, capture: &[f32], base: usize) -> Option<f32> {
        let max_lag = MAX_DELAY.min(self.written - 1);
        let score = |lag| {
            let (mut aa, mut bb, mut ab) = (0.0, 0.0, 0.0);
            for (i, &a) in capture.iter().enumerate().step_by(4) {
                let b = self.at(base + i, lag);
                aa += a * a;
                bb += b * b;
                ab += a * b;
            }
            if bb < ACTIVE_RENDER.powi(2) * capture.len().div_ceil(4) as f32 {
                0.0
            } else {
                ab.abs() / (aa * bb).sqrt().max(1e-12)
            }
        };
        let mut best = (0, 0.0);
        for lag in (0..=max_lag).step_by(4) {
            let value = score(lag);
            if value > best.1 {
                best = (lag, value);
            }
        }
        let refine = if best.1 >= 0.88 {
            best.0.saturating_sub(3)..=(best.0 + 3).min(max_lag)
        } else {
            // A broadband transient can have a narrow correlation peak that
            // falls between the coarse search positions.
            0..=max_lag
        };
        for lag in refine {
            let value = score(lag);
            if value > best.1 {
                best = (lag, value);
            }
        }
        if best.1 < 0.88 {
            return None;
        }
        let fraction = if best.0 > 0 && best.0 < max_lag {
            let (left, right) = (score(best.0 - 1), score(best.0 + 1));
            let curvature = left + right - 2.0 * best.1;
            if curvature < -1e-6 {
                (0.5 * (left - right) / curvature).clamp(-0.5, 0.5)
            } else {
                0.0
            }
        } else {
            0.0
        };
        Some(best.0 as f32 + fraction)
    }

    fn delayed(&self, index: usize, lag: usize, phase: f32) -> f32 {
        let a = self.at(index, lag);
        a + phase * (self.at(index, lag + 1) - a)
    }

    fn features(&self, index: usize, delay: f32) -> [f32; FEATURE_COUNT] {
        let mut features = [0.0; FEATURE_COUNT];
        let lag = delay as usize;
        let phase = delay - lag as f32;
        let lead = lag.min(3);
        for (tap, x) in features[..LINEAR_TAPS].iter_mut().enumerate() {
            *x = self.delayed(index, lag - lead + tap, phase);
        }
        // A parallel polynomial path models even harmonics and soft clipping.
        // Sparse nonlinear taps cover early reflections without a dense
        // Volterra filter or a fixed assumption about any room geometry.
        for tap in 0..NONLINEAR_TAPS {
            let x = self.delayed(index, lag + tap * 8, phase);
            let square = x * x;
            features[LINEAR_TAPS + tap] = square * 2.0;
            features[LINEAR_TAPS + NONLINEAR_TAPS + tap] = square * x * 4.0;
        }
        features
    }

    fn clean(&mut self, capture: &[f32], from: usize, aec3_ratio: f32, output: &mut Vec<f32>) {
        let count = capture.len() - from;
        let base = self.written - capture.len();
        let mic_rms = rms(capture);
        if self.delay.is_none()
            && self.written >= self.next_search
            && capture.len() >= 80
            && mic_rms > ACTIVE_RENDER
            && self
                .last_active_render
                .is_some_and(|last| self.written - last <= MAX_DELAY + FRAME)
        {
            self.next_search = self.written + 80;
            self.delay = self.find_delay(capture, base);
        }
        let Some(delay) = self.delay else {
            output.extend_from_slice(&capture[from..]);
            return;
        };

        let mut features = [[0.0; FEATURE_COUNT]; FRAME];
        let mut residual = [0.0; FRAME];
        let (mut ref_sq, mut mic_ref) = (0.0, 0.0);
        let lag = delay as usize;
        let phase = delay - lag as f32;
        for (i, &me) in capture.iter().enumerate() {
            let x = self.delayed(base + i, lag, phase);
            ref_sq += x * x;
            mic_ref += me * x;
        }
        let n = capture.len() as f32;
        let ref_rms = (ref_sq / n).sqrt();
        if ref_rms < ACTIVE_RENDER {
            output.extend_from_slice(&capture[from..]);
            self.quiet_samples += count;
            // Force reacquisition after a long render gap, e.g. room/device change.
            if self.quiet_samples > 16_000 {
                self.clear_path();
            }
            return;
        }
        self.quiet_samples = 0;
        let coherence = mic_ref.abs() / (n * ref_rms * mic_rms).max(1e-9);
        let gain = mic_ref / ref_sq;
        let cold = self.trained < FRAME * 3;
        let (mut pred_sq, mut residual_ref) = (0.0, 0.0);
        for (i, &me) in capture.iter().enumerate() {
            features[i] = self.features(base + i, delay);
            let estimate = dot(&self.weights, &features[i]);
            pred_sq += estimate * estimate;
            residual[i] = me - estimate;
            residual_ref += residual[i] * self.delayed(base + i, lag, phase);
        }
        let pred_rms = (pred_sq / n).sqrt();
        let residual_rms = rms(&residual[..capture.len()]);
        let residual_coherence = residual_ref.abs() / (n * ref_rms * residual_rms).max(1e-9);
        // A gain change in a strongly coherent echo path can leave a sizable
        // residual, but it will still be phase-locked to the delayed render.
        let gain_drift = coherence > 0.985 && mic_rms < 1.1 * ref_rms && residual_coherence > 0.6;
        // The frozen predictor is a stronger double-talk test than energy
        // alone: even quiet near-end speech (-36 dBFS) under loud speaker echo
        // leaves an uncorrelated innovation above the converged predictor floor.
        let echo_only = if capture.len() < 40 {
            false
        } else if cold {
            coherence > 0.90 && mic_rms < 1.1 * ref_rms
        } else {
            let quiet_near_end_innovation =
                residual_rms >= (0.045 * pred_rms).max(0.006) && residual_coherence < 0.35;
            (!quiet_near_end_innovation && residual_rms < (0.14 * pred_rms).max(0.0022))
                || gain_drift
        };

        if !echo_only {
            let believable = !cold && pred_rms > 0.0 && residual_rms < mic_rms * 1.12;
            output.extend(
                if believable { &residual } else { capture }[from..capture.len()]
                    .iter()
                    .copied()
                    .map(normalized_pcm),
            );
            return;
        }

        if self.trained == 0 {
            self.weights[lag.min(3)] = gain.clamp(-1.1, 1.1);
        }
        for (x, &me) in features[from..capture.len()].iter().zip(&capture[from..]) {
            let error = me - dot(&self.weights, x);
            let step = (if cold { 0.65 } else { 0.28 }) * error / (1e-4 + dot(x, x));
            for (w, &sample) in self.weights.iter_mut().zip(x) {
                *w = (*w + step * sample).clamp(-2.0, 2.0);
            }
        }
        self.trained = self.trained.saturating_add(count);

        let local_ratio = residual_rms / mic_rms.max(1e-6);
        let residual_gain = if cold {
            (1.0 - coherence * coherence).clamp(0.03, 1.0)
        } else {
            (2.0 * local_ratio.powi(2)).clamp(0.02, 1.0)
        };
        let gain = residual_gain * (0.5 + 0.5 * aec3_ratio);
        for (x, &me) in features[from..capture.len()].iter().zip(&capture[from..]) {
            output.push(normalized_pcm(gain * (me - dot(&self.weights, x))));
        }
    }
}

fn dot<const N: usize>(a: &[f32; N], b: &[f32; N]) -> f32 {
    a.iter().zip(b).map(|(&a, &b)| a * b).sum()
}

fn rms(pcm: &[f32]) -> f32 {
    (pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len().max(1) as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headphones_advance_graph_on_real_frames_without_padding_between_calls() {
        let mut aec = WebRtcAec3EchoCanceller::new();
        let me = [0.2; FRAME];
        let render = [0.25; FRAME];
        aec.process_16k(&me[..27], &render[..27], OutputRouteKind::Headphones);
        assert_eq!(aec.residual_ratio, 1.0);
        aec.process_16k(&me[27..], &render[27..], OutputRouteKind::Headphones);
        assert!(aec.residual_ratio < 1.0, "graph did not process the frame");
    }

    #[test]
    fn preserves_quiet_near_end_speech_under_loud_speaker_echo() {
        let mut aec = WebRtcAec3EchoCanceller::new();
        let sr = 16_000.0_f32;
        let delay_samples = 45 * 16; // 45 ms acoustic path
        let warmup_samples = 16_000usize; // 1.0 s echo-only convergence
        let dt_samples = 8_000usize; // 0.5 s quiet near-end speech (-36 dBFS) under loud render
        let total = warmup_samples + dt_samples;

        let render: Vec<f32> = (0..total)
            .map(|i| {
                let t = i as f32 / sr;
                let f0 = 205.0 * std::f32::consts::TAU * t;
                0.34 * (0.65 * f0.sin() + 0.25 * (2.0 * f0).sin() + 0.10 * (3.0 * f0).sin())
            })
            .collect();

        let near_clean: Vec<f32> = (0..dt_samples)
            .map(|i| {
                let t = i as f32 / sr;
                let f0 = 132.0 * std::f32::consts::TAU * t;
                // Quiet near-end voice (~0.016 RMS, ~ -36 dBFS) while echo pred_rms is ~0.13
                0.022 * (0.70 * f0.sin() + 0.20 * (2.0 * f0).sin() + 0.10 * (3.0 * f0).sin())
            })
            .collect();

        let mut capture = vec![0.0_f32; total];
        for i in delay_samples..total {
            let x = render[i - delay_samples] * 0.55;
            capture[i] = (x + 0.08 * x * x).tanh();
        }
        for (i, &near) in near_clean.iter().enumerate() {
            capture[warmup_samples + i] += near;
        }

        let mut cleaned = Vec::with_capacity(total);
        for (me_chunk, ref_chunk) in capture.chunks(FRAME).zip(render.chunks(FRAME)) {
            cleaned.extend(aec.process_16k(me_chunk, ref_chunk, OutputRouteKind::Speakers));
        }

        let target_rms = rms(&near_clean);
        let raw_dt = &capture[warmup_samples..];
        let raw_rms = rms(raw_dt);
        let raw_corr = raw_dt
            .iter()
            .zip(&near_clean)
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / (dt_samples as f32 * raw_rms * target_rms).max(1e-9);
        let out_dt = &cleaned[warmup_samples..];
        let out_rms = rms(out_dt);
        let corr_num: f32 = out_dt.iter().zip(&near_clean).map(|(a, b)| a * b).sum();
        let corr = corr_num / (dt_samples as f32 * out_rms * target_rms).max(1e-9);

        assert!(raw_corr < 0.20, "expected echo-dominated raw capture");
        assert!(
            out_rms >= target_rms * 0.75,
            "quiet near-end speech was over-suppressed: out_rms={out_rms:.5}, target_rms={target_rms:.5}"
        );
        assert!(
            corr >= 0.60,
            "quiet near-end waveform distorted under loud speaker echo: corr={corr:.3} (raw_corr={raw_corr:.3})"
        );
    }
}
