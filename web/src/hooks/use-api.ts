import { useState, useEffect, useCallback, useRef } from "react";
import { apiFetch } from "../lib/api";

/**
 * GET a path and expose the request as loading / error / data.
 *
 * Refetching is keyed on `path` alone: callers vary the query string (filters,
 * pagination) and get a new request for free. There is deliberately no extra
 * dependency array — spreading one into useEffect's deps made the array's
 * length vary between renders, which React does not support.
 *
 * `path === null` skips the request entirely and settles as not-loading with no
 * data. Hooks cannot be called conditionally, so this is how a caller expresses
 * "not yet" or "never" — e.g. an admin-only endpoint the current token would be
 * refused on, where firing the request only yields a 403 to hide from the user.
 *
 * Uses AbortController to cancel stale requests when the path changes or the
 * component unmounts, preventing race conditions where a slow response from a
 * previous path overwrites fresh data.
 */
export function useApi<T>(path: string | null) {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(path !== null);
  const [error, setError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const refetch = useCallback(async () => {
    if (path === null) {
      setLoading(false);
      setError(null);
      setData(null);
      return;
    }
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    setLoading(true);
    setError(null);
    try {
      const result = await apiFetch<T>(path);
      if (controller.signal.aborted) return;
      setData(result);
    } catch (e: unknown) {
      if (controller.signal.aborted) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (!controller.signal.aborted) {
        setLoading(false);
      }
    }
  }, [path]);

  useEffect(() => {
    refetch();
    return () => { abortRef.current?.abort(); };
  }, [refetch]);

  return { data, loading, error, refetch };
}
