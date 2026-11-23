import React from "react";
import { useTranslation } from "react-i18next";
import {
  AlignLeft,
  BookOpen,
  Settings,
  SlidersHorizontal,
} from "lucide-react";
import HandyTextLogo from "./icons/HandyTextLogo";
import VocalKeycapMark from "./icons/VocalKeycapMark";
import { useOsType } from "@/hooks/useOsType";
import { useSettings } from "@/hooks/useSettings";
import { resolveStudioEgressState } from "@/lib/utils/egressState";
import { useModelStore } from "@/stores/modelStore";
import { MEETINGS_SECTION } from "./meetings";
import type { StudioSettingsTab } from "./settings/StudioSettingsWorkspace";

export type SidebarSection =
  | "desk"
  | "meetings"
  | "lexicon"
  | "voice"
  | "settings";

interface IconProps {
  width?: number | string;
  height?: number | string;
  size?: number | string;
  strokeWidth?: number | string;
  className?: string;
  [key: string]: unknown;
}

interface StudioNavEntry {
  id: Exclude<SidebarSection, "settings">;
  labelKey: string;
  defaultLabel: string;
  icon: React.ComponentType<IconProps>;
}

export const STUDIO_NAV_ITEMS: readonly StudioNavEntry[] = [
  {
    id: "desk",
    labelKey: "studio.sidebar.desk",
    defaultLabel: "Desk",
    icon: AlignLeft,
  },
  {
    id: "meetings",
    labelKey: MEETINGS_SECTION.labelKey,
    defaultLabel: "Meetings",
    icon: MEETINGS_SECTION.icon,
  },
  {
    id: "lexicon",
    labelKey: "studio.sidebar.lexicon",
    defaultLabel: "Lexicon",
    icon: BookOpen,
  },
  {
    id: "voice",
    labelKey: "studio.sidebar.voice",
    defaultLabel: "Voice & Style",
    icon: SlidersHorizontal,
  },
] as const;

/**
 * Maps both primary Studio section ids and legacy section payloads
 * (e.g. from backend events or shortcuts) onto the 5-surface Studio shell.
 */
export function resolveStudioNavigation(raw: string): {
  section: SidebarSection;
  settingsTab?: StudioSettingsTab;
} | null {
  switch (raw) {
    case "desk":
    case "history":
      return { section: "desk" };
    case "meetings":
      return { section: "meetings" };
    case "lexicon":
      return { section: "lexicon" };
    case "voice":
    case "flowTone":
    case "postprocessing":
      return { section: "voice" };
    case "settings":
    case "general":
      return { section: "settings", settingsTab: "shortcuts" };
    case "models":
      return { section: "settings", settingsTab: "models" };
    case "advanced":
      return { section: "settings", settingsTab: "system" };
    case "about":
      return { section: "settings", settingsTab: "about" };
    case "debug":
      return { section: "settings", settingsTab: "debug" };
    default:
      return null;
  }
}

interface SidebarProps {
  activeSection: SidebarSection;
  onSectionChange: (
    section: SidebarSection,
    settingsTab?: StudioSettingsTab,
  ) => void;
}

export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
}) => {
  const { t } = useTranslation();
  const osType = useOsType();
  const { settings } = useSettings();
  const { currentModel, models } = useModelStore();
  const activeModelInfo = models.find((m) => m.id === currentModel);
  const egressState = resolveStudioEgressState(activeModelInfo, settings);
  const settingsShortcutLabel = osType === "macos" ? "⌘," : "Ctrl+,";

  return (
    <aside className="flex h-full w-[208px] shrink-0 flex-col justify-between border-e border-line bg-rail px-3 pb-3 select-none">
      <div className="flex flex-col">
        {osType === "macos" ? (
          <div
            data-tauri-drag-region
            className="h-[38px] w-full shrink-0"
            aria-hidden="true"
          />
        ) : (
          <div className="h-3 w-full shrink-0" aria-hidden="true" />
        )}

        <div data-tauri-drag-region className="px-2 pb-4 pt-0.5">
          <div className="flex items-center gap-2">
            <VocalKeycapMark size={22} className="shrink-0" />
            <HandyTextLogo width={90} className="text-text" />
          </div>
          <p className="mt-1.5 font-mono text-[10.5px] leading-snug text-ink-3">
            {t("studio.sidebar.tagline", {
              defaultValue: "lowkey yap. it types.",
            })}
          </p>
        </div>

        <nav
          aria-label={t("studio.sidebar.navAria", {
            defaultValue: "Studio navigation",
          })}
          className="flex flex-col gap-1"
        >
          {STUDIO_NAV_ITEMS.map((item) => {
            const Icon = item.icon;
            const isActive = activeSection === item.id;
            const label = t(item.labelKey, { defaultValue: item.defaultLabel });

            return (
              <button
                key={item.id}
                type="button"
                aria-current={isActive ? "page" : undefined}
                onClick={() => onSectionChange(item.id)}
                className={`flex w-full cursor-pointer items-center justify-between gap-2 rounded-lg px-2.5 py-2 text-start text-[13px] focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
                  isActive
                    ? "bg-surface font-semibold text-text shadow-2xs ring-1 ring-inset ring-line"
                    : "font-medium text-ink-2 hover:bg-wash hover:text-text"
                }`}
              >
                <span className="flex min-w-0 items-center gap-2.5">
                  <Icon
                    size={16}
                    width={16}
                    height={16}
                    strokeWidth={1.85}
                    className="shrink-0"
                  />
                  <span className="truncate" title={label}>
                    {label}
                  </span>
                </span>
                {isActive && (
                  <span
                    aria-hidden="true"
                    className="h-[3px] w-[11px] shrink-0 rounded-full bg-logo-primary"
                  />
                )}
              </button>
            );
          })}
        </nav>
      </div>

      <div className="flex flex-col gap-2.5 border-t border-line pt-2.5">
        <button
          type="button"
          aria-current={activeSection === "settings" ? "page" : undefined}
          onClick={() => onSectionChange("settings", "shortcuts")}
          className={`flex w-full cursor-pointer items-center justify-between gap-2 rounded-lg px-2.5 py-2 text-start text-[13px] focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
            activeSection === "settings"
              ? "bg-surface font-semibold text-text shadow-2xs ring-1 ring-inset ring-line"
              : "font-medium text-ink-2 hover:bg-wash hover:text-text"
          }`}
        >
          <span className="flex min-w-0 items-center gap-2.5">
            <Settings size={16} strokeWidth={1.85} className="shrink-0" />
            <span className="truncate">
              {t("studio.sidebar.settings", { defaultValue: "Settings" })}
            </span>
          </span>
          {activeSection === "settings" ? (
            <span
              aria-hidden="true"
              className="h-[3px] w-[11px] shrink-0 rounded-full bg-logo-primary"
            />
          ) : (
            <kbd className="font-mono text-[11px] font-normal text-ink-3">
              {settingsShortcutLabel}
            </kbd>
          )}
        </button>

        <div className="rounded-lg border border-line bg-surface px-2.5 py-2">
          <div className="flex items-center justify-between gap-1.5 text-[11px]">
            <span className="truncate font-medium text-text">
              {t("studio.sidebar.vaultLabel", {
                defaultValue: "SQLite Vault",
              })}
            </span>
            <span
              className={`shrink-0 font-mono font-semibold ${
                egressState.hasCloudEgress ? "text-warning" : "text-moss"
              }`}
            >
              {egressState.cloudAsr
                ? t("studio.sidebar.cloudMode", { defaultValue: "Cloud ASR" })
                : egressState.cloudLinguist
                  ? t("studio.sidebar.cloudLlmMode", {
                      defaultValue: "Cloud LLM",
                    })
                  : t("studio.sidebar.zeroSent", { defaultValue: "0 B sent" })}
            </span>
          </div>
          <p className="mt-0.5 truncate text-[11px] text-ink-3">
            {egressState.hasCloudEgress
              ? t("studio.sidebar.vaultSubCloud", {
                  defaultValue: "Local vault · cloud active",
                })
              : t("studio.sidebar.vaultSub", {
                  defaultValue: "Private on-device studio",
                })}
          </p>
        </div>
      </div>
    </aside>
  );
};
