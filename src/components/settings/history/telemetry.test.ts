import assert from "node:assert/strict";
import type { HistoryEntry } from "@/bindings";
import {
  computeClientTelemetrySummary,
  detectRtfConvention,
  formatAudioDuration,
  formatSpeedNumber,
  formatWaitMs,
  hasDistinctPolishedAndRaw,
  hasEntryTelemetry,
  resolveEntrySpeedMultiplier,
  resolveLinguistStage,
  rtfToSpeedMultiplier,
} from "./telemetry";

function makeEntry(overrides: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    id: 1,
    file_name: "rec-1.wav",
    timestamp: 1_700_000_000,
    saved: false,
    title: "Test",
    transcription_text: "raw transcript text",
    post_processed_text: null,
    post_process_prompt: null,
    post_process_requested: false,
    source_name: null,
    language: "en",
    language_source: null,
    ...overrides,
  };
}

// Wait formatting
assert.equal(formatWaitMs(780), "780 ms");
assert.equal(formatWaitMs(999.4), "999 ms");
assert.equal(formatWaitMs(1120), "1.12s");
assert.equal(formatWaitMs(1400), "1.4s");
assert.equal(formatWaitMs(12430), "12.4s");
assert.equal(formatWaitMs(null), "—");

// Audio duration formatting
assert.equal(formatAudioDuration(15.8), "15.8s");
assert.equal(formatAudioDuration(192), "3m 12s");
assert.equal(formatAudioDuration(3720), "1h 2m");
assert.equal(formatAudioDuration(null), null);

// RTF convention calibration & throughput
const classicEntry = makeEntry({
  audio_duration_s: 20,
  asr_ms: 930,
  real_time_factor: 0.0465,
});
assert.equal(detectRtfConvention([classicEntry]), "classic_rtf");
assert.equal(
  formatSpeedNumber(resolveEntrySpeedMultiplier(classicEntry, "classic_rtf")),
  "21.5",
);
assert.equal(
  formatSpeedNumber(rtfToSpeedMultiplier(0.0446, "classic_rtf", "none")),
  "22.4",
);

const speedEntry = makeEntry({
  audio_duration_s: 20,
  asr_ms: 930,
  real_time_factor: 21.5,
});
assert.equal(detectRtfConvention([speedEntry]), "speed_multiplier");
assert.equal(
  formatSpeedNumber(
    resolveEntrySpeedMultiplier(speedEntry, "speed_multiplier"),
  ),
  "21.5",
);

// Polished vs Raw toggle & stage badges
assert.equal(hasDistinctPolishedAndRaw(makeEntry()), false);
assert.equal(
  hasDistinctPolishedAndRaw(
    makeEntry({ post_processed_text: "raw transcript text" }),
  ),
  false,
);
assert.equal(
  hasDistinctPolishedAndRaw(
    makeEntry({ post_processed_text: "Polished transcript text." }),
  ),
  true,
);

assert.equal(
  resolveLinguistStage(makeEntry({ linguist_stage: "model_polished" })),
  "polished",
);
assert.equal(
  resolveLinguistStage(makeEntry({ linguist_stage: "deterministic" })),
  "fast_polish",
);
assert.equal(
  resolveLinguistStage(
    makeEntry({ linguist_stage: "fallback_deterministic" }),
  ),
  "fast_polish",
);
assert.equal(
  resolveLinguistStage(
    makeEntry({ linguist_stage: "raw", post_processed_text: "ignored" }),
  ),
  "raw",
);
assert.equal(resolveLinguistStage(makeEntry()), "raw");

assert.equal(hasEntryTelemetry(makeEntry()), false);
assert.equal(hasEntryTelemetry(makeEntry({ release_to_done_ms: 650 })), true);

// Client-side fallback summary
const summary = computeClientTelemetrySummary([
  makeEntry({
    id: 1,
    audio_duration_s: 30,
    asr_ms: 1200,
    cleanup_ms: 300,
    release_to_done_ms: 780,
    real_time_factor: 0.04,
    chunks_predecoded_live: 11,
    chunks_total: 12,
    linguist_stage: "model_polished",
    post_processed_text: "Polished 1.",
  }),
  makeEntry({
    id: 2,
    audio_duration_s: 15,
    asr_ms: 600,
    cleanup_ms: 200,
    release_to_done_ms: 1120,
    real_time_factor: 0.04,
    chunks_predecoded_live: 5,
    chunks_total: 6,
    linguist_stage: "deterministic",
    post_processed_text: "Polished 2.",
  }),
]);
assert.ok(summary);
assert.equal(summary.sample_count, 2);
assert.equal(summary.total_audio_s, 45);
assert.equal(summary.p50_release_to_done_ms, 950);
assert.equal(summary.p95_release_to_done_ms, 1120);
assert.equal(summary.polished_count, 2);
assert.equal(summary.bottleneck, "none");
assert.ok(
  summary.live_predecoded_ratio != null &&
    Math.abs(summary.live_predecoded_ratio - 16 / 18) < 1e-6,
);

assert.equal(computeClientTelemetrySummary([makeEntry()]), null);

const normal2sCleanup = computeClientTelemetrySummary([
  makeEntry({
    id: 10,
    audio_duration_s: 35,
    asr_ms: 850,
    cleanup_ms: 2400,
    release_to_done_ms: 2800,
    real_time_factor: 0.025,
    chunks_predecoded_live: 6,
    chunks_total: 7,
    linguist_stage: "model_polished",
    post_processed_text: "Cleaned.",
  }),
]);
assert.equal(normal2sCleanup?.bottleneck, "none");

const slow4sCleanup = computeClientTelemetrySummary([
  makeEntry({
    id: 11,
    audio_duration_s: 35,
    asr_ms: 850,
    cleanup_ms: 4200,
    release_to_done_ms: 5050,
    real_time_factor: 0.025,
    chunks_predecoded_live: 6,
    chunks_total: 7,
    linguist_stage: "model_polished",
    post_processed_text: "Cleaned.",
  }),
]);
assert.equal(slow4sCleanup?.bottleneck, "slow_linguist_provider");

console.log("telemetry: all assertions passed");
