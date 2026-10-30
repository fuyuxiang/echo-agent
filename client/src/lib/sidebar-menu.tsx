import {
  EchoProjectNavIcon, BuildingIcon, Code2Icon, EchoExpertNavIcon,
  MoreMenuImaKnowledgeIcon, EchoAutomationNavIcon,
} from "@/foundation/components/Icon/icons";
import { CAPABILITY_SECTION_LABEL, isCapabilityView } from "./capability-navigation";

export const SIDEBAR_MENU_ITEMS = {
  projects: { label: "项目", route: "项目", icon: EchoProjectNavIcon },
  organization: { label: "组织", route: "组织", icon: BuildingIcon },
  coding: { label: "代码开发", route: "代码开发", icon: Code2Icon },
  capabilities: { label: CAPABILITY_SECTION_LABEL, route: "专家·技能·连接器", icon: EchoExpertNavIcon },
  knowledge: { label: "知识库", route: "知识库", icon: MoreMenuImaKnowledgeIcon },
  automation: { label: "定时任务", route: "自动化", icon: EchoAutomationNavIcon },
} as const;

export type SidebarMenuId = keyof typeof SIDEBAR_MENU_ITEMS;
export type SidebarMenuGroup = "primary" | "more";
export type SidebarMenuLayout = Record<SidebarMenuGroup, SidebarMenuId[]>;
export const SIDEBAR_MENU_STORAGE_KEY = "echoagent.sidebar-menu.v1";

const DEFAULT_LAYOUT: SidebarMenuLayout = {
  primary: ["projects", "organization", "coding", "capabilities"],
  more: ["knowledge", "automation"],
};

/** Keep every available destination exactly once, including after upgrades. */
export function normalizeSidebarMenuLayout(value: unknown): SidebarMenuLayout {
  const source = value && typeof value === "object" ? value as Partial<SidebarMenuLayout> : {};
  const seen = new Set<SidebarMenuId>();
  const layout: SidebarMenuLayout = { primary: [], more: [] };
  for (const group of ["primary", "more"] as const) {
    const entries = Array.isArray(source[group]) ? source[group] : [];
    for (const id of entries) {
      if (typeof id === "string" && Object.prototype.hasOwnProperty.call(SIDEBAR_MENU_ITEMS, id) && !seen.has(id)) {
        seen.add(id);
        layout[group].push(id);
      }
    }
  }
  for (const group of ["primary", "more"] as const) {
    for (const id of DEFAULT_LAYOUT[group]) {
      if (!seen.has(id)) layout[group].push(id);
    }
  }
  return layout;
}

export function loadSidebarMenuLayout(): SidebarMenuLayout {
  try {
    return normalizeSidebarMenuLayout(JSON.parse(localStorage.getItem(SIDEBAR_MENU_STORAGE_KEY) ?? "null"));
  } catch {
    return normalizeSidebarMenuLayout(null);
  }
}

export function saveSidebarMenuLayout(layout: SidebarMenuLayout): SidebarMenuLayout {
  const normalized = normalizeSidebarMenuLayout(layout);
  // Let the editor report storage failures rather than claim the preference was saved.
  localStorage.setItem(SIDEBAR_MENU_STORAGE_KEY, JSON.stringify(normalized));
  return normalized;
}

/** `index` is an insertion boundary in the original target list. */
export function moveSidebarMenuItem(
  layout: SidebarMenuLayout, id: SidebarMenuId, group: SidebarMenuGroup,
  index = layout[group].length,
): SidebarMenuLayout {
  const previousIndex = layout[group].indexOf(id);
  const next = {
    primary: layout.primary.filter(item => item !== id),
    more: layout.more.filter(item => item !== id),
  };
  const insertion = previousIndex >= 0 && previousIndex < index ? index - 1 : index;
  next[group].splice(Math.max(0, Math.min(insertion, next[group].length)), 0, id);
  return next;
}

export function isSidebarMenuItemActive(id: SidebarMenuId, activeNav: string): boolean {
  return activeNav === SIDEBAR_MENU_ITEMS[id].route
    || (id === "capabilities" && isCapabilityView(activeNav));
}
