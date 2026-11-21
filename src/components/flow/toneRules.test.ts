// Run with: bun src/components/flow/toneRules.test.ts
import assert from "node:assert/strict";
import type { AppToneRule, FlowSettings, Tone, VoiceSnippet } from "@/bindings";
import {
  DEFAULT_FLOW_SETTINGS,
  appMatches,
  findRuleIndex,
  isFlowCustomized,
  normalizeAppId,
  normalizeSnippetTrigger,
  overridingRule,
  validateAppId,
  validateSnippet,
} from "./toneRules";

const rule = (app_id: string, tone: Tone = "casual"): AppToneRule => ({
  app_id,
  tone,
});

const snippet = (trigger: string, expansion: string): VoiceSnippet => ({
  trigger,
  expansion,
});

const flowSettings = (overrides: Partial<FlowSettings> = {}): FlowSettings => ({
  ...DEFAULT_FLOW_SETTINGS,
  ...overrides,
});

// Matching mirrors the Rust side (context.rs `app_matches`).
assert.equal(normalizeAppId("  Slack.EXE "), "slack");
assert.ok(appMatches("slack.exe", "Slack"));
assert.ok(appMatches("com.jetbrains.*", "com.jetbrains.intellij"));
assert.ok(!appMatches("com.jetbrains.*", "com.jetbrainsx"));
assert.ok(!appMatches("*", "anything"));
assert.ok(!appMatches("com.apple.mail", "com.apple.mailx"));

// Validation matches FlowSettings::validated and catches duplicates.
const rules = [rule("com.tinyspeck.slackmacgap"), rule("slack.exe", "formal")];
assert.equal(validateAppId("   ", rules), "empty");
assert.equal(validateAppId(" * ", rules), "wildcardOnly");
assert.equal(validateAppId("SLACK", rules), "duplicate");
assert.equal(validateAppId("slack", rules, 1), null);
assert.equal(validateAppId("com.apple.mail", rules), null);

assert.equal(findRuleIndex(rules, "Slack.exe"), 1);
assert.equal(findRuleIndex(rules, "discord"), -1);

// A user rule overrides a built-in by exact id or by a covering wildcard.
assert.deepEqual(overridingRule(rule("slack"), rules), rules[1]);
assert.deepEqual(
  overridingRule(rule("com.apple.mail", "formal"), [rule("com.apple.*")]),
  rule("com.apple.*"),
);
assert.equal(
  overridingRule(rule("com.jetbrains.*", "code"), [rule("com.jetbrains.x")]),
  undefined,
);
assert.equal(overridingRule(rule("discord"), rules), undefined);

// Snippet normalization and validation.
assert.equal(
  normalizeSnippetTrigger("  my   calendar \t link  "),
  "my calendar link",
);
assert.equal(normalizeSnippetTrigger("   "), "");
const snippets = [snippet("my calendar link", "https://cal.com/you")];
assert.equal(
  validateSnippet("   ", "https://cal.com/you", snippets),
  "emptyTrigger",
);
assert.equal(validateSnippet("my email", "   ", snippets), "emptyExpansion");
assert.equal(
  validateSnippet("  MY   CALENDAR LINK ", "https://cal.com/other", snippets),
  "duplicate",
);
assert.equal(
  validateSnippet("MY CALENDAR LINK", "https://cal.com/updated", snippets, 0),
  null,
);
assert.equal(validateSnippet("my email", "me@example.com", snippets), null);

// Reset is offered only when settings differ from the defaults.
assert.deepEqual(DEFAULT_FLOW_SETTINGS, {
  tone_enabled: false,
  app_tones: [],
  fast_path_enabled: true,
  snippets: [],
});
assert.equal(isFlowCustomized(DEFAULT_FLOW_SETTINGS), false);
assert.equal(isFlowCustomized(flowSettings()), false);
assert.equal(isFlowCustomized(flowSettings({ tone_enabled: true })), true);
assert.equal(
  isFlowCustomized(flowSettings({ app_tones: [rule("slack")] })),
  true,
);
assert.equal(
  isFlowCustomized(flowSettings({ fast_path_enabled: false })),
  true,
);
assert.equal(
  isFlowCustomized(
    flowSettings({ snippets: [snippet("my email", "me@example.com")] }),
  ),
  true,
);
const customDefaults = flowSettings({
  tone_enabled: true,
  app_tones: [rule("slack")],
});
assert.equal(isFlowCustomized(customDefaults, customDefaults), false);
assert.equal(
  isFlowCustomized(
    flowSettings({
      tone_enabled: true,
      app_tones: [rule("slack", "formal")],
    }),
    customDefaults,
  ),
  true,
);

console.log("toneRules tests passed");
