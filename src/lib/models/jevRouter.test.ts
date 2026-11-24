import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { catalog } from "@handy-computer/models";
import { routeAsrWithJev } from "./jevRouter.ts";

const curatedAsr = catalog.curatedModels.filter((m) => m.role === "asr");

describe("Wave 8C: Jev 2-Stage ASR & Multilingual Audio Router", () => {
  it("routes English-only profiles to the highest-utility streaming English specialist without hardcoded IDs", () => {
    const decision = routeAsrWithJev({
      spokenLanguages: ["en"],
      offered: curatedAsr,
      tier: "accelerated",
    });

    assert.equal(decision.isEnglishOnly, true);
    assert.equal(decision.primary?.id, "parakeet-unified-en-0.6b");
    assert.equal(decision.audioPolicy.recommendedSelectedLanguage, "en");
    assert.equal(decision.audioPolicy.requiresConstrainedDetector, false);
    assert.equal(decision.moduleTitle, "English High-Precision Module");
  });

  it("routes English + Hindi (Hinglish) to Qwen3-ASR-0.6B based on superior Hindi benchmark WER", () => {
    const decision = routeAsrWithJev({
      spokenLanguages: ["en", "hi"],
      offered: curatedAsr,
      tier: "accelerated",
    });

    assert.equal(decision.isEnglishOnly, false);
    assert.equal(decision.primary?.id, "Qwen3-ASR-0.6B");
    assert.equal(decision.audioPolicy.recommendedSelectedLanguage, "auto");
    assert.equal(decision.audioPolicy.recommendedDetectionMode, "always");
    assert.equal(decision.audioPolicy.requiresConstrainedDetector, true);
  });

  it("routes cold English + Urdu to whisper-large-v3-turbo, while Warm-Engine Gate preserves already-downloaded whisper-large-v3", () => {
    const cold = routeAsrWithJev({
      spokenLanguages: ["en", "ur"],
      offered: curatedAsr,
      tier: "accelerated",
    });
    assert.equal(cold.primary?.id, "whisper-large-v3-turbo");

    const warm = routeAsrWithJev({
      spokenLanguages: ["en", "ur"],
      offered: curatedAsr,
      tier: "accelerated",
      downloadedCatalogIds: new Set(["whisper-large-v3"]),
    });
    assert.equal(warm.primary?.id, "whisper-large-v3");
    assert.equal(warm.coldBest?.id, "whisper-large-v3-turbo");

    // Switching to English-only still upgrades away from whisper-large-v3 to parakeet-unified-en-0.6b!
    const switchedToEnglish = routeAsrWithJev({
      spokenLanguages: ["en"],
      offered: curatedAsr,
      tier: "accelerated",
      downloadedCatalogIds: new Set(["whisper-large-v3"]),
    });
    assert.equal(switchedToEnglish.primary?.id, "parakeet-unified-en-0.6b");
  });
});
