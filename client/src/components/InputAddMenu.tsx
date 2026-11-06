import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { ArrowLeft, AudioLines, Check, ChevronRight, Globe2, Monitor, Paperclip } from "lucide-react";
import {
  AddIcon,
  ExpertTabIcon,
  SkillTabIcon,
  ConnectorTabIcon,
} from "@/foundation/components/Icon/icons";
import { skillsList, agentsList, mcpList } from "@/lib/agent-client";
import type { AutomationCapabilities, AutomationMode } from "@/lib/automation-client";
import type { AgentEntry, McpServerEntry, SkillInfo } from "@/lib/types";

interface InputAddMenuProps {
  onPickFiles: () => void;
  disabled?: boolean;
  /** Chat readiness may block actions while local meeting history remains accessible. */
  allowMeetingHistoryWhileDisabled?: boolean;
  onSelectExpert?: (agent: AgentEntry) => void;
  onSelectSkill?: (skillName: string) => void;
  onNavigateConnectors?: () => void;
  meetingMinutesAvailable?: boolean;
  onOpenMeetingMinutes?: () => void;
  automationMode?: AutomationMode;
  automationCapabilities?: AutomationCapabilities | null;
  automationModeDisabled?: boolean;
  onAutomationModeChange?: (mode: AutomationMode) => void | Promise<void>;
}

type MenuItemId = "add-files" | "meeting-minutes" | "browser-use" | "computer-use" | "experts" | "skills" | "connectors";
type CatalogId = "experts" | "skills" | "connectors";
type SubmenuStyle = CSSProperties & { "--iam-submenu-top": string };
type SubmenuSide = "left" | "right";

interface MenuItem {
  id: MenuItemId;
  label: string;
  icon: React.ReactNode;
  description?: string;
}

const SUBMENU_GAP = 4;
const SUBMENU_VIEWPORT_INSET = 8;
// Shared with CSS so we can choose explicit panel navigation before a hover opens it.
const SUBMENU_WIDTH_REM = 17.5;

const FILE_ITEMS: MenuItem[] = [
  { id: "add-files", label: "添加文件", icon: <Paperclip size={16} /> },
];

const MEETING_ITEM: MenuItem = {
  id: "meeting-minutes",
  label: "会议录音与纪要",
  description: "查看历史会议，录音、转写并生成纪要",
  icon: <AudioLines size={16} />,
};

const AUTOMATION_ITEMS: MenuItem[] = [
  {
    id: "browser-use",
    label: "操作网页",
    description: "在独立浏览器中完成网页任务",
    icon: <Globe2 size={16} />,
  },
  {
    id: "computer-use",
    label: "操作电脑",
    description: "读取控件、查看屏幕并操作本机应用",
    icon: <Monitor size={16} />,
  },
];

const CATALOG_ITEMS: MenuItem[] = [
  { id: "experts", label: "专家", icon: <ExpertTabIcon size="md" /> },
  { id: "skills", label: "技能", icon: <SkillTabIcon size="md" /> },
  { id: "connectors", label: "连接器", icon: <ConnectorTabIcon size="md" /> },
];

function isCatalogItem(id: MenuItemId): id is CatalogId {
  return id === "experts" || id === "skills" || id === "connectors";
}

function focusCatalogItem(element: HTMLElement | null | undefined) {
  if (!element) return;
  // Native focus scrolling can move the entire chat when the menu extends
  // outside its composer. Reveal options only in the catalog's own scroller.
  element.focus({ preventScroll: true });
  const scroller = element.closest<HTMLElement>(".iam-submenu__scroll");
  if (!scroller) return;
  const item = element.getBoundingClientRect();
  const viewport = scroller.getBoundingClientRect();
  if (item.top < viewport.top) scroller.scrollTop += item.top - viewport.top;
  else if (item.bottom > viewport.bottom) scroller.scrollTop += item.bottom - viewport.bottom;
}

export function InputAddMenu({
  onPickFiles,
  disabled = false,
  allowMeetingHistoryWhileDisabled = false,
  onSelectExpert,
  onSelectSkill,
  onNavigateConnectors,
  meetingMinutesAvailable = false,
  onOpenMeetingMinutes,
  automationMode = "default",
  automationCapabilities,
  automationModeDisabled = false,
  onAutomationModeChange,
}: InputAddMenuProps) {
  const [open, setOpen] = useState(false);
  const [hoveredItem, setHoveredItem] = useState<MenuItemId | null>(null);
  const [submenuTop, setSubmenuTop] = useState(0);
  const [submenuSide, setSubmenuSide] = useState<SubmenuSide>("right");
  const [submenuLeft, setSubmenuLeft] = useState<number | null>(null);
  const [submenuPanel, setSubmenuPanel] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const mainMenuRef = useRef<HTMLDivElement>(null);
  const mainItemRefs = useRef(new Map<MenuItemId, HTMLButtonElement>());
  const submenuRef = useRef<HTMLDivElement>(null);
  const loadGenerationRef = useRef(0);
  const focusFirstItemOnOpenRef = useRef(false);
  const pendingSubmenuFocusRef = useRef(false);
  const pendingMainFocusRef = useRef<MenuItemId | null>(null);
  const previousPanelRef = useRef(false);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const leaveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const [experts, setExperts] = useState<AgentEntry[]>([]);
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [connectors, setConnectors] = useState<McpServerEntry[]>([]);
  const [catalogLoading, setCatalogLoading] = useState(false);
  const [catalogErrors, setCatalogErrors] = useState<Partial<Record<CatalogId, string>>>({});
  const hasMeetingHistory = Boolean(meetingMinutesAvailable && onOpenMeetingMinutes);
  const meetingHistoryOnly = disabled && allowMeetingHistoryWhileDisabled && hasMeetingHistory;
  const triggerDisabled = disabled && !meetingHistoryOnly;
  const fileItems = hasMeetingHistory
    ? [...FILE_ITEMS, MEETING_ITEM]
    : FILE_ITEMS;
  const menuGroups = onAutomationModeChange
    ? [fileItems, AUTOMATION_ITEMS, CATALOG_ITEMS]
    : [fileItems, CATALOG_ITEMS];
  const flatItems = menuGroups.flat();
  const showingPanel = submenuPanel && hoveredItem !== null && isCatalogItem(hoveredItem);

  const clearHoverTimers = useCallback(() => {
    if (hoverTimerRef.current) clearTimeout(hoverTimerRef.current);
    if (leaveTimerRef.current) clearTimeout(leaveTimerRef.current);
    hoverTimerRef.current = null;
    leaveTimerRef.current = null;
  }, []);

  const loadData = useCallback(async () => {
    const generation = ++loadGenerationRef.current;
    setCatalogLoading(true);
    setCatalogErrors({});
    const [expertResult, skillResult, connectorResult] = await Promise.allSettled([
      agentsList(),
      skillsList(),
      mcpList(),
    ]);
    if (generation !== loadGenerationRef.current) return;

    const nextErrors: Partial<Record<CatalogId, string>> = {};
    if (expertResult.status === "fulfilled") setExperts(expertResult.value);
    else nextErrors.experts = "专家加载失败";
    if (skillResult.status === "fulfilled") {
      setSkills(skillResult.value.filter((skill) => skill.enabled));
    } else {
      nextErrors.skills = "技能加载失败";
    }
    if (connectorResult.status === "fulfilled") {
      setConnectors(connectorResult.value.filter((server) => server.enabled));
    } else {
      nextErrors.connectors = "连接器加载失败";
    }
    setCatalogErrors(nextErrors);
    setCatalogLoading(false);
  }, []);

  useEffect(() => {
    if (open && !meetingHistoryOnly) void loadData();
    if (meetingHistoryOnly) {
      loadGenerationRef.current += 1;
      setCatalogLoading(false);
      setHoveredItem(null);
      pendingSubmenuFocusRef.current = false;
      clearHoverTimers();
    }
  }, [open, meetingHistoryOnly, loadData, clearHoverTimers]);

  useEffect(() => {
    if (!open || !focusFirstItemOnOpenRef.current) return;
    mainItemRefs.current.get(meetingHistoryOnly ? "meeting-minutes" : "add-files")?.focus({ preventScroll: true });
    focusFirstItemOnOpenRef.current = false;
  }, [open, meetingHistoryOnly]);

  // Close on outside click
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        clearHoverTimers();
        focusFirstItemOnOpenRef.current = false;
        pendingSubmenuFocusRef.current = false;
        setOpen(false);
        setHoveredItem(null);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open, clearHoverTimers]);

  useEffect(() => {
    if (!triggerDisabled) return;
    clearHoverTimers();
    focusFirstItemOnOpenRef.current = false;
    setOpen(false);
    setHoveredItem(null);
  }, [triggerDisabled, clearHoverTimers]);

  // Close on Escape
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        clearHoverTimers();
        focusFirstItemOnOpenRef.current = false;
        pendingSubmenuFocusRef.current = false;
        setOpen(false);
        setHoveredItem(null);
        triggerRef.current?.focus({ preventScroll: true });
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, clearHoverTimers]);

  // Cleanup timers
  useEffect(() => {
    return () => {
      loadGenerationRef.current += 1;
      clearHoverTimers();
    };
  }, [clearHoverTimers]);

  const handleItemEnter = (id: MenuItemId) => {
    if (leaveTimerRef.current) { clearTimeout(leaveTimerRef.current); leaveTimerRef.current = null; }
    if (hoverTimerRef.current) clearTimeout(hoverTimerRef.current);
    // Direct actions never open the catalog flyout.
    if (disabled || !isCatalogItem(id)) { setHoveredItem(null); return; }
    // A replacement panel must never appear underneath a hovering pointer.
    if (submenuPanel) return;
    hoverTimerRef.current = setTimeout(() => openSubmenu(id), 150);
  };

  const handleItemLeave = () => {
    if (hoverTimerRef.current) { clearTimeout(hoverTimerRef.current); hoverTimerRef.current = null; }
    if (submenuPanel) return;
    leaveTimerRef.current = setTimeout(() => setHoveredItem(null), 200);
  };

  const handleSubmenuEnter = () => {
    if (leaveTimerRef.current) { clearTimeout(leaveTimerRef.current); leaveTimerRef.current = null; }
  };

  const handleSubmenuLeave = () => {
    if (submenuPanel) return;
    leaveTimerRef.current = setTimeout(() => setHoveredItem(null), 200);
  };

  const close = (restoreFocus = true) => {
    clearHoverTimers();
    focusFirstItemOnOpenRef.current = false;
    pendingSubmenuFocusRef.current = false;
    pendingMainFocusRef.current = null;
    setOpen(false);
    setHoveredItem(null);
    if (restoreFocus) triggerRef.current?.focus({ preventScroll: true });
  };

  const returnToMainMenu = () => {
    clearHoverTimers();
    pendingSubmenuFocusRef.current = false;
    pendingMainFocusRef.current = hoveredItem;
    setHoveredItem(null);
  };

  useLayoutEffect(() => {
    if (!open || hoveredItem || !pendingMainFocusRef.current) return;
    mainItemRefs.current.get(pendingMainFocusRef.current)?.focus({ preventScroll: true });
    pendingMainFocusRef.current = null;
  }, [open, hoveredItem]);

  const focusSubmenu = () => {
    pendingSubmenuFocusRef.current = true;
    window.requestAnimationFrame(() => {
      const firstItem = submenuRef.current?.querySelector<HTMLButtonElement>('.iam-submenu__scroll button[role="menuitem"]');
      if (firstItem) {
        focusCatalogItem(firstItem);
        pendingSubmenuFocusRef.current = false;
      } else {
        focusCatalogItem(submenuRef.current?.querySelector<HTMLButtonElement>('.iam-sub-back') ?? submenuRef.current);
      }
    });
  };

  useEffect(() => {
    if (!hoveredItem || catalogLoading || !pendingSubmenuFocusRef.current) return;
    const frame = window.requestAnimationFrame(() => {
      const firstItem = submenuRef.current?.querySelector<HTMLButtonElement>('.iam-submenu__scroll button[role="menuitem"]');
      focusCatalogItem(firstItem ?? submenuRef.current?.querySelector<HTMLButtonElement>('.iam-sub-back') ?? submenuRef.current);
      pendingSubmenuFocusRef.current = false;
    });
    return () => window.cancelAnimationFrame(frame);
  }, [catalogErrors, catalogLoading, connectors, experts, hoveredItem, skills]);

  const openSubmenu = (id: CatalogId, moveFocus = false) => {
    if (disabled) return;
    clearHoverTimers();
    setSubmenuTop(mainItemRefs.current.get(id)?.offsetTop ?? 0);
    setHoveredItem(id);
    if (moveFocus || submenuPanel) focusSubmenu();
  };

  const updateSubmenuPosition = useCallback(() => {
    const popover = popoverRef.current;
    if (!popover) return;
    const popoverRect = popover.getBoundingClientRect();
    const fontSize = Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    const submenu = submenuRef.current;
    const submenuWidth = submenu?.getBoundingClientRect().width || submenu?.offsetWidth
      || Math.min(SUBMENU_WIDTH_REM * fontSize, window.innerWidth - SUBMENU_VIEWPORT_INSET * 2);
    const roomOnRight = window.innerWidth
      - SUBMENU_VIEWPORT_INSET
      - popoverRect.right
      - SUBMENU_GAP;
    const roomOnLeft = popoverRect.left - SUBMENU_VIEWPORT_INSET - SUBMENU_GAP;
    const nextPanel = Math.max(roomOnRight, roomOnLeft) < submenuWidth;
    if (nextPanel && submenu && mainMenuRef.current?.contains(document.activeElement)) {
      pendingSubmenuFocusRef.current = true;
    }
    setSubmenuPanel(nextPanel);
    if (nextPanel && hoverTimerRef.current) {
      clearTimeout(hoverTimerRef.current);
      hoverTimerRef.current = null;
    }
    if (nextPanel && leaveTimerRef.current) {
      clearTimeout(leaveTimerRef.current);
      leaveTimerRef.current = null;
    }
    const nextSide: SubmenuSide = roomOnRight >= submenuWidth || roomOnRight >= roomOnLeft
      ? "right"
      : "left";
    setSubmenuSide((current) => current === nextSide ? current : nextSide);
    if (!hoveredItem || !isCatalogItem(hoveredItem) || !submenu) return;
    const anchor = mainItemRefs.current.get(hoveredItem);
    if (!anchor) return;
    const originTop = popoverRect.top + popover.clientTop;
    const submenuHeight = Math.max(submenu.offsetHeight, submenu.getBoundingClientRect().height);
    const preferredTop = nextPanel ? popover.clientHeight - submenuHeight : anchor.offsetTop;
    const minTop = SUBMENU_VIEWPORT_INSET - originTop;
    const maxTop = window.innerHeight - SUBMENU_VIEWPORT_INSET - originTop - submenuHeight;
    setSubmenuTop(Math.max(minTop, Math.min(preferredTop, Math.max(minTop, maxTop))));
    const nextLeft = nextPanel
      ? Math.max(
        SUBMENU_VIEWPORT_INSET,
        Math.min(popoverRect.left, window.innerWidth - SUBMENU_VIEWPORT_INSET - submenuWidth),
      ) - popoverRect.left - popover.clientLeft
      : null;
    setSubmenuLeft((current) => current === nextLeft ? current : nextLeft);
  }, [hoveredItem]);

  useLayoutEffect(() => {
    if (!open) return;

    updateSubmenuPosition();
    const frame = window.requestAnimationFrame(updateSubmenuPosition);
    window.addEventListener("resize", updateSubmenuPosition);
    window.addEventListener("scroll", updateSubmenuPosition, true);
    const observer = typeof ResizeObserver === "undefined"
      ? null
      : new ResizeObserver(updateSubmenuPosition);
    if (submenuRef.current) observer?.observe(submenuRef.current);
    if (popoverRef.current) observer?.observe(popoverRef.current);
    if (containerRef.current?.parentElement) observer?.observe(containerRef.current.parentElement);

    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("resize", updateSubmenuPosition);
      window.removeEventListener("scroll", updateSubmenuPosition, true);
      observer?.disconnect();
    };
  }, [open, catalogErrors, catalogLoading, connectors.length, experts.length, hoveredItem, skills.length, submenuPanel, updateSubmenuPosition]);

  useLayoutEffect(() => {
    // Resizing a flyout into a panel hides its parent; carry keyboard focus with it.
    const wasPanel = previousPanelRef.current;
    previousPanelRef.current = showingPanel;
    if (hoveredItem && (
      (showingPanel && (pendingSubmenuFocusRef.current || mainMenuRef.current?.contains(document.activeElement)))
      || (wasPanel && !showingPanel && document.activeElement === document.body)
    )) focusSubmenu();
  }, [showingPanel]);

  const handleItemClick = (id: MenuItemId, moveFocusToSubmenu = false) => {
    if (disabled && (!meetingHistoryOnly || id !== "meeting-minutes")) return;
    if (id === "add-files") {
      close();
      onPickFiles();
      return;
    }
    if (id === "meeting-minutes") {
      close(false);
      onOpenMeetingMinutes?.();
      return;
    }
    if (id === "browser-use" || id === "computer-use") {
      const nextMode: AutomationMode = id === "browser-use" ? "browser_use" : "computer_use";
      const capability = nextMode === "browser_use"
        ? automationCapabilities?.browser
        : automationCapabilities?.computer;
      if (automationModeDisabled || capability?.available === false) return;
      close();
      void onAutomationModeChange?.(automationMode === nextMode ? "default" : nextMode);
      return;
    }
    openSubmenu(id, moveFocusToSubmenu);
  };

  const handleTriggerClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    if (triggerDisabled) return;
    event.stopPropagation();
    clearHoverTimers();
    const openedFromKeyboard = event.detail === 0;
    setHoveredItem(null);
    setOpen((current) => {
      const willOpen = !current;
      focusFirstItemOnOpenRef.current = willOpen && openedFromKeyboard;
      return willOpen;
    });
  };

  const handleMainKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    item: MenuItem,
    index: number,
  ) => {
    const focusAt = (nextIndex: number) => {
      if (meetingHistoryOnly) {
        mainItemRefs.current.get("meeting-minutes")?.focus({ preventScroll: true });
        return;
      }
      const normalized = (nextIndex + flatItems.length) % flatItems.length;
      mainItemRefs.current.get(flatItems[normalized].id)?.focus({ preventScroll: true });
    };
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusAt(index + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusAt(index - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusAt(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusAt(flatItems.length - 1);
    } else if (event.key === "ArrowRight" && isCatalogItem(item.id)) {
      event.preventDefault();
      openSubmenu(item.id, true);
    } else if ((event.key === "Enter" || event.key === " ") && isCatalogItem(item.id)) {
      event.preventDefault();
      openSubmenu(item.id, true);
    }
  };

  const handleSubmenuKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const items = Array.from(
      submenuRef.current?.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]') ?? [],
    );
    const index = items.indexOf(document.activeElement as HTMLButtonElement);
    const focusAt = (nextIndex: number) => {
      if (items.length === 0) return;
      focusCatalogItem(items[(nextIndex + items.length) % items.length]);
    };
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusAt(index + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusAt(index - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusAt(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusAt(items.length - 1);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      returnToMainMenu();
    }
  };

  const handleSelectExpert = (agent: AgentEntry) => {
    if (disabled) return;
    close();
    onSelectExpert?.(agent);
  };

  const handleSelectSkill = (name: string) => {
    if (disabled) return;
    close();
    onSelectSkill?.(name);
  };

  const handleSelectConnector = () => {
    if (disabled) return;
    close();
    onNavigateConnectors?.();
  };

  const renderCatalogState = (id: CatalogId, hasItems: boolean, emptyLabel: string) => {
    const error = catalogErrors[id];
    if (error) {
      return (
        <div className="iam-sub-state iam-sub-state--error" role="none">
          <span role="alert">{error}，请重试。</span>
          <button type="button" className="iam-sub-retry" role="menuitem" onClick={() => {
            pendingSubmenuFocusRef.current = true;
            void loadData();
          }}>
            重试
          </button>
        </div>
      );
    }
    if (!hasItems && catalogLoading) {
      return <div className="iam-sub-empty" role="status">正在加载…</div>;
    }
    if (!hasItems) return <div className="iam-sub-empty">{emptyLabel}</div>;
    return null;
  };

  const renderSubmenu = () => {
    if (disabled || !hoveredItem) return null;

    let items: React.ReactNode = null;

    if (hoveredItem === "experts") {
      items = (
        <>
          {renderCatalogState("experts", experts.length > 0, "暂无已安装专家")}
          {experts.map((e) => (
          <button
            key={e.path || e.name}
            type="button"
            className="iam-sub-item"
            role="menuitem"
            onClick={() => handleSelectExpert(e)}
          >
            <span className="iam-sub-avatar" aria-hidden="true">{(e.name || "?")[0]}</span>
            <span className="iam-sub-text">
              <span className="iam-sub-name">{e.name}</span>
              {e.description && <span className="iam-sub-desc">{e.description.slice(0, 40)}</span>}
            </span>
          </button>
          ))}
        </>
      );
    }

    if (hoveredItem === "skills") {
      items = (
        <>
          {renderCatalogState("skills", skills.length > 0, "暂无已启用技能")}
          {skills.map((s) => (
          <button
            key={s.name}
            type="button"
            className="iam-sub-item"
            role="menuitem"
            onClick={() => handleSelectSkill(s.name)}
          >
            <SkillTabIcon size="sm" />
            <span className="iam-sub-text">
              <span className="iam-sub-name">{s.displayName || s.name}</span>
              {s.description && <span className="iam-sub-desc">{s.description.slice(0, 40)}</span>}
            </span>
          </button>
          ))}
        </>
      );
    }

    if (hoveredItem === "connectors") {
      items = (
        <>
          {renderCatalogState("connectors", connectors.length > 0, "暂无已启用连接器")}
          {connectors.slice(0, 8).map((server) => (
              <button
                key={server.name}
                type="button"
                className="iam-sub-item"
                role="menuitem"
                onClick={handleSelectConnector}
              >
                <ConnectorTabIcon size="sm" />
                <span className="iam-sub-text">
                  <span className="iam-sub-name">{server.name}</span>
                  {server.target && <span className="iam-sub-desc">{server.target}</span>}
                </span>
              </button>
            ))}
        </>
      );
    }

    if (!items) return null;

    return (
      <div
        className={`iam-submenu iam-submenu--${submenuSide}${submenuPanel ? " iam-submenu--panel" : ""}`}
        style={{
          "--iam-submenu-top": `${submenuTop}px`,
          left: submenuLeft ?? undefined,
          right: submenuLeft === null ? undefined : "auto",
        } as SubmenuStyle}
        role="menu"
        tabIndex={-1}
        aria-label={`${flatItems.find((item) => item.id === hoveredItem)?.label ?? "添加"}子菜单`}
        ref={submenuRef}
        onKeyDown={handleSubmenuKeyDown}
        onMouseEnter={handleSubmenuEnter}
        onMouseLeave={handleSubmenuLeave}
      >
        {submenuPanel && <div className="iam-submenu__header">
          <button type="button" className="iam-sub-back" role="menuitem" aria-label="返回添加菜单" onClick={returnToMainMenu}>
            <ArrowLeft size={16} />返回
          </button>
          <span className="iam-submenu__title">{flatItems.find((item) => item.id === hoveredItem)?.label}</span>
        </div>}
        <div className="iam-submenu__scroll">{items}</div>
      </div>
    );
  };

  return (
    <div className="iam-wrap" ref={containerRef} style={{ "--iam-catalog-width": `${SUBMENU_WIDTH_REM}rem` } as CSSProperties}
      onKeyDown={(event) => {
        // Continue normal Tab navigation from the trigger after dismissing the menu.
        if (event.key === "Tab") close();
      }}>
      <button
        className="echo-composer__add"
        onClick={handleTriggerClick}
        disabled={triggerDisabled}
        aria-label="添加"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? "composer-add-menu" : undefined}
        title="添加文件、工具或扩展能力"
        ref={triggerRef}
      >
        <AddIcon size="md" />
      </button>

      {open && !triggerDisabled && (
        <div
          className={`iam-popover${showingPanel ? " iam-popover--catalog" : ""}`}
          id="composer-add-menu"
          role={showingPanel ? "presentation" : "menu"}
          aria-label={showingPanel ? undefined : "添加内容"}
          ref={popoverRef}
          onMouseDown={(event) => {
            if (event.button !== 0) return;
            const button = event.target instanceof Element ? event.target.closest("button") : null;
            if (!button || button.disabled) return;
            // Native pointer focus scrolls clipped composer ancestors too.
            // Keep focus visible without moving the workbench before click.
            event.preventDefault();
            button.focus({ preventScroll: true });
          }}
        >
          <div className="iam-main-menu" ref={mainMenuRef} aria-hidden={showingPanel || undefined}>
          {menuGroups.map((group, gi) => (
            <div key={gi}>
              {gi > 0 && <div className="iam-divider" />}
              <div className="iam-group">
                {group.map((item) => {
                  const itemIndex = flatItems.findIndex((candidate) => candidate.id === item.id);
                  const mode = item.id === "browser-use"
                    ? "browser_use"
                    : item.id === "computer-use"
                      ? "computer_use"
                      : null;
                  const capability = mode === "browser_use"
                    ? automationCapabilities?.browser
                    : mode === "computer_use"
                      ? automationCapabilities?.computer
                      : null;
                  const modeUnavailable = Boolean(mode) && (
                    automationModeDisabled || capability?.available === false
                  );
                  const historyRestricted = meetingHistoryOnly && item.id !== "meeting-minutes";
                  const selected = Boolean(mode) && automationMode === mode;
                  const description = capability?.available === false
                    ? capability.reason || "当前设备不可用"
                    : item.description;
                  return (
                  <button
                    type="button"
                    key={item.id}
                    className={
                      "iam-item"
                      + (hoveredItem === item.id ? " iam-item--active" : "")
                      + (selected ? " iam-item--selected" : "")
                      + (modeUnavailable || historyRestricted ? " iam-item--disabled" : "")
                    }
                    onMouseEnter={() => handleItemEnter(item.id)}
                    onMouseLeave={handleItemLeave}
                    onClick={(event) => handleItemClick(item.id, event.detail === 0)}
                    disabled={historyRestricted}
                    role={mode ? "menuitemradio" : "menuitem"}
                    aria-checked={mode ? selected : undefined}
                    aria-disabled={modeUnavailable || historyRestricted || undefined}
                    aria-haspopup={isCatalogItem(item.id) ? "menu" : undefined}
                    aria-expanded={isCatalogItem(item.id) ? hoveredItem === item.id : undefined}
                    data-tip={description}
                    aria-description={description}
                    ref={(element) => {
                      if (element) mainItemRefs.current.set(item.id, element);
                      else mainItemRefs.current.delete(item.id);
                    }}
                    onKeyDown={(event) => handleMainKeyDown(event, item, itemIndex)}
                  >
                    <span className="iam-item__icon">{item.icon}</span>
                    <span className="iam-item__label">{item.label}</span>
                    {selected && <Check className="iam-item__check" size={15} strokeWidth={2} />}
                    {isCatalogItem(item.id) && (
                      <span className="iam-item__chevron">
                        <ChevronRight size={14} strokeWidth={1.5} />
                      </span>
                    )}
                  </button>
                  );
                })}
              </div>
            </div>
          ))}
          </div>
          {renderSubmenu()}
        </div>
      )}
    </div>
  );
}
