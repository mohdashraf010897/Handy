import { NotebookPen } from "lucide-react";
import { MeetingsWorkspace } from "./MeetingsWorkspace";

export { MeetingsWorkspace };
export * from "./api";
export { LiveMeetingCard } from "./LiveMeetingCard";
export { MeetingDetailPane } from "./MeetingDetailPane";

/** Sidebar section for Wave 3 (SV-78) Bot-Free Meeting Notetaker (one-line hook in Sidebar.tsx). */
export const MEETINGS_SECTION = {
  labelKey: "meetings.sidebar",
  icon: NotebookPen,
  component: MeetingsWorkspace,
  enabled: () => true,
};
