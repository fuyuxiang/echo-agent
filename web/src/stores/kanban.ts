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
  result: string;
  error: string;
  retry_count: number;
  max_retries: number;
  created_at: string;
  updated_at: string;
}

// 状态元数据:中文名 + 配色 + 一句说明。覆盖后端全部 9 个状态(models.py TaskStatus)。
// 界面一律读这里,避免再出现英文状态直出与中英混杂。
export interface StatusMeta {
  label: string;
  hint: string;
  chip: string; // 列标题计数/卡片角标配色
}

export const TASK_STATUS_META: Record<string, StatusMeta> = {
  pending: { label: "收件箱", hint: "新建但尚未排队的任务", chip: "bg-gray-200 text-gray-600" },
  queued: { label: "排队中", hint: "等待 Agent 认领执行", chip: "bg-blue-100 text-blue-700" },
  running: { label: "执行中", hint: "Agent 正在处理", chip: "bg-indigo-100 text-indigo-700" },
  blocked: { label: "阻塞", hint: "缺少条件,无法继续", chip: "bg-orange-100 text-orange-700" },
  review: { label: "待审", hint: "已完成,等待人工审核", chip: "bg-purple-100 text-purple-700" },
  success: { label: "已完成", hint: "已通过并结束", chip: "bg-green-100 text-green-700" },
  failed: { label: "失败", hint: "执行出错,可重试", chip: "bg-red-100 text-red-700" },
  cancelled: { label: "已取消", hint: "被人工取消", chip: "bg-gray-200 text-gray-500" },
  suspended: { label: "挂起", hint: "暂时搁置", chip: "bg-yellow-100 text-yellow-700" },
};

export function statusLabel(status: string): string {
  return TASK_STATUS_META[status]?.label ?? status;
}

// 常驻列:失败也常驻(需要人工重试),不再让失败任务从看板消失。
export const PRIMARY_COLUMNS = [
  "pending",
  "queued",
  "running",
  "blocked",
  "review",
  "success",
  "failed",
] as const;

// 归档列:默认隐藏,顶部开关打开后追加。用户主动取消/挂起的任务不必长期占位。
export const ARCHIVED_COLUMNS = ["cancelled", "suspended"] as const;

// 前端镜像后端状态机(models.py VALID_TASK_TRANSITIONS),用于拖拽时判定合法目标列:
// 高亮可落列、忽略非法拖拽,避免"拖了就报错"。后端仍是唯一权威校验。
// 注意:进入 running 是 dispatcher 的专属职责(它同时投递执行事件),手动/拖拽转入
// running 只会产生没有执行者的"幽灵 running",后端已在 transition 接口拒绝,这里也
// 一并移除 *→running 落点,避免拖拽给出会被后端打回的假可落提示。
export const VALID_TRANSITIONS: Record<string, string[]> = {
  pending: ["queued", "cancelled"],
  queued: ["cancelled"],
  running: ["review", "blocked", "failed", "suspended", "cancelled"],
  blocked: ["queued", "cancelled"],
  review: ["success", "queued"],
  suspended: ["queued", "cancelled"],
  failed: ["queued"],
  success: [],
  cancelled: [],
};

export function canTransition(from: string, to: string): boolean {
  return (VALID_TRANSITIONS[from] ?? []).includes(to);
}

interface KanbanState {
  tasks: TaskCard[];
  loading: boolean;
  fetchTasks: () => Promise<void>;
  transitionTask: (id: string, to: string) => Promise<void>;
  retryTask: (id: string) => Promise<void>;
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
    // 不吞错:交给调用方(拖拽/按钮处)在 catch 里回滚。这里补一条 toast 说明失败原因,
    // 否则卡片“弹回原列”而用户不知为何(常见于后端状态机拒绝的非法流转)。
    try {
      const data = await apiFetch<{ task: TaskCard }>(`/tasks/${id}/transition`, {
        method: "POST",
        body: JSON.stringify({ to }),
      });
      // 用后端回传的权威记录覆盖本地,拿到 completed_at/started_at 等派生字段。
      set((s) => ({
        tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...data.task } : t)),
      }));
    } catch (e) {
      toast.error(`流转失败：${e instanceof Error ? e.message : String(e)}`);
      throw e;
    }
  },

  retryTask: async (id) => {
    // 重试走专用端点(manager.retry):递增 retry_count 且校验 max_retries。
    // 不能走通用 transition(failed→queued),那会绕过计数与上限,任务可被无限重试。
    // 不吞错:交给调用方在 catch 里回滚乐观更新,这里补一条 toast 说明失败原因
    // (常见于已达 max_retries 被后端拒绝)。
    try {
      const data = await apiFetch<{ task: TaskCard }>(`/tasks/${id}/retry`, {
        method: "POST",
      });
      set((s) => ({
        tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...data.task } : t)),
      }));
    } catch (e) {
      toast.error(`重试失败：${e instanceof Error ? e.message : String(e)}`);
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
