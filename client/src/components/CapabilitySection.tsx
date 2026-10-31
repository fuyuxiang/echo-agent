import type { ReactNode } from "react";
import { NestedPageChrome, PageToolbar } from "./PageToolbar";
import { CAPABILITY_NAV_ITEMS, CAPABILITY_SECTION_LABEL } from "@/lib/capability-navigation";

export function CapabilitySection({ active, onNavigate, children }: {
  active: string;
  onNavigate?: (label: string) => void;
  children: ReactNode;
}) {
  return <div className="panel-section panel-section--capabilities">
    <PageToolbar className="panel-navigation panel-navigation--capabilities"><nav aria-label={`${CAPABILITY_SECTION_LABEL}管理`}>
      <div className="panel-navigation__links">{CAPABILITY_NAV_ITEMS.map(({ route, title }) => (
        <button key={route} type="button" aria-current={route === active ? "page" : undefined}
          onClick={() => onNavigate?.(route)}>{title}</button>
      ))}</div>
    </nav></PageToolbar>
    <NestedPageChrome>{children}</NestedPageChrome>
  </div>;
}
