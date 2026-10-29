import { create } from "zustand";

import { codingApi } from "../lib/tauri-api";
import type {
  ChangeSet,
  CodingTask,
  ExecutionLedgerEvent,
  OrchestratorState,
  Problem,
  TaskSummary,
  VerificationRecord,
} from "../lib/types";

/**
 * Read-only mirror of the backend's task state.
 *
 * The Rust side owns every value here; this store only caches what the UI last
 * heard so React can render it. Nothing in this file may decide a phase or a
 * verdict — that authority stays in the orchestrator.
 */
interface TaskState {
  root: string;
  summaries: TaskSummary[];
  task: CodingTask | null;
  changeSet: ChangeSet | null;
  verifications: VerificationRecord[];
  problems: Problem[];
  ledger: ExecutionLedgerEvent[];
  orchestrator: OrchestratorState | null;
  loading: boolean;
  error: string | null;

  setRoot: (root: string) => void;
  refreshSummaries: () => Promise<void>;
  selectTask: (taskId: string) => Promise<void>;
  createTask: (
    name: string,
    requirement: string,
    root?: string,
  ) => Promise<CodingTask | null>;
  renameTask: (taskId: string, name: string) => Promise<void>;
  deleteTask: (taskId: string) => Promise<void>;
  refreshTaskState: () => Promise<void>;
  applyPhase: (taskId: string, phase: CodingTask["phase"]) => void;
  applyVerification: (record: VerificationRecord) => void;
  reset: (preserveSummaries?: boolean) => void;
}

function message(error: unknown): string {
  return String(error).replace(/^Error:\s*/, "");
}

let selectionGeneration = 0;
let stateGeneration = 0;
let workspaceGeneration = 0;
let summariesGeneration = 0;
let selectionTarget: string | null = null;
let restoreSelectionPending = false;
const selectionKey = (root: string) => `echo-coding-last-task-v1:${root}`;
function rememberSelection(root: string, id: string) {
  try {
    localStorage.setItem(selectionKey(root), id);
  } catch {
    /* task data remains native-owned */
  }
}

export const useTaskStore = create<TaskState>((set, get) => ({
  root: "",
  summaries: [],
  task: null,
  changeSet: null,
  verifications: [],
  problems: [],
  ledger: [],
  orchestrator: null,
  loading: false,
  error: null,

  setRoot: (root) => {
    if (get().root === root) return;
    selectionGeneration += 1;
    stateGeneration += 1;
    workspaceGeneration += 1;
    summariesGeneration += 1;
    selectionTarget = null;
    restoreSelectionPending = true;
    set({
      root,
      summaries: [],
      task: null,
      changeSet: null,
      verifications: [],
      problems: [],
      ledger: [],
      orchestrator: null,
      loading: false,
      error: null,
    });
  },

  refreshSummaries: async () => {
    const { root } = get();
    if (!root) return;
    const workspace = workspaceGeneration;
    const generation = ++summariesGeneration;
    const isCurrent = () =>
      get().root === root &&
      workspace === workspaceGeneration &&
      generation === summariesGeneration;
    try {
      const summaries = await codingApi.listTasks(root);
      if (isCurrent()) {
        set({ summaries, error: null });
        if (restoreSelectionPending) {
          restoreSelectionPending = false;
          let saved: string | null = null;
          try {
            saved = localStorage.getItem(selectionKey(root));
          } catch {
            /* optional preference */
          }
          if (
            !get().task &&
            saved &&
            summaries.some((item) => item.id === saved)
          )
            await get().selectTask(saved);
        }
      }
    } catch (error) {
      if (isCurrent()) set({ error: message(error) });
    }
  },

  selectTask: async (taskId) => {
    const { root } = get();
    if (!root) return;
    // A user selection owns navigation even while its detail is loading.
    restoreSelectionPending = false;
    const generation = ++selectionGeneration;
    selectionTarget = taskId;
    stateGeneration += 1;
    set({ loading: true });
    try {
      const task = await codingApi.getTask(root, taskId);
      if (generation !== selectionGeneration || get().root !== root) return;
      if (task) rememberSelection(root, task.id);
      set({
        task,
        changeSet: null,
        verifications: [],
        problems: [],
        ledger: [],
        orchestrator: null,
        error: null,
      });
      await get().refreshTaskState();
    } catch (error) {
      if (generation === selectionGeneration && get().root === root) {
        set({ error: message(error) });
      }
    } finally {
      if (generation === selectionGeneration && get().root === root)
        set({ loading: false });
    }
  },

  createTask: async (name, requirement, explicitRoot) => {
    const root = explicitRoot ?? get().root;
    if (!root) return null;
    const workspace = workspaceGeneration;
    const ownsWorkspace = get().root === root;
    const generation = ownsWorkspace
      ? ++selectionGeneration
      : selectionGeneration;
    if (ownsWorkspace) {
      selectionTarget = null;
      stateGeneration += 1;
    }
    const isCurrent = () =>
      get().root === root &&
      workspace === workspaceGeneration &&
      generation === selectionGeneration;
    try {
      const task = await codingApi.createTask(root, name, requirement);
      if (!isCurrent()) return explicitRoot ? task : null;
      selectionTarget = task.id;
      restoreSelectionPending = false;
      rememberSelection(root, task.id);
      set({
        task,
        changeSet: null,
        verifications: [],
        problems: [],
        ledger: [],
        orchestrator: null,
        loading: false,
        error: null,
      });
      await get().refreshSummaries();
      return task;
    } catch (error) {
      if (isCurrent()) set({ loading: false, error: message(error) });
      return null;
    }
  },

  renameTask: async (taskId, name) => {
    const { root } = get();
    if (!root) return;
    const workspace = workspaceGeneration;
    const isCurrent = () =>
      get().root === root && workspace === workspaceGeneration;
    try {
      const task = await codingApi.renameTask(root, taskId, name);
      if (!isCurrent()) return;
      set((state) => ({ task: state.task?.id === taskId ? task : state.task }));
      await get().refreshSummaries();
    } catch (error) {
      if (isCurrent()) set({ error: message(error) });
      throw error;
    }
  },

  deleteTask: async (taskId) => {
    const { root } = get();
    if (!root) return;
    const workspace = workspaceGeneration;
    const isCurrent = () =>
      get().root === root && workspace === workspaceGeneration;
    try {
      await codingApi.deleteTask(root, taskId);
      if (!isCurrent()) return;
      if (selectionTarget === taskId || get().task?.id === taskId) {
        selectionGeneration += 1;
        stateGeneration += 1;
        selectionTarget = null;
        set({ loading: false });
      }
      set((state) =>
        state.task?.id === taskId
          ? {
              ...state,
              task: null,
              changeSet: null,
              verifications: [],
              problems: [],
              ledger: [],
              orchestrator: null,
            }
          : state,
      );
      await get().refreshSummaries();
    } catch (error) {
      if (isCurrent()) set({ error: message(error) });
      throw error;
    }
  },

  refreshTaskState: async () => {
    const { root, task } = get();
    if (!root || !task) return;
    const generation = ++stateGeneration;
    const taskId = task.id;
    try {
      const [changeSet, verifications, problems, ledger, orchestrator] =
        await Promise.all([
          codingApi.getChangeSet(root, taskId),
          codingApi.listVerifications(root, taskId),
          codingApi.listProblems(root, taskId),
          codingApi.executionLedger(root, taskId),
          codingApi.orchestratorState(root, taskId),
        ]);
      if (
        generation !== stateGeneration ||
        get().root !== root ||
        get().task?.id !== taskId
      )
        return;
      set({
        changeSet,
        verifications,
        problems,
        ledger: ledger ?? [],
        orchestrator,
        // The orchestrator's copy is authoritative for phase and criteria.
        task: orchestrator.task,
        error: null,
      });
    } catch (error) {
      if (
        generation === stateGeneration &&
        get().root === root &&
        get().task?.id === taskId
      )
        set({ error: message(error) });
    }
  },

  applyPhase: (taskId, phase) =>
    set((state) => ({
      task: state.task?.id === taskId ? { ...state.task, phase } : state.task,
      summaries: state.summaries.map((entry) =>
        entry.id === taskId ? { ...entry, phase } : entry,
      ),
    })),

  applyVerification: (record) =>
    set((state) => {
      if (state.task?.id !== record.taskId) return state;
      return { verifications: [...state.verifications, record] };
    }),

  reset: (preserveSummaries = false) => {
    restoreSelectionPending = false;
    rememberSelection(get().root, "");
    selectionGeneration += 1;
    stateGeneration += 1;
    workspaceGeneration += 1;
    summariesGeneration += 1;
    selectionTarget = null;
    set({
      summaries: preserveSummaries ? get().summaries : [],
      task: null,
      changeSet: null,
      verifications: [],
      problems: [],
      ledger: [],
      orchestrator: null,
      loading: false,
      error: null,
    });
  },
}));
