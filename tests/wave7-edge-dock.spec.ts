import { expect, test, type Page } from "@playwright/test";

interface DockIpcCall {
  cmd: string;
  args: Record<string, unknown>;
}

declare global {
  interface Window {
    __wave7Calls: DockIpcCall[];
    __wave7Emit: (event: string, payload: unknown) => void;
  }
}

async function installEdgeDockTauriMock(
  page: Page,
  initialEdge: "left" | "bottom" | "right" = "bottom",
): Promise<void> {
  await page.addInitScript(
    ({ startEdge }) => {
      const calls: DockIpcCall[] = [];
      const listeners = new Map<string, number[]>();
      const callbacks = new Map<number, (data: unknown) => unknown>();
      let nextCallbackId = 1;

      let enabled = true;
      let snoozed = false;
      let position: "left" | "bottom" | "right" = startEdge;
      let mode: "collapsed" | "expanded" | "dragging" = "collapsed";
      let menuOpen = false;
      let previewPosition: "left" | "bottom" | "right" = startEdge;
      let autoApplyEnabled = true;
      let selectedPresetId = "default_improve_transcriptions";
      const presets = [
        { id: "default_improve_transcriptions", name: "Polish" },
        { id: "builtin_prompt_engineer", name: "Prompt Engineer" },
      ];

      const emitEvent = (event: string, payload: unknown) => {
        const ids = listeners.get(event) ?? [];
        for (const id of ids) {
          const cb = callbacks.get(id);
          if (cb) cb({ event, id, payload });
        }
      };

      const currentState = () => ({
        enabled,
        snoozed,
        position,
        mode,
        menu_open: menuOpen,
        preview_position: previewPosition,
        dictate_shortcut: "⌥Space",
        meeting_shortcut: "",
        transform_shortcut: "⌥1",
        auto_apply_enabled: autoApplyEnabled,
        selected_preset_id: selectedPresetId,
        active_preset_title:
          presets.find((p) => p.id === selectedPresetId)?.name ?? "Polish",
        presets,
        cursor_rel_x: null,
        cursor_rel_y: null,
        theme: "dark",
      });

      window.__wave7Calls = calls;
      window.__wave7Emit = emitEvent;

      (
        window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: unknown }
      ).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
        unregisterListener: (event: string, eventId: number) => {
          const list = listeners.get(event);
          if (list) {
            listeners.set(
              event,
              list.filter((x) => x !== eventId),
            );
          }
        },
      };

      (
        window as unknown as { __TAURI_INTERNALS__: unknown }
      ).__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: "edge_dock" },
          currentWebview: { windowLabel: "edge_dock", label: "edge_dock" },
        },
        transformCallback: (cb: (data: unknown) => unknown) => {
          const id = nextCallbackId++;
          callbacks.set(id, cb);
          return id;
        },
        unregisterCallback: (id: number) => {
          callbacks.delete(id);
        },
        invoke: async (
          cmd: string,
          args: Record<string, unknown> = {},
        ): Promise<unknown> => {
          calls.push({ cmd, args });
          if (cmd === "plugin:event|listen") {
            const event = String(args.event ?? "");
            const handler = Number(args.handler);
            const existing = listeners.get(event) ?? [];
            existing.push(handler);
            listeners.set(event, existing);
            return handler;
          }
          if (cmd === "plugin:event|unlisten") {
            return null;
          }
          switch (cmd) {
            case "get_app_settings":
              return {
                app_language: "en",
                theme: "dark",
                onboarding_completed: true,
              };
            case "get_edge_dock_state":
              return currentState();
            case "set_edge_dock_expanded":
              if (mode !== "dragging") {
                mode = args.expanded ? "expanded" : "collapsed";
                if (!args.expanded) {
                  menuOpen = false;
                }
                emitEvent("edge-dock-state", currentState());
              }
              return null;
            case "set_edge_dock_menu_open":
              menuOpen = Boolean(args.open);
              if (menuOpen && mode !== "dragging") {
                mode = "expanded";
              }
              emitEvent("edge-dock-state", currentState());
              return null;
            case "change_post_process_enabled_setting":
              autoApplyEnabled = Boolean(args.enabled);
              emitEvent("edge-dock-state", currentState());
              return null;
            case "set_post_process_selected_prompt":
              selectedPresetId = String(args.id ?? args.promptId ?? selectedPresetId);
              emitEvent("edge-dock-state", currentState());
              return null;
            case "snooze_edge_dock":
              snoozed = true;
              menuOpen = false;
              mode = "collapsed";
              emitEvent("edge-dock-state", currentState());
              return null;
            case "hide_edge_dock":
              enabled = false;
              menuOpen = false;
              mode = "collapsed";
              emitEvent("edge-dock-state", currentState());
              return { edge_dock_enabled: false, edge_dock_position: position };
            case "begin_edge_dock_drag":
              mode = "dragging";
              menuOpen = false;
              previewPosition = position;
              emitEvent("edge-dock-state", currentState());
              return null;
            case "finish_edge_dock_drag": {
              const target = (args.position ?? previewPosition) as
                | "left"
                | "bottom"
                | "right";
              position = target;
              previewPosition = target;
              menuOpen = false;
              mode = "collapsed";
              emitEvent("edge-dock-state", currentState());
              return target;
            }
            case "trigger_dock_dictation":
            case "trigger_dock_meeting":
              menuOpen = false;
              mode = "collapsed";
              emitEvent("edge-dock-state", currentState());
              return null;
            case "trigger_dock_transform":
              menuOpen = false;
              mode = "collapsed";
              emitEvent("edge-dock-state", currentState());
              return {
                applied: true,
                pasted: true,
                source: "selection",
                preset_id: selectedPresetId,
                preset_name:
                  presets.find((p) => p.id === selectedPresetId)?.name ??
                  "Polish",
                output: "Polished text.",
                elapsed_ms: 480,
              };
            default:
              return null;
          }
        },
      };
    },
    { startEdge: initialEdge },
  );
}

test.describe("Wave 7 v2.1 — Tactile Whisper-Bar Non-Activating Micro-Dock", () => {
  test("renders 46×9px tactile capsule, triggers hover even behind the bar at the flush screen edge, and runs Transform", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 264, height: 264 });
    await installEdgeDockTauriMock(page, "bottom");
    await page.goto("/src/dock/index.html");

    const sliver = page.getByTestId("edge-dock-sliver");
    await expect(sliver).toBeVisible();
    const hairlineBox = await sliver.locator(".ed-hairline").boundingBox();
    expect(hairlineBox).not.toBeNull();
    expect(hairlineBox!.width).toBeGreaterThanOrEqual(44);
    expect(hairlineBox!.width).toBeLessThanOrEqual(48);
    expect(hairlineBox!.height).toBeGreaterThanOrEqual(8);
    expect(hairlineBox!.height).toBeLessThanOrEqual(10);

    const sliverBox = await sliver.boundingBox();
    expect(sliverBox).not.toBeNull();
    // Flush-to-bottom-edge hit zone: sliver must reach y + height === 264 (0 dead gap behind bar).
    expect(sliverBox!.y + sliverBox!.height).toBeCloseTo(264, 0);

    // 1. Sweeping past/around the bar and landing BEHIND the capsule at y=263 (1px from physical screen bottom)
    // MUST immediately trigger hover expansion and keep it open while behind the bar!
    await page.mouse.move(20, 200);
    await page.mouse.move(40, 263, { steps: 3 });
    await page.mouse.move(132, 263, { steps: 4 });
    await expect(page.getByTestId("edge-dock-expanded")).toBeVisible();

    // Moving cursor away collapses cleanly; entering at the edge below the icons expands with ZERO persistent text until an icon is hovered.
    await page.mouse.move(20, 40);
    await expect(page.getByTestId("edge-dock-expanded")).toBeHidden();
    await page.mouse.move(102, 262);
    await expect(page.getByTestId("edge-dock-expanded")).toBeVisible();
    await expect(page.getByTestId("edge-dock-readout")).toHaveCount(0);

    const readout = page.getByTestId("edge-dock-readout");
    await page.getByTestId("edge-dock-action-dictate").hover();
    await expect(readout).toContainText("Dictate");
    await expect(readout).toContainText("⌥Space");
    const dictateCenter = await readout.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return r.left + r.width / 2;
    });

    await page.getByTestId("edge-dock-action-meeting").hover();
    await expect(readout).toContainText("Meeting notes");
    await expect(readout).toContainText("No bot");
    await expect
      .poll(
        () =>
          readout.evaluate((el) => {
            const r = el.getBoundingClientRect();
            return r.left + r.width / 2;
          }),
        { timeout: 1_500 },
      )
      .toBeGreaterThan(dictateCenter + 18);
    const meetingCenter = await readout.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return r.left + r.width / 2;
    });

    await page.getByTestId("edge-dock-action-transform").hover();
    await expect(readout).toContainText("Transform · Polish");
    await expect(readout).toContainText("⌥1");
    await expect
      .poll(
        () =>
          readout.evaluate((el) => {
            const r = el.getBoundingClientRect();
            return r.left + r.width / 2;
          }),
        { timeout: 1_500 },
      )
      .toBeGreaterThan(meetingCenter + 14);

    // Unfocused macOS nonactivating_panel path: native "edge-dock-cursor" events
    // smoothly update hovered icon + tooltip even without DOM mouseenter/mouseleave!
    const dictateBtnBox = await page
      .getByTestId("edge-dock-action-dictate")
      .boundingBox();
    const meetingBtnBox = await page
      .getByTestId("edge-dock-action-meeting")
      .boundingBox();
    expect(dictateBtnBox).not.toBeNull();
    expect(meetingBtnBox).not.toBeNull();
    await page.evaluate(
      ({ x, y }) => window.__wave7Emit("edge-dock-cursor", { x, y }),
      {
        x: dictateBtnBox!.x + dictateBtnBox!.width / 2,
        y: dictateBtnBox!.y + dictateBtnBox!.height / 2,
      },
    );
    await expect(readout).toContainText("Dictate");
    await page.evaluate(
      ({ x, y }) => window.__wave7Emit("edge-dock-cursor", { x, y }),
      {
        x: meetingBtnBox!.x + meetingBtnBox!.width / 2,
        y: meetingBtnBox!.y + meetingBtnBox!.height / 2,
      },
    );
    await expect(readout).toContainText("Meeting notes");

    // 3. Clicking Transform opens clean macOS HUD preset popover (without Snooze/Hide clutter).
    await page.getByTestId("edge-dock-action-transform").click();
    const menu = page.getByTestId("edge-dock-transform-menu");
    await expect(menu).toBeVisible();
    await expect(page.getByTestId("edge-dock-snooze-btn")).toHaveCount(0);

    // Switching Polish -> Prompt Engineer updates active preset AND immediately applies Transform.
    await page.getByTestId("edge-dock-preset-builtin_prompt_engineer").click();
    await expect(sliver).toBeVisible();

    // 4. Re-opening Transform confirms Prompt Engineer is active, runs ⌥1, and switches cleanly back to Polish!
    await sliver.hover();
    await page.getByTestId("edge-dock-action-transform").click();
    await expect(page.getByTestId("edge-dock-run-transform")).toContainText(
      "Prompt Engineer",
    );
    await page
      .getByTestId("edge-dock-preset-default_improve_transcriptions")
      .click();
    await expect(sliver).toBeVisible();

    const transformCalls = await page.evaluate(() =>
      window.__wave7Calls.filter(
        (c) =>
          c.cmd === "set_post_process_selected_prompt" ||
          c.cmd === "trigger_dock_transform",
      ),
    );
    expect(transformCalls).toEqual([
      {
        cmd: "set_post_process_selected_prompt",
        args: { id: "builtin_prompt_engineer" },
      },
      {
        cmd: "trigger_dock_transform",
        args: { promptId: "builtin_prompt_engineer" },
      },
      {
        cmd: "set_post_process_selected_prompt",
        args: { id: "default_improve_transcriptions" },
      },
      {
        cmd: "trigger_dock_transform",
        args: { promptId: "default_improve_transcriptions" },
      },
    ]);
  });

  test("supports Escape collapse and right-click Snooze 1 hour escape hatch", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 264, height: 264 });
    await installEdgeDockTauriMock(page, "bottom");
    await page.goto("/src/dock/index.html");

    const sliver = page.getByTestId("edge-dock-sliver");
    await sliver.click();
    await expect(page.getByTestId("edge-dock-expanded")).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(page.getByTestId("edge-dock-expanded")).toBeHidden();
    await expect(sliver).toBeVisible();

    // Right-click whisper-bar opens escape hatch menu -> Snooze 1 hour hides dock immediately.
    await sliver.click({ button: "right" });
    await expect(page.getByTestId("edge-dock-transform-menu")).toBeVisible();
    await page.getByTestId("edge-dock-snooze-btn").click();
    await expect(page.getByTestId("edge-dock-stage")).toHaveCount(0);

    const snoozeCall = await page.evaluate(() =>
      window.__wave7Calls.find((c) => c.cmd === "snooze_edge_dock"),
    );
    expect(snoozeCall?.args).toEqual({ minutes: 60 });
  });

  test("drags smoothly across Left, Bottom, and Right snap zones and persists chosen edge", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 900, height: 600 });
    await installEdgeDockTauriMock(page, "left");
    await page.goto("/src/dock/index.html");

    const sliver = page.getByTestId("edge-dock-sliver");
    await expect(sliver).toBeVisible();
    const startBox = await sliver.boundingBox();
    expect(startBox).not.toBeNull();

    // Begin drag from left sliver toward bottom center, then right edge.
    await page.mouse.move(
      startBox!.x + startBox!.width / 2,
      startBox!.y + startBox!.height / 2,
    );
    await page.mouse.down();
    await page.mouse.move(450, 580, { steps: 6 });

    await expect(page.getByTestId("edge-dock-drag-canvas")).toBeVisible();
    await expect(page.getByTestId("edge-dock-snap-left")).toHaveText("");
    await expect(page.getByTestId("edge-dock-snap-bottom")).toHaveText("");
    await expect(page.getByTestId("edge-dock-snap-right")).toHaveText("");
    await expect(page.getByTestId("edge-dock-snap-bottom")).toHaveAttribute(
      "data-active",
      "true",
    );

    // Move to right edge and release to snap right.
    await page.mouse.move(885, 300, { steps: 6 });
    await expect(page.getByTestId("edge-dock-snap-right")).toHaveAttribute(
      "data-active",
      "true",
    );
    await page.mouse.up();

    await expect(page.getByTestId("edge-dock-stage")).toHaveAttribute(
      "data-edge",
      "right",
    );
    await expect(page.getByTestId("edge-dock-sliver")).toHaveAttribute(
      "data-edge",
      "right",
    );
  });
});
