use crate::meeting::aec_guard::FRAME_SAMPLES_16K;
use crate::meeting::macos_bridge;
use crate::meeting::types::{
    ActiveDualCapture, AudioCaptureMode, DualAudioCaptureBackend, DualPcmChunk, OutputRouteKind,
};
use handy_recorder::{AudioChunk, Channels, ErrorKind, Recorder, RecorderConfig, Sink};
use log::warn;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const SAMPLE_RATE_16K: usize = 16_000;
const RING_CAPACITY_SAMPLES: usize = 30 * SAMPLE_RATE_16K;
const MAX_DRAIN_SAMPLES: usize = RING_CAPACITY_SAMPLES;

#[derive(Debug, Default)]
struct BoundedPcmRingBuffer {
    samples: VecDeque<f32>,
    capacity: usize,
}

impl BoundedPcmRingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    fn push(&mut self, chunk: &[f32]) {
        if chunk.is_empty() {
            return;
        }
        if chunk.len() >= self.capacity {
            self.samples.clear();
            self.samples
                .extend(chunk[chunk.len() - self.capacity..].iter().copied());
            return;
        }

        let overflow = self
            .samples
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.capacity);
        if overflow > 0 {
            self.samples.drain(..overflow);
        }
        self.samples.extend(chunk.iter().copied());
    }

    fn drain_up_to(&mut self, max_samples: usize) -> Vec<f32> {
        let amount = max_samples.min(self.samples.len());
        self.samples.drain(..amount).collect()
    }
}

fn align_dual_channels(mut me_16k: Vec<f32>, mut others_16k: Vec<f32>) -> (Vec<f32>, Vec<f32>) {
    let target = me_16k.len().max(others_16k.len());
    me_16k.resize(target, 0.0);
    others_16k.resize(target, 0.0);
    (me_16k, others_16k)
}

#[derive(Debug)]
struct MicRingSink {
    ring: Arc<Mutex<BoundedPcmRingBuffer>>,
}

impl MicRingSink {
    fn new(ring: Arc<Mutex<BoundedPcmRingBuffer>>) -> Self {
        Self { ring }
    }
}

impl Sink for MicRingSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        let channels = usize::from(chunk.channels.max(1));
        let frame_count = chunk.valid_frames.min(chunk.samples.len() / channels);
        if frame_count == 0 {
            return;
        }

        let mut mono = Vec::with_capacity(frame_count);
        if channels == 1 {
            mono.extend_from_slice(&chunk.samples[..frame_count]);
        } else {
            for frame in 0..frame_count {
                let base = frame * channels;
                let mut acc = 0.0_f32;
                for channel in 0..channels {
                    acc += chunk.samples[base + channel];
                }
                mono.push(acc / channels as f32);
            }
        }

        match self.ring.lock() {
            Ok(mut ring) => ring.push(&mono),
            Err(poisoned) => poisoned.into_inner().push(&mono),
        }
    }
}

struct ActiveSystemDualCapture {
    capture_mode: AudioCaptureMode,
    output_route: OutputRouteKind,
    mic_recorder: Option<Recorder<MicRingSink>>,
    mic_ring: Arc<Mutex<BoundedPcmRingBuffer>>,
    others_ring: BoundedPcmRingBuffer,
    system_tap: Option<macos_bridge::SystemAudioTap>,
}

impl ActiveSystemDualCapture {
    fn start() -> Result<Self, String> {
        let mic_ring = Arc::new(Mutex::new(BoundedPcmRingBuffer::new(RING_CAPACITY_SAMPLES)));
        let sink = MicRingSink::new(Arc::clone(&mic_ring));

        let config = RecorderConfig {
            sample_rate: Some(SAMPLE_RATE_16K as u32),
            channels: Channels::MixToMono,
            frames_per_chunk: Some(FRAME_SAMPLES_16K),
            ..RecorderConfig::speech()
        };

        let recorder = Recorder::<MicRingSink>::open(config)
            .map_err(|error| format!("failed to open meeting microphone stream: {error}"))?;
        if let Err(start_error) = recorder.start(sink) {
            return Err(format!(
                "failed to start meeting microphone stream: {}",
                start_error.error
            ));
        }

        let system_tap = macos_bridge::SystemAudioTap::start();
        let capture_mode = if system_tap.is_some() {
            AudioCaptureMode::DualStream
        } else {
            AudioCaptureMode::MicOnlyFallback
        };

        Ok(Self {
            capture_mode,
            output_route: OutputRouteKind::Unknown,
            mic_recorder: Some(recorder),
            mic_ring,
            others_ring: BoundedPcmRingBuffer::new(RING_CAPACITY_SAMPLES),
            system_tap,
        })
    }

    fn drain_mic_ring(&self) -> Vec<f32> {
        match self.mic_ring.lock() {
            Ok(mut ring) => ring.drain_up_to(MAX_DRAIN_SAMPLES),
            Err(poisoned) => poisoned.into_inner().drain_up_to(MAX_DRAIN_SAMPLES),
        }
    }

    fn drain_tap_into_ring(&mut self) {
        if let Some(tap) = self.system_tap.as_mut() {
            let drained = tap.drain(MAX_DRAIN_SAMPLES);
            if !drained.is_empty() {
                self.others_ring.push(&drained);
            }
        }
    }
}

impl ActiveDualCapture for ActiveSystemDualCapture {
    fn capture_mode(&self) -> AudioCaptureMode {
        self.capture_mode
    }

    fn output_route(&self) -> OutputRouteKind {
        self.output_route
    }

    fn drain_16k_frames(&mut self) -> DualPcmChunk {
        let route = macos_bridge::detect_output_route();
        self.output_route = route;

        let me_16k = self.drain_mic_ring();

        if self.capture_mode == AudioCaptureMode::DualStream {
            self.drain_tap_into_ring();
            let others_16k = self.others_ring.drain_up_to(MAX_DRAIN_SAMPLES);
            let (me_aligned, others_aligned) = align_dual_channels(me_16k, others_16k);
            return DualPcmChunk {
                me_16k: me_aligned,
                others_16k: others_aligned,
                output_route: route,
            };
        }

        DualPcmChunk {
            me_16k,
            others_16k: Vec::new(),
            output_route: route,
        }
    }

    fn stop(&mut self) {
        if let Some(recorder) = self.mic_recorder.take() {
            if let Err(error) = recorder.stop() {
                if error.kind() != ErrorKind::NotRecording {
                    warn!("meeting mic recorder stop failed: {error}");
                }
            }
            if let Err(error) = recorder.close() {
                warn!("meeting mic recorder close failed: {error}");
            }
        }

        if let Some(system_tap) = self.system_tap.take() {
            system_tap.stop();
        }
    }
}

impl Drop for ActiveSystemDualCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDualAudioBackend;

impl DualAudioCaptureBackend for SystemDualAudioBackend {
    fn start_capture(&self) -> Result<Box<dyn ActiveDualCapture>, String> {
        ActiveSystemDualCapture::start()
            .map(|capture| Box::new(capture) as Box<dyn ActiveDualCapture>)
    }
}

#[cfg(test)]
mod tests {
    use super::{align_dual_channels, BoundedPcmRingBuffer};

    #[test]
    fn bounded_ring_buffer_overflow_keeps_latest_samples() {
        let mut ring = BoundedPcmRingBuffer::new(5);
        ring.push(&[1.0, 2.0, 3.0, 4.0]);
        ring.push(&[5.0, 6.0, 7.0, 8.0]);
        assert_eq!(ring.drain_up_to(10), vec![4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn align_dual_channels_pads_shorter_side() {
        let (me, others) = align_dual_channels(vec![1.0, 2.0, 3.0], vec![9.0]);
        assert_eq!(me, vec![1.0, 2.0, 3.0]);
        assert_eq!(others, vec![9.0, 0.0, 0.0]);
    }

    #[test]
    fn align_dual_channels_handles_remote_only_audio() {
        let (me, others) = align_dual_channels(Vec::new(), vec![0.2, 0.4, 0.6]);
        assert_eq!(me, vec![0.0, 0.0, 0.0]);
        assert_eq!(others, vec![0.2, 0.4, 0.6]);
    }
}
