import { create } from "zustand";

export type KnowledgeSource = "personal" | "organization";

export interface KnowledgeRetrievalItem {
  title: string;
  path?: string;
  sourceLabel?: string;
  snippet?: string;
  startLine?: number;
  endLine?: number;
}

export type KnowledgeRetrievalStatus =
  | { state: "idle" }
  | { state: "searching" }
  | {
      state: "used";
      resultCount: number;
      sourceCount: number;
      titles: string[];
      items: KnowledgeRetrievalItem[];
      keywordOnly?: boolean;
      notice?: string;
    }
  | { state: "no-match"; sourceCount: number; keywordOnly?: boolean; notice?: string }
  | { state: "blocked"; message: string }
  | { state: "error"; message: string };

export interface KnowledgeTurnTrace {
  selectedSources: KnowledgeSource[];
  personal?: KnowledgeRetrievalStatus;
  organization?: {
    state: "available" | "unavailable";
    message?: string;
  };
}

interface PersistedKnowledgePreferences {
  /** Selection for the next task composer. Consumed when that task is created. */
  defaultSources: KnowledgeSource[];
  /** Optional organization scopes for the next task; empty means every authorized scope. */
  defaultOrganizationScopeIds: string[];
  /** Knowledge selection is task-owned so later messages keep the same scope. */
  sessionSources: Record<string, KnowledgeSource[]>;
  /** Per-task organization scope boundary; empty means every authorized scope. */
  sessionOrganizationScopeIds: Record<string, string[]>;
}

interface KnowledgeState extends PersistedKnowledgePreferences {
  sourceCount: number;
  retrievals: Record<string, KnowledgeRetrievalStatus>;
  turnTraces: Record<string, Record<string, KnowledgeTurnTrace>>;
  traceLoadErrors: Record<string, string | undefined>;
  setTraceLoadError: (sessionId: string, error?: string) => void;
  restoreTurnTraces: (sessionId: string, traces: Record<string, KnowledgeTurnTrace>) => void;
  setDefaultSources: (sources: KnowledgeSource[]) => void;
  setSessionSources: (sessionId: string, sources: KnowledgeSource[]) => void;
  setDefaultOrganizationScopeIds: (scopeIds: string[]) => void;
  setSessionOrganizationScopeIds: (sessionId: string, scopeIds: string[]) => void;
  forgetSession: (sessionId: string) => boolean;
  bindSessionSources: (sessionId: string, consumeDefault?: boolean,
    selection?: Pick<PersistedKnowledgePreferences, "defaultSources" | "defaultOrganizationScopeIds">) => KnowledgeSource[];
  setSourceCount: (count: number) => void;
  beginTurnTrace: (
    sessionId: string,
    promptId: string | undefined,
    sources: KnowledgeSource[],
    organization?: KnowledgeTurnTrace["organization"],
  ) => void;
  setRetrieval: (
    sessionId: string,
    status: KnowledgeRetrievalStatus,
    promptId?: string,
  ) => void;
}

const STORAGE_KEY = "echoagent.knowledge-preferences.v3";
const V2_STORAGE_KEY = "echoagent.knowledge-preferences.v2";
const LEGACY_STORAGE_KEY = "echoagent.knowledge-preferences.v1";
const MAX_PERSISTED_SESSIONS = 500;

function normalizeSources(value: unknown): KnowledgeSource[] {
  if (!Array.isArray(value)) return [];
  const selected = new Set<KnowledgeSource>();
  for (const item of value) {
    if (item === "personal" || item === "organization") selected.add(item);
  }
  return (["personal", "organization"] as const).filter((source) => selected.has(source));
}

function normalizeScopeIds(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return [...new Set(value
    .filter((item): item is string => typeof item === "string")
    .map((item) => item.trim())
    .filter((item) => item.length > 0 && item.length <= 256 && !/[,\u0000-\u001f\u007f]/.test(item)))]
    .slice(0, 64);
}

function boundSessionPreferences(
  sessionSources: Record<string, KnowledgeSource[]>,
  sessionOrganizationScopeIds: Record<string, string[]>,
): void {
  const sourceKeys = Object.keys(sessionSources);
  for (const key of sourceKeys.slice(0, Math.max(0, sourceKeys.length - MAX_PERSISTED_SESSIONS))) {
    delete sessionSources[key];
    delete sessionOrganizationScopeIds[key];
  }
  for (const key of Object.keys(sessionOrganizationScopeIds)) {
    if (!sessionSources[key]?.includes("organization")) delete sessionOrganizationScopeIds[key];
  }
}

function loadPreferences(): PersistedKnowledgePreferences {
  const fallback: PersistedKnowledgePreferences = {
    defaultSources: [],
    defaultOrganizationScopeIds: [],
    sessionSources: {},
    sessionOrganizationScopeIds: {},
  };
  if (typeof localStorage === "undefined") return fallback;
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as unknown;
    if (parsed && typeof parsed === "object") {
      const raw = parsed as Record<string, unknown>;
      const sessionSources: Record<string, KnowledgeSource[]> = {};
      if (raw.sessionSources && typeof raw.sessionSources === "object") {
        for (const [sessionId, value] of Object.entries(raw.sessionSources as Record<string, unknown>).slice(-MAX_PERSISTED_SESSIONS)) {
          if (sessionId.trim()) sessionSources[sessionId] = normalizeSources(value);
        }
      }
      const sessionOrganizationScopeIds: Record<string, string[]> = {};
      if (raw.sessionOrganizationScopeIds && typeof raw.sessionOrganizationScopeIds === "object") {
        for (const [sessionId, value] of Object.entries(raw.sessionOrganizationScopeIds as Record<string, unknown>).slice(-MAX_PERSISTED_SESSIONS)) {
          if (sessionId.trim()) sessionOrganizationScopeIds[sessionId] = normalizeScopeIds(value);
        }
      }
      const defaultSources = normalizeSources(raw.defaultSources);
      boundSessionPreferences(sessionSources, sessionOrganizationScopeIds);
      return {
        defaultSources,
        defaultOrganizationScopeIds: defaultSources.includes("organization")
          ? normalizeScopeIds(raw.defaultOrganizationScopeIds)
          : [],
        sessionSources,
        sessionOrganizationScopeIds,
      };
    }

    const v2 = JSON.parse(localStorage.getItem(V2_STORAGE_KEY) ?? "null") as unknown;
    if (v2 && typeof v2 === "object") {
      const raw = v2 as Record<string, unknown>;
      const sessionSources: Record<string, KnowledgeSource[]> = {};
      if (raw.sessionSources && typeof raw.sessionSources === "object") {
        for (const [sessionId, value] of Object.entries(raw.sessionSources as Record<string, unknown>).slice(-MAX_PERSISTED_SESSIONS)) {
          if (sessionId.trim()) sessionSources[sessionId] = normalizeSources(value);
        }
      }
      return {
        ...fallback,
        defaultSources: normalizeSources(raw.defaultSources),
        sessionSources,
      };
    }

    // V1 had an implicit personal-knowledge switch. Preserve only existing
    // task choices; the next new task intentionally starts with no source.
    const legacy = JSON.parse(localStorage.getItem(LEGACY_STORAGE_KEY) ?? "null") as unknown;
    if (!legacy || typeof legacy !== "object") return fallback;
    const raw = legacy as Record<string, unknown>;
    const sessionSources: Record<string, KnowledgeSource[]> = {};
    if (raw.sessionModes && typeof raw.sessionModes === "object") {
      for (const [sessionId, mode] of Object.entries(raw.sessionModes as Record<string, unknown>).slice(-MAX_PERSISTED_SESSIONS)) {
        if (sessionId.trim()) sessionSources[sessionId] = mode === "auto" ? ["personal"] : [];
      }
    }
    return { ...fallback, sessionSources };
  } catch {
    return fallback;
  }
}

function persistPreferences(state: Pick<KnowledgeState,
  "defaultSources" | "defaultOrganizationScopeIds" | "sessionSources" | "sessionOrganizationScopeIds"
>): boolean {
  if (typeof localStorage === "undefined") return false;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({
      defaultSources: state.defaultSources,
      defaultOrganizationScopeIds: state.defaultOrganizationScopeIds,
      sessionSources: state.sessionSources,
      sessionOrganizationScopeIds: state.sessionOrganizationScopeIds,
    }));
    return true;
  } catch {
    // Preference persistence must never block a task submission.
    return false;
  }
}

function withTurnTrace(
  traces: Record<string, KnowledgeTurnTrace>,
  promptId: string,
  trace: KnowledgeTurnTrace,
): Record<string, KnowledgeTurnTrace> {
  return { ...traces, [promptId]: trace };
}

const initial = loadPreferences();

export const useKnowledgeStore = create<KnowledgeState>((set, get) => ({
  ...initial,
  sourceCount: 0,
  retrievals: {},
  turnTraces: {},
  traceLoadErrors: {},
  setTraceLoadError: (sessionId, error) => set(state => ({ traceLoadErrors: { ...state.traceLoadErrors, [sessionId]: error } })),
  restoreTurnTraces: (sessionId, traces) => set(state => ({ turnTraces: {
    ...state.turnTraces,
    [sessionId]: { ...traces, ...state.turnTraces[sessionId] },
  } })),
  setDefaultSources: (sources) => set((state) => {
    const defaultSources = normalizeSources(sources);
    const defaultOrganizationScopeIds = defaultSources.includes("organization")
      ? state.defaultOrganizationScopeIds
      : [];
    persistPreferences({ ...state, defaultSources, defaultOrganizationScopeIds });
    return { defaultSources, defaultOrganizationScopeIds };
  }),
  setSessionSources: (sessionId, sources) => set((state) => {
    const sessionSources = { ...state.sessionSources };
    const sessionOrganizationScopeIds = { ...state.sessionOrganizationScopeIds };
    delete sessionSources[sessionId];
    sessionSources[sessionId] = normalizeSources(sources);
    if (!sessionSources[sessionId].includes("organization")) delete sessionOrganizationScopeIds[sessionId];
    boundSessionPreferences(sessionSources, sessionOrganizationScopeIds);
    persistPreferences({ ...state, sessionSources, sessionOrganizationScopeIds });
    return { sessionSources, sessionOrganizationScopeIds };
  }),
  setDefaultOrganizationScopeIds: (scopeIds) => set((state) => {
    const defaultOrganizationScopeIds = state.defaultSources.includes("organization")
      ? normalizeScopeIds(scopeIds)
      : [];
    persistPreferences({ ...state, defaultOrganizationScopeIds });
    return { defaultOrganizationScopeIds };
  }),
  setSessionOrganizationScopeIds: (sessionId, scopeIds) => set((state) => {
    const sessionOrganizationScopeIds = { ...state.sessionOrganizationScopeIds };
    if (state.sessionSources[sessionId]?.includes("organization")) {
      sessionOrganizationScopeIds[sessionId] = normalizeScopeIds(scopeIds);
    } else {
      delete sessionOrganizationScopeIds[sessionId];
    }
    boundSessionPreferences({ ...state.sessionSources }, sessionOrganizationScopeIds);
    persistPreferences({ ...state, sessionOrganizationScopeIds });
    return { sessionOrganizationScopeIds };
  }),
  forgetSession: (sessionId) => {
    let saved = false;
    set((state) => {
      const sessionSources = { ...state.sessionSources };
      const sessionOrganizationScopeIds = { ...state.sessionOrganizationScopeIds };
      const retrievals = { ...state.retrievals };
      const turnTraces = { ...state.turnTraces };
      const traceLoadErrors = { ...state.traceLoadErrors };
      delete sessionSources[sessionId];
      delete sessionOrganizationScopeIds[sessionId];
      delete retrievals[sessionId];
      delete turnTraces[sessionId];
      delete traceLoadErrors[sessionId];
      saved = persistPreferences({ ...state, sessionSources, sessionOrganizationScopeIds });
      return { sessionSources, sessionOrganizationScopeIds, retrievals, turnTraces, traceLoadErrors };
    });
    return saved;
  },
  bindSessionSources: (sessionId, consumeDefault = false, selection) => {
    const state = get();
    const submitted = selection ?? state;
    const existing = state.sessionSources[sessionId];
    if (existing) return existing;
    const sources = consumeDefault ? normalizeSources(submitted.defaultSources) : [];
    const sessionSources = { ...state.sessionSources, [sessionId]: sources };
    const sessionOrganizationScopeIds = {
      ...state.sessionOrganizationScopeIds,
      [sessionId]: consumeDefault ? normalizeScopeIds(submitted.defaultOrganizationScopeIds) : [],
    };
    boundSessionPreferences(sessionSources, sessionOrganizationScopeIds);
    // A Home composer selection applies to the task being created, not every
    // future task. New tasks therefore return to the privacy-first default.
    const consumeCurrent = consumeDefault && state.defaultSources === submitted.defaultSources
      && state.defaultOrganizationScopeIds === submitted.defaultOrganizationScopeIds;
    const defaultSources = consumeCurrent ? [] : state.defaultSources;
    const defaultOrganizationScopeIds = consumeCurrent ? [] : state.defaultOrganizationScopeIds;
    persistPreferences({
      defaultSources,
      defaultOrganizationScopeIds,
      sessionSources,
      sessionOrganizationScopeIds,
    });
    set({ defaultSources, defaultOrganizationScopeIds, sessionSources, sessionOrganizationScopeIds });
    return sources;
  },
  setSourceCount: (sourceCount) => set({ sourceCount: Math.max(0, sourceCount) }),
  beginTurnTrace: (sessionId, promptId, sources, organization) => {
    if (!promptId) return;
    set((state) => ({
      turnTraces: {
        ...state.turnTraces,
        [sessionId]: withTurnTrace(
          state.turnTraces[sessionId] ?? {},
          promptId,
          {
            selectedSources: normalizeSources(sources),
            ...(organization ? { organization } : {}),
          },
        ),
      },
    }));
  },
  setRetrieval: (sessionId, status, promptId) => set((state) => {
    const retrievals = { ...state.retrievals, [sessionId]: status };
    if (!promptId) return { retrievals };
    const sessionTraces = state.turnTraces[sessionId] ?? {};
    const current = sessionTraces[promptId] ?? {
      selectedSources: knowledgeSourcesForSession(sessionId),
    };
    return {
      retrievals,
      turnTraces: {
        ...state.turnTraces,
        [sessionId]: withTurnTrace(sessionTraces, promptId, {
          ...current,
          personal: status,
        }),
      },
    };
  }),
}));

export function knowledgeSourcesForSession(sessionId?: string): KnowledgeSource[] {
  const state = useKnowledgeStore.getState();
  return normalizeSources(sessionId
    ? state.sessionSources[sessionId] ?? []
    : state.defaultSources);
}

export function organizationScopeIdsForSession(sessionId?: string): string[] {
  const state = useKnowledgeStore.getState();
  return normalizeScopeIds(sessionId
    ? state.sessionOrganizationScopeIds[sessionId] ?? []
    : state.defaultOrganizationScopeIds);
}
