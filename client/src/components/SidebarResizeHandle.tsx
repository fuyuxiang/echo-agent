import { useEffect, useId, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { SIDEBAR_COLLAPSE_THRESHOLD, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MIN_WIDTH, type useAppSidebarLayout } from "@/lib/sidebar-layout";

type Layout = ReturnType<typeof useAppSidebarLayout>;
type Drag = { pointerId: number; startX: number; origin: number; raw: number; collapsed: boolean; moved: boolean };

export function SidebarResizeHandle({ layout }: { layout: Layout }) {
  const helpId = useId();
  const handleRef = useRef<HTMLDivElement>(null);
  const layoutRef = useRef(layout);
  layoutRef.current = layout;
  const dragRef = useRef<Drag | null>(null);
  const frameRef = useRef<number | null>(null);
  const [hint, setHint] = useState("");
  const [ghostWidth, setGhostWidth] = useState<number | null>(null);

  const finishRef = useRef<(commit: boolean) => void>(() => {});
  finishRef.current = (commit) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
    frameRef.current = null;
    const current = layoutRef.current;
    current.setPreviewWidth(null);
    current.setResizing(false);
    setHint(""); setGhostWidth(null);
    if (handleRef.current?.hasPointerCapture?.(drag.pointerId)) handleRef.current.releasePointerCapture(drag.pointerId);
    if (!commit || !drag.moved) return;
    if (drag.collapsed) {
      if (drag.raw < 40) return;
      if (drag.raw >= SIDEBAR_MIN_WIDTH) current.commitWidth(Math.min(drag.raw, current.restoreMaxWidth));
      current.setCollapsed(false);
    } else if (drag.raw < SIDEBAR_COLLAPSE_THRESHOLD) {
      current.setCollapsed(true);
    } else {
      current.commitWidth(Math.min(drag.raw, current.maxWidth));
    }
  };

  useEffect(() => {
    const move = (event: globalThis.PointerEvent) => {
      const drag = dragRef.current;
      if (!drag || event.pointerId !== drag.pointerId) return;
      drag.raw = drag.origin + event.clientX - drag.startX;
      drag.moved ||= Math.abs(event.clientX - drag.startX) > 3;
      if (frameRef.current !== null) return;
      frameRef.current = requestAnimationFrame(() => {
        frameRef.current = null;
        if (dragRef.current !== drag) return;
        const current = layoutRef.current;
        if (drag.collapsed) {
          setGhostWidth(Math.max(0, Math.min(drag.raw, current.restoreMaxWidth)));
          setHint(drag.raw >= 40 ? "松开展开侧边栏" : "向右拖动展开侧边栏");
        } else {
          current.setPreviewWidth(drag.raw);
          setHint(drag.raw < SIDEBAR_COLLAPSE_THRESHOLD ? "松开收起侧边栏" : "");
        }
      });
    };
    const end = (event: globalThis.PointerEvent) => {
      const drag = dragRef.current;
      if (!drag || event.pointerId !== drag.pointerId) return;
      if (event.type === "pointerup") {
        drag.raw = drag.origin + event.clientX - drag.startX;
        drag.moved ||= Math.abs(event.clientX - drag.startX) > 3;
      }
      finishRef.current(event.type === "pointerup");
    };
    const cancel = () => finishRef.current(false);
    const key = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape" && dragRef.current) { event.preventDefault(); event.stopPropagation(); cancel(); }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
    window.addEventListener("blur", cancel);
    window.addEventListener("keydown", key, true);
    return () => {
      cancel();
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", key, true);
    };
  }, []);

  const begin = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || event.isPrimary === false || dragRef.current) return;
    event.preventDefault(); event.stopPropagation();
    event.currentTarget.focus({ preventScroll: true });
    dragRef.current = { pointerId: event.pointerId, startX: event.clientX, origin: layout.collapsed ? 0 : layout.width,
      raw: layout.collapsed ? 0 : layout.width, collapsed: layout.collapsed, moved: false };
    layout.setResizing(true);
    try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* Window listeners also support older webviews. */ }
  };
  const reset = () => { finishRef.current(false); layout.commitWidth(SIDEBAR_DEFAULT_WIDTH); layout.setCollapsed(false); };
  const keyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (dragRef.current || event.altKey || event.ctrlKey || event.metaKey) return;
    const step = event.shiftKey ? 40 : 16;
    if (event.key === "Enter" || event.key === " ") layout.setCollapsed((value) => !value);
    else if (event.key === "Home") reset();
    else if (event.key === "End") { layout.commitWidth(layout.collapsed ? layout.restoreMaxWidth : layout.maxWidth); layout.setCollapsed(false); }
    else if (event.key === "ArrowRight") {
      if (layout.collapsed) layout.setCollapsed(false);
      else if (layout.width < layout.maxWidth) layout.commitWidth(Math.min(layout.width + step, layout.maxWidth));
    } else if (event.key === "ArrowLeft") {
      if (!layout.collapsed && layout.width <= SIDEBAR_MIN_WIDTH) layout.setCollapsed(true);
      else if (!layout.collapsed) layout.commitWidth(Math.max(SIDEBAR_MIN_WIDTH, layout.width - step));
    } else return;
    event.preventDefault(); event.stopPropagation();
  };

  return <>
    <div ref={handleRef} className={"sidebar-resize" + (layout.resizing ? " sidebar-resize--active" : "")}
      style={{ left: Math.max(0, (ghostWidth ?? (layout.collapsed ? 0 : layout.width)) - 4) }}
      role="separator" aria-orientation="vertical" aria-label="调整侧边栏宽度" aria-controls="app-sidebar"
      aria-describedby={helpId} aria-valuemin={layout.collapsed ? 0 : SIDEBAR_MIN_WIDTH}
      aria-valuemax={layout.collapsed ? layout.restoreMaxWidth : layout.maxWidth}
      aria-valuenow={layout.collapsed ? 0 : Math.round(layout.width)}
      aria-valuetext={layout.collapsed ? "侧边栏已收起" : `${Math.round(layout.width)} 像素`}
      tabIndex={0} data-sidebar-resize data-tip={layout.collapsed ? "向右拖动展开侧边栏" : "拖动调整宽度，双击恢复默认"}
      onPointerDown={begin} onLostPointerCapture={(event) => {
        if (event.pointerId === dragRef.current?.pointerId) finishRef.current(false);
      }} onDoubleClick={reset} onKeyDown={keyDown}>
      {hint && <span className="sidebar-resize__hint" role="status">{hint}</span>}
    </div>
    <span id={helpId} className="sr-only">左右方向键调整宽度，Enter 收起或展开，Home 恢复默认，Escape 取消拖动。</span>
  </>;
}
