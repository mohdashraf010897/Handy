import React, { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import { useFlowStore } from "@/stores/flowStore";
import { DEFAULT_FLOW_SETTINGS } from "@/components/flow/toneRules";
import type {
  EdgeDockPosition,
  FlowSettings,
  OverlayPosition,
  OverlayStyle,
} from "@/bindings";
import { OverlayNoticePreview } from "./OverlayNoticePreview";

interface ShowOverlayProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const hasTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const ShowOverlay: React.FC<ShowOverlayProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const flowSettings = useFlowStore((s) => s.settings);
    const isSavingFlow = useFlowStore((s) => s.isSaving);
    const loadFlow = useFlowStore((s) => s.load);
    const saveFlow = useFlowStore((s) => s.save);

    useEffect(() => {
      if (!flowSettings && hasTauri) {
        void loadFlow();
      }
      if (!hasTauri) return;
      const unlisten = listen<FlowSettings>("flow-settings-changed", (ev) => {
        useFlowStore.setState({ settings: ev.payload });
      });
      return () => {
        void unlisten.then((fn) => fn());
      };
    }, [flowSettings, loadFlow]);

    const effectiveFlow = flowSettings ?? DEFAULT_FLOW_SETTINGS;
    const edgeDockSelectedValue = effectiveFlow.edge_dock_enabled
      ? effectiveFlow.edge_dock_position
      : "off";
    const edgeDockOptions = [
      {
        value: "left",
        label: t("settings.advanced.edgeDock.positions.left"),
      },
      {
        value: "bottom",
        label: t("settings.advanced.edgeDock.positions.bottom"),
      },
      {
        value: "right",
        label: t("settings.advanced.edgeDock.positions.right"),
      },
      {
        value: "off",
        label: t("settings.advanced.edgeDock.positions.off"),
      },
    ];

    const handleEdgeDockSelect = (value: string) => {
      if (!flowSettings && !hasTauri) {
        useFlowStore.setState({
          settings:
            value === "off"
              ? { ...effectiveFlow, edge_dock_enabled: false }
              : {
                  ...effectiveFlow,
                  edge_dock_enabled: true,
                  edge_dock_position: value as EdgeDockPosition,
                },
        });
        return;
      }
      void saveFlow((prev) =>
        value === "off"
          ? { ...prev, edge_dock_enabled: false }
          : {
              ...prev,
              edge_dock_enabled: true,
              edge_dock_position: value as EdgeDockPosition,
            },
      );
    };

    const minimalOptionLabel = t("settings.advanced.overlay.style.options.minimal");
    const styleOptions = [
      {
        value: "none",
        label: t("settings.advanced.overlay.style.options.none"),
      },
      {
        value: "minimal",
        label:
          minimalOptionLabel === "Minimal"
            ? "Minimal (Calm waveform)"
            : minimalOptionLabel,
      },
      {
        value: "live",
        label: t("settings.advanced.overlay.style.options.live"),
      },
    ];

    const positionOptions = [
      {
        value: "bottom",
        label: t("settings.advanced.overlay.position.options.bottom"),
      },
      {
        value: "top",
        label: t("settings.advanced.overlay.position.options.top"),
      },
    ];

    const selectedStyle = (getSetting("overlay_style") ||
      "minimal") as OverlayStyle;
    // Only "top" and "bottom" are selectable; anything else (empty, or a legacy
    // "none" from before the position was retired) falls back to "bottom".
    const selectedPosition: OverlayPosition =
      getSetting("overlay_position") === "top" ? "top" : "bottom";

    return (
      <>
        <SettingContainer
          title={t("settings.advanced.overlay.style.title")}
          description={t("settings.advanced.overlay.style.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Dropdown
            options={styleOptions}
            selectedValue={selectedStyle}
            onSelect={(value) =>
              updateSetting("overlay_style", value as OverlayStyle)
            }
            disabled={isUpdating("overlay_style")}
          />
        </SettingContainer>

        {selectedStyle !== "none" && (
          <>
            <SettingContainer
              title={t("settings.advanced.overlay.position.title")}
              description={t("settings.advanced.overlay.position.description")}
              descriptionMode={descriptionMode}
              grouped={grouped}
            >
              <Dropdown
                options={positionOptions}
                selectedValue={selectedPosition}
                onSelect={(value) =>
                  updateSetting("overlay_position", value as OverlayPosition)
                }
                disabled={isUpdating("overlay_position")}
              />
            </SettingContainer>

            <OverlayNoticePreview
              descriptionMode={descriptionMode}
              grouped={grouped}
            />
          </>
        )}
        <SettingContainer
          title={t("settings.advanced.edgeDock.title")}
          description={t("settings.advanced.edgeDock.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Dropdown
            options={edgeDockOptions}
            selectedValue={edgeDockSelectedValue}
            onSelect={handleEdgeDockSelect}
            disabled={isSavingFlow}
          />
        </SettingContainer>
      </>
    );
  },
);
