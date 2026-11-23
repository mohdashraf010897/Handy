import { BookOpenText } from "lucide-react";
import { LexiconWorkspace } from "./LexiconWorkspace";

export { LexiconWorkspace };

export const LEXICON_SECTION = {
  labelKey: "studio.lexicon.title",
  icon: BookOpenText,
  component: LexiconWorkspace,
  enabled: () => true,
};
