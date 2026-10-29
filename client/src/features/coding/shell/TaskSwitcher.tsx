import { useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  CircleDot,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  Plus,
  Search,
  Trash2,
} from "lucide-react";

import { describeTaskProgress, isBusyPhase } from "../lib/phase";
import type { TaskSummary } from "../lib/types";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";

interface TaskSwitcherProps {
  tasks: TaskSummary[];
  activeId?: string | null;
  activeName?: string;
  onSelect: (taskId: string) => void;
  onNew: () => void;
  newDisabled?: boolean;
  onRename: (task: TaskSummary, returnFocus?: HTMLElement | null) => void;
  onDelete: (task: TaskSummary, returnFocus?: HTMLElement | null) => void;
}

/**
 * Current task plus a menu of the workspace's other tasks.
 *
 * A repository can hold several tasks at once, so the workbench needs an explicit
 * switcher; without it a new task would silently replace the previous one's
 * state, which is what the old implementation did.
 */
export function TaskSwitcher({
  tasks,
  activeId,
  activeName,
  onSelect,
  onNew,
  newDisabled = false,
  onRename,
  onDelete,
}: TaskSwitcherProps) {
  const [open, setOpen] = useState(false);
  const [actionTaskId, setActionTaskId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const menuId = useId();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const toggleRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const { style: menuStyle, placement } = useAnchoredFloating(toggleRef, menuRef, open, {
    preferredPlacement: "bottom",
    align: "start",
    width: 320,
    estimatedHeight: Math.min(460, 54 + tasks.length * 42),
    maxHeight: 460,
    offset: 4,
  });

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent) => {
      if (!containerRef.current?.contains(event.target as Node)
        && !menuRef.current?.contains(event.target as Node)) {
        setOpen(false);
        setActionTaskId(null);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
        setActionTaskId(null);
        toggleRef.current?.focus();
      }
    };
    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  const active = tasks.find((task) => task.id === activeId);
  const filteredTasks = tasks.filter((task) => task.name.toLowerCase().includes(query.trim().toLowerCase()));

  const moveFocus = (delta: number) => {
    const items = [...(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])];
    if (items.length === 0) return;
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = current < 0
      ? delta > 0 ? 0 : items.length - 1
      : (current + delta + items.length) % items.length;
    items[next]?.focus();
  };

  if (!activeId && tasks.length === 0) {
    return <div className="coding-task-switcher coding-task-switcher--heading">新对话</div>;
  }

  return (
    <div className={`coding-task-switcher${activeId ? "" : " is-empty"}`} ref={containerRef}>
      <button
        ref={toggleRef}
        type="button"
        onClick={() => {
          setOpen((value) => !value);
          setActionTaskId(null);
        }}
        onKeyDown={(event) => {
          if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
          event.preventDefault();
          setOpen(true);
          window.requestAnimationFrame(() => moveFocus(event.key === "ArrowDown" ? 1 : -1));
        }}
        aria-expanded={open}
        aria-haspopup="menu"
        aria-controls={open ? menuId : undefined}
        aria-label="切换开发任务"
      >
        <span>{active?.name ?? (activeId ? activeName || "当前任务" : "任务记录")}</span>
        <ChevronDown size={12} />
      </button>

      {open && createPortal(
        <div
          ref={menuRef}
          id={menuId}
          className="coding-task-switcher__menu"
          role="menu"
          aria-label="开发任务"
          style={menuStyle}
          data-placement={placement ?? undefined}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              moveFocus(1);
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              moveFocus(-1);
            } else if (event.key === "Home") {
              event.preventDefault();
              const first = menuRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]:not(:disabled)');
              first?.focus();
            } else if (event.key === "End") {
              event.preventDefault();
              const items = menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)');
              items?.[items.length - 1]?.focus();
            }
          }}
        >
          {tasks.length > 5 && (
            <label className="coding-task-switcher__search">
              <Search size={12} />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="搜索开发任务"
                aria-label="搜索开发任务"
              />
            </label>
          )}
          {tasks.length === 0 && <div className="coding-row">当前仓库还没有开发任务</div>}
          {tasks.length > 0 && filteredTasks.length === 0 && <div className="coding-row">没有匹配的任务</div>}
          {filteredTasks.map((task) => {
            const phase = describeTaskProgress(task.phase);
            const actionsOpen = actionTaskId === task.id;
            return (
              <div className="coding-task-switcher__entry" key={task.id} role="none">
                <div className={`coding-task-switcher__row${task.id === activeId ? " is-active" : ""}`}>
                  <button
                    type="button"
                    role="menuitem"
                    className="coding-task-switcher__select"
                    aria-label={`打开任务：${task.name}，${phase.label}`}
                    aria-current={task.id === activeId ? "true" : undefined}
                    onClick={() => {
                      setOpen(false);
                      setActionTaskId(null);
                      onSelect(task.id);
                    }}
                  >
                    {phase.active ? (
                      <LoaderCircle size={12} className="is-spinning" />
                    ) : task.phase === "delivered" ? (
                      <CheckCircle2 size={12} />
                    ) : task.phase === "blocked" ? (
                      <AlertTriangle size={12} />
                    ) : (
                      <CircleDot size={12} />
                    )}
                    <span>{task.name}</span>
                    <small title={new Date(task.updatedAt).toLocaleString()}>{phase.label}</small>
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    className="coding-task-switcher__more"
                    aria-label={`管理任务：${task.name}`}
                    aria-haspopup="menu"
                    aria-expanded={actionsOpen}
                    onClick={() => setActionTaskId((current) => current === task.id ? null : task.id)}
                  >
                    <MoreHorizontal size={13} />
                  </button>
                </div>
                {actionsOpen && (
                  <div className="coding-task-switcher__actions" role="menu" aria-label={`${task.name} 任务操作`}>
                    <button
                      type="button"
                      role="menuitem"
                      onClick={() => {
                        setOpen(false);
                        setActionTaskId(null);
                        onRename(task, toggleRef.current);
                      }}
                    >
                      <Pencil size={12} />
                      <span>重命名</span>
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="is-danger"
                      disabled={isBusyPhase(task.phase)}
                      title={isBusyPhase(task.phase) ? "请先停止任务再删除" : "删除开发任务"}
                      onClick={() => {
                        setOpen(false);
                        setActionTaskId(null);
                        onDelete(task, toggleRef.current);
                      }}
                    >
                      <Trash2 size={12} />
                      <span>{isBusyPhase(task.phase) ? "执行中不可删除" : "删除任务"}</span>
                    </button>
                  </div>
                )}
              </div>
            );
          })}
          {activeId && <>
            <div className="coding-task-switcher__sep" />
            <button
              type="button"
              role="menuitem"
              disabled={newDisabled}
              title={newDisabled ? "正在准备隔离工作树" : "开始新任务"}
              onClick={() => {
                setOpen(false);
                onNew();
              }}
            >
              <Plus size={12} />
              <span>{newDisabled ? "正在准备…" : "开始新任务"}</span>
            </button>
          </>}
        </div>,
        document.body,
      )}
    </div>
  );
}
