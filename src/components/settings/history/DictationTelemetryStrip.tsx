import React, { useEffect, useMemo, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type DictationTelemetrySummaryDto,
  type HistoryEntry,
} from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import {
  computeClientTelemetrySummary,
  detectRtfConvention,
  formatAudioDuration,
  formatSpeedNumber,
  formatWaitMs,
  isKnownBottleneck,
  rtfToSpeedMultiplier,
} from "./telemetry";

const SUMMARY_SAMPLE_LIMIT = 50;

interface DictationTelemetryStripProps {
  entries: HistoryEntry[];
  onOpenModels?: () => void;
}

export const DictationTelemetryStrip: React.FC<
  DictationTelemetryStripProps
> = ({ entries, onOpenModels }) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting } = useSettings();
  const postProcessEnabled = Boolean(getSetting("post_process_enabled"));
  const [backendSummary, setBackendSummary] =
    useState<DictationTelemetrySummaryDto | null>(null);

  useEffect(() => {
    let cancelled = false;

    const loadSummary = async () => {
      try {
        const result =
          await commands.getDictationTelemetrySummary(SUMMARY_SAMPLE_LIMIT);
        if (!cancelled) {
          if (result.status === "ok" && result.data.sample_count > 0) {
            setBackendSummary(result.data);
          } else {
            setBackendSummary(null);
          }
        }
      } catch {
        if (!cancelled) {
          setBackendSummary(null);
        }
      }
    };

    void loadSummary();

    return () => {
      cancelled = true;
    };
  }, [entries]);

  const fallbackSummary = useMemo(
    () => computeClientTelemetrySummary(entries, SUMMARY_SAMPLE_LIMIT),
    [entries],
  );

  const summary =
    backendSummary && backendSummary.sample_count > 0
      ? backendSummary
      : fallbackSummary;

  const rtfConvention = useMemo(() => detectRtfConvention(entries), [entries]);

  if (!summary || summary.sample_count === 0) {
    return null;
  }

  const p50Text = formatWaitMs(summary.p50_release_to_done_ms);
  const p50Color =
    summary.p50_release_to_done_ms == null
      ? "text-text"
      : summary.p50_release_to_done_ms < 1500
        ? "text-moss"
        : summary.p50_release_to_done_ms >= 4000
          ? "text-warning"
          : "text-text";

  const p95Text = formatWaitMs(summary.p95_release_to_done_ms);
  const p95IsWarning =
    summary.p95_release_to_done_ms != null &&
    summary.p95_release_to_done_ms > 3000;

  const speedX = rtfToSpeedMultiplier(
    summary.median_rtf,
    rtfConvention,
    summary.bottleneck,
  );
  const speedFormatted = formatSpeedNumber(speedX);
  const speedIsWarning = speedX != null && speedX < 6;

  const livePercent =
    summary.live_predecoded_ratio != null &&
    Number.isFinite(summary.live_predecoded_ratio)
      ? Math.round(
          Math.max(0, Math.min(1, summary.live_predecoded_ratio)) * 100,
        )
      : null;

  const polishedPercent =
    summary.sample_count > 0
      ? Math.round(
          Math.max(
            0,
            Math.min(1, summary.polished_count / summary.sample_count),
          ) * 100,
        )
      : 0;

  const totalAudioFormatted =
    summary.total_audio_s > 0
      ? formatAudioDuration(summary.total_audio_s)
      : null;

  const activeBottleneck =
    summary.bottleneck === "slow_linguist_provider" && !postProcessEnabled
      ? "none"
      : summary.bottleneck === "unpolished_takes" && postProcessEnabled
        ? "none"
        : summary.bottleneck;

  const bottleneckMessage =
    activeBottleneck !== "none" && isKnownBottleneck(activeBottleneck)
      ? t(`settings.history.telemetry.bottlenecks.${activeBottleneck}`)
      : null;

  return (
    <section
      aria-label={t("settings.history.telemetry.sectionTitle")}
      className="space-y-2"
    >
      <div className="px-4 flex items-center justify-between gap-2">
        <h2 className="text-xs font-medium text-ink-3 uppercase tracking-wide">
          {t("settings.history.telemetry.sectionTitle")}
        </h2>
        <div className="flex items-center gap-1.5 text-xs text-ink-3 tabular-nums">
          <span>
            {t("settings.history.telemetry.recentTakes", {
              count: summary.sample_count,
            })}
          </span>
          {totalAudioFormatted && (
            <>
              <span aria-hidden="true">·</span>
              <span>
                {t("settings.history.telemetry.totalAudio", {
                  duration: totalAudioFormatted,
                })}
              </span>
            </>
          )}
        </div>
      </div>

      <div className="grid grid-cols-2 sm:grid-cols-4 gap-2.5">
        <div className="rounded-lg bg-wash border border-line px-3 py-2.5 flex flex-col gap-0.5">
          <span className="text-[11px] font-medium text-ink-3">
            {t("settings.history.telemetry.medianWaitLabel")}
          </span>
          <span
            className={`text-base font-semibold tracking-tight tabular-nums ${p50Color}`}
          >
            {p50Text}
          </span>
          <span className="text-[11px] text-ink-3 tabular-nums">
            {summary.p50_asr_ms != null
              ? t("settings.history.telemetry.medianWaitSub", {
                  duration: formatWaitMs(summary.p50_asr_ms),
                })
              : t("settings.history.telemetry.medianWaitFallbackSub")}
          </span>
        </div>

        <div className="rounded-lg bg-wash border border-line px-3 py-2.5 flex flex-col gap-0.5">
          <span className="text-[11px] font-medium text-ink-3">
            {t("settings.history.telemetry.tailWaitLabel")}
          </span>
          <span
            className={`text-base font-semibold tracking-tight tabular-nums ${
              p95IsWarning ? "text-warning" : "text-text"
            }`}
          >
            {p95Text}
          </span>
          <span className="text-[11px] text-ink-3 tabular-nums">
            {summary.p95_cleanup_ms != null
              ? t("settings.history.telemetry.tailWaitSub", {
                  duration: formatWaitMs(summary.p95_cleanup_ms),
                })
              : t("settings.history.telemetry.tailWaitFallbackSub")}
          </span>
        </div>

        <div className="rounded-lg bg-wash border border-line px-3 py-2.5 flex flex-col gap-0.5">
          <span className="text-[11px] font-medium text-ink-3">
            {t("settings.history.telemetry.asrSpeedLabel")}
          </span>
          <span
            className={`text-base font-semibold tracking-tight tabular-nums ${
              speedIsWarning ? "text-warning" : "text-text"
            }`}
          >
            {speedFormatted != null
              ? t("settings.history.telemetry.asrSpeedValue", {
                  speed: speedFormatted,
                })
              : "—"}
          </span>
          <span
            className={`text-[11px] tabular-nums ${
              livePercent != null && livePercent >= 70
                ? "text-moss"
                : "text-ink-2"
            }`}
          >
            {livePercent != null
              ? t("settings.history.telemetry.asrSpeedLiveRatio", {
                  percent: livePercent,
                })
              : t("settings.history.telemetry.asrSpeedFallbackSub")}
          </span>
        </div>

        <div className="rounded-lg bg-wash border border-line px-3 py-2.5 flex flex-col gap-0.5">
          <span className="text-[11px] font-medium text-ink-3">
            {t("settings.history.telemetry.linguistCoverageLabel")}
          </span>
          <span className="text-base font-semibold tracking-tight tabular-nums text-text">
            {summary.polished_count} / {summary.sample_count}{" "}
            <span className="text-xs font-normal text-ink-2">
              {t("settings.history.telemetry.linguistCoverageUnit")}
            </span>
          </span>
          <span className="text-[11px] text-ink-3 tabular-nums">
            {t("settings.history.telemetry.linguistCoverageSub", {
              percent: polishedPercent,
            })}
          </span>
        </div>
      </div>

      {bottleneckMessage && (
        <div
          role="status"
          className="rounded-lg border border-warning/30 bg-wash px-3 py-2.5 flex flex-wrap sm:flex-nowrap items-center justify-between gap-3 text-xs text-ink-2"
        >
          <div className="flex items-start gap-2 min-w-0">
            <AlertTriangle
              className="w-3.5 h-3.5 text-warning shrink-0 mt-0.5"
              aria-hidden="true"
            />
            <span>{bottleneckMessage}</span>
          </div>
          {activeBottleneck === "slow_linguist_provider" && (
            <button
              type="button"
              onClick={() => void updateSetting("post_process_enabled", false)}
              className="px-2.5 py-1 rounded-control border border-line-strong bg-surface text-xs font-medium text-text hover:bg-background transition-colors cursor-pointer shrink-0 whitespace-nowrap"
            >
              {t("settings.history.telemetry.actions.useInstantCleanup", {
                defaultValue: "Use Instant Cleanup",
              })}
            </button>
          )}
          {activeBottleneck === "compute_bound_asr" && onOpenModels && (
            <button
              type="button"
              onClick={onOpenModels}
              className="px-2.5 py-1 rounded-control border border-line-strong bg-surface text-xs font-medium text-text hover:bg-background transition-colors cursor-pointer shrink-0 whitespace-nowrap"
            >
              {t("settings.history.telemetry.actions.chooseFasterModel", {
                defaultValue: "Choose Faster Model",
              })}
            </button>
          )}
          {activeBottleneck === "unpolished_takes" && (
            <button
              type="button"
              onClick={() => void updateSetting("post_process_enabled", true)}
              className="px-2.5 py-1 rounded-control border border-line-strong bg-surface text-xs font-medium text-text hover:bg-background transition-colors cursor-pointer shrink-0 whitespace-nowrap"
            >
              {t("settings.history.telemetry.actions.turnOnCleanup", {
                defaultValue: "Turn On Smart Cleanup",
              })}
            </button>
          )}
        </div>
      )}
    </section>
  );
};
