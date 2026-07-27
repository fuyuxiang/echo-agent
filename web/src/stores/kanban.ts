import { create } from "zustand";
import { apiFetch } from "../lib/api";
import { toast } from "./toast";
import i18n from "../i18n";

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

// 状态元数据:名称 + 配色 + 一句说明。覆盖后端全部 9 个状态(models.py TaskStatus)。
// 界面一律读这里,避免再出现英文状态直出与中英混杂。
export interface StatusMeta {
  label: string;
  hint: string;
  chip: string; // 列标题计数/卡片角标配色
}

// 配色留在代码里(与文案无关);label/hint 运行时按当前语言取。
const STATUS_CHIP: Record<string, string> = {
  pending: "bg-gray-200 text-gray-600",
  queued: "bg-blue-100 text-blue-700",
  running: "bg-indigo-100 text-indigo-700",
  blocked: "bg-orange-100 text-orange-700",
  review: "bg-purple-100 text-purple-700",
  success: "bg-green-100 text-green-700",
  failed: "bg-red-100 text-red-700",
  cancelled: "bg-gray-200 text-gray-500",
  suspended: "bg-yellow-100 text-yellow-700",
};

export function statusMeta(status: string): StatusMeta {
  return {
    label: i18n.t(`kanban:status.${status}.label`, { defaultValue: status }),
    hint: i18n.t(`kanban:status.${status}.hint`, { defaultValue: "" }),
    chip: STATUS_CHIP[status] ?? "bg-gray-200",
  };
}

export function statusLabel(status: string): string {
  return statusMeta(status).label;
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
  /** true 表示已成功拉取过一次;用于区分“首屏加载”与“刷新中”。 */
  loaded: boolean;
  /**
   * 请求飞行期间被本地写过的任务 id。WS 推送、乐观更新、服务端回写都会登记进来,
   * 快照回来时这些 id 保留本地版本,其余仍以服务端为准。
   *
   * mergeSnapshot 只按 id 判断“新增/删除”,对**同一个 id** 无能为力:WS 已把新
   * 状态写进来,而更早发出的快照后到,快照里的旧状态仍会盖回去(表现为“状态跳回”)。
   * 只标记受影响的 id、而不是整体丢弃快照,是因为同一份快照里通常还带着其他任务的
   * 有效更新(以及新任务),丢掉它们就要等下一次刷新。也不用逐项比较 updated_at:
   * 那要额外处理时钟偏移与字段缺失,而“谁刚被本地改过”这件事本地是确知的。
   */
  dirtyIds: Set<string>;
  fetchTasks: () => Promise<void>;
  transitionTask: (id: string, to: string) => Promise<void>;
  retryTask: (id: string) => Promise<void>;
  createTask: (title: string, description?: string) => Promise<boolean>;
  editTask: (id: string, fields: EditableTaskFields) => Promise<boolean>;
  updateLocal: (id: string, changes: Partial<TaskCard>) => void;
  addLocal: (task: TaskCard) => void;
}

/**
 * 可编辑字段,与 PUT /tasks/{id} 的 allowed 白名单对齐(tasks.py update_task)。
 * 状态不在其中:状态只能走 transition/retry,那里有状态机校验。
 */
export type EditableTaskFields = Partial<
  Pick<TaskCard, "title" | "description" | "priority" | "labels" | "assignee">
>;

/**
 * 用服务端快照合并本地列表,而不是整体替换。
 *
 * 请求在飞行期间,WS 可能已经把新任务/新状态写进 store(addLocal/updateLocal)。
 * 直接 set({tasks: data.tasks}) 会把这些更新冲掉——窗口不大但真实存在,表现为
 * “看板刷新后刚变的状态又跳回去了”。
 *
 * 合并规则以“请求发出那一刻的 id 集合”为分界:
 * - 服务端记录一律为准(它是权威),覆盖本地同 id 的乐观状态;
 * - 本地有、快照没有,且请求发出时**就已存在** → 服务端已删除,移除;
 * - 本地有、快照没有,但请求发出后才出现 → WS 刚推来、拍快照时还不存在,保留。
 *
 * 少了最后这条分界,已删除的任务会永远留在看板上;少了它的反面,则会丢掉飞行期间
 * 的新任务。两者都要靠这个时间点区分。
 *
 * 额外一条:`dirtyIds` 里的 id(飞行期间被本地写过)保留本地版本。上面第一条“服务端
 * 一律为准”只在快照不比本地旧时才成立,而一份先发后到的快照对这些 id 恰恰是旧的。
 */
function mergeSnapshot(
  local: TaskCard[],
  snapshot: TaskCard[],
  idsAtRequestStart: Set<string>,
  dirtyIds: Set<string>,
): TaskCard[] {
  const inSnapshot = new Set(snapshot.map((t) => t.id));
  const localById = new Map(local.map((t) => [t.id, t]));
  // 飞行期间本地写过的 id:快照可能已过期,保留本地版本。
  const authoritative = snapshot.map((t) => {
    const mine = dirtyIds.has(t.id) ? localById.get(t.id) : undefined;
    return mine ?? t;
  });
  const arrivedDuringRequest = local.filter(
    (t) => !inSnapshot.has(t.id) && !idsAtRequestStart.has(t.id),
  );
  return [...authoritative, ...arrivedDuringRequest];
}

export const useKanbanStore = create<KanbanState>((set, get) => ({
  tasks: [],
  loading: false,
  loaded: false,
  dirtyIds: new Set<string>(),

  fetchTasks: async () => {
    // 快照发出前先记下当前 id 集合,作为“删除”与“新到”的分界(见 mergeSnapshot),
    // 并清空脏标记:此刻之后的本地写入才可能比这份快照新。
    const idsAtRequestStart = new Set(get().tasks.map((t) => t.id));
    set({ loading: true, dirtyIds: new Set<string>() });
    try {
      const data = await apiFetch<{ tasks: TaskCard[] }>("/tasks?board_id=default");
      set((s) => ({
        tasks: mergeSnapshot(s.tasks, data.tasks ?? [], idsAtRequestStart, s.dirtyIds),
        loaded: true,
      }));
    } catch (e) {
      toast.error(i18n.t("kanban:toast.loadFailed", { error: e instanceof Error ? e.message : String(e) }));
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
        dirtyIds: new Set(s.dirtyIds).add(id),
      }));
    } catch (e) {
      toast.error(i18n.t("kanban:toast.transitionFailed", { error: e instanceof Error ? e.message : String(e) }));
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
        dirtyIds: new Set(s.dirtyIds).add(id),
      }));
    } catch (e) {
      toast.error(i18n.t("kanban:toast.retryFailed", { error: e instanceof Error ? e.message : String(e) }));
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
      set((s) => ({
        tasks: [...s.tasks, data.task],
        dirtyIds: new Set(s.dirtyIds).add(data.task.id),
      }));
      return true;
    } catch (e) {
      toast.error(i18n.t("kanban:toast.createFailed", { error: e instanceof Error ? e.message : String(e) }));
      return false;
    }
  },

  editTask: async (id, fields) => {
    // PUT /tasks/{id}:改标题/描述/优先级/负责人/标签。此前看板完全没有编辑入口——
    // 一个标题打错字的任务只能取消重建,而重建会丢掉 result/重试计数与工作流关联。
    // 与 createTask 同样返回布尔值,失败时调用方保留表单内容而不是清空。
    try {
      const data = await apiFetch<{ task: TaskCard }>(`/tasks/${id}`, {
        method: "PUT",
        body: JSON.stringify(fields),
      });
      set((s) => ({
        tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...data.task } : t)),
        dirtyIds: new Set(s.dirtyIds).add(id),
      }));
      return true;
    } catch (e) {
      toast.error(i18n.t("kanban:toast.updateFailed", { error: e instanceof Error ? e.message : String(e) }));
      return false;
    }
  },

  // WS 推送写入本地状态:登记脏标记,使在飞的 HTTP 快照不覆盖这一条。这条路径带来的
  // 状态比先发后到的快照更新(见 KanbanState.dirtyIds)。
  updateLocal: (id, changes) => {
    set((s) => ({
      tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...changes } : t)),
      dirtyIds: new Set(s.dirtyIds).add(id),
    }));
  },

  addLocal: (task) => {
    set((s) => {
      if (s.tasks.find((t) => t.id === task.id)) return s;
      return { tasks: [...s.tasks, task], dirtyIds: new Set(s.dirtyIds).add(task.id) };
    });
  },
}));
