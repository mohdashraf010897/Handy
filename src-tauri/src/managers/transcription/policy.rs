//! How audio is decoded: the one place the model's traits and the settings
//! choose between streaming, long-form chunking and a single run.

use crate::engine_supervisor::LoadedInfo;

/// What a model can do that decides how its audio is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelTraits {
    /// Decodes audio incrementally as it is recorded.
    pub streaming: bool,
    /// Keeps its accuracy on audio longer than its native window in one run
    /// (whisper today).
    pub long_form: bool,
}

impl ModelTraits {
    /// The loaded model's own traits: the authority once a model is loaded.
    pub fn of(info: &LoadedInfo) -> Self {
        Self {
            streaming: info.capabilities.supports_streaming,
            long_form: info.supports_long_form,
        }
    }
}

/// How a recording is decoded while it is still going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveDecode {
    /// The model decodes the audio as it arrives.
    Stream,
    /// Long audio is cut at pauses and each chunk decoded once its cut is
    /// certain.
    Chunked,
    /// Nothing until the recording stops; then it is decoded in batch.
    None,
}

/// How a whole clip is decoded: a recording without a live result, a
/// re-transcribed history entry, or a file from the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchDecode {
    /// Cut at pauses into chunks of about 30 s, each decoded in turn; a clip
    /// of 40 s or less is one chunk.
    Chunked,
    /// One run.
    Whole,
}

/// Streaming wins. Every other engine can predecode chunks while recording,
/// even when it also supports long-form batch decoding.
pub fn live_decode(model: ModelTraits, long_form_chunking: bool) -> LiveDecode {
    if model.streaming {
        LiveDecode::Stream
    } else if long_form_chunking {
        LiveDecode::Chunked
    } else {
        LiveDecode::None
    }
}

/// Preserve whole-clip decoding for streaming and long-form batch engines.
pub fn batch_decode(model: ModelTraits, long_form_chunking: bool) -> BatchDecode {
    if long_form_chunking && !model.long_form && !model.streaming {
        BatchDecode::Chunked
    } else {
        BatchDecode::Whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_policy_predecodes_every_non_streaming_engine_when_enabled() {
        for long_form in [false, true] {
            let model = ModelTraits {
                streaming: false,
                long_form,
            };
            assert_eq!(live_decode(model, true), LiveDecode::Chunked);
            assert_eq!(live_decode(model, false), LiveDecode::None);
        }
    }

    #[test]
    fn streaming_wins_and_batch_policy_is_unchanged() {
        for streaming in [false, true] {
            for long_form in [false, true] {
                for enabled in [false, true] {
                    let model = ModelTraits {
                        streaming,
                        long_form,
                    };
                    if streaming {
                        assert_eq!(live_decode(model, enabled), LiveDecode::Stream);
                    }
                    assert_eq!(
                        batch_decode(model, enabled),
                        if enabled && !streaming && !long_form {
                            BatchDecode::Chunked
                        } else {
                            BatchDecode::Whole
                        }
                    );
                }
            }
        }
    }
}
