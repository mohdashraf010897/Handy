use std::collections::VecDeque;

use lowyap_ports::{SpeakerChannel, UtteranceSegmenter, UtteranceSpan};

use crate::normalized_pcm;

const SAMPLES_PER_MS: u64 = 16;
const EARSHOT_FRAME: usize = 256;
const MODEL_NOISE_FLOOR: f32 = 0.006;

/// Stateful 16 kHz mono voice-activity scorer.
pub trait FrameSpeechScorer: Send {
    /// Stable scorer name.
    fn name(&self) -> &'static str;
    /// Number of samples expected in each frame (must be nonzero).
    fn frame_samples(&self) -> usize;
    /// Returns a speech probability in `[0, 1]` for one complete frame.
    fn score_frame(&mut self, frame_16k: &[f32]) -> f32;
    /// Clears temporal context between streams.
    fn reset(&mut self);
}

/// Earshot neural VAD with a periodic harmonic assist for strongly voiced frames.
pub struct EarshotSpeechScorer {
    detector: Box<earshot::Detector>,
    previous: [f32; EARSHOT_FRAME],
}

impl EarshotSpeechScorer {
    /// Creates a scorer with no audio history.
    pub fn new() -> Self {
        Self {
            detector: earshot::Detector::default_boxed(),
            previous: [0.0; EARSHOT_FRAME],
        }
    }
}

impl Default for EarshotSpeechScorer {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSpeechScorer for EarshotSpeechScorer {
    fn name(&self) -> &'static str {
        "earshot"
    }

    fn frame_samples(&self) -> usize {
        EARSHOT_FRAME
    }

    fn score_frame(&mut self, frame_16k: &[f32]) -> f32 {
        let Ok(raw) = <&[f32; EARSHOT_FRAME]>::try_from(frame_16k) else {
            return 0.0;
        };
        let frame = raw.map(normalized_pcm);
        let model_score = self.detector.predict_f32(&frame);
        let previous = std::mem::replace(&mut self.previous, frame);
        let (energy, adjacent) = frame_stats(&frame);
        // Centering rejects DC; white noise and isolated impulses have little
        // lag-one correlation even if their amplitude exceeds the noise floor.
        if energy < MODEL_NOISE_FLOOR || adjacent < 0.16 {
            return 0.0;
        }
        let assist: f32 = if voiced_periodicity(&previous, &frame) {
            0.95
        } else {
            0.0
        };
        assist.max(model_score).clamp(0.0, 1.0)
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

fn frame_stats(frame: &[f32]) -> (f32, f32) {
    let len = frame.len().max(1) as f32;
    let mean = frame.iter().sum::<f32>() / len;
    let energy: f32 = frame.iter().map(|x| (x - mean).powi(2)).sum();
    let adjacent: f32 = frame
        .windows(2)
        .map(|pair| (pair[0] - mean) * (pair[1] - mean))
        .sum();
    ((energy / len).sqrt(), adjacent / energy.max(1e-12))
}

fn voiced_periodicity(previous: &[f32; EARSHOT_FRAME], frame: &[f32; EARSHOT_FRAME]) -> bool {
    // Two frames span >2 periods at 80 Hz. Average before decimation to
    // limit aliasing; compare the latest frame against the preceding cycles.
    let mut signal = [0.0; EARSHOT_FRAME / 2];
    for (out, group) in signal.iter_mut().zip(
        previous
            .as_chunks::<4>()
            .0
            .iter()
            .chain(frame.as_chunks::<4>().0),
    ) {
        *out = group.iter().sum::<f32>() * 0.25;
    }
    let len = signal.len() / 2;
    let current = &signal[len..];
    let score = |lag: usize| {
        let delayed = &signal[len - lag..signal.len() - lag];
        let mean_a = current.iter().sum::<f32>() / len as f32;
        let mean_b = delayed.iter().sum::<f32>() / len as f32;
        let (mut aa, mut bb, mut ab) = (0.0, 0.0, 0.0);
        for (&a, &b) in current.iter().zip(delayed) {
            let (a, b) = (a - mean_a, b - mean_b);
            aa += a * a;
            bb += b * b;
            ab += a * b;
        }
        ab / (aa * bb).sqrt().max(1e-12)
    };
    let (mut left, mut mid) = (score(11), score(12));
    for lag in 12..=50 {
        let right = score(lag + 1);
        // A peak must also have a trough within the cycle: a smooth ramp
        // (e.g. a DC transition) should not be classified as pitched speech.
        if mid >= 0.78 && mid >= left && mid > right && score(lag / 2) < 0.3 {
            let curvature = left + right - 2.0 * mid;
            let fraction = if curvature < -1e-6 {
                (0.5 * (left - right) / curvature).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            let period = lag as f32 + fraction;
            if (4_000.0 / 340.0..=4_000.0 / 80.0).contains(&period)
                && has_harmonic_crest(&signal, period)
            {
                return true;
            }
        }
        (left, mid) = (mid, right);
    }
    false
}

fn has_harmonic_crest(signal: &[f32], period: f32) -> bool {
    let mean = signal.iter().sum::<f32>() / signal.len() as f32;
    let (mut energy, mut sum) = (0.0, 0.0);
    let mut peaks = [(0.0, 0.0); 3];
    for (i, &x) in signal.iter().enumerate() {
        // A triangular window limits leakage from nearby partials.
        let weight = 1.0 - (2.0 * i as f32 / (signal.len() - 1) as f32 - 1.0).abs();
        let x = x - mean;
        let (sin, cos) = (std::f32::consts::TAU * i as f32 / period).sin_cos();
        let (sin2, cos2) = (2.0 * sin * cos, 2.0 * cos * cos - 1.0);
        let (sin3, cos3) = (sin2 * cos + cos2 * sin, cos2 * cos - sin2 * sin);
        for ((re, im), (c, s)) in peaks
            .iter_mut()
            .zip([(cos, sin), (cos2, sin2), (cos3, sin3)])
        {
            *re += weight * x * c;
            *im += weight * x * s;
        }
        energy += weight * x * x;
        sum += weight;
    }
    let [base, second, third] =
        peaks.map(|(re, im)| (re * re + im * im) / (energy * sum).max(1e-12));
    base > 0.08 && second.max(third) > 0.012 && base + second + third > 0.24
}

/// Timing and voice thresholds for an utterance segmenter.
#[derive(Clone, Copy, Debug)]
pub struct VadSegmenterConfig {
    /// Audio retained before the first voiced frame, in milliseconds.
    pub pre_roll_ms: u64,
    /// Consecutive voiced audio required to open a span, in milliseconds.
    pub onset_ms: u64,
    /// Unvoiced audio required to close an open span, in milliseconds.
    pub hangover_ms: u64,
    /// Minimum accumulated voiced audio required to emit, in milliseconds.
    pub min_speech_ms: u64,
    /// Age of an open span at which a brief pause can end it, in milliseconds.
    pub soft_split_ms: u64,
    /// Brief pause length after `soft_split_ms`, in milliseconds.
    pub soft_pause_ms: u64,
    /// Hard upper bound on the emitted PCM duration, in milliseconds.
    pub max_utterance_ms: u64,
    /// Minimum probability from the frame scorer.
    pub speech_threshold: f32,
    /// Minimum AC RMS of the normalized analysis frame.
    pub min_rms_floor: f32,
}

impl Default for VadSegmenterConfig {
    fn default() -> Self {
        Self {
            pre_roll_ms: 320,
            onset_ms: 64,
            hangover_ms: 480,
            min_speech_ms: 240,
            soft_split_ms: 10_000,
            soft_pause_ms: 64,
            max_utterance_ms: 20_000,
            speech_threshold: 0.5,
            min_rms_floor: MODEL_NOISE_FLOOR,
        }
    }
}

fn samples(ms: u64) -> usize {
    ms.saturating_mul(SAMPLES_PER_MS).min(usize::MAX as u64) as usize
}

struct ActiveSpan {
    start_sample: u64,
    pcm: Vec<f32>,
    voiced: usize,
    pause: usize,
}

struct ChannelState<S> {
    scorer: S,
    cursor: Option<u64>,
    frame: Vec<f32>,
    analysis: Vec<f32>,
    history: VecDeque<f32>,
    onset: usize,
    active: Option<ActiveSpan>,
}

impl<S: FrameSpeechScorer> ChannelState<S> {
    fn new(scorer: S) -> Self {
        Self {
            scorer,
            cursor: None,
            frame: Vec::new(),
            analysis: Vec::new(),
            history: VecDeque::new(),
            onset: 0,
            active: None,
        }
    }

    fn reset(&mut self) {
        self.scorer.reset();
        self.cursor = None;
        self.frame.clear();
        self.history.clear();
        self.onset = 0;
        self.active = None;
    }

    fn accept(
        &mut self,
        start: u64,
        pcm: &[f32],
        config: VadSegmenterConfig,
        emit: &mut impl FnMut(ActiveSpan),
    ) {
        self.analysis.clear();
        self.analysis
            .extend(pcm.iter().copied().map(normalized_pcm));
        self.analysis.resize(self.scorer.frame_samples(), 0.0);
        let score = self.scorer.score_frame(&self.analysis);
        let voiced = score >= config.speech_threshold
            && frame_stats(&self.analysis[..pcm.len()]).0 > config.min_rms_floor;

        let cap = samples(config.max_utterance_ms).max(1);
        let (mut start, mut pcm) = (start, pcm);
        while !pcm.is_empty() {
            if let Some(active) = &mut self.active {
                let count = pcm.len().min(cap - active.pcm.len());
                active.pcm.extend_from_slice(&pcm[..count]);
                if voiced {
                    active.voiced += count;
                    active.pause = 0;
                } else {
                    active.pause += count;
                }
                let hit_cap = active.pcm.len() == cap;
                let split = hit_cap
                    || (!voiced
                        && (active.pause >= samples(config.hangover_ms)
                            || (active.pcm.len() >= samples(config.soft_split_ms)
                                && active.pause >= samples(config.soft_pause_ms))));
                if split {
                    self.finish(config, emit);
                    if hit_cap && voiced {
                        // Preserve speech continuity across a hard max-utterance cap split
                        // so the next voiced frame opens immediately without an onset gap.
                        self.onset = samples(config.onset_ms);
                    }
                }
                start = start.saturating_add(count as u64);
                pcm = &pcm[count..];
            } else {
                self.onset = if voiced { self.onset + pcm.len() } else { 0 };
                self.history.extend(pcm);
                let keep = samples(config.pre_roll_ms)
                    .saturating_add(self.onset)
                    .min(cap);
                self.history
                    .drain(..self.history.len().saturating_sub(keep));
                if voiced && self.onset >= samples(config.onset_ms) {
                    let start_sample = start
                        .saturating_add(pcm.len() as u64)
                        .saturating_sub(self.history.len() as u64);
                    self.active = Some(ActiveSpan {
                        start_sample,
                        voiced: self.onset.min(self.history.len()),
                        pcm: self.history.drain(..).collect(),
                        pause: 0,
                    });
                    self.onset = 0;
                    if keep == cap {
                        self.finish(config, emit);
                    }
                }
                break;
            }
        }
    }

    fn finish(&mut self, config: VadSegmenterConfig, emit: &mut impl FnMut(ActiveSpan)) {
        if let Some(span) = self.active.take() {
            if span.voiced >= samples(config.min_speech_ms) {
                emit(span);
            }
        }
    }

    fn drain(&mut self, config: VadSegmenterConfig, emit: &mut impl FnMut(ActiveSpan)) {
        if !self.frame.is_empty() {
            let frame = std::mem::take(&mut self.frame);
            let start = self.cursor.unwrap_or(0).saturating_sub(frame.len() as u64);
            self.accept(start, &frame, config, emit);
            self.frame = frame;
        }
        self.finish(config, emit);
        self.reset();
    }
}

/// Streams raw PCM through independent per-channel VAD and utterance buffers.
///
/// Timestamps are monotonic within each channel. Sub-millisecond chunk starts
/// are reconstructed from sample counts; forward gaps close the current span.
pub struct VadUtteranceSegmenter<S: FrameSpeechScorer = EarshotSpeechScorer> {
    config: VadSegmenterConfig,
    me: ChannelState<S>,
    others: ChannelState<S>,
}

impl VadUtteranceSegmenter {
    /// Creates an Earshot-based segmenter with custom timing and thresholds.
    pub fn new(config: VadSegmenterConfig) -> Self {
        Self::with_scorers(
            config,
            EarshotSpeechScorer::new(),
            EarshotSpeechScorer::new(),
        )
    }
}

impl Default for VadUtteranceSegmenter {
    fn default() -> Self {
        Self::new(VadSegmenterConfig::default())
    }
}

impl<S: FrameSpeechScorer> VadUtteranceSegmenter<S> {
    /// Creates a segmenter with a fresh scorer instance for each channel.
    pub fn with_scorers(config: VadSegmenterConfig, me: S, others: S) -> Self {
        Self {
            config,
            me: ChannelState::new(me),
            others: ChannelState::new(others),
        }
    }

    /// Discards all buffered PCM and resets both scorers and clocks.
    pub fn reset(&mut self) {
        self.me.reset();
        self.others.reset();
    }
}

impl<S: FrameSpeechScorer> UtteranceSegmenter for VadUtteranceSegmenter<S> {
    fn name(&self) -> &'static str {
        self.me.scorer.name()
    }

    fn push_chunk(
        &mut self,
        channel: SpeakerChannel,
        mut pcm: &[f32],
        chunk_start_ms: u64,
    ) -> Vec<UtteranceSpan> {
        let state = match channel {
            SpeakerChannel::Me => &mut self.me,
            SpeakerChannel::Others => &mut self.others,
        };
        let frame_size = state.scorer.frame_samples();
        let mut spans = Vec::new();
        let mut emit = |span| spans.push(to_span(channel, span));
        if pcm.is_empty() || frame_size == 0 {
            return spans;
        }
        let supplied = chunk_start_ms.saturating_mul(SAMPLES_PER_MS);
        if state.cursor.is_some_and(|next| supplied > next) {
            state.drain(self.config, &mut emit);
        }
        let mut cursor = state.cursor.unwrap_or(supplied);
        let mut frame = std::mem::take(&mut state.frame);
        while !pcm.is_empty() {
            let count = pcm.len().min(frame_size - frame.len());
            frame.extend_from_slice(&pcm[..count]);
            cursor = cursor.saturating_add(count as u64);
            pcm = &pcm[count..];
            if frame.len() == frame_size {
                state.accept(cursor - frame_size as u64, &frame, self.config, &mut emit);
                frame.clear();
            }
        }
        state.frame = frame;
        state.cursor = Some(cursor);
        spans
    }

    fn flush(&mut self) -> Vec<UtteranceSpan> {
        let mut spans = Vec::new();
        self.me.drain(self.config, &mut |span| {
            spans.push(to_span(SpeakerChannel::Me, span));
        });
        self.others.drain(self.config, &mut |span| {
            spans.push(to_span(SpeakerChannel::Others, span));
        });
        spans.sort_by_key(|span| span.start_ms);
        spans
    }
}

fn to_span(channel: SpeakerChannel, span: ActiveSpan) -> UtteranceSpan {
    UtteranceSpan {
        channel,
        start_ms: span.start_sample / SAMPLES_PER_MS,
        end_ms: span.start_sample.saturating_add(span.pcm.len() as u64) / SAMPLES_PER_MS,
        samples_16k: span.pcm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_assist_needs_overtones_not_just_a_periodic_fundamental() {
        let pitch = 125.0;
        let wave = |overtone: f32, start: usize| {
            std::array::from_fn(|i| {
                let phase = std::f32::consts::TAU * pitch * (start + i) as f32 / 16_000.0;
                phase.sin() + overtone * (2.0 * phase).sin()
            })
        };
        assert!(!voiced_periodicity(
            &wave(0.0, 0),
            &wave(0.0, EARSHOT_FRAME)
        ));
        assert!(voiced_periodicity(&wave(0.4, 0), &wave(0.4, EARSHOT_FRAME)));
    }

    #[test]
    fn max_utterance_cap_continues_voiced_speech_without_onset_gap() {
        struct AlwaysVoicedScorer;
        impl FrameSpeechScorer for AlwaysVoicedScorer {
            fn name(&self) -> &'static str {
                "always_voiced"
            }
            fn frame_samples(&self) -> usize {
                160
            }
            fn score_frame(&mut self, _frame_16k: &[f32]) -> f32 {
                0.95
            }
            fn reset(&mut self) {}
        }

        let cfg = VadSegmenterConfig {
            max_utterance_ms: 1_000,
            onset_ms: 80,
            pre_roll_ms: 0,
            min_speech_ms: 200,
            ..VadSegmenterConfig::default()
        };
        let mut seg =
            VadUtteranceSegmenter::with_scorers(cfg, AlwaysVoicedScorer, AlwaysVoicedScorer);
        // Feed 1.5 s of continuous voiced AC audio in one push.
        let total_samples = 24_000usize;
        let pcm: Vec<f32> = (0..total_samples)
            .map(|i| (i as f32 * 0.08).sin() * 0.25)
            .collect();
        let mut spans = seg.push_chunk(SpeakerChannel::Me, &pcm, 0);
        spans.extend(seg.flush());

        assert_eq!(spans.len(), 2, "expected 1.0s cap span + 0.5s continuation");
        let covered_samples: usize = spans.iter().map(|s| s.samples_16k.len()).sum();
        assert_eq!(
            covered_samples, total_samples,
            "expected zero dropped samples across max-utterance cap split"
        );
        assert_eq!(spans[0].start_ms, 0);
        assert_eq!(spans[0].end_ms, 1_000);
        assert_eq!(spans[1].start_ms, 1_000);
        assert_eq!(spans[1].end_ms, 1_500);
    }
}
