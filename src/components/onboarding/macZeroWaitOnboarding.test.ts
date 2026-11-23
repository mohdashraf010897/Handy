import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { CatalogModel } from "../../lib/models/catalog.ts";
import {
  MACOS_NATIVE_SPEECH_ID,
  resolveMacZeroWaitTarget,
  resolveOnboardingModelReady,
  resolveWalkthroughActiveStep,
  STEP_2_START_SEC,
} from "./macZeroWaitHelpers.ts";

function stubCatalogModel(
  id: string,
  name: string,
  languages: string[],
): CatalogModel {
  return {
    id,
    name,
    languages,
    size_mb: 500,
    capabilities: {
      realtime_preview: false,
      translation: false,
    },
  } as unknown as CatalogModel;
}

describe("Lane C: macOS Permissions Walkthrough + Zero-Wait Model Step", () => {
  it("syncs OnboardingWalkthrough step pills and seek time with Microphone and Accessibility status", () => {
    const step1 = resolveWalkthroughActiveStep("needed", "needed");
    assert.deepEqual(step1, {
      activePermissionStep: "microphone",
      micGranted: false,
      accGranted: false,
      seekTimeSec: 0,
    });

    const step2 = resolveWalkthroughActiveStep("granted", "waiting");
    assert.deepEqual(step2, {
      activePermissionStep: "accessibility",
      micGranted: true,
      accGranted: false,
      seekTimeSec: STEP_2_START_SEC,
    });

    const done = resolveWalkthroughActiveStep("granted", "granted");
    assert.deepEqual(done, {
      activePermissionStep: "accessibility",
      micGranted: true,
      accGranted: true,
      seekTimeSec: STEP_2_START_SEC,
    });
  });

  it("matches English-only spoken languages to parakeet-tdt-0.6b-v3 and multilingual/Urdu to whisper-large-v3-turbo", () => {
    const parakeetV3 = stubCatalogModel("parakeet-tdt-0.6b-v3", "Parakeet V3", [
      "en",
    ]);
    const whisperTurbo = stubCatalogModel(
      "whisper-large-v3-turbo",
      "Whisper Large v3 Turbo",
      [
        "en",
        "ur",
        "hi",
        "es",
        "fr",
        "de",
        "ar",
        "ja",
        "zh-Hans",
        "pt",
        "ru",
        "ko",
      ],
    );
    const offered = [parakeetV3, whisperTurbo];

    const enPick = resolveMacZeroWaitTarget({
      offered,
      pickModel: parakeetV3,
      langs: ["en"],
    });
    assert.equal(enPick.isEnglishOnly, true);
    assert.equal(enPick.autoHdModel?.id, "parakeet-tdt-0.6b-v3");

    const urPick = resolveMacZeroWaitTarget({
      offered,
      pickModel: parakeetV3,
      langs: ["en", "ur"],
    });
    assert.equal(urPick.isEnglishOnly, false);
    assert.equal(urPick.autoHdModel?.id, "whisper-large-v3-turbo");
  });

  it("grants immediate Demo/TryIt readiness on macOS while background HD model downloads or when macos-native-speech is chosen", () => {
    // macOS with background HD download still in progress -> ready immediately
    assert.equal(
      resolveOnboardingModelReady({
        osType: "macos",
        preview: false,
        modelId: "parakeet-tdt-0.6b-v3",
        isDownloaded: false,
        downloadState: "downloading",
      }),
      true,
    );

    // macOS with built-in 0 MB native voice only -> ready immediately
    assert.equal(
      resolveOnboardingModelReady({
        osType: "macos",
        preview: false,
        modelId: MACOS_NATIVE_SPEECH_ID,
        isDownloaded: false,
        downloadState: "none",
      }),
      true,
    );

    // Windows/Linux still waits for downloadState === "ready"
    assert.equal(
      resolveOnboardingModelReady({
        osType: "windows",
        preview: false,
        modelId: "parakeet-tdt-0.6b-v3",
        isDownloaded: false,
        downloadState: "downloading",
      }),
      false,
    );
    assert.equal(
      resolveOnboardingModelReady({
        osType: "windows",
        preview: false,
        modelId: "parakeet-tdt-0.6b-v3",
        isDownloaded: true,
        downloadState: "ready",
      }),
      true,
    );
  });
});
