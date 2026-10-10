import { useState, useCallback, useRef, useEffect, useMemo } from "react";
import { useSessionsStore, selectHasFilter } from "@/stores/sessions-store";
import { useProjectsStore } from "@/stores/projects-store";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { IS_MACOS } from "@/lib/platform";
import { isCapabilityView } from "@/lib/capability-navigation";
import {
  agentRenameSession,
  agentDeleteSession,
  agentSetSessionPinned,
  agentSetSessionArchived,
} from "@/lib/agent-client";
import type { SessionSummary, SessionStatus } from "@/lib/types";
import { isWaitingForUser } from "@/lib/turn-status";
import {
  EchoNewTaskIcon,
  EchoProjectNavIcon,
  EchoExpertNavIcon,
  EchoAutomationNavIcon,
  EchoMoreNavIcon,
  SearchIcon,
  FilterIcon,
  SidebarToggleIcon,
  BellIcon,
  UserIcon,
  SettingsIcon,
  ChevronDownIcon,
  PinFilledIcon,
  MoreDotsIcon,
  AddIcon,
  BuildingIcon,
  MoreMenuImaKnowledgeIcon,
  Code2Icon,
} from "@/foundation/components/Icon/icons";
import { SessionContextMenu } from "./SessionContextMenu";
import { weixinHandoff } from "@/lib/weixin-client";
import { MoveToProjectDialog } from "./MoveToProjectDialog";
import { useAppDialog } from "./AppDialog";
const logoMarkUrl = "/app-icon.png";

const NAV = [
  { label: "项目", icon: EchoProjectNavIcon },
  { label: "组织", icon: BuildingIcon },
  { label: "专家·技能·连接器", display: "扩展", icon: EchoExpertNavIcon },
  { label: "代码开发", icon: Code2Icon },
];

/** Compact, locale-friendly relative time for the sidebar row tail. */
function relativeTime(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const sec = Math.floor((Date.now() - t) / 1000);
  if (sec < 60) return "刚刚";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}小时前`;
  const day = Math.floor(hr / 24);
  if (day === 1) return "昨天";
  if (day < 7) return `${day}天前`;
  const d = new Date(t);
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}

const ACTIVE_STATUS_META: Partial<Record<SessionStatus, { label: string; className: string }>> = {
  working: { label: "运行中", className: "working" },
  planning: { label: "规划中", className: "planning" },
  pending: { label: "未开始", className: "pending" },
  awaiting_permission: { label: "授权待确认", className: "awaiting" },
  awaiting_answer: { label: "等待回答", className: "awaiting" },
  awaiting_approval: { label: "方案待批准", className: "awaiting" },
  pausing: { label: "正在暂停", className: "pausing" },
  paused: { label: "已暂停", className: "paused" },
  stopping: { label: "正在停止", className: "stopping" },
  stopped: { label: "已停止", className: "stopped" },
  failed: { label: "失败", className: "failed" },
};

/** Completed is the quiet default; only actionable/live states consume row width. */
function SessionStatusBadge({ status }: { status?: SessionStatus }) {
  const meta = status ? ACTIVE_STATUS_META[status] : undefined;
  if (!meta) return null;
  return (
    <span className={`sidebar__conv-status sidebar__conv-status--${meta.className}`}>
      <span className="sidebar__conv-status-dot" aria-hidden="true" />
      <span className="sidebar__conv-status-label">{meta.label}</span>
    </span>
  );
}

/** Pinned entries first; within a pin tier, most-recently-active first
 *  (by `updatedAt`) so a session you just chatted in rises to the top and its
 *  relative-time tail stays honest. Insertion order breaks remaining ties. */
function sortPinnedFirst<
  T extends { pinned?: boolean; updatedAt?: string },
>(list: T[]): T[] {
  return [...list].sort((a, b) => {
    const pin = Number(!!b.pinned) - Number(!!a.pinned);
    if (pin !== 0) return pin;
    const at = a.updatedAt ? new Date(a.updatedAt).getTime() : 0;
    const bt = b.updatedAt ? new Date(b.updatedAt).getTime() : 0;
    return bt - at;
  });
}

/** Small project icon for sidebar nodes (three connected circles). */
function ProjectNodeIcon() {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" aria-hidden="true" style={{ flexShrink: 0 }}>
      <circle cx="6" cy="7" r="2.2" stroke="currentColor" strokeWidth="1.6" />
      <circle cx="18" cy="7" r="2.2" stroke="currentColor" strokeWidth="1.6" />
      <circle cx="12" cy="17.5" r="2.2" stroke="currentColor" strokeWidth="1.6" />
      <path d="M7.7 8.4 10.5 15.6M16.3 8.4 13.5 15.6M8 7h8" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

// ---------- Task filter (对齐 EchoAgent TaskFilterMenu) ----------

const DAY_MS = 24 * 60 * 60 * 1000;

const STATUS_OPTIONS: { value: SessionStatus | null; label: string }[] = [
  { value: null,        label: "全部状态" },
  { value: "working",   label: "进行中" },
  { value: "completed", label: "本轮结束" },
  { value: "failed",    label: "失败" },
  { value: "paused",    label: "已暂停" },
  { value: "stopped",   label: "已停止" },
  { value: "awaiting_permission", label: "等待确认" },
  { value: "pending",   label: "未开始" },
  { value: "planning",  label: "规划中" },
];

const DATE_OPTIONS: { value: string | null; label: string }[] = [
  { value: null,           label: "全部时间" },
  { value: "today",        label: "今天" },
  { value: "last7days",    label: "最近 7 天" },
  { value: "last30days",   label: "最近 30 天" },
];

/** Green checkmark shown on the selected filter option. */
function CheckIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path fill="#00C29A" transform="translate(2.676 3.976)"
        d="M11.3137 0.9428L4.2426 8.0139L0 3.7712L0.9428 2.8284L4.2426 6.1283L10.3709 0L11.3137 0.9428Z" />
    </svg>
  );
}

/** Date preset → start-of-range timestamp (ms). null = no date filter. */
function getDateStart(date: string | null): number | null {
  if (!date) return null;
  if (date === "today") { const s = new Date(); s.setHours(0, 0, 0, 0); return s.getTime(); }
  if (date === "last7days") return Date.now() - 7 * DAY_MS;
  if (date === "last30days") return Date.now() - 30 * DAY_MS;
  return null;
}

/** "working" family: planning/running sessions also match 进行中. */
function statusMatches(sessionStatus: SessionStatus | undefined, filter: SessionStatus): boolean {
  // Legacy rows without persisted lifecycle evidence remain visible under
  // "全部状态", but must not be presented as proven completion.
  if (!sessionStatus) return false;
  const s = sessionStatus;
  if (filter === "working") return s === "working" || s === "planning";
  if (filter === "paused") return s === "pausing" || s === "paused";
  if (filter === "stopped") return s === "stopping" || s === "stopped";
  if (filter === "awaiting_permission") return isWaitingForUser(s);
  return s === filter;
}

/** Apply status + date filters to a session list. */
function filterSessions(
  sessions: SessionSummary[],
  status: SessionStatus | null,
  date: string | null,
): SessionSummary[] {
  const dateStart = getDateStart(date);
  return sessions.filter((s) => {
    // Archived content deliberately leaves the work sidebar. Its canonical
    // recovery surface is Settings > Archived, not another sidebar mode.
    if (s.archived) return false;
    if (status && !statusMatches(s.status, status)) return false;
    if (dateStart !== null) {
      const t = s.updatedAt ? new Date(s.updatedAt).getTime() : 0;
      if (t < dateStart) return false;
    }
    return true;
  });
}

/** Dropdown menu with status + date filter sections and a reset button. */
function TaskFilterMenu({
  filterStatus,
  filterDate,
  hasFilter,
  onSelectStatus,
  onSelectDate,
  onClear,
}: {
  filterStatus: SessionStatus | null;
  filterDate: string | null;
  hasFilter: boolean;
  onSelectStatus: (s: SessionStatus | null) => void;
  onSelectDate: (d: string | null) => void;
  onClear: () => void;
}) {
  return (
    <div className="task-filter-menu">
      {/* 筛选状态 */}
      <div className="task-filter-menu__section">
        <div className="task-filter-menu__section-title">筛选状态</div>
        <div className="task-filter-menu__options">
          {STATUS_OPTIONS.map((opt) => {
            const selected = opt.value === null ? filterStatus === null : filterStatus === opt.value;
            return (
              <button
                key={opt.value ?? "__all_status"}
                type="button"
                role="menuitemradio"
                aria-checked={selected}
                className={"task-filter-menu__option" + (selected ? " task-filter-menu__option--selected" : "")}
                onClick={() => onSelectStatus(opt.value)}
              >
                <span className="task-filter-menu__option-label">{opt.label}</span>
                {selected && <span className="task-filter-menu__option-check"><CheckIcon /></span>}
              </button>
            );
          })}
        </div>
      </div>
      {/* 筛选时间 */}
      <div className="task-filter-menu__section">
        <div className="task-filter-menu__section-title">筛选时间</div>
        <div className="task-filter-menu__options">
          {DATE_OPTIONS.map((opt) => {
            const selected = opt.value === null ? filterDate === null : filterDate === opt.value;
            return (
              <button
                key={opt.value ?? "__all_date"}
                type="button"
                role="menuitemradio"
                aria-checked={selected}
                className={"task-filter-menu__option" + (selected ? " task-filter-menu__option--selected" : "")}
                onClick={() => onSelectDate(opt.value)}
              >
                <span className="task-filter-menu__option-label">{opt.label}</span>
                {selected && <span className="task-filter-menu__option-check"><CheckIcon /></span>}
              </button>
            );
          })}
        </div>
      </div>
      <div className="task-filter-menu__divider" />
      {/* 重置 */}
      <button
        type="button"
        className={"task-filter-menu__reset" + (!hasFilter ? " task-filter-menu__reset--disabled" : "")}
        onClick={() => { if (hasFilter) onClear(); }}
        disabled={!hasFilter}
      >
        <span className="task-filter-menu__reset-label">重置筛选条件</span>
      </button>
    </div>
  );
}

const MENU_ITEM_SELECTOR =
  '[role="menuitem"]:not(:disabled), [role="menuitemradio"]:not(:disabled)';

function focusMenuEdge(menu: HTMLElement | null, edge: "first" | "last"): void {
  if (!menu) return;
  const items = Array.from(menu.querySelectorAll<HTMLElement>(MENU_ITEM_SELECTOR));
  items[edge === "first" ? 0 : items.length - 1]?.focus();
}

/** WAI-ARIA-style navigation shared by the sidebar's vertical menus. */
function handleMenuKeyDown(
  event: React.KeyboardEvent<HTMLElement>,
  onEscape: () => void,
): void {
  if (event.altKey || event.ctrlKey || event.metaKey) return;
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    onEscape();
    return;
  }
  if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;

  const items = Array.from(
    event.currentTarget.querySelectorAll<HTMLElement>(MENU_ITEM_SELECTOR),
  );
  if (items.length === 0) return;
  event.preventDefault();
  event.stopPropagation();
  const current = items.indexOf(document.activeElement as HTMLElement);
  if (event.key === "Home") {
    items[0].focus();
  } else if (event.key === "End") {
    items[items.length - 1].focus();
  } else if (event.key === "ArrowDown") {
    items[current < 0 ? 0 : (current + 1) % items.length].focus();
  } else {
    items[current < 0 ? items.length - 1 : (current - 1 + items.length) % items.length].focus();
  }
}

/**
 * "更多" 侧栏按钮的弹出菜单 — 对齐 EchoAgent：
 * - 悬停打开，向右浮出；点击和键盘也可打开。
 * - 离开时延迟关闭，允许鼠标跨过按钮和菜单之间的间隙。
 * - 展示知识库和定时任务；个人记忆由设置管理。
 */
function MoreDropdown({
  onNavigate,
  activeNav,
}: {
  onNavigate: (label: string) => void;
  activeNav: string;
}) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const pendingKeyboardFocus = useRef<"first" | "last" | null>(null);
  const closeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearCloseTimer = useCallback(() => {
    if (closeTimer.current !== null) {
      clearTimeout(closeTimer.current);
      closeTimer.current = null;
    }
  }, []);

  const closeMenu = useCallback(() => {
    clearCloseTimer();
    pendingKeyboardFocus.current = null;
    setOpen(false);
  }, [clearCloseTimer]);

  const openMenu = useCallback(() => {
    clearCloseTimer();
    setOpen(true);
  }, [clearCloseTimer]);

  const scheduleClose = useCallback(() => {
    clearCloseTimer();
    closeTimer.current = setTimeout(() => {
      closeTimer.current = null;
      // Pointer movement must not dismiss a menu being used from the keyboard.
      if (!menuRef.current?.contains(document.activeElement)) closeMenu();
    }, 120);
  }, [clearCloseTimer, closeMenu]);

  useEffect(() => () => clearCloseTimer(), [clearCloseTimer]);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        closeMenu();
      }
    };
    const handleEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        closeMenu();
        triggerRef.current?.focus();
      }
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleEsc);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleEsc);
    };
  }, [open, closeMenu]);

  useEffect(() => {
    if (!open || !pendingKeyboardFocus.current) return;
    focusMenuEdge(menuRef.current, pendingKeyboardFocus.current);
    pendingKeyboardFocus.current = null;
  }, [open]);

  const openFromKeyboard = (edge: "first" | "last") => {
    clearCloseTimer();
    if (open) {
      focusMenuEdge(menuRef.current, edge);
      return;
    }
    pendingKeyboardFocus.current = edge;
    setOpen(true);
  };

  const ITEMS: {
    id: string;
    label: string;
    icon: React.ReactNode;
    action: () => void;
  }[] = [
    {
      id: "knowledge_base",
      label: "知识库",
      icon: <MoreMenuImaKnowledgeIcon size="md" />,
      action: () => {
        closeMenu();
        onNavigate("知识库");
      },
    },
    {
      id: "automation",
      label: "定时任务",
      icon: <EchoAutomationNavIcon size="md" />,
      action: () => {
        closeMenu();
        onNavigate("自动化");
      },
    },
  ];
  const activeMoreLabel = activeNav === "自动化" ? "定时任务" : activeNav;
  const isActive =
    activeNav === "更多" ||
    ITEMS.some((item) => item.label === activeMoreLabel);

  return (
    <div
      className={"sidebar__more-wrap" + (open ? " sidebar__more-wrap--open" : "")}
      ref={containerRef}
      onMouseEnter={openMenu}
      onMouseLeave={scheduleClose}
      onFocusCapture={clearCloseTimer}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) closeMenu();
      }}
    >
      <button
        ref={triggerRef}
        type="button"
        className={
          "sidebar__nav-item" +
          (isActive || open ? " sidebar__nav-item--active" : "")
        }
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? "sidebar-more-menu" : undefined}
        onClick={openMenu}
        onKeyDown={(event) => {
          if (["Enter", " ", "ArrowDown", "ArrowUp"].includes(event.key)) {
            event.preventDefault();
            openFromKeyboard(event.key === "ArrowUp" ? "last" : "first");
          } else if (event.key === "Escape" && open) {
            event.preventDefault();
            closeMenu();
          }
        }}
      >
        <EchoMoreNavIcon size="md" />
        <span>更多</span>
        <span className="sidebar__nav-sub">知识与工具</span>
      </button>
      {open && (
        <div
          ref={menuRef}
          id="sidebar-more-menu"
          className="sidebar__more-popover"
          role="menu"
          aria-label="更多功能"
          onKeyDown={(event) => handleMenuKeyDown(event, () => {
            closeMenu();
            triggerRef.current?.focus();
          })}
        >
          {ITEMS.map((item) => (
            <button
              key={item.id}
              type="button"
              className={
                "sidebar__more-item" +
                (activeMoreLabel === item.label ? " sidebar__more-item--active" : "")
              }
              role="menuitem"
              onClick={item.action}
            >
              <span className="sidebar__more-item-icon">{item.icon}</span>
              <span className="sidebar__more-item-label">{item.label}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * EchoAgent 风格侧栏:品牌行 / 导航 / 双分组(任务 + 项目) / 底部用户区。
 *
 * 工作目录只控制 Agent 的文件上下文。任务与项目的展示归属由项目实体中的显式
 * 会话引用决定，因此升级前使用旧默认目录的任务不会变成伪项目节点。
 */
export function Sidebar({
  onNewSession,
  onSelect,
  onNavigate,
  onOpenSettings,
  onToggleCollapse,
  onOpenSearch,
  onPlaceholder,
  onToast,
  onOpenProject,
  onStartProjectConversation,
  onRenameSession,
  onArchiveSession,
  onDeleteSession,
  onSessionArchived,
  onSessionDeleted,
  onRetrySessions,
  activeNav,
}: {
  onNewSession: () => void;
  onSelect: (sessionId: string, cwd?: string) => void;
  onNavigate: (label: string) => void;
  onOpenSettings: () => void;
  /** Collapse the sidebar; an expand affordance is rendered over the main area. */
  onToggleCollapse: () => void;
  /** Open the session search overlay. */
  onOpenSearch: () => void;
  onPlaceholder: (label: string) => void;
  /** Surface transient feedback (e.g. rename/delete failures). */
  onToast?: (message: string) => void;
  /** Open a project detail view from the sidebar. */
  onOpenProject?: (projectId: string) => void;
  /** Start a new conversation within a project. */
  onStartProjectConversation?: (projectId: string) => void;
  /** Application-owned session lifecycle actions shared by every session surface. */
  onRenameSession?: (sessionId: string, title: string, cwd?: string) => Promise<void>;
  onArchiveSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  /** Keep App's focused transcript and project references in sync. */
  onSessionArchived?: (sessionId: string, archived: boolean) => void;
  onSessionDeleted?: (sessionId: string) => void;
  /** Retry the recoverable session/working-directory catalog load. */
  onRetrySessions?: () => void;
  activeNav: string;
}) {
  const independent = useSessionsStore((s) => s.independent);
  const tasksOpen = useSessionsStore((s) => s.tasksOpen);
  const projectsOpen = useSessionsStore((s) => s.projectsOpen);
  const currentSessionId = useSessionsStore((s) => s.currentSessionId);
  const upsertSession = useSessionsStore((s) => s.upsert);
  const removeSession = useSessionsStore((s) => s.remove);
  const setTasksOpen = useSessionsStore((s) => s.setTasksOpen);
  const setProjectsOpen = useSessionsStore((s) => s.setProjectsOpen);
  const organizationSession = useOrgSessionStore((s) => s.session);
  const organizationUserLabel = organizationSession?.loggedIn
    ? organizationSession.user?.displayName?.trim()
      || organizationSession.user?.username?.trim()
      || "组织用户"
    : "本地用户";

  // Task filter state
  const filterStatus = useSessionsStore((s) => s.filterStatus);
  const filterDate = useSessionsStore((s) => s.filterDate);
  const setFilterStatus = useSessionsStore((s) => s.setFilterStatus);
  const setFilterDate = useSessionsStore((s) => s.setFilterDate);
  const clearFilters = useSessionsStore((s) => s.clearFilters);
  const hasFilter = useSessionsStore(selectHasFilter);
  const sessionsLoading = useSessionsStore((s) => s.loading);
  const sessionsError = useSessionsStore((s) => s.error);

  // Real project entities are the only nodes shown in the 项目 section.
  const projects = useProjectsStore((s) => s.projects);
  const [expandedProjects, setExpandedProjects] = useState<Record<string, boolean>>({});

  const [filterOpen, setFilterOpen] = useState(false);
  const filterRef = useRef<HTMLDivElement>(null);
  const filterTriggerRef = useRef<HTMLButtonElement>(null);
  const filterMenuRef = useRef<HTMLDivElement>(null);
  const pendingFilterFocus = useRef<"first" | "last" | null>(null);

  // Close filter dropdown on outside click / Escape
  useEffect(() => {
    if (!filterOpen) return;
    const handleClick = (e: MouseEvent) => {
      if (filterRef.current && !filterRef.current.contains(e.target as Node)) {
        setFilterOpen(false);
      }
    };
    const handleEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFilterOpen(false);
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleEsc);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleEsc);
    };
  }, [filterOpen]);

  useEffect(() => {
    if (!filterOpen || !pendingFilterFocus.current) return;
    focusMenuEdge(filterMenuRef.current, pendingFilterFocus.current);
    pendingFilterFocus.current = null;
  }, [filterOpen]);

  const openFilterFromKeyboard = (edge: "first" | "last") => {
    if (filterOpen) {
      focusMenuEdge(filterMenuRef.current, edge);
      return;
    }
    pendingFilterFocus.current = edge;
    setFilterOpen(true);
  };

  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    sessionId: string;
    sessionTitle: string;
    isPinned: boolean;
    isArchived: boolean;
    projectId?: string;
    returnFocus?: HTMLElement;
  } | null>(null);
  const [moveDialog, setMoveDialog] = useState<{ session: SessionSummary; returnFocus?: HTMLElement } | null>(null);
  const [focusMovedSessionId, setFocusMovedSessionId] = useState<string | null>(null);
  const sidebarRef = useRef<HTMLElement>(null);
  const skipMenuFocusRestoreRef = useRef(false);
  const { requestConfirmation, dialog } = useAppDialog("sidebar-session-actions");

  const allSessions = useMemo(
    () => independent.filter((session) => !session.hidden),
    [independent],
  );

  const handleContextMenu = useCallback((e: React.MouseEvent, sessionId: string, sessionTitle: string, isPinned: boolean, isArchived: boolean, projectId?: string) => {
    e.preventDefault();
    e.stopPropagation();
    const focused = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    const returnFocus = focused && e.currentTarget.contains(focused)
      ? focused
      : e.currentTarget.querySelector<HTMLElement>("button") ?? undefined;
    setContextMenu({ x: e.clientX, y: e.clientY, sessionId, sessionTitle, isPinned, isArchived, projectId, returnFocus });
  }, []);

  const resolveSessionCwd = useCallback((sessionId: string) => (
    allSessions.find((session) => session.sessionId === sessionId)?.cwd
    ?? projects.find((project) => (
      project.conversations.some((conversation) => conversation.sessionId === sessionId)
      || project.plans.some((plan) => plan.sessionId === sessionId)
      || project.tasks.some((task) => task.sessionId === sessionId)
    ))?.cwd
  ), [allSessions, projects]);

  // Rename via EchoAgent's `echo.agent/session/rename`. EchoAgent broadcasts
  // SessionSummaryGenerated on success (agent://summary → store upsert); we also
  // update optimistically to avoid flicker.
  const handleRename = useCallback(async (sessionId: string, newTitle: string) => {
    const session = allSessions.find(s => s.sessionId === sessionId);
    const cwd = resolveSessionCwd(sessionId);
    try {
      if (onRenameSession) await onRenameSession(sessionId, newTitle, cwd);
      else {
        await agentRenameSession(sessionId, newTitle, cwd);
        if (session) upsertSession({ ...session, title: newTitle });
        for (const project of useProjectsStore.getState().projects) {
          useProjectsStore.getState().updateConversationTitle(project.id, sessionId, newTitle);
        }
      }
    } catch (e) {
      onToast?.(`重命名失败：${String(e).replace(/^Error:\s*/, "")}`);
    }
  }, [allSessions, onRenameSession, onToast, resolveSessionCwd, upsertSession]);

  // Delete via EchoAgent's `echo.agent/session/delete` — removes the on-disk session
  // directory. Only drop the sidebar entry once the backend confirms.
  const handleDelete = useCallback(async (sessionId: string) => {
    const cwd = resolveSessionCwd(sessionId);
    try {
      if (onDeleteSession) await onDeleteSession(sessionId, cwd);
      else {
        const result = await agentDeleteSession(sessionId, cwd);
        onSessionDeleted?.(sessionId);
        removeSession(sessionId, cwd);
        useProjectsStore.getState().removeSessionReferences(sessionId);
        if (result?.memoryCleanupWarning) {
          onToast?.(`会话已删除，但自动摘要清理失败：${result.memoryCleanupWarning}`);
        }
      }
    } catch (e) {
      onToast?.(`删除失败：${String(e).replace(/^Error:\s*/, "")}`);
      throw e;
    }
  }, [onDeleteSession, onSessionDeleted, onToast, removeSession, resolveSessionCwd]);

  // Pin/unpin — EchoAgent-only state (~/.echo-agent/echoagent-state.json).
  const handlePin = useCallback(async (sessionId: string, pinned: boolean) => {
    const session = allSessions.find(s => s.sessionId === sessionId);
    if (!session) return;
    try {
      await agentSetSessionPinned(sessionId, pinned);
      upsertSession({ ...session, pinned });
    } catch (e) {
      onToast?.(`置顶失败：${String(e).replace(/^Error:\s*/, "")}`);
    }
  }, [allSessions, upsertSession, onToast]);

  // Archive — EchoAgent-only state. The complete catalog retains the row for
  // Settings management, while the sidebar deliberately hides it immediately.
  const handleArchive = useCallback(async (sessionId: string, archived: boolean) => {
    const session = allSessions.find((entry) => entry.sessionId === sessionId);
    const cwd = resolveSessionCwd(sessionId);
    try {
      if (onArchiveSession) await onArchiveSession(sessionId, archived, cwd);
      else {
        const next = await agentSetSessionArchived(sessionId, archived);
        if (session) upsertSession({ ...session, archived: next });
        useProjectsStore.getState().setSessionArchived(sessionId, next);
        onSessionArchived?.(sessionId, next);
        onToast?.(next ? "已归档，可在“设置 → 数据管理 → 已归档”中恢复" : "已恢复会话");
      }
    } catch (e) {
      onToast?.(`${archived ? "归档" : "恢复"}失败：${String(e).replace(/^Error:\s*/, "")}`);
    }
  }, [allSessions, onArchiveSession, onSessionArchived, onToast, resolveSessionCwd, upsertSession]);

  const requestDeleteSession = useCallback((sessionId: string) => {
    const title = contextMenu?.sessionId === sessionId
      ? contextMenu.sessionTitle
      : allSessions.find((session) => session.sessionId === sessionId)?.title || "未命名会话";
    // The confirmation dialog owns focus next; do not return it behind the modal.
    skipMenuFocusRestoreRef.current = true;
    requestConfirmation({
      title: `永久删除对话“${title}”？`,
      description: (
        <>
          该对话的全部历史记录和自动生成的会话摘要将被永久删除，且无法恢复。
          项目资产和工作区原始文件不会被删除。
        </>
      ),
      confirmLabel: "永久删除",
      danger: true,
      returnFocus: contextMenu?.returnFocus,
      action: () => handleDelete(sessionId),
    });
  }, [allSessions, contextMenu, handleDelete, requestConfirmation]);

  const detachFromProject = useCallback((projectId: string, sessionId: string) => {
    const project = useProjectsStore.getState().projects.find((item) => item.id === projectId);
    const session = allSessions.find((item) => item.sessionId === sessionId);
    const linkedItems = [
      ...(project?.plans.filter((item) => item.sessionId === sessionId).map((item) => item.title) ?? []),
      ...(project?.tasks.filter((item) => item.sessionId === sessionId).map((item) => item.title) ?? []),
    ];
    const perform = async () => {
      await useProjectsStore.getState().detachSessionFromProject(projectId, sessionId);
      onToast?.(session?.archived
        ? "已移出项目，该会话仍在归档管理中"
        : "已移出项目，对话历史仍可在“任务”中查看");
    };
    if (linkedItems.length > 0) {
      skipMenuFocusRestoreRef.current = true;
      requestConfirmation({
        title: `将“${session?.title || "未命名会话"}”移出项目？`,
        description: `会话关联的工作项“${linkedItems.join("”、“")}”将解除关联。工作项和对话历史都会保留。`,
        confirmLabel: "移出项目",
        returnFocus: contextMenu?.returnFocus,
        action: perform,
      });
      return;
    }
    void perform().catch((error) => onToast?.(`移出项目失败：${String(error).replace(/^Error:\s*/, "")}`));
  }, [allSessions, contextMenu, onToast, requestConfirmation]);

  const openMoveDialog = useCallback((sessionId: string) => {
    const session = allSessions.find((item) => item.sessionId === sessionId);
    if (!session) return;
    skipMenuFocusRestoreRef.current = true;
    setMoveDialog({ session, returnFocus: contextMenu?.returnFocus });
  }, [allSessions, contextMenu]);

  const moveToProject = useCallback(async (projectId: string) => {
    if (!moveDialog) return;
    const session = useSessionsStore.getState().independent.find((item) => item.sessionId === moveDialog.session.sessionId);
    if (!session || session.hidden || session.archived) throw new Error("该会话已不在任务列表中，请刷新后重试");
    await useProjectsStore.getState().moveSessionToProject(projectId, session);
    setProjectsOpen(true);
    setExpandedProjects((current) => ({ ...current, [projectId]: true }));
    setFocusMovedSessionId(session.sessionId);
    const projectName = useProjectsStore.getState().projects.find((item) => item.id === projectId)?.name;
    onToast?.(`已移入项目“${projectName || "项目"}”，原会话工作目录保持不变`);
  }, [moveDialog, onToast, setProjectsOpen]);

  useEffect(() => {
    if (!focusMovedSessionId || moveDialog || !projectsOpen) return;
    const rows = sidebarRef.current?.querySelectorAll<HTMLElement>(".sidebar__conv[data-project-id]");
    const row = Array.from(rows ?? []).find((item) => item.dataset.sessionId === focusMovedSessionId);
    row?.querySelector<HTMLButtonElement>("button")?.focus();
    setFocusMovedSessionId(null);
  }, [focusMovedSessionId, moveDialog, projectsOpen, expandedProjects, projects]);

  // Open the row's context menu anchored to its 更多 hover button.
  const openMenuFromButton = useCallback((e: React.MouseEvent, sessionId: string, sessionTitle: string, isPinned: boolean, isArchived: boolean, projectId?: string) => {
    e.preventDefault();
    e.stopPropagation();
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setContextMenu({
      x: rect.left,
      y: rect.bottom + 4,
      sessionId,
      sessionTitle,
      isPinned,
      isArchived,
      projectId,
      returnFocus: e.currentTarget as HTMLElement,
    });
  }, []);

  // One session row, shared by the 任务 group and 空间 node children.
  // The selectable surface and action button are siblings. Nesting click-only
  // spans inside a button made pin/archive unreachable to keyboard users and
  // produced invalid interactive markup.
  const renderConv = (s: SessionSummary, projectId?: string) => (
    <div
      key={s.sessionId}
      data-session-id={s.sessionId}
      data-project-id={projectId}
      className={
        "sidebar__conv" +
        (s.sessionId === currentSessionId ? " sidebar__conv--active" : "") +
        (s.pinned ? " sidebar__conv--pinned" : "")
      }
      onContextMenu={(e) => handleContextMenu(
        e,
        s.sessionId,
        s.title || "未命名会话",
        s.pinned || false,
        s.archived || false,
        projectId,
      )}
    >
      <button
        type="button"
        onClick={() => onSelect(s.sessionId, s.cwd)}
        title={[
          s.title || "未命名会话",
          s.status && ACTIVE_STATUS_META[s.status]?.label,
          s.updatedAt && relativeTime(s.updatedAt),
        ].filter(Boolean).join(" · ")}
        aria-current={s.sessionId === currentSessionId ? "page" : undefined}
        className="sidebar__conv-select"
      >
        <span className="sidebar__conv-title">{s.title || "未命名会话"}</span>
        {s.pinned && <PinFilledIcon size="sm" className="sidebar__conv-pin" />}
        <SessionStatusBadge status={s.status} />
      </button>
      <button
        type="button"
        className="sidebar__conv-action"
        aria-label={`${s.title || "未命名会话"}的会话操作`}
        aria-haspopup="menu"
        aria-expanded={contextMenu?.sessionId === s.sessionId}
        data-tip="会话操作"
        onClick={(e) => openMenuFromButton(
          e,
          s.sessionId,
          s.title || "未命名会话",
          s.pinned || false,
          s.archived || false,
          projectId,
        )}
      >
        <MoreDotsIcon size="sm" />
      </button>
    </div>
  );

  const projectSessionIds = useMemo(() => {
    const ids = new Set<string>();
    for (const project of projects) {
      for (const conversation of project.conversations) ids.add(conversation.sessionId);
      for (const plan of project.plans) if (plan.sessionId) ids.add(plan.sessionId);
      for (const task of project.tasks) if (task.sessionId) ids.add(task.sessionId);
    }
    return ids;
  }, [projects]);
  const taskSessions = useMemo(
    () => allSessions.filter((session) => !projectSessionIds.has(session.sessionId)),
    [allSessions, projectSessionIds],
  );
  const sessionSummaryById = useMemo(
    () => new Map(allSessions.map((session) => [session.sessionId, session])),
    [allSessions],
  );
  const hiddenSessionIds = useMemo(
    () => new Set(independent.filter((session) => session.hidden).map((session) => session.sessionId)),
    [independent],
  );

  // Apply status + date filters only to active standalone tasks. Archived
  // sessions never return to the work sidebar until explicitly restored.
  const filteredIndependent = useMemo(
    () => filterSessions(taskSessions, filterStatus, filterDate),
    [taskSessions, filterStatus, filterDate],
  );
  const scopedIndependentCount = taskSessions.filter((session) => !session.archived).length;
  const visibleProjectRowCount = projects.reduce((count, project) => {
    if (!expandedProjects[project.id]) return count + 1;
    const conversationCount = project.conversations.filter((conversation) => (
      !(sessionSummaryById.get(conversation.sessionId)?.archived ?? conversation.archived)
      && !hiddenSessionIds.has(conversation.sessionId)
    )).length;
    return count + 1 + Math.max(1, conversationCount);
  }, 0);

  return (
    <aside ref={sidebarRef} className="sidebar">
      {/* macOS Overlay 标题栏:红绿灯悬浮在 logo 行左上,整行作为拖拽区
          (Windows 的窗口拖拽由自绘 TitleBar 负责,故仅在 mac 加属性)。 */}
      <div className="sidebar__logo-row" {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})}>
        <img
          className="sidebar__brand-mark"
          src={logoMarkUrl}
          alt=""
          aria-hidden="true"
          draggable={false}
        />
        <div className="sidebar__logo-col" {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})}>
          <span className="sidebar__logo">EchoAgent</span>
        </div>
        <div className="sidebar__logo-spacer" {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})} />
        <button
          className="sidebar__icon-btn"
          aria-label="收起侧边栏"
          data-tip="收起侧边栏"
          onClick={onToggleCollapse}
        >
          <SidebarToggleIcon size="md" />
        </button>
        <button className="sidebar__icon-btn" aria-label="搜索" onClick={onOpenSearch}>
          <SearchIcon size="md" />
        </button>
      </div>

      <nav className="sidebar__nav">
        <button
          className={
            "sidebar__nav-item" +
            (activeNav === "新建任务" ? " sidebar__nav-item--active" : "")
          }
          onClick={onNewSession}
        >
          <EchoNewTaskIcon size="md" />
          <span>新建任务</span>
        </button>
        {NAV.map(({ label, display, icon: Icon }) => (
          <button
            key={label}
            className={
              "sidebar__nav-item" +
              (activeNav === label || (label === "专家·技能·连接器" && isCapabilityView(activeNav)) ? " sidebar__nav-item--active" : "")
            }
            onClick={() => onNavigate(label)}
          >
            <Icon size="md" />
            <span>{display ?? label}</span>
          </button>
        ))}
        <MoreDropdown onNavigate={onNavigate} activeNav={activeNav} />
      </nav>

      <div className="sidebar__content">
        {sessionsError && (
          <div className="sidebar__empty" role="alert">
            <span>会话目录加载失败：{sessionsError}</span>
            {onRetrySessions && (
              <button type="button" className="btn btn--ghost" onClick={onRetrySessions}>
                重试
              </button>
            )}
          </div>
        )}
        {sessionsLoading && (
          <div className="sidebar__empty" role="status">
            {independent.length === 0 ? "正在加载会话…" : "正在刷新会话…"}
          </div>
        )}
        {/* 任务分组: 所有未归属项目的会话，cwd 不参与分类。 */}
        <div className={"sidebar__list-section sidebar__list-section--tasks" + (tasksOpen && filteredIndependent.length > 3 ? " sidebar__list-section--long" : "")}>
        <div className="sidebar__section-head">
          <button
            type="button"
            className="sidebar__section-label"
            aria-expanded={tasksOpen}
            aria-controls="sidebar-task-list"
            onClick={() => setTasksOpen(!tasksOpen)}
          >
            <span>任务 ({hasFilter ? `${filteredIndependent.length}/${scopedIndependentCount}` : scopedIndependentCount})</span>
            <ChevronDownIcon
              size="sm"
              className={"sidebar__chevron" + (tasksOpen ? "" : " sidebar__chevron--collapsed")}
            />
          </button>
          <div className="task-filter-wrap" ref={filterRef}>
            <button
              ref={filterTriggerRef}
              type="button"
              className={"sidebar__section-action task-filter-trigger" + (hasFilter ? " task-filter-trigger--active" : "")}
              aria-label="筛选任务"
              aria-haspopup="menu"
              aria-expanded={filterOpen}
              aria-controls={filterOpen ? "sidebar-task-filter-menu" : undefined}
              onClick={() => setFilterOpen((v) => !v)}
              onKeyDown={(event) => {
                if (["Enter", " ", "ArrowDown", "ArrowUp"].includes(event.key)) {
                  event.preventDefault();
                  openFilterFromKeyboard(event.key === "ArrowUp" ? "last" : "first");
                } else if (event.key === "Escape" && filterOpen) {
                  event.preventDefault();
                  setFilterOpen(false);
                }
              }}
            >
              <FilterIcon size="sm" />
              {hasFilter && <span className="task-filter-trigger__dot" />}
            </button>
            {filterOpen && (
              <div
                ref={filterMenuRef}
                id="sidebar-task-filter-menu"
                className="task-filter-popover"
                role="menu"
                aria-label="任务筛选"
                onKeyDown={(event) => handleMenuKeyDown(event, () => {
                  setFilterOpen(false);
                  filterTriggerRef.current?.focus();
                })}
              >
                <TaskFilterMenu
                  filterStatus={filterStatus}
                  filterDate={filterDate}
                  hasFilter={hasFilter}
                  onSelectStatus={setFilterStatus}
                  onSelectDate={setFilterDate}
                  onClear={clearFilters}
                />
              </div>
            )}
          </div>
        </div>
        {tasksOpen && (
          <div id="sidebar-task-list" className="sidebar__group">
            {filteredIndependent.length === 0 && scopedIndependentCount > 0 && (
              <div className="sidebar__empty sidebar__empty--filter">无匹配筛选条件的任务</div>
            )}
            {filteredIndependent.length === 0 && scopedIndependentCount === 0 && !sessionsLoading && (
              <div className="sidebar__empty">
                这里还没有任务，去完成第一件事吧
              </div>
            )}
            {sortPinnedFirst(filteredIndependent).map((session) => renderConv(session))}
          </div>
        )}
        </div>

        {/* 项目分组: 仅展示真实项目实体。 */}
        <div className={"sidebar__list-section sidebar__list-section--projects" + (projectsOpen && visibleProjectRowCount > 3 ? " sidebar__list-section--long" : "")}>
        <button
          type="button"
          className="sidebar__section-label"
          aria-expanded={projectsOpen}
          aria-controls="sidebar-project-list"
          onClick={() => setProjectsOpen(!projectsOpen)}
        >
          <span>项目 ({projects.length})</span>
          <ChevronDownIcon
            size="sm"
            className={"sidebar__chevron" + (projectsOpen ? "" : " sidebar__chevron--collapsed")}
          />
        </button>
        {projectsOpen && (
          <div id="sidebar-project-list" className="sidebar__group">
            {projects.length === 0 && (
              <div className="sidebar__empty">还没有项目，给下一件大事安个家吧</div>
            )}
            {projects.map((proj) => {
              const open = !!expandedProjects[proj.id];
              const projectConversations = proj.conversations.filter((conversation) => (
                !(sessionSummaryById.get(conversation.sessionId)?.archived ?? conversation.archived)
                && !hiddenSessionIds.has(conversation.sessionId)
              ));
              return (
                <div key={proj.id} className="sidebar__node-wrap">
                  <div
                    className="sidebar__node sidebar__node--project"
                    title={proj.name}
                  >
                    <button
                      type="button"
                      onClick={() => onOpenProject?.(proj.id)}
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 6,
                        flex: 1,
                        minWidth: 0,
                        padding: 0,
                        border: 0,
                        background: "transparent",
                        color: "inherit",
                        font: "inherit",
                        textAlign: "left",
                        cursor: "pointer",
                      }}
                    >
                      <ProjectNodeIcon />
                      <span className="sidebar__node-name">{proj.name}</span>
                    </button>
                    <button
                      type="button"
                      className="sidebar__node-action"
                      aria-label={`在${proj.name}中新建对话`}
                      data-tip="新建对话"
                      onClick={() => onStartProjectConversation?.(proj.id)}
                      style={{ border: 0, padding: 0, background: "transparent" }}
                    >
                      <AddIcon size="sm" />
                    </button>
                    <button
                      type="button"
                      className="sidebar__node-action"
                      aria-label={`${open ? "收起" : "展开"}${proj.name}对话`}
                      aria-expanded={open}
                      onClick={() => setExpandedProjects((prev) => ({ ...prev, [proj.id]: !prev[proj.id] }))}
                      style={{ border: 0, padding: 0, background: "transparent" }}
                    >
                      <ChevronDownIcon
                        size="sm"
                        className={"sidebar__chevron" + (open ? "" : " sidebar__chevron--collapsed")}
                      />
                    </button>
                  </div>
                  {open && (
                    <div className="sidebar__children">
                      {projectConversations.length === 0 && (
                        <div className="sidebar__empty">
                          还没有对话，从项目页开启第一轮吧
                        </div>
                      )}
                      {projectConversations.map((conversation) => {
                        const summary = sessionSummaryById.get(conversation.sessionId);
                        return renderConv({
                          sessionId: conversation.sessionId,
                          title: conversation.title || summary?.title || "未命名会话",
                          cwd: summary?.cwd || proj.cwd || "",
                          updatedAt: summary?.updatedAt || conversation.createdAt,
                          pinned: summary?.pinned,
                          archived: summary?.archived ?? conversation.archived,
                          status: summary?.status,
                        }, proj.id);
                      })}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
        </div>
      </div>

      <div className="sidebar__footer">
        <button
          className="sidebar__user"
          aria-label={organizationUserLabel}
          title={organizationSession?.loggedIn && organizationSession.serverUrl
            ? `${organizationUserLabel} · ${organizationSession.serverUrl}`
            : organizationUserLabel}
          onClick={() => onPlaceholder("用户中心")}
        >
          <UserIcon size="md" />
          <span className="sidebar__user-label">{organizationUserLabel}</span>
        </button>
        <button className="sidebar__icon-btn" aria-label="通知" onClick={() => onPlaceholder("通知")}>
          <BellIcon size="md" />
        </button>
        <button className="sidebar__icon-btn" aria-label="设置" onClick={onOpenSettings}>
          <SettingsIcon size="md" />
        </button>
      </div>

      {contextMenu && (
        <SessionContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          sessionId={contextMenu.sessionId}
          sessionTitle={contextMenu.sessionTitle}
          isPinned={contextMenu.isPinned}
          isArchived={contextMenu.isArchived}
          onClose={() => {
            const returnFocus = contextMenu.returnFocus;
            const shouldRestoreFocus = !skipMenuFocusRestoreRef.current;
            skipMenuFocusRestoreRef.current = false;
            setContextMenu(null);
            if (shouldRestoreFocus) requestAnimationFrame(() => returnFocus?.focus());
          }}
          onRename={handleRename}
          onDelete={requestDeleteSession}
          onPin={handlePin}
          onArchive={handleArchive}
          onDetach={contextMenu.projectId
            ? (sessionId) => detachFromProject(contextMenu.projectId!, sessionId)
            : undefined}
          onMoveToProject={!contextMenu.projectId ? openMoveDialog : undefined}
          onWeixin={async (sessionId) => {
            try { onToast?.(await weixinHandoff(sessionId)); }
            catch (error) { onToast?.(`微信交接失败：${String(error).replace(/^Error:\s*/, "")}`); }
          }}
        />
      )}
      {moveDialog && (
        <MoveToProjectDialog
          sessionTitle={moveDialog.session.title || "未命名会话"}
          sessionCwd={moveDialog.session.cwd}
          projects={projects}
          returnFocus={moveDialog.returnFocus}
          onMove={moveToProject}
          onCreateProject={() => { setMoveDialog(null); onNavigate("项目"); }}
          onClose={() => setMoveDialog(null)}
        />
      )}
      {dialog}
    </aside>
  );
}
