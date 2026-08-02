import { create } from "zustand";
import { TOKEN_STORAGE_KEY, setUnauthorizedHandler } from "../lib/api";
import { useCapabilitiesStore } from "./capabilities";

interface AuthState {
  token: string | null;
  setToken: (token: string) => void;
  logout: () => void;
  isAuthenticated: () => boolean;
}

export const useAuthStore = create<AuthState>((set, get) => ({
  token: localStorage.getItem(TOKEN_STORAGE_KEY),
  setToken: (token) => {
    localStorage.setItem(TOKEN_STORAGE_KEY, token);
    // The probed scope belongs to the *previous* token. Dropping it forces a
    // re-probe, otherwise signing in with an admin token after an api-token
    // session would keep the admin-only controls disabled.
    useCapabilitiesStore.getState().reset();
    set({ token });
  },
  logout: () => {
    localStorage.removeItem(TOKEN_STORAGE_KEY);
    useCapabilitiesStore.getState().reset();
    set({ token: null });
    // The socket is not closed here on purpose: clearing the token unmounts
    // Layout (and with it every subscriber), whose cleanup releases the last
    // channel reference and closes the connection. Calling into lib/ws from
    // this module would also make the two import each other.
  },
  isAuthenticated: () => !!get().token,
}));

// A 401 from any request means the token is no longer accepted. Clearing it
// here (rather than in api.ts) keeps the store the single owner of auth state,
// and Layout's <Navigate> then routes to /login without a page reload.
//
// Preserve the fact learned from the 401 after logout resets the capability
// probe. Without this, the initial unauthenticated /capabilities request calls
// logout(), reset() bumps the probe generation, and the probe's catch discards
// its own 401 as stale. authRequired then remains null and Layout renders a
// permanent blank screen instead of navigating to /login.
setUnauthorizedHandler(() => {
  useAuthStore.getState().logout();
  useCapabilitiesStore.setState({ admin: true, authRequired: true });
});
