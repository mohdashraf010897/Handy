import { modelMatchesLanguage } from "@handy-computer/models";
import type { CatalogModel } from "../../lib/models/catalog.ts";
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
}): {
  englishGoalModel: CatalogModel | undefined;
  multilingualGoalModel: CatalogModel | undefined;
  isEnglishOnly: boolean;
  autoHdModel: CatalogModel | undefined;
} {
  const { offered, pickModel, langs } = params;
  const byId = new Map(offered.map((m) => [m.id, m]));
  const englishGoalModel =
    byId.get("parakeet-tdt-0.6b-v3") ??
    byId.get("parakeet-unified-en-0.6b") ??
    byId.get("parakeet-tdt-0.6b-v2") ??
    offered.find((m) => modelMatchesLanguage(m, "en")) ??
    pickModel;
  const isMultilingualCandidate = (m: CatalogModel) =>
    m.id !== englishGoalModel?.id &&
    (modelMatchesLanguage(m, "ur") ||
      m.languages.includes("ur") ||
      m.languages.length >= 10);
  const multilingualGoalModel = [
    byId.get("whisper-large-v3-turbo"),
    byId.get("whisper-medium"),
    byId.get("whisper-large-v3"),
    offered.find(isMultilingualCandidate),
  ].find((m): m is CatalogModel => !!m && isMultilingualCandidate(m));

  const isEnglishOnly =
    langs.length > 0 && langs.every((l) => l === "en" || l.startsWith("en-"));
  const autoHdModel =
    (isEnglishOnly ? englishGoalModel : multilingualGoalModel) ??
    englishGoalModel ??
    multilingualGoalModel ??
    pickModel;

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
