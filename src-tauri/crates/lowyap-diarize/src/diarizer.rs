//! Hybrid `ME_ONLY`-enrolled + online multi-speaker clustering diarizer (`SV-89`).

use lowyap_ports::{SpeakerChannel, SpeakerDiarizer, SpeakerIdentity, UtteranceSpan};
use serde::{Deserialize, Serialize};

use crate::embedder::{l2_normalize, AcousticMelVoiceEmbedder, VoiceEmbedder, VoiceEmbedding};
use crate::name_cues::NameCueResolver;

const MIN_ME_ENROLLMENT_VOICED_FRAMES: usize = 8;
const MAX_STORED_F0_OBSERVATIONS: usize = 256;
const F0_DIVERGENCE_RATIO_THRESHOLD: f32 = 0.28;

/// Tuning parameters for [`HybridSpeakerDiarizer`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiarizerConfig {
    /// Maximum number of distinct remote speaker clusters (`speaker_1..speaker_N`).
    pub max_remote_speakers: usize,
    /// Similarity threshold in `[-1.0, 1.0]` for matching an utterance to an existing remote cluster.
    pub remote_match_threshold: f32,
    /// Minimum voiced frames (`~135 ms` at `12` frames) required before spawning a new speaker cluster.
    pub min_frames_for_new_speaker: usize,
    /// When `true` (`MicOnlyFallback`) and `Me` is enrolled, `Me`-channel turns strongly diverging
    /// from `Me` below [`Self::me_verification_threshold`] are clustered into in-room speakers.
    pub separate_single_mic_speakers: bool,
    /// Similarity floor below which a `Me`-channel utterance in single-mic mode is attributed
    /// to an in-room speaker rather than `"me"`.
    pub me_verification_threshold: f32,
}

impl Default for DiarizerConfig {
    fn default() -> Self {
        Self {
            max_remote_speakers: 8,
            remote_match_threshold: 0.82,
            min_frames_for_new_speaker: 12,
            separate_single_mic_speakers: false,
            me_verification_threshold: 0.72,
        }
    }
}

#[derive(Clone, Debug)]
struct SpeakerCluster {
    centroid: Vec<f32>,
    observations: usize,
    voiced_observations: usize,
    f0_history: Vec<f32>,
    median_f0_hz: Option<f32>,
}

impl SpeakerCluster {
    fn new(emb: VoiceEmbedding) -> Self {
        let mut f0_history = Vec::new();
        if let Some(f0) = emb.median_f0_hz {
            f0_history.push(f0);
        }
        let voiced_observations = usize::from(emb.median_f0_hz.is_some());
        Self {
            centroid: emb.vector,
            observations: 1,
            voiced_observations,
            median_f0_hz: emb.median_f0_hz,
            f0_history,
        }
    }

    fn update(&mut self, emb: &VoiceEmbedding, min_alpha: f32) {
        if self.centroid.len() != emb.vector.len() {
            return;
        }
        let alpha = (1.0 / (self.observations as f32 + 1.0)).max(min_alpha);
        for (c, &x) in self.centroid.iter_mut().zip(emb.vector.iter()) {
            *c = *c * (1.0 - alpha) + x * alpha;
        }
        l2_normalize(&mut self.centroid);
        self.observations = self.observations.saturating_add(1);

        if let Some(f0) = emb.median_f0_hz {
            if self.f0_history.len() >= MAX_STORED_F0_OBSERVATIONS {
                self.f0_history.remove(0);
            }
            self.f0_history.push(f0);
            self.voiced_observations = self.voiced_observations.saturating_add(1);
            self.median_f0_hz = compute_median(&self.f0_history);
        }
    }

    fn similarity_with_f0_guard(&self, emb: &VoiceEmbedding) -> (f32, f32) {
        let cos = emb.cosine_similarity(&self.centroid);
        let penalty = match (self.median_f0_hz, emb.median_f0_hz) {
            (Some(cluster_f0), Some(span_f0))
                if self.voiced_observations >= 1 && emb.voiced_frames >= 6 =>
            {
                let hi = cluster_f0.max(span_f0);
                let lo = cluster_f0.min(span_f0).max(1.0);
                let rel_diff = (hi / lo) - 1.0;
                if rel_diff > F0_DIVERGENCE_RATIO_THRESHOLD {
                    0.25 + (rel_diff - F0_DIVERGENCE_RATIO_THRESHOLD) * 0.60
                } else if rel_diff > 0.20 {
                    (rel_diff - 0.20) * 1.25
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        (cos, cos - penalty)
    }
}

/// Stateful speaker diarizer combining `ME_ONLY` local voiceprint enrollment,
/// online L2-normalized remote speaker clustering, and conversational name cues.
#[derive(Debug)]
pub struct HybridSpeakerDiarizer<E: VoiceEmbedder = AcousticMelVoiceEmbedder> {
    config: DiarizerConfig,
    embedder: E,
    me_cluster: Option<SpeakerCluster>,
    remote_clusters: Vec<SpeakerCluster>,
    last_remote_speaker_id: Option<String>,
    name_cues: NameCueResolver,
}

impl Default for HybridSpeakerDiarizer<AcousticMelVoiceEmbedder> {
    fn default() -> Self {
        Self::new(DiarizerConfig::default())
    }
}

impl HybridSpeakerDiarizer<AcousticMelVoiceEmbedder> {
    /// Creates a [`HybridSpeakerDiarizer`] backed by the default [`AcousticMelVoiceEmbedder`].
    pub fn new(config: DiarizerConfig) -> Self {
        Self::with_embedder(config, AcousticMelVoiceEmbedder::new())
    }
}

impl<E: VoiceEmbedder> HybridSpeakerDiarizer<E> {
    /// Creates a [`HybridSpeakerDiarizer`] with a custom [`VoiceEmbedder`] implementation.
    pub fn with_embedder(config: DiarizerConfig, embedder: E) -> Self {
        let mut sanitized = config;
        sanitized.max_remote_speakers = sanitized.max_remote_speakers.max(1);
        Self {
            config: sanitized,
            embedder,
            me_cluster: None,
            remote_clusters: Vec::new(),
            last_remote_speaker_id: None,
            name_cues: NameCueResolver::new(),
        }
    }

    /// Returns the active [`DiarizerConfig`].
    pub fn config(&self) -> &DiarizerConfig {
        &self.config
    }

    /// Returns `true` once at least one clean `ME_ONLY` window (`voiced_frames >= 8`) has enrolled `Me`.
    pub fn is_me_enrolled(&self) -> bool {
        self.me_cluster.is_some()
    }

    /// Returns the number of clean `ME_ONLY` windows incorporated into the `Me` voiceprint.
    pub fn me_observation_count(&self) -> usize {
        self.me_cluster.as_ref().map_or(0, |c| c.observations)
    }

    /// Returns the enrolled median `F0` (Hz) for `Me`, if enrolled with voiced speech.
    pub fn me_median_f0_hz(&self) -> Option<f32> {
        self.me_cluster.as_ref().and_then(|c| c.median_f0_hz)
    }

    /// Returns the number of active remote/in-room speaker clusters (`speaker_1..speaker_k`).
    pub fn remote_speaker_count(&self) -> usize {
        self.remote_clusters.len()
    }

    /// Returns an immutable reference to the embedded [`NameCueResolver`].
    pub fn name_cues(&self) -> &NameCueResolver {
        &self.name_cues
    }

    /// Returns a mutable reference to the embedded [`NameCueResolver`].
    pub fn name_cues_mut(&mut self) -> &mut NameCueResolver {
        &mut self.name_cues
    }

    /// Feeds a recognized transcript turn into [`NameCueResolver`] so subsequent
    /// speaker attributions automatically include resolved names (`"Speaker 1 (Rahul)"`).
    pub fn observe_transcript_turn(
        &mut self,
        channel: SpeakerChannel,
        speaker_id: &str,
        start_ms: u64,
        end_ms: u64,
        text: &str,
    ) -> Option<String> {
        self.name_cues
            .observe_transcript_turn(channel, speaker_id, start_ms, end_ms, text)
    }

    fn resolved_label(&self, speaker_id: &str, default_label: &str) -> String {
        self.name_cues.display_label_for(speaker_id, default_label)
    }

    fn fallback_remote_identity(
        &self,
        channel: SpeakerChannel,
        confidence: f32,
    ) -> SpeakerIdentity {
        let speaker_id = self
            .last_remote_speaker_id
            .clone()
            .unwrap_or_else(|| "speaker_1".to_owned());
        let default_label = default_remote_label(&speaker_id);
        let display_label = self.resolved_label(&speaker_id, &default_label);
        SpeakerIdentity {
            channel,
            speaker_id,
            display_label,
            confidence: Some(confidence),
        }
    }

    fn cluster_remote_embedding(
        &mut self,
        channel: SpeakerChannel,
        emb: VoiceEmbedding,
    ) -> SpeakerIdentity {
        let mut best_idx: Option<usize> = None;
        let mut best_effective = f32::NEG_INFINITY;
        let mut best_raw_cos = 0.0_f32;

        for (idx, cluster) in self.remote_clusters.iter().enumerate() {
            let (raw_cos, effective) = cluster.similarity_with_f0_guard(&emb);
            if effective > best_effective {
                best_effective = effective;
                best_raw_cos = raw_cos;
                best_idx = Some(idx);
            }
        }

        if let Some(idx) = best_idx {
            if best_effective >= self.config.remote_match_threshold {
                self.remote_clusters[idx].update(&emb, 0.08);
                let number = idx + 1;
                let speaker_id = format!("speaker_{number}");
                let default_label = format!("Speaker {number}");
                let display_label = self.resolved_label(&speaker_id, &default_label);
                self.last_remote_speaker_id = Some(speaker_id.clone());
                let confidence = ((best_raw_cos + 1.0) * 0.5).clamp(0.55, 0.99);
                return SpeakerIdentity {
                    channel,
                    speaker_id,
                    display_label,
                    confidence: Some(confidence),
                };
            }
        }

        if self.remote_clusters.len() < self.config.max_remote_speakers
            && emb.voiced_frames >= self.config.min_frames_for_new_speaker
        {
            self.remote_clusters.push(SpeakerCluster::new(emb));
            let number = self.remote_clusters.len();
            let speaker_id = format!("speaker_{number}");
            let default_label = format!("Speaker {number}");
            let display_label = self.resolved_label(&speaker_id, &default_label);
            self.last_remote_speaker_id = Some(speaker_id.clone());
            return SpeakerIdentity {
                channel,
                speaker_id,
                display_label,
                confidence: Some(0.86),
            };
        }

        // Cap reached or insufficient voiced frames to open a new speaker cluster:
        // assign to nearest existing cluster without polluting centroids on short bursts or cap overflow.
        if let Some(idx) = best_idx {
            let number = idx + 1;
            let speaker_id = format!("speaker_{number}");
            let default_label = format!("Speaker {number}");
            let display_label = self.resolved_label(&speaker_id, &default_label);
            self.last_remote_speaker_id = Some(speaker_id.clone());
            let confidence = ((best_raw_cos + 1.0) * 0.42).clamp(0.42, 0.80);
            SpeakerIdentity {
                channel,
                speaker_id,
                display_label,
                confidence: Some(confidence),
            }
        } else {
            self.fallback_remote_identity(channel, 0.42)
        }
    }
}

impl<E: VoiceEmbedder> SpeakerDiarizer for HybridSpeakerDiarizer<E> {
    fn name(&self) -> &'static str {
        "hybrid_voiceprint_diarizer"
    }

    fn observe_me_clean(&mut self, clean_me_16k: &[f32]) {
        let Some(emb) = self.embedder.embed_16k(clean_me_16k) else {
            return;
        };
        if emb.voiced_frames < MIN_ME_ENROLLMENT_VOICED_FRAMES {
            return;
        }
        match &mut self.me_cluster {
            Some(me) => me.update(&emb, 0.05),
            None => self.me_cluster = Some(SpeakerCluster::new(emb)),
        }
    }

    fn identify(&mut self, span: &UtteranceSpan) -> SpeakerIdentity {
        match span.channel {
            SpeakerChannel::Me => {
                let maybe_emb = self.embedder.embed_16k(&span.samples_16k);

                if let (Some(me), Some(emb)) = (&self.me_cluster, maybe_emb) {
                    let (cos, effective_sim) = me.similarity_with_f0_guard(&emb);
                    if self.config.separate_single_mic_speakers
                        && effective_sim < self.config.me_verification_threshold
                        && emb.voiced_frames >= self.config.min_frames_for_new_speaker
                    {
                        return self.cluster_remote_embedding(SpeakerChannel::Me, emb);
                    }
                    let confidence = ((cos + 1.0) * 0.5).clamp(0.50, 0.99);
                    SpeakerIdentity {
                        channel: SpeakerChannel::Me,
                        speaker_id: "me".to_owned(),
                        display_label: self.resolved_label("me", "Me"),
                        confidence: Some(confidence),
                    }
                } else if self.me_cluster.is_some() {
                    SpeakerIdentity {
                        channel: SpeakerChannel::Me,
                        speaker_id: "me".to_owned(),
                        display_label: self.resolved_label("me", "Me"),
                        confidence: Some(0.75),
                    }
                } else {
                    SpeakerIdentity {
                        channel: SpeakerChannel::Me,
                        speaker_id: "me".to_owned(),
                        display_label: self.resolved_label("me", "Me"),
                        confidence: Some(0.85),
                    }
                }
            }
            SpeakerChannel::Others => {
                let Some(emb) = self.embedder.embed_16k(&span.samples_16k) else {
                    return self.fallback_remote_identity(SpeakerChannel::Others, 0.40);
                };
                self.cluster_remote_embedding(SpeakerChannel::Others, emb)
            }
        }
    }

    fn reset(&mut self) {
        self.me_cluster = None;
        self.remote_clusters.clear();
        self.last_remote_speaker_id = None;
        self.name_cues.reset();
    }
}

fn default_remote_label(speaker_id: &str) -> String {
    if let Some(num) = speaker_id.strip_prefix("speaker_") {
        format!("Speaker {num}")
    } else {
        "Speaker 1".to_owned()
    }
}

fn compute_median(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Some(sorted[mid])
    } else {
        Some(0.5 * (sorted[mid - 1] + sorted[mid]))
    }
}
