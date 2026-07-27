import { create } from "zustand";
import { useEffect } from "react";
import { apiFetch } from "../lib/api";

/**
 * What the logged-in token is allowed to do, per GET /capabilities.
 *
 * The dashboard mixes two token scopes on the same pages: most endpoints accept
 * an api token, but knowledge upload/delete, skill delete/import/dep-install,
 * memory writes and /config require an admin one. Deployments that configure a
 * separate `admin_tokens` therefore had pages where every control rendered
 * enabled and then failed with a bare 403 toast — the UI had no way to know the
 * boundary short of firing the request.
 *
 * Kept in a store rather than a per-page `useApi` so the probe runs once per
 * login instead of once per page mount.
 */
interface CapabilitiesState {
  /** null = not probed yet. Distinguishes "unknown" from "known not admin" so
   *  the UI does not flash controls as disabled before the answer lands. */
  admin: boolean | null;
  /** Shared across concurrent callers so two pages mounting together issue one
   *  request instead of two. Cleared once settled. */
  inflight: Promise<void> | null;
  /** Bumped by reset(). A probe stamps the generation it started under and
   *  discards its own result if that no longer matches — otherwise a probe
   *  issued for the previous token could resolve after a logout/re-login and
   *  write the *old* token's scope over the new one, and its `finally` could
   *  clear the new probe's inflight promise. */
  generation: number;
  probe: () => Promise<void>;
  reset: () => void;
}

export const useCapabilitiesStore = create<CapabilitiesState>((set, get) => ({
  admin: null,
  inflight: null,
  generation: 0,

  probe: async () => {
    const state = get();
    if (state.admin !== null) return;
    if (state.inflight) return state.inflight;

    const startedAt = state.generation;
    const isStale = () => get().generation !== startedAt;

    const inflight = (async () => {
      try {
        const data = await apiFetch<{ admin: boolean }>("/capabilities");
        if (isStale()) return;
        set({ admin: data.admin });
      } catch {
        // A failed probe must not disable the UI: fall back to optimistic
        // (assume allowed) and let the endpoint's own 403 stay authoritative.
        // That is the pre-existing behaviour, so a gateway too old to serve
        // /capabilities degrades to it rather than locking the user out.
        if (isStale()) return;
        set({ admin: true });
      } finally {
        // Only clear our own inflight — a newer probe owns the slot otherwise.
        if (!isStale()) set({ inflight: null });
      }
    })();

    set({ inflight });
    return inflight;
  },

  reset: () => set((s) => ({ admin: null, inflight: null, generation: s.generation + 1 })),
}));

/**
 * true / false once known, null while probing. Callers should treat null as
 * "allow" for enablement so nothing is disabled on a slow first paint.
 */
export function useIsAdmin(): boolean | null {
  const admin = useCapabilitiesStore((s) => s.admin);
  const probe = useCapabilitiesStore((s) => s.probe);
  useEffect(() => {
    void probe();
  }, [probe]);
  return admin;
}
