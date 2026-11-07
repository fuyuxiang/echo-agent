import { invoke } from "@tauri-apps/api/core";
import type { KbEntry } from "./knowledge-base";
import { isTauriAvailable } from "./tauri-kb-reader";

export type PersonalKnowledgeIndexState = "idle" | "indexing" | "ready" | "degraded" | "error";

export interface PersonalKnowledgeIndexStatus {
  state: PersonalKnowledgeIndexState;
  warnings?: string[];
  message?: string | null;
  fileCount: number;
  chunkCount: number;
  embeddedChunkCount: number;
  pendingEmbeddingCount: number;
  lastUpdatedAt?: number | null;
  embeddingModel: string;
  rerankModel: string;
}

export interface PersonalKnowledgeSearchItem extends KbEntry {
  sourceLabel: string;
  path: string;
  startLine: number;
  endLine: number;
  score: number;
}

export interface PersonalKnowledgeSearchResponse {
  items: PersonalKnowledgeSearchItem[];
  retrievalMode: "none" | "keyword" | "keyword-reranked" | "hybrid" | "hybrid-reranked";
  degradedReason?: string | null;
  index: PersonalKnowledgeIndexStatus;
}

let searchRequestSequence = 0;

export function createPersonalKnowledgeSearchRequestId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return `personal-knowledge-${uuid}`;
  searchRequestSequence = (searchRequestSequence + 1) % Number.MAX_SAFE_INTEGER;
  return `personal-knowledge-${Date.now()}-${searchRequestSequence}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function parseStatus(value: unknown): PersonalKnowledgeIndexStatus | null {
  if (!isRecord(value)) return null;
  if (
    typeof value.state !== "string"
    || typeof value.fileCount !== "number"
    || typeof value.chunkCount !== "number"
    || typeof value.embeddedChunkCount !== "number"
    || typeof value.pendingEmbeddingCount !== "number"
    || typeof value.embeddingModel !== "string"
    || typeof value.rerankModel !== "string"
  ) return null;
  return {...value,warnings:Array.isArray(value.warnings)?value.warnings.filter((item):item is string=>typeof item==="string"):[]} as unknown as PersonalKnowledgeIndexStatus;
}

function parseSearchResponse(value: unknown): PersonalKnowledgeSearchResponse | null {
  if (!isRecord(value) || !Array.isArray(value.items)) return null;
  const index = parseStatus(value.index);
  if (!index) return null;
  const items = value.items.filter((item): item is PersonalKnowledgeSearchItem => {
    if (!isRecord(item)) return false;
    return typeof item.id === "string"
      && typeof item.title === "string"
      && typeof item.snippet === "string"
      && typeof item.source === "string"
      && typeof item.url === "string";
  });
  return {
    items,
    retrievalMode: value.retrievalMode === "hybrid-reranked"
      || value.retrievalMode === "hybrid"
      || value.retrievalMode === "keyword-reranked"
      || value.retrievalMode === "keyword"
      ? value.retrievalMode
      : "none",
    degradedReason: typeof value.degradedReason === "string" ? value.degradedReason : null,
    index,
  };
}

/** Returns null outside Tauri or when a legacy/mock bridge has no payload. */
export async function searchPersonalKnowledge(
  query: string,
  limit = 5,
  requestId?: string,
): Promise<PersonalKnowledgeSearchResponse | null> {
  if (!isTauriAvailable()) return null;
  const raw = await invoke<unknown>("personal_knowledge_search", {
    query,
    limit,
    requestId: requestId ?? null,
  });
  if (raw == null) return null;
  const response = parseSearchResponse(raw);
  if (!response) throw new Error("个人知识库返回了无效的检索结果");
  return response;
}

export async function cancelPersonalKnowledgeSearch(requestId: string): Promise<boolean> {
  if (!isTauriAvailable()) return false;
  return invoke<boolean>("personal_knowledge_cancel_search", { requestId });
}

export async function rebuildPersonalKnowledgeIndex(): Promise<PersonalKnowledgeIndexStatus | null> {
  if (!isTauriAvailable()) return null;
  return parseStatus(await invoke<unknown>("personal_knowledge_rebuild"));
}

export async function getPersonalKnowledgeIndexStatus(): Promise<PersonalKnowledgeIndexStatus | null> {
  if (!isTauriAvailable()) return null;
  return parseStatus(await invoke<unknown>("personal_knowledge_index_status"));
}
