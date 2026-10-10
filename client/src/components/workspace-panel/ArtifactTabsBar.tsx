/**
 * 统一标签栏 —— 对齐 EchoAgent `agent-sidebar-ui/ArtifactTabsBar`。
 *
 * 特性：
 *  - 按标签 kind 显示图标（file → 文件 emoji，preview → 🌐，artifact/changes → 文件 emoji）
 *  - 点击切换激活标签
 *  - 关闭按钮（×）
 *  - 指针拖拽排序（threshold 4px，FLIP 动画过渡）
 *  - 跟随激活标签滚入视野
 */
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import type { UnifiedTab } from "@/lib/use-unified-tabs";
import { pickFileEmoji } from "./file-tab-icon";
import { IS_MACOS } from "@/lib/platform";
import { Globe } from "lucide-react";

const DRAG_START_THRESHOLD = 4;

function clamp(value: number, min: number, max: number): number {
  if (max < min) return min;
  return Math.min(Math.max(value, min), max);
}

/** 按 tab kind / label 选图标（SP2 起全部走 SVG）。 */
function pickTabIcon(tab: UnifiedTab): React.ReactNode {
  if (tab.kind === "preview") return <Globe size={12} aria-hidden />;
  // file / artifact / changes：用文件名后缀映射。
  const name =
    tab.kind === "file"
      ? (tab.filePath ?? tab.label)
      : tab.kind === "artifact"
        ? (tab.subtitle ?? tab.label)
        : tab.label;
  return pickFileEmoji(name);
}

function orderTabs(tabs: UnifiedTab[], orderedIds: string[] | null): UnifiedTab[] {
  if (!orderedIds) return tabs;
  const byId = new Map(tabs.map((t) => [t.id, t]));
  const ordered: UnifiedTab[] = [];
  orderedIds.forEach((id) => {
    const t = byId.get(id);
    if (t) ordered.push(t);
  });
  tabs.forEach((t) => {
    if (!ordered.some((x) => x.id === t.id)) ordered.push(t);
  });
  return ordered;
}

function moveIdByPointer(
  order: string[],
  draggedId: string,
  pointerX: number,
  tabRefs: Map<string, HTMLElement>,
): string[] {
  const withoutDragged = order.filter((id) => id !== draggedId);
  let insertIndex = withoutDragged.length;
  for (let i = 0; i < withoutDragged.length; i += 1) {
    const node = tabRefs.get(withoutDragged[i]);
    if (!node) continue;
    const rect = node.getBoundingClientRect();
    if (pointerX < rect.left + rect.width / 2) {
      insertIndex = i;
      break;
    }
  }
  const next = [...withoutDragged];
  next.splice(insertIndex, 0, draggedId);
  return next;
}

interface DragState {
  id: string;
  startX: number;
  startY: number;
  offsetX: number;
  top: number;
  width: number;
  height: number;
  minLeft: number;
  maxLeft: number;
  hasStarted: boolean;
  initialOrder: string[];
}

interface DragSnapshot {
  id: string;
  left: number;
  top: number;
  width: number;
  height: number;
}

export function ArtifactTabsBar({
  tabs,
  activeTabId,
  onSelect,
  onClose,
  onReorder,
}: {
  tabs: UnifiedTab[];
  activeTabId?: string;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onReorder?: (orderedIds: string[]) => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef(new Map<string, HTMLElement>());
  const dragStateRef = useRef<DragState | null>(null);
  const visualOrderRef = useRef<string[] | null>(null);
  const previousRectsRef = useRef<Map<string, DOMRect> | null>(null);
  const removeDragListenersRef = useRef<null | (() => void)>(null);
  const suppressNextClickRef = useRef(false);
  const [visualOrder, setVisualOrder] = useState<string[] | null>(null);
  const [dragSnapshot, setDragSnapshot] = useState<DragSnapshot | null>(null);

  const tabsById = useMemo(
    () => new Map(tabs.map((t) => [t.id, t])),
    [tabs],
  );
  const renderedTabs = useMemo(
    () => orderTabs(tabs, visualOrder),
    [tabs, visualOrder],
  );
  const draggedTab = dragSnapshot ? tabsById.get(dragSnapshot.id) : undefined;
  const activeTabIdForRender = visualOrder
    ? (dragStateRef.current?.id ?? activeTabId)
    : activeTabId;

  // 无拖拽时清理 visual order。
  useEffect(() => {
    if (!dragStateRef.current) {
      setVisualOrder(null);
      visualOrderRef.current = null;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tabs]);

  // 激活标签滚入视野。
  useEffect(() => {
    if (!activeTabId || dragStateRef.current) return;
    const node = tabRefs.current.get(activeTabId);
    if (!node) return;
    node.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [activeTabId, tabs]);

  const setOrderedIds = useCallback((nextOrder: string[]) => {
    const current = visualOrderRef.current;
    if (
      current &&
      current.length === nextOrder.length &&
      current.every((id, i) => id === nextOrder[i])
    )
      return;
    previousRectsRef.current = new Map();
    tabRefs.current.forEach((node, id) => {
      previousRectsRef.current?.set(id, node.getBoundingClientRect());
    });
    visualOrderRef.current = nextOrder;
    setVisualOrder(nextOrder);
  }, []);

  // FLIP 动画：记录旧位置 → 重排 → 计算位移 → 过渡归零。
  useLayoutEffect(() => {
    const previousRects = previousRectsRef.current;
    previousRectsRef.current = null;
    if (!previousRects || typeof window === "undefined") return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const draggedId = dragStateRef.current?.id;
    const frameIds: number[] = [];
    tabRefs.current.forEach((node, id) => {
      if (id === draggedId) return;
      const prev = previousRects.get(id);
      if (!prev) return;
      const cur = node.getBoundingClientRect();
      const deltaX = prev.left - cur.left;
      if (Math.abs(deltaX) < 0.5) return;
      node.style.transition = "none";
      node.style.transform = `translate3d(${deltaX}px, 0, 0)`;
      frameIds.push(
        window.requestAnimationFrame(() => {
          node.style.transition = "transform 160ms cubic-bezier(0.2, 0, 0, 1)";
          node.style.transform = "";
        }),
      );
    });
    return () => {
      frameIds.forEach((fid) => window.cancelAnimationFrame(fid));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visualOrder]);

  const registerTabRef = useCallback((id: string, node: HTMLElement | null) => {
    if (node) tabRefs.current.set(id, node);
    else tabRefs.current.delete(id);
  }, []);

  const handleSelect = useCallback(
    (id: string) => {
      if (suppressNextClickRef.current) {
        suppressNextClickRef.current = false;
        return;
      }
      onSelect(id);
    },
    [onSelect],
  );

  const handleClose = useCallback(
    (e: React.MouseEvent, id: string) => {
      e.stopPropagation();
      const restoreKeyboardFocus = e.detail === 0 && document.activeElement === e.currentTarget;
      const index = renderedTabs.findIndex((tab) => tab.id === id);
      const next = id === activeTabIdForRender
        ? renderedTabs[index + 1] ?? renderedTabs[index - 1]
        : renderedTabs.find((tab) => tab.id === activeTabIdForRender);
      onClose(id);
      if (restoreKeyboardFocus && next) {
        if (id === activeTabIdForRender) onSelect(next.id);
        tabRefs.current.get(next.id)?.focus();
      }
    },
    [onClose, onSelect, renderedTabs, activeTabIdForRender],
  );

  const handleTabKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>, id: string) => {
    // A close button is a separate control; its keyboard event must not select a tab.
    if (event.target !== event.currentTarget) return;
    const index = renderedTabs.findIndex((tab) => tab.id === id);
    let nextIndex: number | undefined;
    if (event.key === "ArrowRight") nextIndex = (index + 1) % renderedTabs.length;
    else if (event.key === "ArrowLeft") nextIndex = (index - 1 + renderedTabs.length) % renderedTabs.length;
    else if (event.key === "Home") nextIndex = 0;
    else if (event.key === "End") nextIndex = renderedTabs.length - 1;
    else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      handleSelect(id);
      return;
    } else if (event.key === "Delete") {
      event.preventDefault();
      const adjacent = renderedTabs[index + 1] ?? renderedTabs[index - 1];
      onClose(id);
      if (adjacent) {
        onSelect(adjacent.id);
        tabRefs.current.get(adjacent.id)?.focus();
      }
      return;
    }
    if (nextIndex !== undefined) {
      event.preventDefault();
      const next = renderedTabs[nextIndex];
      handleSelect(next.id);
      tabRefs.current.get(next.id)?.focus();
    }
  };

  const clearDrag = useCallback(() => {
    dragStateRef.current = null;
    setDragSnapshot(null);
  }, []);

  const removeDragListeners = useCallback(() => {
    removeDragListenersRef.current?.();
    removeDragListenersRef.current = null;
  }, []);

  const finishDrag = useCallback(() => {
    const ds = dragStateRef.current;
    if (!ds) return;
    if (ds.hasStarted) {
      const finalOrder = visualOrderRef.current ?? ds.initialOrder;
      const changed =
        finalOrder.length !== ds.initialOrder.length ||
        finalOrder.some((id, i) => id !== ds.initialOrder[i]);
      onReorder?.(finalOrder);
      if (!changed) {
        visualOrderRef.current = null;
        setVisualOrder(null);
      }
      onSelect(ds.id);
      window.setTimeout(() => {
        suppressNextClickRef.current = false;
      }, 0);
    }
    removeDragListeners();
    clearDrag();
  }, [clearDrag, onReorder, onSelect, removeDragListeners]);

  const beginDragIfNeeded = useCallback((clientX: number, clientY: number) => {
    const ds = dragStateRef.current;
    if (!ds || ds.hasStarted) return ds?.hasStarted ?? false;
    const dx = clientX - ds.startX;
    const dy = clientY - ds.startY;
    if (Math.hypot(dx, dy) < DRAG_START_THRESHOLD) return false;
    ds.hasStarted = true;
    suppressNextClickRef.current = true;
    visualOrderRef.current = ds.initialOrder;
    setVisualOrder(ds.initialOrder);
    return true;
  }, []);

  const updateDrag = useCallback(
    (clientX: number, clientY: number) => {
      const ds = dragStateRef.current;
      if (!ds) return false;
      if (!beginDragIfNeeded(clientX, clientY)) return false;
      setDragSnapshot({
        id: ds.id,
        left: clamp(clientX - ds.offsetX, ds.minLeft, ds.maxLeft),
        top: ds.top,
        width: ds.width,
        height: ds.height,
      });
      setOrderedIds(
        moveIdByPointer(
          visualOrderRef.current ?? ds.initialOrder,
          ds.id,
          clientX,
          tabRefs.current,
        ),
      );
      return true;
    },
    [beginDragIfNeeded, setOrderedIds],
  );

  const handlePointerDown = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>, id: string) => {
      if (!onReorder || tabs.length < 2 || e.button !== 0) return;
      if ((e.target as HTMLElement).closest(".artifact-tab__close")) return;
      const rect = e.currentTarget.getBoundingClientRect();
      const listRect = listRef.current?.getBoundingClientRect();
      const listStyle = listRef.current
        ? window.getComputedStyle(listRef.current)
        : null;
      const paddingLeft = listStyle ? Number.parseFloat(listStyle.paddingLeft) || 0 : 0;
      const paddingRight = listStyle ? Number.parseFloat(listStyle.paddingRight) || 0 : 0;
      const minLeft = (listRect?.left ?? rect.left) + paddingLeft;
      const maxLeft = (listRect?.right ?? rect.right) - paddingRight - rect.width;
      dragStateRef.current = {
        id,
        startX: e.clientX,
        startY: e.clientY,
        offsetX: e.clientX - rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        minLeft,
        maxLeft,
        hasStarted: false,
        initialOrder: tabs.map((t) => t.id),
      };
      removeDragListeners();
      const pointerId = e.pointerId;
      const onMove = (ev: PointerEvent) => {
        if (ev.pointerId !== pointerId) return;
        if (updateDrag(ev.clientX, ev.clientY)) ev.preventDefault();
      };
      const onEnd = (ev: PointerEvent) => {
        if (ev.pointerId !== pointerId) return;
        finishDrag();
      };
      window.addEventListener("pointermove", onMove, { passive: false });
      window.addEventListener("pointerup", onEnd);
      window.addEventListener("pointercancel", onEnd);
      removeDragListenersRef.current = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onEnd);
        window.removeEventListener("pointercancel", onEnd);
      };
    },
    [finishDrag, onReorder, removeDragListeners, tabs, updateDrag],
  );

  // 卸载时移除拖拽监听。
  useEffect(() => () => removeDragListeners(), [removeDragListeners]);

  if (tabs.length === 0) return null;

  return (
    // macOS：标签条贴窗口顶边（Overlay 标题栏），容器与最后一个标签右侧的
    // 空白区标记为拖拽区（拖动窗口 / 双击缩放）。tab 子元素不是拖拽目标，
    // 点击切换 / 指针拖拽排序不受影响。
    <div
      className="artifact-tabs"
      role="tablist"
      aria-label="工作区文件和预览标签"
      {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})}
    >
      <div
        ref={listRef}
        className={
          "artifact-tabs__list" +
          (visualOrder ? " artifact-tabs__list--dragging" : "")
        }
        {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})}
      >
        {renderedTabs.map((tab) => (
          <div
            key={tab.id}
            ref={(node) => registerTabRef(tab.id, node)}
            className={
              "artifact-tab" +
              (tab.id === activeTabIdForRender ? " artifact-tab--active" : "") +
              (onReorder && tabs.length > 1 ? " artifact-tab--draggable" : "") +
              (dragSnapshot?.id === tab.id ? " artifact-tab--drag-placeholder" : "")
            }
            onClick={() => handleSelect(tab.id)}
            onPointerDown={(e) => handlePointerDown(e, tab.id)}
            onKeyDown={(e) => handleTabKeyDown(e, tab.id)}
            role="tab"
            aria-selected={tab.id === activeTabIdForRender}
            tabIndex={tab.id === (activeTabIdForRender ?? renderedTabs[0]?.id) ? 0 : -1}
            title={tab.subtitle ?? tab.label}
          >
            <span className="artifact-tab__icon">{pickTabIcon(tab)}</span>
            <span className="artifact-tab__name">{tab.label}</span>
            <button
              type="button"
              className="artifact-tab__close"
              onClick={(e) => handleClose(e, tab.id)}
              onMouseDown={(e) => e.stopPropagation()}
              aria-label={`关闭标签 ${tab.label}`}
            >
              ×
            </button>
          </div>
        ))}
      </div>
      {dragSnapshot && draggedTab &&
        createPortal(
          <div
            className={
              "artifact-tab artifact-tab--drag-clone" +
              (draggedTab.id === activeTabIdForRender
                ? " artifact-tab--active"
                : "")
            }
            style={{
              width: dragSnapshot.width,
              height: dragSnapshot.height,
              transform: `translate3d(${dragSnapshot.left}px, ${dragSnapshot.top}px, 0)`,
            }}
          >
            <span className="artifact-tab__icon">{pickTabIcon(draggedTab)}</span>
            <span className="artifact-tab__name">{draggedTab.label}</span>
          </div>,
          document.body,
        )}
    </div>
  );
}
