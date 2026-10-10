import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type MouseEvent, type PointerEvent, type RefObject } from "react";

export type ImageSize = { width: number; height: number };
type Point = { x: number; y: number };
type Geometry = { stage: ImageSize; image: ImageSize; scale: number };
const PADDING = 24;
const MAX_ZOOM = 4;
const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(max, value));

export function validImageSize(size?: ImageSize | null): size is ImageSize {
  return !!size && Number.isFinite(size.width) && Number.isFinite(size.height) && size.width > 0 && size.height > 0;
}

function imageOffset(stage: ImageSize, image: ImageSize, scale: number): Point {
  return { x: Math.max(PADDING, (stage.width - image.width * scale) / 2), y: Math.max(PADDING, (stage.height - image.height * scale) / 2) };
}

function boundedScroll(point: Point, { stage, image, scale }: Geometry): Point {
  return {
    x: clamp(point.x, 0, Math.max(0, image.width * scale + PADDING * 2 - stage.width)),
    y: clamp(point.y, 0, Math.max(0, image.height * scale + PADDING * 2 - stage.height)),
  };
}

/** Keeps native scrollbars while adding pointer-anchored zoom and bounded pan. */
export function useImagePreviewViewport(stageRef: RefObject<HTMLDivElement>, image: ImageSize | null) {
  const [stageSize, setStageSize] = useState<ImageSize>({ width: 0, height: 0 });
  const [zoom, setZoom] = useState<number | null>(null);
  const [wheelPan, setWheelPan] = useState(false);
  const [dragging, setDragging] = useState(false);
  const zoomRef = useRef<number | null>(null);
  const pendingScroll = useRef<Point | null>(null);
  const previousGeometry = useRef<Geometry | null>(null);
  const lastScroll = useRef<Point>({ x: 0, y: 0 });
  const pointers = useRef(new Map<number, Point>());
  const moved = useRef(false);
  const dragEndedAt = useRef(0);
  const pointerDistance = useRef(0);
  const safariGesture = useRef<{ scale: number } | null>(null);
  const handlers = useRef({ wheel: (_event: WheelEvent) => {}, move: (_event: globalThis.PointerEvent) => {}, end: (_event: globalThis.PointerEvent) => {}, cancel: () => {}, gesture: (_event: Event) => {} });

  useLayoutEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const measure = () => setStageSize({ width: stage.clientWidth, height: stage.clientHeight });
    measure();
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(measure);
      observer.observe(stage);
      return () => observer.disconnect();
    }
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [stageRef]);

  const ready = !!image && stageSize.width > 0 && stageSize.height > 0;
  const fitScale = ready ? Math.min(1, Math.max(1, stageSize.width - PADDING * 2) / image.width, Math.max(1, stageSize.height - PADDING * 2) / image.height) : null;
  // Extremely large diagrams must still be able to fit below 2%.
  const minScale = Math.min(0.02, fitScale ?? 0.02);
  const scale = ready ? zoom ?? fitScale : null;
  const geometry = scale !== null && image ? { stage: stageSize, image, scale } : null;
  const canPan = !!geometry && (image!.width * geometry.scale + PADDING * 2 > stageSize.width || image!.height * geometry.scale + PADDING * 2 > stageSize.height);

  const currentGeometry = (): Geometry | null => image && fitScale !== null
    ? { stage: stageSize, image, scale: zoomRef.current ?? fitScale }
    : null;
  const scroll = (): Point => pendingScroll.current ?? { x: stageRef.current?.scrollLeft ?? 0, y: stageRef.current?.scrollTop ?? 0 };
  const localPoint = (clientX: number, clientY: number): Point => {
    const rect = stageRef.current!.getBoundingClientRect();
    return { x: clientX - rect.left, y: clientY - rect.top };
  };
  const applyScroll = (point: Point) => {
    const stage = stageRef.current!;
    stage.scrollLeft = point.x;
    stage.scrollTop = point.y;
    lastScroll.current = { x: stage.scrollLeft, y: stage.scrollTop };
  };

  const zoomAt = (nextZoom: number | null, anchor: Point = { x: stageSize.width / 2, y: stageSize.height / 2 }) => {
    const old = currentGeometry();
    if (!old) return;
    const nextScale = nextZoom === null ? fitScale! : clamp(nextZoom, minScale, MAX_ZOOM);
    const oldOffset = imageOffset(old.stage, old.image, old.scale);
    const newOffset = imageOffset(old.stage, old.image, nextScale);
    const oldScroll = scroll();
    const nextScroll = nextZoom === null ? { x: 0, y: 0 } : boundedScroll({
      x: newOffset.x + (oldScroll.x + anchor.x - oldOffset.x) * nextScale / old.scale - anchor.x,
      y: newOffset.y + (oldScroll.y + anchor.y - oldOffset.y) * nextScale / old.scale - anchor.y,
    }, { ...old, scale: nextScale });
    zoomRef.current = nextZoom === null ? null : nextScale;
    pendingScroll.current = nextScroll;
    setZoom(zoomRef.current);
    // Only write immediately if the DOM already has this size. A burst of
    // wheel events can reach the limit before React commits the enlarged image.
    if (previousGeometry.current?.scale === nextScale) {
      applyScroll(nextScroll);
      pendingScroll.current = null;
    }
  };
  const changeZoom = (factor: number) => {
    const current = currentGeometry();
    if (current) zoomAt(current.scale * factor);
  };
  const panBy = (x: number, y: number) => {
    const current = currentGeometry();
    const stage = stageRef.current;
    if (!current || !stage) return;
    const position = scroll();
    const next = boundedScroll({ x: position.x + x, y: position.y + y }, current);
    if (pendingScroll.current) pendingScroll.current = next;
    else applyScroll(next);
  };

  useLayoutEffect(() => {
    const stage = stageRef.current;
    if (!stage || !geometry) return;
    let next = pendingScroll.current;
    const old = previousGeometry.current;
    if (!next && old && zoom !== null) {
      const oldOffset = imageOffset(old.stage, old.image, old.scale);
      const newOffset = imageOffset(geometry.stage, geometry.image, geometry.scale);
      next = {
        x: newOffset.x + (lastScroll.current.x + old.stage.width / 2 - oldOffset.x) * geometry.scale / old.scale - geometry.stage.width / 2,
        y: newOffset.y + (lastScroll.current.y + old.stage.height / 2 - oldOffset.y) * geometry.scale / old.scale - geometry.stage.height / 2,
      };
    }
    next = boundedScroll(next ?? { x: 0, y: 0 }, geometry);
    applyScroll(next);
    pendingScroll.current = null;
    previousGeometry.current = geometry;
  }, [scale, stageSize.width, stageSize.height, image?.width, image?.height, zoom, stageRef]);

  const releasePointer = (id: number) => {
    const stage = stageRef.current;
    if (stage?.hasPointerCapture?.(id)) stage.releasePointerCapture(id);
  };
  const cancelPointers = () => {
    const ids = [...pointers.current.keys()];
    pointers.current.clear();
    ids.forEach(releasePointer);
    if (moved.current) dragEndedAt.current = Date.now();
    safariGesture.current = null;
    setDragging(false);
  };

  handlers.current = {
    wheel(event) {
      event.preventDefault();
      event.stopPropagation();
      const current = currentGeometry();
      if (!current || safariGesture.current) return;
      const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? stageSize.height : 1;
      const dx = event.deltaX * unit;
      const dy = event.deltaY * unit;
      if ((wheelPan || event.shiftKey) && !event.ctrlKey && !event.metaKey) {
        panBy(event.shiftKey && !dx ? dy : dx, event.shiftKey && !dx ? 0 : dy);
      } else if (dy) {
        zoomAt(current.scale * Math.exp(-clamp(dy, -240, 240) * 0.003), localPoint(event.clientX, event.clientY));
      } else if (dx) panBy(dx, 0);
    },
    move(event) {
      const previous = pointers.current.get(event.pointerId);
      if (!previous) return;
      event.preventDefault();
      const next = localPoint(event.clientX, event.clientY);
      const before = [...pointers.current.values()];
      pointers.current.set(event.pointerId, next);
      // WebKit gesture events already own this pinch; avoid applying it twice.
      if (safariGesture.current) return;
      const after = [...pointers.current.values()];
      pointerDistance.current += Math.hypot(next.x - previous.x, next.y - previous.y);
      if (pointerDistance.current > 3) moved.current = true;
      if (before.length === 2) {
        const oldDistance = Math.hypot(before[0].x - before[1].x, before[0].y - before[1].y);
        const newDistance = Math.hypot(after[0].x - after[1].x, after[0].y - after[1].y);
        const oldCenter = { x: (before[0].x + before[1].x) / 2, y: (before[0].y + before[1].y) / 2 };
        const newCenter = { x: (after[0].x + after[1].x) / 2, y: (after[0].y + after[1].y) / 2 };
        const current = currentGeometry();
        if (current && oldDistance > 0 && newDistance > 0) zoomAt(current.scale * newDistance / oldDistance, oldCenter);
        panBy(oldCenter.x - newCenter.x, oldCenter.y - newCenter.y);
      } else panBy(previous.x - next.x, previous.y - next.y);
    },
    end(event) {
      if (!pointers.current.delete(event.pointerId)) return;
      if (moved.current) dragEndedAt.current = Date.now();
      releasePointer(event.pointerId);
      setDragging(pointers.current.size > 0 && canPan);
    },
    cancel: cancelPointers,
    gesture(event) {
      event.preventDefault();
      event.stopPropagation();
      const current = currentGeometry();
      if (!current) return;
      const gesture = event as Event & { scale?: number; clientX?: number; clientY?: number };
      if (event.type === "gesturestart") safariGesture.current = { scale: current.scale };
      if (event.type === "gesturechange" && safariGesture.current && typeof gesture.scale === "number" && gesture.scale > 0) {
        zoomAt(safariGesture.current.scale * gesture.scale,
          typeof gesture.clientX === "number" && typeof gesture.clientY === "number"
            ? localPoint(gesture.clientX, gesture.clientY) : undefined);
      }
      if (event.type === "gestureend") safariGesture.current = null;
    },
  };

  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const wheel = (event: WheelEvent) => handlers.current.wheel(event);
    const move = (event: globalThis.PointerEvent) => handlers.current.move(event);
    const end = (event: globalThis.PointerEvent) => handlers.current.end(event);
    const cancel = () => handlers.current.cancel();
    const gesture = (event: Event) => handlers.current.gesture(event);
    const onScroll = () => {
      const previous = previousGeometry.current;
      // A resize can clamp native scroll before ResizeObserver runs. Preserve
      // the last position measured in the old viewport for center restoration.
      if (!pendingScroll.current && previous?.stage.width === stage.clientWidth && previous.stage.height === stage.clientHeight) {
        lastScroll.current = { x: stage.scrollLeft, y: stage.scrollTop };
      }
    };
    stage.addEventListener("wheel", wheel, { passive: false });
    stage.addEventListener("scroll", onScroll);
    stage.addEventListener("lostpointercapture", end);
    for (const type of ["gesturestart", "gesturechange", "gestureend"]) stage.addEventListener(type, gesture, { passive: false });
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
    window.addEventListener("blur", cancel);
    return () => {
      stage.removeEventListener("wheel", wheel);
      stage.removeEventListener("scroll", onScroll);
      stage.removeEventListener("lostpointercapture", end);
      for (const type of ["gesturestart", "gesturechange", "gestureend"]) stage.removeEventListener(type, gesture);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      window.removeEventListener("blur", cancel);
      const ids = [...pointers.current.keys()];
      pointers.current.clear();
      ids.forEach(releasePointer);
    };
  }, [stageRef]);

  useEffect(() => {
    if (!ready) handlers.current.cancel();
  }, [ready]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (!ready || event.button !== 0) return;
    if (pointers.current.size === 0) {
      moved.current = false;
      pointerDistance.current = 0;
      event.currentTarget.focus({ preventScroll: true });
    }
    const point = localPoint(event.clientX, event.clientY);
    // Leave native scrollbar presses to the browser.
    if (point.x < 0 || point.y < 0 || point.x >= stageSize.width || point.y >= stageSize.height) return;
    if ((event.pointerType !== "touch" && (pointers.current.size > 0 || !canPan)) || pointers.current.size >= 2) return;
    event.preventDefault();
    pointers.current.set(event.pointerId, point);
    try { event.currentTarget.setPointerCapture?.(event.pointerId); } catch { /* Window listeners cover unavailable capture. */ }
    setDragging(canPan);
  };
  const onDoubleClick = (event: MouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    if (!ready || moved.current) return;
    zoomAt(zoomRef.current === null ? (fitScale! < 1 ? 1 : 2) : null, localPoint(event.clientX, event.clientY));
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.target !== event.currentTarget || !ready || event.ctrlKey || event.metaKey || event.altKey) return;
    const key = event.key;
    if (["+", "=", "-", "_", "0", "1", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(key)) event.preventDefault();
    const step = event.shiftKey ? 160 : 48;
    if (key === "+" || key === "=") changeZoom(1.25);
    if (key === "-" || key === "_") changeZoom(0.8);
    if (key === "0") zoomAt(null);
    if (key === "1") zoomAt(1);
    if (key === "ArrowLeft") panBy(-step, 0);
    if (key === "ArrowRight") panBy(step, 0);
    if (key === "ArrowUp") panBy(0, -step);
    if (key === "ArrowDown") panBy(0, step);
  };

  return { ready, zoom, scale, minScale, maxScale: MAX_ZOOM, wheelPan, setWheelPan, canPan, dragging, changeZoom, zoomAt, onPointerDown, onDoubleClick, onKeyDown,
    resetClickSuppression: () => { if (pointers.current.size === 0) moved.current = false; },
    suppressClick: () => pointers.current.size > 0 || (moved.current && Date.now() - dragEndedAt.current < 350),
  };
}
