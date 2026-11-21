//! Adapters bridging `lowyap_ports`, `lowyap_dsp`, `lowyap_diarize`, and
//! `lowyap_summarize` into the meeting session pipeline.

use super::{aec_guard::SpeakerEchoGuard, summarizer, types};
use crate::audio_toolkit::vad::{SileroVad, SpeechScorer, VoiceActivityDetector};
use crate::flow::multilingual::{llm_script_instruction, HindustaniScript};
use crate::settings::{get_settings, VadBackend, APPLE_INTELLIGENCE_PROVIDER_ID};
use log::warn;
use lowyap_diarize::{DiarizerConfig, HybridSpeakerDiarizer};
use lowyap_dsp::{
    FrameSpeechScorer, VadSegmenterConfig, VadUtteranceSegmenter, WebRtcAec3EchoCanceller,
};
use lowyap_ports::{
    AudioCaptureMode, EchoCanceller, HybridNotesRequest, MeetingTemplate, NoteSummarizer,
    OutputRouteKind, PortError, SpeakerChannel, SpeakerDiarizer, SpeakerIdentity, SummaryOutput,
    TranscriptSegment, UtteranceSegmenter, UtteranceSpan, WindowSummaryRequest,
};
use lowyap_summarize::{
    validate_meeting_endpoint_url, CompletionBackend, HybridMapReduceSummarizer,
    LoopbackOpenAiBackend,
};
use tauri::{AppHandle, Manager};

const SILERO_MEETING_THRESHOLD: f32 = 0.35;
const SILERO_DIGITAL_SILENCE_RMS: f32 = 0.0025;

pub(super) const WINDOW_SAMPLES: usize = 160_000;
pub(super) const MIN_FLUSH_SAMPLES: usize = 4_000;

impl From<SpeakerChannel> for types::SpeakerChannel {
    fn from(channel: SpeakerChannel) -> Self {
        match channel {
            SpeakerChannel::Me => Self::Me,
            SpeakerChannel::Others => Self::Others,
        }
    }
}

impl From<types::SpeakerChannel> for SpeakerChannel {
    fn from(channel: types::SpeakerChannel) -> Self {
        match channel {
            types::SpeakerChannel::Me => Self::Me,
            types::SpeakerChannel::Others => Self::Others,
        }
    }
}

impl From<OutputRouteKind> for types::OutputRouteKind {
    fn from(route: OutputRouteKind) -> Self {
        match route {
            OutputRouteKind::Headphones => Self::Headphones,
            OutputRouteKind::Speakers => Self::Speakers,
            OutputRouteKind::Unknown => Self::Unknown,
        }
    }
}

impl From<types::OutputRouteKind> for OutputRouteKind {
    fn from(route: types::OutputRouteKind) -> Self {
        match route {
            types::OutputRouteKind::Headphones => Self::Headphones,
            types::OutputRouteKind::Speakers => Self::Speakers,
            types::OutputRouteKind::Unknown => Self::Unknown,
        }
    }
}

impl From<AudioCaptureMode> for types::AudioCaptureMode {
    fn from(mode: AudioCaptureMode) -> Self {
        match mode {
            AudioCaptureMode::DualStream => Self::DualStream,
            AudioCaptureMode::MicOnlyFallback => Self::MicOnlyFallback,
        }
    }
}

impl From<types::AudioCaptureMode> for AudioCaptureMode {
    fn from(mode: types::AudioCaptureMode) -> Self {
        match mode {
            types::AudioCaptureMode::DualStream => Self::DualStream,
            types::AudioCaptureMode::MicOnlyFallback => Self::MicOnlyFallback,
        }
    }
}

impl From<MeetingTemplate> for types::MeetingTemplate {
    fn from(template: MeetingTemplate) -> Self {
        match template {
            MeetingTemplate::General => Self::General,
            MeetingTemplate::OneOnOne => Self::OneOnOne,
            MeetingTemplate::Standup => Self::Standup,
            MeetingTemplate::LanguageLesson => Self::LanguageLesson,
            MeetingTemplate::ClientCall => Self::ClientCall,
        }
    }
}

impl From<types::MeetingTemplate> for MeetingTemplate {
    fn from(template: types::MeetingTemplate) -> Self {
        match template {
            types::MeetingTemplate::General => Self::General,
            types::MeetingTemplate::OneOnOne => Self::OneOnOne,
            types::MeetingTemplate::Standup => Self::Standup,
            types::MeetingTemplate::LanguageLesson => Self::LanguageLesson,
            types::MeetingTemplate::ClientCall => Self::ClientCall,
        }
    }
}

/// Converts a persisted [`types::MeetingSegment`] into a [`TranscriptSegment`]
/// carrying speaker attribution (`Speaker 1`, `Speaker 1 (Rahul)`, `Me`).
pub fn to_port_segment(seg: &types::MeetingSegment) -> TranscriptSegment {
    let channel: SpeakerChannel = seg.speaker.into();
    let speaker_id = seg
        .speaker_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| channel.as_str().to_owned());

    TranscriptSegment {
        speaker: SpeakerIdentity {
            channel,
            speaker_id,
            display_label: seg.effective_speaker_label().to_string(),
            confidence: Some(1.0),
        },
        start_ms: seg.start_ms,
        end_ms: seg.end_ms,
        text: seg.text.clone(),
        language: seg.language.clone(),
    }
}

pub fn to_port_segments(segments: &[types::MeetingSegment]) -> Vec<TranscriptSegment> {
    segments.iter().map(to_port_segment).collect()
}

pub fn from_port_segment(segment: &TranscriptSegment) -> types::MeetingSegment {
    let speaker = if segment.speaker.speaker_id == "me" {
        types::SpeakerChannel::Me
    } else if segment.speaker.speaker_id != "me" && !segment.speaker.speaker_id.is_empty() {
        match segment.speaker.channel {
            SpeakerChannel::Me if segment.speaker.speaker_id == "me" => types::SpeakerChannel::Me,
            SpeakerChannel::Me => types::SpeakerChannel::Others,
            SpeakerChannel::Others => types::SpeakerChannel::Others,
        }
    } else {
        segment.speaker.channel.into()
    };

    types::MeetingSegment {
        id: 0,
        meeting_id: String::new(),
        speaker,
        start_ms: segment.start_ms,
        end_ms: segment.end_ms,
        text: segment.text.clone(),
        language: segment.language.clone(),
        speaker_id: Some(segment.speaker.speaker_id.clone()).filter(|s| !s.trim().is_empty()),
        speaker_label: Some(segment.speaker.display_label.clone()).filter(|s| !s.trim().is_empty()),
    }
}

pub fn to_rollup_strings(rollups: &[types::MeetingRollup]) -> Vec<String> {
    rollups
        .iter()
        .map(|rollup| rollup.summary_bullets.clone())
        .collect()
}

pub fn script_language_instruction(
    script: HindustaniScript,
    spoken_languages: &[String],
    segments: &[types::MeetingSegment],
) -> Option<String> {
    if !spoken_languages.is_empty() {
        return llm_script_instruction(script, spoken_languages).map(str::to_owned);
    }

    let mut inferred = Vec::new();
    for seg in segments {
        if let Some(lang) = seg
            .language
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !inferred
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(lang))
            {
                inferred.push(lang.to_owned());
            }
        }
    }
    llm_script_instruction(script, &inferred).map(str::to_owned)
}

pub fn to_window_summary_request(
    window_index: u32,
    segments: &[types::MeetingSegment],
    language_instruction: Option<String>,
) -> WindowSummaryRequest {
    WindowSummaryRequest {
        window_index,
        segments: to_port_segments(segments),
        language_instruction,
    }
}

pub fn to_hybrid_notes_request(
    title: &str,
    template: types::MeetingTemplate,
    rough_notes: &str,
    rollups: &[types::MeetingRollup],
    tail_segments: &[types::MeetingSegment],
    language_instruction: Option<String>,
) -> HybridNotesRequest {
    HybridNotesRequest {
        title: title.to_owned(),
        template: template.into(),
        rough_notes: rough_notes.to_owned(),
        rollups: to_rollup_strings(rollups),
        tail_segments: to_port_segments(tail_segments),
        language_instruction,
    }
}

pub fn to_summary_result(output: SummaryOutput) -> summarizer::SummaryResult {
    summarizer::SummaryResult {
        text: output.markdown,
        summary_source: output.source_id,
    }
}

#[derive(Default)]
pub struct SpeakerEchoGuardAdapter(SpeakerEchoGuard);

/// `lowyap_dsp::FrameSpeechScorer` adapter wrapping a stateful `SileroVad` session.
pub struct SileroFrameScorer {
    silero: SileroVad,
    scratch: Vec<f32>,
}

impl SileroFrameScorer {
    pub fn new(silero: SileroVad) -> Self {
        let frame_samples = silero.frame_samples();
        Self {
            silero,
            scratch: Vec::with_capacity(frame_samples),
        }
    }
}

impl FrameSpeechScorer for SileroFrameScorer {
    fn name(&self) -> &'static str {
        "silero_vad_v6"
    }

    fn frame_samples(&self) -> usize {
        self.silero.frame_samples()
    }

    fn score_frame(&mut self, frame_16k: &[f32]) -> f32 {
        if frame_16k.len() != self.silero.frame_samples() || frame_16k.is_empty() {
            return 0.0;
        }
        self.scratch.clear();
        let mut sum_sq = 0.0_f32;
        for &sample in frame_16k {
            let clean = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            sum_sq += clean * clean;
            self.scratch.push(clean);
        }
        let rms = (sum_sq / frame_16k.len() as f32).sqrt();
        let prob = match self.silero.score(&self.scratch) {
            Ok(probs) => probs.first().copied().unwrap_or(0.0),
            Err(err) => {
                warn!("Silero meeting VAD frame score failed: {err}");
                0.0
            }
        };
        if rms < SILERO_DIGITAL_SILENCE_RMS {
            0.0
        } else {
            prob.clamp(0.0, 1.0)
        }
    }

    fn reset(&mut self) {
        self.silero.reset();
    }
}

/// Constructs the production acoustic echo canceller (`WebRtcAec3EchoCanceller`).
pub fn build_production_echo_canceller() -> Box<dyn EchoCanceller> {
    Box::new(WebRtcAec3EchoCanceller::new())
}

/// Constructs the production utterance segmenter (`SileroVad` when configured and
/// available in bundle resources, otherwise pure-Rust `EarshotSpeechScorer`).
pub fn build_production_utterance_segmenter(
    app_handle: Option<&AppHandle>,
) -> Box<dyn UtteranceSegmenter> {
    let config = VadSegmenterConfig::default();
    if let Some(app) = app_handle {
        let vad_backend = crate::settings::get_settings(app).vad_backend;
        if vad_backend == VadBackend::Silero {
            match app.path().resolve(
                "resources/models/silero-vad-v6.2-F32.gguf",
                tauri::path::BaseDirectory::Resource,
            ) {
                Ok(vad_path) => match (
                    SileroVad::new(&vad_path, SILERO_MEETING_THRESHOLD),
                    SileroVad::new(&vad_path, SILERO_MEETING_THRESHOLD),
                ) {
                    (Ok(me_vad), Ok(others_vad)) => {
                        return Box::new(VadUtteranceSegmenter::with_scorers(
                            config,
                            SileroFrameScorer::new(me_vad),
                            SileroFrameScorer::new(others_vad),
                        ));
                    }
                    (Err(err), _) | (_, Err(err)) => {
                        warn!(
                            "Silero VAD unavailable for meeting segmentation ({err}); falling back to Earshot"
                        );
                    }
                },
                Err(err) => {
                    warn!(
                        "Failed to resolve Silero VAD path for meeting segmentation ({err}); falling back to Earshot"
                    );
                }
            }
        }
    }

    Box::new(VadUtteranceSegmenter::new(config))
}

/// Constructs the production speaker diarizer (`HybridSpeakerDiarizer`),
/// enabling single-mic speaker separation automatically in `MicOnlyFallback`.
pub fn build_production_speaker_diarizer(
    capture_mode: types::AudioCaptureMode,
) -> Box<dyn SpeakerDiarizer> {
    Box::new(HybridSpeakerDiarizer::new(DiarizerConfig {
        separate_single_mic_speakers: capture_mode == types::AudioCaptureMode::MicOnlyFallback,
        ..Default::default()
    }))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub struct AppleIntelligenceBackend {
    max_output_tokens: i32,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl AppleIntelligenceBackend {
    pub fn new(max_output_tokens: i32) -> Self {
        Self {
            max_output_tokens: max_output_tokens.max(512),
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl CompletionBackend for AppleIntelligenceBackend {
    fn source_id(&self) -> &'static str {
        "apple_intelligence"
    }

    fn max_prompt_chars(&self) -> usize {
        9_000
    }

    fn supports_non_latin_scripts(&self) -> bool {
        false
    }

    fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: u32,
    ) -> Result<String, PortError> {
        if !crate::apple_intelligence::check_apple_intelligence_availability() {
            return Err("Apple Intelligence is not available on this device".to_string());
        }

        let requested = i32::try_from(max_tokens).unwrap_or(self.max_output_tokens);
        let effective_tokens = requested.clamp(256, self.max_output_tokens.max(1_536));
        let output = crate::apple_intelligence::process_text_with_system_prompt(
            system_prompt,
            user_prompt,
            effective_tokens,
        )
        .map_err(|err| format!("Apple Intelligence failed: {err}"))?;

        let trimmed = output.trim();
        if trimmed.is_empty() {
            return Err("Apple Intelligence returned an empty response".to_string());
        }
        Ok(trimmed.to_string())
    }
}

fn cloud_meeting_summary_opted_in() -> bool {
    std::env::var("LOWYAP_ALLOW_CLOUD_MEETING_SUMMARY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Builds the prioritized list of completion backends honoring D-19 local-only
/// privacy defaults (`LOWYAP_ALLOW_CLOUD_MEETING_SUMMARY` required for non-loopback).
pub fn build_production_completion_backends(
    app: Option<&AppHandle>,
) -> Vec<Box<dyn CompletionBackend>> {
    let cloud_opt_in = cloud_meeting_summary_opted_in();
    let mut backends: Vec<Box<dyn CompletionBackend>> = Vec::new();

    if let Ok(raw_url) = std::env::var("LOWYAP_MEETING_LLM_URL") {
        let trimmed_url = raw_url.trim();
        if !trimmed_url.is_empty() {
            match validate_meeting_endpoint_url(trimmed_url, cloud_opt_in) {
                Ok(_) => {
                    let model = std::env::var("LOWYAP_MEETING_LLM_MODEL")
                        .ok()
                        .map(|m| m.trim().to_string())
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| "Qwen3-4B-Instruct-2507".to_string());
                    backends.push(Box::new(
                        LoopbackOpenAiBackend::new(trimmed_url, model)
                            .with_allow_cloud_egress(cloud_opt_in),
                    ));
                }
                Err(err) => {
                    warn!("Skipping LOWYAP_MEETING_LLM_URL under D-19 privacy policy: {err}");
                }
            }
        }
    }

    if let Some(app_handle) = app {
        let settings = get_settings(app_handle);
        if settings.post_process_enabled {
            if let Some(provider) = settings.active_post_process_provider() {
                let configured_model = settings
                    .post_process_models
                    .get(&provider.id)
                    .cloned()
                    .unwrap_or_default();

                if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
                    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                    {
                        let token_limit = configured_model.trim().parse::<i32>().unwrap_or(3072);
                        backends.push(Box::new(AppleIntelligenceBackend::new(token_limit)));
                    }
                } else if !provider.base_url.trim().is_empty() {
                    match validate_meeting_endpoint_url(&provider.base_url, cloud_opt_in) {
                        Ok(_) => {
                            if configured_model.trim().is_empty() {
                                warn!(
                                    "Skipping post-process provider '{}' for meeting summary: no model configured",
                                    provider.id
                                );
                            } else {
                                let api_key = settings
                                    .post_process_api_keys
                                    .get(&provider.id)
                                    .cloned()
                                    .filter(|k| !k.trim().is_empty());
                                backends.push(Box::new(
                                    LoopbackOpenAiBackend::new(
                                        provider.base_url.trim(),
                                        configured_model.trim(),
                                    )
                                    .with_api_key(api_key)
                                    .with_allow_cloud_egress(cloud_opt_in)
                                    .with_source_id("llm"),
                                ));
                            }
                        }
                        Err(err) => {
                            warn!(
                                "Skipping post-process provider '{}' for meeting summary under D-19: {err}",
                                provider.id
                            );
                        }
                    }
                }
            }
        }
    }

    backends
}

/// Constructs the production map-reduce meeting note summarizer with budgeted
/// context management and automatic local deterministic fallback.
pub fn build_production_note_summarizer(app: Option<&AppHandle>) -> HybridMapReduceSummarizer {
    HybridMapReduceSummarizer::with_default_budget(build_production_completion_backends(app))
}

impl EchoCanceller for SpeakerEchoGuardAdapter {
    fn name(&self) -> &'static str {
        "speaker_echo_guard"
    }

    fn process_16k(&mut self, mic: &[f32], reference: &[f32], route: OutputRouteKind) -> Vec<f32> {
        self.0.process(mic, reference, route.into())
    }

    fn reset(&mut self) {
        self.0 = SpeakerEchoGuard::new();
    }
}

#[derive(Default)]
struct PendingSpan {
    samples: Vec<f32>,
    start_ms: u64,
}

impl PendingSpan {
    fn drain(&mut self, channel: SpeakerChannel) -> UtteranceSpan {
        let samples_16k = std::mem::take(&mut self.samples);
        UtteranceSpan {
            channel,
            start_ms: self.start_ms,
            end_ms: self.start_ms.saturating_add(samples_ms(samples_16k.len())),
            samples_16k,
        }
    }
}

fn samples_ms(samples: usize) -> u64 {
    samples as u64 / 16
}

#[derive(Default)]
pub struct FixedTenSecSegmenterAdapter {
    me: PendingSpan,
    others: PendingSpan,
}

impl UtteranceSegmenter for FixedTenSecSegmenterAdapter {
    fn name(&self) -> &'static str {
        "fixed_ten_sec"
    }

    fn push_chunk(
        &mut self,
        channel: SpeakerChannel,
        samples: &[f32],
        chunk_start_ms: u64,
    ) -> Vec<UtteranceSpan> {
        let pending = match channel {
            SpeakerChannel::Me => &mut self.me,
            SpeakerChannel::Others => &mut self.others,
        };
        let mut spans = Vec::new();
        let mut offset = 0;
        while offset < samples.len() {
            if pending.samples.is_empty() {
                pending.start_ms = chunk_start_ms.saturating_add(samples_ms(offset));
            }
            let count = (WINDOW_SAMPLES - pending.samples.len()).min(samples.len() - offset);
            pending
                .samples
                .extend_from_slice(&samples[offset..offset + count]);
            offset += count;
            if pending.samples.len() == WINDOW_SAMPLES {
                spans.push(pending.drain(channel));
            }
        }
        spans
    }

    fn flush(&mut self) -> Vec<UtteranceSpan> {
        let mut spans = Vec::new();
        for (channel, pending) in [
            (SpeakerChannel::Me, &mut self.me),
            (SpeakerChannel::Others, &mut self.others),
        ] {
            if pending.samples.len() >= MIN_FLUSH_SAMPLES {
                spans.push(pending.drain(channel));
            } else {
                pending.samples.clear();
            }
        }
        spans
    }
}

#[derive(Default)]
pub struct ChannelPassThroughDiarizer {
    observed_me_clean: bool,
}

impl ChannelPassThroughDiarizer {
    /// True once the caller has provided clean, non-silent local audio.
    pub fn observed_me_clean(&self) -> bool {
        self.observed_me_clean
    }
}

impl SpeakerDiarizer for ChannelPassThroughDiarizer {
    fn name(&self) -> &'static str {
        "channel_pass_through"
    }

    fn observe_me_clean(&mut self, samples: &[f32]) {
        self.observed_me_clean |= samples
            .iter()
            .any(|sample| sample.is_finite() && *sample != 0.0);
    }

    fn identify(&mut self, span: &UtteranceSpan) -> SpeakerIdentity {
        SpeakerIdentity {
            channel: span.channel,
            speaker_id: span.channel.as_str().to_owned(),
            display_label: span.channel.display_label().to_owned(),
            confidence: None,
        }
    }

    fn reset(&mut self) {
        self.observed_me_clean = false;
    }
}

#[derive(Default)]
pub struct LocalExtractiveSummarizerAdapter;

fn local_segments(segments: &[TranscriptSegment]) -> Vec<types::MeetingSegment> {
    segments.iter().map(from_port_segment).collect()
}

fn summary_output(result: summarizer::SummaryResult) -> SummaryOutput {
    SummaryOutput {
        markdown: result.text,
        source_id: result.summary_source,
    }
}

impl NoteSummarizer for LocalExtractiveSummarizerAdapter {
    fn name(&self) -> &'static str {
        "local_extractive"
    }

    fn max_input_chars(&self) -> usize {
        // The deterministic path caps selected output, not total input.
        usize::MAX
    }

    fn summarize_window(&self, req: &WindowSummaryRequest) -> Result<SummaryOutput, PortError> {
        // Deterministic local extraction preserves source transcript language directly
        // without generative LLM prompt steering (`window_index` / `language_instruction`
        // are retained on the port for cloud/Apple Intelligence adapters).
        let _ = (&req.window_index, &req.language_instruction);
        Ok(summary_output(summarizer::summarize_five_min_window_local(
            &local_segments(&req.segments),
        )))
    }

    fn expand_hybrid_notes(&self, req: &HybridNotesRequest) -> Result<SummaryOutput, PortError> {
        let _ = &req.language_instruction;
        let rollups: Vec<_> = req
            .rollups
            .iter()
            .enumerate()
            .map(|(index, summary)| types::MeetingRollup {
                id: 0,
                meeting_id: String::new(),
                window_index: u32::try_from(index).unwrap_or(u32::MAX),
                start_ms: 0,
                end_ms: 0,
                summary_bullets: summary.clone(),
            })
            .collect();
        let template = match req.template {
            lowyap_ports::MeetingTemplate::General => types::MeetingTemplate::General,
            lowyap_ports::MeetingTemplate::OneOnOne => types::MeetingTemplate::OneOnOne,
            lowyap_ports::MeetingTemplate::Standup => types::MeetingTemplate::Standup,
            lowyap_ports::MeetingTemplate::LanguageLesson => types::MeetingTemplate::LanguageLesson,
            lowyap_ports::MeetingTemplate::ClientCall => types::MeetingTemplate::ClientCall,
        };
        Ok(summary_output(
            summarizer::generate_hybrid_meeting_notes_local(
                &req.title,
                template,
                &req.rough_notes,
                &rollups,
                &local_segments(&req.tail_segments),
                HindustaniScript::Auto,
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_adapter_matches_native_guard_and_resets() {
        let mut adapter = SpeakerEchoGuardAdapter::default();
        let mic = vec![0.1; 320];
        let reference = vec![0.5; 320];
        let mut engine = SpeakerEchoGuard::new();
        for route in [
            OutputRouteKind::Headphones,
            OutputRouteKind::Speakers,
            OutputRouteKind::Unknown,
        ] {
            assert_eq!(
                adapter.process_16k(&mic, &reference, route),
                engine.process(&mic, &reference, route.into())
            );
        }
        adapter.reset();
        assert_eq!(
            adapter.process_16k(&mic, &[], OutputRouteKind::Unknown),
            mic
        );
    }

    #[test]
    fn segmenter_preserves_channels_and_flush_boundary() {
        let mut adapter = FixedTenSecSegmenterAdapter::default();
        assert!(adapter
            .push_chunk(SpeakerChannel::Others, &[0.2; 3_999], 100)
            .is_empty());
        assert!(adapter
            .push_chunk(SpeakerChannel::Me, &[0.1; 16_000], 500)
            .is_empty());
        let spans = adapter.push_chunk(SpeakerChannel::Me, &vec![0.3; 148_000], 1_500);
        assert_eq!(
            (
                spans[0].start_ms,
                spans[0].end_ms,
                spans[0].samples_16k.len()
            ),
            (500, 10_500, 160_000)
        );
        let tail = adapter.flush();
        assert_eq!(
            tail,
            vec![UtteranceSpan {
                channel: SpeakerChannel::Me,
                samples_16k: vec![0.3; 4_000],
                start_ms: 10_500,
                end_ms: 10_750
            }]
        );
        assert!(adapter.flush().is_empty());
    }

    #[test]
    fn diarizer_labels_channels_without_claiming_identity_confidence() {
        let mut adapter = ChannelPassThroughDiarizer::default();
        adapter.observe_me_clean(&[]);
        assert!(!adapter.observed_me_clean());
        adapter.observe_me_clean(&[0.4]);
        assert!(adapter.observed_me_clean());
        for channel in [SpeakerChannel::Me, SpeakerChannel::Others] {
            let identity = adapter.identify(&UtteranceSpan {
                channel,
                samples_16k: vec![0.4],
                start_ms: 0,
                end_ms: 0,
            });
            assert_eq!(
                (
                    identity.channel,
                    identity.display_label.as_str(),
                    identity.confidence
                ),
                (channel, channel.display_label(), None)
            );
        }
        adapter.reset();
        assert!(!adapter.observed_me_clean());
    }

    #[test]
    fn summary_adapter_keeps_local_provenance_and_transcript_evidence() {
        let segment = TranscriptSegment {
            speaker: SpeakerIdentity {
                channel: SpeakerChannel::Me,
                speaker_id: "me".into(),
                display_label: "Me".into(),
                confidence: None,
            },
            start_ms: 12_000,
            end_ms: 14_000,
            text: "Send the draft on Friday.".into(),
            language: None,
        };
        let summary = LocalExtractiveSummarizerAdapter
            .summarize_window(&WindowSummaryRequest {
                window_index: 0,
                segments: vec![segment.clone()],
                language_instruction: None,
            })
            .unwrap();
        assert_eq!(
            summary.markdown,
            "- **[Me]** [00:12]: Send the draft on Friday."
        );
        assert_eq!(summary.source_id, "local_extractive");
        let expanded = LocalExtractiveSummarizerAdapter
            .expand_hybrid_notes(&HybridNotesRequest {
                title: "Test".into(),
                template: lowyap_ports::MeetingTemplate::General,
                rough_notes: "Follow up".into(),
                rollups: vec!["- [00:08] Discussed draft".into()],
                tail_segments: vec![segment],
                language_instruction: None,
            })
            .unwrap();
        assert!(expanded.markdown.contains("Follow up"));
        assert!(expanded.markdown.contains("Send the draft on Friday."));
        assert_eq!(expanded.source_id, "local_extractive");
    }

    #[test]
    fn production_diarizer_and_note_summarizer_work_deterministically_offline() {
        let mut dual_diarizer =
            build_production_speaker_diarizer(types::AudioCaptureMode::DualStream);
        let me_span = UtteranceSpan {
            channel: SpeakerChannel::Me,
            samples_16k: vec![0.12; 16_000],
            start_ms: 0,
            end_ms: 1_000,
        };
        let me_identity = dual_diarizer.identify(&me_span);
        assert_eq!(me_identity.channel, SpeakerChannel::Me);
        assert_eq!(me_identity.speaker_id, "me");
        assert_eq!(me_identity.display_label, "Me");

        let mut mic_only_diarizer =
            build_production_speaker_diarizer(types::AudioCaptureMode::MicOnlyFallback);
        assert_eq!(mic_only_diarizer.name(), "hybrid_voiceprint_diarizer");
        mic_only_diarizer.reset();

        let seg = types::MeetingSegment {
            id: 1,
            meeting_id: "m-offline".to_string(),
            speaker: types::SpeakerChannel::Others,
            start_ms: 15_000,
            end_ms: 19_000,
            text: "Action: Priya will ship the release checklist.".to_string(),
            language: Some("en".to_string()),
            speaker_id: Some("speaker_1".to_string()),
            speaker_label: Some("Speaker 1 (Priya)".to_string()),
        };

        let summarizer_engine = build_production_note_summarizer(None);
        let win = summarizer_engine
            .summarize_window(&to_window_summary_request(
                0,
                std::slice::from_ref(&seg),
                None,
            ))
            .expect("offline window summary");
        assert_eq!(win.source_id, "local_extractive");
        assert!(win.markdown.contains("Speaker 1 (Priya)"));
        assert!(win.markdown.contains("[00:15]"));

        let notes = summarizer_engine
            .expand_hybrid_notes(&to_hybrid_notes_request(
                "Release Sync",
                types::MeetingTemplate::General,
                "- verify checklist owner",
                &[],
                &[seg],
                None,
            ))
            .expect("offline hybrid notes");
        assert_eq!(notes.source_id, "local_extractive");
        assert!(notes.markdown.contains("## TL;DR"));
        assert!(notes.markdown.contains("Speaker 1 (Priya)"));
    }
}
