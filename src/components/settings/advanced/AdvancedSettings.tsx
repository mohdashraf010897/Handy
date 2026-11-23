import React, { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { ShowOverlay } from "../ShowOverlay";
import { ModelUnloadTimeoutSetting } from "../ModelUnloadTimeout";
import { CustomWords } from "../CustomWords";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { SettingContainer } from "../../ui/SettingContainer";
import { StartHidden } from "../StartHidden";
import { AutostartToggle } from "../AutostartToggle";
import { ShowTrayIcon } from "../ShowTrayIcon";
import { PasteMethodSetting } from "../PasteMethod";
import { TypingToolSetting } from "../TypingTool";
import { ClipboardHandlingSetting } from "../ClipboardHandling";
import { AutoSubmit } from "../AutoSubmit";
import { PostProcessingToggle } from "../PostProcessingToggle";
import { AppendTrailingSpace } from "../AppendTrailingSpace";
import { HistoryLimit } from "../HistoryLimit";
import { RecordingRetentionPeriodSelector } from "../RecordingRetentionPeriod";
import { ExperimentalToggle } from "../ExperimentalToggle";
import { useSettings } from "../../../hooks/useSettings";
import { KeyboardImplementationSelector } from "../debug/KeyboardImplementationSelector";
import { VoiceActivityDetection } from "../VoiceActivityDetection";
import { LongFormChunking } from "../LongFormChunking";
import { LanguageDetection } from "../LanguageDetection";
import { AccelerationSelector } from "../AccelerationSelector";
import { LazyStreamClose } from "../LazyStreamClose";
import { FillerWordRemoval } from "../FillerWordRemoval";
import { VadBackendSelector } from "../VadBackendSelector";

export interface AdvancedSettingsProps {
  studioScoped?: boolean;
}

export const AdvancedSettings: React.FC<AdvancedSettingsProps> = ({
  studioScoped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const experimentalEnabled = getSetting("experimental_enabled") || false;
  const [previewingCallPill, setPreviewingCallPill] = useState(false);

  const handlePreviewCallDetected = async () => {
    setPreviewingCallPill(true);
    setTimeout(() => setPreviewingCallPill(false), 2800);
    try {
      await invoke<void>("preview_overlay_notice", { kind: "call_detected" });
    } catch (error) {
      console.error("Failed to preview call_detected overlay notice:", error);
    }
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.advanced.groups.app")}>
        <StartHidden descriptionMode="tooltip" grouped={true} />
        <AutostartToggle descriptionMode="tooltip" grouped={true} />
        <ShowTrayIcon descriptionMode="tooltip" grouped={true} />
        <ShowOverlay descriptionMode="tooltip" grouped={true} />
        <SettingContainer
          title={t("settings.advanced.previewNotices.callDetected")}
          description={t("meetings.autoDetect.description")}
          descriptionMode="tooltip"
          grouped={true}
        >
          <button
            type="button"
            data-testid="preview-notice-call_detected"
            aria-label={t("settings.advanced.previewNotices.callDetected")}
            aria-pressed={previewingCallPill}
            onClick={() => void handlePreviewCallDetected()}
            className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium transition-colors cursor-pointer focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
              previewingCallPill
                ? "border-logo-primary bg-logo-primary/20 text-text"
                : "border-line-strong bg-wash text-text hover:border-logo-primary hover:bg-logo-primary/10"
            }`}
          >
            <span
              aria-hidden="true"
              className={`h-1.5 w-1.5 shrink-0 rounded-full bg-overlay-accent ${
                previewingCallPill
                  ? "animate-pulse motion-reduce:animate-none"
                  : ""
              }`}
            />
            <span>
              {previewingCallPill
                ? t("settings.advanced.overlay.noticePreview.showing")
                : t("settings.advanced.previewNotices.callDetected")}
            </span>
          </button>
        </SettingContainer>
        <ModelUnloadTimeoutSetting descriptionMode="tooltip" grouped={true} />
        <ExperimentalToggle descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>

      <SettingsGroup title={t("settings.advanced.groups.output")}>
        <PasteMethodSetting descriptionMode="tooltip" grouped={true} />
        <TypingToolSetting descriptionMode="tooltip" grouped={true} />
        <ClipboardHandlingSetting descriptionMode="tooltip" grouped={true} />
        <AutoSubmit descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>

      <SettingsGroup title={t("settings.advanced.groups.transcription")}>
        <VoiceActivityDetection descriptionMode="tooltip" grouped={true} />
        <LongFormChunking descriptionMode="tooltip" grouped={true} />
        {!studioScoped && (
          <>
            <FillerWordRemoval descriptionMode="tooltip" grouped={true} />
            <CustomWords descriptionMode="tooltip" grouped />
          </>
        )}
        <AppendTrailingSpace descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>

      <SettingsGroup title={t("settings.advanced.groups.history")}>
        <HistoryLimit descriptionMode="tooltip" grouped={true} />
        <RecordingRetentionPeriodSelector
          descriptionMode="tooltip"
          grouped={true}
        />
      </SettingsGroup>

      {experimentalEnabled && (
        <SettingsGroup title={t("settings.advanced.groups.experimental")}>
          {!studioScoped && (
            <PostProcessingToggle descriptionMode="tooltip" grouped={true} />
          )}
          <KeyboardImplementationSelector
            descriptionMode="tooltip"
            grouped={true}
          />
          <AccelerationSelector descriptionMode="tooltip" grouped={true} />
          <LazyStreamClose descriptionMode="tooltip" grouped={true} />
          <VadBackendSelector descriptionMode="tooltip" grouped={true} />
          <LanguageDetection descriptionMode="tooltip" grouped={true} />
        </SettingsGroup>
      )}
    </div>
  );
};
