import { useLayoutEffect, useRef, type ReactNode } from "react";
import { EchoNewTaskIcon, SidebarToggleIcon } from "@/foundation/components/Icon/icons";
import { shortcutLabel } from "@/lib/platform";

/** Window-owned navigation stays mounted while the sidebar and pages change. */
export function WindowSidebarNavigation({ collapsed, onToggle, onNewTask, children }: {
  collapsed: boolean;
  onToggle: () => void;
  onNewTask: () => void;
  children: ReactNode;
}) {
  const label = collapsed ? "展开侧边栏" : "收起侧边栏";
  const toggleRef = useRef<HTMLButtonElement>(null);
  const taskToolsRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (collapsed && taskToolsRef.current?.contains(document.activeElement)) {
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
      <div ref={taskToolsRef} className="window-sidebar-task-tools" hidden={collapsed}>
        {children}
      </div>
      {collapsed && (
        <button type="button" className="main-topbar__btn" aria-label="新建任务" data-tip="新建任务" onClick={onNewTask}>
          <EchoNewTaskIcon size="md" />
        </button>
      )}
    </div>
    </>
  );
}
