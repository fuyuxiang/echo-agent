import { memo, useEffect, useRef } from "react";

// Only active marks need observation. Share one observer and pause their CSS
// animations outside the viewport or while the document is in the background.
const visibleMarks = new Map<HTMLElement, boolean>();
let visibilityObserver: IntersectionObserver | undefined;

function updateMotion(element: HTMLElement, visible: boolean): void {
  element.dataset.motion = visible && !document.hidden ? "on" : "off";
}

function updateDocumentMotion(): void {
  visibleMarks.forEach((visible, element) => updateMotion(element, visible));
}

function observeMark(element: HTMLElement): () => void {
  if (visibleMarks.size === 0) {
    document.addEventListener("visibilitychange", updateDocumentMotion);
    if (typeof IntersectionObserver !== "undefined") {
      visibilityObserver = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          const mark = entry.target as HTMLElement;
          if (!visibleMarks.has(mark)) continue;
          const visible = entry.isIntersecting && entry.intersectionRatio >= 0.1;
          visibleMarks.set(mark, visible);
          updateMotion(mark, visible);
        }
      }, { threshold: 0.1 });
    }
  }

  const visible = !visibilityObserver;
  visibleMarks.set(element, visible);
  updateMotion(element, visible);
  visibilityObserver?.observe(element);

  return () => {
    visibilityObserver?.unobserve(element);
    visibleMarks.delete(element);
    if (visibleMarks.size === 0) {
      visibilityObserver?.disconnect();
      visibilityObserver = undefined;
      document.removeEventListener("visibilitychange", updateDocumentMotion);
    }
  };
}

/** Keep the app artwork intact; only the waiting halo moves. */
export const EchoMark = memo(function EchoMark({ active = false }: { active?: boolean }) {
  const ref = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (active) return observeMark(element);
    element.dataset.motion = "off";
  }, [active]);

  return (
    <span
      ref={ref}
      className={`echo-mark${active ? " echo-mark--active" : ""}`}
      aria-hidden="true"
      data-motion="off"
    >
      <img src="/app-icon.png" alt="" width={24} height={24} draggable={false} decoding="async" />
    </span>
  );
});
