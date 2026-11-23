import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";
import { isChinese } from "@/lib/utils/languageSuggestions";
import { SettingsGroup } from "../ui/SettingsGroup";
import { ChineseScriptSetting } from "../settings/ChineseScript";
import { CustomWords } from "../settings/CustomWords";
import { ModelSettingsCard } from "../settings/general/ModelSettingsCard";
import { HindustaniScriptSelector } from "../settings/HindustaniScriptSelector";
import { PreferredLanguages } from "../settings/PreferredLanguages";

export const LexiconWorkspace: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();

  const speaksChinese =
    (getSetting("spoken_languages") ?? []).some(isChinese) ||
    (getSetting("chinese_script") ?? "as_transcribed") !== "as_transcribed";

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <header className="flex flex-wrap items-baseline justify-between gap-3 border-b border-line px-1 pb-3">
        <h1 className="font-display text-2xl font-semibold tracking-tight text-text">
          {t("studio.lexicon.title", { defaultValue: "Lexicon" })}
        </h1>
        <p className="text-end font-mono text-xs text-ink-3">
          {t("studio.lexicon.subtitle", {
            defaultValue: "Custom vocabulary · spoken languages · script rules",
          })}
        </p>
      </header>

      <SettingsGroup
        title={t("studio.lexicon.vocabularyGroup", {
          defaultValue: "Custom Vocabulary",
        })}
      >
        <CustomWords descriptionMode="inline" grouped={true} />
      </SettingsGroup>

      <SettingsGroup
        title={t("studio.lexicon.languagesGroup", {
          defaultValue: "Spoken Languages & Script Lock",
        })}
      >
        <PreferredLanguages descriptionMode="inline" grouped={true} />
        <HindustaniScriptSelector descriptionMode="inline" grouped={true} />
        {speaksChinese && (
          <ChineseScriptSetting descriptionMode="inline" grouped={true} />
        )}
      </SettingsGroup>

      <ModelSettingsCard />
    </div>
  );
};

export default LexiconWorkspace;
