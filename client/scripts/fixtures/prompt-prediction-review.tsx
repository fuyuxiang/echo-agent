// Real ChatView/Composer with a controlled native prediction transport.
import "./ui-surfaces";
import { useSessionStore } from "../../src/stores/session-store";
import { usePromptPredictionStore } from "../../src/stores/prompt-prediction-store";
import { useSessionsStore } from "../../src/stores/sessions-store";
const transport = (window as any).__TAURI_INTERNALS__;
const original = transport.invoke;
const requests: { sessionId: string; generation: number; model?: string; resolve: (value: unknown) => void }[] = [];
transport.invoke = (command: string, args: any = {}) => {
  if (command === "agent_suggest_prompt") return new Promise(resolve => requests.push({ ...args, resolve }));
  return original(command, args);
};
let sequence = 0;
let promptId = "";
const start = () => {
  const store = useSessionStore.getState();
  const sid = store.sessionId!;
  promptId = `prediction-turn-${++sequence}`;
  store.pushUser("请整理实施方案", [], sid);
  store.startStreaming(sid, promptId);
};
Object.assign(window, { predictionReview: {
  requests,
  start,
  complete() {
    const store = useSessionStore.getState(), sid = store.sessionId!;
    store.applyUpdate({ __sessionId: sid, sessionUpdate: "agent_message_chunk", content: { type: "text", text: "方案已整理，包含中文适配、草稿保护和测试验证。" }, _meta: { promptId } });
    store.markComplete({ sessionId: sid, promptId, stopReason: "end_turn" });
  },
  answer(index: number, text: string) {
    const request = requests[index];
    request.resolve({ suggestion: text, generation: request.generation });
  },
  switchSession(id: string) { useSessionStore.getState().setSession(id); },
  enabled(value: boolean) { usePromptPredictionStore.getState().save({ enabled: value, model: "" }); },
  draft() { const sid = useSessionStore.getState().sessionId!; return useSessionsStore.getState().drafts[sid] ?? ""; },
} });
