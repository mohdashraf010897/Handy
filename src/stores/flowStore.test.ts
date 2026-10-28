// Run with: bun src/stores/flowStore.test.ts
import assert from "node:assert/strict";
import { commands, type FlowSettings } from "@/bindings";
import { useFlowStore } from "./flowStore";

type SetResult = Awaited<ReturnType<typeof commands.setFlowSettings>>;

const sent: FlowSettings[] = [];
let fail = new Set<number>();
commands.getFlowLastApp = async () => null;
commands.setFlowSettings = async (settings) => {
  const call = sent.push(settings);
  // Yield so overlapping saves would interleave if they were not queued.
  await new Promise((resolve) => setTimeout(resolve, 5));
  return (
    fail.has(call)
      ? { status: "error", error: "disk full" }
      : { status: "ok", data: settings }
  ) as SetResult;
};

const reset = (settings: FlowSettings) => {
  sent.length = 0;
  fail = new Set();
  useFlowStore.setState({ settings, isSaving: false });
};

const { save } = useFlowStore.getState();

// Blur-commit then an immediate click: both edits land, in order.
reset({
  tone_enabled: false,
  app_tones: [{ app_id: "slack", tone: "casual" }],
});
const rename = save((s) => ({
  ...s,
  app_tones: s.app_tones.map((r) => ({ ...r, app_id: "discord" })),
}));
const enable = save((s) => ({ ...s, tone_enabled: true }));
assert.equal(useFlowStore.getState().isSaving, true);
assert.deepEqual(await Promise.all([rename, enable]), [true, true]);
assert.deepEqual(useFlowStore.getState().settings, {
  tone_enabled: true,
  app_tones: [{ app_id: "discord", tone: "casual" }],
});
assert.equal(sent.length, 2);
assert.equal(
  sent[1].app_tones[0].app_id,
  "discord",
  "second save saw the first",
);
assert.equal(useFlowStore.getState().isSaving, false);

// A failed save rolls back only itself; the next queued save still applies.
reset({ tone_enabled: false, app_tones: [] });
fail = new Set([1]);
const failed = save((s) => ({ ...s, tone_enabled: true }));
const added = save((s) => ({
  ...s,
  app_tones: [...s.app_tones, { app_id: "mail", tone: "formal" }],
}));
assert.deepEqual(await Promise.all([failed, added]), [false, true]);
assert.deepEqual(useFlowStore.getState().settings, {
  tone_enabled: false,
  app_tones: [{ app_id: "mail", tone: "formal" }],
});

// Plain values still work (used by Reset).
reset({ tone_enabled: true, app_tones: [{ app_id: "x", tone: "code" }] });
assert.equal(await save({ tone_enabled: false, app_tones: [] }), true);
assert.deepEqual(useFlowStore.getState().settings, {
  tone_enabled: false,
  app_tones: [],
});

console.log("flowStore tests passed");
