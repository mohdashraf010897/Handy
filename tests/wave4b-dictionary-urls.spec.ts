import { expect, test, type Page } from "@playwright/test";

interface IpcCallRecord {
  cmd: string;
  args: Record<string, unknown>;
}

interface MockHistoryEntry {
  id: number;
  file_name: string;
  timestamp: number;
  saved: boolean;
  title: string;
  transcription_text: string;
  post_processed_text: string | null;
  post_process_prompt: string | null;
  source_name: string | null;
}

declare global {
  interface Window {
    __wave4bCalls: IpcCallRecord[];
  }
}

async function installWave4bHistoryMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const calls: IpcCallRecord[] = [];
    const listeners = new Map<string, number[]>();
    const callbacks = new Map<number, (data: unknown) => unknown>();
    let nextCallbackId = 1;

    let customWords: string[] = ["Handy"];
    let entries: MockHistoryEntry[] = [
      {
        id: 101,
        file_name: "handy-1700000101.wav",
        timestamp: 1700000101,
        saved: false,
        title: "Recording 101",
        transcription_text: "Check github.com/low app",
        post_processed_text: null,
        post_process_prompt: null,
        source_name: null,
      },
      {
        id: 102,
        file_name: "handy-1700000102.wav",
        timestamp: 1700000102,
        saved: true,
        title: "Recording 102",
        transcription_text: "Visit low app.com today",
        post_processed_text: "Visit low app.com today.",
        post_process_prompt: "Clean up",
        source_name: null,
      },
    ];

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
        currentWindow: { label: "main" },
        currentWebview: {
          windowLabel: "main",
          label: "main",
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
          case "plugin:app|version":
            return "0.9.0";
          case "plugin:macos-permissions|check_accessibility_permission":
          case "plugin:macos-permissions|check_microphone_permission":
            return true;
          case "get_app_settings":
          case "get_default_settings":
            return {
              app_language: "en",
              overlay_position: "bottom",
              onboarding_completed: true,
              show_whats_new_on_update: false,
              whats_new_last_seen_version: "99.0.0",
              selected_model: "parakeet-tdt-0.6b-v3",
              theme: "dark",
              custom_words: [...customWords],
              brand_custom_words_seeded: true,
              word_correction_threshold: 0.18,
              history_limit: 50,
              recording_retention_period: "preserve_limit",
              bindings: {},
            };
          case "get_history_entries":
            return {
              entries: entries.map((e) => ({ ...e })),
              has_more: false,
            };
          case "correct_history_entry_and_save_word": {
            const id = Number(args.id);
            const updatedText = String(args.updatedText ?? "").trim();
            const rawWord =
              args.dictionaryWord === null || args.dictionaryWord === undefined
                ? null
                : String(args.dictionaryWord);

            const target = entries.find((e) => e.id === id);
            if (!target) {
              throw "History entry not found";
            }

            const updatedEntry: MockHistoryEntry = {
              ...target,
              transcription_text: updatedText,
              post_processed_text:
                target.post_processed_text !== null ? updatedText : null,
            };
            entries = entries.map((e) => (e.id === id ? updatedEntry : e));

            if (rawWord && rawWord.trim().length > 0) {
              const normalized = rawWord
                .replace(/[<>"']/g, "")
                .trim()
                .replace(/\s+/g, " ");
              const existingIdx = customWords.findIndex(
                (w) => w.toLowerCase() === normalized.toLowerCase(),
              );
              if (existingIdx >= 0) {
                customWords[existingIdx] = normalized;
              } else {
                customWords = [...customWords, normalized];
              }
              emitEvent("settings-changed", {
                setting: "custom_words",
                value: [...customWords],
              });
            }

            emitEvent("history-update-payload", {
              action: "updated",
              entry: updatedEntry,
            });
            return updatedEntry;
          }
          case "check_custom_sounds":
            return { start: false, stop: false };
          case "get_retired_models":
          case "get_available_microphones":
          case "get_available_output_devices":
          case "get_available_models":
          case "list_meetings":
            return [];
          case "get_current_model":
            return "parakeet-tdt-0.6b-v3";
          case "get_selected_microphone":
          case "get_selected_output_device":
            return "Default";
          case "is_laptop":
            return true;
          case "initialize_enigo":
          case "initialize_shortcuts":
            return null;
          default:
            return null;
        }
      },
    };

    window.__wave4bCalls = calls;
  });
}

test.describe("Wave 4B — History 1-Click Correct & Save to Dictionary (Headless)", () => {
  test("1. Edits transcript, auto-suggests replaced token 'lowyap', saves via IPC, and syncs CustomWords", async ({
    page,
  }) => {
    await installWave4bHistoryMock(page);
    await page.goto("/");

    // Wave 5 Studio lands directly on Desk with the Takes Ledger visible
    await expect(page.getByText("Check github.com/low app")).toBeVisible();

    // Open inline editor on entry 101 ("Check github.com/low app")
    const correctButtons = page.getByRole("button", {
      name: "Correct & save to dictionary",
    });
    await correctButtons.first().click();

    const editor = page.getByTestId("history-inline-editor-101");
    await expect(editor).toBeVisible();

    const transcriptInput = editor.getByLabel("Corrected transcript");
    const dictWordInput = editor.getByLabel("Dictionary word");

    await expect(transcriptInput).toHaveValue("Check github.com/low app");
    await expect(dictWordInput).toHaveValue("");

    // Edit "Check github.com/low app" -> "Check github.com/lowyap"
    await transcriptInput.fill("Check github.com/lowyap");

    // Verify smart token-diff auto-suggests "lowyap" and switches primary CTA
    await expect(dictWordInput).toHaveValue("lowyap");
    const saveAndAddBtn = editor.getByRole("button", {
      name: "Save & Add to Dictionary",
    });
    await expect(saveAndAddBtn).toBeVisible();

    await saveAndAddBtn.click();

    // Editor closes and History card displays updated text immediately
    await expect(editor).toHaveCount(0);
    await expect(page.getByText("Check github.com/lowyap")).toBeVisible();

    // Assert IPC payload sent to `correct_history_entry_and_save_word`
    const saveCall = await page.evaluate(() =>
      window.__wave4bCalls.find(
        (c) => c.cmd === "correct_history_entry_and_save_word",
      ),
    );
    expect(saveCall).toBeTruthy();
    expect(saveCall?.args).toEqual({
      id: 101,
      updatedText: "Check github.com/lowyap",
      dictionaryWord: "lowyap",
    });

    // Switch to Lexicon in Studio rail and verify CustomWords synced "lowyap" live
    await page.getByRole("button", { name: "Lexicon", exact: true }).click();
    await expect(page.getByRole("button", { name: /lowyap/i })).toBeVisible();
  });

  test("2. Escape cancels cleanly without mutating text, and manual dictionary word override works", async ({
    page,
  }) => {
    await installWave4bHistoryMock(page);
    await page.goto("/");

    await expect(page.getByText("Visit low app.com today")).toBeVisible();

    const correctButtons = page.getByRole("button", {
      name: "Correct & save to dictionary",
    });
    await correctButtons.nth(1).click();

    const editor = page.getByTestId("history-inline-editor-102");
    await expect(editor).toBeVisible();

    const transcriptInput = editor.getByLabel("Corrected transcript");
    await transcriptInput.fill("Mutated draft that should be discarded");

    // Press Escape -> inline editor closes without mutating entry or invoking IPC
    await transcriptInput.press("Escape");
    await expect(editor).toHaveCount(0);
    await expect(page.getByText("Visit low app.com today")).toBeVisible();
    expect(
      await page.evaluate(() =>
        window.__wave4bCalls.some(
          (c) => c.cmd === "correct_history_entry_and_save_word",
        ),
      ),
    ).toBe(false);

    // Re-open editor, manually override dictionary word, then edit transcript:
    // manual dictionary word must NOT be overwritten by auto-suggestion!
    await correctButtons.nth(1).click();
    await expect(editor).toBeVisible();

    const dictWordInput = editor.getByLabel("Dictionary word");
    await dictWordInput.fill("lowyap.com");
    await transcriptInput.fill("Visit lowyap.com today");
    await expect(dictWordInput).toHaveValue("lowyap.com");

    // Save with Cmd/Ctrl+Enter keyboard shortcut
    await dictWordInput.press("ControlOrMeta+Enter");

    await expect(editor).toHaveCount(0);
    await expect(page.getByText("Visit lowyap.com today")).toBeVisible();

    const saveCall = await page.evaluate(() =>
      window.__wave4bCalls.find(
        (c) => c.cmd === "correct_history_entry_and_save_word",
      ),
    );
    expect(saveCall?.args).toEqual({
      id: 102,
      updatedText: "Visit lowyap.com today",
      dictionaryWord: "lowyap.com",
    });
  });
});
