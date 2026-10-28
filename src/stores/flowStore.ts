import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  type AppToneRule,
  type FlowSettings,
  type LastFlowApp,
} from "@/bindings";

interface FlowStore {
  settings: FlowSettings | null;
  builtins: AppToneRule[];
  lastApp: LastFlowApp | null;
  isLoading: boolean;
  isSaving: boolean;
  load: () => Promise<void>;
  refreshLastApp: () => Promise<void>;
  /** Persists `next`, rolling back to the previous value on failure. */
  save: (next: FlowSettings) => Promise<boolean>;
}

const errorMessage = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

export const useFlowStore = create<FlowStore>()((set, get) => ({
  settings: null,
  builtins: [],
  lastApp: null,
  isLoading: true,
  isSaving: false,

  load: async () => {
    set({ isLoading: true });
    try {
      const [settings, builtins, lastApp] = await Promise.all([
        commands.getFlowSettings(),
        commands.getFlowBuiltinAppTones(),
        commands.getFlowLastApp(),
      ]);
      set({ settings, builtins, lastApp });
    } catch (error) {
      console.error("Failed to load per-app tone settings:", error);
      toast.error(errorMessage(error));
    } finally {
      set({ isLoading: false });
    }
  },

  refreshLastApp: async () => {
    try {
      set({ lastApp: await commands.getFlowLastApp() });
    } catch (error) {
      console.error("Failed to read last dictated app:", error);
    }
  },

  save: async (next) => {
    const previous = get().settings;
    set({ settings: next, isSaving: true });
    try {
      const result = await commands.setFlowSettings(next);
      if (result.status === "error") throw new Error(result.error);
      set({ settings: result.data });
      // The rule that applies to the last app may have changed.
      await get().refreshLastApp();
      return true;
    } catch (error) {
      console.error("Failed to save per-app tone settings:", error);
      set({ settings: previous });
      toast.error(errorMessage(error));
      return false;
    } finally {
      set({ isSaving: false });
    }
  },
}));
