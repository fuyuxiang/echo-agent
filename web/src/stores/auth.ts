import { create } from "zustand";

interface AuthState {
  token: string | null;
  setToken: (token: string) => void;
  logout: () => void;
  isAuthenticated: () => boolean;
}

export const useAuthStore = create<AuthState>((set, get) => ({
  token: localStorage.getItem("echo_token"),
  setToken: (token) => {
    localStorage.setItem("echo_token", token);
    set({ token });
  },
  logout: () => {
    localStorage.removeItem("echo_token");
    set({ token: null });
  },
  isAuthenticated: () => !!get().token,
}));
