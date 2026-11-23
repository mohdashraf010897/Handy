import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { STUDIO_NAV_ITEMS, resolveStudioNavigation } from "./Sidebar";

describe("Studio Sidebar navigation", () => {
  it("exposes the 4 primary Studio surfaces above Settings", () => {
    assert.deepEqual(
      STUDIO_NAV_ITEMS.map((item) => item.id),
      ["desk", "meetings", "lexicon", "voice"],
    );
  });

  it("maps primary and legacy section payloads onto the 5-surface Studio shell", () => {
    assert.deepEqual(resolveStudioNavigation("desk"), { section: "desk" });
    assert.deepEqual(resolveStudioNavigation("history"), { section: "desk" });
    assert.deepEqual(resolveStudioNavigation("meetings"), {
      section: "meetings",
    });
    assert.deepEqual(resolveStudioNavigation("lexicon"), {
      section: "lexicon",
    });
    assert.deepEqual(resolveStudioNavigation("voice"), { section: "voice" });
    assert.deepEqual(resolveStudioNavigation("flowTone"), { section: "voice" });
    assert.deepEqual(resolveStudioNavigation("postprocessing"), {
      section: "voice",
    });
    assert.deepEqual(resolveStudioNavigation("settings"), {
      section: "settings",
      settingsTab: "shortcuts",
    });
    assert.deepEqual(resolveStudioNavigation("general"), {
      section: "settings",
      settingsTab: "shortcuts",
    });
    assert.deepEqual(resolveStudioNavigation("models"), {
      section: "settings",
      settingsTab: "models",
    });
    assert.deepEqual(resolveStudioNavigation("advanced"), {
      section: "settings",
      settingsTab: "system",
    });
    assert.deepEqual(resolveStudioNavigation("about"), {
      section: "settings",
      settingsTab: "about",
    });
    assert.deepEqual(resolveStudioNavigation("debug"), {
      section: "settings",
      settingsTab: "debug",
    });
    assert.equal(resolveStudioNavigation("nonexistent"), null);
  });
});
