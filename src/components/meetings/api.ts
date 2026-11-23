import { invoke } from "@tauri-apps/api/core";

export type SpeakerChannel = "me" | "others";

export type OutputRouteKind = "headphones" | "speakers" | "unknown";

export type AudioCaptureMode = "dual_stream" | "mic_only_fallback";

export type MeetingStatus = "idle" | "recording" | "paused" | "summarizing";

export type MeetingTemplate =
  | "general"
  | "one_on_one"
  | "standup"
  | "language_lesson"
  | "client_call";

export const MEETING_TEMPLATES: readonly MeetingTemplate[] = [
  "general",
  "one_on_one",
  "standup",
  "language_lesson",
  "client_call",
] as const;

export interface DetectedCallInfo {
  call_id: string;
  app_bundle_id: string;
  app_name: string;
  output_route: OutputRouteKind;
  detected_at_ms: number;
}

export interface MeetingSegment {
  id: number;
  meeting_id: string;
  speaker: SpeakerChannel;
  start_ms: number;
  end_ms: number;
  text: string;
  language: string | null;
  speaker_id?: string | null;
  speaker_label?: string | null;
}

export interface MeetingRollup {
  id: number;
  meeting_id: string;
  window_index: number;
  start_ms: number;
  end_ms: number;
  summary_bullets: string;
}

export interface MeetingSummaryRecord {
  id: string;
  title: string;
  started_at_ms: number;
  ended_at_ms: number | null;
  duration_ms: number;
  template: MeetingTemplate;
  capture_mode: AudioCaptureMode;
  output_route: OutputRouteKind;
  app_name: string | null;
  segment_count: number;
  preview_snippet: string;
}

export interface MeetingDetail {
  summary: MeetingSummaryRecord;
  rough_notes: string;
  expanded_notes_md: string;
  summary_source: string;
  segments: Vec<MeetingSegment>;
  rollups: Vec<MeetingRollup>;
}

type Vec<T> = T[];

export interface MeetingLiveState {
  status: MeetingStatus;
  active_meeting_id: string | null;
  title: string;
  template: MeetingTemplate;
  elapsed_ms: number;
  capture_mode: AudioCaptureMode;
  output_route: OutputRouteKind;
  app_name: string | null;
  rough_notes: string;
  recent_segments: MeetingSegment[];
  rollup_count: number;
  detected_call: DetectedCallInfo | null;
  auto_detect_enabled: boolean;
}

export const DEFAULT_LIVE_STATE: MeetingLiveState = {
  status: "idle",
  active_meeting_id: null,
  title: "",
  template: "general",
  elapsed_ms: 0,
  capture_mode: "mic_only_fallback",
  output_route: "unknown",
  app_name: null,
  rough_notes: "",
  recent_segments: [],
  rollup_count: 0,
  detected_call: null,
  auto_detect_enabled: true,
};

/** Wrap dynamic text in Unicode First-Strong Isolate (`FSI`..`PDI`) for mixed English + RTL safety (`D-40`). */
export function isolateBidi(value: string | null | undefined): string {
  if (!value) return "";
  if (value.startsWith("\u2068") && value.endsWith("\u2069")) return value;
  return `\u2068${value}\u2069`;
}

/**
 * Resolves the display badge text for a transcript segment (`SV-89`).
 * Preserves 26-locale i18n labels for generic `"me"` / `"others"` turns while
 * surfacing specific diarized speaker labels (`Speaker 1`, `Speaker 1 (Rahul)`).
 */
export function formatSegmentSpeakerBadge(
  seg: Pick<MeetingSegment, "speaker" | "speaker_label">,
  meTranslated: string,
  othersTranslated: string,
): string {
  const custom = seg.speaker_label?.trim() ?? "";
  if (seg.speaker === "me") {
    if (!custom || custom.toLowerCase() === "me") {
      return meTranslated;
    }
    return custom;
  }
  if (!custom || custom.toLowerCase() === "others") {
    return othersTranslated;
  }
  return custom;
}

/** Format milliseconds as `MM:SS` (or `HH:MM:SS` when >= 1 hour). */
export function formatDurationMmSs(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(totalSec / 3600);
  const minutes = Math.floor((totalSec % 3600) / 60);
  const seconds = totalSec % 60;
  if (hours > 0) {
    return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

export async function get_meeting_live_state(): Promise<MeetingLiveState> {
  return await invoke<MeetingLiveState>("get_meeting_live_state");
}

export async function start_meeting_notes(
  title?: string | null,
  template?: MeetingTemplate | null,
): Promise<MeetingLiveState> {
  return await invoke<MeetingLiveState>("start_meeting_notes", {
    title: title ?? null,
    template: template ?? null,
  });
}

export async function pause_meeting_notes(): Promise<MeetingLiveState> {
  return await invoke<MeetingLiveState>("pause_meeting_notes");
}

export async function resume_meeting_notes(): Promise<MeetingLiveState> {
  return await invoke<MeetingLiveState>("resume_meeting_notes");
}

export async function stop_meeting_notes(): Promise<MeetingDetail> {
  return await invoke<MeetingDetail>("stop_meeting_notes");
}

export async function update_live_rough_notes(
  roughNotes: string,
): Promise<void> {
  await invoke<void>("update_live_rough_notes", {
    roughNotes,
    rough_notes: roughNotes,
    notes: roughNotes,
  });
}

export async function set_meeting_auto_detect(
  enabled: boolean,
): Promise<boolean> {
  return await invoke<boolean>("set_meeting_auto_detect", { enabled });
}

export async function dismiss_detected_call(
  callId?: string | null,
): Promise<void> {
  const resolved = callId ?? null;
  await invoke<void>("dismiss_detected_call", {
    callId: resolved,
    call_id: resolved,
  });
}

export async function list_meetings(
  query?: string | null,
  limit?: number | null,
): Promise<MeetingSummaryRecord[]> {
  return await invoke<MeetingSummaryRecord[]>("list_meetings", {
    query: query ?? null,
    limit: limit ?? null,
  });
}

export async function get_meeting_detail(
  meetingId: string,
): Promise<MeetingDetail> {
  return await invoke<MeetingDetail>("get_meeting_detail", {
    meetingId,
    meeting_id: meetingId,
    id: meetingId,
  });
}

export async function update_meeting_meta(params: {
  meetingId: string;
  title?: string | null;
  roughNotes?: string | null;
  template?: MeetingTemplate | null;
}): Promise<MeetingDetail> {
  const { meetingId, title = null, roughNotes = null, template = null } = params;
  return await invoke<MeetingDetail>("update_meeting_meta", {
    meetingId,
    meeting_id: meetingId,
    id: meetingId,
    title,
    roughNotes,
    rough_notes: roughNotes,
    template,
  });
}

export async function regenerate_meeting_notes(
  meetingId: string,
  template?: MeetingTemplate | null,
): Promise<MeetingDetail> {
  return await invoke<MeetingDetail>("regenerate_meeting_notes", {
    meetingId,
    meeting_id: meetingId,
    id: meetingId,
    template: template ?? null,
  });
}

export async function export_meeting_markdown(
  meetingId: string,
): Promise<string> {
  return await invoke<string>("export_meeting_markdown", {
    meetingId,
    meeting_id: meetingId,
    id: meetingId,
  });
}

export async function copy_meeting_markdown(
  meetingId: string,
): Promise<string> {
  return await invoke<string>("copy_meeting_markdown", {
    meetingId,
    meeting_id: meetingId,
    id: meetingId,
  });
}

export async function delete_meeting(meetingId: string): Promise<boolean> {
  return await invoke<boolean>("delete_meeting", {
    meetingId,
    meeting_id: meetingId,
    id: meetingId,
  });
}

export type CalendarAccessStatus =
  | "not_determined"
  | "authorized"
  | "denied"
  | "unsupported";

export interface ActiveCalendarMeeting {
  title: string;
  attendees: string[];
}

export async function get_calendar_access_status(): Promise<CalendarAccessStatus> {
  try {
    return await invoke<CalendarAccessStatus>("get_calendar_access_status");
  } catch {
    return "unsupported";
  }
}

export async function connect_calendar_access(): Promise<CalendarAccessStatus> {
  return await invoke<CalendarAccessStatus>("connect_calendar_access");
}

export async function get_active_calendar_meeting(): Promise<ActiveCalendarMeeting | null> {
  try {
    return await invoke<ActiveCalendarMeeting | null>(
      "get_active_calendar_meeting",
    );
  } catch {
    return null;
  }
}

export const meetingApi = {
  getLiveState: get_meeting_live_state,
  startNotes: start_meeting_notes,
  pauseNotes: pause_meeting_notes,
  resumeNotes: resume_meeting_notes,
  stopNotes: stop_meeting_notes,
  updateLiveRoughNotes: update_live_rough_notes,
  setAutoDetect: set_meeting_auto_detect,
  dismissDetectedCall: dismiss_detected_call,
  listMeetings: list_meetings,
  getMeetingDetail: get_meeting_detail,
  updateMeetingMeta: update_meeting_meta,
  regenerateMeetingNotes: regenerate_meeting_notes,
  exportMeetingMarkdown: export_meeting_markdown,
  copyMeetingMarkdown: copy_meeting_markdown,
  deleteMeeting: delete_meeting,
  getCalendarAccessStatus: get_calendar_access_status,
  connectCalendarAccess: connect_calendar_access,
  getActiveCalendarMeeting: get_active_calendar_meeting,
};
