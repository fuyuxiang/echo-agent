import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type RefObject } from "react";
import type { SidebarMenuGroup, SidebarMenuId } from "./sidebar-menu";

type DropTarget = { group: SidebarMenuGroup; index: number };
type Preview = { id: SidebarMenuId; left: number; top: number; width: number; height: number };
type Drag = {
  id: SidebarMenuId; pointerId: number; handle: HTMLButtonElement;
  startX: number; startY: number; x: number; y: number;
  offsetX: number; offsetY: number; width: number; height: number; moved: boolean;
};

/** Internal menu sorting must not use HTML5 DnD: Tauri's native file-drop
 * handler consumes that path on desktop. Pointer capture leaves file drops intact. */
export function useSidebarMenuDrag(
  bodyRef: RefObject<HTMLDivElement>,
  onMove: (id: SidebarMenuId, group: SidebarMenuGroup, index: number) => void,
) {
  const [preview, setPreview] = useState<Preview | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);
  const dragRef = useRef<Drag | null>(null);
  const frameRef = useRef<number | null>(null);
  const onMoveRef = useRef(onMove);
  onMoveRef.current = onMove;
  const suppressClick = useRef(false);
  const clickTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const locate = (x: number, y: number): DropTarget | null => {
    const body = bodyRef.current;
    if (!body) return null;
    const bounds = body.getBoundingClientRect();
    if (x < bounds.left || x > bounds.right || y < bounds.top || y > bounds.bottom) return null;
    for (const section of body.querySelectorAll<HTMLElement>("[data-menu-group]")) {
      const rect = section.getBoundingClientRect();
      if (x < rect.left || x > rect.right || y < rect.top || y > rect.bottom) continue;
      const rows = [...section.querySelectorAll<HTMLElement>("[data-menu-id]")];
      const before = rows.findIndex(row => {
        const rect = row.getBoundingClientRect();
        return y < rect.top + rect.height / 2;
      });
      return { group: section.dataset.menuGroup as SidebarMenuGroup, index: before < 0 ? rows.length : before };
    }
    return null;
  };

  const finishRef = useRef<(commit: boolean, x?: number, y?: number) => void>(() => {});
  finishRef.current = (commit, x, y) => {
    const drag = dragRef.current;
    if (!drag) return;
    const target = commit && drag.moved ? locate(x ?? drag.x, y ?? drag.y) : null;
    dragRef.current = null;
    if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
    frameRef.current = null;
    if (drag.handle.hasPointerCapture?.(drag.pointerId)) drag.handle.releasePointerCapture(drag.pointerId);
    setPreview(null);
    setDropTarget(null);
    if (drag.moved) {
      // Reparenting a row on release must not turn the gesture into an overlay click.
      suppressClick.current = true;
      if (clickTimer.current !== null) clearTimeout(clickTimer.current);
      clickTimer.current = setTimeout(() => { suppressClick.current = false; clickTimer.current = null; }, 0);
    }
    if (target) onMoveRef.current(drag.id, target.group, target.index);
  };

  useEffect(() => {
    const frame = () => {
      frameRef.current = null;
      const drag = dragRef.current;
      const body = bodyRef.current;
      if (!drag?.moved || !body) return;
      const bounds = body.getBoundingClientRect();
      if (drag.x >= bounds.left && drag.x <= bounds.right) {
        const edge = 32;
        const speed = drag.y < bounds.top + edge ? -Math.min(14, (bounds.top + edge - drag.y) / 2)
          : drag.y > bounds.bottom - edge ? Math.min(14, (drag.y - bounds.bottom + edge) / 2) : 0;
        if (speed) body.scrollTop += speed;
      }
      const next = locate(drag.x, drag.y);
      setDropTarget(current => current?.group === next?.group && current?.index === next?.index ? current : next);
      setPreview(current => {
        const left = drag.x - drag.offsetX, top = drag.y - drag.offsetY;
        return current?.left === left && current?.top === top ? current
          : { id: drag.id, left, top, width: drag.width, height: drag.height };
      });
      frameRef.current = requestAnimationFrame(frame);
    };
    const move = (event: PointerEvent) => {
      const drag = dragRef.current;
      if (!drag || event.pointerId !== drag.pointerId) return;
      drag.x = event.clientX;
      drag.y = event.clientY;
      drag.moved ||= Math.hypot(drag.x - drag.startX, drag.y - drag.startY) >= 4;
      if (!drag.moved) return;
      event.preventDefault();
      if (frameRef.current === null) frameRef.current = requestAnimationFrame(frame);
    };
    const end = (event: PointerEvent) => {
      if (event.pointerId !== dragRef.current?.pointerId) return;
      finishRef.current(event.type === "pointerup", event.clientX, event.clientY);
    };
    const cancel = () => finishRef.current(false);
    const key = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || !dragRef.current) return;
      event.preventDefault();
      event.stopPropagation();
      finishRef.current(false);
    };
    const click = (event: MouseEvent) => {
      if (!suppressClick.current) return;
      suppressClick.current = false;
      event.preventDefault();
      event.stopPropagation();
    };
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
    window.addEventListener("blur", cancel);
    window.addEventListener("keydown", key, true);
    window.addEventListener("click", click, true);
    return () => {
      cancel();
      if (clickTimer.current !== null) clearTimeout(clickTimer.current);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("click", click, true);
    };
  }, [bodyRef]);

  const begin = (event: ReactPointerEvent<HTMLButtonElement>, id: SidebarMenuId) => {
    if (event.button !== 0 || event.isPrimary === false || dragRef.current) return;
    const row = event.currentTarget.closest<HTMLElement>("[data-menu-id]");
    if (!row) return;
    const bounds = row.getBoundingClientRect();
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.focus({ preventScroll: true });
    dragRef.current = {
      id, pointerId: event.pointerId, handle: event.currentTarget,
      startX: event.clientX, startY: event.clientY, x: event.clientX, y: event.clientY,
      offsetX: event.clientX - bounds.left, offsetY: event.clientY - bounds.top,
      width: bounds.width, height: bounds.height, moved: false,
    };
    try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* Window listeners cover older WebViews. */ }
  };

  const lostCapture = (pointerId: number) => {
    if (pointerId === dragRef.current?.pointerId) finishRef.current(false);
  };
  return { preview, dropTarget, begin, lostCapture };
}
