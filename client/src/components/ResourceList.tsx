import { forwardRef, useId, useLayoutEffect, useRef, useState, type HTMLAttributes, type ReactNode } from "react";

/** Shared collection chrome. Features retain ownership of actions and state. */
export function ResourceList({ className = "", ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div role="list" {...props} className={`resource-list ${className}`} />;
}

export const ResourceRow = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(
  function ResourceRow({ className = "", ...props }, ref) {
    return <div ref={ref} role="listitem" {...props} className={`resource-row ${className}`} />;
  },
);

export function ResourceIcon({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <span className={`resource-icon ${className}`} aria-hidden="true">{children}</span>;
}

/** Keep descriptions compact without making their remaining text inaccessible. */
export function ResourceDescription({ text, label, className = "" }: { text: string; label: string; className?: string }) {
  const id = useId();
  const textRef = useRef<HTMLDivElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [overflows, setOverflows] = useState(false);

  useLayoutEffect(() => {
    const element = textRef.current;
    if (!element) return;
    setExpanded(false);
    const measure = () => {
      // scrollHeight includes the hidden lines even while the preview is clamped.
      // Compare with two lines so the same measurement also works when expanded.
      const lineHeight = Number.parseFloat(getComputedStyle(element).lineHeight);
      setOverflows(element.scrollHeight > Math.ceil(lineHeight * 2) + 1);
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(element);
    window.addEventListener("resize", measure);
    document.fonts?.addEventListener("loadingdone", measure);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", measure);
      document.fonts?.removeEventListener("loadingdone", measure);
    };
  }, [text]);

  return <div className="resource-description">
    <div ref={textRef} id={id} tabIndex={-1}
      className={`resource-description__text resource-row__description ${className}${expanded ? "" : " resource-clamp"}`}>{text}</div>
    {(overflows || expanded) && <button type="button" className="resource-description__toggle"
      aria-expanded={expanded} aria-controls={id} aria-label={`${expanded ? "收起" : "展开"}${label}`}
      onClick={() => {
        // A wider layout can make the toggle unnecessary; keep keyboard focus
        // in the description when collapsing removes the focused button.
        if (expanded && !overflows) textRef.current?.focus({ preventScroll: true });
        setExpanded(value => !value);
      }}>{expanded ? "收起说明" : "展开说明"}</button>}
  </div>;
}
