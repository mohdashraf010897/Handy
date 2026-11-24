import React, { useEffect, useId, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";
import { isRTLLanguage } from "@/lib/utils/rtl";
import { PerAppToneSettings } from "../flow/PerAppToneSettings";
import { FillerWordRemoval } from "../settings/FillerWordRemoval";
import { PostProcessingSettings } from "../settings/post-processing/PostProcessingSettings";
import { PostProcessingToggle } from "../settings/PostProcessingToggle";
import { SettingsGroup } from "../ui/SettingsGroup";

export type VoiceStyleTab = "toneAndSnippets" | "linguistEngine";

export interface VoiceStyleWorkspaceProps {
  initialTab?: VoiceStyleTab;
}

const VOICE_TABS: readonly VoiceStyleTab[] = [
  "toneAndSnippets",
  "linguistEngine",
];

const BUILTIN_POLISH_ID = "default_improve_transcriptions";
const BUILTIN_PROMPT_ENGINEER_ID = "builtin_prompt_engineer";

export const VoiceStyleWorkspace: React.FC<VoiceStyleWorkspaceProps> = ({
  initialTab = "toneAndSnippets",
}) => {
  const { t, i18n } = useTranslation();
  const { getSetting, updateSetting, refreshSettings } = useSettings();
  const [tab, setTab] = useState<VoiceStyleTab>(initialTab);
  const baseId = useId();
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  useEffect(() => {
    setTab(initialTab);
  }, [initialTab]);

  useEffect(() => {
    const unlistenTab = listen<string>("open-voice-tab", (event) => {
      if (
        event.payload === "linguistEngine" ||
        event.payload === "transforms"
      ) {
        setTab("linguistEngine");
      } else if (
        event.payload === "toneAndSnippets" ||
        event.payload === "tones"
      ) {
        setTab("toneAndSnippets");
      }
    });
    const unlistenTransformSync = listen("transform-settings-changed", () => {
      void refreshSettings();
    });
    return () => {
      unlistenTab.then((fn) => fn());
      unlistenTransformSync.then((fn) => fn());
    };
  }, [refreshSettings]);

  // PerAppToneSettings already renders PostProcessingToggle inside its own card
  // when post_process_enabled is false; only render it in the pipeline group
  // when enabled so the toggle appears once in every state.
  const postProcessEnabled = getSetting("post_process_enabled") ?? false;
  const prompts = getSetting("post_process_prompts") ?? [];
  const activePromptId =
    getSetting("post_process_selected_prompt_id") ||
    prompts[0]?.id ||
    BUILTIN_POLISH_ID;
  const transformShortcut =
    getSetting("bindings")?.transform?.current_binding || "";

  const formatPresetName = (id: string, rawName: string): string => {
    if (
      id === BUILTIN_POLISH_ID &&
      (rawName === "Polish" || rawName === "Improve Transcriptions")
    ) {
      return t("overlay.transformPresetPolish", { defaultValue: "Polish" });
    }
    if (id === BUILTIN_PROMPT_ENGINEER_ID && rawName === "Prompt Engineer") {
      return t("overlay.transformPresetPromptEngineer", {
        defaultValue: "Prompt Engineer",
      });
    }
    return rawName;
  };

  const handleSelectPreset = (promptId: string) => {
    updateSetting("post_process_selected_prompt_id", promptId);
    if (!postProcessEnabled) {
      updateSetting("post_process_enabled", true);
    }
  };

  const tabLabels: Record<VoiceStyleTab, string> = {
    toneAndSnippets: t("studio.voice.tabs.toneAndSnippets", {
      defaultValue: "App Tone & Snippets",
    }),
    linguistEngine: t("studio.voice.tabs.linguistEngine", {
      defaultValue: "Linguist Engine & Prompts",
    }),
  };

  const handleTabKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    const isRtl = isRTLLanguage(i18n.language);
    const forwardKey = isRtl ? "ArrowLeft" : "ArrowRight";
    const backwardKey = isRtl ? "ArrowRight" : "ArrowLeft";
    let nextIndex = -1;
    if (event.key === forwardKey) {
      nextIndex = (index + 1) % VOICE_TABS.length;
    } else if (event.key === backwardKey) {
      nextIndex = (index - 1 + VOICE_TABS.length) % VOICE_TABS.length;
    } else if (event.key === "Home") {
      nextIndex = 0;
    } else if (event.key === "End") {
      nextIndex = VOICE_TABS.length - 1;
    }

    if (nextIndex !== -1) {
      event.preventDefault();
      const nextTab = VOICE_TABS[nextIndex];
      setTab(nextTab);
      tabRefs.current[nextIndex]?.focus();
    }
  };

  const tabId = (key: VoiceStyleTab) => `${baseId}-tab-${key}`;
  const panelId = (key: VoiceStyleTab) => `${baseId}-panel-${key}`;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <header className="flex flex-wrap items-baseline justify-between gap-3 border-b border-line px-1 pb-3">
        <h1 className="font-display text-2xl font-semibold tracking-tight text-text">
          {t("studio.voice.title", { defaultValue: "Voice & Style" })}
        </h1>
        <p className="text-end font-mono text-xs text-ink-3">
          {t("studio.voice.subtitle", {
            defaultValue: "Linguist polish · per-app tone · voice snippets",
          })}
        </p>
      </header>

      <div
        role="tablist"
        aria-label={t("studio.voice.title", { defaultValue: "Voice & Style" })}
        className="inline-flex flex-wrap items-center gap-1 rounded-control border border-line bg-wash p-1"
      >
        {VOICE_TABS.map((key, index) => {
          const isSelected = tab === key;
          return (
            <button
              key={key}
              ref={(el) => {
                tabRefs.current[index] = el;
              }}
              id={tabId(key)}
              type="button"
              role="tab"
              aria-selected={isSelected}
              aria-controls={panelId(key)}
              tabIndex={isSelected ? 0 : -1}
              onClick={() => setTab(key)}
              onKeyDown={(e) => handleTabKeyDown(e, index)}
              className={`cursor-pointer rounded-chip px-3 py-1.5 text-small font-medium transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
                isSelected
                  ? "bg-background text-text shadow-2xs ring-1 ring-inset ring-line"
                  : "text-ink-2 hover:text-text"
              }`}
            >
              {tabLabels[key]}
            </button>
          );
        })}
      </div>

      {tab === "toneAndSnippets" ? (
        <div
          id={panelId("toneAndSnippets")}
          role="tabpanel"
          aria-labelledby={tabId("toneAndSnippets")}
          className="space-y-6"
        >
          <SettingsGroup
            title={t("studio.voice.pipelineGroup", {
              defaultValue: "Linguist Pipeline",
            })}
          >
            <FillerWordRemoval descriptionMode="inline" grouped={true} />
            {postProcessEnabled && (
              <PostProcessingToggle descriptionMode="inline" grouped={true} />
            )}
          </SettingsGroup>

          <PerAppToneSettings />
        </div>
      ) : (
        <div
          id={panelId("linguistEngine")}
          role="tabpanel"
          aria-labelledby={tabId("linguistEngine")}
          className="space-y-6"
        >
          <div className="space-y-2">
            <PostProcessingToggle descriptionMode="inline" grouped={false} />
            {!postProcessEnabled && (
              <p className="px-1 text-xs text-ink-2" role="status">
                {t("studio.voice.stageANotice", {
                  defaultValue:
                    "Stage-A deterministic cleanup (<1ms) is active. Enable post-processing to unlock Stage-B local or API LLM rewrites.",
                })}
              </p>
            )}
          </div>

          {prompts.length > 0 && (
            <div className="rounded-lg border border-line bg-background p-4 space-y-2.5">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <h2 className="text-sm font-semibold text-text">
                  {t("studio.voice.transformPresetsTitle", {
                    defaultValue: "Active Transform Preset",
                  })}
                </h2>
                {transformShortcut && (
                  <kbd className="rounded border border-line bg-wash px-1.5 py-0.5 font-mono text-[11px] text-ink-2">
                    {transformShortcut
                      .replace(/^option\+/i, "⌥")
                      .replace(/^alt\+/i, "Alt+")}
                  </kbd>
                )}
              </div>
              <p className="text-xs text-ink-2">
                {t("studio.voice.transformPresetsHint", {
                  defaultValue:
                    "Used automatically after dictation when AI Rewriting is on, or anytime on highlighted text via the Transform shortcut or recording pill.",
                })}
              </p>
              <div
                role="radiogroup"
                aria-label={t("studio.voice.transformPresetsTitle", {
                  defaultValue: "Active Transform Preset",
                })}
                className="flex flex-wrap items-center gap-1.5 pt-0.5"
              >
                {prompts.map((prompt) => {
                  const selected = prompt.id === activePromptId;
                  return (
                    <button
                      key={prompt.id}
                      type="button"
                      role="radio"
                      aria-checked={selected}
                      onClick={() => handleSelectPreset(prompt.id)}
                      className={`cursor-pointer rounded-chip border px-3 py-1.5 text-small font-medium transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
                        selected
                          ? "border-logo-primary bg-logo-primary/10 text-text"
                          : "border-line bg-wash text-ink-2 hover:text-text"
                      }`}
                    >
                      {formatPresetName(prompt.id, prompt.name)}
                    </button>
                  );
                })}
              </div>
            </div>
          )}

          <PostProcessingSettings />
        </div>
      )}
    </div>
  );
};

export default VoiceStyleWorkspace;
