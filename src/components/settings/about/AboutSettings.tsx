import React, { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { SettingContainer } from "../../ui/SettingContainer";
import { Button } from "../../ui/Button";
import HandyTextLogo from "../../icons/HandyTextLogo";
import { AppDataDirectory } from "../AppDataDirectory";
import { AppLanguageSelector } from "../AppLanguageSelector";
import { ShowWhatsNewOnUpdate } from "../ShowWhatsNewOnUpdate";
import { ThemeSelector } from "../ThemeSelector";
import { LogDirectory } from "../debug";
import {
  BRAND_HOMEPAGE,
  BRAND_LICENSE,
  BRAND_NOTICES,
  BRAND_TAGLINE,
  UPSTREAM_HANDY,
} from "../../../lib/brand";

const TM_SYMBOL = "\u2122";
const VERSION_FALLBACK = "\u2014";

export const AboutSettings: React.FC = () => {
  const { t } = useTranslation();
  const [version, setVersion] = useState("");

  useEffect(() => {
    const fetchVersion = async () => {
      try {
        const appVersion = await getVersion();
        setVersion(appVersion);
      } catch (error) {
        console.error("Failed to get app version:", error);
      }
    };

    fetchVersion();
  }, []);

  const versionLabel = version ? `v${version}` : VERSION_FALLBACK;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="rounded-xl border border-line bg-wash/60 px-5 py-4 flex flex-col gap-3">
        <div className="flex items-center justify-between gap-4 flex-wrap">
          <div className="flex items-center gap-3.5">
            <div
              className="w-12 h-12 rounded-[11px] shrink-0 flex items-center justify-center shadow-xs border border-white/10"
              style={{
                background: "linear-gradient(180deg, #24222E 0%, #121117 100%)",
              }}
              aria-hidden="true"
            >
              <svg width="30" height="30" viewBox="0 0 64 64" fill="none">
                <path
                  fill="#7C5CFF"
                  d="M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6Z"
                />
                <path
                  fill="#FFFFFF"
                  d="M23 30H41A4 4 0 0 1 41 38H23A4 4 0 0 1 23 30Z"
                />
              </svg>
            </div>
            <div className="flex flex-col gap-0.5">
              <div className="flex items-start gap-0.5">
                <HandyTextLogo width={96} className="block" />
                <span className="text-[10px] leading-none font-medium text-ink-3 select-none">
                  {TM_SYMBOL}
                </span>
              </div>
              <p className="font-mono text-xs text-ink-2">{BRAND_TAGLINE}</p>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <span className="px-2 py-0.5 rounded-md border border-line bg-background text-xs font-mono text-ink-2 tabular-nums">
              {versionLabel}
            </span>
            <span className="px-2 py-0.5 rounded-md border border-line bg-background text-xs font-mono text-ink-2">
              {BRAND_LICENSE}
            </span>
          </div>
        </div>
        <p className="text-xs leading-relaxed text-ink-2">
          {t("flow.about.privacySummary")}
        </p>
      </div>

      <SettingsGroup title={t("settings.about.title")}>
        <AppLanguageSelector descriptionMode="tooltip" grouped={true} />
        <ThemeSelector descriptionMode="tooltip" grouped={true} />
        <SettingContainer
          title={t("settings.about.version.title")}
          description={t("settings.about.version.description")}
          grouped={true}
        >
          <span className="text-sm font-mono tabular-nums">{versionLabel}</span>
        </SettingContainer>

        <ShowWhatsNewOnUpdate descriptionMode="tooltip" grouped={true} />

        <SettingContainer
          title={t("settings.about.sourceCode.title")}
          description={t("settings.about.sourceCode.description")}
          grouped={true}
        >
          <Button
            variant="secondary"
            size="md"
            onClick={() => openUrl(BRAND_HOMEPAGE)}
          >
            {t("settings.about.sourceCode.button")}
          </Button>
        </SettingContainer>

        <AppDataDirectory descriptionMode="tooltip" grouped={true} />
        <LogDirectory grouped={true} />
      </SettingsGroup>

      <SettingsGroup title={t("settings.about.acknowledgments.title")}>
        <SettingContainer
          title={t("settings.about.acknowledgments.ggml.title")}
          description={t("settings.about.acknowledgments.ggml.description")}
          grouped={true}
          layout="stacked"
        >
          <div className="text-sm text-ink-3">
            {t("settings.about.acknowledgments.ggml.details")}
          </div>
        </SettingContainer>
        <SettingContainer
          title={t("settings.about.acknowledgments.handy.title")}
          description={t("settings.about.acknowledgments.handy.description")}
          grouped={true}
          layout="stacked"
        >
          <div className="flex flex-col items-start gap-2">
            <div className="text-sm text-ink-3">
              {t("settings.about.acknowledgments.handy.details")}
            </div>
            <Button
              variant="secondary"
              size="md"
              onClick={() => openUrl(UPSTREAM_HANDY)}
            >
              {t("settings.about.sourceCode.button")}
            </Button>
          </div>
        </SettingContainer>
        <SettingContainer
          title={t("settings.about.acknowledgments.notices.title")}
          description={t("settings.about.acknowledgments.notices.description")}
          grouped={true}
        >
          <Button
            variant="secondary"
            size="md"
            onClick={() => openUrl(BRAND_NOTICES)}
          >
            {t("settings.about.sourceCode.button")}
          </Button>
        </SettingContainer>
      </SettingsGroup>
    </div>
  );
};
