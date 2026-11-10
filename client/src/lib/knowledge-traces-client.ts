import { invoke } from "@tauri-apps/api/core";
import { isTauriAvailable } from "./tauri-kb-reader";
import { useKnowledgeStore, type KnowledgeTurnTrace } from "@/stores/knowledge-store";

function validTrace(value: unknown): value is KnowledgeTurnTrace {
  if (!value || typeof value !== "object") return false;
  const trace = value as KnowledgeTurnTrace;
  if (!Array.isArray(trace.selectedSources) || trace.selectedSources.some(source => source !== "personal" && source !== "organization")) return false;
  if (trace.personal?.state === "used") return Array.isArray(trace.personal.items) && trace.personal.items.every(item => typeof item.title === "string") && typeof trace.personal.resultCount === "number";
  return !trace.personal || ["idle", "no-match", "blocked", "error"].includes(trace.personal.state);
}

/** Save before submitting the model prompt, so its citations survive a crash. */
export async function persistKnowledgeTrace(sessionId: string, promptId?: string): Promise<void> {
  if (!promptId || !isTauriAvailable() || sessionId.startsWith("group:")) return;
  const trace = useKnowledgeStore.getState().turnTraces[sessionId]?.[promptId];
  if (!trace || !trace.selectedSources.length) return;
  await invoke("knowledge_trace_save", { sessionId, promptId, trace });
}

const traceLoads = new Map<string, Promise<void>>();

export function invalidateKnowledgeTraceLoad(sessionId: string): void {
  traceLoads.delete(sessionId);
  useKnowledgeStore.getState().setTraceLoadError(sessionId);
}

export function restoreKnowledgeTraces(sessionId: string): Promise<void> {
  const pending = traceLoads.get(sessionId);
  if (pending) return pending;

  const isCurrent = () => traceLoads.get(sessionId) === request;
  const request: Promise<void> = Promise.resolve()
    .then(() => hydrateKnowledgeTraces(sessionId, isCurrent))
    .then(() => {
      if (isCurrent()) useKnowledgeStore.getState().setTraceLoadError(sessionId);
    }, error => {
      if (isCurrent()) useKnowledgeStore.getState().setTraceLoadError(sessionId, `历史知识来源恢复失败：${String(error).replace(/^Error:\s*/, "")}。对话仍可继续，原记录已保留。`);
    })
    .finally(() => { if (isCurrent()) traceLoads.delete(sessionId); });
  traceLoads.set(sessionId, request);
  return request;
}

export async function hydrateKnowledgeTraces(sessionId: string, isCurrent = () => true): Promise<void> {
  if (!isTauriAvailable()) return;
  let cursor: string | null = null;
  const seen = new Set<string>();
  do {
    if (!isCurrent()) return;
    const raw: { traces: unknown; nextCursor: string | null } | null = await invoke<{ traces: unknown; nextCursor: string | null } | null>("knowledge_traces_load", { sessionId, cursor });
    if (!isCurrent()) return;
    if (raw == null) return; // A legacy bridge/fixture has no provenance payload.
    if (!raw.traces || typeof raw.traces !== "object" || Array.isArray(raw.traces)) throw new Error("历史知识来源记录格式无效");
    const restored: Record<string, KnowledgeTurnTrace> = {};
    for (const [id, trace] of Object.entries(raw.traces)) {
      if (!validTrace(trace)) throw new Error("历史知识来源记录不可读，原记录已保留");
      restored[id] = trace;
    }
    useKnowledgeStore.getState().restoreTurnTraces(sessionId, restored);
    cursor = raw.nextCursor;
    if (cursor && (typeof cursor !== "string" || seen.has(cursor))) throw new Error("历史知识来源分页无效");
    if (cursor) seen.add(cursor);
  } while (cursor);
}
