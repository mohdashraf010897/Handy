import { isWeakLanguage } from "./catalog";

export interface ActiveModelCapabilityInput {
  id: string;
  name?: string;
  supported_languages?: readonly string[];
}

export interface PartitionSpokenLanguageSupportParams {
  languages: readonly string[];
  activeModel?: ActiveModelCapabilityInput | null;
  isWeak?: (language: string) => boolean;
}

export interface SpokenLanguageSupportPartition {
  adapterSupported: string[];
  weak: string[];
}

const HINDUSTANI_CODES = new Set(["ur", "hi"]);

const baseLanguageCode = (code: string): string =>
  code.trim().toLowerCase().split(/[-_]/)[0] ?? "";

export const isHindustaniLanguage = (code: string): boolean =>
  HINDUSTANI_CODES.has(baseLanguageCode(code));

export const modelSupportsHindustani = (
  activeModel?: ActiveModelCapabilityInput | null,
): boolean => {
  if (!activeModel?.id) return false;

  const supported = activeModel.supported_languages ?? [];
  if (supported.length > 0) {
    return supported.some((code) => isHindustaniLanguage(code));
  }

  // Pre-hydration fallback only when model metadata has not loaded yet.
  const idLower = activeModel.id.trim().toLowerCase();
  if (/\.en(?=$|[-_/])/.test(idLower)) {
    return false;
  }

  return (
    idLower.startsWith("whisper") ||
    idLower.includes("/whisper") ||
    idLower.includes("sense-voice")
  );
};

export const partitionSpokenLanguageSupport = ({
  languages,
  activeModel,
  isWeak = isWeakLanguage,
}: PartitionSpokenLanguageSupportParams): SpokenLanguageSupportPartition => {
  const supportsHindustani = modelSupportsHindustani(activeModel);
  const adapterSupported: string[] = [];
  const weak: string[] = [];

  for (const lang of languages) {
    if (supportsHindustani && isHindustaniLanguage(lang)) {
      adapterSupported.push(lang);
      continue;
    }
    if (isWeak(lang)) {
      weak.push(lang);
    }
  }

  return { adapterSupported, weak };
};
