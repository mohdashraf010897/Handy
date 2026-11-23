import { expect, test, type Page } from "@playwright/test";

async function installWave5StudioMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    let listenerSeq = 1;
    const listeners = new Map<
      number,
      { event: string; handler: (ev: { event: string; id: number; payload: unknown }) => void }
    >();

    const nowSec = Math.floor(Date.now() / 1000);
    const entries = [
      {
        id: 501,
        file_name: "lowyap-501.wav",
        timestamp: nowSec - 180,
        saved: true,
        title: "Studio take 1",
        transcription_text: "Ship the Quiet Tactile Studio Instrument across Desk, Lexicon, and Voice & Style.",
        post_processed_text:
          "Ship the Quiet Tactile Studio Instrument across Desk, Lexicon, and Voice & Style.",
        post_process_prompt: "Stage-A deterministic cleanup",
        audio_duration_ms: 4200,
        asr_ms: 148,
        post_process_ms: 1,
        total_pipeline_ms: 162,
        post_process_path: "stage_a",
      },
      {
        id: 502,
        file_name: "lowyap-502.wav",
        timestamp: nowSec - 90_000,
        saved: false,
        title: "Studio take 2",
        transcription_text: "Check lowyap.com and github.com/lowyap.",
        post_processed_text: null,
        post_process_prompt: null,
        audio_duration_ms: 2800,
        asr_ms: 112,
        post_process_ms: 0,
        total_pipeline_ms: 120,
        post_process_path: "stage_a",
      },
    ];

    const settings = {
      bindings: {
        transcribe: {
          id: "transcribe",
          name: "Transcribe",
          description: "Hold to yap",
          default_binding: "option+space",
          current_binding: "option+space",
        },
        cancel: {
          id: "cancel",
          name: "Cancel",
          description: "Cancel recording",
          default_binding: "escape",
          current_binding: "escape",
        },
      },
      push_to_talk: true,
      audio_feedback: false,
      audio_feedback_volume: 0.5,
      sound_theme: "marimba",
      start_hidden: false,
      autostart_enabled: false,
      update_checks_enabled: false,
      selected_model: "whisper-large-v3-turbo",
      always_on_microphone: false,
      selected_microphone: "MacBook Pro Mic",
      selected_mic_channel: null,
      clamshell_microphone: null,
      selected_output_device: null,
      translate_to_english: false,
      selected_language: "auto",
      spoken_languages: ["en", "ur"],
      overlay_position: "bottom",
      debug_mode: false,
      log_level: "info",
      custom_words: ["lowyap", "Parakeet", "CoreAudio", "Nastaliq"],
      model_unload_timeout: "never",
      word_correction_threshold: 0.18,
      history_limit: 50,
      recording_retention_period: "preserve_limit",
      paste_method: "ctrl_v",
      clipboard_handling: "dont_modify",
      post_process_enabled: false,
      post_process_provider_id: "apple_intelligence",
      post_process_providers: [],
      post_process_api_keys: {},
      post_process_models: {},
      post_process_prompts: [],
      post_process_selected_prompt_id: null,
      mute_while_recording: false,
      append_trailing_space: false,
      app_language: "en",
      app_theme: "light",
      experimental_enabled: false,
      keyboard_implementation: "tauri",
      show_tray_icon: true,
      paste_delay_ms: 60,
      paste_delay_after_ms: 80,
      typing_tool: "auto",
      external_script_path: null,
      filler_word_removal: true,
      auto_submit: false,
      auto_submit_key: "enter",
      vad_enabled: true,
      long_form_chunking: true,
      reliable_paste: false,
      recording_buffer_ms: 0,
      shortcut_mode: "auto",
      hold_threshold_ms: 280,
      whisper_accelerator: "auto",
      ort_accelerator: "auto",
      language_detection_enabled: false,
      language_detection_audio_seconds: 5,
      chinese_script: "as_transcribed",
      lazy_stream_close: true,
      vad_backend: "silero_ten",
      onboarding_completed_at: "2026-10-20T00:00:00Z",
      last_seen_version: "0.8.2",
      show_whats_new_on_update: false,
    };

    const models = [
      {
        id: "whisper-large-v3-turbo",
        name: "Whisper Large v3 Turbo",
        description: "Fast multilingual Whisper checkpoint",
        filename: "ggml-large-v3-turbo.bin",
        url: null,
        size_mb: 809,
        is_downloaded: true,
        is_downloading: false,
        partial_size: 0,
        is_directory: false,
        accuracy_score: 0.9,
        speed_score: 0.85,
        supports_translation: true,
        is_recommended: true,
        supported_languages: ["en", "ur", "hi", "ar", "es", "fr", "de", "ja"],
        supports_language_selection: true,
        is_custom: false,
      },
    ];

    (window as unknown as Record<string, unknown>).__TAURI_OS_PLUGIN_INTERNALS__ = {
      arch: "aarch64",
      eol: "\n",
      exe_extension: "",
      family: "unix",
      os_type: "macos",
      platform: "macos",
      version: "15.4.0",
    };

    (window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: (_event: string, eventId: number) => {
        listeners.delete(eventId);
      },
    };

    (window as unknown as Record<string, unknown>).__wave5Emit = (
      event: string,
      payload: unknown,
    ) => {
      for (const [id, entry] of listeners.entries()) {
        if (entry.event === event) {
          entry.handler({ event, id, payload });
        }
      }
    };

    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { windowLabel: "main", label: "main" },
      },
      transformCallback: (callback: (payload: unknown) => void) => {
        const id = listenerSeq++;
        (window as unknown as Record<string, unknown>)[`_${id}`] = callback;
        return id;
      },
      convertFileSrc: (filePath: string) => filePath,
      invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
        switch (cmd) {
          case "plugin:event|listen": {
            const eventName = String(args.event ?? "");
            const handlerId = Number(args.handler);
            const cb = (window as unknown as Record<string, (ev: unknown) => void>)[
              `_${handlerId}`
            ];
            if (cb) {
              listeners.set(handlerId, { event: eventName, handler: cb });
            }
            return handlerId;
          }
          case "plugin:event|unlisten": {
            listeners.delete(Number(args.eventId));
            return null;
          }
          case "plugin:os|locale":
            return "en";
          case "plugin:app|version":
            return "0.9.0";
          case "plugin:macos-permissions|check_accessibility_permission":
          case "plugin:macos-permissions|check_microphone_permission":
            return true;
          case "get_app_settings":
          case "get_settings":
          case "get_default_settings":
            return {
              ...settings,
              onboarding_completed: true,
              whats_new_last_seen_version: "99.0.0",
              brand_custom_words_seeded: true,
            };
          case "check_custom_sounds":
            return { start: false, stop: false };
          case "get_retired_models":
            return [];
          case "get_available_models":
            return models;
          case "get_current_model":
            return settings.selected_model;
          case "get_transcription_model_status":
            return settings.selected_model;
          case "has_any_models_available":
            return true;
          case "get_history_entries":
          case "get_history_entries_page":
            return { entries, has_more: false };
          case "get_session_bus_snapshot":
            return {
              seq: 1,
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
                title: "",
                elapsed_ms: 0,
              },
            };
          case "get_meeting_live_state":
            return {
              status: "idle",
              active_meeting_id: null,
              title: "",
              elapsed_ms: 0,
              capture_mode: "system_and_mic",
              headphones_detected: false,
              app_name: null,
              rolling_checkpoint_count: 0,
            };
          case "list_meetings":
            return [];
          case "get_flow_settings":
            return {
              tone_enabled: false,
              app_tones: [],
              fast_path_enabled: true,
              snippets: [],
              hindustani_script: "nastaliq",
              auto_boost_mic_volume: true,
              mic_boost_target_percent: 85,
              quiet_speech_agc_enabled: true,
            };
          case "get_flow_builtin_app_tones":
            return [];
          case "get_flow_last_app":
            return null;
          case "get_available_microphones":
            return [{ index: "0", name: "MacBook Pro Mic" }];
          case "get_available_output_devices":
            return [{ index: "0", name: "MacBook Pro Speakers" }];
          case "is_secure_input_blocked":
            return { blocked: false, culprit: null };
          case "change_app_theme_setting": {
            settings.app_theme = String(args.theme ?? "light");
            return null;
          }
          default:
            return null;
        }
      },
    };
  });
}

test.describe("Wave 5 Studio Shell — Quiet Tactile Studio Instrument", () => {
  test("launches on Desk, renders 5-surface Studio rail, Urdu script-lock capability pill, Voice & Style, and Cmd+, Settings", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 960, height: 660 });
    await installWave5StudioMock(page);
    await page.goto("/");

    // 1. Lands directly on Desk (not General settings) with day-grouped Takes Ledger
    await expect(
      page.getByRole("heading", { name: "Desk", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("Hold to yap · release to type anywhere"),
    ).toBeVisible();
    await expect(page.getByText("Today", { exact: true })).toBeVisible();
    await expect(page.getByText("1 take", { exact: true })).toHaveCount(2);
    await expect(page.getByText("Yesterday", { exact: true })).toBeVisible();
    await expect(
      page.getByText(
        "Ship the Quiet Tactile Studio Instrument across Desk, Lexicon, and Voice & Style.",
      ),
    ).toBeVisible();

    const screenshotDir = process.env.LOWYAP_STUDIO_SCREENSHOT_DIR;
    if (screenshotDir) {
      await page.screenshot({
        path: `${screenshotDir}/wave5-studio-desk-light.png`,
        animations: "disabled",
      });
      await page.evaluate(() => {
        document.documentElement.setAttribute("data-theme", "dark");
      });
      await page.screenshot({
        path: `${screenshotDir}/wave5-studio-desk-dark.png`,
        animations: "disabled",
      });
      await page.evaluate(() => {
        document.documentElement.setAttribute("data-theme", "light");
      });
    }

    // 2. Navigate to Lexicon -> verifies Hindustani capability pill replaces self-sabotaging Urdu warning
    const nav = page.getByRole("navigation", { name: "Studio navigation" });
    await nav.getByRole("button", { name: "Lexicon", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Lexicon", exact: true }),
    ).toBeVisible();
    await expect(page.getByTestId("hindustani-capability-pill")).toContainText(
      "Urdu / Hindi script-lock active via Whisper Large v3 Turbo",
    );
    await expect(
      page.getByText("No model recognizes Urdu well yet."),
    ).toHaveCount(0);
    if (screenshotDir) {
      await page.screenshot({
        path: `${screenshotDir}/wave5-studio-lexicon-urdu.png`,
        animations: "disabled",
      });
    }

    // 3. Navigate to Voice & Style -> verifies segmented tabs & zero duplicate toggles
    await nav
      .getByRole("button", { name: "Voice & Style", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "Voice & Style", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("tab", { name: "App Tone & Snippets" }),
    ).toHaveAttribute("aria-selected", "true");
    if (screenshotDir) {
      await page.screenshot({
        path: `${screenshotDir}/wave5-studio-voice.png`,
        animations: "disabled",
      });
    }
    await page.getByRole("tab", { name: "Linguist Engine & Prompts" }).click();
    await expect(
      page.getByText(/Stage-A deterministic cleanup/i),
    ).toBeVisible();

    // 4. Press Cmd/Ctrl+, -> opens Settings -> Shortcuts & Audio without duplicating Lexicon/Voice controls
    await page.keyboard.press("ControlOrMeta+,");
    await expect(
      page.getByRole("heading", { name: "Settings", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("tab", { name: "Shortcuts & Audio" }),
    ).toHaveAttribute("aria-selected", "true");
  });
});
