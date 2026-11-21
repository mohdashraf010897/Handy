import React, { useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { HindustaniScript } from "@/bindings";
import { DEFAULT_FLOW_SETTINGS } from "@/components/flow/toneRules";
import { useFlowStore } from "@/stores/flowStore";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";

export type { HindustaniScript };

interface HindustaniScriptSelectorProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const HindustaniScriptSelector: React.FC<HindustaniScriptSelectorProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { settings, isLoading, isSaving, load, save } = useFlowStore();

    useEffect(() => {
      if (!settings) {
        void load();
      }
    }, [settings, load]);

    const current = settings ?? DEFAULT_FLOW_SETTINGS;
    const selectedScript = current.hindustani_script;

    const scriptOptions: Array<{ value: HindustaniScript; label: string }> = [
      {
        value: "auto",
        label: t("settings.languages.hindustaniScript.options.auto", {
          defaultValue: "Automatic (follow Spoken Languages)",
        }),
      },
      {
        value: "roman_hinglish",
        label: t("settings.languages.hindustaniScript.options.romanHinglish", {
          defaultValue: "Roman Hinglish (Haan, let's check this)",
        }),
      },
      {
        value: "urdu_perso_arabic",
        label: t(
          "settings.languages.hindustaniScript.options.urduPersoArabic",
          {
            defaultValue: "Urdu & Arabic script (\u2068اردو · عربي\u2069)",
          },
        ),
      },
      {
        value: "devanagari",
        label: t("settings.languages.hindustaniScript.options.devanagari", {
          defaultValue: "Hindi Devanagari (हिन्दी)",
        }),
      },
    ];

    const handleSelect = (value: string) => {
      const next = value as HindustaniScript;
      if (next === selectedScript) return;
      void save((s) => ({ ...s, hindustani_script: next }));
    };

    const ready = settings !== null && !isLoading;

    return (
      <SettingContainer
        title={t("settings.languages.hindustaniScript.title", {
          defaultValue: "Urdu, Hindi & Hinglish writing script",
        })}
        description={t("settings.languages.hindustaniScript.description", {
          defaultValue:
            "Urdu and Hindi sound alike; lock your script to avoid unwanted Devanagari.",
        })}
        descriptionMode={descriptionMode}
        grouped={grouped}
        disabled={!ready}
      >
        <div data-testid="hindustani-script-selector">
          <Dropdown
            className="min-w-[260px]"
            menuClassName="right-0 min-w-[280px]"
            options={scriptOptions}
            selectedValue={selectedScript}
            onSelect={handleSelect}
            disabled={!ready || isSaving}
          />
        </div>
      </SettingContainer>
    );
  });

export default HindustaniScriptSelector;
