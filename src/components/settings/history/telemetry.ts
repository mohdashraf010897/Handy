import type { DictationTelemetrySummaryDto, HistoryEntry } from "@/bindings";

export type RtfConvention = "classic_rtf" | "speed_multiplier";

export type KnownBottleneck =
  | "post_stop_batch_decode"
  | "compute_bound_asr"
  | "slow_linguist_provider"
  | "unpolished_takes";

export type LinguistStageKind = "polished" | "fast_polish" | "raw";

const KNOWN_BOTTLENECKS: ReadonlySet<string> = new Set<KnownBottleneck>([
  "post_stop_batch_decode",
  "compute_bound_asr",
  "slow_linguist_provider",
  "unpolished_takes",
]);

export function isKnownBottleneck(value: string): value is KnownBottleneck {
  return KNOWN_BOTTLENECKS.has(value);
}

/**
 * Format wait latency: `< 1000 ms` as integer ms (`780 ms`), `>= 1000 ms` as seconds (`1.4s`, `1.12s`).
 */
export function formatWaitMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) {
    return "—";
  }
  if (ms < 1000) {
    return `${Math.round(ms)} ms`;
  }
  const seconds = ms / 1000;
  if (seconds >= 10) {
    return `${seconds.toFixed(1)}s`;
  }
  const twoDec = seconds.toFixed(2);
  // Render `1.40` as `1.4s`, while preserving `1.12s` and `2.0s`.
  const trimmed = twoDec.endsWith("0") ? twoDec.slice(0, -1) : twoDec;
  return `${trimmed}s`;
}

/**
 * Format audio duration: `< 60s` as `15.8s`, `>= 60s` as `3m 12s`, `>= 3600s` as `1h 12m`.
 */
export function formatAudioDuration(
  seconds: number | null | undefined,
): string | null {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) {
    return null;
  }
  if (seconds < 60) {
    return `${seconds.toFixed(1)}s`;
  }
  const wholeSeconds = Math.round(seconds);
  if (wholeSeconds >= 3600) {
    const hours = Math.floor(wholeSeconds / 3600);
    const mins = Math.floor((wholeSeconds % 3600) / 60);
    return `${hours}h ${mins}m`;
  }
  const mins = Math.floor(wholeSeconds / 60);
  const secs = wholeSeconds % 60;
  return `${mins}m ${secs}s`;
}

/**
 * Determine whether `real_time_factor` in backend payloads is classic RTF
 * (`asr_s / audio_s`, `< 1` = faster than real-time) or speed multiplier (`audio_s / asr_s`).
 */
export function detectRtfConvention(entries: HistoryEntry[]): RtfConvention {
  let classicVotes = 0;
  let speedVotes = 0;

  for (const entry of entries) {
    const audioS = entry.audio_duration_s;
    const asrMs = entry.asr_ms;
    const rtf = entry.real_time_factor;
    if (
      audioS == null ||
      asrMs == null ||
      rtf == null ||
      audioS <= 0 ||
      asrMs <= 0 ||
      rtf <= 0
    ) {
      continue;
    }

    const classicRtf = asrMs / 1000 / audioS;
    const speedX = audioS / (asrMs / 1000);
    // Skip near-1× ratios where both orientations overlap.
    if (classicRtf > 0.8 && classicRtf < 1.25) {
      continue;
    }

    const logRtf = Math.log(rtf);
    const distClassic = Math.abs(logRtf - Math.log(classicRtf));
    const distSpeed = Math.abs(logRtf - Math.log(speedX));
    if (distClassic < distSpeed) {
      classicVotes += 1;
    } else if (distSpeed < distClassic) {
      speedVotes += 1;
    }
  }

  return speedVotes > classicVotes ? "speed_multiplier" : "classic_rtf";
}

/**
 * Convert a `real_time_factor` / `median_rtf` scalar into `× real-time` throughput.
 */
export function rtfToSpeedMultiplier(
  rtf: number | null | undefined,
  convention: RtfConvention,
  bottleneck?: string,
): number | null {
  if (rtf == null || !Number.isFinite(rtf) || rtf <= 0) {
    return null;
  }
  if (convention === "speed_multiplier") {
    return rtf;
  }
  // If no calibration entries existed and a healthy summary reports > 1 (e.g. `22.4`),
  // interpret it as already expressed in `× real-time`.
  if (rtf > 1 && bottleneck !== undefined && bottleneck !== "compute_bound_asr") {
    return rtf;
  }
  return 1 / rtf;
}

export function resolveEntrySpeedMultiplier(
  entry: HistoryEntry,
  convention: RtfConvention,
): number | null {
  const rtf = entry.real_time_factor;
  if (rtf != null && Number.isFinite(rtf) && rtf > 0) {
    const audioS = entry.audio_duration_s;
    const asrMs = entry.asr_ms;
    if (audioS != null && asrMs != null && audioS > 0 && asrMs > 0) {
      const classicRtf = asrMs / 1000 / audioS;
      const speedX = audioS / (asrMs / 1000);
      if (classicRtf <= 0.8 || classicRtf >= 1.25) {
        const logRtf = Math.log(rtf);
        const distClassic = Math.abs(logRtf - Math.log(classicRtf));
        const distSpeed = Math.abs(logRtf - Math.log(speedX));
        return distSpeed < distClassic ? rtf : 1 / rtf;
      }
    }
    return convention === "speed_multiplier" ? rtf : 1 / rtf;
  }

  const audioS = entry.audio_duration_s;
  const asrMs = entry.asr_ms;
  if (audioS != null && asrMs != null && audioS > 0 && asrMs > 0) {
    return audioS / (asrMs / 1000);
  }
  return null;
}

export function formatSpeedNumber(speedX: number | null | undefined): string | null {
  if (speedX == null || !Number.isFinite(speedX) || speedX <= 0) {
    return null;
  }
  if (speedX >= 100) {
    return String(Math.round(speedX));
  }
  return speedX.toFixed(1);
}

export function hasEntryTelemetry(entry: HistoryEntry): boolean {
  return entry.release_to_done_ms != null || entry.audio_duration_s != null;
}

export function hasDistinctPolishedAndRaw(entry: HistoryEntry): boolean {
  const polished = entry.post_processed_text?.trim() ?? "";
  const raw = entry.transcription_text.trim();
  return polished.length > 0 && raw.length > 0 && polished !== raw;
}

export function resolveLinguistStage(entry: HistoryEntry): LinguistStageKind {
  if (entry.linguist_stage === "model_polished") {
    return "polished";
  }
  if (
    entry.linguist_stage === "deterministic" ||
    entry.linguist_stage === "fallback_deterministic"
  ) {
    return "fast_polish";
  }
  if (entry.linguist_stage === "raw" || !entry.post_processed_text?.trim()) {
    return "raw";
  }
  return "polished";
}

function percentile(values: number[], p: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil((p / 100) * sorted.length) - 1),
  );
  return sorted[idx];
}

function median(values: number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 1) {
    return sorted[mid];
  }
  return (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * Client-side fallback summary computed from loaded history entries when the
 * backend telemetry summary command is unavailable (e.g. in isolated tests) or empty.
 */
export function computeClientTelemetrySummary(
  entries: HistoryEntry[],
  limit = 50,
): DictationTelemetrySummaryDto | null {
  const telemetryEntries = entries
    .filter(
      (entry) =>
        entry.release_to_done_ms != null ||
        entry.audio_duration_s != null ||
        entry.asr_ms != null ||
        entry.real_time_factor != null,
    )
    .slice(0, limit);

  if (telemetryEntries.length === 0) {
    return null;
  }

  const convention = detectRtfConvention(telemetryEntries);

  let totalAudioS = 0;
  const releaseToDone: number[] = [];
  const asrValues: number[] = [];
  const cleanupValues: number[] = [];
  const rtfValues: number[] = [];
  let liveChunksSum = 0;
  let totalChunksSum = 0;
  let polishedCount = 0;

  for (const entry of telemetryEntries) {
    if (entry.audio_duration_s != null && entry.audio_duration_s > 0) {
      totalAudioS += entry.audio_duration_s;
    }
    if (entry.release_to_done_ms != null && entry.release_to_done_ms >= 0) {
      releaseToDone.push(entry.release_to_done_ms);
    }
    if (entry.asr_ms != null && entry.asr_ms >= 0) {
      asrValues.push(entry.asr_ms);
    }
    if (entry.cleanup_ms != null && entry.cleanup_ms >= 0) {
      cleanupValues.push(entry.cleanup_ms);
    }
    if (entry.real_time_factor != null && entry.real_time_factor > 0) {
      rtfValues.push(entry.real_time_factor);
    } else if (
      entry.audio_duration_s != null &&
      entry.audio_duration_s > 0 &&
      entry.asr_ms != null &&
      entry.asr_ms > 0
    ) {
      const derivedClassic = entry.asr_ms / 1000 / entry.audio_duration_s;
      rtfValues.push(
        convention === "speed_multiplier" ? 1 / derivedClassic : derivedClassic,
      );
    }
    if (entry.chunks_total != null && entry.chunks_total > 0) {
      totalChunksSum += entry.chunks_total;
      liveChunksSum += Math.max(0, entry.chunks_predecoded_live ?? 0);
    }
    if (resolveLinguistStage(entry) !== "raw") {
      polishedCount += 1;
    }
  }

  const p50Release = median(releaseToDone);
  const p95Release = percentile(releaseToDone, 95);
  const p50Asr = median(asrValues);
  const p95Cleanup = percentile(cleanupValues, 95);
  const medianRtf = median(rtfValues);
  const liveRatio =
    totalChunksSum > 0 ? Math.min(1, liveChunksSum / totalChunksSum) : null;
  const medianSpeed = rtfToSpeedMultiplier(medianRtf, convention);

  let bottleneck = "none";
  if (
    p95Release != null &&
    p95Release > 3000 &&
    (liveRatio == null || liveRatio < 0.5)
  ) {
    bottleneck = "post_stop_batch_decode";
  } else if (medianSpeed != null && medianSpeed < 6) {
    bottleneck = "compute_bound_asr";
  } else if (p95Cleanup != null && p95Cleanup > 3000) {
    bottleneck = "slow_linguist_provider";
  } else if (
    telemetryEntries.length >= 3 &&
    polishedCount / telemetryEntries.length < 0.5
  ) {
    bottleneck = "unpolished_takes";
  }

  return {
    sample_count: telemetryEntries.length,
    total_audio_s: totalAudioS,
    p50_release_to_done_ms: p50Release,
    p95_release_to_done_ms: p95Release,
    p50_asr_ms: p50Asr,
    p95_cleanup_ms: p95Cleanup,
    median_rtf: medianRtf,
    live_predecoded_ratio: liveRatio,
    polished_count: polishedCount,
    bottleneck,
  };
}
