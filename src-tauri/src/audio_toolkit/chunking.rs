//! Long-form chunking: long audio is decoded in chunks cut at pauses, because
//! most models' accuracy degrades past 30-40 s of audio in one run.
//!
//! Cuts come from per-frame voice activity probabilities. Every duration here
//! is in samples of 16 kHz audio, never in frames, so any detector's frame size
//! works with the same rules. The thresholds are Silero's.
//!
//! Ported from pi-voice's `chunking.ts`. The cut plan is the same; the Rust
//! version returns chunks for the caller to decode rather than decoding them.

use std::ops::Range;

use anyhow::Result;
use log::{debug, warn};

use super::constants;
use super::vad::SpeechScorer;

const RATE: usize = constants::WHISPER_SAMPLE_RATE as usize;

/// Clean pause floor (seconds): forced hard cuts or tight pauses shorter than
/// `250 ms` retain `forced_cut_overlap` samples at the head of the next chunk
/// so mid-syllable accented words are never sliced at chunk boundaries.
const CLEAN_PAUSE_MIN_SECONDS: f64 = 0.25;

/// Default sliding audio overlap (`1.6 s` = `25,600` samples at 16 kHz) for
/// forced/tight cuts in live dictation (`policy.max_chunk_ms <= 20_000`).
const FORCED_CUT_OVERLAP_SAMPLES: usize = 1600 * RATE / 1000;

struct SamplePolicy {
    target: usize,
    max: usize,
    min: usize,
    lookahead: usize,
    forced_cut_overlap: usize,
}

impl From<lowyap_ports::ChunkingPolicy> for SamplePolicy {
    fn from(policy: lowyap_ports::ChunkingPolicy) -> Self {
        let samples =
            |ms: u64| usize::try_from(ms.saturating_mul(RATE as u64) / 1000).unwrap_or(usize::MAX);
        let max = samples(policy.max_chunk_ms).max(1);
        let forced_cut_overlap = if policy.max_chunk_ms <= 20_000 {
            FORCED_CUT_OVERLAP_SAMPLES
        } else {
            0
        };
        Self {
            target: samples(policy.target_chunk_ms).clamp(1, max),
            max,
            min: samples(policy.min_chunk_ms).clamp(1, max),
            lookahead: samples(policy.lookahead_ms),
            forced_cut_overlap,
        }
    }
}

// Silero's `get_speech_timestamps` defaults, as `VadSession::run` applies them.
const THRESHOLD: f32 = 0.5;
const NEG_THRESHOLD: f32 = THRESHOLD - 0.15;
const MIN_SPEECH_SAMPLES: usize = 250 * RATE / 1000;
const MIN_SILENCE_SAMPLES: usize = 100 * RATE / 1000;
const SPEECH_PAD_SAMPLES: usize = 30 * RATE / 1000;

/// Speech segments, in samples, from per-frame speech probabilities: Silero's
/// `get_speech_timestamps` without a maximum speech length. `samples` is the
/// length of the audio the probabilities score.
fn speech_segments(probs: &[f32], frame_samples: usize, samples: usize) -> Vec<Range<usize>> {
    let mut segments: Vec<Range<usize>> = Vec::new();
    let mut triggered = false;
    let mut start = 0;
    let mut temp_end: Option<usize> = None;
    for (i, &prob) in probs.iter().enumerate() {
        let at = i * frame_samples;
        if prob >= THRESHOLD {
            temp_end = None;
            if !triggered {
                triggered = true;
                start = at;
            }
            continue;
        }
        if prob < NEG_THRESHOLD && triggered {
            let end = *temp_end.get_or_insert(at);
            if at - end < MIN_SILENCE_SAMPLES {
                continue;
            }
            if end - start > MIN_SPEECH_SAMPLES {
                segments.push(start..end);
            }
            triggered = false;
            temp_end = None;
        }
    }
    if triggered && samples.saturating_sub(start) > MIN_SPEECH_SAMPLES {
        segments.push(start..samples);
    }

    for i in 0..segments.len() {
        if i == 0 {
            segments[0].start = segments[0].start.saturating_sub(SPEECH_PAD_SAMPLES);
        }
        if i + 1 == segments.len() {
            segments[i].end = (segments[i].end + SPEECH_PAD_SAMPLES).min(samples);
            continue;
        }
        let silence = segments[i + 1].start - segments[i].end;
        let pad = if silence < 2 * SPEECH_PAD_SAMPLES {
            silence / 2
        } else {
            SPEECH_PAD_SAMPLES
        };
        segments[i].end = (segments[i].end + pad).min(samples);
        segments[i + 1].start = segments[i + 1].start.saturating_sub(pad);
    }
    segments
}

/// A place a chunk may end: the middle of a pause.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CutCandidate {
    cut: usize,
    gap_seconds: f64,
}

/// One candidate in the middle of each pause, lead-in and tail included.
fn cut_candidates(segments: &[Range<usize>], samples: usize) -> Vec<CutCandidate> {
    let mut candidates = Vec::new();
    let mut silence_start = 0;
    for segment in segments
        .iter()
        .cloned()
        .chain(std::iter::once(samples..samples))
    {
        if segment.start > silence_start {
            candidates.push(CutCandidate {
                cut: (silence_start + segment.start) / 2,
                gap_seconds: (segment.start - silence_start) as f64 / RATE as f64,
            });
        }
        silence_start = silence_start.max(segment.end);
    }
    candidates
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CutPlan {
    cut: usize,
    overlap: usize,
}

/// Greedy, left to right from `from`: each cut is the best-scoring pause
/// 5-40 s after the previous one, preferring long pauses near 30 s and ones
/// that leave at least 5 s after them. With no pause, it is a hard cut at 40 s,
/// or halfway when that would leave under 5 s. Audio of 40 s or less is never
/// cut. `candidates` must be in order.
#[cfg(test)]
fn plan_cuts(candidates: &[CutCandidate], samples: usize, from: usize) -> Vec<usize> {
    plan_cuts_with_policy(
        candidates,
        samples,
        from,
        &lowyap_ports::ChunkingPolicy::batch_clip().into(),
    )
    .into_iter()
    .map(|plan| plan.cut)
    .collect()
}

fn plan_cuts_with_policy(
    candidates: &[CutCandidate],
    samples: usize,
    from: usize,
    policy: &SamplePolicy,
) -> Vec<CutPlan> {
    let mut cuts = Vec::new();
    let mut start = from;
    // Each window starts after the last, so earlier candidates never return.
    let mut first = 0;
    while samples - start > policy.max {
        while first < candidates.len() && candidates[first].cut < start + policy.min {
            first += 1;
        }
        // A pause that leaves under 5 s is only used when no other pause will do.
        let mut best: Option<(CutCandidate, f64)> = None;
        let mut fallback: Option<(CutCandidate, f64)> = None;
        for &candidate in candidates[first..]
            .iter()
            .take_while(|candidate| candidate.cut <= start + policy.max)
        {
            let position = (candidate.cut - start) as f64 / RATE as f64;
            let target_seconds = seconds(policy.target);
            let off_target = if position > target_seconds {
                0.1 * (position - target_seconds)
            } else {
                0.05 * (target_seconds - position)
            };
            let score = candidate.gap_seconds.min(1.0) - off_target;
            if samples - candidate.cut >= policy.min && best.is_none_or(|(_, best)| score > best) {
                best = Some((candidate, score));
            }
            if fallback.is_none_or(|(_, fallback)| score > fallback) {
                fallback = Some((candidate, score));
            }
        }
        let hard_cut = if samples - start < policy.max.saturating_add(policy.min) {
            start + (samples - start) / 2
        } else {
            start + policy.max
        };
        let (cut, is_clean_pause) = match best.or(fallback) {
            Some((chosen, _)) => (chosen.cut, chosen.gap_seconds >= CLEAN_PAUSE_MIN_SECONDS),
            None => (hard_cut, false),
        };
        let chunk_len = cut.saturating_sub(start);
        let max_safe_overlap = chunk_len.saturating_sub(1) / 2;
        let overlap = if is_clean_pause {
            0
        } else {
            policy.forced_cut_overlap.min(max_safe_overlap)
        };
        cuts.push(CutPlan { cut, overlap });
        let next_start = cut.saturating_sub(overlap);
        start = if next_start > start { next_start } else { cut };
    }
    cuts
}

/// Makes a fresh scorer for one clip.
pub type MakeScorer = Box<dyn Fn() -> Result<Box<dyn SpeechScorer>> + Send>;

/// A chunk cut by [`Chunker`], carrying whether its audio start overlaps with
/// the tail of the immediately preceding chunk (`plan.overlap > 0`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkPiece {
    pub samples: Vec<f32>,
    pub overlaps_with_prev: bool,
}

/// Cuts audio into chunks at pauses as it arrives, and hands each back once
/// its cut is certain, so it can be decoded while the user is still
/// speaking. Pushed all at once, a clip is cut exactly as it is live: the
/// same scores and the same plan.
///
/// If the audio can't be scored (the VAD fails to load or to run), nothing
/// more is cut and the rest is one chunk: chunking only places cuts, so the
/// audio is still decoded, as it would be without chunking.
pub struct Chunker {
    policy: SamplePolicy,
    make_scorer: MakeScorer,
    /// Made the first time audio has to be scored, so a clip short enough to
    /// be one chunk never loads a VAD.
    scorer: Option<Box<dyn SpeechScorer>>,
    /// The scorer's frame size, once it is made.
    frame_samples: usize,
    /// Set when scoring fails: no more cuts.
    uncut: bool,
    /// Audio from `start` on; everything before it is already in a chunk.
    pending: Vec<f32>,
    /// One speech probability per whole frame from the beginning. Audio is
    /// only scored once a cut has to be planned, so a clip short enough to
    /// be one chunk is never scored.
    probs: Vec<f32>,
    start: usize,
    len: usize,
    next_overlaps_with_prev: bool,
}

impl Chunker {
    /// `make_scorer` must make a fresh scorer: scores carry on from what it
    /// saw before.
    pub fn new(make_scorer: MakeScorer) -> Self {
        Self::with_policy(make_scorer, lowyap_ports::ChunkingPolicy::batch_clip())
    }

    /// Uses engine-independent cut durations, converted to 16 kHz samples.
    pub fn with_policy(make_scorer: MakeScorer, policy: lowyap_ports::ChunkingPolicy) -> Self {
        Self::from_sample_policy(make_scorer, policy.into())
    }

    /// Uses `policy` with an explicit sliding overlap (in milliseconds) retained
    /// across forced hard cuts or tight pauses (`< 250 ms`).
    pub fn with_overlap(
        make_scorer: MakeScorer,
        policy: lowyap_ports::ChunkingPolicy,
        forced_cut_overlap_ms: u64,
    ) -> Self {
        let mut sample_policy = SamplePolicy::from(policy);
        sample_policy.forced_cut_overlap =
            usize::try_from(forced_cut_overlap_ms.saturating_mul(RATE as u64) / 1000)
                .unwrap_or(usize::MAX);
        Self::from_sample_policy(make_scorer, sample_policy)
    }

    fn from_sample_policy(make_scorer: MakeScorer, policy: SamplePolicy) -> Self {
        Self {
            policy,
            make_scorer,
            scorer: None,
            frame_samples: 0,
            uncut: false,
            pending: Vec::new(),
            probs: Vec::new(),
            start: 0,
            len: 0,
            next_overlaps_with_prev: false,
        }
    }

    /// Samples pushed so far, chunked or not. A caller checks this against
    /// the whole recording before trusting the chunks.
    pub fn samples(&self) -> usize {
        self.len
    }

    /// Adds audio. Returns the chunks whose cuts this made certain, in order.
    /// A whole clip pushed at once is moved in, not copied.
    pub fn push(&mut self, pcm: impl Into<Vec<f32>>) -> Vec<Vec<f32>> {
        self.push_pieces(pcm)
            .into_iter()
            .map(|piece| piece.samples)
            .collect()
    }

    /// Adds audio and returns committed [`ChunkPiece`]s with overlap provenance.
    pub fn push_pieces(&mut self, pcm: impl Into<Vec<f32>>) -> Vec<ChunkPiece> {
        let pcm = pcm.into();
        self.len += pcm.len();
        if self.pending.is_empty() {
            self.pending = pcm;
        } else {
            self.pending.extend_from_slice(&pcm);
        }
        if self.uncut
            || self.len - self.start <= self.policy.max.saturating_add(self.policy.lookahead)
        {
            return Vec::new();
        }
        // Plan over whole frames only; a partial one waits for the rest.
        let scored = match self.score(false) {
            Ok(scored) => scored,
            Err(e) => {
                self.stop_cutting(e);
                return Vec::new();
            }
        };
        let segments = speech_segments(&self.probs, self.frame_samples, scored);
        // A pause still open at the end moves its midpoint as it grows: wait
        // for speech.
        if segments.last().map(|segment| segment.end) != Some(scored) {
            return Vec::new();
        }
        // One plan, committed while each cut's whole window plus the lookahead
        // is scored.
        let cuts = plan_cuts_with_policy(
            &cut_candidates(&segments, scored),
            scored,
            self.start,
            &self.policy,
        );
        let mut chunks = Vec::new();
        for plan in cuts {
            if scored - self.start <= self.policy.max.saturating_add(self.policy.lookahead) {
                break;
            }
            chunks.push(self.take_until(plan));
        }
        chunks
    }

    /// Cuts what is left and returns the remaining chunks, in order. Nothing
    /// more is pushed after.
    pub fn finish(&mut self) -> Vec<Vec<f32>> {
        self.finish_pieces()
            .into_iter()
            .map(|piece| piece.samples)
            .collect()
    }

    /// Cuts what is left and returns remaining [`ChunkPiece`]s with overlap provenance.
    pub fn finish_pieces(&mut self) -> Vec<ChunkPiece> {
        let mut chunks = Vec::new();
        if !self.uncut && self.len - self.start > self.policy.max {
            match self.score(true) {
                Ok(_) => {
                    let segments = speech_segments(&self.probs, self.frame_samples, self.len);
                    let cuts = plan_cuts_with_policy(
                        &cut_candidates(&segments, self.len),
                        self.len,
                        self.start,
                        &self.policy,
                    );
                    for plan in cuts {
                        chunks.push(self.take_until(plan));
                    }
                }
                Err(e) => self.stop_cutting(e),
            }
        }
        if self.len > self.start {
            chunks.push(self.take_until(CutPlan {
                cut: self.len,
                overlap: 0,
            }));
        }
        chunks
    }

    /// Scores every frame not yet scored, making the scorer first if this is
    /// the first time: whole frames only, or with `pad_last` the partial last
    /// frame too, zero padded. Returns how many samples are scored.
    fn score(&mut self, pad_last: bool) -> Result<usize> {
        if self.scorer.is_none() {
            // Nothing is cut before the first score, so a fresh scorer starts
            // at the first sample, as it must.
            debug_assert!(self.start == 0 && self.probs.is_empty());
            let scorer = (self.make_scorer)()?;
            self.frame_samples = scorer.frame_samples();
            assert!(self.frame_samples > 0, "VAD frame size must be non-zero");
            self.scorer = Some(scorer);
        }
        let scorer = self.scorer.as_mut().expect("the scorer was made above");
        let frame = self.frame_samples;
        let scored = self.probs.len() * frame;
        // Cuts only fall in scored audio, so its unscored rest is pending.
        let from = scored - self.start;
        let whole = (self.len - scored) / frame * frame;
        if whole > 0 {
            self.probs
                .extend(scorer.score(&self.pending[from..from + whole])?);
        }
        if pad_last && self.len > scored + whole {
            let mut tail = self.pending[from + whole..].to_vec();
            tail.resize(frame, 0.0);
            self.probs.extend(scorer.score(&tail)?);
            return Ok(self.len);
        }
        Ok(scored + whole)
    }

    /// Scoring failed: nothing more is cut, so the rest is one chunk.
    fn stop_cutting(&mut self, e: anyhow::Error) {
        warn!(
            "Voice activity detection failed at {:.1}s; decoding the rest in one piece: {:#}",
            seconds(self.len),
            e
        );
        self.uncut = true;
    }

    fn take_until(&mut self, plan: CutPlan) -> ChunkPiece {
        debug!(
            "Long-form chunk: {:.1}-{:.1}s (overlap {:.2}s)",
            seconds(self.start),
            seconds(plan.cut),
            seconds(plan.overlap)
        );
        let overlaps_with_prev = self.next_overlaps_with_prev;
        let chunk_len = plan.cut - self.start;
        let next_start = plan.cut.saturating_sub(plan.overlap).max(self.start);
        let drain_len = next_start - self.start;
        let samples = if plan.overlap == 0 {
            self.pending.drain(..chunk_len).collect()
        } else {
            let out = self.pending[..chunk_len].to_vec();
            self.pending.drain(..drain_len);
            out
        };
        self.next_overlaps_with_prev = plan.overlap > 0 && next_start < plan.cut;
        self.start = next_start;
        ChunkPiece {
            samples,
            overlaps_with_prev,
        }
    }
}

/// Chunk texts in order across sliding-overlap boundaries.
pub fn join_transcripts<S: AsRef<str>>(texts: &[S]) -> String {
    lowyap_flow::stitch_overlapping_transcripts(texts)
}

/// Chunk texts in order with explicit per-chunk overlap provenance (`overlaps_with_prev`),
/// deduplicating only sliding-overlap boundaries while preserving intentional
/// repeated words spoken across clean silence cuts.
pub fn join_transcript_boundaries<S: AsRef<str>>(segments: &[(S, bool)]) -> String {
    lowyap_flow::stitch_transcript_boundaries(segments)
}

fn seconds(samples: usize) -> f64 {
    samples as f64 / RATE as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_toolkit::vad::{VadFrame, VoiceActivityDetector};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const FRAME: usize = 512;

    fn s(seconds: f64) -> usize {
        (seconds * RATE as f64).round() as usize
    }

    /// Frame probabilities: speech everywhere except the given pauses, in seconds.
    fn probs_with_pauses(seconds: f64, pauses: &[(f64, f64)]) -> Vec<f32> {
        (0..s(seconds) / FRAME)
            .map(|i| {
                let at = (i * FRAME) as f64 / RATE as f64;
                if pauses.iter().any(|&(start, end)| at >= start && at < end) {
                    0.0
                } else {
                    1.0
                }
            })
            .collect()
    }

    /// Cut positions, in tenths of a second.
    fn plan(seconds: f64, pauses: &[(f64, f64)]) -> Vec<f64> {
        let samples = s(seconds);
        let segments = speech_segments(&probs_with_pauses(seconds, pauses), FRAME, samples);
        plan_cuts(&cut_candidates(&segments, samples), samples, 0)
            .into_iter()
            .map(|cut| (cut as f64 / RATE as f64 * 10.0).round() / 10.0)
            .collect()
    }

    fn lengths(chunks: &[Vec<f32>]) -> Vec<usize> {
        chunks.iter().map(Vec::len).collect()
    }

    /// Where chunks end, except the last, in tenths of a second.
    fn cuts(chunks: &[Vec<f32>]) -> Vec<f64> {
        let mut end = 0;
        chunks[..chunks.len().saturating_sub(1)]
            .iter()
            .map(|chunk| {
                end += chunk.len();
                (end as f64 / RATE as f64 * 10.0).round() / 10.0
            })
            .collect()
    }

    #[test]
    fn cuts_prefer_long_pauses_near_30s() {
        assert!(
            plan(40.0, &[(20.0, 21.0)]).is_empty(),
            "40 s or less is never cut"
        );
        // A 1 s pause at 28-29 s beats a short one right at 30 s; 41.5 s
        // remain, so cut again.
        assert_eq!(
            plan(70.0, &[(28.0, 29.0), (30.0, 30.2), (55.0, 56.0)]),
            vec![28.5, 55.5]
        );
        // Without any pause, cuts are hard 40 s ones.
        assert_eq!(plan(100.0, &[]), vec![40.0, 80.0]);
        // A pause under 5 s in is never used.
        assert_eq!(plan(60.0, &[(2.0, 3.0)]), vec![40.0]);
    }

    #[test]
    fn cuts_avoid_leaving_a_short_last_chunk() {
        // The pause at 38 s would leave 4 s, so the weaker one at 10 s wins.
        assert_eq!(plan(42.0, &[(9.8, 10.2), (37.5, 38.5)]), vec![10.0]);
        // Without a pause, a short rest is split evenly instead of cut at 40 s.
        assert_eq!(plan(84.0, &[]), vec![40.0, 62.0]);
        // A pause that leaves a short rest still beats cutting mid-speech.
        assert_eq!(plan(40.5, &[(38.8, 39.2)]), vec![39.0]);
    }

    #[test]
    fn joins_put_spaces_only_between_spaced_scripts() {
        assert_eq!(
            join_transcripts(&[" Hello there. ", "", "General Kenobi."]),
            "Hello there. General Kenobi."
        );
        assert_eq!(
            join_transcripts(&["今天天气很好。", "我们去公园吧。"]),
            "今天天气很好。我们去公园吧。"
        );
        assert_eq!(join_transcripts(&["I said", "你好"]), "I said你好");
        assert_eq!(
            join_transcripts(&["안녕하세요.", "반갑습니다."]),
            "안녕하세요. 반갑습니다."
        );
        // CJK Extension B, outside the Basic Multilingual Plane.
        assert_eq!(join_transcripts(&["你好𠀀", "我们"]), "你好𠀀我们");
    }

    /// Scores each frame with its first sample, so a test clip carries its
    /// own probabilities. Counts the frames it scores, and fails on the
    /// `fail_on`th call (from 0).
    struct ScriptedScorer {
        scored: Arc<AtomicUsize>,
        calls: usize,
        fail_on: Option<usize>,
    }

    impl VoiceActivityDetector for ScriptedScorer {
        fn push_frame<'a>(&'a mut self, _frame: &'a [f32]) -> Result<VadFrame<'a>> {
            unreachable!("chunking only scores")
        }

        fn frame_samples(&self) -> usize {
            FRAME
        }
    }

    impl SpeechScorer for ScriptedScorer {
        fn score(&mut self, frames: &[f32]) -> Result<Vec<f32>> {
            assert!(frames.len().is_multiple_of(FRAME), "whole frames only");
            self.calls += 1;
            if self.fail_on == Some(self.calls - 1) {
                anyhow::bail!("scripted VAD failure");
            }
            self.scored
                .fetch_add(frames.len() / FRAME, Ordering::Relaxed);
            Ok(frames.chunks(FRAME).map(|frame| frame[0]).collect())
        }
    }

    /// How many scorers a chunker made, and how many frames they scored.
    #[derive(Clone, Default)]
    struct Counts {
        made: Arc<AtomicUsize>,
        scored: Arc<AtomicUsize>,
    }

    fn scripted(fail_on: Option<usize>) -> (Chunker, Counts) {
        let counts = Counts::default();
        let make = {
            let counts = counts.clone();
            move || {
                counts.made.fetch_add(1, Ordering::Relaxed);
                let scorer = ScriptedScorer {
                    scored: Arc::clone(&counts.scored),
                    calls: 0,
                    fail_on,
                };
                Ok(Box::new(scorer) as Box<dyn SpeechScorer>)
            }
        };
        (Chunker::new(Box::new(make)), counts)
    }

    fn chunker() -> (Chunker, Counts) {
        scripted(None)
    }

    /// Audio whose every frame holds its own probability.
    fn clip(probs: &[f32]) -> Vec<f32> {
        probs.iter().flat_map(|&p| [p; FRAME]).collect()
    }

    /// 100 s of audio, with pauses at 29-30 s and 58-59 s.
    fn long_clip() -> Vec<f32> {
        clip(&probs_with_pauses(100.0, &[(29.0, 30.0), (58.0, 59.0)]))
    }

    #[test]
    fn live_chunks_arrive_before_the_end_at_the_same_cuts() {
        // A partial last frame, scored padded.
        let mut pcm = long_clip();
        pcm.extend([1.0; 100]);
        let (mut once, _) = chunker();
        let mut offline = once.push(&pcm[..]);
        offline.extend(once.finish());
        // The pauses' midpoints, then the 41.5 s left split evenly.
        assert_eq!(cuts(&offline), vec![29.5, 58.5, 79.3]);

        // Pieces that split frames, as the recorder's do.
        let (mut chunker, counts) = chunker();
        let mut live = Vec::new();
        for piece in pcm.chunks(FRAME * 3 + 7) {
            live.extend(chunker.push(piece));
        }
        // A cut is made whenever more than 45 s is waiting: 2 of the 4 chunks
        // before the end.
        assert_eq!(live.len(), 2, "chunks are ready while audio still arrives");
        live.extend(chunker.finish());
        assert_eq!(lengths(&live), lengths(&offline));
        assert_eq!(live.concat(), pcm, "every sample once, in order");

        // One stateful scorer for the clip, scoring each frame once.
        assert_eq!(counts.made.load(Ordering::Relaxed), 1);
        assert_eq!(
            counts.scored.load(Ordering::Relaxed),
            pcm.len().div_ceil(FRAME)
        );
    }

    #[test]
    fn live_cuts_wait_for_a_pause_to_end() {
        // At 45 s the pause so far is 34-45 s, a cut at 39.5 s; the whole
        // pause's midpoint, 42 s, is out of reach, so the plan cuts at 40 s.
        let pcm = clip(&probs_with_pauses(90.0, &[(34.0, 50.0)]));
        let planned = vec![s(40.0), s(40.0), pcm.len() - s(80.0)];

        let (mut chunker, _) = chunker();
        let mut live = Vec::new();
        for piece in pcm.chunks(16 * FRAME) {
            live.extend(chunker.push(piece));
        }
        live.extend(chunker.finish());
        assert_eq!(lengths(&live), planned);
    }

    #[test]
    fn audio_that_is_one_chunk_never_loads_a_vad() {
        let probs = probs_with_pauses(40.0, &[(20.0, 21.0)]);
        let pcm = clip(&probs);
        let (mut chunker, counts) = chunker();
        for frame in pcm.chunks(FRAME) {
            assert!(chunker.push(frame).is_empty());
        }
        assert_eq!(lengths(&chunker.finish()), vec![pcm.len()]);
        assert_eq!(counts.made.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_vad_failure_leaves_the_rest_in_one_piece() {
        // The VAD fails on its second score, after the first chunk is out.
        let pcm = long_clip();
        let (mut chunker, _) = scripted(Some(1));
        let mut chunks = Vec::new();
        for frame in pcm.chunks(FRAME) {
            chunks.extend(chunker.push(frame));
        }
        assert_eq!(chunks.len(), 1);
        chunks.extend(chunker.finish());
        assert_eq!(cuts(&chunks), vec![29.5]);
        assert_eq!(chunks.concat(), pcm, "no audio lost");
    }

    #[test]
    fn live_policy_predecodes_shorter_chunks_and_preserves_every_sample() {
        let pcm = clip(&probs_with_pauses(
            34.0,
            &[(7.5, 8.5), (15.5, 16.5), (23.5, 24.5)],
        ));
        let (batch, _) = chunker();
        let make_scorer = batch.make_scorer;
        let mut live =
            Chunker::with_policy(make_scorer, lowyap_ports::ChunkingPolicy::live_dictation());
        let mut chunks = Vec::new();
        for piece in pcm.chunks(FRAME * 3 + 7) {
            chunks.extend(live.push(piece));
        }
        assert!(
            chunks.len() >= 3,
            "multiple chunks finish while still recording"
        );
        chunks.extend(live.finish());
        assert_eq!(&cuts(&chunks)[..3], &[8.0, 16.0, 24.0]);
        assert_eq!(chunks.concat(), pcm);
        assert!(chunks.iter().all(|chunk| chunk.len() <= s(12.0)));
    }

    #[test]
    fn live_policy_waits_for_lookahead_and_short_takes_skip_vad() {
        let (batch, counts) = chunker();
        let mut live = Chunker::with_policy(
            batch.make_scorer,
            lowyap_ports::ChunkingPolicy::live_dictation(),
        );
        assert!(live.push(vec![1.0; s(12.0)]).is_empty());
        assert_eq!(live.finish().len(), 1);
        assert_eq!(counts.made.load(Ordering::Relaxed), 0);

        let (batch, _) = chunker();
        let mut live = Chunker::with_policy(
            batch.make_scorer,
            lowyap_ports::ChunkingPolicy::live_dictation(),
        );
        assert!(live.push(vec![1.0; s(12.7)]).is_empty());
        assert_eq!(live.push(vec![1.0; FRAME]).len(), 1);
    }

    #[test]
    fn live_policy_forced_and_tight_cuts_carry_1_6s_sliding_overlap() {
        // 1. Forced hard cut (no pauses at all over 22s continuous speech).
        let continuous_pcm = clip(&probs_with_pauses(22.0, &[]));
        let (batch, _) = chunker();
        let mut live = Chunker::with_policy(
            batch.make_scorer,
            lowyap_ports::ChunkingPolicy::live_dictation(),
        );
        let mut chunks = Vec::new();
        for piece in continuous_pcm.chunks(FRAME * 2) {
            chunks.extend(live.push(piece));
        }
        chunks.extend(live.finish());
        assert!(
            chunks.len() >= 2,
            "expected forced cut on 22s continuous speech"
        );
        assert_eq!(live.samples(), continuous_pcm.len());
        let expected_overlap = FORCED_CUT_OVERLAP_SAMPLES;
        assert_eq!(expected_overlap, 25_600);
        assert_eq!(
            &chunks[0][chunks[0].len() - expected_overlap..],
            &chunks[1][..expected_overlap],
            "forced cut must carry 1.6s (25,600 samples) trailing overlap into next chunk"
        );

        // 2. Tight pause (< 250ms, e.g. 160ms raw pause -> ~100ms post-pad) also retains 1.6s overlap.
        let tight_pcm = clip(&probs_with_pauses(20.0, &[(8.0, 8.16)]));
        let (batch, _) = chunker();
        let mut tight_live = Chunker::with_overlap(
            batch.make_scorer,
            lowyap_ports::ChunkingPolicy::live_dictation(),
            1_600,
        );
        let mut tight_chunks = tight_live.push(tight_pcm);
        tight_chunks.extend(tight_live.finish());
        assert!(tight_chunks.len() >= 2);
        assert_eq!(
            &tight_chunks[0][tight_chunks[0].len() - 25_600..],
            &tight_chunks[1][..25_600],
            "tight pause (< 0.25s) must also carry 1.6s overlap"
        );
    }

    #[test]
    fn join_transcripts_stitches_overlapping_boundaries_across_scripts() {
        assert_eq!(
            join_transcripts(&["we are testing the live", "the live transcription engine"]),
            "we are testing the live transcription engine"
        );
        assert_eq!(
            join_transcripts(&["transcription", "transcription works"]),
            "transcription works"
        );
        assert_eq!(join_transcripts(&["hello", "hello"]), "hello hello");
        assert_eq!(join_transcripts(&["very", "very"]), "very very");
        assert_eq!(
            join_transcripts(&["live trans-", "transcription works"]),
            "live transcription works"
        );
    }
}
