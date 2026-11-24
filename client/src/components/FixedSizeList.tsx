import {
  forwardRef,
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";

export interface FixedSizeListProps<T> {
  items: T[];
  itemHeight: number;
  height: number;
  overscan?: number;
  renderItem: (item: T, index: number) => ReactNode;
  getKey: (item: T, index: number) => string;
  /** Initial scroll offset (parent may want to restore scroll position). */
  initialScrollTop?: number;
  onScroll?: (scrollTop: number) => void;
  /** List occupying the start of an existing scroll viewport, such as the workspace. */
  viewportRef?: RefObject<HTMLDivElement>;
  ariaLabel?: string;
  role?: "listbox" | "presentation";
  className?: string;
}

export interface FixedSizeListHandle {
  scrollToIndex(index: number): void;
}

/**
 * Minimal fixed-size virtual list. Only renders rows visible in the viewport
 * plus `overscan` rows above and below, keeping the DOM bounded regardless of
 * `items.length`.
 */
function FixedSizeListInner<T>(
  props: FixedSizeListProps<T>,
  ref: React.Ref<FixedSizeListHandle>,
) {
  const {
    items,
    itemHeight,
    height,
    overscan = 8,
    renderItem,
    getKey,
    initialScrollTop = 0,
    onScroll,
    viewportRef,
    ariaLabel,
    role = "listbox",
    className,
  } = props;

  const containerRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(initialScrollTop);

  // Apply initial scrollTop once the container has a height.
  useLayoutEffect(() => {
    if (viewportRef) return;
    const node = containerRef.current;
    if (!node || height <= 0) return;
    if (initialScrollTop > 0 && node.scrollTop !== initialScrollTop) {
      node.scrollTop = initialScrollTop;
    }
  }, [initialScrollTop, height, viewportRef]);

  const recordScroll = useCallback(
    (next: number) => {
      setScrollTop((prev) => (prev === next ? prev : next));
      onScroll?.(next);
    },
    [onScroll],
  );

  useLayoutEffect(() => {
    const viewport = viewportRef?.current;
    if (!viewport) return;
    const handleScroll = () => recordScroll(viewport.scrollTop);
    handleScroll();
    viewport.addEventListener("scroll", handleScroll, { passive: true });
    return () => viewport.removeEventListener("scroll", handleScroll);
  }, [viewportRef, recordScroll]);

  const { start, end, totalHeight } = useMemo(() => {
    if (height <= 0 || itemHeight <= 0) {
      return { start: 0, end: 0, totalHeight: items.length * itemHeight };
    }
    const total = items.length * itemHeight;
    const offset = Math.max(0, Math.min(scrollTop, total - height));
    const first = Math.max(0, Math.floor(offset / itemHeight) - overscan);
    const visibleCount = Math.ceil(height / itemHeight) + overscan * 2;
    const last = Math.min(items.length, first + visibleCount);
    return { start: first, end: last, totalHeight: total };
  }, [items.length, itemHeight, scrollTop, height, overscan]);

  const handle = useMemo<FixedSizeListHandle>(
    () => ({
      scrollToIndex: (index: number) => {
        const node = viewportRef?.current ?? containerRef.current;
        if (!node) return;
        const top = index * itemHeight;
        node.scrollTop = top;
        recordScroll(node.scrollTop);
      },
    }),
    [itemHeight, viewportRef, recordScroll],
  );
  useImperativeHandle(ref, () => handle, [handle]);

  return (
    <div
      ref={containerRef}
      role={role}
      aria-label={ariaLabel}
      className={["fixed-size-list", className].filter(Boolean).join(" ")}
      style={viewportRef ? undefined : { height, overflowY: "auto" }}
      onScroll={viewportRef ? undefined : (event) => recordScroll(event.currentTarget.scrollTop)}
      data-fixed-size-list=""
    >
      <div style={{ height: totalHeight, position: "relative" }}>
        <div
          style={{
            position: "absolute",
            top: start * itemHeight,
            left: 0,
            right: 0,
          }}
          data-virtual-window-start={start}
          data-virtual-window-end={end}
        >
          {items.slice(start, end).map((item, offset) => {
            const index = start + offset;
            return (
              <div
                key={getKey(item, index)}
                style={{ height: itemHeight }}
                data-virtual-index={index}
              >
                {renderItem(item, index)}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

export const FixedSizeList = forwardRef(FixedSizeListInner) as <T>(
  props: FixedSizeListProps<T> & { ref?: React.Ref<FixedSizeListHandle> },
) => ReturnType<typeof FixedSizeListInner>;
