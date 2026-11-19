import { useEffect, useState, type RefObject } from "react";

/**
 * Track the latest measured height (or width) of a DOM element via
 * ResizeObserver. Returns `null` until the first measurement arrives so the
 * caller can fall back to a default until layout settles.
 */
export function useElementSize(
  ref: RefObject<HTMLElement | null>,
  dimension: "height" | "width" = "height",
): number | null {
  const [size, setSize] = useState<number | null>(null);

  useEffect(() => {
    const node = ref.current;
    if (!node || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const next = dimension === "height" ? entry.contentRect.height : entry.contentRect.width;
        setSize((prev) => (prev === next ? prev : next));
      }
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [ref, dimension]);

  return size;
}
