import { create } from "zustand";

export interface QuestionOption {
  id?: string;
  label: string;
  description: string;
  preview?: string;
}

export interface QuestionItem {
  id: string;
  question: string;
  options: QuestionOption[];
  multiSelect: boolean;
}

export interface QuestionRequest {
  requestId: string;
  sessionId: string;
  toolCallId: string;
  title: string;
  questions: QuestionItem[];
  mode: "default" | "plan";
  /** Seconds, when supplied by the Runtime extension version. */
  timeout?: number;
  /** Renderer deadline captured on admission, never restarted by navigation. */
  expiresAt?: number;
}

export interface QuestionResponse {
  selections: Record<string, string[]>;
  customInputs: Record<string, string>;
  activeIndex: number;
  resolving: boolean;
  error: string | null;
}

export const questionScope = (sessionId: string, requestId: string) => JSON.stringify([sessionId, requestId]);
const emptyResponse = (): QuestionResponse => ({ selections: {}, customInputs: {}, activeIndex: 0, resolving: false, error: null });

interface QuestionState {
  /** sessionId → ordered queue of pending question requests. */
  queues: Record<string, QuestionRequest[]>;
  /** Closed reverse requests may arrive again in a delayed event or snapshot. */
  closedRequestIds: string[];
  /** Drafts and in-flight ACKs belong to pending requests, not mounted cards. */
  responses: Record<string, QuestionResponse>;
  updateResponse: (sessionId: string, requestId: string, update: (response: QuestionResponse) => QuestionResponse) => void;
  beginAnswer: (sessionId: string, requestId: string) => boolean;
  finishAnswer: (sessionId: string, requestId: string, error?: string) => void;
  /** Push a new question request from the backend. */
  request: (q: QuestionRequest) => void;
  /** Remove a question from its session's queue. */
  dismiss: (requestId: string, sessionId?: string) => void;
  /** Drop every stale request after the shared agent process exits. */
  clearAll: () => void;
  /** Drop stale questions owned by a session after that session is deleted. */
  clearSession: (sessionId: string) => void;
}

export const useQuestionStore = create<QuestionState>((set) => ({
  queues: {},
  closedRequestIds: [],
  responses: {},
  updateResponse: (sessionId, requestId, update) => set(state => {
    const key = questionScope(sessionId, requestId);
    const response = state.responses[key];
    if (!response || response.resolving) return state;
    return { responses: { ...state.responses, [key]: update(response) } };
  }),
  beginAnswer: (sessionId, requestId) => {
    let admitted = false;
    set(state => {
      const request = state.queues[sessionId || "__global"]?.find(item => item.requestId === requestId);
      const key = questionScope(sessionId, requestId);
      const response = state.responses[key];
      if (!request || !response || response.resolving || (request.expiresAt != null && Date.now() >= request.expiresAt)) return state;
      admitted = true;
      return { responses: { ...state.responses, [key]: { ...response, resolving: true, error: null } } };
    });
    return admitted;
  },
  finishAnswer: (sessionId, requestId, error) => set(state => {
    const key = questionScope(sessionId, requestId);
    const response = state.responses[key];
    if (!response) return state;
    return { responses: { ...state.responses, [key]: { ...response, resolving: false, error: error ?? null } } };
  }),
  request: (q) =>
    set((s) => {
      if (s.closedRequestIds.includes(q.requestId)) return s;
      const sid = q.sessionId || "__global";
      const prev = s.queues[sid] ?? [];
      if (prev.some((pending) => pending.requestId === q.requestId)) return s;
      const admitted = { ...q, ...(q.timeout == null ? {} : { expiresAt: Date.now() + Math.max(0, q.timeout) * 1000 }) };
      return {
        queues: { ...s.queues, [sid]: [...prev, admitted] },
        responses: { ...s.responses, [questionScope(q.sessionId, q.requestId)]: emptyResponse() },
      };
    }),
  dismiss: (requestId, sessionId) =>
    set((s) => {
      const closedRequestIds = [...s.closedRequestIds.filter((id) => id !== requestId), requestId].slice(-256);
      const responses = { ...s.responses };
      for (const requests of Object.values(s.queues)) for (const request of requests) {
        if (request.requestId === requestId && (!sessionId || request.sessionId === sessionId)) {
          delete responses[questionScope(request.sessionId, requestId)];
        }
      }
      if (sessionId) {
        const prev = s.queues[sessionId];
        if (!prev) return { closedRequestIds, responses };
        return {
          closedRequestIds, responses,
          queues: {
            ...s.queues,
            [sessionId]: prev.filter((q) => q.requestId !== requestId),
          },
        };
      }
      const queues = { ...s.queues };
      for (const sid of Object.keys(queues)) {
        queues[sid] = queues[sid].filter((q) => q.requestId !== requestId);
      }
      return { queues, closedRequestIds, responses };
    }),
  clearAll: () => set({ queues: {}, closedRequestIds: [], responses: {} }),
  clearSession: (sessionId) =>
    set((state) => {
      if (!state.queues[sessionId]) return state;
      const queues = { ...state.queues };
      const responses = { ...state.responses };
      for (const request of queues[sessionId]) delete responses[questionScope(request.sessionId, request.requestId)];
      delete queues[sessionId];
      return { queues, responses };
    }),
}));

/** Select the first pending question for a given session. */
export const selectQuestionForSession =
  (sessionId: string | null) =>
  (s: QuestionState): QuestionRequest | null => {
    if (!sessionId) return null;
    return s.queues[sessionId]?.[0] ?? null;
  };

/** Imperative guard for event handlers that may run with stale React props. */
export function hasPendingQuestionForSession(sessionId: string): boolean {
  return (useQuestionStore.getState().queues[sessionId]?.length ?? 0) > 0;
}
