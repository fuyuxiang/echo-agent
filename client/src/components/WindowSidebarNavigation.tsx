import { EchoNewTaskIcon, SidebarToggleIcon } from "@/foundation/components/Icon/icons";
import { shortcutLabel } from "@/lib/platform";

/** Window-owned navigation stays mounted while the sidebar and pages change. */
export function WindowSidebarNavigation({ collapsed, onToggle, onNewTask }: {
  collapsed: boolean;
  onToggle: () => void;
  onNewTask: () => void;
}) {
  const label = collapsed ? "展开侧边栏" : "收起侧边栏";
  return (
    <>
    <div className="window-native-drag-region" data-tauri-drag-region aria-hidden="true" />
    <div className="window-sidebar-navigation" role="group" aria-label="窗口导航">
      <span className="sidebar__logo window-sidebar-brand" data-tauri-drag-region>
        <span className="sidebar__logo-echo">Echo</span><span className="sidebar__logo-agent">Agent</span>
      </span>
      <button
        type="button"
        className="main-topbar__btn window-sidebar-toggle"
        aria-label={label}
        aria-expanded={!collapsed}
        aria-controls="app-sidebar"
        data-tip={`${label}（${shortcutLabel("⌘B", "Ctrl+B")}）`}
        onClick={onToggle}
      >
        <SidebarToggleIcon size="md" />
      </button>
      {collapsed && (
        <button type="button" className="main-topbar__btn" aria-label="新建任务" data-tip="新建任务" onClick={onNewTask}>
          <EchoNewTaskIcon size="md" />
        </button>
      )}
    </div>
    </>
  );
}
