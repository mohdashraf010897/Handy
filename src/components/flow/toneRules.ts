import type { AppToneRule, Tone } from "@/bindings";

/** Display order of the tone presets. */
export const TONES: readonly Tone[] = ["default", "casual", "formal", "code"];

/** Mirrors `normalize` in `src-tauri/src/flow/context.rs`. */
export const normalizeAppId = (id: string): string => {
  const lower = id.trim().toLowerCase();
  return lower.endsWith(".exe") ? lower.slice(0, -4) : lower;
};

/** Mirrors `app_matches`: a trailing `*` matches any suffix. */
export const appMatches = (pattern: string, appId: string): boolean => {
  const p = normalizeAppId(pattern);
  const id = normalizeAppId(appId);
  if (p.endsWith("*")) {
    const prefix = p.slice(0, -1);
    return prefix.length > 0 && id.startsWith(prefix);
  }
  return p === id;
};

export type AppIdError = "empty" | "wildcardOnly" | "duplicate";

/** Same rules as `FlowSettings::validated`, plus duplicate detection. */
export const validateAppId = (
  id: string,
  rules: readonly AppToneRule[],
  ignoreIndex?: number,
): AppIdError | null => {
  const trimmed = id.trim();
  if (!trimmed) return "empty";
  if (trimmed === "*") return "wildcardOnly";
  const key = normalizeAppId(trimmed);
  const duplicate = rules.some(
    (rule, index) =>
      index !== ignoreIndex && normalizeAppId(rule.app_id) === key,
  );
  return duplicate ? "duplicate" : null;
};

/** Index of the user rule written for exactly this app id, or -1. */
export const findRuleIndex = (
  rules: readonly AppToneRule[],
  appId: string,
): number => {
  const key = normalizeAppId(appId);
  return rules.findIndex((rule) => normalizeAppId(rule.app_id) === key);
};

/** The user rule that takes precedence over a built-in preset, if any. */
export const overridingRule = (
  builtin: AppToneRule,
  rules: readonly AppToneRule[],
): AppToneRule | undefined => {
  const key = normalizeAppId(builtin.app_id);
  const isWildcard = key.endsWith("*");
  return rules.find(
    (rule) =>
      normalizeAppId(rule.app_id) === key ||
      (!isWildcard && appMatches(rule.app_id, builtin.app_id)),
  );
};
