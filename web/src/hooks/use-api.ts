import { useState, useEffect, useCallback } from "react";
import { apiFetch } from "../lib/api";

/**
 * GET a path and expose the request as loading / error / data.
 *
 * Refetching is keyed on `path` alone: callers vary the query string (filters,
 * pagination) and get a new request for free. There is deliberately no extra
 * dependency array — spreading one into useEffect's deps made the array's
 * length vary between renders, which React does not support.
 */
export function useApi<T>(path: string) {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refetch = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await apiFetch<T>(path);
      setData(result);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [path]);

  useEffect(() => { refetch(); }, [refetch]);

  return { data, loading, error, refetch };
}
