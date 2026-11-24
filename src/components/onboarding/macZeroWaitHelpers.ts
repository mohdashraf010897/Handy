import { modelMatchesLanguage } from "@handy-computer/models";
import type { CatalogModel } from "../../lib/models/catalog.ts";
import { routeAsrWithJev } from "../../lib/models/jevRouter.ts";
import type { Permissions as PermissionsState } from "./usePermissions.ts";

export const MACOS_NATIVE_SPEECH_ID = "macos-native-speech";
export const STEP_2_START_SEC = 3.5;

export type WalkthroughStep = "microphone" | "accessibility";

export interface WalkthroughStepResolution {
  activePermissionStep: WalkthroughStep;
  micGranted: boolean;
  accGranted: boolean;
  seekTimeSec: number;
}

export function resolveWalkthroughActiveStep(
  microphone: PermissionsState["microphone"],
  accessibility: PermissionsState["accessibility"],
): WalkthroughStepResolution {
  const micGranted = microphone === "granted";
  const accGranted = accessibility === "granted";
  const activePermissionStep: WalkthroughStep = micGranted
    ? "accessibility"
    : "microphone";
  return {
    activePermissionStep,
    micGranted,
    accGranted,
    seekTimeSec: micGranted ? STEP_2_START_SEC : 0,
  };
}

export function resolveMacZeroWaitTarget(params: {
  offered: CatalogModel[];
  pickModel?: CatalogModel;
  langs: string[];
  downloadedCatalogIds?: ReadonlySet<string>;
}): {
  englishGoalModel: CatalogModel | undefined;
  multilingualGoalModel: CatalogModel | undefined;
  isEnglishOnly: boolean;
  autoHdModel: CatalogModel | undefined;
} {
  const { offered, pickModel, langs, downloadedCatalogIds } = params;
  const englishRoute = routeAsrWithJev({
    spokenLanguages: ["en"],
    offered,
    downloadedCatalogIds,
  });
  const englishGoalModel =
    englishRoute.primary ??
    offered.find((m) => modelMatchesLanguage(m, "en")) ??
    pickModel;

  const isEnglishOnly =
    langs.length > 0 && langs.every((l) => l === "en" || l.startsWith("en-"));

  const multiLangs = isEnglishOnly ? ["en", "ur"] : langs;
  const multilingualPool = offered.filter((m) => m.id !== englishGoalModel?.id);
  const multilingualRoute = routeAsrWithJev({
    spokenLanguages: multiLangs,
    offered: multilingualPool.length > 0 ? multilingualPool : offered,
    downloadedCatalogIds,
  });
  const multilingualGoalModel = multilingualRoute.primary;

  const activeRoute = isEnglishOnly
    ? englishGoalModel
    : (routeAsrWithJev({
        spokenLanguages: langs,
        offered,
        downloadedCatalogIds,
      }).primary ?? multilingualGoalModel);

  const autoHdModel =
    activeRoute ?? englishGoalModel ?? multilingualGoalModel ?? pickModel;

  return {
    englishGoalModel,
    multilingualGoalModel,
    isEnglishOnly,
    autoHdModel,
  };
}

export function resolveOnboardingModelReady(params: {
  osType: string;
  preview: boolean;
  modelId: string | null;
  isDownloaded: boolean;
  downloadState:
    | "none"
    | "downloading"
    | "checking"
    | "extracting"
    | "ready"
    | "failed";
}): boolean {
  const { osType, preview, modelId, isDownloaded, downloadState } = params;
  if (preview) return true;
  if (modelId === MACOS_NATIVE_SPEECH_ID) return true;
  if (osType === "macos") return true;
  if (isDownloaded) return true;
  return downloadState === "ready";
}
