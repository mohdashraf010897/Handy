import { expect, test, type Page } from "@playwright/test";
import {
  type OverlayProjection,
  type SessionBusSnapshot,
  computeLiveMeetingElapsedMs,
  computeSoftCancelProgress,
} from "../src/lib/sessionBus";

interface IpcCallRecord {
  cmd: string;
  args: Record<string, unknown>;
}

declare global {
  interface Window {
    __wave4Calls: IpcCallRecord[];
    __wave4Emit: (event: string, payload: unknown) => void;
    __wave4SetSnapshot: (snapshot: SessionBusSnapshot) => void;
  }
}

const IDLE_TRAY: SessionBusSnapshot["tray_meeting"] = {
  active: false,
  paused: false,
  summarizing: false,
  meeting_id: null,
  title: null,
  elapsed_ms: 0,
};

function makeIdleSnapshot(seq = 0, nowMs = Date.now()): SessionBusSnapshot {
  return {
    seq,
    server_now_ms: nowMs,
    dictation: { phase: "idle" },
    meeting: { phase: "idle" },
    detected_call: null,
    transient_notice: null,
    overlay: { mode: "hidden" },
    tray_meeting: IDLE_TRAY,
  };
}

async function installTauriWave4Mock(
  page: Page,
  initialLanguage = "en",
): Promise<void> {
  await page.addInitScript(
    ({ lang }) => {
      const calls: IpcCallRecord[] = [];
      const listeners = new Map<string, number[]>();
      const callbacks = new Map<number, (data: unknown) => unknown>();
      let nextCallbackId = 1;

      let currentSnapshot: SessionBusSnapshot = {
        seq: 0,
        server_now_ms: Date.now(),
        dictation: { phase: "idle" },
        meeting: { phase: "idle" },
        detected_call: null,
        transient_notice: null,
        overlay: { mode: "hidden" },
        tray_meeting: {
          active: false,
          paused: false,
          summarizing: false,
          meeting_id: null,
          title: null,
          elapsed_ms: 0,
        },
      };

      let liveMeetingState = {
        status: "idle" as "idle" | "recording" | "paused" | "summarizing",
        active_meeting_id: null as string | null,
        title: "",
        elapsed_ms: 0,
        capture_mode: "mic_and_system_audio" as const,
        app_name: null as string | null,
        rough_notes: "",
        recent_segments: [],
        rollup_count: 0,
        detected_call: null as {
          call_id: string;
          app_bundle_id: string;
          app_name: string;
          output_route: "headphones";
          detected_at_ms: number;
        } | null,
      };

      const archivedMeetings: Array<{
        id: string;
        title: string;
        template: "general";
        status: "completed";
        capture_mode: "mic_and_system_audio";
        app_bundle_id: string | null;
        app_name: string | null;
        started_at: number;
        ended_at: number | null;
        duration_ms: number;
        summary_preview: string;
        segment_count: number;
      }> = [];

      function registerCallback(
        callback: (data: unknown) => unknown,
        once = false,
      ): number {
        const id = nextCallbackId++;
        callbacks.set(id, (data: unknown) => {
          if (once) {
            callbacks.delete(id);
          }
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
          currentWindow: { label: "recording_overlay" },
          currentWebview: {
            windowLabel: "recording_overlay",
            label: "recording_overlay",
          },
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
          if (cmd === "plugin:event|unlisten") {
            const eventName = String(args.event ?? "");
            const eventId = Number(args.eventId ?? args.id);
            const list = listeners.get(eventName);
            if (list) {
              const idx = list.indexOf(eventId);
              if (idx !== -1) list.splice(idx, 1);
            }
            return null;
          }
          if (cmd === "plugin:event|emit") {
            emitEvent(String(args.event ?? ""), args.payload);
            return null;
          }

          calls.push({
            cmd,
            args: JSON.parse(JSON.stringify(args)) as Record<string, unknown>,
          });

          switch (cmd) {
            case "plugin:os|locale":
              return lang;
            case "plugin:macos-permissions|check_accessibility_permission":
            case "plugin:macos-permissions|check_microphone_permission":
              return true;
            case "get_app_settings":
            case "get_default_settings":
              return {
                app_language: lang,
                overlay_position: "bottom",
                onboarding_completed: true,
                selected_model: "parakeet-tdt-0.6b-v3",
                app_theme: "dark",
                bindings: {},
              };
            case "get_session_bus_state_json":
              return JSON.stringify(currentSnapshot);
            case "undo_cancel_recording":
              return true;
            case "confirm_cancel_recording":
              return true;
            case "set_meeting_pill_dismissed":
              return true;
            case "open_meetings_workspace":
              return null;
            case "dismiss_detected_call":
              return null;
            case "start_meeting_notes": {
              const now = Date.now();
              liveMeetingState = {
                ...liveMeetingState,
                status: "recording",
                active_meeting_id: "mtg-live-1",
                title: "Design Sync",
                elapsed_ms: 252_000, // 04:12
                app_name: "zoom.us",
                detected_call: null,
              };
              const nextSnap: SessionBusSnapshot = {
                seq: currentSnapshot.seq + 1,
                server_now_ms: now,
                dictation: { phase: "idle" },
                meeting: {
                  phase: "recording",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  started_at_ms: now - 252_000,
                  accumulated_paused_ms: 0,
                  capture_mode: "mic_and_system_audio",
                  app_name: "zoom.us",
                },
                detected_call: null,
                transient_notice: null,
                overlay: {
                  mode: "meeting_active",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  paused: false,
                  elapsed_ms: 252_000,
                  started_at_ms: now - 252_000,
                  accumulated_paused_ms: 0,
                  paused_at_ms: null,
                  app_name: "zoom.us",
                },
                tray_meeting: {
                  active: true,
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  paused: false,
                  summarizing: false,
                  elapsed_ms: 252_000,
                },
              };
              currentSnapshot = nextSnap;
              emitEvent("session-bus-state", nextSnap);
              return liveMeetingState;
            }
            case "pause_meeting_notes": {
              const now = Date.now();
              liveMeetingState = {
                ...liveMeetingState,
                status: "paused",
                elapsed_ms: 254_000,
              };
              const nextSnap: SessionBusSnapshot = {
                ...currentSnapshot,
                seq: currentSnapshot.seq + 1,
                server_now_ms: now,
                meeting: {
                  phase: "paused",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  started_at_ms: now - 254_000,
                  paused_at_ms: now,
                  accumulated_paused_ms: 0,
                  capture_mode: "mic_and_system_audio",
                  app_name: "zoom.us",
                },
                overlay: {
                  mode: "meeting_active",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  paused: true,
                  elapsed_ms: 254_000,
                  started_at_ms: now - 254_000,
                  accumulated_paused_ms: 0,
                  paused_at_ms: now,
                  app_name: "zoom.us",
                },
                tray_meeting: {
                  active: true,
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  paused: true,
                  summarizing: false,
                  elapsed_ms: 254_000,
                },
              };
              currentSnapshot = nextSnap;
              emitEvent("session-bus-state", nextSnap);
              return liveMeetingState;
            }
            case "resume_meeting_notes": {
              const now = Date.now();
              liveMeetingState = {
                ...liveMeetingState,
                status: "recording",
                elapsed_ms: 254_000,
              };
              const nextSnap: SessionBusSnapshot = {
                ...currentSnapshot,
                seq: currentSnapshot.seq + 1,
                server_now_ms: now,
                meeting: {
                  phase: "recording",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  started_at_ms: now - 256_000,
                  accumulated_paused_ms: 2_000,
                  capture_mode: "mic_and_system_audio",
                  app_name: "zoom.us",
                },
                overlay: {
                  mode: "meeting_active",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  paused: false,
                  elapsed_ms: 254_000,
                  started_at_ms: now - 256_000,
                  accumulated_paused_ms: 2_000,
                  paused_at_ms: null,
                  app_name: "zoom.us",
                },
                tray_meeting: {
                  active: true,
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  paused: false,
                  summarizing: false,
                  elapsed_ms: 254_000,
                },
              };
              currentSnapshot = nextSnap;
              emitEvent("session-bus-state", nextSnap);
              return liveMeetingState;
            }
            case "stop_meeting_notes": {
              const now = Date.now();
              const summarizingSnap: SessionBusSnapshot = {
                ...currentSnapshot,
                seq: currentSnapshot.seq + 1,
                server_now_ms: now,
                meeting: {
                  phase: "summarizing",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                },
                overlay: {
                  mode: "meeting_summarizing",
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                },
                tray_meeting: {
                  active: true,
                  meeting_id: "mtg-live-1",
                  title: "Design Sync",
                  paused: false,
                  summarizing: true,
                  elapsed_ms: 0,
                },
              };
              currentSnapshot = summarizingSnap;
              emitEvent("session-bus-state", summarizingSnap);
              return {
                id: "mtg-live-1",
                title: "Design Sync",
                template: "general",
                status: "completed",
                capture_mode: "mic_and_system_audio",
                app_bundle_id: "us.zoom.xos",
                app_name: "zoom.us",
                started_at: now - 255_000,
                ended_at: now,
                duration_ms: 255_000,
                rough_notes: "",
                summary_markdown: "# Summary",
                audio_path: null,
                error_message: null,
                created_at: now - 255_000,
                updated_at: now,
                segments: [],
              };
            }
            case "get_meeting_live_state":
              return liveMeetingState;
            case "list_meetings":
              return archivedMeetings;
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
            case "get_available_models":
              return [];
            case "get_current_model":
              return "parakeet-tdt-0.6b-v3";
            case "initialize_enigo":
            case "initialize_shortcuts":
            case "cancel_operation":
            case "pause_recording":
            case "resume_recording":
            case "finish_recording":
              return true;
            default:
              return null;
          }
        },
      };

      window.__wave4Calls = calls;
      window.__wave4Emit = emitEvent;
      window.__wave4SetSnapshot = (snapshot: SessionBusSnapshot) => {
        currentSnapshot = snapshot;
        if (snapshot.meeting.phase === "idle") {
          archivedMeetings.splice(0, archivedMeetings.length, {
            id: "mtg-finished-1",
            title: "Design Sync",
            template: "general",
            status: "completed",
            capture_mode: "mic_and_system_audio",
            app_bundle_id: "us.zoom.xos",
            app_name: "zoom.us",
            started_at: snapshot.server_now_ms - 180_000,
            ended_at: snapshot.server_now_ms,
            duration_ms: 180_000,
            summary_preview: "Shipped Wave 4A floating controls.",
            segment_count: 6,
          });
        }
        emitEvent("session-bus-state", snapshot);
      };
    },
    { lang: initialLanguage },
  );
}

test.describe("Wave 4A — SessionBus pure timing helpers", () => {
  test("computeLiveMeetingElapsedMs advances smoothly without pause/resume drift", () => {
    const activeOverlay: Extract<OverlayProjection, { mode: "meeting_active" }> =
      {
        mode: "meeting_active",
        meeting_id: "m-1",
        title: "Standup",
        paused: false,
        elapsed_ms: 60_000,
        started_at_ms: 100_000,
        accumulated_paused_ms: 15_000,
        paused_at_ms: null,
        app_name: "zoom.us",
      };

    // clientNowMs (170_000) + skew (5_000) = serverNow (175_000) -> 175_000 - 100_000 - 15_000 = 60_000
    expect(computeLiveMeetingElapsedMs(activeOverlay, 170_000, 5_000)).toBe(
      60_000,
    );
    // 3 seconds later -> 63_000ms
    expect(computeLiveMeetingElapsedMs(activeOverlay, 173_000, 5_000)).toBe(
      63_000,
    );

    const pausedOverlay: Extract<OverlayProjection, { mode: "meeting_active" }> =
      {
        ...activeOverlay,
        paused: true,
        paused_at_ms: 178_000,
        elapsed_ms: 63_000,
      };
    // Frozen at 63_000ms regardless of later client clock ticks
    expect(computeLiveMeetingElapsedMs(pausedOverlay, 250_000, 5_000)).toBe(
      63_000,
    );
  });

  test("computeSoftCancelProgress clamps cleanly from 1.0 down to 0.0", () => {
    expect(computeSoftCancelProgress(1_000, 6_000, 5_000, 900)).toBe(1);
    expect(computeSoftCancelProgress(1_000, 6_000, 5_000, 3_500)).toBeCloseTo(
      0.5,
      5,
    );
    expect(computeSoftCancelProgress(1_000, 6_000, 5_000, 6_000)).toBe(0);
    expect(computeSoftCancelProgress(1_000, 6_000, 5_000, 7_500)).toBe(0);
  });
});

test.describe("Wave 4A — Floating Overlay UX Controls (Headless)", () => {
  test("1. Escape Soft-Cancel -> Countdown Bar -> Undo -> Resumes active locked recording", async ({
    page,
  }) => {
    await installTauriWave4Mock(page);
    await page.goto("/src/overlay/index.html");

    // Start locked dictation recording (takeId = 42)
    await page.evaluate((idleTray) => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 1,
        server_now_ms: now,
        dictation: {
          phase: "recording",
          take_id: 42,
          started_at_ms: now - 1_200,
          locked: true,
          paused: false,
        },
        meeting: { phase: "idle" },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "dictation_recording",
          take_id: 42,
          locked: true,
          paused: false,
        },
        tray_meeting: idleTray,
      });
      window.__wave4Emit("mic-level", new Array(16).fill(0.45));
    }, IDLE_TRAY);

    await expect(page.getByTestId("overlay-pause-resume-btn")).toBeVisible();
    await expect(page.getByTestId("overlay-done-btn")).toBeVisible();

    // Trigger Escape soft-cancel with grace window
    await page.evaluate((idleTray) => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 2,
        server_now_ms: now,
        dictation: {
          phase: "soft_cancelled",
          take_id: 42,
          cancelled_at_ms: now,
          expires_at_ms: now + 2_500,
          grace_ms: 2_500,
        },
        meeting: { phase: "idle" },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "dictation_soft_cancelled",
          take_id: 42,
          cancelled_at_ms: now,
          expires_at_ms: now + 2_500,
          grace_ms: 2_500,
        },
        tray_meeting: idleTray,
      });
    }, IDLE_TRAY);

    const pill = page.getByTestId("overlay-soft-cancel-pill");
    await expect(pill).toBeVisible();
    await expect(pill).toContainText("Transcript cancelled");

    const undoBtn = page.getByTestId("overlay-undo-cancel-btn");
    await expect(undoBtn).toBeVisible();
    await expect(undoBtn).toHaveAttribute("aria-label", "Undo");

    const progress = page.getByTestId("overlay-soft-cancel-progress");
    await expect(progress).toHaveAttribute("role", "progressbar");
    const initialNow = Number(await progress.getAttribute("aria-valuenow"));
    expect(initialNow).toBeGreaterThan(60);

    // Wait briefly and verify countdown progress bar decreases smoothly
    await expect
      .poll(async () => Number(await progress.getAttribute("aria-valuenow")), {
        timeout: 2_000,
      })
      .toBeLessThan(initialNow);

    // Click [Undo] -> invokes undo_cancel_recording({ takeId: 42 }) & restores locked recording
    await undoBtn.click();

    const undoCall = await page.evaluate(() =>
      window.__wave4Calls.find((c) => c.cmd === "undo_cancel_recording"),
    );
    expect(undoCall).toBeTruthy();
    expect(undoCall?.args).toEqual({ takeId: 42 });

    // Emit resumed SessionBus snapshot from backend
    await page.evaluate((idleTray) => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 3,
        server_now_ms: now,
        dictation: {
          phase: "recording",
          take_id: 42,
          started_at_ms: now - 1_800,
          locked: true,
          paused: false,
        },
        meeting: { phase: "idle" },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "dictation_recording",
          take_id: 42,
          locked: true,
          paused: false,
        },
        tray_meeting: idleTray,
      });
    }, IDLE_TRAY);

    await expect(pill).toHaveCount(0);
    await expect(page.getByTestId("overlay-pause-resume-btn")).toBeVisible();
    await expect(page.getByTestId("overlay-done-btn")).toBeVisible();
  });

  test("2. Escape Soft-Cancel -> [×] Dismiss & Countdown Expiry invoke confirm_cancel_recording and hide pill", async ({
    page,
  }) => {
    await installTauriWave4Mock(page);
    await page.goto("/src/overlay/index.html");

    // Case A: Explicit [×] Dismiss
    await page.evaluate((idleTray) => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 1,
        server_now_ms: now,
        dictation: {
          phase: "soft_cancelled",
          take_id: 77,
          cancelled_at_ms: now,
          expires_at_ms: now + 5_000,
          grace_ms: 5_000,
        },
        meeting: { phase: "idle" },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "dictation_soft_cancelled",
          take_id: 77,
          cancelled_at_ms: now,
          expires_at_ms: now + 5_000,
          grace_ms: 5_000,
        },
        tray_meeting: idleTray,
      });
    }, IDLE_TRAY);

    await expect(page.getByTestId("overlay-soft-cancel-pill")).toBeVisible();
    await page.getByTestId("overlay-dismiss-cancel-btn").click();

    await expect(page.getByTestId("overlay-soft-cancel-pill")).toHaveCount(0);
    const dismissCall = await page.evaluate(() =>
      window.__wave4Calls.find((c) => c.cmd === "confirm_cancel_recording"),
    );
    expect(dismissCall?.args).toEqual({ takeId: 77 });

    // Case B: Automatic countdown expiry
    await page.evaluate((idleTray) => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 2,
        server_now_ms: now,
        dictation: {
          phase: "soft_cancelled",
          take_id: 78,
          cancelled_at_ms: now,
          expires_at_ms: now + 280,
          grace_ms: 280,
        },
        meeting: { phase: "idle" },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "dictation_soft_cancelled",
          take_id: 78,
          cancelled_at_ms: now,
          expires_at_ms: now + 280,
          grace_ms: 280,
        },
        tray_meeting: idleTray,
      });
    }, IDLE_TRAY);

    await expect(page.getByTestId("overlay-soft-cancel-pill")).toHaveCount(0, {
      timeout: 2_500,
    });
    const expiryCall = await page.evaluate(() =>
      window.__wave4Calls
        .filter((c) => c.cmd === "confirm_cancel_recording")
        .pop(),
    );
    expect(expiryCall?.args).toEqual({ takeId: 78 });
  });

  test("3. Call Detected -> [Start Notes] (open_workspace: false) -> Active Meeting Pill Pause / Resume / Open Notes / Minimize / Stop", async ({
    page,
  }) => {
    await installTauriWave4Mock(page);
    await page.goto("/src/overlay/index.html");

    // Emit call_detected projection matching lowyap_ports::OverlayProjection::CallDetected
    await page.evaluate((idleTray) => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 1,
        server_now_ms: now,
        dictation: { phase: "idle" },
        meeting: { phase: "idle" },
        detected_call: {
          call_id: "zoom-call-9",
          app_name: "zoom.us",
          output_route: "headphones",
        },
        transient_notice: null,
        overlay: {
          mode: "call_detected",
          call_id: "zoom-call-9",
          app_name: "zoom.us",
          output_route: "headphones",
        },
        tray_meeting: idleTray,
      });
    }, IDLE_TRAY);

    const startNotesBtn = page.getByRole("button", { name: "Start Notes" });
    await expect(startNotesBtn).toBeVisible();
    await startNotesBtn.click();

    // Verify zero focus steal: open_workspace is explicitly false
    const startCall = await page.evaluate(() =>
      window.__wave4Calls.find((c) => c.cmd === "start_meeting_notes"),
    );
    expect(startCall).toBeTruthy();
    expect(startCall?.args.open_workspace).toBe(false);
    expect(startCall?.args.openWorkspace).toBe(false);

    // Active Meeting Pill is visible with MM:SS, [Pause], [Stop], [↗], [–]
    const meetingPill = page.getByTestId("overlay-meeting-active-pill");
    await expect(meetingPill).toBeVisible();
    await expect(page.getByTestId("overlay-meeting-timer")).toHaveText(
      /^04:1[2-4]$/,
    );

    const pauseResumeBtn = page.getByTestId("overlay-meeting-pause-resume-btn");
    const stopBtn = page.getByTestId("overlay-meeting-stop-btn");
    const openNotesBtn = page.getByTestId("overlay-meeting-open-notes-btn");
    const minimizeBtn = page.getByTestId("overlay-meeting-minimize-btn");

    await expect(pauseResumeBtn).toContainText("Pause");
    await expect(stopBtn).toContainText("Stop");
    await expect(openNotesBtn).toHaveAttribute(
      "aria-label",
      "Open meeting notes",
    );
    await expect(minimizeBtn).toHaveAttribute(
      "aria-label",
      "Minimize to menu bar",
    );

    // Click [↗] Open Notes -> calls open_meetings_workspace explicitly
    await openNotesBtn.click();
    expect(
      await page.evaluate(() =>
        window.__wave4Calls.some((c) => c.cmd === "open_meetings_workspace"),
      ),
    ).toBe(true);

    // Click [Pause] -> calls pause_meeting_notes and switches to [Resume] + Paused badge
    await pauseResumeBtn.click();
    await expect(pauseResumeBtn).toContainText("Resume");
    await expect(meetingPill).toContainText("Paused");
    expect(
      await page.evaluate(() =>
        window.__wave4Calls.some((c) => c.cmd === "pause_meeting_notes"),
      ),
    ).toBe(true);

    // Click [Resume] -> calls resume_meeting_notes and returns to [Pause]
    await pauseResumeBtn.click();
    await expect(pauseResumeBtn).toContainText("Pause");
    expect(
      await page.evaluate(() =>
        window.__wave4Calls.some((c) => c.cmd === "resume_meeting_notes"),
      ),
    ).toBe(true);

    // Click [–] Minimize -> calls set_meeting_pill_dismissed({ dismissed: true }) and hides pill
    await minimizeBtn.click();
    await expect(meetingPill).toHaveCount(0);
    const dismissPillCall = await page.evaluate(() =>
      window.__wave4Calls.find((c) => c.cmd === "set_meeting_pill_dismissed"),
    );
    expect(dismissPillCall?.args).toEqual({ dismissed: true });

    // Restore pill from tray (`set_meeting_pill_dismissed(false)` emitted by backend) and test [Stop]
    await page.evaluate(() => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 10,
        server_now_ms: now,
        dictation: { phase: "idle" },
        meeting: {
          phase: "recording",
          meeting_id: "mtg-live-1",
          title: "Design Sync",
          started_at_ms: now - 260_000,
          accumulated_paused_ms: 2_000,
          capture_mode: "mic_and_system_audio",
          app_name: "zoom.us",
        },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "meeting_active",
          meeting_id: "mtg-live-1",
          title: "Design Sync",
          paused: false,
          elapsed_ms: 258_000,
          started_at_ms: now - 260_000,
          accumulated_paused_ms: 2_000,
          paused_at_ms: null,
          app_name: "zoom.us",
        },
        tray_meeting: {
          active: true,
          meeting_id: "mtg-live-1",
          title: "Design Sync",
          paused: false,
          summarizing: false,
          elapsed_ms: 258_000,
        },
      });
    });

    await expect(meetingPill).toBeVisible();
    await stopBtn.click();

    expect(
      await page.evaluate(() =>
        window.__wave4Calls.some((c) => c.cmd === "stop_meeting_notes"),
      ),
    ).toBe(true);
    const summarizingPill = page.getByTestId(
      "overlay-meeting-summarizing-pill",
    );
    await expect(summarizingPill).toBeVisible();
    await expect(summarizingPill).toContainText("Saving notes…");
  });

  test("4. Dictation-over-Active-Meeting Restoration + stale seq & hide-overlay guards", async ({
    page,
  }) => {
    await installTauriWave4Mock(page);
    await page.goto("/src/overlay/index.html");

    // 1. Active meeting is recording (seq = 10)
    await page.evaluate(() => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 10,
        server_now_ms: now,
        dictation: { phase: "idle" },
        meeting: {
          phase: "recording",
          meeting_id: "mtg-harmony-1",
          title: "Product Review",
          started_at_ms: now - 125_000,
          accumulated_paused_ms: 0,
          capture_mode: "mic_and_system_audio",
          app_name: "Microsoft Teams",
        },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "meeting_active",
          meeting_id: "mtg-harmony-1",
          title: "Product Review",
          paused: false,
          elapsed_ms: 125_000,
          started_at_ms: now - 125_000,
          accumulated_paused_ms: 0,
          paused_at_ms: null,
          app_name: "Microsoft Teams",
        },
        tray_meeting: {
          active: true,
          meeting_id: "mtg-harmony-1",
          title: "Product Review",
          paused: false,
          summarizing: false,
          elapsed_ms: 125_000,
        },
      });
    });

    await expect(page.getByTestId("overlay-meeting-active-pill")).toBeVisible();

    // 2. Quick dictation starts on top of active meeting (seq = 11)
    await page.evaluate(() => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 11,
        server_now_ms: now,
        dictation: {
          phase: "recording",
          take_id: 99,
          started_at_ms: now,
          locked: true,
          paused: false,
        },
        meeting: {
          phase: "recording",
          meeting_id: "mtg-harmony-1",
          title: "Product Review",
          started_at_ms: now - 126_000,
          accumulated_paused_ms: 0,
          capture_mode: "mic_and_system_audio",
          app_name: "Microsoft Teams",
        },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "dictation_recording",
          take_id: 99,
          locked: true,
          paused: false,
        },
        tray_meeting: {
          active: true,
          meeting_id: "mtg-harmony-1",
          title: "Product Review",
          paused: false,
          summarizing: false,
          elapsed_ms: 126_000,
        },
      });
      window.__wave4Emit("mic-level", new Array(16).fill(0.5));
    });

    await expect(page.getByTestId("overlay-meeting-active-pill")).toHaveCount(
      0,
    );
    await expect(page.getByTestId("overlay-done-btn")).toBeVisible();

    // 3. Dictation is soft-cancelled (seq = 12), while meeting remains active underneath
    await page.evaluate(() => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 12,
        server_now_ms: now,
        dictation: {
          phase: "soft_cancelled",
          take_id: 99,
          cancelled_at_ms: now,
          expires_at_ms: now + 5_000,
          grace_ms: 5_000,
        },
        meeting: {
          phase: "recording",
          meeting_id: "mtg-harmony-1",
          title: "Product Review",
          started_at_ms: now - 127_000,
          accumulated_paused_ms: 0,
          capture_mode: "mic_and_system_audio",
          app_name: "Microsoft Teams",
        },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "dictation_soft_cancelled",
          take_id: 99,
          cancelled_at_ms: now,
          expires_at_ms: now + 5_000,
          grace_ms: 5_000,
        },
        tray_meeting: {
          active: true,
          meeting_id: "mtg-harmony-1",
          title: "Product Review",
          paused: false,
          summarizing: false,
          elapsed_ms: 127_000,
        },
      });
    });

    await expect(page.getByTestId("overlay-soft-cancel-pill")).toBeVisible();

    // 4. Dismiss soft-cancel -> automatically restores `meeting_active` instead of vanishing!
    await page.getByTestId("overlay-dismiss-cancel-btn").click();
    await expect(page.getByTestId("overlay-soft-cancel-pill")).toHaveCount(0);
    await expect(page.getByTestId("overlay-meeting-active-pill")).toBeVisible();

    // 5. Emit legacy `hide-overlay` AND an out-of-order stale `seq: 8` hidden snapshot:
    // neither may hide the active meeting pill!
    await page.evaluate((staleSnap) => {
      window.__wave4SetSnapshot(staleSnap);
      window.__wave4Emit("hide-overlay", null);
    }, makeIdleSnapshot(8));

    await expect(page.getByTestId("overlay-meeting-active-pill")).toBeVisible();
  });

  test("5. MeetingsWorkspace syncs instantaneously with session-bus-state events", async ({
    page,
  }) => {
    await installTauriWave4Mock(page);
    await page.goto("/");

    // Navigate main window to Meetings workspace
    await page.evaluate(() => {
      window.__wave4Emit("navigate-section", "meetings");
    });

    // Emit active recording on SessionBus (seq = 20)
    await page.evaluate(() => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 20,
        server_now_ms: now,
        dictation: { phase: "idle" },
        meeting: {
          phase: "recording",
          meeting_id: "mtg-sync-1",
          title: "Architecture Sync",
          started_at_ms: now - 65_000,
          accumulated_paused_ms: 0,
          capture_mode: "mic_and_system_audio",
          app_name: "zoom.us",
        },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "meeting_active",
          meeting_id: "mtg-sync-1",
          title: "Architecture Sync",
          paused: false,
          elapsed_ms: 65_000,
          started_at_ms: now - 65_000,
          accumulated_paused_ms: 0,
          paused_at_ms: null,
          app_name: "zoom.us",
        },
        tray_meeting: {
          active: true,
          meeting_id: "mtg-sync-1",
          title: "Architecture Sync",
          paused: false,
          summarizing: false,
          elapsed_ms: 65_000,
        },
      });
    });

    await expect(page.getByRole("button", { name: "Pause" })).toBeVisible();

    // Pausing from the floating pill/menu bar updates MeetingsWorkspace immediately (seq = 21)
    await page.evaluate(() => {
      const now = Date.now();
      window.__wave4SetSnapshot({
        seq: 21,
        server_now_ms: now,
        dictation: { phase: "idle" },
        meeting: {
          phase: "paused",
          meeting_id: "mtg-sync-1",
          title: "Architecture Sync",
          started_at_ms: now - 68_000,
          paused_at_ms: now,
          accumulated_paused_ms: 0,
          capture_mode: "mic_and_system_audio",
          app_name: "zoom.us",
        },
        detected_call: null,
        transient_notice: null,
        overlay: {
          mode: "meeting_active",
          meeting_id: "mtg-sync-1",
          title: "Architecture Sync",
          paused: true,
          elapsed_ms: 68_000,
          started_at_ms: now - 68_000,
          accumulated_paused_ms: 0,
          paused_at_ms: now,
          app_name: "zoom.us",
        },
        tray_meeting: {
          active: true,
          meeting_id: "mtg-sync-1",
          title: "Architecture Sync",
          paused: true,
          summarizing: false,
          elapsed_ms: 68_000,
        },
      });
    });

    await expect(page.getByRole("button", { name: "Resume" })).toBeVisible();

    // Completing summary -> idle refreshes archive list automatically (seq = 22)
    await page.evaluate((idleSnap) => {
      window.__wave4SetSnapshot(idleSnap);
    }, makeIdleSnapshot(22));

    await expect(page.getByText("Design Sync")).toBeVisible();
    await expect(page.getByText("6 segments")).toBeVisible();
  });

  test("6. RTL locale (Arabic), prefers-reduced-motion countdown, and legacy show-overlay fallback", async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await installTauriWave4Mock(page, "ar");
    await page.goto("/src/overlay/index.html");

    // Legacy "show-overlay" === "soft_cancelled" works even without a prior SessionBus snapshot
    await page.evaluate(() => {
      window.__wave4Emit("show-overlay", "soft_cancelled");
    });

    const softCancelPill = page.getByTestId("overlay-soft-cancel-pill");
    await expect(softCancelPill).toBeVisible();
    await expect(page.locator(".ov-stage")).toHaveAttribute("dir", "rtl");
    await expect(softCancelPill).toContainText("تم إلغاء التفريغ");
    await expect(page.getByTestId("overlay-undo-cancel-btn")).toContainText(
      "تراجع",
    );

    // Reduced-motion stepped interval still updates aria-valuenow downward
    const progress = page.getByTestId("overlay-soft-cancel-progress");
    await expect
      .poll(async () => Number(await progress.getAttribute("aria-valuenow")), {
        timeout: 2_000,
      })
      .toBeLessThan(100);

    // Legacy "show-overlay" === "meeting_summarizing" renders localized Arabic saving label
    await page.evaluate(() => {
      window.__wave4Emit("show-overlay", "meeting_summarizing");
    });
    await expect(
      page.getByTestId("overlay-meeting-summarizing-pill"),
    ).toContainText("جارٍ حفظ الملاحظات…");
  });
});
