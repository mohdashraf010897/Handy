//! Simulated dictation scheduling and timed, offline linguist fixture replays.

use std::fmt::Write;
use std::hint::black_box;
use std::time::Instant;

use lowyap_flow::{
    apply_stage_a_linguist, merged_lexicon_terms, run_plug_and_play_linguist,
    validate_linguist_output,
};
use lowyap_ports::{
    ChunkingPolicy, LinguistRequest, LinguistRewrite, LinguistRewriter, LinguistStageKind,
    LinguistTone, PortError,
};
use serde::{Deserialize, Serialize};

use crate::scorecard::{EvaluationTier, SYNTHETIC_HONESTY_BANNER};

const AUDIO_MS: u64 = 191_600;
const SYNTHETIC_PAUSE_INTERVAL_MS: u64 = 14_430;
const DECODE_SPEED: f64 = 4.78;
const TIMING_RUNS: usize = 100;

/// Decode-only latency for a synthetic recording, not a hardware measurement.
#[derive(Debug, Serialize, Deserialize)]
pub struct DictationLatencySimulation {
    /// Scheduling mode under test.
    pub mode: String,
    /// Chunks that finish decoding before or at release.
    pub chunks_decoded_live: usize,
    /// Chunks decoded across the entire recording.
    pub chunks_total: usize,
    /// Seconds of audio in the final chunk.
    pub final_chunk_s: f64,
    /// Milliseconds from release until all decoder work finishes.
    pub post_release_decode_ms: f64,
}

/// Observed text and Stage-A elapsed times for a single fixture.
#[derive(Debug, Serialize, Deserialize)]
pub struct LinguistReplay {
    /// Human-readable fixture name.
    pub name: String,
    /// Input word count, using whitespace-separated words.
    pub raw_words: usize,
    /// Stage-A word count.
    pub stage_a_words: usize,
    /// Paragraphs in the Stage-A output.
    pub stage_a_paragraphs: usize,
    /// Number of timed Stage-A calls after warm-up.
    pub timing_runs: usize,
    /// Median elapsed nanoseconds for Stage A.
    pub stage_a_median_ns: u64,
    /// 95th percentile elapsed nanoseconds for Stage A.
    pub stage_a_p95_ns: u64,
    /// Slowest observed Stage-A call in nanoseconds.
    pub stage_a_max_ns: u64,
    /// Which stage supplied the final text.
    pub stage: LinguistStageKind,
    /// Output guard rejection, if the scripted adapter output was dropped.
    pub rejection_reason: Option<String>,
    /// Exact expected text for the fixture.
    pub expected: String,
    /// Final output after executing the pipeline.
    pub output: String,
}

/// Offline dictation benchmark results; no composite quality score is computed.
#[derive(Debug, Serialize, Deserialize)]
pub struct DictationBenchReport {
    /// Evaluation tier distinguishing synthetic plumbing checks from real-audio runs.
    #[serde(default = "EvaluationTier::synthetic_default")]
    pub evaluation_tier: EvaluationTier,
    /// Synthetic recording duration in seconds.
    pub audio_duration_s: f64,
    /// Assumed single-worker decode speed in audio seconds per wall-clock second.
    pub decode_speed_x: f64,
    /// Interval between eligible pauses in the synthetic recording.
    pub pause_interval_s: f64,
    /// Decode timing comparison.
    pub latency: [DictationLatencySimulation; 2],
    /// Results of replaying synthetic transcripts and scripted adapter outputs.
    pub replays: Vec<LinguistReplay>,
}

/// Runs the 191.6-second scheduling simulation and times offline text replays.
///
/// The simulator assumes pause boundaries are ready immediately, a serial decoder
/// at 4.78× real time with no per-chunk overhead, and the final tail ready on release.
/// Actual microphone, VAD, model loading, LLM and paste latency are not measured.
pub fn run_dictation_bench() -> DictationBenchReport {
    let release = AUDIO_MS as f64;
    let batch = DictationLatencySimulation {
        mode: "PostStopBatch".to_owned(),
        chunks_decoded_live: 0,
        chunks_total: 1,
        final_chunk_s: release / 1000.0,
        post_release_decode_ms: release / DECODE_SPEED,
    };
    let policy = ChunkingPolicy::live_dictation();
    let mut finished_ms: f64 = 0.0;
    let mut start = 0;
    let mut decoded_live = 0;
    let mut chunks_total = 0;
    // Scheduled points stand in for VAD pauses; this is not a VAD replay.
    loop {
        let end = ((start + policy.target_chunk_ms).div_ceil(SYNTHETIC_PAUSE_INTERVAL_MS)
            * SYNTHETIC_PAUSE_INTERVAL_MS)
            .max(start + policy.min_chunk_ms);
        if end >= AUDIO_MS {
            break;
        }
        let (end, ready) = if end <= start + policy.max_chunk_ms {
            (end, end)
        } else {
            let forced = start + policy.max_chunk_ms;
            let ready = forced + policy.lookahead_ms;
            if ready >= AUDIO_MS {
                break;
            }
            (forced, ready)
        };
        finished_ms = finished_ms.max(ready as f64) + (end - start) as f64 / DECODE_SPEED;
        decoded_live += usize::from(finished_ms <= release);
        chunks_total += 1;
        start = end;
    }
    let tail = (AUDIO_MS - start) as f64;
    let live = DictationLatencySimulation {
        mode: "LiveChunkPreDecode".to_owned(),
        chunks_decoded_live: decoded_live,
        chunks_total: chunks_total + 1,
        final_chunk_s: tail / 1000.0,
        post_release_decode_ms: (finished_ms - release).max(0.0) + tail / DECODE_SPEED,
    };

    DictationBenchReport {
        evaluation_tier: EvaluationTier::SyntheticPlumbingOnly,
        audio_duration_s: release / 1000.0,
        decode_speed_x: DECODE_SPEED,
        pause_interval_s: SYNTHETIC_PAUSE_INTERVAL_MS as f64 / 1000.0,
        latency: [batch, live],
        replays: replay_fixtures(),
    }
}

/// Formats raw scheduling and fixture metrics as a compact Markdown report.
pub fn format_dictation_bench_markdown(report: &DictationBenchReport) -> String {
    let passed_invariants = report
        .replays
        .iter()
        .filter(|row| row.output == row.expected)
        .count();
    let total_invariants = report.replays.len();
    let gate_status = if passed_invariants == total_invariants {
        "PASS"
    } else {
        "FAIL"
    };
    let mut out = format!(
        "# Dictation benchmark — PLUMBING GATE: {} ({}/{} deterministic invariants)\n\n\
         {}\n\n\
         Decode-only simulation: {:.1} s of audio, {:.3} s pause cadence, \
         {:.2}× real-time decode speed (one worker, no fixed overhead).\n\n\
         | Mode | Decoded live / total | Final chunk (s) | Post-release decode (ms) |\n\
         | --- | ---: | ---: | ---: |\n",
        gate_status,
        passed_invariants,
        total_invariants,
        SYNTHETIC_HONESTY_BANNER,
        report.audio_duration_s,
        report.pause_interval_s,
        report.decode_speed_x,
    );
    for row in &report.latency {
        let _ = writeln!(
            out,
            "| {} | {}/{} | {:.3} | {:.2} |",
            row.mode,
            row.chunks_decoded_live,
            row.chunks_total,
            row.final_chunk_s,
            row.post_release_decode_ms
        );
    }
    out.push_str(
        "\nStage-A wall-clock samples on this machine; Stage B uses scripted outputs, not a model.\n\n\
         | Fixture | Raw → Stage-A words | Paragraphs | Runs | Median / p95 / max (ns) | Stage | Guard rejection | Exact match |\n\
         | --- | ---: | ---: | ---: | ---: | --- | --- | --- |\n",
    );
    for row in &report.replays {
        let _ = writeln!(
            out,
            "| {} | {} → {} | {} | {} | {} / {} / {} | {:?} | {} | {} |",
            row.name,
            row.raw_words,
            row.stage_a_words,
            row.stage_a_paragraphs,
            row.timing_runs,
            row.stage_a_median_ns,
            row.stage_a_p95_ns,
            row.stage_a_max_ns,
            row.stage,
            row.rejection_reason.as_deref().unwrap_or("—"),
            row.output == row.expected,
        );
    }
    out
}

struct ScriptedRewriter(&'static str);

impl LinguistRewriter for ScriptedRewriter {
    fn name(&self) -> &'static str {
        "scripted_replay"
    }

    fn rewrite(&self, _req: &LinguistRequest) -> Result<LinguistRewrite, PortError> {
        Ok(LinguistRewrite {
            text: self.0.to_owned(),
            provider_id: self.name().to_owned(),
        })
    }
}

fn replay_fixtures() -> Vec<LinguistReplay> {
    let raw_long = concat!(
        "we booked the small room by the hallway because the larger room already had a workshop scheduled for the entire morning ",
        "remember that the first group needs the projector and a spare cable and that the second group only needs the whiteboard ",
        "what if we keep the sign in sheet on the front table so people can find their names before they sit down ",
        "one more thing the building opens at eight but the side door stays locked until the receptionist comes back from the depot ",
        "next is the lunch order which includes three vegetarian meals and two extra drinks for the volunteers who are arriving just before noon ",
        "so it's worth checking the room again after lunch so the afternoon team has all the chairs and supplies it needs"
    );
    let expected_long = concat!(
        "We booked the small room by the hallway because the larger room already had a workshop scheduled for the entire morning. ",
        "Remember that the first group needs the projector and a spare cable and that the second group only needs the whiteboard. ",
        "What if we keep the sign in sheet on the front table so people can find their names before they sit down.\n\n",
        "One more thing the building opens at eight but the side door stays locked until the receptionist comes back from the depot. ",
        "Next is the lunch order which includes three vegetarian meals and two extra drinks for the volunteers who are arriving just before noon. ",
        "So it's worth checking the room again after lunch so the afternoon team has all the chairs and supplies it needs."
    );
    [
        (
            "Messy English",
            "Um, I I think the and the kind of time we we need is an hour",
            "I think the kind of time we need is an hour.",
            8.0,
            None,
        ),
        (
            "Roman Hinglish",
            "uh kal low app pe whisper to level ka demo hai",
            "Kal lowyap pe Wispr-level ka demo hai.",
            4.0,
            None,
        ),
        (
            "Spoken self-repair",
            "let's meet Tuesday no wait Wednesday",
            "Let's meet Wednesday.",
            3.0,
            Some("Let's meet Wednesday."),
        ),
        (
            "Unpunctuated monologue",
            raw_long,
            expected_long,
            90.0,
            None,
        ),
        (
            "Hostile adapter output",
            "kal budget review hai",
            "Kal budget review hai.",
            2.0,
            Some("Sure, here is an invented approval from the finance team."),
        ),
    ]
    .into_iter()
    .map(|(name, raw, expected, audio_duration_s, candidate)| {
        let req = LinguistRequest {
            raw_transcript: raw.to_owned(),
            stage_a_text: String::new(),
            tone: LinguistTone::Default,
            audio_duration_s,
            language_code: None,
            script_rule: None,
            lexicon_terms: merged_lexicon_terms(&[]),
            field_context: None,
            custom_prompt_override: None,
        };
        let stage_a = apply_stage_a_linguist(raw, req.tone, &req.lexicon_terms, audio_duration_s);
        let mut samples = [0; TIMING_RUNS];
        for elapsed_ns in &mut samples {
            let start = Instant::now();
            let output = apply_stage_a_linguist(
                black_box(raw),
                req.tone,
                &req.lexicon_terms,
                audio_duration_s,
            );
            black_box(&output);
            *elapsed_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        }
        samples.sort_unstable();
        let adapter = candidate.map(ScriptedRewriter);
        let (output, stage, _) = run_plug_and_play_linguist(
            &req,
            adapter.as_ref().map(|value| value as &dyn LinguistRewriter),
            true,
        );
        LinguistReplay {
            name: name.to_owned(),
            raw_words: raw.split_whitespace().count(),
            stage_a_words: stage_a.split_whitespace().count(),
            stage_a_paragraphs: stage_a
                .split("\n\n")
                .filter(|s| !s.trim().is_empty())
                .count(),
            timing_runs: samples.len(),
            stage_a_median_ns: samples[samples.len() / 2],
            stage_a_p95_ns: samples[(samples.len() * 95).div_ceil(100) - 1],
            stage_a_max_ns: samples[samples.len() - 1],
            stage,
            rejection_reason: candidate.and_then(|text| {
                validate_linguist_output(&stage_a, text, true)
                    .rejection_reason
                    .map(str::to_owned)
            }),
            expected: expected.to_owned(),
            output,
        }
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_matches_expected_text_and_expected_adapter_stages() {
        let rows = replay_fixtures();
        for (row, expected_stage) in rows.iter().zip([
            LinguistStageKind::Deterministic,
            LinguistStageKind::Deterministic,
            LinguistStageKind::ModelPolished,
            LinguistStageKind::Deterministic,
            LinguistStageKind::FallbackDeterministic,
        ]) {
            assert_eq!(row.output, row.expected, "{}", row.name);
            assert_eq!(row.stage, expected_stage, "{}", row.name);
        }
        assert!(rows[3].raw_words >= 120);
        assert_eq!(rows[3].stage_a_paragraphs, 2);
        assert_eq!(
            rows[4].rejection_reason.as_deref(),
            Some("assistant_meta_chatter")
        );
    }

    #[test]
    fn live_schedule_pre_decodes_all_but_final_chunk_before_release_at_assumed_speed() {
        let report = run_dictation_bench();
        assert_eq!(
            report.evaluation_tier,
            EvaluationTier::SyntheticPlumbingOnly
        );
        let [batch, live] = report.latency;
        assert_eq!(batch.chunks_decoded_live, 0);
        assert!((40_000.0..41_000.0).contains(&batch.post_release_decode_ms));
        assert_eq!((live.chunks_decoded_live, live.chunks_total), (16, 17));
        assert!(
            (1_300.0..1_400.0).contains(&live.post_release_decode_ms),
            "expected ~1347ms post-release decode under current live chunking policy, got {}",
            live.post_release_decode_ms
        );
    }
}
