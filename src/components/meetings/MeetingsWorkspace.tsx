import React, { useCallback, useEffect, useId, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { Clock, Search, X } from "lucide-react";
import { Alert } from "../ui/Alert";
import { Input } from "../ui/Input";
import { SettingsGroup } from "../ui/SettingsGroup";
import { LiveMeetingCard } from "./LiveMeetingCard";
import { MeetingDetailPane } from "./MeetingDetailPane";
import {
  SESSION_BUS_EVENT,
  type SessionBusSnapshot,
  computeMeetingPhaseElapsedMs,
  fetchSessionBusSnapshot,
} from "@/lib/sessionBus";
import {
  DEFAULT_LIVE_STATE,
  type DetectedCallInfo,
  type MeetingDetail,
  type MeetingLiveState,
  type MeetingSummaryRecord,
  type MeetingTemplate,
  copy_meeting_markdown,
  delete_meeting,
  dismiss_detected_call,
  formatDurationMmSs,
  get_meeting_detail,
  get_meeting_live_state,
  isolateBidi,
  list_meetings,
  pause_meeting_notes,
  regenerate_meeting_notes,
  resume_meeting_notes,
  set_meeting_auto_detect,
  start_meeting_notes,
  stop_meeting_notes,
  update_meeting_meta,
} from "./api";

const SEARCH_DEBOUNCE_MS = 200;

export const MeetingsWorkspace: React.FC = () => {
  const { t, i18n } = useTranslation();
  const searchInputId = useId();

  const [liveState, setLiveState] =
    useState<MeetingLiveState>(DEFAULT_LIVE_STATE);
  const [meetings, setMeetings] = useState<MeetingSummaryRecord[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedDetail, setSelectedDetail] = useState<MeetingDetail | null>(
    null,
  );
  const [searchQuery, setSearchQuery] = useState("");
  const [isBusy, setIsBusy] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const searchTimerRef = useRef<ReturnType<typeof setTimeout>>();
  const selectedIdRef = useRef<string | null>(null);
  const searchQueryRef = useRef("");
  const latestBusSeqRef = useRef(0);
  const liveStateRef = useRef<MeetingLiveState>(liveState);
  selectedIdRef.current = selectedId;
  searchQueryRef.current = searchQuery;
  liveStateRef.current = liveState;

  const applyBusSnapshotToWorkspace = useCallback(
    (
      snapshot: SessionBusSnapshot,
      refreshArchiveCb: (q?: string) => Promise<void>,
    ) => {
      if (snapshot.seq < latestBusSeqRef.current) return;
      latestBusSeqRef.current = snapshot.seq;

      const currentLive = liveStateRef.current;
      const becameIdle =
        snapshot.meeting.phase === "idle" &&
        (currentLive.status !== "idle" ||
          currentLive.active_meeting_id !== null);
      const startedNewMeeting =
        (snapshot.meeting.phase === "recording" ||
          snapshot.meeting.phase === "paused" ||
          snapshot.meeting.phase === "soft_stopped") &&
        currentLive.active_meeting_id !== snapshot.meeting.meeting_id;

      setLiveState((prev) => {
        const nextDetectedCall: DetectedCallInfo | null = snapshot.detected_call
          ? {
              call_id: snapshot.detected_call.call_id,
              app_bundle_id:
                prev.detected_call?.call_id === snapshot.detected_call.call_id
                  ? prev.detected_call.app_bundle_id
                  : "",
              app_name: snapshot.detected_call.app_name,
              output_route: snapshot.detected_call.output_route,
              detected_at_ms:
                prev.detected_call?.call_id === snapshot.detected_call.call_id
                  ? prev.detected_call.detected_at_ms
                  : snapshot.server_now_ms,
            }
          : null;

        switch (snapshot.meeting.phase) {
          case "idle": {
            return {
              ...prev,
              status: "idle",
              active_meeting_id: null,
              title: "",
              elapsed_ms: 0,
              app_name: null,
              rough_notes: "",
              recent_segments: [],
              rollup_count: 0,
              detected_call: nextDetectedCall,
            };
          }
          case "recording": {
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
              rough_notes:
                prev.active_meeting_id === snapshot.meeting.meeting_id
                  ? prev.rough_notes
                  : "",
              recent_segments:
                prev.active_meeting_id === snapshot.meeting.meeting_id
                  ? prev.recent_segments
                  : [],
              rollup_count:
                prev.active_meeting_id === snapshot.meeting.meeting_id
                  ? prev.rollup_count
                  : 0,
              detected_call: nextDetectedCall,
            };
          }
          case "paused":
          case "soft_stopped": {
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
              detected_call: nextDetectedCall,
            };
          }
          case "summarizing": {
            return {
              ...prev,
              status: "summarizing",
              active_meeting_id: snapshot.meeting.meeting_id,
              title: snapshot.meeting.title,
              detected_call: nextDetectedCall,
            };
          }
        }
      });

      if (becameIdle) {
        void refreshArchiveCb(searchQueryRef.current);
      } else if (startedNewMeeting) {
        void get_meeting_live_state()
          .then((full) => {
            setLiveState((curr) =>
              curr.active_meeting_id === full.active_meeting_id
                ? { ...full, status: curr.status, elapsed_ms: curr.elapsed_ms }
                : curr,
            );
          })
          .catch(() => {});
      }
    },
    [],
  );

  const refreshArchive = useCallback(async (query?: string) => {
    try {
      const trimmed = (query ?? "").trim();
      const rows = await list_meetings(trimmed.length > 0 ? trimmed : null, 100);
      setMeetings(rows);
      if (rows.length > 0 && !selectedIdRef.current) {
        const firstId = rows[0].id;
        setSelectedId(firstId);
        const detail = await get_meeting_detail(firstId);
        setSelectedDetail(detail);
      } else if (rows.length === 0) {
        setSelectedId(null);
        setSelectedDetail(null);
      }
    } catch {
      // Backend may still be initializing; keep existing state cleanly.
    }
  }, []);

  const refreshLiveState = useCallback(async () => {
    try {
      const next = await get_meeting_live_state();
      setLiveState(next);
    } catch {
      // Ignore transient startup errors.
    }
  }, []);

  useEffect(() => {
    void refreshLiveState();
    void refreshArchive("");

    let disposed = false;
    let teardown: (() => void) | undefined;

    const setupListeners = async () => {
      const unlistenState = await listen<MeetingLiveState>(
        "meeting-state-changed",
        (event) => {
          if (event.payload && typeof event.payload === "object") {
            setLiveState(event.payload);
            if (event.payload.status === "idle") {
              void refreshArchive(searchQueryRef.current);
            }
          } else {
            void refreshLiveState();
          }
        },
      );

      const unlistenSessionBus = await listen<SessionBusSnapshot>(
        SESSION_BUS_EVENT,
        (event) => {
          if (event.payload && typeof event.payload === "object") {
            applyBusSnapshotToWorkspace(event.payload, refreshArchive);
          }
        },
      );

      const unlistenCall = await listen<DetectedCallInfo | null>(
        "call-detected",
        (event) => {
          setLiveState((prev) => ({
            ...prev,
            detected_call: event.payload ?? null,
          }));
        },
      );

      const initialBus = await fetchSessionBusSnapshot();
      if (!disposed && initialBus && initialBus.seq > 0) {
        applyBusSnapshotToWorkspace(initialBus, refreshArchive);
      }

      return () => {
        unlistenState();
        unlistenSessionBus();
        unlistenCall();
      };
    };

    void setupListeners().then((cleanup) => {
      if (disposed) {
        cleanup();
      } else {
        teardown = cleanup;
      }
    });

    return () => {
      disposed = true;
      teardown?.();
      clearTimeout(searchTimerRef.current);
    };
  }, [refreshArchive, refreshLiveState]);

  const handleSearchChange = (value: string) => {
    setSearchQuery(value);
    clearTimeout(searchTimerRef.current);
    searchTimerRef.current = setTimeout(() => {
      void refreshArchive(value);
    }, SEARCH_DEBOUNCE_MS);
  };

  const handleSelectMeeting = async (meetingId: string) => {
    setSelectedId(meetingId);
    try {
      const detail = await get_meeting_detail(meetingId);
      setSelectedDetail(detail);
    } catch (err) {
      setErrorMessage(String(err));
    }
  };

  const handleStart = async (
    title: string | null,
    template: MeetingTemplate,
  ) => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      const next = await start_meeting_notes(title, template);
      setLiveState(next);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setIsBusy(false);
    }
  };

  const handlePause = async () => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      const next = await pause_meeting_notes();
      setLiveState(next);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setIsBusy(false);
    }
  };

  const handleResume = async () => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      const next = await resume_meeting_notes();
      setLiveState(next);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setIsBusy(false);
    }
  };

  const handleStop = async () => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      const detail = await stop_meeting_notes();
      setSelectedId(detail.summary.id);
      setSelectedDetail(detail);
      await refreshLiveState();
      await refreshArchive(searchQuery);
    } catch (err) {
      setErrorMessage(String(err));
      await refreshLiveState();
    } finally {
      setIsBusy(false);
    }
  };

  const handleToggleAutoDetect = async (enabled: boolean) => {
    try {
      const saved = await set_meeting_auto_detect(enabled);
      setLiveState((prev) => ({ ...prev, auto_detect_enabled: saved }));
    } catch (err) {
      setErrorMessage(String(err));
    }
  };

  const handleDismissDetectedCall = async (callId?: string | null) => {
    try {
      await dismiss_detected_call(callId ?? null);
      setLiveState((prev) => ({ ...prev, detected_call: null }));
    } catch (err) {
      setErrorMessage(String(err));
    }
  };

  const handleUpdateMeta = async (params: {
    meetingId: string;
    title?: string | null;
    roughNotes?: string | null;
    template?: MeetingTemplate | null;
  }) => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      const updated = await update_meeting_meta(params);
      setSelectedDetail(updated);
      await refreshArchive(searchQuery);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setIsBusy(false);
    }
  };

  const handleRegenerate = async (
    meetingId: string,
    template?: MeetingTemplate | null,
  ) => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      const updated = await regenerate_meeting_notes(meetingId, template);
      setSelectedDetail(updated);
      await refreshArchive(searchQuery);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setIsBusy(false);
    }
  };

  const handleSaveAndReexpand = async (
    meetingId: string,
    roughNotes: string,
    template: MeetingTemplate,
  ) => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      await update_meeting_meta({
        meetingId,
        roughNotes,
        template,
      });
      const updated = await regenerate_meeting_notes(meetingId, template);
      setSelectedDetail(updated);
      await refreshArchive(searchQuery);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setIsBusy(false);
    }
  };

  const handleCopyMarkdown = async (meetingId: string) => {
    try {
      await copy_meeting_markdown(meetingId);
    } catch (err) {
      setErrorMessage(String(err));
    }
  };

  const handleDelete = async (meetingId: string) => {
    setIsBusy(true);
    setErrorMessage(null);
    try {
      await delete_meeting(meetingId);
      if (selectedId === meetingId) {
        setSelectedId(null);
        setSelectedDetail(null);
      }
      await refreshArchive(searchQuery);
    } catch (err) {
      setErrorMessage(String(err));
    } finally {
      setIsBusy(false);
    }
  };

  const formatStartedDate = (ms: number): string => {
    try {
      return new Intl.DateTimeFormat(i18n.language, {
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      }).format(new Date(ms));
    } catch {
      return new Date(ms).toLocaleDateString();
    }
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      {errorMessage && <Alert variant="error">{errorMessage}</Alert>}

      <SettingsGroup
        title={t("meetings.title")}
        description={t("meetings.subtitle")}
      >
        <LiveMeetingCard
          liveState={liveState}
          isBusy={isBusy}
          onStart={handleStart}
          onPause={handlePause}
          onResume={handleResume}
          onStop={handleStop}
          onToggleAutoDetect={handleToggleAutoDetect}
          onDismissDetectedCall={handleDismissDetectedCall}
        />
      </SettingsGroup>

      {/* Archive + Detail Split View */}
      <div className="space-y-2">
        <div className="px-4 flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-xs font-medium text-ink-3 uppercase tracking-wide">
            {t("meetings.archive.title")}
          </h2>

          {/* Instant Search Bar */}
          <div className="relative min-w-[14rem] flex-1 max-w-xs">
            <Search
              className="w-3.5 h-3.5 text-ink-3 absolute start-2.5 top-1/2 -translate-y-1/2 pointer-events-none"
              aria-hidden="true"
            />
            <Input
              id={searchInputId}
              variant="compact"
              dir="auto"
              value={searchQuery}
              placeholder={t("meetings.archive.searchPlaceholder")}
              aria-label={t("meetings.archive.searchAriaLabel")}
              onChange={(e) => handleSearchChange(e.target.value)}
              className="w-full ps-8 pe-7"
            />
            {searchQuery.length > 0 && (
              <button
                type="button"
                aria-label={t("common.clear")}
                onClick={() => handleSearchChange("")}
                className="absolute end-2 top-1/2 -translate-y-1/2 text-ink-3 hover:text-text cursor-pointer focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary rounded"
              >
                <X className="w-3.5 h-3.5" aria-hidden="true" />
              </button>
            )}
          </div>
        </div>

        <div className="grid grid-cols-1 md:grid-cols-12 gap-4 items-start">
          {/* Left Column: Past Meetings List */}
          <div className="md:col-span-5 bg-background border border-line rounded-lg overflow-hidden divide-y divide-line max-h-[520px] overflow-y-auto">
            {meetings.length === 0 ? (
              <div className="p-4 text-xs text-ink-3">
                {searchQuery.trim().length > 0
                  ? t("meetings.archive.noSearchMatch", {
                      query: isolateBidi(searchQuery.trim()),
                    })
                  : t("meetings.archive.empty")}
              </div>
            ) : (
              meetings.map((item) => {
                const isSelected = item.id === selectedId;
                const durationLabel = formatDurationMmSs(item.duration_ms);
                const dateLabel = formatStartedDate(item.started_at_ms);
                return (
                  <button
                    key={item.id}
                    type="button"
                    aria-label={isolateBidi(item.title)}
                    onClick={() => void handleSelectMeeting(item.id)}
                    className={`w-full text-start px-3.5 py-3 transition-colors cursor-pointer focus:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-logo-primary ${
                      isSelected
                        ? "bg-logo-primary/20 text-text"
                        : "hover:bg-wash text-text"
                    }`}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <p
                        dir="auto"
                        className="text-sm font-semibold truncate text-text"
                      >
                        {isolateBidi(item.title)}
                      </p>
                      <span className="shrink-0 px-2 py-0.5 rounded-full text-[11px] font-medium bg-wash border border-line text-ink-2">
                        {t(`meetings.templates.${item.template}`)}
                      </span>
                    </div>

                    <div className="mt-1 flex items-center gap-2 text-xs text-ink-3">
                      <span className="inline-flex items-center gap-1 font-mono tabular-nums">
                        <Clock className="w-3 h-3" aria-hidden="true" />
                        <span>{durationLabel}</span>
                      </span>
                      <span>{dateLabel}</span>
                      <span>
                        {t("meetings.archive.segmentsCount", {
                          count: item.segment_count,
                        })}
                      </span>
                    </div>

                    {item.preview_snippet && (
                      <p
                        dir="auto"
                        className="mt-1.5 text-xs text-ink-2 line-clamp-2"
                      >
                        {isolateBidi(item.preview_snippet)}
                      </p>
                    )}
                  </button>
                );
              })
            )}
          </div>

          {/* Right Column: Selected Meeting Detail Pane */}
          <div className="md:col-span-7">
            <MeetingDetailPane
              detail={selectedDetail}
              isBusy={isBusy}
              onUpdateMeta={handleUpdateMeta}
              onRegenerate={handleRegenerate}
              onSaveAndReexpand={handleSaveAndReexpand}
              onCopyMarkdown={handleCopyMarkdown}
              onDelete={handleDelete}
            />
          </div>
        </div>
      </div>
    </div>
  );
};
