import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import EdgeDock from "./EdgeDock";
import {
  applyTheme,
  getStoredTheme,
  syncThemeFromSettings,
} from "@/lib/utils/theme";
import type { Theme } from "@/bindings";
import "@/i18n";

const hasTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

applyTheme(getStoredTheme());
if (hasTauri) {
  syncThemeFromSettings();
  void listen<Theme>("theme-changed", (event) => applyTheme(event.payload));
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <EdgeDock />
  </React.StrictMode>,
);
