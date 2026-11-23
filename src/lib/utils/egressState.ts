import type { AppSettings, ModelInfo, PostProcessProvider } from "@/bindings";

export type StudioEgressMode =
  | "zero_egress"
  | "cloud_asr"
  | "cloud_llm"
  | "cloud_both";

export interface StudioEgressState {
  mode: StudioEgressMode;
  hasCloudEgress: boolean;
  cloudAsr: boolean;
  cloudLinguist: boolean;
}

const ON_DEVICE_PROVIDER_IDS = new Set([
  "apple_intelligence",
  "local_sidecar",
]);

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]", "::1"]);

export function isLoopbackBaseUrl(rawUrl: string | undefined | null): boolean {
  const trimmed = rawUrl?.trim();
  if (!trimmed) return false;
  if (trimmed.startsWith("apple-intelligence://")) return true;

  try {
    const parsed = new URL(trimmed);
    return LOOPBACK_HOSTS.has(parsed.hostname.toLowerCase());
  } catch {
    return false;
  }
}

export function isRemotePostProcessActive(
  settings?: Pick<
    AppSettings,
    | "post_process_enabled"
    | "post_process_provider_id"
    | "post_process_providers"
  > | null,
): boolean {
  if (!settings?.post_process_enabled) return false;

  const providerId = settings.post_process_provider_id?.trim() ?? "";
  if (!providerId) return false;
  if (ON_DEVICE_PROVIDER_IDS.has(providerId)) return false;

  const providers: readonly PostProcessProvider[] =
    settings.post_process_providers ?? [];
  const matched = providers.find((p) => p.id === providerId);
  if (!matched) {
    // Unknown provider ID while post-processing is enabled: fail closed so we
    // never falsely advertise "0 B sent".
    return true;
  }

  return !isLoopbackBaseUrl(matched.base_url);
}

export type EgressModelInput = Partial<ModelInfo> & {
  is_cloud?: boolean;
};

export function resolveStudioEgressState(
  modelInfo?: EgressModelInput | null,
  settings?: Pick<
    AppSettings,
    | "post_process_enabled"
    | "post_process_provider_id"
    | "post_process_providers"
  > | null,
): StudioEgressState {
  const cloudAsr = Boolean(modelInfo?.is_cloud);
  const cloudLinguist = isRemotePostProcessActive(settings);

  const mode: StudioEgressMode =
    cloudAsr && cloudLinguist
      ? "cloud_both"
      : cloudAsr
        ? "cloud_asr"
        : cloudLinguist
          ? "cloud_llm"
          : "zero_egress";

  return {
    mode,
    hasCloudEgress: mode !== "zero_egress",
    cloudAsr,
    cloudLinguist,
  };
}
