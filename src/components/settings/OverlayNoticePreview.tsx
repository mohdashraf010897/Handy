import { invoke } from "@tauri-apps/api/core";
import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { SettingContainer } from "../ui/SettingContainer";

export type OverlayNoticePreviewKind =
  | "mic_muted"
  | "no_speech"
  | "mic_permission"
  | "no_mic"
  | "restart_required"
  | "transcription_failed";

interface OverlayNoticePreviewProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const PREVIEW_HIGHLIGHT_MS = 2800;

export const OverlayNoticePreview: React.FC<OverlayNoticePreviewProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const [activeKind, setActiveKind] =
      useState<OverlayNoticePreviewKind | null>(null);
    const timerRef = useRef<ReturnType<typeof setTimeout>>();

    useEffect(() => {
      return () => {
        clearTimeout(timerRef.current);
      };
    }, []);

    const chips: Array<{
      kind: OverlayNoticePreviewKind;
      label: string;
    }> = [
      {
        kind: "mic_muted",
        label: t("settings.advanced.overlay.noticePreview.chips.micMuted", {
          defaultValue: "Mic muted",
        }),
      },
      {
        kind: "no_speech",
        label: t("settings.advanced.overlay.noticePreview.chips.noSpeech", {
          defaultValue: "No speech",
        }),
      },
      {
        kind: "mic_permission",
        label: t(
          "settings.advanced.overlay.noticePreview.chips.micPermission",
          {
            defaultValue: "Mic permission",
          },
        ),
      },
      {
        kind: "no_mic",
        label: t("settings.advanced.overlay.noticePreview.chips.noMic", {
          defaultValue: "No mic found",
        }),
      },
      {
        kind: "restart_required",
        label: t(
          "settings.advanced.overlay.noticePreview.chips.restartRequired",
          {
            defaultValue: "App updated",
          },
        ),
      },
    ];

    const showingLabel = t("settings.advanced.overlay.noticePreview.showing", {
      defaultValue: "Showing on screen…",
    });

    const handlePreview = async (kind: OverlayNoticePreviewKind) => {
      clearTimeout(timerRef.current);
      setActiveKind(kind);
      timerRef.current = setTimeout(() => {
        setActiveKind((prev) => (prev === kind ? null : prev));
      }, PREVIEW_HIGHLIGHT_MS);

      try {
        await invoke<void>("preview_overlay_notice", { kind });
      } catch (error) {
        console.error("Failed to preview overlay notice:", error);
      }
    };

    return (
      <SettingContainer
        title={t("settings.advanced.overlay.noticePreview.title", {
          defaultValue: "Preview status notices",
        })}
        description={t("settings.advanced.overlay.noticePreview.description", {
          defaultValue:
            "Test how menu-bar alerts appear on screen when your mic is muted, no speech is heard, or lowyap needs attention.",
        })}
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="stacked"
      >
        <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
          {chips.map(({ kind, label }) => {
            const isActive = activeKind === kind;
            return (
              <button
                key={kind}
                type="button"
                data-testid={`preview-notice-${kind}`}
                aria-pressed={isActive}
                onClick={() => void handlePreview(kind)}
                className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium transition-colors cursor-pointer focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
                  isActive
                    ? "border-error/50 bg-error-wash text-text"
                    : "border-line-strong bg-wash text-text hover:border-logo-primary hover:bg-logo-primary/10"
                }`}
              >
                <span
                  aria-hidden="true"
                  className={`h-1.5 w-1.5 shrink-0 rounded-full bg-[#e5534b] ${
                    isActive ? "animate-pulse motion-reduce:animate-none" : ""
                  }`}
                />
                <span>{isActive ? showingLabel : label}</span>
              </button>
            );
          })}
        </div>
      </SettingContainer>
    );
  });

export default OverlayNoticePreview;
