//! Long-form chunked decoding, shared by a live recording and a batch clip:
//! both cut the same audio at the same pauses and decode it the same way.

use std::time::Instant;

use log::info;
use transcribe_cpp::Transcript;

use super::{RunSetup, TranscriptionManager};
use crate::audio_toolkit::chunking::Chunker;
use crate::engine_supervisor::{CancelMark, EngineError, LoadedInfo};
use crate::language_detection::Plan;
use crate::managers::audio::create_detector;
use crate::settings::{AppSettings, VadBackend};

/// How a chunked decode's runs are set up: known up front (a fixed language),
/// or resolved from chunk audio (once on Chunk 1, or per chunk in Auto mode).
pub(super) enum ChunkSetup {
    Ready(Box<RunSetup>),
    Pending(Box<PendingSetup>),
}

/// What a chunked decode's setup is resolved from.
pub(super) struct PendingSetup {
    pub plan: Plan,
    pub info: LoadedInfo,
    pub settings: AppSettings,
}

impl PendingSetup {
    /// In `"auto"` mode, resolve language per chunk rather than locking every
    /// chunk of a long recording to Chunk 1's language.
    pub(super) fn per_chunk(&self) -> bool {
        self.settings
            .selected_language
            .trim()
            .eq_ignore_ascii_case("auto")
    }
}

impl ChunkSetup {
    /// Resolves the setup for `audio`. When `Pending` is in per-chunk mode,
    /// computes a fresh `RunSetup` for this chunk into `last_setup` while
    /// keeping `Self::Pending` for subsequent chunks.
    fn resolve_mut<'s>(
        &'s mut self,
        last_setup: &'s mut Option<Box<RunSetup>>,
        tm: &TranscriptionManager,
        audio: &[f32],
    ) -> &'s mut RunSetup {
        self.resolve_mut_with(
            last_setup,
            |pending, pcm| tm.setup_for(&pending.info, &pending.settings, &pending.plan, pcm),
            audio,
        )
    }

    fn resolve_mut_with<'s>(
        &'s mut self,
        last_setup: &'s mut Option<Box<RunSetup>>,
        resolve_fn: impl FnOnce(&PendingSetup, &[f32]) -> RunSetup,
        audio: &[f32],
    ) -> &'s mut RunSetup {
        match self {
            Self::Ready(setup) => &mut **setup,
            Self::Pending(pending) if pending.per_chunk() => {
                let setup = resolve_fn(pending, audio);
                &mut **last_setup.insert(Box::new(setup))
            }
            Self::Pending(pending) => {
                let setup = resolve_fn(pending, audio);
                *self = Self::Ready(Box::new(setup));
                match self {
                    Self::Ready(setup) => &mut **setup,
                    Self::Pending(_) => unreachable!("resolved above"),
                }
            }
        }
    }

    fn into_final_setup(
        self,
        last_setup: Option<Box<RunSetup>>,
        tm: &TranscriptionManager,
    ) -> RunSetup {
        self.into_final_setup_with(last_setup, |pending, pcm| {
            tm.setup_for(&pending.info, &pending.settings, &pending.plan, pcm)
        })
    }

    fn into_final_setup_with(
        self,
        last_setup: Option<Box<RunSetup>>,
        resolve_fn: impl FnOnce(&PendingSetup, &[f32]) -> RunSetup,
    ) -> RunSetup {
        match self {
            Self::Ready(setup) => *setup,
            Self::Pending(pending) => match last_setup {
                Some(setup) => *setup,
                None => resolve_fn(&pending, &[]),
            },
        }
    }
}

/// Cuts audio at pauses as it arrives and decodes each chunk once its cut is
/// certain, so a recording has only its last chunk left when it stops.
pub(super) struct ChunkedDecode<'a> {
    tm: &'a TranscriptionManager,
    /// Every chunk's run stops with one cancel.
    mark: CancelMark,
    setup: ChunkSetup,
    /// Most recent per-chunk setup when `setup` stays `Pending` across chunks.
    last_setup: Option<Box<RunSetup>>,
    model_id: String,
    chunker: Chunker,
    transcripts: Vec<Transcript>,
    /// The first run that failed or was cancelled; nothing more is decoded
    /// after it.
    failure: Option<EngineError>,
}

impl<'a> ChunkedDecode<'a> {
    pub(super) fn new(
        tm: &'a TranscriptionManager,
        mark: CancelMark,
        setup: ChunkSetup,
        model_id: &str,
        backend: VadBackend,
    ) -> Self {
        // The VAD is only loaded if the audio turns out long enough to cut.
        let app_handle = tm.app_handle.clone();
        Self {
            tm,
            mark,
            setup,
            last_setup: None,
            model_id: model_id.to_string(),
            chunker: Chunker::new(Box::new(move || create_detector(backend, &app_handle))),
            transcripts: Vec::new(),
            failure: None,
        }
    }

    /// A whole clip's chunks, decoded in order, and the setup they ran with.
    pub(super) fn decode_clip(
        tm: &'a TranscriptionManager,
        mark: CancelMark,
        setup: ChunkSetup,
        audio: Vec<f32>,
        model_id: &str,
        backend: VadBackend,
    ) -> Result<(Vec<Transcript>, RunSetup), EngineError> {
        let mut decode = Self::new(tm, mark, setup, model_id, backend);
        decode.push(audio);
        decode.finish()
    }

    /// Samples pushed so far.
    pub(super) fn samples(&self) -> usize {
        self.chunker.samples()
    }

    /// Chunks decoded so far.
    pub(super) fn decoded(&self) -> usize {
        self.transcripts.len()
    }

    /// Whether a failure already ended the decode; [`finish`](Self::finish)
    /// reports it.
    pub(super) fn failed(&self) -> bool {
        self.failure.is_some()
    }

    /// Adds audio, decoding each chunk it makes certain.
    pub(super) fn push(&mut self, pcm: Vec<f32>) {
        if self.failure.is_some() {
            return;
        }
        let chunks = self.chunker.push(pcm);
        self.decode(chunks);
    }

    /// Decodes what is left. Every chunk's transcript in order and the setup
    /// they ran with, or the run that failed or was cancelled.
    pub(super) fn finish(mut self) -> Result<(Vec<Transcript>, RunSetup), EngineError> {
        if self.failure.is_none() {
            let chunks = self.chunker.finish();
            self.decode(chunks);
        }
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if self.transcripts.len() > 1 {
            info!(
                "Decoded {:.1}s of audio in {} chunks",
                self.samples() as f64 / 16_000.0,
                self.transcripts.len()
            );
        }
        // No chunk at all (no audio) still needs a setup to finish with.
        let setup = self.setup.into_final_setup(self.last_setup, self.tm);
        Ok((self.transcripts, setup))
    }

    fn decode(&mut self, chunks: Vec<Vec<f32>>) {
        let tm = self.tm;
        let mark = self.mark;
        for chunk in chunks {
            if self.failure.is_some() {
                return;
            }
            let setup = self.setup.resolve_mut(&mut self.last_setup, tm, &chunk);
            let audio_secs = chunk.len() as f64 / 16_000.0;
            let started = Instant::now();
            match tm.decode_chunk_for_spoken_languages(mark, chunk, setup, &self.model_id) {
                Ok(transcript) => {
                    self.transcripts.push(transcript);
                    // Real-time factor: audio seconds decoded per second of
                    // wall clock. Floored so a near-instant decode reports a
                    // large factor instead of infinity.
                    let elapsed_secs = started.elapsed().as_secs_f64().max(1e-6);
                    info!(
                        "Decoded chunk ({:.1}s of audio) in {:.2}s ({:.0}x real time)",
                        audio_secs,
                        elapsed_secs,
                        audio_secs / elapsed_secs
                    );
                }
                Err(e) => {
                    info!("Chunk decoding stopped: {}", e);
                    self.failure = Some(e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use transcribe_cpp::{Capabilities, TimestampKind};

    use super::*;
    use crate::flow::multilingual::HindustaniScript;
    use crate::language_detection::{LanguageDecision, LanguageSource};
    use crate::managers::transcription::run_setup;

    fn sample_info() -> LoadedInfo {
        LoadedInfo {
            arch: "whisper".to_string(),
            variant: "large-v3".to_string(),
            backend: "metal".to_string(),
            device: "Apple M3".to_string(),
            on_gpu: true,
            capabilities: Capabilities {
                native_sample_rate: 16_000,
                languages: vec!["en".to_string(), "ur".to_string(), "hi".to_string()],
                translate_target_languages: vec!["en".to_string()],
                max_timestamp_kind: TimestampKind::Word,
                supports_language_detect: true,
                supports_translate: true,
                supports_streaming: false,
                supports_spec_decode: false,
                max_audio_ms: 30_000,
            },
            supports_initial_prompt: true,
            supports_long_form: true,
            supports_vocabulary: true,
        }
    }

    #[test]
    fn pending_setup_resolves_per_chunk_in_auto_mode() {
        let info = sample_info();
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            spoken_languages: vec!["en".to_string(), "ur".to_string()],
            ..Default::default()
        };
        let mut chunk_setup = ChunkSetup::Pending(Box::new(PendingSetup {
            plan: Plan::Native,
            info,
            settings,
        }));
        let mut last_setup = None;
        let calls = Cell::new(0usize);

        let lang_chunk_1 = {
            let setup = chunk_setup.resolve_mut_with(
                &mut last_setup,
                |pending, _pcm| {
                    calls.set(calls.get() + 1);
                    run_setup(
                        &pending.info,
                        &pending.settings,
                        LanguageDecision {
                            language: Some("en".to_string()),
                            source: LanguageSource::Detected,
                        },
                        HindustaniScript::Auto,
                    )
                },
                &[0.1; 160],
            );
            setup.run_options.language.clone()
        };
        assert_eq!(lang_chunk_1.as_deref(), Some("en"));
        assert!(matches!(chunk_setup, ChunkSetup::Pending(_)));

        let lang_chunk_2 = {
            let setup = chunk_setup.resolve_mut_with(
                &mut last_setup,
                |pending, _pcm| {
                    calls.set(calls.get() + 1);
                    run_setup(
                        &pending.info,
                        &pending.settings,
                        LanguageDecision {
                            language: Some("ur".to_string()),
                            source: LanguageSource::Detected,
                        },
                        HindustaniScript::Auto,
                    )
                },
                &[0.2; 160],
            );
            setup.run_options.language.clone()
        };
        assert_eq!(lang_chunk_2.as_deref(), Some("ur"));
        assert_eq!(calls.get(), 2);

        let final_setup = chunk_setup.into_final_setup_with(last_setup, |_pending, _pcm| {
            panic!("last_setup should already hold chunk 2's setup")
        });
        assert_eq!(final_setup.run_options.language.as_deref(), Some("ur"));
    }

    #[test]
    fn pending_setup_locks_after_first_chunk_when_language_is_explicit() {
        let info = sample_info();
        let settings = AppSettings {
            selected_language: "en".to_string(),
            ..Default::default()
        };
        let mut chunk_setup = ChunkSetup::Pending(Box::new(PendingSetup {
            plan: Plan::Fixed {
                language: "en".to_string(),
                source: LanguageSource::Selected,
            },
            info,
            settings,
        }));
        let mut last_setup = None;
        let calls = Cell::new(0usize);

        for _ in 0..2 {
            let setup = chunk_setup.resolve_mut_with(
                &mut last_setup,
                |pending, _pcm| {
                    calls.set(calls.get() + 1);
                    run_setup(
                        &pending.info,
                        &pending.settings,
                        LanguageDecision {
                            language: Some("en".to_string()),
                            source: LanguageSource::Selected,
                        },
                        HindustaniScript::Auto,
                    )
                },
                &[0.1; 160],
            );
            assert_eq!(setup.run_options.language.as_deref(), Some("en"));
        }

        assert_eq!(calls.get(), 1);
        assert!(matches!(chunk_setup, ChunkSetup::Ready(_)));
    }
}
