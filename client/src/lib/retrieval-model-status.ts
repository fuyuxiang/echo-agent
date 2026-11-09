import { invoke } from "@tauri-apps/api/core";
import { isTauriAvailable } from "./tauri-kb-reader";

const MODEL_STATES = ["missing", "downloading", "verifying", "ready", "error", "unsupported"] as const;
export const RETRIEVAL_MODELS_CHANGED = "echo:retrieval-models-changed";

export interface RetrievalModelStatus {
  state: typeof MODEL_STATES[number];
  source?: "bundled" | "repaired" | "missing";
  message?: string | null;
  operationError?: string | null;
  downloadedBytes: number;
  totalBytes: number;
  embeddingReady: boolean;
  rerankerReady: boolean;
}

export function parseRetrievalModelStatus(value: unknown): RetrievalModelStatus | null {
  if (!value || typeof value !== "object") return null;
  const status = value as RetrievalModelStatus;
  return MODEL_STATES.includes(status.state)
    && Number.isFinite(status.totalBytes)
    && Number.isFinite(status.downloadedBytes)
    && typeof status.embeddingReady === "boolean"
    && typeof status.rerankerReady === "boolean" ? status : null;
}

export async function getRetrievalModelStatus(): Promise<RetrievalModelStatus | null> {
  if (!isTauriAvailable()) return null;
  return parseRetrievalModelStatus(await invoke<unknown>("retrieval_models_status"));
}
