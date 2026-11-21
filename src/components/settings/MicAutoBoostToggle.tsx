import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import { DEFAULT_FLOW_SETTINGS } from "@/components/flow/toneRules";
import { useFlowStore } from "@/stores/flowStore";
import { SettingContainer } from "../ui/SettingContainer";
import { Switch } from "../ui/Switch";
import { ToggleSwitch } from "../ui/ToggleSwitch";

interface MicAutoBoostToggleProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const MicAutoBoostToggle: React.FC<MicAutoBoostToggleProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { settings, isLoading, isSaving, load, save } = useFlowStore();
    const [micVolume, setMicVolume] = useState<number | null>(null);

    const refreshMicVolume = useCallback(async () => {
      try {
        const vol = await commands.getSystemMicVolume();
        setMicVolume(
          typeof vol === "number" && Number.isFinite(vol)
            ? Math.round(vol)
            : null,
        );
      } catch {
        setMicVolume(null);
      }
    }, []);

    useEffect(() => {
      if (!settings) {
        void load();
      }
    }, [settings, load]);

    useEffect(() => {
      void refreshMicVolume();
      const onWindowFocus = () => {
        void refreshMicVolume();
      };
      window.addEventListener("focus", onWindowFocus);
      return () => {
        window.removeEventListener("focus", onWindowFocus);
      };
    }, [refreshMicVolume]);

    const current = settings ?? DEFAULT_FLOW_SETTINGS;
    const autoBoostEnabled = current.auto_boost_mic_volume;
    const quietAgcEnabled = current.quiet_speech_agc_enabled;
    const targetPercent = current.mic_boost_target_percent;
    const ready = settings !== null && !isLoading;

    const handleAutoBoostChange = (enabled: boolean) => {
      void save((s) => ({ ...s, auto_boost_mic_volume: enabled })).then(() =>
        refreshMicVolume(),
      );
    };

    const handleQuietAgcChange = (enabled: boolean) => {
      void save((s) => ({ ...s, quiet_speech_agc_enabled: enabled }));
    };

    const autoBoostLabel = t("settings.sound.micAutoBoost.label", {
      defaultValue: "Raise low mic volume automatically",
    });
    const autoBoostDescription = t("settings.sound.micAutoBoost.description", {
      defaultValue:
        "Lifts low Mac mic volume while speaking, then restores it.",
    });

    const liveBadgeText =
      micVolume === null
        ? null
        : autoBoostEnabled && micVolume < targetPercent
          ? t("settings.sound.micAutoBoost.liveBadgeBoost", {
              current: micVolume,
              target: targetPercent,
              defaultValue:
                "Current mic: {{current}}% → {{target}}% while speaking",
            })
          : t("settings.sound.micAutoBoost.liveBadgeCurrent", {
              current: micVolume,
              defaultValue: "Current mic: {{current}}%",
            });

    const rows = (
      <>
        <SettingContainer
          title={autoBoostLabel}
          description={autoBoostDescription}
          descriptionMode={descriptionMode}
          grouped={grouped}
          disabled={!ready}
        >
          <div
            data-testid="mic-auto-boost-toggle"
            className="flex items-center gap-2.5"
          >
            {liveBadgeText && (
              <span
                data-testid="mic-auto-boost-live-badge"
                className="inline-flex items-center rounded-full border border-line bg-wash px-2.5 py-0.5 text-xs font-medium text-ink-2 whitespace-nowrap tabular-nums"
              >
                {liveBadgeText}
              </span>
            )}
            <Switch
              checked={autoBoostEnabled}
              onChange={handleAutoBoostChange}
              disabled={!ready || isSaving}
              ariaLabel={autoBoostLabel}
            />
          </div>
        </SettingContainer>

        <ToggleSwitch
          checked={quietAgcEnabled}
          onChange={handleQuietAgcChange}
          disabled={!ready}
          isUpdating={isSaving}
          label={t("settings.sound.quietSpeechAgc.label", {
            defaultValue: "Boost quiet voices automatically (AGC)",
          })}
          description={t("settings.sound.quietSpeechAgc.description", {
            defaultValue: "Lifts soft speech automatically.",
          })}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
      </>
    );

    return grouped ? rows : <div className="space-y-2">{rows}</div>;
  },
);

export default MicAutoBoostToggle;
