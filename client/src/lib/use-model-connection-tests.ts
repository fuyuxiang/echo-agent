import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { providersTestModelConnection, type ModelEntry, type ModelProviderEntry, type ProviderListModel } from "./agent-client";

type ModelTestResult = { status: "testing" } | {
  status: "success" | "error";
  completedAt: number;
  durationMs: number;
  error?: string;
};

/** Diagnostic results belong to this settings visit and exact catalog snapshot.
 * They are never persisted as model availability or used to change selection. */
export function useModelConnectionTests(catalog: ProviderListModel) {
  const [results, setResults] = useState<Record<string, ModelTestResult>>({});
  const generation = useRef(0);
  const pending = useRef(new Set<string>());
  useLayoutEffect(() => {
    generation.current += 1;
    pending.current.clear();
    setResults({});
    return () => { generation.current += 1; };
  }, [catalog]);

  // Results are short-lived observations, not a permanent health indicator.
  useEffect(() => {
    const expiry = Object.values(results).flatMap(result => result.status === "testing" ? [] : [result.completedAt + 15 * 60_000]);
    if (!expiry.length) return;
    const timer = window.setTimeout(() => setResults(current => Object.fromEntries(Object.entries(current)
      .filter(([, result]) => result.status === "testing" || result.completedAt + 15 * 60_000 > Date.now()))),
    Math.max(0, Math.min(...expiry) - Date.now()));
    return () => window.clearTimeout(timer);
  }, [results]);

  const test = async (provider: ModelProviderEntry, model: ModelEntry) => {
    const id = model.modelId;
    if (pending.current.has(id)) return;
    pending.current.add(id);
    const startedGeneration = generation.current;
    setResults(current => ({ ...current, [id]: { status: "testing" } }));
    const started = performance.now();
    let error: string | undefined;
    try {
      await providersTestModelConnection({ ...provider, apiKey: undefined }, model.remoteModelId || id);
    } catch (cause) {
      error = String(cause).replace(/^Error:\s*/, "") || "请求未完成，请重试。";
    }
    if (generation.current !== startedGeneration) return;
    pending.current.delete(id);
    setResults(current => ({ ...current, [id]: {
      status: error ? "error" : "success", error,
      completedAt: Date.now(), durationMs: Math.max(0, performance.now() - started),
    } }));
  };
  return { results, test };
}

export function modelTestDuration(durationMs: number): string {
  return durationMs < 100 ? "不足 0.1 秒" : `${(durationMs / 1000).toFixed(1)} 秒`;
}
