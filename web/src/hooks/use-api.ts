import { useState, useEffect, useCallback } from "react";
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
 */
export function useApi<T>(path: string | null) {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(path !== null);
  const [error, setError] = useState<string | null>(null);

  const refetch = useCallback(async () => {
    if (path === null) {
      // Clear any state left from a previous path so a skipped request never
      // shows stale data or a stale error.
      setLoading(false);
      setError(null);
      setData(null);
      return;
    }
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
