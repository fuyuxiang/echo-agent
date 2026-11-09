import { useLayoutEffect, useRef } from "react";
import { EchoNewTaskIcon, SearchIcon, SidebarToggleIcon } from "@/foundation/components/Icon/icons";
import { shortcutLabel } from "@/lib/platform";

/** Window-owned navigation stays mounted while the sidebar and pages change. */
export function WindowSidebarNavigation({ collapsed, onToggle, onNewTask, onOpenSearch }: {
  collapsed: boolean;
  onToggle: () => void;
  onNewTask: () => void;
  onOpenSearch: () => void;
}) {
  const label = collapsed ? "展开侧边栏" : "收起侧边栏";
  const toggleRef = useRef<HTMLButtonElement>(null);
  const quickActionsRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!collapsed && quickActionsRef.current?.contains(document.activeElement)) {
      toggleRef.current?.focus({ preventScroll: true });
    }
  }, [collapsed]);

  return (
    <>
    <div className="window-native-drag-region" data-tauri-drag-region aria-hidden="true" />
    <div className="window-sidebar-navigation" role="group" aria-label="侧边栏工具">
      <button
        ref={toggleRef}
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
      <span className="sidebar__logo window-sidebar-brand" hidden={collapsed} data-tauri-drag-region>
        Echo<span>Agent</span>
      </span>
      <div ref={quickActionsRef} className="window-sidebar-quick-actions" hidden={!collapsed}>
        <button type="button" className="main-topbar__btn" aria-label="搜索全部任务"
          data-tip={`搜索全部任务（${shortcutLabel("⌘K", "Ctrl+K")}）`} onClick={onOpenSearch}>
          <SearchIcon size="md" />
        </button>
        <button type="button" className="main-topbar__btn" aria-label="新建任务" data-tip="新建任务" onClick={onNewTask}>
          <EchoNewTaskIcon size="md" />
        </button>
      </div>
    </div>
    </>
  );
}
