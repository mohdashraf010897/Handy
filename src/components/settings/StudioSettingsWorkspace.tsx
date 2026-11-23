import React, { useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";
import { isRTLLanguage } from "@/lib/utils/rtl";
import { SettingsGroup } from "../ui/SettingsGroup";
import { AboutSettings } from "./about/AboutSettings";
import { AdvancedSettings } from "./advanced/AdvancedSettings";
import { AppLanguageSelector } from "./AppLanguageSelector";
import { DebugSettings } from "./debug/DebugSettings";
import { GeneralSettings } from "./general/GeneralSettings";
import { ModelsSettings } from "./models/ModelsSettings";
import { ThemeSelector } from "./ThemeSelector";

export type StudioSettingsTab =
  | "shortcuts"
  | "models"
  | "system"
  | "about"
  | "debug";

export interface StudioSettingsWorkspaceProps {
  initialTab?: StudioSettingsTab;
  onNavigateToLexicon?: () => void;
  onPreviewOnboarding: (step: any) => void;
  onRunOnboarding: () => void;
  onRunModelMigration: () => void;
}

export const StudioSettingsWorkspace: React.FC<
  StudioSettingsWorkspaceProps
> = ({
  initialTab = "shortcuts",
  onNavigateToLexicon,
  onPreviewOnboarding,
  onRunOnboarding,
  onRunModelMigration,
}) => {
  const { t, i18n } = useTranslation();
  const { settings } = useSettings();
  const [tab, setTab] = useState<StudioSettingsTab>(initialTab);
  const baseId = useId();
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  useEffect(() => {
    setTab(initialTab);
  }, [initialTab]);

  const debugEnabled = settings?.debug_mode ?? false;

  const visibleTabs = useMemo<StudioSettingsTab[]>(
    () =>
      debugEnabled
        ? ["shortcuts", "models", "system", "about", "debug"]
        : ["shortcuts", "models", "system", "about"],
    [debugEnabled],
  );

  const activeTab: StudioSettingsTab =
    tab === "debug" && !debugEnabled ? "shortcuts" : tab;

  const tabLabels: Record<StudioSettingsTab, string> = {
    shortcuts: t("studio.settings.tabs.shortcuts", {
      defaultValue: "Shortcuts & Audio",
    }),
    models: t("studio.settings.tabs.models", {
      defaultValue: "Engines & Models",
    }),
    system: t("studio.settings.tabs.system", {
      defaultValue: "System & Vault",
    }),
    about: t("studio.settings.tabs.about", {
      defaultValue: "About",
    }),
    debug: t("studio.settings.tabs.debug", {
      defaultValue: "Debug",
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
      nextIndex = (index + 1) % visibleTabs.length;
    } else if (event.key === backwardKey) {
      nextIndex = (index - 1 + visibleTabs.length) % visibleTabs.length;
    } else if (event.key === "Home") {
      nextIndex = 0;
    } else if (event.key === "End") {
      nextIndex = visibleTabs.length - 1;
    }

    if (nextIndex !== -1) {
      event.preventDefault();
      const nextTab = visibleTabs[nextIndex];
      setTab(nextTab);
      tabRefs.current[nextIndex]?.focus();
    }
  };

  const tabId = (key: StudioSettingsTab) => `${baseId}-tab-${key}`;
  const panelId = (key: StudioSettingsTab) => `${baseId}-panel-${key}`;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <header className="border-b border-line px-1 pb-3">
        <h1 className="font-display text-2xl font-semibold tracking-tight text-text">
          {t("studio.settings.title", { defaultValue: "Settings" })}
        </h1>
      </header>

      <div
        role="tablist"
        aria-label={t("studio.settings.title", { defaultValue: "Settings" })}
        className="inline-flex flex-wrap items-center gap-1 rounded-control border border-line bg-wash p-1"
      >
        {visibleTabs.map((key, index) => {
          const isSelected = activeTab === key;
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

      <div
        id={panelId(activeTab)}
        role="tabpanel"
        aria-labelledby={tabId(activeTab)}
        className="space-y-6"
      >
        {activeTab === "shortcuts" && <GeneralSettings studioScoped />}

        {activeTab === "models" && (
          <ModelsSettings
            onChangeLanguages={() =>
              onNavigateToLexicon ? onNavigateToLexicon() : setTab("shortcuts")
            }
          />
        )}

        {activeTab === "system" && (
          <>
            <SettingsGroup
              title={t("studio.settings.appearanceGroup", {
                defaultValue: "Interface & Appearance",
              })}
            >
              <AppLanguageSelector descriptionMode="tooltip" grouped={true} />
              <ThemeSelector descriptionMode="tooltip" grouped={true} />
            </SettingsGroup>
            <AdvancedSettings studioScoped />
          </>
        )}

        {activeTab === "about" && <AboutSettings studioScoped />}

        {activeTab === "debug" && debugEnabled && (
          <DebugSettings
            onPreviewOnboarding={onPreviewOnboarding}
            onRunOnboarding={onRunOnboarding}
            onRunModelMigration={onRunModelMigration}
          />
        )}
      </div>
    </div>
  );
};

export default StudioSettingsWorkspace;
