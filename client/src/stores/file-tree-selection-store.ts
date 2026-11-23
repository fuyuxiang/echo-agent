import { create } from "zustand";

interface FileTreeSelectionState {
  selectedPaths: Set<string>;
  anchorPath: string | null;

  select(absolutePaths: string[], anchor?: string): void;
  clear(): void;
  add(path: string): void;
  remove(path: string): void;
  toggle(path: string): void;
  rangeSelect(visiblePaths: string[], anchor: string, target: string): void;
  setAnchor(path: string | null): void;
}

export const useFileTreeSelectionStore = create<FileTreeSelectionState>(
  (set) => ({
    selectedPaths: new Set<string>(),
    anchorPath: null,

    select: (absolutePaths, anchor) =>
      set(() => ({
        selectedPaths: new Set(absolutePaths),
        anchorPath: anchor ?? absolutePaths[absolutePaths.length - 1] ?? null,
      })),

    clear: () => set({ selectedPaths: new Set<string>(), anchorPath: null }),

    add: (path) =>
      set((s) => {
        const next = new Set(s.selectedPaths);
        next.add(path);
        return { selectedPaths: next, anchorPath: path };
      }),

    remove: (path) =>
      set((s) => {
        if (!s.selectedPaths.has(path)) return s;
        const next = new Set(s.selectedPaths);
        next.delete(path);
        return {
          selectedPaths: next,
          anchorPath: s.anchorPath === path ? null : s.anchorPath,
        };
      }),

    toggle: (path) =>
      set((s) => {
        const next = new Set(s.selectedPaths);
        if (next.has(path)) next.delete(path);
        else next.add(path);
        return { selectedPaths: next, anchorPath: path };
      }),

    rangeSelect: (visiblePaths, anchor, target) =>
      set(() => {
        const ai = visiblePaths.indexOf(anchor);
        const ti = visiblePaths.indexOf(target);
        if (ai < 0 || ti < 0) return {};
        const [from, to] = ai <= ti ? [ai, ti] : [ti, ai];
        const selected = new Set(visiblePaths.slice(from, to + 1));
        return { selectedPaths: selected, anchorPath: target };
      }),

    setAnchor: (path) => set({ anchorPath: path }),
  }),
);
