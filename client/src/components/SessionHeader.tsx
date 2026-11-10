import { useContext, useLayoutEffect, useRef, type ReactNode } from "react";
import { SidebarLayoutContext } from "@/lib/sidebar-layout";
import { PanelRight } from "lucide-react";
import { SidebarToggleIcon, EchoNewTaskIcon, FolderIcon } from "@/foundation/components/Icon/icons";
import { TopbarTitle } from "./TopbarTitle";
import { TopbarActions, type TopbarActionsProps } from "./TopbarActions";

export type WorkspaceTarget = "fileTree" | "changes" | "artifacts" | "browser" | "plan" | "subagents" | "teams";
export interface SessionWorkspaceSummary {
  searchable: boolean;
  hasContent: boolean;
  changeCount: number;
  artifactCount: number;
  hasPlan: boolean;
  planCompleted: number;
  planTotal: number;
  subagentCount: number;
  runningSubagents: number;
  teamCount: number;
}
export interface SessionNavigationProps {
  sidebarCollapsed?: boolean;
  onToggleSidebar?: () => void;
  onNewTask?: () => void;
}

export interface ProjectSessionBreadcrumb {
  name: string;
  onOpenProjects: () => void;
  onOpenProject: () => void;
}

export function SessionNavigation({ sidebarCollapsed, onToggleSidebar, onNewTask }: SessionNavigationProps) {
  const layout = useContext(SidebarLayoutContext);
  if (!sidebarCollapsed || layout?.windowNavigationMounted) return null;
  return <>
    {onToggleSidebar && <button type="button" className="main-topbar__btn" aria-label="展开侧边栏"
      aria-expanded={false} aria-controls="app-sidebar" onClick={onToggleSidebar} data-tip="展开侧边栏">
      <SidebarToggleIcon size="md" />
    </button>}
    {onNewTask && <button type="button" className="main-topbar__btn" aria-label="新建任务" data-tip="新建任务" onClick={onNewTask}><EchoNewTaskIcon size="md" /></button>}
  </>;
}

/** One non-scrolling session header. Domain state and workspace navigation stay in ChatView. */
export function SessionHeader({ sessionId, title, onRename, actions, summary, activeView,
  onView, onCloseView, onToggleWorkspace, onFind, findOpen, navigation, leading, historyDisabled, projectBreadcrumb,
}: {
  sessionId: string;
  title: string;
  onRename?: (title: string) => Promise<void>;
  actions?: Omit<TopbarActionsProps, "sessionId" | "title">;
  summary: SessionWorkspaceSummary;
  activeView: string | null;
  onView: (view: WorkspaceTarget) => void;
  onCloseView: () => void;
  onToggleWorkspace?: () => void;
  onFind: () => void;
  findOpen: boolean;
  navigation?: SessionNavigationProps;
  leading?: ReactNode;
  historyDisabled?: boolean;
  projectBreadcrumb?: ProjectSessionBreadcrumb;
}) {
  const headerRef = useRef<HTMLElement>(null);
  const navigationRef = useRef<HTMLDivElement>(null);
  const actionsRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const header = headerRef.current;
    const navigation = navigationRef.current;
    const actions = actionsRef.current;
    if (!header || !navigation || !actions) return;
    // Reserve the wider side on both sides of the title, including macOS's
    // traffic-light inset. Tool changes must not shift or overlap the title.
    const measure = () => {
      const bounds = header.getBoundingClientRect();
      const left = navigation.getBoundingClientRect();
      const right = actions.getBoundingClientRect();
      const space = Math.ceil(Math.max(parseFloat(getComputedStyle(header).paddingInlineStart) || 0, left.width ? left.right - bounds.left : 0, bounds.right - right.left) + 12);
      header.style.setProperty("--session-header-side-space", `${space}px`);
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    [header, navigation, actions].forEach(element => observer?.observe(element));
    return () => observer?.disconnect();
  }, []);

  const sessionTitle = onRename
    ? <TopbarTitle key={sessionId} title={title} onRename={onRename} />
    : <span className="main-topbar__title" title={title}>{title}</span>;

  return <header ref={headerRef} className="main-topbar session-header" data-tauri-drag-region>
    <div ref={navigationRef} className="session-header__navigation"><SessionNavigation {...navigation} />{leading}</div>
    {projectBreadcrumb ? (
      <nav className="session-header__project-path" aria-label="项目会话路径">
        <button type="button" className="session-header__project-list" onClick={projectBreadcrumb.onOpenProjects}>
          <FolderIcon size="sm" aria-hidden="true" />
          项目
        </button>
        <span aria-hidden="true">/</span>
        <button type="button" className="session-header__project-name" title={projectBreadcrumb.name} onClick={projectBreadcrumb.onOpenProject}>
          {projectBreadcrumb.name}
        </button>
        <span aria-hidden="true">/</span>
        <span className="session-header__project-current" aria-current="page">{sessionTitle}</span>
      </nav>
    ) : <div className="session-header__identity" data-tauri-drag-region>{sessionTitle}</div>}
    <div ref={actionsRef} className="session-header__actions">
      {summary.hasPlan && <button type="button" className="session-header__context"
        aria-label={summary.planTotal ? `查看执行计划，已完成 ${summary.planCompleted}/${summary.planTotal} 步` : "查看执行计划"}
        aria-expanded={activeView === "plan"} aria-controls="session-workspace" onClick={() => onView("plan")}>
        {summary.planTotal ? `计划 ${summary.planCompleted}/${summary.planTotal}` : "执行计划"}
      </button>}
      <TopbarActions key={sessionId} sessionId={sessionId} title={title} {...actions} historyDisabled={historyDisabled}
        onFind={summary.searchable ? onFind : undefined} findOpen={findOpen} />
      <button type="button" className="main-topbar__btn session-header__workspace-toggle"
        aria-label={activeView ? "收起工作区" : "展开工作区"} data-tip={activeView ? "收起工作区" : "展开工作区"}
        aria-expanded={Boolean(activeView)} aria-controls="session-workspace"
        onClick={onToggleWorkspace ?? (() => activeView ? onCloseView() : onView("artifacts"))}>
        <PanelRight size={18} aria-hidden="true" />
        {summary.artifactCount > 0 && <span className="session-header__badge" aria-label={`${summary.artifactCount} 项成果`}>{summary.artifactCount > 99 ? "99+" : summary.artifactCount}</span>}
      </button>
    </div>
  </header>;
}
