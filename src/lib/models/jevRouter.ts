/**
 * Jev-style 2-stage capability + benchmark scorecard router for ASR & multilingual audio.
 *
 * Inspired by Jev (`eligible()` hard capability filter -> linear benchmark scorecard +
 * warm-engine gate), adapted for zero-latency local speech models:
 *  - Zero hardcoded model IDs: every pick emerges from `@handy-computer/models`
 *    capabilities, per-language WER benchmarks, Apple Silicon / GPU wait time,
 *    download footprint, and on-disk warm state.
 *  - Multilingual audio policy: locks `selected_language` when monolingual (eliminating
 *    short-utterance `<|lang|>` misdetection) and enables `VoxLingua107` constrained
 *    acoustic routing (`language_detection: "always"`) when 2+ languages are spoken.
 */
import {
  catalog,
  languageIdentity,
  modelMatchesLanguage,
  type CatalogModel,
  type MachineTier,
} from "@handy-computer/models";

const METHODOLOGY = catalog.methodology;

/** Default fresh-download ceiling (MB) for consumer automatic picks unless no smaller full-coverage model exists. */
const DEFAULT_MAX_FRESH_DOWNLOAD_MB = 1800;

export interface JevRouteRequest {
  /** User's spoken languages (ISO codes, e.g. `["en"]`, `["en", "hi"]`, `["en", "ur"]`). */
  spokenLanguages: string[];
  /** Offered ASR catalog models available to the runtime. */
  offered: CatalogModel[];
  /** Hardware tier (`"accelerated"` on Apple Silicon / GPU). Defaults to `"accelerated"`. */
  tier?: MachineTier;
  /** Catalog model IDs (`model.id`) already downloaded on disk for the Warm-Engine Gate. */
  downloadedCatalogIds?: ReadonlySet<string>;
  /** Maximum preferred size (MB) for a cold download when smaller full-coverage modules exist. */
  maxFreshDownloadMb?: number;
}

export interface JevScoredCandidate {
  model: CatalogModel;
  /** Lower is better (WER-equivalent utility cost). */
  cost: number;
  /** Geometric mean WER (%) across measured spoken languages, if available. */
  werGeoMean: number | null;
  /** Worst single-language WER (%) across measured spoken languages, if available. */
  worstLanguageWer: number | null;
  /** Estimated post-utterance wait (seconds) on this machine tier. */
  waitSeconds: number;
  /** Default quantization download footprint in MB. */
  sizeMb: number;
  /** Number of requested spoken languages supported by this model. */
  coveredLanguageCount: number;
  /** Whether this model is already on disk (`0 MB` cold wait). */
  isWarmOnDisk: boolean;
}

export interface JevAudioRoutingPolicy {
  /** Exact `selected_language` setting (`"en"` for 1-language English, `"auto"` for 2+ languages). */
  recommendedSelectedLanguage: string;
  /** Recommended `language_detection` mode (`"always"` when 2+ languages constrain Auto to spoken set). */
  recommendedDetectionMode: "when_needed" | "always";
  /** True when 2+ spoken languages benefit from downloading `VoxLingua107` to constrain Auto strictly to `spoken_languages`. */
  requiresConstrainedDetector: boolean;
}

export interface JevRouteDecision {
  /** Normalized canonical spoken language codes (non-empty, deduplicated). */
  normalizedLanguages: string[];
  /** True when only English (`en` / `en-*`) is active. */
  isEnglishOnly: boolean;
  /** Top recommended model from Jev Stage 1 + Stage 2 + Warm-Engine Gate. */
  primary: CatalogModel | undefined;
  /** Cold benchmark #1 ignoring on-disk warm bonus (identical to `primary` unless Warm-Engine Gate kept a warm peer). */
  coldBest: CatalogModel | undefined;
  /** Faster lower-footprint alternative covering the same languages, if distinct from `primary`. */
  fasterAlternative: CatalogModel | undefined;
  /** Higher-accuracy heavyweight alternative covering the same languages, if distinct from `primary`. */
  accurateAlternative: CatalogModel | undefined;
  /** Ranked candidates after Stage 1 + Stage 2 scoring. */
  ranked: JevScoredCandidate[];
  /** Consumer-friendly module title (abstracts away raw GGUF/architecture jargon). */
  moduleTitle: string;
  /** Plain-English 1-line explanation of what this module optimizes for. */
  moduleSubtitle: string;
  /** Coordinated `selected_language` + `language_detection` audio routing policy. */
  audioPolicy: JevAudioRoutingPolicy;
}

function normalizeSpokenLanguages(langs: string[]): string[] {
  const out: string[] = [];
  for (const raw of langs) {
    const trimmed = raw.trim();
    if (!trimmed || trimmed.toLowerCase() === "auto") continue;
    const canonical = languageIdentity(trimmed);
    if (canonical && !out.includes(canonical)) {
      out.push(canonical);
    }
  }
  return out.length > 0 ? out : ["en"];
}

function defaultSizeMb(model: CatalogModel): number {
  const quant = (model as { quant?: string }).quant;
  const files = (model as { files?: Record<string, { size?: number }> }).files;
  const rawBytes = quant && files?.[quant]?.size;
  if (typeof rawBytes === "number" && rawBytes > 0) {
    return Math.round(rawBytes / (1024 * 1024));
  }
  const stubMb = (model as unknown as { size_mb?: number }).size_mb;
  return typeof stubMb === "number" && stubMb > 0 ? stubMb : 700;
}

function measuredLanguageError(
  modelId: string,
  lang: string,
): number | undefined {
  return catalog.data.results[modelId]?.accuracy[languageIdentity(lang)]?.error;
}

function geoMean(values: number[]): number {
  return Math.exp(
    values.reduce((acc, v) => acc + Math.log(Math.max(v, 0.05)), 0) /
      values.length,
  );
}

function humanLanguageLabel(code: string): string {
  const map: Record<string, string> = {
    en: "English",
    hi: "Hindi / Hinglish",
    ur: "Urdu",
    es: "Spanish",
    fr: "French",
    de: "German",
    ar: "Arabic",
    zh: "Chinese",
    "zh-Hans": "Chinese",
    ja: "Japanese",
    ko: "Korean",
    pt: "Portuguese",
    ru: "Russian",
  };
  return map[code] ?? code.toUpperCase();
}

function buildModuleCopy(
  langs: string[],
  primary: CatalogModel | undefined,
): { moduleTitle: string; moduleSubtitle: string } {
  if (langs.length === 1 && langs[0] === "en") {
    const streaming = Boolean(primary?.capabilities?.streaming);
    return {
      moduleTitle: "English High-Precision Module",
      moduleSubtitle: streaming
        ? "Dedicated English speech module with live word-by-word streaming and zero multilingual cross-talk."
        : "Dedicated English speech module tuned for fast, accurate offline dictation on Apple Silicon.",
    };
  }
  if (langs.length === 1) {
    const name = humanLanguageLabel(langs[0]);
    return {
      moduleTitle: `${name} Voice Module`,
      moduleSubtitle: `Locked to ${name} for crisp accuracy without cross-language guessing.`,
    };
  }
  const labels = langs.slice(0, 3).map(humanLanguageLabel).join(" + ");
  const suffix = langs.length > 3 ? ` +${langs.length - 3}` : "";
  return {
    moduleTitle: `Multilingual Voice Module (${labels}${suffix})`,
    moduleSubtitle:
      "Routes spoken audio strictly across your selected languages — no manual switching mid-sentence.",
  };
}

/**
 * Runs the 2-stage Jev capability + benchmark scorecard router across `offered`.
 */
export function routeAsrWithJev(request: JevRouteRequest): JevRouteDecision {
  const normalizedLanguages = normalizeSpokenLanguages(request.spokenLanguages);
  const tier: MachineTier = request.tier ?? "accelerated";
  const maxFreshMb = request.maxFreshDownloadMb ?? DEFAULT_MAX_FRESH_DOWNLOAD_MB;
  const downloaded = request.downloadedCatalogIds;

  const isEnglishOnly =
    normalizedLanguages.length === 1 && normalizedLanguages[0] === "en";
  const isSingleLanguage = normalizedLanguages.length === 1;

  // Stage 1: Hard capability & license filter (`eligible`).
  const nonSpecialistPool = request.offered.filter((m) => {
    const license = (m as { license?: string }).license ?? "";
    return !license.startsWith("cc-by-nc") && m.id !== "medasr";
  });
  const basePool =
    nonSpecialistPool.length > 0 ? nonSpecialistPool : request.offered;

  const withCoverage = basePool.map((model) => {
    const coveredLanguageCount = normalizedLanguages.filter((lang) =>
      modelMatchesLanguage(model, lang),
    ).length;
    return {
      model,
      coveredLanguageCount,
      sizeMb: defaultSizeMb(model),
      isWarmOnDisk: Boolean(downloaded?.has(model.id)),
    };
  });

  const fullCoverage = withCoverage.filter(
    (c) => c.coveredLanguageCount === normalizedLanguages.length,
  );

  let stage1Pool = fullCoverage;
  if (stage1Pool.length === 0) {
    const maxCovered = Math.max(
      0,
      ...withCoverage.map((c) => c.coveredLanguageCount),
    );
    stage1Pool =
      maxCovered > 0
        ? withCoverage.filter((c) => c.coveredLanguageCount === maxCovered)
        : withCoverage;
  }

  // Respect consumer fresh-download footprint ceiling when at least one full-coverage module fits.
  const compactEligible = stage1Pool.filter(
    (c) => c.isWarmOnDisk || c.sizeMb <= maxFreshMb,
  );
  const finalEligible =
    compactEligible.length > 0 ? compactEligible : stage1Pool;

  // Stage 2: Benchmark linear scorecard (`accuracy + wait + footprint + capability`).
  const coldScored: JevScoredCandidate[] = finalEligible.map((entry) => {
    const { model, coveredLanguageCount, sizeMb, isWarmOnDisk } = entry;
    const measured = normalizedLanguages
      .map((lang) => measuredLanguageError(model.id, lang))
      .filter((v): v is number => typeof v === "number");

    let werGeoMean: number | null = null;
    let worstLanguageWer: number | null = null;
    let errorCost: number;

    if (measured.length === normalizedLanguages.length) {
      werGeoMean = geoMean(measured);
      worstLanguageWer = Math.max(...measured);
      // Penalize weak secondary-language tails in bilingual pairs (e.g. Hindi/Urdu).
      const tailPenalty = 0.22 * Math.max(0, worstLanguageWer - 12);
      errorCost = werGeoMean + tailPenalty;
    } else if (measured.length > 0) {
      werGeoMean = geoMean(measured);
      worstLanguageWer = Math.max(...measured);
      const missingCount = normalizedLanguages.length - measured.length;
      errorCost = werGeoMean + missingCount * 3.5;
    } else {
      // Unmeasured/stub fallback: prefer concise specialist models for 1 language, broad coverage for 2+.
      errorCost =
        14.0 + (isSingleLanguage ? Math.min(model.languages.length, 20) * 0.15 : 0);
    }

    const waitSeconds = catalog.modelWaitSeconds(model, tier) ?? 0.6;
    const waitCost =
      METHODOLOGY.waitWeight *
      Math.max(0, waitSeconds - METHODOLOGY.freeWaitSeconds);
    const sizePenalty = isWarmOnDisk
      ? 0
      : 0.35 * Math.max(0, (sizeMb - 950) / 500);

    const monolingualBonus =
      isSingleLanguage && model.languages.length === 1 ? -0.45 : 0;
    const streamingBonus = model.capabilities?.streaming ? -0.35 : 0;
    const nativeLangIdBonus =
      !isSingleLanguage && model.capabilities?.languageDetection ? -0.35 : 0;

    const cost =
      errorCost +
      waitCost +
      sizePenalty +
      monolingualBonus +
      streamingBonus +
      nativeLangIdBonus;

    return {
      model,
      cost,
      werGeoMean,
      worstLanguageWer,
      waitSeconds,
      sizeMb,
      coveredLanguageCount,
      isWarmOnDisk,
    };
  });

  coldScored.sort((a, b) => a.cost - b.cost);
  const coldBestCandidate = coldScored[0];

  // Warm-Engine Gate: keep an already-downloaded full-coverage model if its accuracy is
  // competitive (within 1.5% geoMean WER of cold #1), crediting the avoided cold download
  // footprint while still upgrading when a specialist is dramatically faster (e.g. English Parakeet).
  const ranked = coldScored
    .map((cand) => {
      if (!cand.isWarmOnDisk || !coldBestCandidate) return cand;
      const coldWer = coldBestCandidate.werGeoMean;
      const candWer = cand.werGeoMean;
      const accuracyCompetitive =
        coldWer == null || candWer == null || candWer - coldWer <= 1.5;
      const avoidedDownloadCredit =
        1.6 * Math.min(2.0, coldBestCandidate.sizeMb / 700);
      return accuracyCompetitive
        ? { ...cand, cost: cand.cost - avoidedDownloadCredit }
        : cand;
    })
    .sort((a, b) => a.cost - b.cost);

  const primary = ranked[0]?.model;
  const coldBest = coldBestCandidate?.model;

  const fasterAlternative = ranked.find(
    (c) =>
      c.model.id !== primary?.id &&
      c.waitSeconds + 0.08 < (ranked[0]?.waitSeconds ?? 0) &&
      c.sizeMb <= (ranked[0]?.sizeMb ?? Infinity),
  )?.model;

  const accurateAlternative = fullCoverage
    .map((entry) => {
      const measured = normalizedLanguages
        .map((lang) => measuredLanguageError(entry.model.id, lang))
        .filter((v): v is number => typeof v === "number");
      return {
        model: entry.model,
        wer:
          measured.length === normalizedLanguages.length
            ? geoMean(measured)
            : null,
      };
    })
    .filter(
      (c): c is { model: CatalogModel; wer: number } =>
        c.wer !== null && c.model.id !== primary?.id,
    )
    .sort((a, b) => a.wer - b.wer)
    .find(
      (c) =>
        ranked[0]?.werGeoMean != null && c.wer + 0.25 < ranked[0].werGeoMean,
    )?.model;

  const { moduleTitle, moduleSubtitle } = buildModuleCopy(
    normalizedLanguages,
    primary,
  );

  const audioPolicy: JevAudioRoutingPolicy = isSingleLanguage
    ? {
        recommendedSelectedLanguage: normalizedLanguages[0],
        recommendedDetectionMode: "when_needed",
        requiresConstrainedDetector: false,
      }
    : {
        recommendedSelectedLanguage: "auto",
        recommendedDetectionMode: "always",
        requiresConstrainedDetector: true,
      };

  return {
    normalizedLanguages,
    isEnglishOnly,
    primary,
    coldBest,
    fasterAlternative,
    accurateAlternative,
    ranked,
    moduleTitle,
    moduleSubtitle,
    audioPolicy,
  };
}
