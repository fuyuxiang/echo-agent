import { useEffect, useRef, useState } from "react";
import { agentSuggestPrompt } from "./agent-client";
import { useDraftRevision } from "./draft-lifecycle";
import { inferTurnOrigin } from "./execution-turn";
import { usePromptPredictionStore } from "@/stores/prompt-prediction-store";
import type { ChatMessage } from "@/stores/session-store";
import type { ModelOption } from "@/components/ModelSelector";

export interface PromptPrediction { sessionId: string; turnId: string; generation: number; text: string }

/** Never silently use a large conversation model for a per-turn background call. */
export function predictionModel(configured: string, models: ModelOption[]): string | undefined {
  return configured || models.find(model => /(?:mini|nano|flash|haiku|small|luna|echo-agent-build-0\.1)/i
    .test(model.remoteModelId ?? model.id))?.id;
}

export function usePromptPrediction({ sessionId, messages, busy, draft, models = [] }: {
  sessionId: string | null;
  messages: ChatMessage[];
  busy: boolean;
  draft: string;
  models?: ModelOption[];
}): { prediction: PromptPrediction | null; dismiss: () => void } {
  const enabled = usePromptPredictionStore(s => s.enabled);
  const configuredModel = usePromptPredictionStore(s => s.model);
  const model = predictionModel(configuredModel, models);
  const revision = useDraftRevision(sessionId ?? undefined);
  const last = messages[messages.length - 1];
  const origin = last?.execution?.origin ?? inferTurnOrigin(last?.promptId).origin;
  const turnId = last?.role === "assistant" && last.complete && !last.replayed
    && (origin === "user" || origin === "plan_resume")
    && (!last.stopReason || last.stopReason === "end_turn")
    && last.parts.some(part => part.kind === "text" && part.text.trim())
    ? last.promptId ?? last.id : null;
  // Opening a completed transcript does not generate a new prediction. Only a
  // newly completed turn observed by this mounted conversation can trigger it.
  const scope = useRef({ sessionId, consumed: turnId });
  const generation = useRef(0);
  const [loaded, setLoaded] = useState<(PromptPrediction & { revision: number; model?: string }) | null>(null);
  const [dismissed, setDismissed] = useState<number | null>(null);

  useEffect(() => {
    let active = true;
    const current = ++generation.current;
    setLoaded(null);
    if (scope.current.sessionId !== sessionId) {
      scope.current = { sessionId, consumed: turnId };
      return;
    }
    if (!sessionId || busy || !enabled || !turnId || scope.current.consumed === turnId) return;
    scope.current.consumed = turnId;
    if (draft.length) return;
    // Coalesce completion and queue admission updates before spending a call.
    const timer = window.setTimeout(() => {
      if (!active || current !== generation.current) return;
      void agentSuggestPrompt(sessionId, current, model).then(result => {
        if (!active || current !== generation.current || result.generation !== current) return;
        const text = result.suggestion?.trim();
        if (text && Array.from(text).length <= 120 && !/[\r\n\u0000-\u001f\u007f]/.test(text)) {
          setLoaded({ sessionId, turnId, generation: current, text, revision, model });
        }
      }).catch(() => { /* Optional predictions never block sending or report a chat error. */ });
    }, 250);
    return () => { active = false; window.clearTimeout(timer); };
  }, [sessionId, turnId, busy, enabled, revision, draft, model]);

  const prediction = enabled && !busy && !draft.length && loaded?.sessionId === sessionId
    && loaded.turnId === turnId && loaded.revision === revision && loaded.model === model
    && loaded.generation !== dismissed ? loaded : null;
  return { prediction, dismiss: () => {
    generation.current += 1;
    setDismissed(loaded?.generation ?? null);
    setLoaded(null);
  } };
}
