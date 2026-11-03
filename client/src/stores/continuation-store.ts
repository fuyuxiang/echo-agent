import { create } from "zustand";

/** Shared by the reply menu, top bar and /fork, including before React commits. */
export const useContinuationStore = create<{
  pending: Record<string, boolean>;
  setPending: (sessionId:string, pending:boolean) => void;
}>(set => ({
  pending:{},
  setPending:(id,pending) => set(state => {
    const next = {...state.pending};
    if (pending) next[id] = true; else delete next[id];
    return {pending:next};
  }),
}));
