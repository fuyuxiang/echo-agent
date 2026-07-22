import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { DndContext, DragEndEvent, DragStartEvent, closestCenter, useDroppable, useDraggable } from "@dnd-kit/core";
import {
  useKanbanStore,
  PRIMARY_COLUMNS,
  ARCHIVED_COLUMNS,
  statusMeta,
  TaskCard,
  canTransition,
} from "../stores/kanban";
import { useWsSubscribe } from "../hooks/use-ws";
import { toast } from "../stores/toast";
import i18n from "../i18n";
import { Plus, X, RotateCcw, Check, Undo2, Play } from "lucide-react";

export function Kanban() {
  const { t } = useTranslation("kanban");
  const { tasks, loading, fetchTasks, transitionTask, createTask, updateLocal, addLocal } = useKanbanStore();
  const [newTitle, setNewTitle] = useState("");
  const [showArchived, setShowArchived] = useState(false);
  const [draggingFrom, setDraggingFrom] = useState<string | null>(null);

  useEffect(() => { fetchTasks(); }, []);

  // 实时同步:后端每次任务状态变更(create/transition/update)都会经 TaskManager
  // 通过 dashboard WS 广播,这里订阅 tasks 频道把变更并入本地——覆盖后台自动执行
  // (dispatcher 写 running、Agent 写 success/failed)与多端操作,不再需要手动刷新。
  // payload 是后端权威的完整任务记录(task.to_dict()),直接合并即可。
  useWsSubscribe(
    ["tasks"],
    (ev) => {
      const task = ev.payload as TaskCard;
      if (!task || !task.id) return;
      if (ev.type === "task_created") addLocal(task);
      else updateLocal(task.id, task); // transitioned / updated
    },
    ["task_created", "task_transitioned", "task_updated"],
  );

  const columns = useMemo(
    () => (showArchived ? [...PRIMARY_COLUMNS, ...ARCHIVED_COLUMNS] : [...PRIMARY_COLUMNS]),
    [showArchived],
  );

  const handleDragStart = (event: DragStartEvent) => {
    const task = tasks.find((t) => t.id === event.active.id);
    setDraggingFrom(task ? task.status : null);
  };

  const handleDragEnd = (event: DragEndEvent) => {
    setDraggingFrom(null);
    const { active, over } = event;
    if (!over) return;
    const targetColumn = over.id as string;
    const taskId = active.id as string;
    const task = tasks.find((t) => t.id === taskId);
    if (!task || task.status === targetColumn) return;
    // 非法流转直接忽略:不发请求、不弹错。合法性以前端镜像判定,后端仍会二次校验。
    if (!canTransition(task.status, targetColumn)) return;
    const previousStatus = task.status;
    updateLocal(taskId, { status: targetColumn }); // 乐观更新
    transitionTask(taskId, targetColumn).catch(() => {
      updateLocal(taskId, { status: previousStatus }); // 失败回滚
    });
  };

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newTitle.trim()) return;
    // 仅在创建成功后清空输入:失败时保留用户已输入的标题,避免网络/后端错误吞掉输入。
    const ok = await createTask(newTitle.trim());
    if (ok) setNewTitle("");
  };

  if (loading) return <div className="p-8 text-center text-gray-500">{t("loading")}</div>;

  return (
    <div className="h-full flex flex-col">
      <form onSubmit={handleCreate} className="flex gap-2 mb-2">
        <input
          value={newTitle}
          onChange={(e) => setNewTitle(e.target.value)}
          placeholder={t("newPlaceholder")}
          className="border rounded px-3 py-1.5 flex-1"
        />
        <button type="submit" className="bg-blue-600 text-white px-3 py-1.5 rounded flex items-center gap-1">
          <Plus size={16} /> {t("create")}
        </button>
      </form>

      <div className="flex items-center justify-between mb-3 text-xs text-gray-500">
        <span>{t("autoClaimHint")}</span>
        <label className="flex items-center gap-1 cursor-pointer select-none">
          <input type="checkbox" checked={showArchived} onChange={(e) => setShowArchived(e.target.checked)} />
          {t("showArchived")}
        </label>
      </div>

      <DndContext collisionDetection={closestCenter} onDragStart={handleDragStart} onDragEnd={handleDragEnd}>
        <div className="flex gap-3 flex-1 overflow-x-auto">
          {columns.map((col) => (
            <KanbanColumn
              key={col}
              status={col}
              tasks={tasks.filter((t) => t.status === col)}
              draggingFrom={draggingFrom}
            />
          ))}
        </div>
      </DndContext>
    </div>
  );
}

function KanbanColumn({
  status,
  tasks,
  draggingFrom,
}: {
  status: string;
  tasks: TaskCard[];
  draggingFrom: string | null;
}) {
  const { setNodeRef, isOver } = useDroppable({ id: status });
  const { t } = useTranslation("kanban");
  const meta = statusMeta(status);

  // 拖拽中:高亮合法落点、灰化非法列,给出“能不能放这里”的即时反馈。
  const isValidTarget = draggingFrom !== null && draggingFrom !== status && canTransition(draggingFrom, status);
  const isInvalidTarget = draggingFrom !== null && draggingFrom !== status && !isValidTarget;

  // 列内按优先级降序(priority 大在前),同级按创建时间。
  const sorted = useMemo(
    () =>
      [...tasks].sort(
        (a, b) => b.priority - a.priority || a.created_at.localeCompare(b.created_at),
      ),
    [tasks],
  );

  let bg = "bg-gray-100";
  if (isOver && isValidTarget) bg = "bg-blue-50 ring-2 ring-blue-300";
  else if (isValidTarget) bg = "bg-blue-50/50";
  else if (isInvalidTarget) bg = "bg-gray-100 opacity-40";

  return (
    <div
      ref={setNodeRef}
      className={`flex-shrink-0 w-64 rounded-lg p-2 flex flex-col transition-colors ${bg}`}
    >
      <div className="font-semibold text-sm mb-2 flex items-center gap-2" title={meta.hint}>
        {meta.label}
        <span className={`text-xs rounded-full px-2 ${meta.chip}`}>{tasks.length}</span>
      </div>
      <div className="flex-1 space-y-2 overflow-y-auto">
        {sorted.length === 0 && <div className="text-xs text-gray-400 py-2 text-center">{t("emptyColumn")}</div>}
        {sorted.map((task) => (
          <KanbanCard key={task.id} task={task} />
        ))}
      </div>
    </div>
  );
}

// 卡片操作:全部走既有 transition 接口(后端已暴露)。transitionTask 内部已在失败时
// 弹 error toast 并 re-throw,这里只负责乐观更新、回滚与成功提示,不重复弹错。
function useCardActions(task: TaskCard) {
  const { transitionTask, retryTask, updateLocal } = useKanbanStore();

  const run = async (to: string, okMsg: string) => {
    const prev = task.status;
    updateLocal(task.id, { status: to }); // 乐观更新
    try {
      await transitionTask(task.id, to);
      toast.success(okMsg);
    } catch {
      updateLocal(task.id, { status: prev }); // 失败回滚(错误提示已由 transitionTask 弹出)
    }
  };

  // 重试走专用 retry 端点(递增 retry_count + 校验 max_retries),不走通用 transition。
  const doRetry = async () => {
    const prev = task.status;
    updateLocal(task.id, { status: "queued" }); // 乐观更新
    try {
      await retryTask(task.id);
      toast.success(i18n.t("kanban:toast.requeued"));
    } catch {
      updateLocal(task.id, { status: prev }); // 失败回滚(错误提示已由 retryTask 弹出)
    }
  };

  return {
    start: () => run("queued", i18n.t("kanban:toast.queued")),
    cancel: () => run("cancelled", i18n.t("kanban:toast.cancelled")),
    retry: doRetry,
    approve: () => run("success", i18n.t("kanban:toast.approved")),
    reject: () => run("queued", i18n.t("kanban:toast.rejected")),
  };
}

function KanbanCard({ task }: { task: TaskCard }) {
  const { t } = useTranslation("kanban");
  const { attributes, listeners, setNodeRef, transform, isDragging } = useDraggable({ id: task.id });
  const actions = useCardActions(task);

  const style = transform
    ? { transform: `translate(${transform.x}px, ${transform.y}px)` }
    : undefined;

  // 卡片上展示的可用操作,依状态而定。
  const canStart = task.status === "pending"; // 收件箱任务显式排队 → 触发 dispatcher 执行
  const canCancel = ["pending", "queued", "running", "blocked", "suspended"].includes(task.status);
  const canRetry = task.status === "failed";
  const inReview = task.status === "review";

  const detail =
    task.status === "failed"
      ? task.error
      : task.status === "blocked"
        ? task.blocked_reason
        : task.status === "review"
          ? task.review_summary
          : "";

  return (
    <div
      ref={setNodeRef}
      style={style}
      className={`group bg-white rounded-md border p-3 shadow-sm ${isDragging ? "opacity-50" : ""}`}
    >
      {/* 拖拽把手区域:标题栏可拖,操作按钮不带 listeners 以免误触发拖拽。 */}
      <div {...listeners} {...attributes} className="cursor-grab active:cursor-grabbing">
        <div className="flex items-start justify-between gap-2">
          <div className="text-sm font-medium flex-1">{task.title}</div>
          <span className="text-[10px] text-gray-400 shrink-0 mt-0.5" title={t("priorityTitle")}>P{task.priority}</span>
        </div>
        {detail && <div className="text-xs text-gray-500 mt-1 line-clamp-2">{detail}</div>}
        {task.assignee && <div className="text-xs text-gray-500 mt-1">@{task.assignee}</div>}
        {task.labels.length > 0 && (
          <div className="flex gap-1 mt-1 flex-wrap">
            {task.labels.map((l) => (
              <span key={l} className="text-xs bg-blue-100 text-blue-700 px-1.5 rounded">{l}</span>
            ))}
          </div>
        )}
        {task.status === "failed" && task.retry_count > 0 && (
          <div className="text-[10px] text-gray-400 mt-1">{t("retried", { count: task.retry_count, max: task.max_retries })}</div>
        )}
      </div>

      {/* 操作条:hover 显现,避免常态干扰。 */}
      <div className="flex gap-1 mt-2 opacity-0 group-hover:opacity-100 transition-opacity">
        {canStart && (
          <button onClick={actions.start} className="p-1 hover:bg-blue-50 rounded text-blue-600" title={t("action.start")}>
            <Play size={14} />
          </button>
        )}
        {inReview && (
          <>
            <button onClick={actions.approve} className="p-1 hover:bg-green-50 rounded text-green-600" title={t("action.approve")}>
              <Check size={14} />
            </button>
            <button onClick={actions.reject} className="p-1 hover:bg-gray-100 rounded text-gray-500" title={t("action.reject")}>
              <Undo2 size={14} />
            </button>
          </>
        )}
        {canRetry && (
          <button onClick={actions.retry} className="p-1 hover:bg-blue-50 rounded text-blue-600" title={t("action.retry")}>
            <RotateCcw size={14} />
          </button>
        )}
        {canCancel && (
          <button onClick={actions.cancel} className="p-1 hover:bg-red-50 rounded text-red-500" title={t("action.cancel")}>
            <X size={14} />
          </button>
        )}
      </div>
    </div>
  );
}

