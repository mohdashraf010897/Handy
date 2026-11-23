import React, { useEffect, useId, useRef, useState } from "react";
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

export const VoiceStyleWorkspace: React.FC<VoiceStyleWorkspaceProps> = ({
  initialTab = "toneAndSnippets",
}) => {
  const { t, i18n } = useTranslation();
  const { getSetting } = useSettings();
  const [tab, setTab] = useState<VoiceStyleTab>(initialTab);
  const baseId = useId();
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  useEffect(() => {
    setTab(initialTab);
  }, [initialTab]);

  // PerAppToneSettings already renders PostProcessingToggle inside its own card
  // when post_process_enabled is false; only render it in the pipeline group
  // when enabled so the toggle appears once in every state.
  const postProcessEnabled = getSetting("post_process_enabled") ?? false;

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

          <PostProcessingSettings />
        </div>
      )}
    </div>
  );
};

export default VoiceStyleWorkspace;
