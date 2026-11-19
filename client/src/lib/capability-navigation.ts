import { createContext } from "react";

export const CAPABILITY_NAV_ITEMS = [
  { route: "专家·技能·连接器", title: "专家" },
  { route: "专家团", title: "专家团" },
  { route: "技能", title: "技能" },
  { route: "连接器", title: "连接器" },
  { route: "插件", title: "插件" },
] as const;

export function isCapabilityView(route: string): boolean {
  return CAPABILITY_NAV_ITEMS.some(item => item.route === route);
}
// Persisted navigation keys stay compatible; display names describe the whole section.
export const CAPABILITY_SECTION_LABEL = "专家·技能·连接器";

export type ComposerCapabilityDestination =
  | { route: typeof CAPABILITY_SECTION_LABEL; create?: true }
  | { route: "技能" }
  | { route: "连接器"; configure?: true; serverName?: string };

export type OpenComposerCapability = (destination: ComposerCapabilityDestination, cwd?: string) => void;

export const ComposerCapabilityNavigation = createContext<OpenComposerCapability | null>(null);
