import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import "./RecordingOverlay.css";
import { commands, events } from "@/bindings";
import type {
  StreamPhase,
  StreamPhaseEvent,
  StreamTextEvent,
  StreamWorkKind,
} from "@/bindings";
import {
  type MeetingLiveState,
  formatDurationMmSs,
  pause_meeting_notes,
  resume_meeting_notes,
  stop_meeting_notes,
} from "@/components/meetings/api";
import { syncLanguageFromSettings } from "@/i18n";
import {
  DEFAULT_MEETING_AUTO_STOP_GRACE_MS,
  DEFAULT_SOFT_CANCEL_GRACE_MS,
  type MeetingActiveOverlay,
  type MeetingSoftStoppedOverlay,
  type MeetingSummarizingOverlay,
  SESSION_BUS_EVENT,
  type SessionBusSnapshot,
  computeLiveMeetingElapsedMs,
  computeSoftCancelProgress,
  confirmCancelRecording,
  fetchSessionBusSnapshot,
  openMeetingsWorkspace,
  projectMeetingOverlay,
  setMeetingPillDismissed,
  undoCancelRecording,
} from "@/lib/sessionBus";
import type { ModelStateEvent } from "@/lib/types/events";
import { getLanguageDirection } from "@/lib/utils/rtl";

type OverlayState =
  | "recording"
  | "streaming"
  | "soft_cancelled"
  | "transcribing"
  | "processing"
  | "meeting_active"
  | "meeting_soft_stopped"
  | "meeting_summarizing"
  | "notice";

interface SoftCancelState {
  takeId: number | null;
  cancelledAtMs: number;
  expiresAtMs: number;
  graceMs: number;
}

type OverlayNoticeKind =
  | "restart_required"
  | "transcription_failed"
  | "mic_muted"
  | "no_speech"
  | "mic_permission"
  | "no_mic"
  | "copied_to_clipboard"
  | "call_detected";

interface DetectedCallPayload {
  call_id: string;
  app_bundle_id?: string;
  bundle_id?: string;
  app_name: string;
}

// 11 center-weighted symmetrical tactile bars driven at 60fps via rAF.
// Incoming `mic-level` events arrive as 16 FFT buckets (~15-30 Hz); we map
// them onto a symmetrical center-peaked arch and spring/lerp at 60fps.
const WAVE_BARS = 11;
const WAVE_CENTER_INDEX = 5;
const WAVE_CENTER_WEIGHTS: readonly number[] = [
  0.36, 0.52, 0.7, 0.86, 0.96, 1.0, 0.96, 0.86, 0.7, 0.52, 0.36,
];

function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

function mapBucketsToSymmetricalTargets(buckets: number[]): number[] {
  const b = (idx: number) => Math.max(0, Math.min(1, buckets[idx] ?? 0));
  const vocalPresence = (b(0) + b(1) + b(2) + b(3)) * 0.25;
  const targets = new Array<number>(WAVE_BARS);
  for (let i = 0; i < WAVE_BARS; i++) {
    const dist = Math.abs(i - WAVE_CENTER_INDEX);
    const isLeft = i < WAVE_CENTER_INDEX;
    let band = 0;
    switch (dist) {
      case 0:
        band = b(0) * 0.5 + b(1) * 0.35 + b(2) * 0.15;
        break;
      case 1:
        band = isLeft
          ? b(1) * 0.55 + b(3) * 0.45
          : b(2) * 0.55 + b(3) * 0.45;
        break;
      case 2:
        band = isLeft
          ? b(3) * 0.55 + b(5) * 0.45
          : b(4) * 0.55 + b(6) * 0.45;
        break;
      case 3:
        band = isLeft
          ? b(5) * 0.55 + b(8) * 0.45
          : b(6) * 0.55 + b(9) * 0.45;
        break;
      case 4:
        band = isLeft
          ? b(7) * 0.55 + b(11) * 0.45
          : b(8) * 0.55 + b(12) * 0.45;
        break;
      default:
        band = isLeft
          ? b(10) * 0.55 + b(14) * 0.45
          : b(11) * 0.55 + b(15) * 0.45;
        break;
    }
    targets[i] = Math.min(
      1,
      (band * 0.58 + vocalPresence * 0.42) * WAVE_CENTER_WEIGHTS[i],
    );
  }
  return targets;
}

function computeWaveBarHeightPx(
  level: number,
  index: number,
  breathPhaseRad: number,
  withBreath: boolean,
): number {
  const clamped = Math.max(0, Math.min(1, level));
  const breathAttenuation = 1 - Math.min(1, clamped * 2.5);
  const breathPx = withBreath
    ? Math.sin(breathPhaseRad - Math.abs(index - WAVE_CENTER_INDEX) * 0.42) *
      0.6 *
      breathAttenuation
    : 0;
  const restingFloor = withBreath ? 3.6 : 3;
  const activeSpan = withBreath ? 14.4 : 15;
  return Math.max(
    3,
    Math.min(
      18,
      restingFloor + breathPx + Math.pow(clamped, 0.68) * activeSpan,
    ),
  );
}

// Only call out a model load in the Live preview once it has run this long.
// Warm loads finish in well under this (~0.2s on Apple Silicon, ~1.5s on a
// Windows CPU backend), so the common case never flashes a loading notice while
// the user is speaking; cold loads can take 40s+.
const SLOW_MODEL_LOAD_MS = 2000;

// Wait briefly before revealing the Paste raw escape hatch so fast post-process
// runs finish cleanly without flashing a button, while slow LLM runs never trap
// the user's transcript behind a spinner.
const PASTE_RAW_DELAY_MS = 450;

const RecordingOverlay: React.FC = () => {
  const { t, i18n: activeI18n } = useTranslation();
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
  // `Stream::play()` returning does not mean hardware callbacks are flowing.
  // Stay visually in an arming state until the backend processes the first
  // actual microphone sample chunk.
  const [captureReady, setCaptureReady] = useState(false);
  // Recording starts while the model loads in the background. Say so, otherwise
  // a cold start looks like an empty Live preview that ignores speech, then a
  // hung "Transcribing..." spinner. Once recording stops, any in-flight load is
  // the wait, so the working label reflects it immediately; the Live notice
  // waits for the load to be slow so fast loads don't flash it mid-speech.
  const [modelLoading, setModelLoading] = useState(false);
  const [modelLoadSlow, setModelLoadSlow] = useState(false);
  const modelLoadTimerRef = useRef<ReturnType<typeof setTimeout>>();
  // Latched per Live session: once the loading notice has opened the panel, keep
  // it open after the load so it doesn't collapse and reopen when text arrives.
  const [loadNoticeShown, setLoadNoticeShown] = useState(false);
  const [levels, setLevels] = useState<number[]>(Array(WAVE_BARS).fill(0));
  const [streamText, setStreamText] = useState<StreamTextEvent>({
    committed: "",
    tentative: "",
  });
  const [phase, setPhase] = useState<StreamPhase>("listening");
  const [workKind, setWorkKind] = useState<StreamWorkKind>("transcribing");
  const [showPasteRaw, setShowPasteRaw] = useState(false);
  const [pastingRaw, setPastingRaw] = useState(false);
  const [noticeKind, setNoticeKind] = useState<OverlayNoticeKind>(
    "transcription_failed",
  );
  const [detectedCall, setDetectedCall] =
    useState<DetectedCallPayload | null>(null);
  const [isPaused, setIsPaused] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const pausePendingRef = useRef(false);
  const finishPendingRef = useRef(false);
  // Bumped on each new streaming session so the Live card remounts fresh (replays
  // the pop-in, and never animates in from the previous panel's open size).
  const [session, setSession] = useState(0);
  // Overlay placement (top vs bottom of the screen). The Live panel grows downward
  // from a top overlay (oldest line under the pill) and upward from a bottom one.
  const [position, setPosition] = useState<"top" | "bottom">("bottom");
  // True once live text overflows the cap. A top overlay fades its top edge only
  // while overflowing, so the resting first line stays crisp flush under the pill.
  const [overflowing, setOverflowing] = useState(false);

  // Wave 4A: SessionBus state & SV-82 / SV-86 pill state
  const [softCancelInfo, setSoftCancelInfo] =
    useState<SoftCancelState | null>(null);
  const [softCancelProgress, setSoftCancelProgress] = useState(1);
  const [undoPending, setUndoPending] = useState(false);
  const [meetingOverlay, setMeetingOverlay] =
    useState<MeetingActiveOverlay | null>(null);
  const [meetingSoftStoppedOverlay, setMeetingSoftStoppedOverlay] =
    useState<MeetingSoftStoppedOverlay | null>(null);
  const [meetingSoftStopProgress, setMeetingSoftStopProgress] = useState(1);
  const [summarizingOverlay, setSummarizingOverlay] =
    useState<MeetingSummarizingOverlay | null>(null);
  const [meetingDisplayElapsedMs, setMeetingDisplayElapsedMs] = useState(0);
  const [meetingTogglePending, setMeetingTogglePending] = useState(false);
  const [meetingStopPending, setMeetingStopPending] = useState(false);

  const latestSeqRef = useRef(0);
  const latestSnapshotRef = useRef<SessionBusSnapshot | null>(null);
  const clockSkewRef = useRef(0);
  const stateRef = useRef<OverlayState>("recording");
  const isVisibleRef = useRef(false);
  const activeTakeIdRef = useRef<number | null>(null);
  const softCancelledTakeIdRef = useRef<number | null>(null);
  const preSoftCancelStreamingRef = useRef(false);
  const undoPendingRef = useRef(false);
  const confirmPendingRef = useRef(false);
  const autoExpiredTakeRef = useRef<number | null>(null);
  const meetingTogglePendingRef = useRef(false);
  const meetingStopPendingRef = useRef(false);
  const localMeetingDismissedIdRef = useRef<string | null>(null);

  stateRef.current = state;
  isVisibleRef.current = isVisible;

  const targetLevelsRef = useRef<number[]>(Array(WAVE_BARS).fill(0));
  const currentLevelsRef = useRef<number[]>(Array(WAVE_BARS).fill(0));
  const breathPhaseRef = useRef(0);
  const waveBarRefs = useRef<(HTMLElement | null)[]>([]);

  const resetWaveformLevels = () => {
    const zero = Array(WAVE_BARS).fill(0);
    targetLevelsRef.current = [...zero];
    currentLevelsRef.current = [...zero];
    breathPhaseRef.current = 0;
    setLevels(zero);
    for (const el of waveBarRefs.current) {
      if (el) el.style.height = "3px";
    }
  };
  // Live-text scroll-back: the text region "sticks" to the newest line while the
  // user is at the bottom; if they scroll up to read history, auto-follow pauses
  // until they scroll back down.
  const capRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);
  const direction = getLanguageDirection(activeI18n.language);

  useEffect(() => {
    let disposed = false;
    let teardown: (() => void) | undefined;

    const refreshPlacementAndLanguage = async () => {
      await syncLanguageFromSettings();
      try {
        const settings = await commands.getAppSettings();
        if (!disposed && settings.status === "ok") {
          setPosition(
            settings.data.overlay_position === "top" ? "top" : "bottom",
          );
        }
      } catch {
        // Keep the previous/default placement if settings can't be read.
      }
    };

    const applySessionBusSnapshot = (
      snapshot: SessionBusSnapshot,
      isInitialHydration = false,
    ) => {
      if (snapshot.seq < latestSeqRef.current) {
        return;
      }
      if (
        isInitialHydration &&
        snapshot.seq === 0 &&
        snapshot.overlay.mode === "hidden"
      ) {
        latestSnapshotRef.current = snapshot;
        return;
      }

      latestSeqRef.current = snapshot.seq;
      latestSnapshotRef.current = snapshot;
      clockSkewRef.current = snapshot.server_now_ms - Date.now();

      if (
        snapshot.dictation.phase === "idle" &&
        snapshot.transient_notice === null &&
        (snapshot.meeting.phase === "recording" ||
          snapshot.meeting.phase === "paused") &&
        snapshot.overlay.mode === "hidden"
      ) {
        localMeetingDismissedIdRef.current = snapshot.meeting.meeting_id;
      } else if (snapshot.overlay.mode === "meeting_active") {
        localMeetingDismissedIdRef.current = null;
      }

      const overlay = snapshot.overlay;
      if (overlay.mode !== "hidden" && !isVisibleRef.current) {
        void refreshPlacementAndLanguage();
      }

      switch (overlay.mode) {
        case "hidden": {
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setMeetingSoftStoppedOverlay(null);
          setSummarizingOverlay(null);
          setCaptureReady(false);
          setIsPaused(false);
          pausePendingRef.current = false;
          finishPendingRef.current = false;
          preSoftCancelStreamingRef.current = false;
          meetingTogglePendingRef.current = false;
          meetingStopPendingRef.current = false;
          setMeetingTogglePending(false);
          setMeetingStopPending(false);
          setIsVisible(false);
          break;
        }
        case "dictation_recording": {
          const wasSoftCancelledSameTake =
            (stateRef.current === "soft_cancelled" ||
              softCancelledTakeIdRef.current === overlay.take_id) &&
            activeTakeIdRef.current === overlay.take_id;
          const isNewTake = activeTakeIdRef.current !== overlay.take_id;
          activeTakeIdRef.current = overlay.take_id;

          setSoftCancelInfo(null);
          setSummarizingOverlay(null);
          pausePendingRef.current = false;
          finishPendingRef.current = false;
          setIsPaused(overlay.paused);

          if (wasSoftCancelledSameTake) {
            setCaptureReady(true);
            if (preSoftCancelStreamingRef.current) {
              setPhase("listening");
              setState("streaming");
              stateRef.current = "streaming";
            } else {
              setState("recording");
              stateRef.current = "recording";
            }
          } else if (isNewTake) {
            softCancelledTakeIdRef.current = null;
            preSoftCancelStreamingRef.current = false;
            if (stateRef.current !== "streaming") {
              setElapsed(0);
              resetWaveformLevels();
              setCaptureReady(overlay.locked || overlay.paused);
              setState("recording");
              stateRef.current = "recording";
            } else {
              setPhase("listening");
              if (overlay.locked || overlay.paused) {
                setCaptureReady(true);
              }
            }
          } else if (overlay.locked || overlay.paused) {
            setCaptureReady(true);
          }

          setIsVisible(true);
          break;
        }
        case "dictation_soft_cancelled": {
          if (stateRef.current === "streaming") {
            preSoftCancelStreamingRef.current = true;
          }
          activeTakeIdRef.current = overlay.take_id;
          softCancelledTakeIdRef.current = overlay.take_id;
          undoPendingRef.current = false;
          confirmPendingRef.current = false;
          setUndoPending(false);
          setSoftCancelInfo({
            takeId: overlay.take_id,
            cancelledAtMs: overlay.cancelled_at_ms,
            expiresAtMs: overlay.expires_at_ms,
            graceMs: overlay.grace_ms,
          });
          setSoftCancelProgress(
            computeSoftCancelProgress(
              overlay.cancelled_at_ms,
              overlay.expires_at_ms,
              overlay.grace_ms,
              snapshot.server_now_ms,
            ),
          );
          setState("soft_cancelled");
          stateRef.current = "soft_cancelled";
          setIsVisible(true);
          break;
        }
        case "dictation_transcribing": {
          activeTakeIdRef.current = overlay.take_id;
          softCancelledTakeIdRef.current = null;
          setSoftCancelInfo(null);
          setIsPaused(false);
          if (stateRef.current === "streaming") {
            setPhase("working");
            setWorkKind("transcribing");
          } else {
            setState("transcribing");
            stateRef.current = "transcribing";
          }
          setIsVisible(true);
          break;
        }
        case "dictation_post_processing": {
          activeTakeIdRef.current = overlay.take_id;
          setSoftCancelInfo(null);
          setIsPaused(false);
          if (stateRef.current === "streaming") {
            setPhase("working");
            setWorkKind("polishing");
          } else {
            setState("processing");
            stateRef.current = "processing";
          }
          setIsVisible(true);
          break;
        }
        case "meeting_active": {
          preSoftCancelStreamingRef.current = false;
          setSoftCancelInfo(null);
          setMeetingSoftStoppedOverlay(null);
          setSummarizingOverlay(null);
          meetingTogglePendingRef.current = false;
          meetingStopPendingRef.current = false;
          setMeetingTogglePending(false);
          setMeetingStopPending(false);
          setMeetingOverlay(overlay);
          setState("meeting_active");
          stateRef.current = "meeting_active";
          setIsVisible(true);
          break;
        }
        case "meeting_soft_stopped": {
          preSoftCancelStreamingRef.current = false;
          localMeetingDismissedIdRef.current = null;
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setSummarizingOverlay(null);
          meetingTogglePendingRef.current = false;
          meetingStopPendingRef.current = false;
          setMeetingTogglePending(false);
          setMeetingStopPending(false);
          setMeetingSoftStoppedOverlay(overlay);
          setMeetingSoftStopProgress(
            computeSoftCancelProgress(
              overlay.stopped_at_ms,
              overlay.expires_at_ms,
              overlay.grace_ms,
              snapshot.server_now_ms,
            ),
          );
          setState("meeting_soft_stopped");
          stateRef.current = "meeting_soft_stopped";
          setIsVisible(true);
          break;
        }
        case "meeting_summarizing": {
          preSoftCancelStreamingRef.current = false;
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setMeetingSoftStoppedOverlay(null);
          setSummarizingOverlay(overlay);
          setState("meeting_summarizing");
          stateRef.current = "meeting_summarizing";
          setIsVisible(true);
          break;
        }
        case "call_detected": {
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setSummarizingOverlay(null);
          setDetectedCall((prev) => ({
            call_id: overlay.call_id,
            app_name: overlay.app_name,
            app_bundle_id:
              prev?.call_id === overlay.call_id
                ? prev.app_bundle_id
                : undefined,
            bundle_id:
              prev?.call_id === overlay.call_id ? prev.bundle_id : undefined,
          }));
          setNoticeKind("call_detected");
          setState("notice");
          stateRef.current = "notice";
          setIsVisible(true);
          break;
        }
        case "transient_notice": {
          setSoftCancelInfo(null);
          setNoticeKind(overlay.notice_kind as OverlayNoticeKind);
          setState("notice");
          stateRef.current = "notice";
          setIsVisible(true);
          break;
        }
      }
    };

    const setupEventListeners = async () => {
      const unlistenSessionBus = await listen<SessionBusSnapshot>(
        SESSION_BUS_EVENT,
        (event) => {
          if (event.payload && typeof event.payload === "object") {
            applySessionBusSnapshot(event.payload);
          }
        },
      );

      const unlistenShow = await listen("show-overlay", async (event) => {
        const overlayState = event.payload as OverlayState;
        const seqAtShow = latestSeqRef.current;
        const isResumingSoftCancelledSameTake =
          (overlayState === "recording" || overlayState === "streaming") &&
          softCancelledTakeIdRef.current !== null &&
          softCancelledTakeIdRef.current === activeTakeIdRef.current;
        const resolvedState: OverlayState =
          isResumingSoftCancelledSameTake && preSoftCancelStreamingRef.current
            ? "streaming"
            : overlayState;

        // Reset synchronously before settings I/O. A fast microphone can emit
        // recording-ready while the awaits below are in flight; resetting after
        // them would overwrite that event and leave the overlay stuck arming.
        // Exception: undoing a soft-cancelled take preserves pre-cancel elapsed
        // speech duration and live preview text instead of resetting to 00:00.
        setIsPaused(false);
        pausePendingRef.current = false;
        finishPendingRef.current = false;
        if (resolvedState === "recording" || resolvedState === "streaming") {
          if (isResumingSoftCancelledSameTake) {
            setCaptureReady(true);
          } else {
            setCaptureReady(false);
            setElapsed(0);
            resetWaveformLevels();
            setStreamText({ committed: "", tentative: "" });
          }
        }
        if (resolvedState === "soft_cancelled") {
          if (activeTakeIdRef.current !== null) {
            softCancelledTakeIdRef.current = activeTakeIdRef.current;
          }
          setSoftCancelInfo((prev) => {
            if (prev) return prev;
            const nowMs = Date.now() + clockSkewRef.current;
            return {
              takeId: activeTakeIdRef.current,
              cancelledAtMs: nowMs,
              expiresAtMs: nowMs + DEFAULT_SOFT_CANCEL_GRACE_MS,
              graceMs: DEFAULT_SOFT_CANCEL_GRACE_MS,
            };
          });
          setSoftCancelProgress(1);
        }
        if (resolvedState === "meeting_soft_stopped") {
          setMeetingSoftStoppedOverlay((prev) => {
            if (prev) return prev;
            const nowMs = Date.now() + clockSkewRef.current;
            return {
              mode: "meeting_soft_stopped",
              meeting_id: "active",
              title: "",
              elapsed_ms: 0,
              stopped_at_ms: nowMs,
              expires_at_ms: nowMs + DEFAULT_MEETING_AUTO_STOP_GRACE_MS,
              grace_ms: DEFAULT_MEETING_AUTO_STOP_GRACE_MS,
              app_name: null,
            };
          });
          setMeetingSoftStopProgress(1);
        }
        if (resolvedState === "meeting_active") {
          setMeetingOverlay((prev) => {
            if (prev) return prev;
            const nowMs = Date.now() + clockSkewRef.current;
            return {
              mode: "meeting_active",
              meeting_id: "active",
              title: "",
              paused: false,
              elapsed_ms: 0,
              started_at_ms: nowMs,
              accumulated_paused_ms: 0,
              paused_at_ms: null,
              app_name: null,
            };
          });
        }

        // Apply state synchronously so rapid follow-up SessionBus transitions
        // are never overwritten after settings I/O completes.
        if (
          seqAtShow === 0 ||
          resolvedState === "streaming" ||
          latestSnapshotRef.current?.overlay.mode !== "dictation_soft_cancelled"
        ) {
          setState(resolvedState);
          stateRef.current = resolvedState;
        }
        if (resolvedState === "streaming") {
          setPhase("listening");
          setWorkKind("transcribing");
          if (!isResumingSoftCancelledSameTake) {
            setElapsed(0);
            setLoadNoticeShown(false);
            setSession((s) => s + 1); // remount the card fresh for this session
          }
        }
        setIsVisible(true);

        await refreshPlacementAndLanguage();
      });

      const unlistenHide = await listen("hide-overlay", () => {
        setCaptureReady(false);
        setIsPaused(false);
        pausePendingRef.current = false;
        finishPendingRef.current = false;
        preSoftCancelStreamingRef.current = false;
        softCancelledTakeIdRef.current = null;

        const snap = latestSnapshotRef.current;
        if (snap?.overlay.mode === "meeting_active") {
          setSoftCancelInfo(null);
          setMeetingSoftStoppedOverlay(null);
          setSummarizingOverlay(null);
          setMeetingOverlay(snap.overlay);
          setState("meeting_active");
          stateRef.current = "meeting_active";
          setIsVisible(true);
          return;
        }
        if (snap?.overlay.mode === "meeting_soft_stopped") {
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setSummarizingOverlay(null);
          setMeetingSoftStoppedOverlay(snap.overlay);
          setState("meeting_soft_stopped");
          stateRef.current = "meeting_soft_stopped";
          setIsVisible(true);
          return;
        }
        if (snap?.overlay.mode === "meeting_summarizing") {
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setMeetingSoftStoppedOverlay(null);
          setSummarizingOverlay(snap.overlay);
          setState("meeting_summarizing");
          stateRef.current = "meeting_summarizing";
          setIsVisible(true);
          return;
        }
        if (snap && snap.meeting.phase !== "idle") {
          const meetingId = snap.meeting.meeting_id;
          if (
            snap.meeting.phase === "summarizing" ||
            snap.meeting.phase === "soft_stopped" ||
            localMeetingDismissedIdRef.current !== meetingId
          ) {
            const projected = projectMeetingOverlay(
              snap.meeting,
              Date.now() + clockSkewRef.current,
            );
            if (projected?.mode === "meeting_active") {
              setSoftCancelInfo(null);
              setMeetingSoftStoppedOverlay(null);
              setSummarizingOverlay(null);
              setMeetingOverlay(projected);
              setState("meeting_active");
              stateRef.current = "meeting_active";
              setIsVisible(true);
              return;
            }
            if (projected?.mode === "meeting_soft_stopped") {
              setSoftCancelInfo(null);
              setMeetingOverlay(null);
              setSummarizingOverlay(null);
              setMeetingSoftStoppedOverlay(projected);
              setState("meeting_soft_stopped");
              stateRef.current = "meeting_soft_stopped";
              setIsVisible(true);
              return;
            }
            if (projected?.mode === "meeting_summarizing") {
              setSoftCancelInfo(null);
              setMeetingOverlay(null);
              setMeetingSoftStoppedOverlay(null);
              setSummarizingOverlay(projected);
              setState("meeting_summarizing");
              stateRef.current = "meeting_summarizing";
              setIsVisible(true);
              return;
            }
          }
        }

        setSoftCancelInfo(null);
        setIsVisible(false);
      });

      const unlistenMeetingLegacy = await listen<MeetingLiveState>(
        "meeting-state-changed",
        (event) => {
          const payload = event.payload;
          if (!payload || typeof payload !== "object") return;
          if (
            payload.status === "idle" &&
            latestSeqRef.current === 0 &&
            (stateRef.current === "meeting_active" ||
              stateRef.current === "meeting_summarizing")
          ) {
            setMeetingOverlay(null);
            setSummarizingOverlay(null);
            setIsVisible(false);
          }
        },
      );

      const unlistenReady = await listen("recording-ready", () => {
        const isResumingSoftCancelledSameTake =
          softCancelledTakeIdRef.current !== null &&
          softCancelledTakeIdRef.current === activeTakeIdRef.current;
        if (!isResumingSoftCancelledSameTake) {
          setElapsed(0);
        }
        setIsPaused(false);
        setCaptureReady(true);
      });

      const unlistenPaused = await listen("recording-paused", () => {
        targetLevelsRef.current = Array(WAVE_BARS).fill(0);
        currentLevelsRef.current = Array(WAVE_BARS).fill(0);
        setIsPaused(true);
      });

      const unlistenResumed = await listen("recording-resumed", () => {
        setIsPaused(false);
      });

      const unlistenLevel = await listen<number[]>("mic-level", (event) => {
        const nextTargets = mapBucketsToSymmetricalTargets(
          Array.isArray(event.payload) ? event.payload : [],
        );
        targetLevelsRef.current = nextTargets;
        if (prefersReducedMotion()) {
          currentLevelsRef.current = [...nextTargets];
          setLevels(nextTargets);
        }
      });

      const unlistenStream = await events.streamTextEvent.listen((event) => {
        setStreamText(event.payload);
      });

      const unlistenPhase = await events.streamPhaseEvent.listen((event) => {
        const payload: StreamPhaseEvent = event.payload;
        setPhase(payload.phase);
        if (payload.phase === "working") {
          setIsPaused(false);
        }
        if (payload.kind) setWorkKind(payload.kind);
      });

      const unlistenNotice = await listen<string>("overlay-notice", (event) => {
        setNoticeKind(event.payload as OverlayNoticeKind);
      });

      const unlistenCallDetected = await listen<DetectedCallPayload | null>(
        "call-detected",
        (event) => {
          setDetectedCall(event.payload ?? null);
        },
      );

      // The backend ends every `loading_started` with exactly one of completed
      // or failed, so only those end a load. Other events (e.g. `unloaded` from
      // the idle watcher) aren't ordered against an in-flight load and must not
      // clear it.
      const unlistenModel = await listen<ModelStateEvent>(
        "model-state-changed",
        (event) => {
          const type = event.payload.event_type;
          if (type === "loading_started") {
            clearTimeout(modelLoadTimerRef.current);
            setModelLoading(true);
            setModelLoadSlow(false);
            modelLoadTimerRef.current = setTimeout(
              () => setModelLoadSlow(true),
              SLOW_MODEL_LOAD_MS,
            );
          } else if (
            type === "loading_completed" ||
            type === "loading_failed"
          ) {
            clearTimeout(modelLoadTimerRef.current);
            setModelLoading(false);
            setModelLoadSlow(false);
          }
        },
      );

      const initialSnapshot = await fetchSessionBusSnapshot();
      if (!disposed && initialSnapshot) {
        applySessionBusSnapshot(initialSnapshot, true);
      }

      return () => {
        unlistenSessionBus();
        unlistenShow();
        unlistenHide();
        unlistenMeetingLegacy();
        unlistenReady();
        unlistenPaused();
        unlistenResumed();
        unlistenLevel();
        unlistenStream();
        unlistenPhase();
        unlistenNotice();
        unlistenCallDetected();
        unlistenModel();
        clearTimeout(modelLoadTimerRef.current);
      };
    };

    void setupEventListeners().then((cleanup) => {
      if (disposed) {
        cleanup();
      } else {
        teardown = cleanup;
      }
    });

    return () => {
      disposed = true;
      teardown?.();
    };
  }, []);

  const isListeningPhase =
    state === "recording" || (state === "streaming" && phase === "listening");

  // Smooth 60fps requestAnimationFrame spring/lerp loop + subtle ±0.6px organic
  // breath motion while visible & listening (falls back to direct updates under
  // prefers-reduced-motion: reduce).
  useEffect(() => {
    if (!isVisible || !isListeningPhase || isPaused) return;
    if (prefersReducedMotion()) {
      const snap = [...targetLevelsRef.current];
      currentLevelsRef.current = snap;
      setLevels(snap);
      return;
    }

    let rafId = 0;
    let lastTs = performance.now();

    const step = (now: number) => {
      const dt = Math.min(0.05, Math.max(0.001, (now - lastTs) / 1000));
      lastTs = now;
      if (captureReady) {
        breathPhaseRef.current =
          (breathPhaseRef.current + dt * 3.1) % (Math.PI * 200);
      }

      const targets = targetLevelsRef.current;
      const currents = currentLevelsRef.current;
      const bars = waveBarRefs.current;
      const withBreath = captureReady && !isPaused;

      for (let i = 0; i < WAVE_BARS; i++) {
        const target = targets[i] ?? 0;
        const prev = currents[i] ?? 0;
        const rate = target > prev ? 28 : 11;
        const alpha = 1 - Math.exp(-rate * dt);
        const next = prev + (target - prev) * alpha;
        currents[i] = next;

        const barEl = bars[i];
        if (barEl) {
          const h = computeWaveBarHeightPx(
            next,
            i,
            breathPhaseRef.current,
            withBreath,
          );
          barEl.style.height = `${h.toFixed(2)}px`;
        }
      }

      rafId = window.requestAnimationFrame(step);
    };

    rafId = window.requestAnimationFrame(step);
    return () => {
      if (rafId) window.cancelAnimationFrame(rafId);
    };
  }, [isVisible, isListeningPhase, isPaused, captureReady]);

  // Elapsed capture timer ticks only while samples are flowing and not paused.
  useEffect(() => {
    if (!isListeningPhase || !isVisible || !captureReady || isPaused) return;
    const id = setInterval(() => setElapsed((e) => e + 1), 1000);
    return () => clearInterval(id);
  }, [isListeningPhase, isVisible, captureReady, isPaused]);

  // Stick to the bottom as text streams in — but only while pinned, so a user who
  // has scrolled up to read history isn't yanked back down by the next chunk.
  useLayoutEffect(() => {
    const el = capRef.current;
    if (!el) return;
    // Fade the top edge only once text actually overflows the cap.
    setOverflowing(el.scrollHeight > el.clientHeight + 1);
    if (pinnedRef.current) el.scrollTop = el.scrollHeight;
  }, [streamText]);

  // `session` re-runs this when a new Live session starts mid-load (e.g. a
  // retry during a cold load), since the reset doesn't change the other deps.
  useEffect(() => {
    if (modelLoadSlow && state === "streaming") setLoadNoticeShown(true);
  }, [modelLoadSlow, state, session]);

  // Each fresh streaming session starts pinned to the bottom, fade cleared.
  useEffect(() => {
    pinnedRef.current = true;
    setOverflowing(false);
  }, [session]);

  const isPolishing =
    !modelLoading &&
    (state === "processing" ||
      (state === "streaming" &&
        phase === "working" &&
        workKind === "polishing"));

  useEffect(() => {
    if (!isVisible || !isPolishing) {
      setShowPasteRaw(false);
      setPastingRaw(false);
      return;
    }
    setShowPasteRaw(false);
    setPastingRaw(false);
    const timer = setTimeout(() => {
      setShowPasteRaw(true);
    }, PASTE_RAW_DELAY_MS);
    return () => clearTimeout(timer);
  }, [isVisible, isPolishing, session]);

  const restoreUnderlyingMeetingOrHide = () => {
    const snap = latestSnapshotRef.current;
    if (snap && snap.meeting.phase !== "idle") {
      const meetingId = snap.meeting.meeting_id;
      if (
        snap.meeting.phase === "summarizing" ||
        snap.meeting.phase === "soft_stopped" ||
        localMeetingDismissedIdRef.current !== meetingId
      ) {
        const projected = projectMeetingOverlay(
          snap.meeting,
          Date.now() + clockSkewRef.current,
        );
        if (projected?.mode === "meeting_active") {
          setSoftCancelInfo(null);
          setMeetingSoftStoppedOverlay(null);
          setSummarizingOverlay(null);
          setMeetingOverlay(projected);
          setState("meeting_active");
          stateRef.current = "meeting_active";
          setIsVisible(true);
          return;
        }
        if (projected?.mode === "meeting_soft_stopped") {
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setSummarizingOverlay(null);
          setMeetingSoftStoppedOverlay(projected);
          setState("meeting_soft_stopped");
          stateRef.current = "meeting_soft_stopped";
          setIsVisible(true);
          return;
        }
        if (projected?.mode === "meeting_summarizing") {
          setSoftCancelInfo(null);
          setMeetingOverlay(null);
          setMeetingSoftStoppedOverlay(null);
          setSummarizingOverlay(projected);
          setState("meeting_summarizing");
          stateRef.current = "meeting_summarizing";
          setIsVisible(true);
          return;
        }
      }
    }
    setSoftCancelInfo(null);
    setIsVisible(false);
  };

  // SV-82: Smooth 100% -> 0% soft-cancel countdown bar synced to backend deadline.
  useEffect(() => {
    if (!isVisible || state !== "soft_cancelled" || !softCancelInfo) return;

    const computeNow = () =>
      computeSoftCancelProgress(
        softCancelInfo.cancelledAtMs,
        softCancelInfo.expiresAtMs,
        softCancelInfo.graceMs,
        Date.now() + clockSkewRef.current,
      );

    setSoftCancelProgress(computeNow());

    const reducedMotion =
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    let rafId = 0;
    let intervalId: ReturnType<typeof setInterval> | undefined;

    const tick = () => {
      const next = computeNow();
      setSoftCancelProgress(next);
      if (
        next <= 0 &&
        autoExpiredTakeRef.current !== (softCancelInfo.takeId ?? -1) &&
        !undoPendingRef.current &&
        !confirmPendingRef.current
      ) {
        autoExpiredTakeRef.current = softCancelInfo.takeId ?? -1;
        const seqBefore = latestSeqRef.current;
        void confirmCancelRecording(softCancelInfo.takeId)
          .catch(() => false)
          .then(() => {
            if (
              latestSeqRef.current === seqBefore &&
              stateRef.current === "soft_cancelled"
            ) {
              restoreUnderlyingMeetingOrHide();
            }
          });
        return;
      }
      if (!reducedMotion && next > 0) {
        rafId = window.requestAnimationFrame(tick);
      }
    };

    if (reducedMotion) {
      intervalId = setInterval(tick, 250);
    } else {
      rafId = window.requestAnimationFrame(tick);
    }

    return () => {
      if (rafId) window.cancelAnimationFrame(rafId);
      if (intervalId) clearInterval(intervalId);
    };
  }, [isVisible, state, softCancelInfo]);

  // Smooth 100% -> 0% countdown bar for auto-stopped meetings after call hangup.
  useEffect(() => {
    if (
      !isVisible ||
      state !== "meeting_soft_stopped" ||
      !meetingSoftStoppedOverlay
    ) {
      return;
    }

    const computeNow = () =>
      computeSoftCancelProgress(
        meetingSoftStoppedOverlay.stopped_at_ms,
        meetingSoftStoppedOverlay.expires_at_ms,
        meetingSoftStoppedOverlay.grace_ms,
        Date.now() + clockSkewRef.current,
      );

    setMeetingSoftStopProgress(computeNow());

    const reducedMotion = prefersReducedMotion();
    let rafId = 0;
    let intervalId: ReturnType<typeof setInterval> | undefined;

    const tick = () => {
      const next = computeNow();
      setMeetingSoftStopProgress(next);
      if (!reducedMotion && next > 0) {
        rafId = window.requestAnimationFrame(tick);
      }
    };

    if (reducedMotion) {
      intervalId = setInterval(tick, 250);
    } else {
      rafId = window.requestAnimationFrame(tick);
    }

    return () => {
      if (rafId) window.cancelAnimationFrame(rafId);
      if (intervalId) clearInterval(intervalId);
    };
  }, [isVisible, state, meetingSoftStoppedOverlay]);

  // SV-86: Drift-free live MM:SS meeting timer across pause/resume cycles.
  useEffect(() => {
    if (!isVisible || state !== "meeting_active" || !meetingOverlay) return;
    const syncElapsed = () => {
      setMeetingDisplayElapsedMs(
        computeLiveMeetingElapsedMs(
          meetingOverlay,
          Date.now(),
          clockSkewRef.current,
        ),
      );
    };
    syncElapsed();
    if (meetingOverlay.paused) return;
    const intervalId = setInterval(syncElapsed, 250);
    return () => clearInterval(intervalId);
  }, [isVisible, state, meetingOverlay]);

  if (!isVisible) return null;

  // Re-pin when the user is within ~a line of the bottom; unpin otherwise.
  const handleStreamScroll = () => {
    const el = capRef.current;
    if (!el) return;
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight <= 16;
  };

  const formatElapsed = (s: number) =>
    `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;

  const formatStageEllipsis = (text: string) => text.replace(/\.{3}$/, "…");
  const listeningLabel = t("overlay.listening", { defaultValue: "Listening" });
  const transcribingLabel = formatStageEllipsis(
    modelLoading
      ? t("overlay.loadingModel", { defaultValue: "Loading model…" })
      : t("overlay.transcribing", { defaultValue: "Transcribing…" }),
  );
  const rawPolishing = t("overlay.polishing", {
    defaultValue: t("overlay.processing", { defaultValue: "Polishing…" }),
  });
  const polishingLabel =
    rawPolishing === "Processing..." || rawPolishing === "Processing…"
      ? "Polishing…"
      : formatStageEllipsis(rawPolishing);

  const handleTogglePause = async () => {
    if (pausePendingRef.current) return;
    pausePendingRef.current = true;
    const nextPaused = !isPaused;
    setIsPaused(nextPaused);
    try {
      const ok = await invoke<boolean>(
        nextPaused ? "pause_recording" : "resume_recording",
      );
      if (!ok) {
        setIsPaused(!nextPaused);
      }
    } catch {
      setIsPaused(!nextPaused);
    } finally {
      pausePendingRef.current = false;
    }
  };

  const handleFinishRecording = async () => {
    if (finishPendingRef.current) return;
    finishPendingRef.current = true;
    try {
      await invoke<boolean>("finish_recording");
    } catch {
      // Backend may already be finalizing or idle.
    } finally {
      finishPendingRef.current = false;
    }
  };

  // ---- Shared building blocks (one visual language for every overlay form) ----
  const reducedMotionNow = prefersReducedMotion();
  const withBreathNow = captureReady && !isPaused && !reducedMotionNow;
  const waveform = (
    <div
      data-testid="overlay-waveform"
      className={`swave ${captureReady ? "ready" : "arming"} ${
        captureReady && isPaused ? "paused" : ""
      }`}
    >
      {levels.map((v, i) => {
        const liveVal = reducedMotionNow
          ? v
          : (currentLevelsRef.current[i] ?? v);
        const h = computeWaveBarHeightPx(
          liveVal,
          i,
          breathPhaseRef.current,
          withBreathNow,
        );
        return (
          <i
            key={i}
            ref={(el) => {
              waveBarRefs.current[i] = el;
            }}
            style={{
              height: `${h.toFixed(2)}px`,
            }}
          />
        );
      })}
    </div>
  );

  const handlePasteRaw = async () => {
    if (pastingRaw) return;
    setPastingRaw(true);
    try {
      const res = await commands.pasteRawNow();
      if (res.status === "error" || !res.data) {
        setPastingRaw(false);
      }
    } catch {
      setPastingRaw(false);
    }
  };

  const pasteRawBtn = (
    <button
      type="button"
      className="paste-raw-button"
      disabled={pastingRaw}
      title={t("overlay.pasteRawTitle")}
      onClick={handlePasteRaw}
    >
      {t("overlay.pasteRaw")}
    </button>
  );

  const pauseLabel = t("overlay.pause", { defaultValue: "Pause recording" });
  const resumeLabel = t("overlay.resume", { defaultValue: "Resume recording" });
  const doneLabel = t("overlay.done", { defaultValue: "Finish dictation" });
  const cancelLabel = t("common.cancel", { defaultValue: "Cancel" });
  const dismissLabel = t("overlay.dismiss", { defaultValue: "Dismiss" });

  const cancelBtn = (
    <button
      type="button"
      className="sx"
      aria-label={cancelLabel}
      onClick={() => commands.cancelOperation()}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path
          d="M4 4 L12 12 M12 4 L4 12"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
        />
      </svg>
    </button>
  );

  const pauseResumeBtn = (
    <button
      type="button"
      data-testid="overlay-pause-resume-btn"
      className={`sctrl-btn ${isPaused ? "is-resume" : "is-pause"}`}
      aria-label={isPaused ? resumeLabel : pauseLabel}
      title={isPaused ? resumeLabel : pauseLabel}
      onClick={handleTogglePause}
    >
      {isPaused ? (
        <>
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M5 3.5L12.5 8L5 12.5V3.5Z" fill="currentColor" />
          </svg>
          <span>{t("overlay.resumeShort", { defaultValue: "Resume" })}</span>
        </>
      ) : (
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <rect
            x="4.5"
            y="3.5"
            width="2.5"
            height="9"
            rx="1"
            fill="currentColor"
          />
          <rect
            x="9"
            y="3.5"
            width="2.5"
            height="9"
            rx="1"
            fill="currentColor"
          />
        </svg>
      )}
    </button>
  );

  const doneBtn = (
    <button
      type="button"
      data-testid="overlay-done-btn"
      className="sctrl-btn is-done"
      aria-label={doneLabel}
      title={doneLabel}
      onClick={handleFinishRecording}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path
          d="M3.5 8.5L6.5 11.5L12.5 4.5"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    </button>
  );

  // dot (left) | waveform + stage label (center) | timer + pause/resume + done + cancel (right)
  const listeningRow = (showCancel: boolean, showStageLabel = false) => (
    <div className={`sbase ${captureReady ? "is-capturing" : ""}`}>
      <div className="sbase-l">
        <span className={`sdot ${captureReady ? "ready" : "arming"}`} />
      </div>
      <div className="sbase-mid">
        {waveform}
        {captureReady &&
          (isPaused ? (
            <span data-testid="overlay-stage-label" className="spaused-label">
              {t("overlay.paused", { defaultValue: "Paused" })}
            </span>
          ) : (
            showStageLabel && (
              <span data-testid="overlay-stage-label" className="sstage-label">
                {listeningLabel}
              </span>
            )
          ))}
      </div>
      <div className="sbase-r">
        {captureReady && (
          <>
            <span className="stimer">{formatElapsed(elapsed)}</span>
            {pauseResumeBtn}
            {doneBtn}
          </>
        )}
        {showCancel && cancelBtn}
      </div>
    </div>
  );

  // spinner (left) | label (center) | paste-raw + cancel (right) — same 3-zone
  // grid as the listening row, widening smoothly when Paste raw appears.
  const workingRow = (
    label: string,
    showCancel: boolean,
    withPasteRaw = false,
  ) => (
    <div className={`sbase ${withPasteRaw ? "has-paste-raw" : ""}`}>
      <div className="sbase-l">
        <span className="sspinner" />
      </div>
      <span data-testid="overlay-stage-label" className="swork-label">
        {label}
      </span>
      <div className="sbase-r">
        {withPasteRaw && pasteRawBtn}
        {showCancel && cancelBtn}
      </div>
    </div>
  );

  // ---- Live overlay: a pill that sculpts open into a panel ----
  if (state === "streaming") {
    const hasText =
      streamText.committed.length > 0 || streamText.tentative.length > 0;
    const working = phase === "working";
    // While listening, a slow model load opens the panel with a notice where the
    // text would be, so speech that isn't transcribing yet doesn't look ignored.
    const loadingNotice = modelLoadSlow && !working && !hasText;
    // Keep the panel open whenever there's text — even while finalizing — so the
    // transcript stays put under a working spinner instead of collapsing and
    // squishing the text mid-stream. Only fall back to the small working pill
    // when there was no text to preserve.
    const open = hasText || loadingNotice || (loadNoticeShown && !working);
    const collapsed = working && !hasText;
    const capturingCard = !working && captureReady;
    const pausedCard = capturingCard && isPaused;

    return (
      <div dir={direction} className={`ov-stage ${position}`}>
        <div
          key={session}
          className={`scard ${open ? "open" : ""} ${
            capturingCard ? "is-capturing" : ""
          } ${pausedCard ? "cpaused" : ""} ${collapsed ? "working" : ""} ${
            showPasteRaw ? "has-paste-raw" : ""
          } ${isVisible ? "" : "leaving"}`}
        >
          <div className="stext">
            <div className="stext-clip">
              <div
                className={`stext-cap ${overflowing ? "overflowing" : ""}`}
                ref={capRef}
                onScroll={handleStreamScroll}
              >
                <p dir="auto">
                  {loadingNotice && (
                    <span className="sloading">
                      {t("overlay.loadingModel")}
                    </span>
                  )}
                  <span className="committed">
                    {streamText.committed
                      ? `\u2068${streamText.committed}\u2069 `
                      : ""}
                  </span>
                  <span className="tentative">
                    {streamText.tentative
                      ? `\u2068${streamText.tentative}\u2069`
                      : ""}
                  </span>
                  {/* Drop the blinking caret once finalizing — it's no longer
                      capturing, and a static spinner conveys the work. */}
                  {!working && !loadingNotice && <span className="scaret" />}
                </p>
              </div>
            </div>
          </div>
          {working
            ? workingRow(
                workKind === "polishing" ? polishingLabel : transcribingLabel,
                true,
                showPasteRaw,
              )
            : listeningRow(true, false)}
        </div>
      </div>
    );
  }

  // ---- Minimal overlay: exactly one row at a time — waveform (recording), or a
  // spinner + label (transcribing / processing), or a transient status notice.
  const isNotice = state === "notice";
  const isCallDetectedNotice = isNotice && noticeKind === "call_detected";
  const callAppKind = (() => {
    const id = (
      detectedCall?.bundle_id ??
      detectedCall?.app_bundle_id ??
      ""
    ).toLowerCase();
    const name = (detectedCall?.app_name ?? "").toLowerCase();
    if (id.includes("teams") || name.includes("teams")) return "teams";
    if (id.includes("zoom") || name.includes("zoom")) return "zoom";
    if (
      id.includes("chrome") ||
      id.includes("safari") ||
      id.includes("browser") ||
      id.includes("firefox") ||
      id.includes("edge")
    ) {
      return "meet";
    }
    return "generic";
  })();

  const noticeLabel = (() => {
    switch (noticeKind) {
      case "call_detected": {
        const base = t("overlay.notices.callDetected", {
          defaultValue: "Meeting detected",
        });
        const shortApp = detectedCall?.app_name
          ?.replace(/^Microsoft\s+/i, "")
          ?.replace(/\.us$/i, "")
          ?.trim();
        return shortApp ? `${base} · \u2068${shortApp}\u2069` : base;
      }
      case "restart_required":
        return t("overlay.noticeRestartRequired");
      case "mic_muted":
        return t("overlay.noticeMicMuted");
      case "no_speech":
        return t("overlay.noticeNoSpeech");
      case "mic_permission":
        return t("overlay.noticeMicPermission");
      case "no_mic":
        return t("overlay.noticeNoMic");
      case "copied_to_clipboard":
        return t("overlay.noticeCopiedToClipboard", {
          defaultValue: "Copied — press ⌘V to paste",
        });
      case "transcription_failed":
      default:
        return t("overlay.noticeTranscriptionFailed");
    }
  })();

  const handleDismissCallNotice = async () => {
    setIsVisible(false);
    try {
      const callId = detectedCall?.call_id ?? null;
      await invoke("dismiss_detected_call", { callId, call_id: callId });
    } catch {
      // Ignore if no call is pending.
    }
  };

  const handleStartCallNotes = async () => {
    // In Settings preview mode (`detectedCall === null`), dismiss cleanly instead
    // of starting an unexpected real background meeting session.
    if (!detectedCall) {
      await handleDismissCallNotice();
      return;
    }
    const seqBefore = latestSeqRef.current;
    const nowServerMs = Date.now() + clockSkewRef.current;
    const optimisticOverlay: MeetingActiveOverlay = {
      mode: "meeting_active",
      meeting_id: `pending-${detectedCall.call_id}`,
      title: detectedCall.app_name,
      paused: false,
      elapsed_ms: 0,
      started_at_ms: nowServerMs,
      accumulated_paused_ms: 0,
      paused_at_ms: null,
      app_name: detectedCall.app_name,
    };
    localMeetingDismissedIdRef.current = null;
    setSummarizingOverlay(null);
    setMeetingOverlay(optimisticOverlay);
    setState("meeting_active");
    stateRef.current = "meeting_active";
    setIsVisible(true);

    try {
      const live = await invoke<MeetingLiveState | null>(
        "start_meeting_notes",
        {
          title: null,
          template: null,
          openWorkspace: false,
          open_workspace: false,
        },
      );
      if (live && latestSeqRef.current === seqBefore) {
        const syncedNow = Date.now() + clockSkewRef.current;
        const elapsedMs = Math.max(0, live.elapsed_ms ?? 0);
        const isPausedNext = live.status === "paused";
        setMeetingOverlay({
          mode: "meeting_active",
          meeting_id: live.active_meeting_id ?? optimisticOverlay.meeting_id,
          title: live.title || optimisticOverlay.title,
          paused: isPausedNext,
          elapsed_ms: elapsedMs,
          started_at_ms: syncedNow - elapsedMs,
          accumulated_paused_ms: 0,
          paused_at_ms: isPausedNext ? syncedNow : null,
          app_name: live.app_name ?? detectedCall.app_name,
        });
        setState("meeting_active");
        stateRef.current = "meeting_active";
        setIsVisible(true);
      }
    } catch {
      if (latestSeqRef.current === seqBefore) {
        setMeetingOverlay(null);
        setState("notice");
        stateRef.current = "notice";
      }
    }
  };

  const handleUndoSoftCancel = async () => {
    if (undoPendingRef.current || confirmPendingRef.current) return;
    undoPendingRef.current = true;
    setUndoPending(true);
    const seqBefore = latestSeqRef.current;
    const takeId = softCancelInfo?.takeId ?? activeTakeIdRef.current;
    try {
      const ok = await undoCancelRecording(takeId);
      if (ok && latestSeqRef.current === seqBefore) {
        setSoftCancelInfo(null);
        setCaptureReady(true);
        setIsPaused(false);
        const nextState = preSoftCancelStreamingRef.current
          ? "streaming"
          : "recording";
        if (nextState === "streaming") {
          setPhase("listening");
        }
        setState(nextState);
        stateRef.current = nextState;
      }
    } catch {
      // Ignore if deadline already elapsed on backend.
    } finally {
      undoPendingRef.current = false;
      setUndoPending(false);
    }
  };

  const handleConfirmSoftCancel = async () => {
    if (confirmPendingRef.current || undoPendingRef.current) return;
    confirmPendingRef.current = true;
    const seqBefore = latestSeqRef.current;
    const takeId = softCancelInfo?.takeId ?? activeTakeIdRef.current;
    try {
      await confirmCancelRecording(takeId);
    } catch {
      // Ignore if take already expired.
    } finally {
      confirmPendingRef.current = false;
      if (
        latestSeqRef.current === seqBefore &&
        stateRef.current === "soft_cancelled"
      ) {
        restoreUnderlyingMeetingOrHide();
      }
    }
  };

  const handleToggleMeetingPause = async () => {
    if (
      meetingTogglePendingRef.current ||
      meetingStopPendingRef.current ||
      !meetingOverlay
    ) {
      return;
    }
    meetingTogglePendingRef.current = true;
    setMeetingTogglePending(true);
    const seqBefore = latestSeqRef.current;
    const wasPaused = meetingOverlay.paused;
    try {
      const nextLive = await (wasPaused
        ? resume_meeting_notes()
        : pause_meeting_notes());
      if (nextLive && latestSeqRef.current === seqBefore) {
        const nowServerMs = Date.now() + clockSkewRef.current;
        const elapsedMs = Math.max(0, nextLive.elapsed_ms ?? 0);
        const nextPaused = nextLive.status === "paused";
        setMeetingOverlay((prev) => {
          const base = prev ?? meetingOverlay;
          return {
            ...base,
            meeting_id: nextLive.active_meeting_id ?? base.meeting_id,
            title: nextLive.title || base.title,
            paused: nextPaused,
            elapsed_ms: elapsedMs,
            started_at_ms: nowServerMs - elapsedMs,
            accumulated_paused_ms: 0,
            paused_at_ms: nextPaused ? nowServerMs : null,
            app_name: nextLive.app_name ?? base.app_name,
          };
        });
      }
    } catch {
      // Keep current overlay state if command failed.
    } finally {
      meetingTogglePendingRef.current = false;
      setMeetingTogglePending(false);
    }
  };

  const handleResumeStoppedMeeting = async () => {
    if (meetingTogglePendingRef.current || meetingStopPendingRef.current) {
      return;
    }
    meetingTogglePendingRef.current = true;
    setMeetingTogglePending(true);
    const seqBefore = latestSeqRef.current;
    const base = meetingSoftStoppedOverlay;
    try {
      const nextLive = await resume_meeting_notes();
      if (nextLive && latestSeqRef.current === seqBefore) {
        const nowServerMs = Date.now() + clockSkewRef.current;
        const elapsedMs = Math.max(
          0,
          nextLive.elapsed_ms ?? base?.elapsed_ms ?? 0,
        );
        setMeetingSoftStoppedOverlay(null);
        setMeetingOverlay({
          mode: "meeting_active",
          meeting_id:
            nextLive.active_meeting_id ?? base?.meeting_id ?? "active",
          title: nextLive.title || base?.title || "",
          paused: false,
          elapsed_ms: elapsedMs,
          started_at_ms: nowServerMs - elapsedMs,
          accumulated_paused_ms: 0,
          paused_at_ms: null,
          app_name: nextLive.app_name ?? base?.app_name ?? null,
        });
        setState("meeting_active");
        stateRef.current = "meeting_active";
        setIsVisible(true);
      }
    } catch {
      // Backend may have already finalized the meeting notes.
    } finally {
      meetingTogglePendingRef.current = false;
      setMeetingTogglePending(false);
    }
  };

  const handleStopMeeting = async () => {
    if (meetingStopPendingRef.current) return;
    meetingStopPendingRef.current = true;
    setMeetingStopPending(true);
    const currentMeeting = meetingOverlay ?? meetingSoftStoppedOverlay;
    setMeetingOverlay(null);
    setMeetingSoftStoppedOverlay(null);
    setSummarizingOverlay({
      mode: "meeting_summarizing",
      meeting_id: currentMeeting?.meeting_id ?? "summarizing",
      title: currentMeeting?.title ?? "",
    });
    setState("meeting_summarizing");
    stateRef.current = "meeting_summarizing";
    setIsVisible(true);

    try {
      await stop_meeting_notes();
    } catch {
      // Keep graceful recovery if backend already stopped.
    } finally {
      meetingStopPendingRef.current = false;
      setMeetingStopPending(false);
    }
  };

  const handleOpenMeetingNotes = async () => {
    try {
      await openMeetingsWorkspace();
    } catch {
      // Ignore if workspace already open or unavailable in isolated harness.
    }
  };

  const handleMinimizeMeetingPill = async () => {
    const seqBefore = latestSeqRef.current;
    const currentId = meetingOverlay?.meeting_id ?? null;
    localMeetingDismissedIdRef.current = currentId;
    try {
      await setMeetingPillDismissed(true);
    } catch {
      // Allow local hide fallback even when unmocked.
    }
    if (
      latestSeqRef.current === seqBefore &&
      stateRef.current === "meeting_active"
    ) {
      setIsVisible(false);
    }
  };

  // ---- Surface A: SV-82 Escape Soft-Cancel 5-Second Undo Pill ----
  if (state === "soft_cancelled") {
    const cancelledLabel = t("overlay.transcriptCancelled");
    const undoLabel = t("overlay.undoCancel");
    const dismissCancelLabel = t("overlay.dismissCancel");
    const progressPercent = Math.round(softCancelProgress * 100);

    return (
      <div
        dir={direction}
        className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
      >
        <div
          data-testid="overlay-soft-cancel-pill"
          className="scard compact is-soft-cancelled"
        >
          <div className="sundo-row">
            <div className="sundo-leading">
              <span className="sundo-icon" aria-hidden="true">
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <path
                    d="M8 2.5a2.25 2.25 0 0 1 2.25 2.25v2.7a2.25 2.25 0 0 1-4.5 0v-2.7A2.25 2.25 0 0 1 8 2.5Z"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.4"
                  />
                  <path
                    d="M4.2 7.6a3.8 3.8 0 0 0 7.6 0M8 11.4v2.1M3 3l10 10"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.4"
                    strokeLinecap="round"
                  />
                </svg>
              </span>
              <span className="sundo-label">{cancelledLabel}</span>
            </div>
            <div className="sundo-actions">
              <button
                type="button"
                data-testid="overlay-undo-cancel-btn"
                className="sundo-btn"
                disabled={undoPending}
                aria-label={undoLabel}
                onClick={() => void handleUndoSoftCancel()}
              >
                {undoLabel}
              </button>
              <button
                type="button"
                data-testid="overlay-dismiss-cancel-btn"
                className="sx sundo-dismiss"
                aria-label={dismissCancelLabel}
                title={dismissCancelLabel}
                onClick={() => void handleConfirmSoftCancel()}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <path
                    d="M4 4 L12 12 M12 4 L4 12"
                    stroke="currentColor"
                    strokeWidth="1.6"
                    strokeLinecap="round"
                  />
                </svg>
              </button>
            </div>
          </div>
          <div
            role="progressbar"
            data-testid="overlay-soft-cancel-progress"
            className="sundo-progress-track"
            aria-label={cancelledLabel}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progressPercent}
          >
            <div
              className="sundo-progress-fill"
              style={{
                width: `${(softCancelProgress * 100).toFixed(2)}%`,
              }}
            />
          </div>
        </div>
      </div>
    );
  }

  // ---- Surface B2: Call-End Auto-Stop Pill ("Notetaker stopped · Resume [×]") ----
  if (state === "meeting_soft_stopped") {
    const stoppedLabel = t("overlay.meetingStopped", {
      defaultValue: "Notetaker stopped",
    });
    const resumeMeetingLabel = t("overlay.resumeMeeting", {
      defaultValue: "Resume",
    });
    const dismissStoppedLabel = t("overlay.dismissMeetingStopped", {
      defaultValue: "Save & close",
    });
    const progressPercent = Math.round(meetingSoftStopProgress * 100);

    return (
      <div
        dir={direction}
        className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
      >
        <div
          data-testid="overlay-meeting-soft-stopped-pill"
          className="scard compact is-soft-cancelled is-meeting-soft-stopped"
        >
          <div className="sundo-row">
            <div className="sundo-leading">
              <span className="smeeting-stopped-halo" aria-hidden="true">
                <span className="smeeting-stopped-dot" />
              </span>
              <span className="sundo-label">{stoppedLabel}</span>
            </div>
            <div className="sundo-actions">
              <button
                type="button"
                data-testid="overlay-meeting-resume-stopped-btn"
                className="sundo-btn"
                disabled={meetingTogglePending || meetingStopPending}
                aria-label={resumeMeetingLabel}
                onClick={() => void handleResumeStoppedMeeting()}
              >
                {resumeMeetingLabel}
              </button>
              <button
                type="button"
                data-testid="overlay-meeting-confirm-stop-btn"
                className="sx sundo-dismiss"
                disabled={meetingStopPending}
                aria-label={dismissStoppedLabel}
                title={dismissStoppedLabel}
                onClick={() => void handleStopMeeting()}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <path
                    d="M4 4 L12 12 M12 4 L4 12"
                    stroke="currentColor"
                    strokeWidth="1.6"
                    strokeLinecap="round"
                  />
                </svg>
              </button>
            </div>
          </div>
          <div
            role="progressbar"
            data-testid="overlay-meeting-soft-stop-progress"
            className="sundo-progress-track"
            aria-label={stoppedLabel}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progressPercent}
          >
            <div
              className="sundo-progress-fill"
              style={{
                width: `${(meetingSoftStopProgress * 100).toFixed(2)}%`,
              }}
            />
          </div>
        </div>
      </div>
    );
  }

  // ---- Surface B: SV-86 Unified Active Meeting Pill ----
  if (state === "meeting_active") {
    const meetingPaused = meetingOverlay?.paused ?? false;
    const statusText = meetingPaused
      ? t("overlay.meetingPaused")
      : t("overlay.meetingRecording");
    const pauseMeetingLabel = t("overlay.pauseMeeting");
    const resumeMeetingLabel = t("overlay.resumeMeeting");
    const stopMeetingLabel = t("overlay.stopMeeting");
    const openNotesLabel = t("overlay.openMeetingNotes");
    const minimizePillLabel = t("overlay.minimizeMeetingPill");

    return (
      <div
        dir={direction}
        className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
      >
        <div
          data-testid="overlay-meeting-active-pill"
          className={`scard compact is-meeting-pill ${
            meetingPaused ? "is-paused" : "is-recording"
          }`}
        >
          <div className="smeeting-row">
            <div className="smeeting-status">
              <span
                className={`sdot smeeting-dot ${
                  meetingPaused ? "paused" : "recording"
                }`}
                role="img"
                aria-label={statusText}
                title={statusText}
              />
              <span
                data-testid="overlay-meeting-timer"
                className="stimer smeeting-timer"
              >
                {formatDurationMmSs(meetingDisplayElapsedMs)}
              </span>
              {meetingPaused && (
                <span className="smeeting-paused-badge">
                  {t("overlay.meetingPaused")}
                </span>
              )}
            </div>

            <div className="smeeting-actions">
              <button
                type="button"
                data-testid="overlay-meeting-pause-resume-btn"
                className={`smeeting-btn ${
                  meetingPaused ? "is-resume" : "is-pause"
                }`}
                disabled={meetingTogglePending || meetingStopPending}
                aria-label={
                  meetingPaused ? resumeMeetingLabel : pauseMeetingLabel
                }
                title={meetingPaused ? resumeMeetingLabel : pauseMeetingLabel}
                onClick={() => void handleToggleMeetingPause()}
              >
                {meetingPaused ? (
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <path d="M5 3.5L12.5 8L5 12.5V3.5Z" fill="currentColor" />
                  </svg>
                ) : (
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <rect
                      x="4.2"
                      y="3.5"
                      width="2.6"
                      height="9"
                      rx="1"
                      fill="currentColor"
                    />
                    <rect
                      x="9.2"
                      y="3.5"
                      width="2.6"
                      height="9"
                      rx="1"
                      fill="currentColor"
                    />
                  </svg>
                )}
                <span
                  className={meetingPaused ? undefined : "smeeting-sr-only"}
                >
                  {meetingPaused ? resumeMeetingLabel : pauseMeetingLabel}
                </span>
              </button>

              <button
                type="button"
                data-testid="overlay-meeting-stop-btn"
                className="smeeting-btn is-stop"
                disabled={meetingStopPending}
                aria-label={stopMeetingLabel}
                title={stopMeetingLabel}
                onClick={() => void handleStopMeeting()}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <rect
                    x="4"
                    y="4"
                    width="8"
                    height="8"
                    rx="1.6"
                    fill="currentColor"
                  />
                </svg>
                <span className="smeeting-sr-only">{stopMeetingLabel}</span>
              </button>

              <button
                type="button"
                data-testid="overlay-meeting-open-notes-btn"
                className="smeeting-icon-btn"
                aria-label={openNotesLabel}
                title={openNotesLabel}
                onClick={() => void handleOpenMeetingNotes()}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <path
                    d="M5 11L11 5M6.2 5H11V9.8"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.6"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  />
                </svg>
              </button>

              <button
                type="button"
                data-testid="overlay-meeting-minimize-btn"
                className="smeeting-icon-btn"
                aria-label={minimizePillLabel}
                title={minimizePillLabel}
                onClick={() => void handleMinimizeMeetingPill()}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <path
                    d="M4 8H12"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.8"
                    strokeLinecap="round"
                  />
                </svg>
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (state === "meeting_summarizing") {
    const savingNotesLabel = t("overlay.meetingSavingNotes");
    return (
      <div
        dir={direction}
        className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
      >
        <div
          data-testid="overlay-meeting-summarizing-pill"
          className="scard compact is-meeting-pill is-summarizing"
          title={summarizingOverlay?.title || undefined}
        >
          <div className="smeeting-row is-summarizing">
            <span className="sspinner smeeting-spinner" aria-hidden="true" />
            <span className="swork-label smeeting-saving-label">
              {savingNotesLabel}
            </span>
          </div>
        </div>
      </div>
    );
  }

  if (isNotice) {
    const startNotesLabel = t("overlay.callStartNotes", {
      defaultValue: "Start Notes",
    });
    const notNowLabel = t("overlay.callDismiss", {
      defaultValue: "Not now",
    });
    const nowLabel = t("overlay.callNow", { defaultValue: "Now" });

    return (
      <div
        dir={direction}
        className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
      >
        <div
          className={`scard compact cnotice ${
            isCallDetectedNotice ? "is-call-detected" : ""
          }`}
        >
          <div className="sbase is-notice">
            <div className="sbase-l">
              {isCallDetectedNotice ? (
                <span
                  className={`scall-app-badge is-${callAppKind}`}
                  aria-hidden="true"
                >
                  {callAppKind === "teams" ? (
                    "T"
                  ) : callAppKind === "zoom" ? (
                    "Z"
                  ) : (
                    <svg viewBox="0 0 16 16" aria-hidden="true">
                      <rect
                        x="2"
                        y="4"
                        width="8.5"
                        height="8"
                        rx="2"
                        fill="currentColor"
                      />
                      <path
                        d="M11 6.6L14.2 4.6V11.4L11 9.4V6.6Z"
                        fill="currentColor"
                      />
                    </svg>
                  )}
                </span>
              ) : (
                <span className="sdot notice" />
              )}
            </div>
            {isCallDetectedNotice ? (
              <div className="scall-mid">
                <span className="swork-label snotice-label">{noticeLabel}</span>
                <span className="scall-now">
                  <span className="sdot call" />
                  <span>{nowLabel}</span>
                </span>
              </div>
            ) : (
              <span className="swork-label snotice-label">{noticeLabel}</span>
            )}
            <div className="sbase-r">
              {isCallDetectedNotice ? (
                <div className="scall-actions">
                  <button
                    type="button"
                    data-testid="overlay-call-start-btn"
                    className="scall-btn is-start"
                    aria-label={startNotesLabel}
                    onClick={() => void handleStartCallNotes()}
                  >
                    {startNotesLabel}
                  </button>
                  <button
                    type="button"
                    data-testid="overlay-call-dismiss-btn"
                    className="sx"
                    aria-label={notNowLabel}
                    title={notNowLabel}
                    onClick={() => void handleDismissCallNotice()}
                  >
                    <svg viewBox="0 0 16 16" aria-hidden="true">
                      <path
                        d="M4 4 L12 12 M12 4 L4 12"
                        stroke="currentColor"
                        strokeWidth="1.6"
                        strokeLinecap="round"
                      />
                    </svg>
                  </button>
                </div>
              ) : (
                <button
                  type="button"
                  className="sx"
                  aria-label={dismissLabel}
                  onClick={() => void handleDismissCallNotice()}
                >
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <path
                      d="M4 4 L12 12 M12 4 L4 12"
                      stroke="currentColor"
                      strokeWidth="1.6"
                      strokeLinecap="round"
                    />
                  </svg>
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    );
  }

  const working = state === "transcribing" || state === "processing";
  const capturingCard = !working && captureReady;
  const pausedCard = capturingCard && isPaused;
  const workLabel = state === "processing" ? polishingLabel : transcribingLabel;

  return (
    <div
      dir={direction}
      className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
    >
      <div
        className={`scard compact ${capturingCard ? "is-capturing" : ""} ${
          pausedCard ? "cpaused" : ""
        } ${working && isVisible ? "cworking" : ""} ${
          showPasteRaw ? "has-paste-raw" : ""
        }`}
      >
        {working
          ? workingRow(workLabel, true, showPasteRaw)
          : listeningRow(true, true)}
      </div>
    </div>
  );
};

export default RecordingOverlay;
