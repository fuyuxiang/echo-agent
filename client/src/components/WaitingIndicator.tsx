import { useEffect, useRef } from "react";

// Share one observer and pause animation outside the viewport or in the background.
const visibleIndicators = new Map<HTMLElement, boolean>();
let visibilityObserver: IntersectionObserver | undefined;

function updateMotion(element: HTMLElement, visible: boolean): void {
  element.dataset.motion = visible && !document.hidden ? "on" : "off";
}

function updateDocumentMotion(): void {
  visibleIndicators.forEach((visible, element) => updateMotion(element, visible));
}

function observeIndicator(element: HTMLElement): () => void {
  if (visibleIndicators.size === 0) {
    document.addEventListener("visibilitychange", updateDocumentMotion);
    if (typeof IntersectionObserver !== "undefined") {
      visibilityObserver = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          const indicator = entry.target as HTMLElement;
          if (!visibleIndicators.has(indicator)) continue;
          const visible = entry.isIntersecting && entry.intersectionRatio >= 0.1;
          visibleIndicators.set(indicator, visible);
          updateMotion(indicator, visible);
        }
      }, { threshold: 0.1 });
    }
  }

  const visible = !visibilityObserver;
  visibleIndicators.set(element, visible);
  updateMotion(element, visible);
  visibilityObserver?.observe(element);

  return () => {
    visibilityObserver?.unobserve(element);
    visibleIndicators.delete(element);
    if (visibleIndicators.size === 0) {
      visibilityObserver?.disconnect();
      visibilityObserver = undefined;
      document.removeEventListener("visibilitychange", updateDocumentMotion);
    }
  };
}

export function useVisibleMotion(enabled = true) {
  const ref = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (!enabled) {
      element.dataset.motion = "off";
      return;
    }
    return observeIndicator(element);
  }, [enabled]);

  return ref;
}

export function WaitingIndicator() {
  const ref = useVisibleMotion();
  return <span ref={ref} className="waiting-indicator" aria-hidden="true" data-motion="off" />;
}
