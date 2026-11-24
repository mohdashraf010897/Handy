import { expect, test, type Page } from "@playwright/test";
import type { SessionBusSnapshot } from "../src/lib/sessionBus";

interface IpcCallRecord {
  cmd: string;
  args: Record<string, unknown>;
}

declare global {
  interface Window {
    __wave6Calls: IpcCallRecord[];
    __wave6Emit: (event: string, payload: unknown) => void;
    __wave6SetPosition: (pos: "bottom" | "top") => void;
  }
}

async function installWave6OverlayMock(
  page: Page,
  initialPosition: "bottom" | "top" = "bottom",
): Promise<void> {
  await page.addInitScript(
    ({ pos }) => {
      const calls: IpcCallRecord[] = [];
      const listeners = new Map<string, number[]>();
      const callbacks = new Map<number, (data: unknown) => unknown>();
      let nextCallbackId = 1;
      let overlayPosition: "bottom" | "top" = pos;
      let postProcessEnabled = true;
      let selectedPromptId = "default_improve_transcriptions";

      const prompts = [
        {
          id: "default_improve_transcriptions",
          name: "Polish",
          prompt: "Clean this speech transcript...",
        },
        {
          id: "builtin_prompt_engineer",
          name: "Prompt Engineer",
          prompt: "Turn spoken instructions into a structured prompt...",
        },
        {
          id: "custom_concise_pr",
          name: "Concise PR Note",
          prompt: "Rewrite into crisp engineering PR notes: ${output}",
        },
      ];

      const idleSnapshot: SessionBusSnapshot = {
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
      win.__wave6Calls = calls;
      win.__wave6Emit = emitEvent;
      win.__wave6SetPosition = (nextPos: "bottom" | "top") => {
        overlayPosition = nextPos;
      };

      win.__TAURI_OS_PLUGIN_INTERNALS__ = {
        platform: "macos",
        arch: "aarch64",
        family: "unix",
        os_type: "macos",
        version: "15.4.0",
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
              return "en";
            case "get_session_bus_state_json":
              return JSON.stringify(idleSnapshot);
            case "get_app_settings":
            case "get_default_settings":
              return {
                app_language: "en",
                overlay_position: overlayPosition,
                onboarding_completed: true,
                selected_model: "parakeet-tdt-0.6b-v3",
                app_theme: "dark",
                post_process_enabled: postProcessEnabled,
                post_process_provider_id: "apple_intelligence",
                post_process_prompts: prompts,
                post_process_selected_prompt_id: selectedPromptId,
                bindings: {
                  transform: {
                    id: "transform",
                    name: "Transform",
                    description: "Transform selected text",
                    default_binding: "option+1",
                    current_binding: "option+1",
                  },
                },
              };
            case "change_post_process_enabled_setting":
              postProcessEnabled = Boolean(args.enabled);
              emitEvent("transform-settings-changed", null);
              return null;
            case "set_post_process_selected_prompt":
              selectedPromptId = String(args.id ?? "");
              emitEvent("transform-settings-changed", null);
              return null;
            case "set_overlay_transform_menu_open":
            case "open_transforms_workspace":
            case "cancel_operation":
            case "finish_recording":
              return null;
            default:
              return null;
          }
        },
      };
    },
    { pos: initialPosition },
  );
}

async function installWave6StudioMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    let listenerSeq = 1;
    const listeners = new Map<
      number,
      {
        event: string;
        handler: (ev: { event: string; id: number; payload: unknown }) => void;
      }
    >();

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
        transform: {
          id: "transform",
          name: "Transform Shortcut",
          description: "Rewrite highlighted text",
          default_binding: "option+1",
          current_binding: "option+1",
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
      spoken_languages: ["en", "hi"],
      overlay_position: "bottom",
      debug_mode: false,
      log_level: "info",
      custom_words: ["lowyap"],
      model_unload_timeout: "never",
      word_correction_threshold: 0.18,
      history_limit: 50,
      recording_retention_period: "preserve_limit",
      paste_method: "ctrl_v",
      clipboard_handling: "dont_modify",
      post_process_enabled: true,
      post_process_provider_id: "apple_intelligence",
      post_process_providers: [
        {
          id: "apple_intelligence",
          label: "Apple Intelligence",
          base_url: "apple-intelligence://local",
          allow_base_url_edit: false,
          models_endpoint: null,
          supports_structured_output: true,
        },
      ],
      post_process_api_keys: {},
      post_process_models: { apple_intelligence: "Apple Intelligence" },
      post_process_prompts: [
        {
          id: "default_improve_transcriptions",
          name: "Polish",
          prompt: "Clean this speech transcript...",
        },
        {
          id: "builtin_prompt_engineer",
          name: "Prompt Engineer",
          prompt: "Turn spoken instructions into a structured prompt...",
        },
      ],
      post_process_selected_prompt_id: "default_improve_transcriptions",
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
      last_seen_version: "0.9.0",
      show_whats_new_on_update: false,
    };

    const win = window as unknown as Record<string, unknown>;
    win.__wave6Emit = (event: string, payload: unknown) => {
      for (const [id, entry] of listeners.entries()) {
        if (entry.event === event) {
          entry.handler({ event, id, payload });
        }
      }
    };
    win.__TAURI_OS_PLUGIN_INTERNALS__ = {
      arch: "aarch64",
      eol: "\n",
      exe_extension: "",
      family: "unix",
      os_type: "macos",
      platform: "macos",
      version: "15.4.0",
    };
    win.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: (_event: string, eventId: number) => {
        listeners.delete(eventId);
      },
    };
    win.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { windowLabel: "main", label: "main" },
      },
      transformCallback: (callback: (payload: unknown) => void) => {
        const id = listenerSeq++;
        win[`_${id}`] = callback;
        return id;
      },
      convertFileSrc: (filePath: string) => filePath,
      invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
        switch (cmd) {
          case "plugin:event|listen": {
            const eventName = String(args.event ?? "");
            const handlerId = Number(args.handler);
            const cb = win[`_${handlerId}`] as
              | ((ev: { event: string; id: number; payload: unknown }) => void)
              | undefined;
            if (cb) {
              listeners.set(handlerId, { event: eventName, handler: cb });
            }
            return handlerId;
          }
          case "plugin:event|unlisten":
            listeners.delete(Number(args.eventId));
            return null;
          case "plugin:os|locale":
            return "en";
          case "plugin:app|version":
            return "0.1.0";
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
          case "get_flow_builtin_app_tones":
            return [];
          case "get_flow_last_app":
            return null;
          case "has_any_models_available":
            return true;
          case "get_transcription_model_status":
            return "whisper-large-v3-turbo";
          case "is_secure_input_blocked":
            return { blocked: false, culprit: null };
          case "get_history_entries":
          case "get_history_entries_page":
          case "get_history_entries_paginated":
            return { entries: [], has_more: false };
          case "get_available_models":
            return [
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
                supported_languages: ["en", "hi", "ur"],
                supports_language_selection: true,
                is_custom: false,
              },
            ];
          case "get_current_model":
            return "whisper-large-v3-turbo";
          case "get_flow_settings":
            return {
              enabled: true,
              default_tone: "neutral",
              rules: [],
              snippets: [],
              hindustani_script: "roman_hinglish",
            };
          case "get_available_microphones":
          case "get_available_output_devices":
          case "list_meetings":
            return [];
          case "get_meeting_live_state":
            return {
              status: "idle",
              active_meeting_id: null,
              title: "",
              elapsed_ms: 0,
              capture_mode: "mic_and_system_audio",
              app_name: null,
              rough_notes: "",
              recent_segments: [],
              rollup_count: 0,
              detected_call: null,
            };
          case "check_apple_intelligence_availability":
            return true;
          case "set_post_process_selected_prompt":
            settings.post_process_selected_prompt_id = String(args.id ?? "");
            return null;
          default:
            return null;
        }
      },
    };
  });
}

test.describe("Wave 6 — Recording Pill Transform Popover & Studio Handoff", () => {
  test("1. Bottom & top overlay placements open Transform popover with 0px pill jump, switch to Prompt Engineer, and fit inside 348×252 window", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 348, height: 252 });
    await installWave6OverlayMock(page, "bottom");
    await page.goto("/src/overlay/index.html");

    await page.evaluate(() => {
      window.__wave6Emit("show-overlay", "recording");
      window.__wave6Emit("recording-ready", null);
    });

    const card = page.locator(".scard.compact.is-capturing");
    const transformBtn = page.getByTestId("overlay-transform-btn");
    await expect(card).toBeVisible();
    // Resting state omits "Listening" text and stays ultra-compact until hovered
    await expect(page.getByTestId("overlay-stage-label")).toHaveCount(0);
    await expect(transformBtn).toBeHidden();
    const restingBox = await card.boundingBox();
    expect(restingBox).not.toBeNull();
    expect(restingBox!.width).toBeLessThanOrEqual(104);

    await card.hover();
    await expect(transformBtn).toBeVisible();
    await expect(transformBtn).toHaveAttribute("aria-expanded", "false");
    await expect(transformBtn).toHaveAttribute("title", "Transform · Polish");

    const beforeBottomBox = await card.boundingBox();
    expect(beforeBottomBox).not.toBeNull();
    expect(beforeBottomBox!.width).toBeLessThanOrEqual(280);

    await transformBtn.click();
    const popover = page.getByTestId("overlay-transform-popover");
    await expect(popover).toBeVisible();
    await expect(transformBtn).toHaveAttribute("aria-expanded", "true");
    await expect(popover.locator(".stransform-kbd")).toHaveText("⌥1");

    const afterBottomBox = await card.boundingBox();
    const popoverBox = await popover.boundingBox();
    expect(afterBottomBox).not.toBeNull();
    expect(popoverBox).not.toBeNull();
    // Bottom edge of .scard stays anchored with 0px jump when popover opens above it
    expect(
      Math.abs(
        afterBottomBox!.y +
          afterBottomBox!.height -
          (beforeBottomBox!.y + beforeBottomBox!.height),
      ),
    ).toBeLessThanOrEqual(0.5);
    // Popover fits inside the 252px expanded window without clipping top edge
    expect(popoverBox!.y).toBeGreaterThanOrEqual(0);

    // Select "Prompt Engineer" preset -> closes popover, emits IPC, and shows active preset pip
    await page
      .getByTestId("overlay-transform-preset-builtin_prompt_engineer")
      .click();
    await expect(popover).toHaveCount(0);
    await expect(transformBtn).toHaveAttribute(
      "title",
      "Transform · Prompt Engineer",
    );
    await expect(transformBtn.locator(".stransform-preset-pip")).toBeVisible();

    const calls = await page.evaluate(() => window.__wave6Calls);
    expect(
      calls.some(
        (c) =>
          c.cmd === "set_overlay_transform_menu_open" && c.args.open === true,
      ),
    ).toBe(true);
    expect(
      calls.some(
        (c) =>
          c.cmd === "set_post_process_selected_prompt" &&
          c.args.id === "builtin_prompt_engineer",
      ),
    ).toBe(true);
    expect(
      calls.some(
        (c) =>
          c.cmd === "set_overlay_transform_menu_open" && c.args.open === false,
      ),
    ).toBe(true);

    // Verify top-of-screen placement also keeps .scard anchored at top edge with 0px jump
    await page.evaluate(() => {
      window.__wave6SetPosition("top");
      window.__wave6Emit("show-overlay", "recording");
      window.__wave6Emit("recording-ready", null);
    });
    await expect(page.locator(".ov-stage.top")).toBeVisible();
    await card.hover();
    await expect(transformBtn).toBeVisible();
    const beforeTopBox = await card.boundingBox();
    await transformBtn.click();
    await expect(popover).toBeVisible();
    const afterTopBox = await card.boundingBox();
    expect(Math.abs(afterTopBox!.y - beforeTopBox!.y)).toBeLessThanOrEqual(0.5);

    // Configure transforms... invokes open_transforms_workspace and closes popover
    await page.getByTestId("overlay-transform-configure-btn").click();
    await expect(popover).toHaveCount(0);
    const afterConfigureCalls = await page.evaluate(() => window.__wave6Calls);
    expect(
      afterConfigureCalls.some((c) => c.cmd === "open_transforms_workspace"),
    ).toBe(true);
  });

  test("2. Overlay renders transformed_copied (moss success dot) and transform_empty notices cleanly", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 348, height: 64 });
    await installWave6OverlayMock(page, "bottom");
    await page.goto("/src/overlay/index.html");

    await page.evaluate(() => {
      window.__wave6Emit("overlay-notice", "transformed_copied");
      window.__wave6Emit("show-overlay", "notice");
    });
    await expect(
      page.getByText("Transformed — press ⌘V to paste"),
    ).toBeVisible();
    await expect(page.locator(".sdot.success")).toBeVisible();

    await page.evaluate(() => {
      window.__wave6Emit("overlay-notice", "transform_empty");
      window.__wave6Emit("show-overlay", "notice");
    });
    await expect(page.getByText("Select text or dictate first")).toBeVisible();
  });

  test("3. Studio navigate-section='transforms' opens Voice & Style -> AI Rewriting & Prompts with Active Transform Preset bar and Transform Shortcut in Settings", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 960, height: 660 });
    await installWave6StudioMock(page);
    await page.goto("/");

    await expect(
      page.getByRole("heading", { name: "Desk", exact: true }),
    ).toBeVisible();

    await page.evaluate(() => {
      window.__wave6Emit("navigate-section", "transforms");
    });

    await expect(
      page.getByRole("heading", { name: "Voice & Style", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("tab", { name: "AI Rewriting & Prompts" }),
    ).toHaveAttribute("aria-selected", "true");
    await expect(
      page.getByRole("heading", {
        name: "Active Transform Preset",
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("radio", { name: "Polish", exact: true }),
    ).toHaveAttribute("aria-checked", "true");

    const promptEngineerRadio = page.getByRole("radio", {
      name: "Prompt Engineer",
      exact: true,
    });
    await promptEngineerRadio.click();
    await expect(promptEngineerRadio).toHaveAttribute("aria-checked", "true");

    // Verify Transform Shortcut appears in Settings -> Shortcuts & Audio
    await page.keyboard.press("ControlOrMeta+,");
    await expect(
      page.getByRole("heading", { name: "Settings", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("Transform Shortcut", { exact: true }),
    ).toBeVisible();
  });
});
