import { create } from "zustand";
import { readDurable, writeDurable } from "@/lib/durable-ui-state";

export const PROMPT_PREDICTION_KEY = "echoagent.promptPredictions";
interface Preferences { enabled: boolean; model: string }
const defaults: Preferences = { enabled: true, model: "" };
function valid(value: unknown): value is Preferences {
  const v = value as Preferences | null;
  return !!v && typeof v.enabled === "boolean" && typeof v.model === "string"
    && v.model.length <= 256 && !/[\u0000-\u001f\u007f]/.test(v.model);
}

export const usePromptPredictionStore = create<Preferences & {
  save: (preferences: Preferences) => boolean;
}>(set => ({
  ...readDurable(PROMPT_PREDICTION_KEY, defaults, valid),
  save: preferences => {
    const next = { ...preferences, model: preferences.model.trim() };
    if (!valid(next) || !writeDurable(PROMPT_PREDICTION_KEY, next)) return false;
    set(next);
    return true;
  },
}));

// Settings changes in another desktop window retire predictions here as well.
if (typeof window !== "undefined") window.addEventListener("storage", event => {
  if (event.key === PROMPT_PREDICTION_KEY || event.key === null) {
    usePromptPredictionStore.setState(readDurable(PROMPT_PREDICTION_KEY, defaults, valid));
  }
});
