import { useContext, type ReactNode } from "react";
import { ArrowLeft, Code2 } from "lucide-react";
import { SidebarToggleIcon } from "@/foundation/components/Icon/icons";
import { SidebarLayoutContext } from "@/lib/sidebar-layout";

/** Shared identity for the empty and open-project workbench headers. */
export function WorkbenchIdentity({ onExit, children }: { onExit: () => void; children?: ReactNode }) {
  const sidebarLayout = useContext(SidebarLayoutContext);
  return (
    <div className="coding-workbench__topbar-left" data-tauri-drag-region>
      {sidebarLayout?.collapsed && (
        <button type="button" className="coding-icon-btn" onClick={sidebarLayout.expandSidebar}
          aria-label="展开侧边栏" data-tip="展开侧边栏">
          <SidebarToggleIcon size="md" />
        </button>
      )}
      <button type="button" className="coding-icon-btn" onClick={onExit} aria-label="返回工作台" title="返回工作台">
        <ArrowLeft size={16} />
      </button>
      <div className="coding-workbench__product" data-tauri-drag-region>
        <span className="coding-workbench__product-mark" aria-hidden="true"><Code2 size={14} /></span>
        <strong>代码开发</strong>
      </div>
      {children && <><span className="coding-workbench__topbar-separator" aria-hidden="true" />{children}</>}
    </div>
  );
}
