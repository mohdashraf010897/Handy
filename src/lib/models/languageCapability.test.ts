import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { partitionSpokenLanguageSupport } from "./languageCapability";

const stubWeak = (lang: string) =>
  lang === "ur" || lang === "hi" || lang === "am";

describe("partitionSpokenLanguageSupport", () => {
  it("classifies ur into adapterSupported (not weak) for whisper-large-v3-turbo", () => {
    const result = partitionSpokenLanguageSupport({
      languages: ["en", "ur"],
      activeModel: {
        id: "whisper-large-v3-turbo",
        name: "Whisper Large v3 Turbo",
        supported_languages: ["en", "hi", "ur"],
      },
      isWeak: stubWeak,
    });

    assert.deepEqual(result, {
      adapterSupported: ["ur"],
      weak: [],
    });
  });

  it("keeps ur in weak for an English-only model like moonshine-base", () => {
    const result = partitionSpokenLanguageSupport({
      languages: ["en", "ur"],
      activeModel: {
        id: "moonshine-base",
        name: "Moonshine Base",
        supported_languages: ["en"],
      },
      isWeak: stubWeak,
    });

    assert.deepEqual(result, {
      adapterSupported: [],
      weak: ["ur"],
    });
  });

  it("keeps non-Hindustani weak languages in weak even when Hindustani is adapter-supported", () => {
    const result = partitionSpokenLanguageSupport({
      languages: ["ur", "hi", "am", "en"],
      activeModel: {
        id: "sense-voice-int8",
        supported_languages: [],
      },
      isWeak: stubWeak,
    });

    assert.deepEqual(result, {
      adapterSupported: ["ur", "hi"],
      weak: ["am"],
    });
  });

  it("treats English-only .en Whisper checkpoints as unsupported for Urdu", () => {
    const result = partitionSpokenLanguageSupport({
      languages: ["ur"],
      activeModel: {
        id: "whisper-small.en",
        supported_languages: ["en"],
      },
      isWeak: stubWeak,
    });

    assert.deepEqual(result, {
      adapterSupported: [],
      weak: ["ur"],
    });
  });

  it("trusts hydrated supported_languages over model id heuristics", () => {
    const result = partitionSpokenLanguageSupport({
      languages: ["ur", "de"],
      activeModel: {
        id: "whisper-custom-euro",
        supported_languages: ["en", "de", "fr"],
      },
      isWeak: stubWeak,
    });

    assert.deepEqual(result, {
      adapterSupported: [],
      weak: ["ur"],
    });
  });
});
