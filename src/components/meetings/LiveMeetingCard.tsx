import React, { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Headphones,
  Mic,
  Pause,
  Play,
  Radio,
  ShieldCheck,
  Sparkles,
  Square,
  Volume2,
} from "lucide-react";
import { Button } from "../ui/Button";
import { Input } from "../ui/Input";
import { Switch } from "../ui/Switch";
import { Textarea } from "../ui/Textarea";
import {
  MEETING_TEMPLATES,
  type MeetingLiveState,
  type MeetingTemplate,
  formatDurationMmSs,
  formatSegmentSpeakerBadge,
  isolateBidi,
  update_live_rough_notes,
} from "./api";

interface LiveMeetingCardProps {
  liveState: MeetingLiveState;
  isBusy: boolean;
  onStart: (title: string | null, template: MeetingTemplate) => Promise<void>;
  onPause: () => Promise<void>;
  onResume: () => Promise<void>;
  onStop: () => Promise<void>;
  onToggleAutoDetect: (enabled: boolean) => Promise<void>;
  onDismissDetectedCall: (callId?: string | null) => Promise<void>;
}

const ROUGH_NOTES_DEBOUNCE_MS = 350;

export const LiveMeetingCard: React.FC<LiveMeetingCardProps> = ({
  liveState,
  isBusy,
  onStart,
  onPause,
  onResume,
  onStop,
  onToggleAutoDetect,
  onDismissDetectedCall,
}) => {
  const { t } = useTranslation();
  const titleInputId = useId();
  const templateSelectId = useId();
  const roughNotesId = useId();

  const [draftTitle, setDraftTitle] = useState("");
  const [draftTemplate, setDraftTemplate] =
    useState<MeetingTemplate>("general");
  const [roughNotes, setRoughNotes] = useState(liveState.rough_notes);
  const [saveIndicator, setSaveIndicator] = useState<"idle" | "saving" | "saved">(
    "idle",
  );
  const [displayElapsedMs, setDisplayElapsedMs] = useState(
    liveState.elapsed_ms,
  );
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout>>();
  const latestRoughNotesRef = useRef(liveState.rough_notes);
  const dirtyRoughNotesRef = useRef(false);
  const statusRef = useRef(liveState.status);
  statusRef.current = liveState.status;

  // Keep rough notes synced when a new session starts or finishes.
  useEffect(() => {
    setRoughNotes(liveState.rough_notes);
    latestRoughNotesRef.current = liveState.rough_notes;
    dirtyRoughNotesRef.current = false;
  }, [liveState.active_meeting_id]);

  // Smooth 1s local timer while recording, anchored to backend elapsed_ms.
  useEffect(() => {
    setDisplayElapsedMs(liveState.elapsed_ms);
    if (liveState.status !== "recording") return;
    const interval = setInterval(() => {
      setDisplayElapsedMs((prev) => prev + 1000);
    }, 1000);
    return () => clearInterval(interval);
  }, [liveState.elapsed_ms, liveState.status]);

  useEffect(() => {
    return () => {
      clearTimeout(debounceTimerRef.current);
      if (
        dirtyRoughNotesRef.current &&
        (statusRef.current === "recording" || statusRef.current === "paused")
      ) {
        dirtyRoughNotesRef.current = false;
        void update_live_rough_notes(latestRoughNotesRef.current).catch(
          () => {},
        );
      }
    };
  }, []);

  const flushPendingRoughNotes = async () => {
    clearTimeout(debounceTimerRef.current);
    if (!dirtyRoughNotesRef.current) return;
    dirtyRoughNotesRef.current = false;
    try {
      await update_live_rough_notes(latestRoughNotesRef.current);
      setSaveIndicator("saved");
    } catch {
      setSaveIndicator("idle");
    }
  };

  const handleRoughNotesChange = (value: string) => {
    setRoughNotes(value);
    latestRoughNotesRef.current = value;
    dirtyRoughNotesRef.current = true;
    setSaveIndicator("saving");
    clearTimeout(debounceTimerRef.current);
    debounceTimerRef.current = setTimeout(async () => {
      dirtyRoughNotesRef.current = false;
      try {
        await update_live_rough_notes(value);
        setSaveIndicator("saved");
      } catch {
        setSaveIndicator("idle");
      }
    }, ROUGH_NOTES_DEBOUNCE_MS);
  };

  const handleStopWithFlush = async () => {
    await flushPendingRoughNotes();
    await onStop();
  };

  const handleStartClick = async () => {
    const trimmed = draftTitle.trim();
    await onStart(trimmed.length > 0 ? trimmed : null, draftTemplate);
    setDraftTitle("");
  };

  const isIdle = liveState.status === "idle";
  const isRecording = liveState.status === "recording";
  const isPaused = liveState.status === "paused";
  const isSummarizing = liveState.status === "summarizing";
  const detectedCall = liveState.detected_call;

  const statusLabel = (() => {
    switch (liveState.status) {
      case "recording":
        return t("meetings.status.recording");
      case "paused":
        return t("meetings.status.paused");
      case "summarizing":
        return t("meetings.status.summarizing");
      default:
        return t("meetings.status.idle");
    }
  })();

  const captureBadgeLabel =
    liveState.capture_mode === "dual_stream"
      ? t("meetings.badges.systemAndMic")
      : t("meetings.badges.micOnly");

  const routeBadgeLabel =
    liveState.output_route === "headphones"
      ? t("meetings.badges.headphones")
      : t("meetings.badges.speakerEchoGuard");

  const formattedTimer = `[${formatDurationMmSs(displayElapsedMs)}]`;

  return (
    <section
      aria-label={t("meetings.title")}
      className="bg-background border border-line rounded-lg overflow-hidden divide-y divide-line"
    >
      {/* Subtle Bot-Free Privacy + Consent + Auto-Detect Bar (D-19) */}
      <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5 bg-wash/60">
        <div className="flex items-center gap-2 min-w-0">
          <ShieldCheck
            className="w-4 h-4 shrink-0 text-moss"
            aria-hidden="true"
          />
          <p className="text-xs text-ink-2">
            {t("meetings.privacyConsentBar")}
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <span className="text-xs font-medium text-text">
            {t("meetings.autoDetect.label")}
          </span>
          <Switch
            checked={liveState.auto_detect_enabled}
            disabled={isBusy}
            ariaLabel={t("meetings.autoDetect.label")}
            onChange={(enabled) => void onToggleAutoDetect(enabled)}
          />
        </div>
      </div>

      {/* Detected Call Banner when idle */}
      {isIdle && detectedCall && (
        <div
          role="region"
          aria-label={t("meetings.detectedBanner.title", {
            app: isolateBidi(detectedCall.app_name),
          })}
          className="flex flex-wrap items-center justify-between gap-3 px-4 py-3 bg-logo-primary/15"
        >
          <div className="flex items-center gap-2.5 min-w-0">
            <Radio
              className="w-4 h-4 shrink-0 text-overlay-accent animate-pulse motion-reduce:animate-none"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <p className="text-sm font-semibold text-text truncate">
                {t("meetings.detectedBanner.title", {
                  app: isolateBidi(detectedCall.app_name),
                })}
              </p>
              <p className="text-xs text-ink-2">
                {t("meetings.detectedBanner.subtitle")}
              </p>
            </div>
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <Button
              variant="primary"
              size="sm"
              disabled={isBusy}
              aria-label={t("meetings.detectedBanner.startButton")}
              onClick={() =>
                void onStart(
                  draftTitle.trim() || null,
                  draftTemplate,
                )
              }
            >
              {t("meetings.detectedBanner.startButton")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={isBusy}
              aria-label={t("meetings.detectedBanner.dismissButton")}
              onClick={() => void onDismissDetectedCall(detectedCall.call_id)}
            >
              {t("meetings.detectedBanner.dismissButton")}
            </Button>
          </div>
        </div>
      )}

      {/* Idle Start Bar */}
      {isIdle ? (
        <div className="p-4 space-y-3">
          <div className="flex flex-wrap items-center gap-2.5">
            <div className="min-w-[14rem] flex-1">
              <Input
                id={titleInputId}
                variant="default"
                className="w-full"
                value={draftTitle}
                disabled={isBusy}
                dir="auto"
                placeholder={t("meetings.idle.titlePlaceholder")}
                aria-label={t("meetings.idle.titleAriaLabel")}
                onChange={(e) => setDraftTitle(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void handleStartClick();
                  }
                }}
              />
            </div>

            <div className="flex items-center gap-1.5">
              <label
                htmlFor={templateSelectId}
                className="text-xs font-medium text-ink-2 sr-only"
              >
                {t("meetings.idle.templateLabel")}
              </label>
              <select
                id={templateSelectId}
                value={draftTemplate}
                disabled={isBusy}
                aria-label={t("meetings.idle.templateAriaLabel")}
                onChange={(e) =>
                  setDraftTemplate(e.target.value as MeetingTemplate)
                }
                className="px-3 py-2 text-sm font-semibold bg-wash border border-line-strong rounded-md text-text hover:border-logo-primary focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary cursor-pointer"
              >
                {MEETING_TEMPLATES.map((tpl) => (
                  <option key={tpl} value={tpl}>
                    {t(`meetings.templates.${tpl}`)}
                  </option>
                ))}
              </select>
            </div>

            <Button
              variant="primary"
              size="md"
              disabled={isBusy}
              aria-label={t("meetings.idle.startButton")}
              onClick={() => void handleStartClick()}
              className="flex items-center gap-1.5"
            >
              <Mic className="w-4 h-4" aria-hidden="true" />
              <span>{t("meetings.idle.startButton")}</span>
            </Button>
          </div>
        </div>
      ) : (
        /* Active / Paused / Summarizing Studio */
        <div className="p-4 space-y-4">
          {/* Studio Header: Badges + Controls */}
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex flex-wrap items-center gap-2 min-w-0">
              <span
                className="font-mono text-sm font-semibold text-text tabular-nums"
                aria-label={formatDurationMmSs(displayElapsedMs)}
              >
                {formattedTimer}
              </span>

              {/* Status Pill */}
              <span
                className={`inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-xs font-semibold border ${
                  isRecording
                    ? "bg-moss-wash text-moss border-moss/30"
                    : isPaused
                      ? "bg-warning-wash text-warning border-warning/30"
                      : "bg-logo-primary/20 text-text border-logo-primary/40"
                }`}
              >
                <span
                  aria-hidden="true"
                  className={`w-2 h-2 rounded-full ${
                    isRecording
                      ? "bg-moss animate-pulse motion-reduce:animate-none"
                      : isPaused
                        ? "bg-warning"
                        : "bg-overlay-accent animate-pulse motion-reduce:animate-none"
                  }`}
                />
                <span>{statusLabel}</span>
              </span>

              {/* Capture Mode Badge */}
              <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-wash border border-line text-ink-2">
                <Mic className="w-3 h-3" aria-hidden="true" />
                <span>{captureBadgeLabel}</span>
              </span>

              {/* Audio Route Badge */}
              <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-wash border border-line text-ink-2">
                {liveState.output_route === "headphones" ? (
                  <Headphones className="w-3 h-3" aria-hidden="true" />
                ) : (
                  <Volume2 className="w-3 h-3" aria-hidden="true" />
                )}
                <span>{routeBadgeLabel}</span>
              </span>

              {/* Rolling 5-min rollup count badge */}
              {liveState.rollup_count > 0 && (
                <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-logo-primary/15 border border-logo-primary/30 text-text">
                  <Sparkles className="w-3 h-3" aria-hidden="true" />
                  <span>
                    {t("meetings.badges.rollups", {
                      count: liveState.rollup_count,
                    })}
                  </span>
                </span>
              )}
            </div>

            {/* Session Controls */}
            <div className="flex items-center gap-2 shrink-0">
              {(isRecording || isPaused) && (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={isBusy || isSummarizing}
                  aria-label={
                    isPaused
                      ? t("meetings.controls.resume")
                      : t("meetings.controls.pause")
                  }
                  onClick={() =>
                    void (isPaused ? onResume() : onPause())
                  }
                  className="flex items-center gap-1.5"
                >
                  {isPaused ? (
                    <>
                      <Play className="w-3.5 h-3.5" aria-hidden="true" />
                      <span>{t("meetings.controls.resume")}</span>
                    </>
                  ) : (
                    <>
                      <Pause className="w-3.5 h-3.5" aria-hidden="true" />
                      <span>{t("meetings.controls.pause")}</span>
                    </>
                  )}
                </Button>
              )}

              <Button
                variant="primary"
                size="sm"
                disabled={isBusy || isSummarizing}
                aria-label={t("meetings.controls.finishAndSummarize")}
                onClick={() => void handleStopWithFlush()}
                className="flex items-center gap-1.5"
              >
                <Square className="w-3.5 h-3.5" aria-hidden="true" />
                <span>{t("meetings.controls.finishAndSummarize")}</span>
              </Button>
            </div>
          </div>

          {/* Granola-Style Split Live Workspace: Rough Bullets + Live Transcript */}
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            {/* Left: Your Rough Bullets */}
            <div className="flex flex-col space-y-1.5">
              <div className="flex items-center justify-between">
                <label
                  htmlFor={roughNotesId}
                  className="text-xs font-semibold uppercase tracking-wide text-ink-2"
                >
                  {t("meetings.live.roughBulletsTitle")}
                </label>
                <span className="text-xs text-ink-3" aria-live="polite">
                  {saveIndicator === "saving"
                    ? t("meetings.live.saving")
                    : saveIndicator === "saved"
                      ? t("meetings.live.saved")
                      : t("meetings.live.roughBulletsHint")}
                </span>
              </div>
              <Textarea
                id={roughNotesId}
                dir="auto"
                value={roughNotes}
                disabled={isSummarizing}
                placeholder={t("meetings.live.roughBulletsPlaceholder")}
                aria-label={t("meetings.live.roughBulletsTitle")}
                onChange={(e) => handleRoughNotesChange(e.target.value)}
                className="w-full min-h-[160px] font-normal text-sm leading-relaxed"
              />
            </div>

            {/* Right: Live Transcript Feed */}
            <div className="flex flex-col space-y-1.5">
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold uppercase tracking-wide text-ink-2">
                  {t("meetings.live.transcriptTitle")}
                </span>
                {liveState.app_name && (
                  <span className="text-xs text-ink-3 truncate">
                    {isolateBidi(liveState.app_name)}
                  </span>
                )}
              </div>
              <div
                role="log"
                aria-live="polite"
                aria-label={t("meetings.live.transcriptTitle")}
                className="flex-1 min-h-[160px] max-h-[220px] overflow-y-auto rounded-md border border-line bg-wash/50 p-3 space-y-2"
              >
                {liveState.recent_segments.length === 0 ? (
                  <p className="text-xs text-ink-3 italic">
                    {t("meetings.live.transcriptEmpty")}
                  </p>
                ) : (
                  liveState.recent_segments.map((seg) => {
                    const isMe = seg.speaker === "me";
                    const timeTag = `[${formatDurationMmSs(seg.start_ms)}]`;
                    return (
                      <div
                        key={seg.id}
                        className="flex items-start gap-2 text-xs leading-relaxed"
                      >
                        <span className="font-mono text-ink-3 shrink-0 tabular-nums">
                          {timeTag}
                        </span>
                        <span
                          title={formatSegmentSpeakerBadge(
                            seg,
                            t("meetings.speaker.me"),
                            t("meetings.speaker.others"),
                          )}
                          className={`px-1.5 py-0.5 rounded font-semibold shrink-0 max-w-44 truncate ${
                            isMe
                              ? "bg-logo-primary/20 text-text"
                              : "bg-line text-ink-2"
                          }`}
                        >
                          {isolateBidi(
                            formatSegmentSpeakerBadge(
                              seg,
                              t("meetings.speaker.me"),
                              t("meetings.speaker.others"),
                            ),
                          )}
                        </span>
                        <p
                          dir="auto"
                          className="text-text flex-1 break-words"
                        >
                          {isolateBidi(seg.text)}
                        </p>
                      </div>
                    );
                  })
                )}
              </div>
            </div>
          </div>
        </div>
      )}
    </section>
  );
};
