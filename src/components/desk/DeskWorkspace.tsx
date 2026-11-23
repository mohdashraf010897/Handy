import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useOsType } from "@/hooks/useOsType";
import { useSettings } from "@/hooks/useSettings";
import {
  SESSION_BUS_EVENT,
  computeMeetingPhaseElapsedMs,
  fetchSessionBusSnapshot,
  type SessionBusSnapshot,
} from "@/lib/sessionBus";
import { resolveStudioEgressState } from "@/lib/utils/egressState";
import { useModelStore } from "@/stores/modelStore";
import {
  DEFAULT_LIVE_STATE,
  formatDurationMmSs,
  get_meeting_live_state,
  type MeetingLiveState,
} from "../meetings/api";
import type { StudioSettingsTab } from "../settings/StudioSettingsWorkspace";
import { HistorySettings } from "../settings/history/HistorySettings";
import { Keycaps } from "../shared/Keycaps";

export interface DeskWorkspaceProps {
  onNavigate: (
    target: "meetings" | "lexicon" | "voice" | "settings",
    settingsTab?: StudioSettingsTab,
  ) => void;
}

interface EntriesSummary {
  loadedCount: number;
  wordCount: number;
}

export const DeskWorkspace: React.FC<DeskWorkspaceProps> = ({ onNavigate }) => {
  const { t } = useTranslation();
  const osType = useOsType();
  const { settings } = useSettings();
  const currentModel = useModelStore((state) => state.currentModel);
  const models = useModelStore((state) => state.models);

  const [summary, setSummary] = useState<EntriesSummary>({
    loadedCount: 0,
    wordCount: 0,
  });
  const [liveMeeting, setLiveMeeting] =
    useState<MeetingLiveState>(DEFAULT_LIVE_STATE);
  const latestBusSeqRef = useRef(0);

  const currentModelInfo = useMemo(
    () => models.find((m) => m.id === currentModel),
    [models, currentModel],
  );

  const egressState = useMemo(
    () => resolveStudioEgressState(currentModelInfo, settings),
    [currentModelInfo, settings],
  );

  const transcribeBinding =
    settings?.bindings?.transcribe?.current_binding || "option+space";

  const micLabel =
    settings?.selected_microphone ||
    t("studio.desk.systemDefaultMic", {
      defaultValue: "System default mic",
    });

  const modelLabel =
    currentModelInfo?.name ||
    currentModel ||
    t("studio.desk.localModel", {
      defaultValue: "Local ASR",
    });

  const egressLabel = egressState.cloudAsr
    ? t("studio.desk.cloudAsrSelected", {
        defaultValue: "Cloud ASR selected",
      })
    : egressState.cloudLinguist
      ? t("studio.desk.cloudLlmSelected", {
          defaultValue: "Cloud LLM active",
        })
      : t("studio.desk.zeroEgress", {
          defaultValue: "0 B network egress",
        });

  const vaultSummaryLabel = useMemo(() => {
    if (summary.loadedCount > 0) {
      const countsLabel = t("studio.desk.vaultSummaryCounts", {
        count: summary.loadedCount,
        words: summary.wordCount,
        defaultValue: "{{count}} takes · {{words}} words",
      });
      return `${countsLabel} · ${egressLabel}`;
    }
    return egressLabel;
  }, [egressLabel, summary.loadedCount, summary.wordCount, t]);

  const applyBusSnapshot = useCallback((snapshot: SessionBusSnapshot) => {
    if (snapshot.seq < latestBusSeqRef.current) return;
    latestBusSeqRef.current = snapshot.seq;

    setLiveMeeting((prev) => {
      switch (snapshot.meeting.phase) {
        case "recording":
          return {
            ...prev,
            status: "recording",
            active_meeting_id: snapshot.meeting.meeting_id,
            title: snapshot.meeting.title,
            elapsed_ms: computeMeetingPhaseElapsedMs(
              snapshot.meeting,
              snapshot.server_now_ms,
            ),
            capture_mode: snapshot.meeting.capture_mode,
            app_name: snapshot.meeting.app_name,
          };
        case "paused":
        case "soft_stopped":
          return {
            ...prev,
            status: "paused",
            active_meeting_id: snapshot.meeting.meeting_id,
            title: snapshot.meeting.title,
            elapsed_ms: computeMeetingPhaseElapsedMs(
              snapshot.meeting,
              snapshot.server_now_ms,
            ),
            capture_mode: snapshot.meeting.capture_mode,
            app_name: snapshot.meeting.app_name,
          };
        case "summarizing":
          return {
            ...prev,
            status: "summarizing",
            active_meeting_id: snapshot.meeting.meeting_id,
            title: snapshot.meeting.title,
          };
        case "idle":
        default:
          return {
            ...prev,
            status: "idle",
            active_meeting_id: null,
            title: "",
            elapsed_ms: 0,
            app_name: null,
          };
      }
    });
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlistenMeeting: (() => void) | undefined;
    let unlistenBus: (() => void) | undefined;

    void get_meeting_live_state()
      .then((state) => {
        if (!disposed && state) {
          setLiveMeeting(state);
        }
      })
      .catch(() => {});

    const setup = async () => {
      const stopMeeting = await listen<MeetingLiveState>(
        "meeting-state-changed",
        (event) => {
          if (!disposed && event.payload && typeof event.payload === "object") {
            setLiveMeeting(event.payload);
          }
        },
      );
      if (disposed) {
        stopMeeting();
        return;
      }
      unlistenMeeting = stopMeeting;

      const stopBus = await listen<SessionBusSnapshot>(
        SESSION_BUS_EVENT,
        (event) => {
          if (!disposed && event.payload && typeof event.payload === "object") {
            applyBusSnapshot(event.payload);
          }
        },
      );
      if (disposed) {
        stopBus();
        return;
      }
      unlistenBus = stopBus;

      const snapshot = await fetchSessionBusSnapshot();
      if (!disposed && snapshot && snapshot.seq > 0) {
        applyBusSnapshot(snapshot);
      }
    };

    void setup().catch(() => {});

    return () => {
      disposed = true;
      unlistenMeeting?.();
      unlistenBus?.();
    };
  }, [applyBusSnapshot]);

  const isMeetingActive =
    liveMeeting.status === "recording" || liveMeeting.status === "paused";
  const isMeetingPaused = liveMeeting.status === "paused";

  const activeMeetingLabel = useMemo(() => {
    const title =
      liveMeeting.title.trim() ||
      t("studio.desk.untitledMeeting", {
        defaultValue: "Untitled session",
      });
    if (liveMeeting.elapsed_ms > 0) {
      return `${title} · ${formatDurationMmSs(liveMeeting.elapsed_ms)}`;
    }
    return title;
  }, [liveMeeting.elapsed_ms, liveMeeting.title, t]);

  return (
    <div className="max-w-4xl w-full mx-auto space-y-5">
      {/* 1. Header Row */}
      <header className="flex items-baseline justify-between gap-4">
        <h1 className="text-title font-display font-semibold text-text tracking-tight">
          {t("studio.desk.title", { defaultValue: "Desk" })}
        </h1>
        <p className="font-mono text-xs text-ink-2 tabular-nums">
          {vaultSummaryLabel}
        </p>
      </header>

      {/* 2. Machined Hardware Trigger Strip */}
      <section
        aria-label={t("studio.desk.triggerStripAria", {
          defaultValue: "Dictation trigger status",
        })}
        className="rounded-card border border-line-strong bg-surface px-4 py-3.5 flex flex-wrap sm:flex-nowrap items-center justify-between gap-3"
      >
        <div className="flex items-center gap-3.5 min-w-0">
          <Keycaps
            binding={transcribeBinding}
            osType={osType}
            size="sm"
            className="shrink-0"
          />
          <div className="min-w-0 flex flex-col gap-0.5">
            <p className="text-sm font-medium text-text truncate">
              {t("studio.desk.holdToYap", {
                defaultValue: "Hold to yap · release to type anywhere",
              })}
            </p>
            <p className="font-mono text-xs text-ink-2 truncate">
              {t("studio.desk.triggerStatus", {
                mic: micLabel,
                model: modelLabel,
                defaultValue: "Ready · {{mic}} · {{model}}",
              })}
            </p>
          </div>
        </div>

        <div className="flex items-center gap-2.5 shrink-0">
          <span
            className={`font-mono text-[11px] font-semibold px-2.5 py-1 rounded-chip whitespace-nowrap ${
              egressState.hasCloudEgress
                ? "bg-warning/15 text-warning"
                : "bg-moss-wash text-moss"
            }`}
          >
            {egressState.cloudAsr
              ? t("studio.desk.cloudSeal", {
                  defaultValue: "Cloud ASR active",
                })
              : egressState.cloudLinguist
                ? t("studio.desk.cloudLlmSeal", {
                    defaultValue: "Cloud LLM active",
                  })
                : t("studio.desk.onDeviceSeal", {
                    defaultValue: "On-device · 0 B sent",
                  })}
          </span>
          <button
            type="button"
            onClick={() => onNavigate("settings", "shortcuts")}
            className="px-2.5 py-1 rounded-control border border-line bg-rail text-xs font-medium text-ink-2 hover:text-text hover:border-line-strong transition-colors cursor-pointer whitespace-nowrap"
          >
            {t("studio.desk.shortcutAndMic", {
              defaultValue: "Shortcut & Mic",
            })}
          </button>
        </div>
      </section>

      {(currentModel === "whisper-large-v3-gguf" ||
        (Boolean(currentModel?.includes("whisper-large-v3-gguf")) &&
          !currentModel?.includes("turbo"))) && (
        <div
          role="status"
          className="rounded-control border border-line bg-surface px-3.5 py-2 flex flex-wrap sm:flex-nowrap items-center justify-between gap-3"
        >
          <p className="text-xs text-ink-2">
            <span className="font-semibold text-text">
              {t("studio.desk.turboHintLead", {
                defaultValue: "Heavy speech model active",
              })}
            </span>{" "}
            {t("studio.desk.turboHintBody", {
              defaultValue:
                "— switch to Whisper Large v3 Turbo or Parakeet for 3–5× faster live typing.",
            })}
          </p>
          <button
            type="button"
            onClick={() => onNavigate("settings", "models")}
            className="px-2.5 py-1 rounded-control border border-line-strong bg-rail text-xs font-medium text-text hover:border-logo-primary transition-colors cursor-pointer whitespace-nowrap shrink-0"
          >
            {t("studio.desk.switchEngineButton", {
              defaultValue: "Choose Faster Model →",
            })}
          </button>
        </div>
      )}

      {/* 3. Contextual 1-Line Bot-Free 2ch Studio Tape Bar */}
      <section
        aria-label={t("studio.desk.tapeBarAria", {
          defaultValue: "Dual-channel studio tape status",
        })}
        className="rounded-control border border-line bg-rail px-3.5 py-2.5 flex items-center justify-between gap-3"
      >
        {isMeetingActive ? (
          <>
            <div className="flex items-center gap-2.5 min-w-0">
              <span
                aria-hidden="true"
                className={`w-2 h-2 rounded-full shrink-0 ${
                  isMeetingPaused ? "bg-warning" : "bg-moss animate-pulse"
                }`}
              />
              <span className="font-mono text-[11px] font-semibold px-2 py-0.5 rounded-chip border border-line bg-surface text-text shrink-0">
                {isMeetingPaused
                  ? t("studio.desk.meetingPausedBadge", {
                      defaultValue: "PAUSED",
                    })
                  : t("studio.desk.meetingRecordingBadge", {
                      defaultValue: "LIVE TAPE",
                    })}
              </span>
              <span className="text-xs font-medium text-text truncate tabular-nums">
                {activeMeetingLabel}
              </span>
            </div>
            <button
              type="button"
              onClick={() => onNavigate("meetings")}
              className="px-2.5 py-1 rounded-control border border-line-strong bg-surface text-xs font-medium text-text hover:bg-background transition-colors cursor-pointer shrink-0 whitespace-nowrap"
            >
              {t("studio.desk.openLiveTape", {
                defaultValue: "Open Live Tape",
              })}
            </button>
          </>
        ) : (
          <>
            <div className="flex items-center gap-2.5 min-w-0">
              <span className="font-mono text-[11px] font-semibold px-2 py-0.5 rounded-chip border border-line bg-surface text-ink-2 shrink-0">
                {t("studio.desk.tapeBadge", {
                  defaultValue: "2CH TAPE",
                })}
              </span>
              <span className="text-xs text-ink-2 truncate">
                {t("studio.desk.meetingIdleHint", {
                  defaultValue:
                    "Ready for calls & lectures — records cleanly on your Mac with zero bots",
                })}
              </span>
            </div>
            <button
              type="button"
              onClick={() => onNavigate("meetings")}
              className="px-2.5 py-1 rounded-control border border-line bg-surface text-xs font-medium text-ink-2 hover:text-text hover:border-line-strong transition-colors cursor-pointer shrink-0 whitespace-nowrap"
            >
              {t("studio.desk.openMeetings", {
                defaultValue: "Open Meetings",
              })}
            </button>
          </>
        )}
      </section>

      {/* 4. Embedded Chronological Takes Ledger */}
      <HistorySettings
        embedded
        onEntriesSummary={setSummary}
        onOpenModels={() => onNavigate("settings", "models")}
      />
    </div>
  );
};
