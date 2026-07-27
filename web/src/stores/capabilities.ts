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
  probe: () => Promise<void>;
  reset: () => void;
}

export const useCapabilitiesStore = create<CapabilitiesState>((set, get) => ({
  admin: null,
  inflight: null,

  probe: async () => {
    const state = get();
    if (state.admin !== null) return;
    if (state.inflight) return state.inflight;

    const inflight = (async () => {
      try {
        const data = await apiFetch<{ admin: boolean }>("/capabilities");
        set({ admin: data.admin });
      } catch {
        // A failed probe must not disable the UI: fall back to optimistic
        // (assume allowed) and let the endpoint's own 403 stay authoritative.
        // That is the pre-existing behaviour, so a gateway too old to serve
        // /capabilities degrades to it rather than locking the user out.
        set({ admin: true });
      } finally {
        set({ inflight: null });
      }
    })();

    set({ inflight });
    return inflight;
  },

  reset: () => set({ admin: null, inflight: null }),
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
