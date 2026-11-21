import { expect, test, type Page } from "@playwright/test";
import type {
  MeetingDetail,
  MeetingLiveState,
  MeetingSummaryRecord,
} from "../src/components/meetings/api";

async function installWave4cMeetingsMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const listeners = new Map<string, number[]>();
    const callbacks = new Map<number, (data: unknown) => unknown>();
    let nextCallbackId = 1;

    const liveState: MeetingLiveState = {
      status: "recording",
      active_meeting_id: "mtg-wave4c-live",
      title: "Meeting — 2026-10-27 16:00",
      template: "general",
      elapsed_ms: 95_000,
      capture_mode: "dual_stream",
      output_route: "headphones",
      app_name: "zoom.us",
      rough_notes: "- Q4 SDK rollout",
      recent_segments: [
        {
          id: 1,
          meeting_id: "mtg-wave4c-live",
          speaker: "me",
          start_ms: 4_000,
          end_ms: 8_000,
          text: "Priya, can we confirm the Q4 SDK rollout?",
          language: "en",
          speaker_id: "me",
          speaker_label: "Me",
        },
        {
          id: 2,
          meeting_id: "mtg-wave4c-live",
          speaker: "others",
          start_ms: 9_000,
          end_ms: 14_000,
          text: "Sure, backend p95 is down to 18ms.",
          language: "en",
          speaker_id: "speaker_1",
          speaker_label: "Speaker 1 (Priya)",
        },
        {
          id: 3,
          meeting_id: "mtg-wave4c-live",
          speaker: "others",
          start_ms: 15_000,
          end_ms: 21_000,
          text: "ہمیں کل تک ریلیز چیک لسٹ مکمل کرنی ہے۔",
          language: "ur",
          speaker_id: "speaker_2",
          speaker_label: "Speaker 2",
        },
        {
          id: 4,
          meeting_id: "mtg-wave4c-live",
          speaker: "others",
          start_ms: 22_000,
          end_ms: 25_000,
          text: "Unattributed legacy segment fallback check.",
          language: "en",
          speaker_id: null,
          speaker_label: null,
        },
      ],
      rollup_count: 0,
      detected_call: null,
      auto_detect_enabled: true,
    };

    const summaryRecord: MeetingSummaryRecord = {
      id: "mtg-wave4c-past",
      title: "Q4 SDK Rollout & Latency Review",
      started_at_ms: 1730040000000,
      ended_at_ms: 1730041800000,
      duration_ms: 1_800_000,
      template: "general",
      capture_mode: "dual_stream",
      output_route: "headphones",
      app_name: "Google Meet",
      segment_count: 3,
      preview_snippet: "## TL;DR\nAligned on Q4 SDK rollout.",
    };

    const pastDetail: MeetingDetail = {
      summary: summaryRecord,
      rough_notes: "- Q4 SDK rollout\n- p95 latency 18ms",
      expanded_notes_md:
        "## TL;DR\n- Aligned on Q4 SDK rollout.\n\n## Key Decisions & Discussion\n- Backend p95 dropped to 18ms.\n\n## Expanded from Your Notes\n- Q4 SDK rollout\n\n## Action Items\n- Complete release checklist tomorrow.",
      summary_source: "local_extractive",
      segments: [
        {
          id: 11,
          meeting_id: "mtg-wave4c-past",
          speaker: "me",
          start_ms: 2_000,
          end_ms: 6_000,
          text: "Let's review the release plan.",
          language: "en",
          speaker_id: "me",
          speaker_label: "Me",
        },
        {
          id: 12,
          meeting_id: "mtg-wave4c-past",
          speaker: "others",
          start_ms: 7_000,
          end_ms: 13_000,
          text: "Hi, I'm Rahul — mobile SDK checks are green.",
          language: "en",
          speaker_id: "speaker_1",
          speaker_label: "Speaker 1 (Rahul)",
        },
        {
          id: 13,
          meeting_id: "mtg-wave4c-past",
          speaker: "others",
          start_ms: 14_000,
          end_ms: 19_000,
          text: "QA sign-off finishes at 5pm.",
          language: "en",
          speaker_id: "speaker_2",
          speaker_label: "Speaker 2 (Priya)",
        },
      ],
      rollups: [],
    };

    function registerCallback(
      callback: (data: unknown) => unknown,
      once = false,
    ): number {
      const id = nextCallbackId++;
      callbacks.set(id, (data: unknown) => {
        if (once) callbacks.delete(id);
        return callback ? callback(data) : undefined;
      });
      return id;
    }

    function unregisterCallback(id: number): void {
      callbacks.delete(id);
    }

    function runCallback(id: number, data: unknown): void {
      const cb = callbacks.get(id);
      if (cb) cb(data);
    }

    function emitEvent(event: string, payload: unknown): void {
      const ids = listeners.get(event) ?? [];
      for (const id of [...ids]) {
        runCallback(id, { event, id, payload });
      }
    }

    const win = window as unknown as Record<string, unknown>;
    win.__wave4cEmit = emitEvent;
    win.__TAURI_OS_PLUGIN_INTERNALS__ = {
      platform: "macos",
      arch: "aarch64",
      family: "unix",
      os_type: "macos",
      version: "15.0.0",
      exe_extension: "",
      eol: "\n",
    };
    win.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: (_event: string, id: number) => {
        unregisterCallback(id);
      },
    };
    win.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { windowLabel: "main", label: "main" },
      },
      callbacks,
      transformCallback: registerCallback,
      unregisterCallback,
      runCallback,
      invoke: async (cmd: string, rawArgs?: Record<string, unknown>) => {
        const args = (rawArgs ?? {}) as Record<string, unknown>;
        if (cmd === "plugin:event|listen") {
          const eventName = String(args.event ?? "");
          const handlerId = Number(args.handler);
          const list = listeners.get(eventName) ?? [];
          list.push(handlerId);
          listeners.set(eventName, list);
          return handlerId;
        }
        if (cmd === "plugin:event|unlisten" || cmd === "plugin:event|emit") {
          return null;
        }

        switch (cmd) {
          case "plugin:os|locale":
            return "en-US";
          case "plugin:macos-permissions|check_accessibility_permission":
          case "plugin:macos-permissions|check_microphone_permission":
            return true;
          case "check_accessibility_permission_status":
          case "check_microphone_permission_status":
            return true;
          case "get_app_settings":
          case "get_default_settings":
            return {
              app_language: "en",
              overlay_position: "bottom",
              onboarding_completed: true,
              selected_model: "whisper-medium-q4_1",
              selected_language: "auto",
              spoken_languages: ["en", "hi", "ur"],
              app_theme: "dark",
              custom_words: [],
              bindings: {},
            };
          case "get_available_models":
            return [
              {
                id: "whisper-medium-q4_1",
                name: "Whisper Medium",
                is_downloaded: true,
                is_downloading: false,
                partial_size: 0,
                size_mb: 500,
              },
            ];
          case "get_current_model":
            return "whisper-medium-q4_1";
          case "has_any_models_available":
          case "has_any_models_or_downloads":
            return true;
          case "is_Select_model_ready":
          case "get_model_load_status":
            return { is_loaded: true, current_model: "whisper-medium-q4_1" };
          case "get_session_bus_state_json":
            return JSON.stringify({
              seq: 1,
              server_now_ms: Date.now(),
              dictation: { phase: "idle" },
              meeting: {
                phase: "recording",
                meeting_id: "mtg-wave4c-live",
                title: "Meeting — 2026-10-27 16:00",
                started_at_ms: Date.now() - 95_000,
                accumulated_paused_ms: 0,
                capture_mode: "dual_stream",
                app_name: "zoom.us",
              },
              detected_call: null,
              transient_notice: null,
              overlay: { mode: "hidden" },
              tray_meeting: {
                active: true,
                paused: false,
                summarizing: false,
                meeting_id: "mtg-wave4c-live",
                title: "Meeting — 2026-10-27 16:00",
                elapsed_ms: 95_000,
              },
            });
          case "get_meeting_live_state":
            return liveState;
          case "list_meetings":
            return [summaryRecord];
          case "get_meeting_detail":
            return pastDetail;
          case "get_meeting_settings":
            return {
              call_detection_enabled: true,
              system_audio_enabled: true,
              auto_summarize: true,
              retain_meeting_audio: false,
              default_template: "general",
            };
          case "check_system_audio_capability":
            return {
              supported: true,
              macos_version: "15.0",
              detail: "Audio Tap ready",
            };
          case "get_retired_models":
          case "get_available_microphones":
          case "get_available_output_devices":
            return [];
          case "initialize_enigo":
          case "initialize_shortcuts":
            return true;
          default:
            return null;
        }
      },
    };
  });
}

test.describe("Wave 4C Speaker Attribution & Auto-Title UI (SV-89 / SV-90)", () => {
  test("renders multi-speaker labels with BiDi isolation in live feed and archived detail pane", async ({
    page,
  }) => {
    await installWave4cMeetingsMock(page);
    await page.goto("/");

    await page.evaluate(() => {
      const win = window as unknown as {
        __wave4cEmit: (event: string, payload: unknown) => void;
      };
      win.__wave4cEmit("navigate-section", "meetings");
    });

    // 1. Live transcript feed renders Me, Speaker 1 (Priya), Speaker 2, and fallback Others
    const liveLog = page.getByRole("log");
    await expect(liveLog).toBeVisible();
    await expect(liveLog).toContainText("\u2068Speaker 1 (Priya)\u2069");
    await expect(liveLog).toContainText("\u2068Speaker 2\u2069");
    await expect(liveLog).toContainText("\u2068Me\u2069");
    await expect(liveLog).toContainText("\u2068Others\u2069");

    // 2. Past meeting list + detail pane shows auto-generated title and attributed Transcript tab
    const pastMeetingCard = page.getByText("Q4 SDK Rollout & Latency Review");
    await expect(pastMeetingCard).toBeVisible();
    await pastMeetingCard.click();

    const transcriptTab = page.getByRole("tab", { name: /Transcript/i });
    await transcriptTab.click();

    await expect(page.getByText("\u2068Speaker 1 (Rahul)\u2069")).toBeVisible();
    await expect(page.getByText("\u2068Speaker 2 (Priya)\u2069")).toBeVisible();
  });
});
