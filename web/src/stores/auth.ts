import { create } from "zustand";
import { TOKEN_STORAGE_KEY, setUnauthorizedHandler } from "../lib/api";

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
    set({ token });
  },
  logout: () => {
    localStorage.removeItem(TOKEN_STORAGE_KEY);
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
setUnauthorizedHandler(() => useAuthStore.getState().logout());
