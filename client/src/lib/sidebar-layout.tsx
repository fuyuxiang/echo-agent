import { createContext, useCallback, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type SetStateAction } from "react";

export const SIDEBAR_DEFAULT_WIDTH = 264;
export const SIDEBAR_MIN_WIDTH = 240;
export const SIDEBAR_MAX_WIDTH = 400;
export const SIDEBAR_COLLAPSE_THRESHOLD = 160;
export const SIDEBAR_WIDTH_KEY = "app-sidebar-width";
export const MAIN_CONTENT_MIN_WIDTH = 480;

type PanelRegistration = { minimumWidth: number; close: () => void } | null;
export const SidebarLayoutContext = createContext<{
  panelMaxWidth: number;
  registerPanel: (panel: PanelRegistration) => void;
  collapsed: boolean;
  resizing: boolean;
  expandSidebar: () => void;
} | null>(null);

function clampWidth(value: number) {
  return Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, value));
}

function readWidth() {
  try {
    const stored = localStorage.getItem(SIDEBAR_WIDTH_KEY);
    const width = stored?.trim() ? Number(stored) : NaN;
    return Number.isFinite(width) && width > 0 ? clampWidth(width) : SIDEBAR_DEFAULT_WIDTH;
  } catch {
    return SIDEBAR_DEFAULT_WIDTH;
  }
}

export function sidebarBounds(containerWidth: number, panelMinimum: number) {
  const available = containerWidth - panelMinimum - MAIN_CONTENT_MIN_WIDTH;
  return {
    maxWidth: Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, available)),
    mustCollapse: panelMinimum > 0 && available < SIDEBAR_MIN_WIDTH,
  };
}

/** Keep the user's width separate from temporary constraints imposed by panels. */
export function useAppSidebarLayout() {
  const bodyRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(() => typeof window === "undefined" ? 1200 : window.innerWidth);
  const [preferredWidth, setPreferredWidth] = useState(readWidth);
  const [requestedCollapsed, setRequestedCollapsed] = useState(false);
  const [previewWidth, setPreviewWidth] = useState<number | null>(null);
  const [resizing, setResizing] = useState(false);
  const [panelMinimum, setPanelMinimum] = useState(0);
  const closePanelRef = useRef<(() => void) | undefined>();
  const expandFocusRef = useRef(false);
  const bounds = sidebarBounds(containerWidth, panelMinimum);
  const collapsed = requestedCollapsed || bounds.mustCollapse;
  const width = Math.min(bounds.maxWidth, clampWidth(previewWidth ?? preferredWidth));
  const restoreMaxWidth = sidebarBounds(containerWidth, 0).maxWidth;
  const snapshot = useRef({ collapsed, mustCollapse: bounds.mustCollapse });
  snapshot.current = { collapsed, mustCollapse: bounds.mustCollapse };

  useLayoutEffect(() => {
    const measure = () => {
      const measured = bodyRef.current?.getBoundingClientRect().width;
      setContainerWidth(measured && measured > 0 ? measured : window.innerWidth);
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    if (bodyRef.current) observer?.observe(bodyRef.current);
    window.addEventListener("resize", measure);
    return () => { observer?.disconnect(); window.removeEventListener("resize", measure); };
  }, []);

  const registerPanel = useCallback((panel: PanelRegistration) => {
    closePanelRef.current = panel?.close;
    setPanelMinimum(panel?.minimumWidth ?? 0);
  }, []);

  const setCollapsed = useCallback((action: SetStateAction<boolean>) => {
    const next = typeof action === "function" ? action(snapshot.current.collapsed) : action;
    if (!next) {
      // An explicit restore always works, even when a narrow window cannot fit
      // both side panels. Close the workspace panel rather than hide navigation.
      if (snapshot.current.mustCollapse) closePanelRef.current?.();
      expandFocusRef.current = document.activeElement?.getAttribute("aria-label") === "展开侧边栏";
    }
    setRequestedCollapsed(next);
  }, []);

  useLayoutEffect(() => {
    if (collapsed && bodyRef.current?.querySelector(".sidebar")?.contains(document.activeElement)) {
      bodyRef.current.querySelector<HTMLElement>("[data-sidebar-resize]")?.focus({ preventScroll: true });
    } else if (!collapsed && expandFocusRef.current) {
      bodyRef.current?.querySelector<HTMLElement>('.sidebar button[aria-label="收起侧边栏"]')?.focus({ preventScroll: true });
      expandFocusRef.current = false;
    }
  }, [collapsed]);

  const commitWidth = useCallback((value: number) => {
    if (!Number.isFinite(value)) return;
    const next = Math.round(clampWidth(value));
    setPreferredWidth(next);
    try { localStorage.setItem(SIDEBAR_WIDTH_KEY, String(next)); } catch { /* Storage is optional. */ }
  }, []);

  // Repair invalid stored dimensions once, and never persist drag previews or
  // widths temporarily clamped by a smaller window / an open workspace panel.
  useLayoutEffect(() => { commitWidth(preferredWidth); }, [commitWidth]);

  const panelMaxWidth = Math.max(
    panelMinimum || 280,
    containerWidth - (collapsed ? 0 : width) - Math.min(MAIN_CONTENT_MIN_WIDTH, Math.max(240, containerWidth - 280)),
  );
  const expandSidebar = useCallback(() => setCollapsed(false), [setCollapsed]);
  const context = useMemo(() => ({ panelMaxWidth, registerPanel, collapsed, resizing, expandSidebar }),
    [panelMaxWidth, registerPanel, collapsed, resizing, expandSidebar]);
  return {
    bodyRef, context, collapsed, setCollapsed, width, preferredWidth,
    maxWidth: bounds.maxWidth, restoreMaxWidth, commitWidth, setPreviewWidth, resizing, setResizing,
    style: { "--app-sidebar-width": `${width}px` } as CSSProperties,
  };
}
