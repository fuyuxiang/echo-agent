import { create } from "zustand";
import { apiFetch } from "../lib/api";

export interface TaskCard {
  id: string;
  title: string;
  description: string;
  status: string;
  priority: number;
  labels: string[];
  assignee: string;
  source: string;
  session_id: string;
  blocked_reason: string;
  review_summary: string;
  created_at: string;
  updated_at: string;
}

export const COLUMNS = [
  { id: "pending", label: "Inbox" },
  { id: "queued", label: "Queued" },
  { id: "running", label: "Running" },
  { id: "blocked", label: "Blocked" },
  { id: "review", label: "Review" },
  { id: "success", label: "Done" },
] as const;

interface KanbanState {
  tasks: TaskCard[];
  loading: boolean;
  fetchTasks: () => Promise<void>;
  transitionTask: (id: string, to: string) => Promise<void>;
  createTask: (title: string, description?: string) => Promise<void>;
  updateLocal: (id: string, changes: Partial<TaskCard>) => void;
  addLocal: (task: TaskCard) => void;
}

export const useKanbanStore = create<KanbanState>((set) => ({
  tasks: [],
  loading: false,

  fetchTasks: async () => {
    set({ loading: true });
    try {
      const data = await apiFetch<{ tasks: TaskCard[] }>("/tasks?board_id=default");
      set({ tasks: data.tasks });
    } finally {
      set({ loading: false });
    }
  },

  transitionTask: async (id, to) => {
    await apiFetch(`/tasks/${id}/transition`, {
      method: "POST",
      body: JSON.stringify({ to }),
    });
    set((s) => ({
      tasks: s.tasks.map((t) => (t.id === id ? { ...t, status: to } : t)),
    }));
  },

  createTask: async (title, description = "") => {
    const data = await apiFetch<{ task: TaskCard }>("/tasks", {
      method: "POST",
      body: JSON.stringify({ title, description, source: "human" }),
    });
    set((s) => ({ tasks: [...s.tasks, data.task] }));
  },

  updateLocal: (id, changes) => {
    set((s) => ({
      tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...changes } : t)),
    }));
  },

  addLocal: (task) => {
    set((s) => {
      if (s.tasks.find((t) => t.id === task.id)) return s;
      return { tasks: [...s.tasks, task] };
    });
  },
}));
