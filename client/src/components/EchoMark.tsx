import { memo, useEffect, useId, useRef } from "react";

// Only active marks need observation. Share one observer and pause their CSS
// animations outside the viewport or while the document is in the background.
const visibleMarks = new Map<SVGSVGElement, boolean>();
let visibilityObserver: IntersectionObserver | undefined;

function updateMotion(element: SVGSVGElement, visible: boolean): void {
  element.dataset.motion = visible && !document.hidden ? "on" : "off";
}

function updateDocumentMotion(): void {
  visibleMarks.forEach((visible, element) => updateMotion(element, visible));
}

function observeMark(element: SVGSVGElement): () => void {
  if (visibleMarks.size === 0) {
    document.addEventListener("visibilitychange", updateDocumentMotion);
    if (typeof IntersectionObserver !== "undefined") {
      visibilityObserver = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          const mark = entry.target as SVGSVGElement;
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

/** The brand's three ribbons stay legible as an E while gently flowing. */
export const EchoMark = memo(function EchoMark({ active = false }: { active?: boolean }) {
  const gradientId = `echo-mark-${useId().replace(/:/g, "")}`;
  const ref = useRef<SVGSVGElement>(null);

  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (active) return observeMark(element);
    element.dataset.motion = "off";
  }, [active]);

  return (
    <svg
      ref={ref}
      className={`echo-mark${active ? " echo-mark--active" : ""}`}
      viewBox="0 0 32 32"
      width="24"
      height="24"
      aria-hidden="true"
      focusable="false"
      data-motion="off"
    >
      <defs>
        <linearGradient id={gradientId} gradientUnits="userSpaceOnUse" x1="4" y1="4" x2="28" y2="28">
          <stop className="echo-mark__stop echo-mark__stop--1" offset="4%" />
          <stop className="echo-mark__stop echo-mark__stop--2" offset="46%" />
          <stop className="echo-mark__stop echo-mark__stop--3" offset="62%" />
          <stop className="echo-mark__stop echo-mark__stop--4" offset="88%" />
        </linearGradient>
      </defs>
      <g fill={`url(#${gradientId})`}>
        <path className="echo-mark__ribbon" d="M11 4h17c-1.6 4.2-4.9 6-10.5 6H9.7C7 10 5.5 11 4 12.6 4.7 7.2 6.8 4 11 4Z" />
        <path className="echo-mark__ribbon" d="M9 13h16c-1.4 4-4.4 6-9 6H8c-2.2 0-3.8.6-5 1.8C3.3 15.8 5.2 13 9 13Z" />
        <path className="echo-mark__ribbon" d="M7 22h12.5c4.2 0 6.7 2 8.5 6H11c-4.8 0-7.3-2.3-8-6.5 1 .4 2.4.5 4 .5Z" />
      </g>
    </svg>
  );
});
