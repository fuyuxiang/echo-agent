import type { ReactNode } from "react";
import { PanelRight } from "lucide-react";
import { SidebarToggleIcon, EchoNewTaskIcon } from "@/foundation/components/Icon/icons";
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

export function SessionNavigation({ sidebarCollapsed, onToggleSidebar, onNewTask }: SessionNavigationProps) {
  return <>
    {onToggleSidebar && <button type="button" className="main-topbar__btn" aria-label={sidebarCollapsed ? "展开侧边栏" : "收起侧边栏"}
      aria-expanded={!sidebarCollapsed} aria-controls="app-sidebar" onClick={onToggleSidebar} data-tip={sidebarCollapsed ? "展开侧边栏" : "收起侧边栏"}>
      <SidebarToggleIcon size="md" />
    </button>}
    {sidebarCollapsed && onNewTask && <button type="button" className="main-topbar__btn" aria-label="新建任务" data-tip="新建任务" onClick={onNewTask}><EchoNewTaskIcon size="md" /></button>}
  </>;
}

/** One non-scrolling session header. Domain state and workspace navigation stay in ChatView. */
export function SessionHeader({ sessionId, title, onRename, actions, summary, activeView,
  onView, onCloseView, onToggleWorkspace, onFind, findOpen, navigation, leading, historyDisabled,
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
}) {
  return <header className="main-topbar session-header" data-tauri-drag-region>
    <div className="session-header__navigation"><SessionNavigation {...navigation} />{leading}</div>
    <div className="session-header__identity" data-tauri-drag-region>
      {onRename ? <TopbarTitle key={sessionId} title={title} onRename={onRename} /> : <span className="main-topbar__title" title={title}>{title}</span>}
    </div>
    <div className="session-header__actions">
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
