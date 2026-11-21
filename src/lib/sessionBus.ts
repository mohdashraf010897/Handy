import { invoke } from "@tauri-apps/api/core";
import type {
  AudioCaptureMode,
  OutputRouteKind,
} from "../components/meetings/api";

export type { AudioCaptureMode, OutputRouteKind };

/** Default undo window for a softly cancelled dictation take (`5_000` ms). */
export const DEFAULT_SOFT_CANCEL_GRACE_MS = 5_000;

/** Event name emitted whenever the backend `SessionBus` advances. */
export const SESSION_BUS_EVENT = "session-bus-state";

/** Current dictation lifecycle state, discriminated by `phase` on the wire. */
export type DictationPhase =
  | { phase: "idle" }
  | {
      phase: "recording";
      take_id: number;
      started_at_ms: number;
      locked: boolean;
      paused: boolean;
    }
  | {
      phase: "soft_cancelled";
      take_id: number;
      cancelled_at_ms: number;
      expires_at_ms: number;
      grace_ms: number;
    }
  | {
      phase: "transcribing";
      take_id: number;
    }
  | {
      phase: "post_processing";
      take_id: number;
    };

/** A detected call that can be offered for meeting capture. */
export interface DetectedCallBanner {
  call_id: string;
  app_name: string;
  output_route: OutputRouteKind;
}

/** Current meeting lifecycle state, discriminated by `phase` on the wire. */
export type MeetingPhase =
  | { phase: "idle" }
  | {
      phase: "recording";
      meeting_id: string;
      title: string;
      started_at_ms: number;
      accumulated_paused_ms: number;
      app_name: string | null;
      capture_mode: AudioCaptureMode;
    }
  | {
      phase: "paused";
      meeting_id: string;
      title: string;
      started_at_ms: number;
      paused_at_ms: number;
      accumulated_paused_ms: number;
      app_name: string | null;
      capture_mode: AudioCaptureMode;
    }
  | {
      phase: "summarizing";
      meeting_id: string;
      title: string;
    };

/** What to render in the floating pill, discriminated by `mode` on the wire. */
export type OverlayProjection =
  | { mode: "hidden" }
  | {
      mode: "dictation_recording";
      take_id: number;
      locked: boolean;
      paused: boolean;
    }
  | {
      mode: "dictation_soft_cancelled";
      take_id: number;
      cancelled_at_ms: number;
      expires_at_ms: number;
      grace_ms: number;
    }
  | {
      mode: "dictation_transcribing";
      take_id: number;
    }
  | {
      mode: "dictation_post_processing";
      take_id: number;
    }
  | {
      mode: "meeting_active";
      meeting_id: string;
      title: string;
      paused: boolean;
      elapsed_ms: number;
      started_at_ms: number;
      accumulated_paused_ms: number;
      paused_at_ms: number | null;
      app_name: string | null;
    }
  | {
      mode: "meeting_summarizing";
      meeting_id: string;
      title: string;
    }
  | {
      mode: "call_detected";
      call_id: string;
      app_name: string;
      output_route: OutputRouteKind;
    }
  | {
      mode: "transient_notice";
      notice_kind: string;
    };

export type MeetingActiveOverlay = Extract<
  OverlayProjection,
  { mode: "meeting_active" }
>;

export type MeetingSummarizingOverlay = Extract<
  OverlayProjection,
  { mode: "meeting_summarizing" }
>;

export type DictationSoftCancelledOverlay = Extract<
  OverlayProjection,
  { mode: "dictation_soft_cancelled" }
>;

/** Meeting state for the tray, independent of which pill is visible. */
export interface TrayMeetingProjection {
  active: boolean;
  paused: boolean;
  summarizing: boolean;
  meeting_id: string | null;
  title: string | null;
  elapsed_ms: number;
}

/** A consistent view of the session phases and their UI projections. */
export interface SessionBusSnapshot {
  seq: number;
  server_now_ms: number;
  dictation: DictationPhase;
  meeting: MeetingPhase;
  detected_call: DetectedCallBanner | null;
  transient_notice: string | null;
  overlay: OverlayProjection;
  tray_meeting: TrayMeetingProjection;
}

/**
 * Computes live active meeting recording duration in milliseconds without drift
 * across pause/resume cycles.
 *
 * `clockSkewMs` is `server_now_ms - snapshotReceivedClientNowMs` so
 * `clientNowMs + clockSkewMs` estimates the backend's current timestamp.
 */
export function computeLiveMeetingElapsedMs(
  overlay: Extract<OverlayProjection, { mode: "meeting_active" }>,
  clientNowMs: number,
  clockSkewMs: number,
): number {
  if (overlay.paused) {
    if (overlay.paused_at_ms !== null) {
      return Math.max(
        0,
        overlay.paused_at_ms -
          overlay.started_at_ms -
          overlay.accumulated_paused_ms,
      );
    }
    return Math.max(0, overlay.elapsed_ms);
  }
  const estimatedServerNowMs = clientNowMs + clockSkewMs;
  const liveElapsedMs =
    estimatedServerNowMs -
    overlay.started_at_ms -
    overlay.accumulated_paused_ms;
  return Math.max(0, overlay.elapsed_ms, liveElapsedMs);
}

/**
 * Computes soft-cancel countdown progress in `[0.0, 1.0]` (`1.0` -> `0.0`),
 * clamped cleanly even if the webview renders late or slightly ahead of clock.
 */
export function computeSoftCancelProgress(
  cancelledAtMs: number,
  expiresAtMs: number,
  graceMs: number,
  nowMs: number,
): number {
  const windowMs = graceMs > 0 ? graceMs : expiresAtMs - cancelledAtMs;
  if (!Number.isFinite(windowMs) || windowMs <= 0 || nowMs >= expiresAtMs) {
    return 0;
  }
  const remainingMs = expiresAtMs - nowMs;
  return Math.min(1, Math.max(0, remainingMs / windowMs));
}

/** Mirrors `lowyap_ports::MeetingPhase::elapsed_ms`. */
export function computeMeetingPhaseElapsedMs(
  meeting: MeetingPhase,
  nowMs: number,
): number {
  switch (meeting.phase) {
    case "recording":
      return Math.max(
        0,
        nowMs - meeting.started_at_ms - meeting.accumulated_paused_ms,
      );
    case "paused":
      return Math.max(
        0,
        meeting.paused_at_ms -
          meeting.started_at_ms -
          meeting.accumulated_paused_ms,
      );
    case "idle":
    case "summarizing":
    default:
      return 0;
  }
}

/** Projects a `MeetingPhase` into its corresponding meeting overlay variant. */
export function projectMeetingOverlay(
  meeting: MeetingPhase,
  nowMs: number,
): MeetingActiveOverlay | MeetingSummarizingOverlay | null {
  switch (meeting.phase) {
    case "recording":
      return {
        mode: "meeting_active",
        meeting_id: meeting.meeting_id,
        title: meeting.title,
        paused: false,
        elapsed_ms: computeMeetingPhaseElapsedMs(meeting, nowMs),
        started_at_ms: meeting.started_at_ms,
        accumulated_paused_ms: meeting.accumulated_paused_ms,
        paused_at_ms: null,
        app_name: meeting.app_name,
      };
    case "paused":
      return {
        mode: "meeting_active",
        meeting_id: meeting.meeting_id,
        title: meeting.title,
        paused: true,
        elapsed_ms: computeMeetingPhaseElapsedMs(meeting, nowMs),
        started_at_ms: meeting.started_at_ms,
        accumulated_paused_ms: meeting.accumulated_paused_ms,
        paused_at_ms: meeting.paused_at_ms,
        app_name: meeting.app_name,
      };
    case "summarizing":
      return {
        mode: "meeting_summarizing",
        meeting_id: meeting.meeting_id,
        title: meeting.title,
      };
    case "idle":
    default:
      return null;
  }
}

/**
 * Reads the current `SessionBusSnapshot` from the backend, returning `null`
 * gracefully when running against older mocks or before backend init.
 */
export async function fetchSessionBusSnapshot(): Promise<SessionBusSnapshot | null> {
  try {
    const raw = await invoke<string | SessionBusSnapshot>(
      "get_session_bus_state_json",
    );
    if (!raw) return null;
    if (typeof raw === "string") {
      return JSON.parse(raw) as SessionBusSnapshot;
    }
    return raw;
  } catch {
    return null;
  }
}

export async function undoCancelRecording(
  takeId: number | null = null,
): Promise<boolean> {
  return await invoke<boolean>("undo_cancel_recording", {
    takeId: takeId ?? null,
  });
}

export async function confirmCancelRecording(
  takeId: number | null = null,
): Promise<boolean> {
  return await invoke<boolean>("confirm_cancel_recording", {
    takeId: takeId ?? null,
  });
}

export async function setMeetingPillDismissed(
  dismissed: boolean,
): Promise<boolean> {
  return await invoke<boolean>("set_meeting_pill_dismissed", {
    dismissed,
  });
}

export async function openMeetingsWorkspace(): Promise<void> {
  await invoke<void>("open_meetings_workspace");
}
