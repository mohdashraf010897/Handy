import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ChevronDown, Download, RefreshCw } from "lucide-react";
import { Tooltip } from "@/components/ui/Tooltip";
import { useModelStore } from "@/stores/modelStore";
import { useSettings } from "@/hooks/useSettings";
import { useModelCatalog } from "@/hooks/useModelCatalog";
import { useLanguageDetector } from "@/hooks/useLanguageDetector";
import { routeAsrWithJev } from "@/lib/models/jevRouter";
import { suggestedLanguages } from "@/lib/utils/languageSuggestions";
import {
  ModelTable,
  Note,
  useModelFilters,
  type TableSection,
} from "@/components/shared/modelTable/ModelTable";
import RetiredModelsNotice from "./RetiredModelsNotice";
import { ModelRow } from "./ModelRow";
import { Legend } from "./Legend";
import {
  cancelDownload,
  pickModel,
  useModelEntries,
  usePlace,
} from "./modelEntries";

/** Past this many models on disk, the rest fold under "Show N more". */
const YOURS_LIMIT = 5;

/** Look for model files added outside Handy: quiet, beside its section's name. */
const Rescan: React.FC = () => {
  const { t } = useTranslation();
  const rescanning = useModelStore((s) => s.isRescanning);
  return (
    <Tooltip text={t("settings.models.rescan.tooltip")}>
      <button
        type="button"
        onClick={() => useModelStore.getState().rescanLocalModels()}
        disabled={rescanning}
        aria-label={t("settings.models.rescan.tooltip")}
        className="ms-[6px] grid size-[22px] cursor-pointer place-items-center rounded-full text-ink-3 hover:bg-wash hover:text-text disabled:cursor-default focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
      >
        <RefreshCw
          className={`size-[12px] ${rescanning ? "animate-spin" : ""}`}
          aria-hidden="true"
        />
      </button>
    </Tooltip>
  );
};

interface ModelsSettingsProps {
  /** Where the languages are changed (General). */
  onChangeLanguages?: () => void;
}

const LANGUAGE_PRESETS: ReadonlyArray<{
  id: string;
  label: string;
  badge?: string;
  langs: string[];
}> = [
  {
    id: "en",
    label: "English",
    badge: "Max Precision",
    langs: ["en"],
  },
  {
    id: "en-hi",
    label: "English + Hindi",
    badge: "Hinglish",
    langs: ["en", "hi"],
  },
  {
    id: "en-ur",
    label: "English + Urdu",
    langs: ["en", "ur"],
  },
  {
    id: "en-ar",
    label: "English + Arabic",
    badge: "العربية",
    langs: ["en", "ar"],
  },
  {
    id: "en-ur-ar",
    label: "English + Urdu + Arabic",
    badge: "Lesson / Deen",
    langs: ["en", "ur", "ar"],
  },
];

function sameLanguageSet(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  const setA = new Set(a.map((x) => x.split(/[-_]/)[0].toLowerCase()));
  const setB = new Set(b.map((x) => x.split(/[-_]/)[0].toLowerCase()));
  if (setA.size !== setB.size) return false;
  for (const item of setA) {
    if (!setB.has(item)) return false;
  }
  return true;
}

const RecommendedModuleAction: React.FC<{
  runtimeId: string;
  sizeMb: number;
  routingAligned: boolean;
  onActivate: () => void;
}> = ({ runtimeId, sizeMb, routingAligned, onActivate }) => {
  const place = usePlace(runtimeId);
  const progress = useModelStore(
    (s) => s.downloadProgress[runtimeId]?.percentage ?? 0,
  );

  if (place === "downloading" || place === "checking") {
    const pct = Math.max(1, Math.min(99, Math.round(progress)));
    return (
      <button
        type="button"
        data-testid="voice-module-primary-action"
        onClick={() => cancelDownload(runtimeId)}
        className="inline-flex h-[32px] shrink-0 cursor-pointer items-center gap-[8px] rounded-[8px] border border-line-strong bg-wash px-[12px] text-xs font-medium text-text hover:border-ink-3"
      >
        <span>
          {place === "checking" ? "Verifying…" : `Downloading ${pct}%`}
        </span>
        <span className="text-ink-3">Cancel</span>
      </button>
    );
  }

  if (place === "loading") {
    return (
      <span
        data-testid="voice-module-primary-action"
        className="inline-flex h-[32px] shrink-0 items-center rounded-[8px] bg-wash px-[12px] text-xs font-medium text-ink-2"
      >
        Activating…
      </span>
    );
  }

  if (place === "active" && routingAligned) {
    return (
      <span
        data-testid="voice-module-primary-action"
        className="inline-flex h-[30px] shrink-0 items-center gap-[5px] rounded-full border border-line-strong bg-wash px-[11px] text-xs font-medium text-text"
      >
        <Check className="size-[13px]" aria-hidden="true" />
        Active on this Mac
      </span>
    );
  }

  if (place === "active" && !routingAligned) {
    return (
      <button
        type="button"
        data-testid="voice-module-primary-action"
        onClick={onActivate}
        className="inline-flex h-[32px] shrink-0 cursor-pointer items-center gap-[6px] rounded-[8px] bg-text px-[13px] text-xs font-medium text-background hover:opacity-90 focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
      >
        Optimize Audio Routing
      </button>
    );
  }

  if (place === "ready") {
    return (
      <button
        type="button"
        data-testid="voice-module-primary-action"
        onClick={onActivate}
        className="inline-flex h-[32px] shrink-0 cursor-pointer items-center gap-[6px] rounded-[8px] bg-text px-[13px] text-xs font-medium text-background hover:opacity-90 focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
      >
        Use Recommended Module
      </button>
    );
  }

  return (
    <button
      type="button"
      data-testid="voice-module-primary-action"
      onClick={onActivate}
      className="inline-flex h-[32px] shrink-0 cursor-pointer items-center gap-[6px] rounded-[8px] bg-text px-[13px] text-xs font-medium text-background hover:opacity-90 focus:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary"
    >
      <Download className="size-[13px]" aria-hidden="true" />
      <span>Download Module ({sizeMb} MB)</span>
    </button>
  );
};

/** Consumer-first Language & Automatic Voice Module card powered by Jev router. */
const VoiceModuleSetupCard: React.FC<{
  onChangeLanguages?: () => void;
}> = ({ onChangeLanguages }) => {
  const { getSetting, updateSetting } = useSettings();
  const spokenRaw = getSetting("spoken_languages");
  const selectedLanguage = getSetting("selected_language") ?? "auto";
  const detectionMode = getSetting("language_detection") ?? "when_needed";
  const langs = spokenRaw?.length ? spokenRaw : suggestedLanguages();

  const { tier, offered, infoFor } = useModelCatalog(langs);
  const infos = useModelStore((s) => s.models);
  const detector = useLanguageDetector();

  const downloadedCatalogIds = useMemo(() => {
    const set = new Set<string>();
    for (const model of offered) {
      if (infoFor(model)?.is_downloaded) {
        set.add(model.id);
      }
    }
    return set;
  }, [offered, infos]);

  const decision = useMemo(
    () =>
      routeAsrWithJev({
        spokenLanguages: langs,
        offered,
        tier,
        downloadedCatalogIds,
      }),
    [langs, offered, tier, downloadedCatalogIds],
  );

  const primaryCatalog = decision.primary;
  const primaryInfo = primaryCatalog ? infoFor(primaryCatalog) : undefined;
  const primaryCandidate = decision.ranked[0];

  const applyAudioPolicy = async (nextLangs: string[]) => {
    const nextDecision = routeAsrWithJev({
      spokenLanguages: nextLangs,
      offered,
      tier,
      downloadedCatalogIds,
    });
    await updateSetting("spoken_languages", nextLangs);
    await updateSetting(
      "selected_language",
      nextDecision.audioPolicy.recommendedSelectedLanguage,
    );
    await updateSetting(
      "language_detection",
      nextDecision.audioPolicy.recommendedDetectionMode,
    );
    if (
      nextDecision.audioPolicy.requiresConstrainedDetector &&
      detector.downloaded === false &&
      !detector.downloading
    ) {
      void detector.download();
    }
    const nextRuntime = nextDecision.primary
      ? infoFor(nextDecision.primary)
      : undefined;
    if (nextRuntime?.is_downloaded) {
      pickModel(nextRuntime.id);
    }
  };

  const handleActivatePrimary = () => {
    void updateSetting(
      "selected_language",
      decision.audioPolicy.recommendedSelectedLanguage,
    );
    void updateSetting(
      "language_detection",
      decision.audioPolicy.recommendedDetectionMode,
    );
    if (
      decision.audioPolicy.requiresConstrainedDetector &&
      detector.downloaded === false &&
      !detector.downloading
    ) {
      void detector.download();
    }
    if (primaryInfo) {
      pickModel(primaryInfo.id);
    }
  };

  const detectorReady =
    !decision.audioPolicy.requiresConstrainedDetector ||
    detector.downloaded === true ||
    detector.downloading;
  const routingAligned =
    selectedLanguage === decision.audioPolicy.recommendedSelectedLanguage &&
    (!decision.audioPolicy.requiresConstrainedDetector ||
      detectionMode === "always") &&
    detectorReady;

  const matchesPreset = LANGUAGE_PRESETS.some((p) =>
    sameLanguageSet(langs, p.langs),
  );

  return (
    <section
      data-testid="voice-module-setup-card"
      className="mx-[12px] mb-[16px] rounded-card border border-line-strong bg-surface p-[16px]"
    >
      <div className="flex flex-wrap items-center justify-between gap-[8px]">
        <div>
          <div className="text-xs font-medium tracking-wide text-ink-3 uppercase">
            Languages you speak
          </div>
          <p className="mt-[2px] text-xs text-ink-2">
            Pick your speaking profile and lowyap automatically prepares the
            right offline voice module.
          </p>
        </div>
        {onChangeLanguages && (
          <button
            type="button"
            onClick={onChangeLanguages}
            className="shrink-0 cursor-pointer text-xs font-medium text-ink-2 underline decoration-line-strong underline-offset-[3px] hover:text-text"
          >
            More languages…
          </button>
        )}
      </div>

      <div className="mt-[12px] flex flex-wrap items-center gap-[8px]">
        {LANGUAGE_PRESETS.map((preset) => {
          const active = sameLanguageSet(langs, preset.langs);
          return (
            <button
              key={preset.id}
              type="button"
              data-testid={`voice-profile-${preset.id}`}
              onClick={() => void applyAudioPolicy(preset.langs)}
              className={`inline-flex h-[30px] cursor-pointer items-center gap-[6px] rounded-full border px-[12px] text-xs font-medium transition-colors ${
                active
                  ? "border-text bg-text text-background"
                  : "border-line-strong bg-background text-text hover:border-ink-3"
              }`}
            >
              <span>{preset.label}</span>
              {preset.badge && (
                <span
                  className={`rounded-full px-[6px] py-[1px] text-[10px] ${
                    active
                      ? "bg-background/20 text-background"
                      : "bg-wash text-ink-2"
                  }`}
                >
                  {preset.badge}
                </span>
              )}
            </button>
          );
        })}
        {!matchesPreset && (
          <span className="inline-flex h-[30px] items-center rounded-full border border-text bg-text px-[12px] text-xs font-medium text-background">
            {decision.normalizedLanguages.map((c) => c.toUpperCase()).join(" + ")}
          </span>
        )}
      </div>

      {primaryCatalog && primaryInfo && (
        <div className="mt-[14px] flex flex-col justify-between gap-[12px] rounded-[10px] border border-line bg-background p-[13px] sm:flex-row sm:items-center">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-[8px]">
              <span className="text-sm font-semibold text-text">
                {decision.moduleTitle}
              </span>
              <span className="rounded-[5px] bg-wash px-[6px] py-[1px] text-[11px] font-medium text-ink-2">
                {primaryCatalog.name}
              </span>
            </div>
            <p className="mt-[3px] text-xs text-ink-2">
              {decision.moduleSubtitle}
            </p>
          </div>
          <RecommendedModuleAction
            runtimeId={primaryInfo.id}
            sizeMb={primaryCandidate?.sizeMb ?? Math.round(primaryInfo.size_mb)}
            routingAligned={routingAligned}
            onActivate={handleActivatePrimary}
          />
        </div>
      )}
    </section>
  );
};

/** The Models page: simple Voice Module setup up top, custom open-weight catalog on demand. */
export const ModelsSettings: React.FC<ModelsSettingsProps> = ({
  onChangeLanguages,
}) => {
  const { t } = useTranslation();
  const loading = useModelStore((s) => s.loading);
  const { facts, lists } = useModelEntries();
  const { filters, setFilters, searching, matches } = useModelFilters();
  const [more, setMore] = useState(false);
  const [catalogExpanded, setCatalogExpanded] = useState(false);
  const showCatalog = catalogExpanded || searching;

  if (loading) {
    return (
      <div className="max-w-3xl w-full mx-auto">
        <div className="flex items-center justify-center py-16">
          <div className="w-8 h-8 border-2 border-logo-primary border-t-transparent rounded-full animate-spin" />
        </div>
      </div>
    );
  }

  const keep = (xs: typeof lists.yours) => xs.filter(matches);
  const moreButton = !searching && lists.yours.length > YOURS_LIMIT && (
    <button
      type="button"
      onClick={() => setMore((m) => !m)}
      className="ms-[40px] mt-[2px] flex h-[28px] cursor-pointer items-center gap-[8px] rounded-[7px] px-[2px] text-small font-medium text-ink-2 hover:text-text focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
    >
      {more
        ? t("settings.models.showFewer")
        : t("settings.models.showMore", {
            count: lists.yours.length - YOURS_LIMIT,
          })}
      <span
        aria-hidden="true"
        className={`inline-block size-[6px] shrink-0 border-e-[1.5px] border-b-[1.5px] border-current ${more ? "mt-[3px] -rotate-[135deg]" : "-mt-[3px] rotate-45"}`}
      />
    </button>
  );

  const sections: TableSection[] = searching
    ? [
        {
          key: "yours",
          label: t("settings.models.sections.yours"),
          rows: keep(lists.yours),
        },
        {
          key: "fits",
          label: t("settings.models.sections.fits"),
          rows: keep(lists.fits),
        },
        {
          key: "partial",
          label: t("modelTable.missingLanguage"),
          rows: keep(lists.partial),
        },
        {
          key: "untested",
          label: t("settings.models.sections.untested"),
          rows: filters.query.trim() ? keep(lists.untested) : [],
          note: t("settings.models.untestedNote"),
        },
      ]
    : [
        {
          key: "yours",
          label: (
            <>
              {t("settings.models.sections.yours")}
              <Rescan />
            </>
          ),
          rows: more ? lists.yours : lists.yours.slice(0, YOURS_LIMIT),
          after: lists.yours.length ? (
            moreButton
          ) : (
            <Note>{t("settings.models.emptyYours")}</Note>
          ),
        },
        {
          key: "fits",
          label: t("settings.models.sections.fits"),
          rows: lists.fits,
        },
        {
          key: "partial",
          label: t("modelTable.missingLanguage"),
          rows: lists.partial,
        },
      ];

  return (
    <div className="mx-auto flex min-h-0 w-full max-w-3xl flex-1 flex-col">
      {/* The title, and across from it where to change the languages the
          grades are for. */}
      <div className="mb-[16px] flex flex-none items-center justify-between gap-[12px] px-[12px]">
        <h1 className="text-xl font-semibold">{t("settings.models.title")}</h1>
        {onChangeLanguages && (
          <button
            type="button"
            onClick={onChangeLanguages}
            className="shrink-0 cursor-pointer text-sm text-text underline decoration-line-strong underline-offset-[3px] hover:decoration-text focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
          >
            {t("settings.models.changeLanguages")}
          </button>
        )}
      </div>
      <div className="mx-[12px] mb-[16px] flex-none empty:hidden">
        <RetiredModelsNotice />
      </div>
      <VoiceModuleSetupCard onChangeLanguages={onChangeLanguages} />

      <div className="mx-[12px] mb-[10px] flex-none">
        <button
          type="button"
          data-testid="toggle-custom-catalog"
          aria-expanded={showCatalog}
          onClick={() => setCatalogExpanded((open) => !open)}
          className="flex w-full cursor-pointer items-center justify-between rounded-[10px] border border-line bg-wash/50 px-[13px] py-[9px] text-left transition-colors hover:border-line-strong hover:bg-wash focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
        >
          <div className="flex flex-wrap items-center gap-[8px]">
            <span className="text-xs font-medium text-text">
              Custom Engine &amp; Open-Weight Catalog
            </span>
            <span className="text-xs text-ink-3">
              {lists.yours.length} installed · Bring your own GGUF or compare
              all engines
            </span>
          </div>
          <ChevronDown
            className={`size-[15px] shrink-0 text-ink-2 transition-transform duration-200 ${
              showCatalog ? "rotate-180" : ""
            }`}
            aria-hidden="true"
          />
        </button>
      </div>

      {showCatalog && (
        <ModelTable
          grading={facts}
          filters={filters}
          setFilters={setFilters}
          // "Yours" always shows at rest; a search hides empty sections.
          sections={sections.filter(
            (s) => s.rows.length || (s.key === "yours" && !searching),
          )}
          renderRow={(e) => <ModelRow key={e.id} entry={e} />}
          actionsHead={<Legend tier={facts.tier} />}
          footer={
            !searching &&
            lists.untested.length > 0 && (
              <Note>
                {t("settings.models.untestedHidden", {
                  count: lists.untested.length,
                })}
              </Note>
            )
          }
          thinEdge
        />
      )}
    </div>
  );
};
