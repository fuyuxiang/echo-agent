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
  /**
   * Whether this deployment authenticates at all, per GET /capabilities.
   * null = not probed yet.
   *
   * The dashboard used to treat `!!token` as "logged in", which broke the
   * supported open / no-token mode: an empty token is correct there, but Layout
   * bounced it to /login, Login's probe succeeded and navigated back to /, and
   * Layout bounced it again. Only entering a nonsense non-empty token escaped
   * the loop. The server now reports the fact, so the UI stops guessing.
   */
  authRequired: boolean | null;
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
  authRequired: null,
  inflight: null,
  generation: 0,

  probe: async () => {
    const state = get();
    if (state.admin !== null && state.authRequired !== null) return;
    if (state.inflight) return state.inflight;

    const startedAt = state.generation;
    const isStale = () => get().generation !== startedAt;

    const inflight = (async () => {
      try {
        const data = await apiFetch<{ admin: boolean; authRequired?: boolean }>(
          "/capabilities",
        );
        if (isStale()) return;
        set({
          admin: data.admin,
          // A gateway too old to report the field is treated as requiring auth:
          // that is the pre-existing behaviour, and assuming "open" on silence
          // would hide the login screen on a deployment that needs it.
          authRequired: data.authRequired ?? true,
        });
      } catch {
        // A failed probe must not disable the UI: fall back to optimistic
        // (assume allowed) and let the endpoint's own 403 stay authoritative.
        // That is the pre-existing behaviour, so a gateway too old to serve
        // /capabilities degrades to it rather than locking the user out.
        //
        // A 401 here is itself the answer for authRequired: the server refused
        // an unauthenticated read, so this deployment does authenticate.
        if (isStale()) return;
        set({ admin: true, authRequired: true });
      } finally {
        // Only clear our own inflight — a newer probe owns the slot otherwise.
        if (!isStale()) set({ inflight: null });
      }
    })();

    set({ inflight });
    return inflight;
  },

  reset: () => set((s) => ({
    admin: null,
    authRequired: null,
    inflight: null,
    generation: s.generation + 1,
  })),
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

/**
 * Whether this deployment needs a token: true / false once known, null while
 * probing. Unlike useIsAdmin, null must NOT be collapsed to a default by the
 * caller — Layout has to wait for the real answer, because guessing either way
 * is a visible bug (guess "required" and open mode loops back to /login; guess
 * "not required" and a real deployment flashes the dashboard before its first
 * 401).
 */
export function useAuthRequired(): boolean | null {
  const authRequired = useCapabilitiesStore((s) => s.authRequired);
  const probe = useCapabilitiesStore((s) => s.probe);
  useEffect(() => {
    void probe();
  }, [probe]);
  return authRequired;
}
