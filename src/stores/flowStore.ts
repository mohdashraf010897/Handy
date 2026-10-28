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
  /**
   * Persists new settings, rolling back on failure. Saves run one at a time;
   * pass an updater so each applies to the latest settings rather than to a
   * value captured by an earlier render.
   */
  save: (update: FlowSettingsUpdate) => Promise<boolean>;
}

export type FlowSettingsUpdate =
  | FlowSettings
  | ((current: FlowSettings) => FlowSettings);

const errorMessage = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

let saveQueue: Promise<unknown> = Promise.resolve();
let pendingSaves = 0;

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

  save: (update) => {
    const run = async () => {
      const previous = get().settings;
      if (!previous) return false;
      const next = typeof update === "function" ? update(previous) : update;
      set({ settings: next });
      try {
        const result = await commands.setFlowSettings(next);
        if (result.status === "error") throw new Error(result.error);
        set({ settings: result.data });
      } catch (error) {
        console.error("Failed to save per-app tone settings:", error);
        set({ settings: previous });
        toast.error(errorMessage(error));
        return false;
      }
      // The rule that applies to the last app may have changed.
      await get().refreshLastApp();
      return true;
    };

    pendingSaves += 1;
    set({ isSaving: true });
    const saved = saveQueue.then(run);
    saveQueue = saved.catch(() => undefined);
    return saved.finally(() => {
      pendingSaves -= 1;
      if (pendingSaves === 0) set({ isSaving: false });
    });
  },
}));
