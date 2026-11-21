// Run with: bun src/stores/flowStore.test.ts
import assert from "node:assert/strict";
import { commands, type FlowSettings } from "@/bindings";
import { DEFAULT_FLOW_SETTINGS } from "@/components/flow/toneRules";
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

const reset = (settings: Partial<FlowSettings>) => {
  sent.length = 0;
  fail = new Set();
  useFlowStore.setState({
    settings: { ...DEFAULT_FLOW_SETTINGS, ...settings },
    isSaving: false,
  });
};

const { save } = useFlowStore.getState();

// Blur-commit then an immediate click: both edits land, in order.
reset({
  tone_enabled: false,
  app_tones: [{ app_id: "slack", tone: "casual" }],
  fast_path_enabled: true,
  snippets: [],
});
const rename = save((s) => ({
  ...s,
  app_tones: s.app_tones.map((r) => ({ ...r, app_id: "discord" })),
}));
const enable = save((s) => ({ ...s, tone_enabled: true }));
assert.equal(useFlowStore.getState().isSaving, true);
assert.deepEqual(await Promise.all([rename, enable]), [true, true]);
assert.deepEqual(useFlowStore.getState().settings, {
  ...DEFAULT_FLOW_SETTINGS,
  tone_enabled: true,
  app_tones: [{ app_id: "discord", tone: "casual" }],
  fast_path_enabled: true,
  snippets: [],
});
assert.equal(sent.length, 2);
assert.equal(
  sent[1].app_tones[0].app_id,
  "discord",
  "second save saw the first",
);
assert.equal(useFlowStore.getState().isSaving, false);

// Fast-path toggle, Hindustani script, mic boost, and voice snippet queue sequentially without overwriting.
const toggleFastPath = save((s) => ({ ...s, fast_path_enabled: false }));
const setScript = save((s) => ({
  ...s,
  hindustani_script: "urdu_perso_arabic",
  auto_boost_mic_volume: false,
}));
const addSnippet = save((s) => ({
  ...s,
  snippets: [
    ...s.snippets,
    { trigger: "my calendar link", expansion: "https://cal.com/you" },
  ],
}));
assert.deepEqual(
  await Promise.all([toggleFastPath, setScript, addSnippet]),
  [true, true, true],
);
assert.deepEqual(useFlowStore.getState().settings, {
  ...DEFAULT_FLOW_SETTINGS,
  tone_enabled: true,
  app_tones: [{ app_id: "discord", tone: "casual" }],
  fast_path_enabled: false,
  hindustani_script: "urdu_perso_arabic",
  auto_boost_mic_volume: false,
  snippets: [{ trigger: "my calendar link", expansion: "https://cal.com/you" }],
});

// A failed save rolls back only itself; the next queued save still applies.
reset({
  tone_enabled: false,
  app_tones: [],
  fast_path_enabled: true,
  snippets: [],
});
fail = new Set([1]);
const failed = save((s) => ({ ...s, tone_enabled: true }));
const added = save((s) => ({
  ...s,
  app_tones: [...s.app_tones, { app_id: "mail", tone: "formal" }],
}));
assert.deepEqual(await Promise.all([failed, added]), [false, true]);
assert.deepEqual(useFlowStore.getState().settings, {
  ...DEFAULT_FLOW_SETTINGS,
  tone_enabled: false,
  app_tones: [{ app_id: "mail", tone: "formal" }],
  fast_path_enabled: true,
  snippets: [],
});

// Plain values still work (used by Reset).
reset({
  tone_enabled: true,
  app_tones: [{ app_id: "x", tone: "code" }],
  fast_path_enabled: false,
  snippets: [{ trigger: "sig", expansion: "Best, Alex" }],
});
assert.equal(await save(DEFAULT_FLOW_SETTINGS), true);
assert.deepEqual(useFlowStore.getState().settings, DEFAULT_FLOW_SETTINGS);

console.log("flowStore tests passed");
