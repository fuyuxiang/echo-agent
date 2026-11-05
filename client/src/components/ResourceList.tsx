import { Children, forwardRef, useEffect, useId, useLayoutEffect, useRef, useState, type HTMLAttributes, type ReactNode } from "react";
import { LayoutGrid, List, MoreHorizontal } from "lucide-react";
import { ActionMenu } from "./ActionMenu";

type ResourceView = "cards" | "list";
type CollectionKey = "projects" | "experts" | "groups" | "skills" | "connectors" | "plugins" | "automations";
function savedView(key: string, fallback: ResourceView): ResourceView {
  try {
    const value = localStorage.getItem(key);
    return value === "cards" || value === "list" ? value : fallback;
  } catch { return fallback; }
}

/** Shared collection chrome. Features retain ownership of actions and state. */
export function ResourceList({ className = "", collectionKey, layout = "grid", summary, children, ...props }:
  HTMLAttributes<HTMLDivElement> & { collectionKey?: CollectionKey; layout?: "grid" | "compact"; summary?: ReactNode }) {
  const storageKey = `echo.resource-view.${collectionKey}`;
  const [view, setView] = useState<ResourceView>(() => savedView(storageKey, "cards"));
  useEffect(() => {
    setView(savedView(storageKey, "cards"));
    const sync = (event: StorageEvent) => {
      if (event.key === storageKey || event.key === null) setView(savedView(storageKey, "cards"));
    };
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, [storageKey]);
  const label = props["aria-label"] || "资源";
  return <div className="resource-collection">
    {collectionKey && <div className="resource-collection__toolbar">
      <span className="resource-collection__count">{summary ?? `${Children.count(children)} 项`}</span>
      <div className="resource-view" role="group" aria-label={`${label}显示方式`}>
        {(["cards", "list"] as const).map(value => <button key={value} type="button"
          aria-label={value === "cards" ? "卡片视图" : "列表视图"} aria-pressed={view === value}
          onClick={() => {
            setView(value);
            try { localStorage.setItem(storageKey, value); } catch { /* The current view still works when storage is unavailable. */ }
          }}>
          {value === "cards" ? <LayoutGrid size={15} aria-hidden="true" /> : <List size={15} aria-hidden="true" />}
          <span>{value === "cards" ? "卡片" : "列表"}</span>
        </button>)}
      </div>
    </div>}
    <div role="list" {...props} data-view={collectionKey ? view : "list"} data-layout={layout}
      className={`resource-list ${className}`}>{children}</div>
  </div>;
}

/** Reuse the app's portalled, keyboard-accessible menu so cards never clip actions. */
export function ResourceMenu({ label, actions, disabled = false }: { label: string; disabled?: boolean; actions: {
  label: string; text?: string; onSelect: () => void; danger?: boolean; disabled?: boolean;
}[] }) {
  if (!actions.length) return null;
  return <ActionMenu label={`${label}更多操作`} menuLabel={`${label}操作菜单`} busy={disabled}
    trigger={<MoreHorizontal size={18} aria-hidden="true" />} triggerClassName="resource-more"
    menuClassName="topbar-actions__menu resource-menu">
    {({ close }) => actions.map(action => <button key={action.label} type="button" role="menuitem"
      aria-label={action.label} className={`topbar-actions__item${action.danger ? " resource-action--danger" : ""}`}
      disabled={action.disabled} onClick={() => { close(true); action.onSelect(); }}>{action.text || action.label}</button>)}
  </ActionMenu>;
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
