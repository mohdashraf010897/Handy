use std::sync::{mpsc, Arc};

use handy_recorder::{AudioChunk, Sink};

use crate::audio_toolkit::{
    audio::AudioVisualiser,
    constants,
    vad::{NoiseGatedAgc, VadFrame, VoiceActivityDetector},
};
use crate::flow::session_wal::{PauseController, RecordingWal};

/// How 16 kHz mono frames should be filtered for one recording session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VadPolicy {
    /// Bypass VAD and forward every frame.
    Disabled,
    /// Current offline-tuned VAD profile.
    Offline,
    /// VAD profile with a longer post-speech tail for streaming-capable models.
    Streaming,
}

/// A single VAD engine plus the two hangover-tail lengths its smoothing wrapper
/// should use. The offline and streaming policies are never active
/// concurrently, so one detector is reconfigured per recording rather than
/// kept as two resident engines.
pub struct CaptureVad {
    detector: Box<dyn VoiceActivityDetector>,
    offline_hangover_frames: usize,
    streaming_hangover_frames: usize,
}

impl CaptureVad {
    pub fn new(
        detector: Box<dyn VoiceActivityDetector>,
        offline_hangover_frames: usize,
        streaming_hangover_frames: usize,
    ) -> Self {
        assert!(
            detector.frame_samples() > 0,
            "VAD frame size must be non-zero"
        );
        Self {
            detector,
            offline_hangover_frames,
            streaming_hangover_frames,
        }
    }

    /// Samples per detector frame; the recorder delivers chunks of this size.
    pub fn frame_samples(&self) -> usize {
        self.detector.frame_samples()
    }

    /// Post-speech hangover tail (in detector frames) for the given policy.
    /// `Disabled` never reaches the detector, so it maps to the offline value.
    fn hangover_for(&self, policy: VadPolicy) -> usize {
        match policy {
            VadPolicy::Streaming => self.streaming_hangover_frames,
            VadPolicy::Offline | VadPolicy::Disabled => self.offline_hangover_frames,
        }
    }
}

/// Receives spectrum levels for the recording overlay.
pub type LevelCallback = Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>;
/// Receives each 16 kHz mono frame that passes the recording's VAD policy, in
/// order. Used to feed live streaming transcription as audio arrives.
pub type AudioFrameCallback = Arc<dyn Fn(&[f32]) + Send + Sync + 'static>;

/// What a finished recording hands back.
pub struct CapturedAudio {
    /// 16 kHz mono samples that passed the VAD policy.
    pub samples: Vec<f32>,
    /// Every sample received (pre-VAD) was exactly zero, e.g. a muted mic.
    /// False when no audio arrived at all.
    pub input_silent: bool,
    /// The detector the recording borrowed, for the next one.
    pub vad: CaptureVad,
    /// Crash-recovery write-ahead log to commit and remove once recording finishes.
    pub wal: Option<RecordingWal>,
}

/// One recording's consumer of 16 kHz mono audio from handy-recorder. Runs on
/// the recorder's delivery thread, never the real-time audio thread, so it
/// can run VAD inline. Chunks are exactly one VAD frame long.
pub struct CaptureSink {
    vad: CaptureVad,
    policy: VadPolicy,
    visualizer: AudioVisualiser,
    on_levels: LevelCallback,
    on_audio: AudioFrameCallback,
    /// Signalled on the first chunk: the device is delivering audio. Silence
    /// counts; readiness is not speech.
    ready: Option<mpsc::Sender<()>>,
    samples: Vec<f32>,
    heard_sound: bool,
    emitted_speech: bool,
    noise_gap_samples: usize,
    pause_controller: Option<Arc<PauseController>>,
    agc: Option<NoiseGatedAgc>,
    agc_scratch: Vec<f32>,
    wal: Option<RecordingWal>,
}

/// Spectrum window at 16 kHz: 32 ms, near the overlay's ~30 Hz refresh.
const VISUALIZER_WINDOW: usize = 512;
const VISUALIZER_BUCKETS: usize = 16;
/// Minimum post-hangover VAD-rejected silence (`~192 ms` at 16 kHz) between two
/// speech bursts before inserting a clean silence separator so downstream
/// clause/long-form chunkers preserve natural sentence and code-switch pauses.
const MIN_INTER_CLAUSE_GAP_SAMPLES: usize = (constants::WHISPER_SAMPLE_RATE as usize * 192) / 1000;
/// Clean digital-zero pause (`384 ms` = `6,144` samples, an exact multiple of
/// 512-sample Silero frames) inserted lazily only when speech resumes after a
/// genuine inter-clause pause. Trailing silence at recording end emits nothing.
const INTER_CLAUSE_SEPARATOR_SAMPLES: usize = 12 * 512;

impl CaptureSink {
    /// A sink for one recording. Resets the detector and applies the policy's
    /// hangover tail, so no state carries over from the previous recording.
    pub fn new(
        mut vad: CaptureVad,
        policy: VadPolicy,
        on_levels: LevelCallback,
        on_audio: AudioFrameCallback,
        ready: mpsc::Sender<()>,
    ) -> Self {
        if policy != VadPolicy::Disabled {
            let hangover = vad.hangover_for(policy);
            vad.detector.set_hangover_frames(hangover);
            vad.detector.reset();
        }
        Self {
            vad,
            policy,
            visualizer: AudioVisualiser::new(
                constants::WHISPER_SAMPLE_RATE,
                VISUALIZER_WINDOW,
                VISUALIZER_BUCKETS,
                400.0,
                4000.0,
            ),
            on_levels,
            on_audio,
            ready: Some(ready),
            samples: Vec::new(),
            heard_sound: false,
            emitted_speech: false,
            noise_gap_samples: 0,
            pause_controller: None,
            agc: None,
            agc_scratch: Vec::new(),
            wal: None,
        }
    }

    /// Attaches Wave 2 session controls (Pause ⇄ Resume gate, noise-gated AGC,
    /// and 10-second crash-recovery PCM WAL) to this capture session.
    pub fn with_controls(
        mut self,
        pause_controller: Option<Arc<PauseController>>,
        agc: Option<NoiseGatedAgc>,
        wal: Option<RecordingWal>,
    ) -> Self {
        let scratch_cap = if agc.is_some() {
            self.vad.frame_samples().max(VISUALIZER_WINDOW)
        } else {
            0
        };
        self.pause_controller = pause_controller;
        self.agc = agc;
        self.agc_scratch = Vec::with_capacity(scratch_cap);
        self.wal = wal;
        self
    }

    /// The detector, from a sink that never recorded (its start failed).
    pub fn into_vad(mut self) -> CaptureVad {
        if let Some(wal) = self.wal.take() {
            let _ = wal.commit_and_remove();
        }
        self.vad
    }

    /// Hands back the recording and the detector. Logs what VAD still held
    /// back when the recording stopped.
    pub fn finish(self) -> CapturedAudio {
        // Diagnostic for audio still withheld when capture stopped; it is not
        // conclusive in either direction.
        if self.policy != VadPolicy::Disabled {
            if let Some(report) = self.vad.detector.tail_report() {
                log::debug!(
                    "VAD at stop: withheld tail {} frames (~{}ms, {} voiced), in_speech={}, onset_counter={}, hangover_counter={}",
                    report.withheld_frames,
                    report.withheld_frames * self.vad.frame_samples() * 1000
                        / constants::WHISPER_SAMPLE_RATE as usize,
                    report.withheld_voiced_frames,
                    report.in_speech,
                    report.onset_counter,
                    report.hangover_counter
                );
            }
        }
        CapturedAudio {
            samples: self.samples,
            // `ready` is spent on the first chunk, so `None` means audio came.
            input_silent: self.ready.is_none() && !self.heard_sound,
            vad: self.vad,
            wal: self.wal,
        }
    }

    fn emit(
        samples: &mut Vec<f32>,
        on_audio: &AudioFrameCallback,
        wal: &mut Option<RecordingWal>,
        frame: &[f32],
    ) {
        samples.extend_from_slice(frame);
        if let Some(wal) = wal.as_mut() {
            if let Err(err) = wal.push_samples(frame) {
                log::warn!("Recording WAL segment write failed: {err}");
            }
        }
        on_audio(frame);
    }
}

impl Sink for CaptureSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(());
        }
        if self
            .pause_controller
            .as_ref()
            .is_some_and(|ctrl| ctrl.is_paused())
        {
            return;
        }
        if !self.heard_sound && !chunk.is_digital_silence() {
            self.heard_sound = true;
        }

        // Apply optional noise-gated AGC into reusable scratch storage so soft
        // speech is lifted cleanly ahead of visualizer, VAD, ASR, and WAL.
        let processed: &[f32] = if let Some(agc) = self.agc.as_mut() {
            self.agc_scratch.clear();
            self.agc_scratch.extend_from_slice(chunk.samples);
            agc.process_in_place(&mut self.agc_scratch);
            &self.agc_scratch
        } else {
            chunk.samples
        };

        // Mono by construction: the recorder mixes down or picks one channel.
        let real = &processed[..chunk.valid_frames.min(processed.len())];
        if let Some(levels) = self.visualizer.feed(real) {
            (self.on_levels)(levels);
        }

        if self.policy == VadPolicy::Disabled {
            Self::emit(&mut self.samples, &self.on_audio, &mut self.wal, real);
            return;
        }

        // The detector needs whole frames, so the final chunk goes in with its
        // zero padding; at most one frame of silence reaches the recording.
        match self
            .vad
            .detector
            .push_frame(processed)
            .unwrap_or(VadFrame::Speech(processed))
        {
            VadFrame::Speech(frame) => {
                if self.emitted_speech && self.noise_gap_samples >= MIN_INTER_CLAUSE_GAP_SAMPLES {
                    const ZERO_FRAME: [f32; 512] = [0.0; 512];
                    let mut remaining = INTER_CLAUSE_SEPARATOR_SAMPLES;
                    while remaining > 0 {
                        let step = remaining.min(ZERO_FRAME.len());
                        Self::emit(
                            &mut self.samples,
                            &self.on_audio,
                            &mut self.wal,
                            &ZERO_FRAME[..step],
                        );
                        remaining -= step;
                    }
                }
                self.emitted_speech = true;
                self.noise_gap_samples = 0;
                Self::emit(&mut self.samples, &self.on_audio, &mut self.wal, frame);
            }
            VadFrame::Noise => {
                if self.emitted_speech {
                    self.noise_gap_samples = self.noise_gap_samples.saturating_add(processed.len());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
