// Mounts <PerAppToneSettings/> in a plain browser with Tauri IPC mocked, so
// Playwright can drive the real component without the Tauri shell.
//
// Query params:
//   postProcess=off   AI post-processing starts disabled (default: on)
//   lastApp=textedit  get_flow_last_app reports TextEdit (default: none)
import React from "react";
import ReactDOM from "react-dom/client";
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { mockIPC } from "@tauri-apps/api/mocks";
import type { AppToneRule, FlowSettings, LastFlowApp } from "@/bindings";
import { PerAppToneSettings } from "@/components/flow";
import { DEFAULT_FLOW_SETTINGS, appMatches } from "@/components/flow/toneRules";
import en from "@/i18n/locales/en/translation.json";
import "@/App.css";

// A subset of BUILTIN_APP_TONES in src-tauri/src/flow/context.rs.
const BUILTINS: AppToneRule[] = [
  { app_id: "com.tinyspeck.slackmacgap", tone: "casual" },
  { app_id: "com.apple.mail", tone: "formal" },
  { app_id: "com.jetbrains.*", tone: "code" },
  { app_id: "com.apple.Terminal", tone: "code" },
];

const LAST_APPS: Record<string, { app_id: string; app_name: string }> = {
  textedit: { app_id: "com.apple.TextEdit", app_name: "TextEdit" },
};

export interface FlowHarness {
  /** Every settings payload sent with set_flow_settings, in order. */
  saves: FlowSettings[];
  /** What the mocked backend currently stores. */
  stored: FlowSettings;
  postProcessEnabled: boolean;
  /** Commands the harness does not implement. */
  unhandled: string[];
}

declare global {
  interface Window {
    __flowHarness: FlowHarness;
  }
}

const params = new URLSearchParams(window.location.search);
const clone = <T,>(value: T): T => structuredClone(value);

const harness: FlowHarness = {
  saves: [],
  stored: clone(DEFAULT_FLOW_SETTINGS),
  postProcessEnabled: params.get("postProcess") !== "off",
  unhandled: [],
};
window.__flowHarness = harness;

const lastAppContext = LAST_APPS[params.get("lastApp") ?? ""] ?? null;

// Mirrors `get_flow_last_app` + `matching_rule`: user rules, then built-ins.
const lastFlowApp = (): LastFlowApp | null => {
  if (!lastAppContext) return null;
  const match =
    harness.stored.app_tones
      .map((rule) => ({ rule, user: true }))
      .concat(BUILTINS.map((rule) => ({ rule, user: false })))
      .find(({ rule }) => appMatches(rule.app_id, lastAppContext.app_id)) ??
    null;
  return {
    ...lastAppContext,
    tone: match?.rule.tone ?? "default",
    matched_rule: match?.rule.app_id ?? null,
    user_rule: match?.user ?? false,
  };
};

const appSettings = () => ({
  app_language: "en",
  post_process_enabled: harness.postProcessEnabled,
});

mockIPC(
  (cmd, args) => {
    const payload = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "get_flow_settings":
        return clone(harness.stored);
      case "set_flow_settings": {
        const settings = clone(payload.settings as FlowSettings);
        harness.saves.push(clone(settings));
        harness.stored = settings;
        return clone(settings);
      }
      case "get_flow_builtin_app_tones":
        return clone(BUILTINS);
      case "get_flow_last_app":
        return lastFlowApp();
      case "get_app_settings":
      case "get_default_settings":
        return appSettings();
      case "change_post_process_enabled_setting":
        harness.postProcessEnabled = payload.enabled as boolean;
        return null;
      case "check_custom_sounds":
        return { start: false, stop: false };
      case "is_update_checks_locked":
        return false;
      default:
        harness.unhandled.push(cmd);
        throw new Error(`Unmocked command: ${cmd}`);
    }
  },
  { shouldMockEvents: true },
);

void i18n.use(initReactI18next).init({
  resources: { en: { translation: en } },
  lng: "en",
  fallbackLng: "en",
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <main className="p-6">
      <PerAppToneSettings />
    </main>
  </React.StrictMode>,
);
