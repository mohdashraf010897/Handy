import { Sparkles } from "lucide-react";
import {
  VoiceStyleWorkspace,
  type VoiceStyleTab,
  type VoiceStyleWorkspaceProps,
} from "./VoiceStyleWorkspace";

export {
  VoiceStyleWorkspace,
  type VoiceStyleTab,
  type VoiceStyleWorkspaceProps,
};

export const VOICE_STYLE_SECTION = {
  labelKey: "studio.voice.title",
  icon: Sparkles,
  component: VoiceStyleWorkspace,
  enabled: () => true,
};
