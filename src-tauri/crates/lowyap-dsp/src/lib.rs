//! Pure-Rust plug-and-play acoustic echo cancellation and utterance VAD adapters
//! implementing `lowyap_ports` traits with zero UI or framework dependencies.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod aec;
mod vad;

pub use aec::WebRtcAec3EchoCanceller;
pub use vad::{EarshotSpeechScorer, FrameSpeechScorer, VadSegmenterConfig, VadUtteranceSegmenter};

fn normalized_pcm(sample: f32) -> f32 {
    if sample.is_finite() && sample.abs() >= 1e-20 {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}
