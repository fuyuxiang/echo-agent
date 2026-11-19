import type { ReactNode } from "react";
import { ChevronLeftIcon } from "@/foundation/components/Icon/icons";
import { ManagementToolbar, NestedPageChrome } from "./PageToolbar";
import { CAPABILITY_NAV_ITEMS, CAPABILITY_SECTION_LABEL } from "@/lib/capability-navigation";

export function CapabilitySection({ active, onNavigate, onBack, children, hideNavigation = false }: {
  active: string;
  onNavigate?: (label: string) => void;
  onBack?: () => void;
  children: ReactNode;
  hideNavigation?: boolean;
}) {
  return <div className="panel-section panel-section--capabilities">
    {!hideNavigation && <ManagementToolbar className="panel-navigation panel-navigation--capabilities"><nav aria-label={`${CAPABILITY_SECTION_LABEL}管理`}>
      <div className="panel-navigation__links">
        {onBack && <button type="button" autoFocus onClick={onBack}><ChevronLeftIcon size="sm" />返回输入</button>}
        {CAPABILITY_NAV_ITEMS.map(({ route, title }) => (
          <button key={route} type="button" aria-current={route === active ? "page" : undefined}
            onClick={() => onNavigate?.(route)}>{title}</button>
        ))}
      </div>
    </nav></ManagementToolbar>}
    <NestedPageChrome>{children}</NestedPageChrome>
  </div>;
}
