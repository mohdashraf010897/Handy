import { AppWindow } from "lucide-react";
import { PerAppToneSettings } from "./PerAppToneSettings";

export { PerAppToneSettings };

/** Sidebar section for the fork's per-app tone screen (one-line hook in Sidebar.tsx). */
export const FLOW_TONE_SECTION = {
  labelKey: "flow.sidebar",
  icon: AppWindow,
  component: PerAppToneSettings,
  enabled: () => true,
};
