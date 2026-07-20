import { create } from "zustand";
import { apiFetch } from "../lib/api";
import { toast } from "./toast";

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
  createTask: (title: string, description?: string) => Promise<boolean>;
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
    } catch (e) {
      toast.error(`任务加载失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      set({ loading: false });
    }
  },

  transitionTask: async (id, to) => {
    // 不吞错:交给调用方(拖拽处)在 catch 里回滚。这里补一条 toast 说明失败原因,
    // 否则卡片“弹回原列”而用户不知为何(常见于后端状态机拒绝的非法流转)。
    try {
      await apiFetch(`/tasks/${id}/transition`, {
        method: "POST",
        body: JSON.stringify({ to }),
      });
      set((s) => ({
        tasks: s.tasks.map((t) => (t.id === id ? { ...t, status: to } : t)),
      }));
    } catch (e) {
      toast.error(`流转失败：${e instanceof Error ? e.message : String(e)}`);
      throw e;
    }
  },

  createTask: async (title, description = "") => {
    // 返回成功布尔值:创建失败(网络/后端)时调用方据此保留用户输入,不清空标题框。
    try {
      const data = await apiFetch<{ task: TaskCard }>("/tasks", {
        method: "POST",
        body: JSON.stringify({ title, description, source: "human" }),
      });
      set((s) => ({ tasks: [...s.tasks, data.task] }));
      return true;
    } catch (e) {
      toast.error(`创建失败：${e instanceof Error ? e.message : String(e)}`);
      return false;
    }
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
