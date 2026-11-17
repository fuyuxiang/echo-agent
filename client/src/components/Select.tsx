import {
  forwardRef, useEffect, useId, useImperativeHandle, useLayoutEffect, useRef, useState,
  type CSSProperties, type SelectHTMLAttributes,
} from "react";
import { createPortal } from "react-dom";
import { Check } from "lucide-react";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";

interface Choice { index: number; value: string; label: string; disabled: boolean; group?: string }
interface SelectProps extends SelectHTMLAttributes<HTMLSelectElement> { density?: "default" | "compact" }

/** Keep the native field for labels, forms, validation and change handlers;
 * paint the single-choice popup ourselves so all desktop WebViews agree. */
export const Select = forwardRef<HTMLSelectElement, SelectProps>(
  function Select({ children, className = "", density = "default", onMouseDown, onClick, onKeyDown, onBlur, ...props }, forwardedRef) {
    const field = useRef<HTMLSelectElement>(null);
    const menu = useRef<HTMLDivElement>(null);
    const listId = `echo-select-${useId().replace(/:/g, "")}`;
    const [open, setOpen] = useState(false);
    const [choices, setChoices] = useState<Choice[]>([]);
    const [active, setActive] = useState(-1);
    const [layer, setLayer] = useState<CSSProperties["zIndex"]>("var(--echo-layer-popover)");
    const search = useRef({ text: "", time: 0 });
    const nativeList = props.multiple || (props.size ?? 0) > 1;
    const expanded = open && !props.disabled && !nativeList;
    useImperativeHandle(forwardedRef, () => field.current!);
    const { style, placement } = useAnchoredFloating(field, menu, expanded, {
      preferredPlacement: "bottom", width: "content", maxWidth: 480,
      maxHeight: 320, estimatedHeight: Math.min(320, choices.length * 36 + 12), offset: 6, zIndex: layer,
    });

    const readChoices = () => Array.from(field.current?.options ?? []).map(option => {
      const group = option.parentElement instanceof HTMLOptGroupElement ? option.parentElement : null;
      return { index: option.index, value: option.value, label: option.label,
        disabled: option.disabled || !!group?.disabled || option.hidden, group: group?.label };
    });
    const show = (edge?: "first" | "last") => {
      if (props.disabled || nativeList || field.current?.matches(":disabled")) return;
      const rootStyle = getComputedStyle(document.documentElement);
      const baseLayer = Number(rootStyle.getPropertyValue("--echo-layer-popover")) || 1100;
      let containingLayer = 0;
      for (let parent = field.current?.parentElement; parent; parent = parent.parentElement) {
        containingLayer = Math.max(containingLayer, Number(getComputedStyle(parent).zIndex) || 0);
      }
      setLayer(containingLayer >= baseLayer
        ? Math.max(containingLayer + 1, Number(rootStyle.getPropertyValue("--echo-layer-dialog-popover")) || 1300)
        : "var(--echo-layer-popover)");
      const next = readChoices();
      const enabled = next.filter(choice => !choice.disabled);
      setChoices(next);
      setActive((edge === "last" ? enabled[enabled.length - 1] : edge === "first" ? enabled[0]
        : enabled.find(choice => choice.index === field.current?.selectedIndex) ?? enabled[0])?.index ?? -1);
      setOpen(true);
    };
    const choose = (choice: Choice) => {
      const select = field.current;
      if (!select || choice.disabled || select.matches(":disabled")) return;
      setOpen(false);
      select.focus();
      if (select.selectedIndex === choice.index) return;
      select.selectedIndex = choice.index;
      select.dispatchEvent(new Event("input", { bubbles: true }));
      select.dispatchEvent(new Event("change", { bubbles: true }));
    };

    useLayoutEffect(() => {
      if (!expanded) return;
      const next = readChoices();
      setChoices(next);
      setActive(current => next.some(choice => choice.index === current && !choice.disabled)
        ? current : next.find(choice => !choice.disabled)?.index ?? -1);
    }, [children, expanded]);
    useLayoutEffect(() => {
      if (expanded) document.getElementById(`${listId}-${active}`)?.scrollIntoView?.({ block: "nearest" });
    }, [active, expanded, listId]);
    useEffect(() => {
      if (!expanded) { setOpen(false); search.current.text = ""; return; }
      const dismiss = (event: Event) => {
        if (!(event.target instanceof Node) || (!field.current?.contains(event.target) && !menu.current?.contains(event.target))) setOpen(false);
      };
      window.addEventListener("pointerdown", dismiss);
      window.addEventListener("mousedown", dismiss);
      window.addEventListener("blur", dismiss);
      return () => {
        window.removeEventListener("pointerdown", dismiss);
        window.removeEventListener("mousedown", dismiss);
        window.removeEventListener("blur", dismiss);
      };
    }, [expanded]);

    const label = props["aria-label"] ?? (Array.from(field.current?.labels ?? []).map(element => {
      const copy = element.cloneNode(true) as HTMLElement;
      copy.querySelectorAll("select").forEach(select => select.remove());
      return copy.textContent?.trim();
    }).filter(Boolean).join(" ") || undefined);

    return <>
      <select {...props} ref={field} className={`echo-select${density === "compact" ? " echo-select--compact" : ""} ${className}`}
        aria-haspopup={nativeList ? undefined : "listbox"}
        aria-expanded={nativeList ? undefined : expanded}
        aria-controls={expanded ? listId : undefined}
        aria-activedescendant={expanded && active >= 0 ? `${listId}-${active}` : undefined}
        onMouseDown={event => {
          onMouseDown?.(event);
          if (!nativeList && !event.defaultPrevented && event.button === 0) {
            event.preventDefault(); field.current?.focus();
          }
        }}
        onClick={event => {
          onClick?.(event);
          if (!nativeList && !event.defaultPrevented) { event.preventDefault(); if (expanded) setOpen(false); else show(); }
        }}
        onBlur={event => { onBlur?.(event); setOpen(false); }}
        onKeyDown={event => {
          onKeyDown?.(event);
          if (event.defaultPrevented || nativeList || event.nativeEvent.isComposing) return;
          if (event.key === "Tab") { setOpen(false); return; }
          if (event.key === "Escape") {
            if (expanded) { event.preventDefault(); event.stopPropagation(); setOpen(false); }
            return;
          }
          if (["ArrowDown", "ArrowUp", "Home", "End", "Enter", " "].includes(event.key)) {
            event.preventDefault(); event.stopPropagation();
            if (!expanded) { show(event.key === "Home" ? "first" : event.key === "End" ? "last" : undefined); return; }
            const enabled = choices.filter(choice => !choice.disabled);
            if (event.key === "Enter" || event.key === " ") { const choice = choices.find(choice => choice.index === active); if (choice) choose(choice); }
            else if (event.altKey && event.key === "ArrowUp") setOpen(false);
            else {
              const index = enabled.findIndex(choice => choice.index === active);
              const next = event.key === "Home" ? 0 : event.key === "End" ? enabled.length - 1
                : Math.max(0, Math.min(enabled.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)));
              setActive(enabled[next]?.index ?? -1);
            }
          } else if (event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) {
            event.preventDefault();
            const now = Date.now();
            const previous = now - search.current.time < 700 ? search.current.text : "";
            const text = previous + event.key.toLocaleLowerCase();
            search.current = { text, time: now };
            if (!expanded) show();
            const enabled = readChoices().filter(choice => !choice.disabled);
            const start = enabled.findIndex(choice => choice.index === active);
            const ordered = [...enabled.slice(start + 1), ...enabled.slice(0, start + 1)];
            const match = ordered.find(choice => choice.label.toLocaleLowerCase().startsWith(text))
              ?? ordered.find(choice => choice.label.toLocaleLowerCase().startsWith(event.key.toLocaleLowerCase()));
            if (match) setActive(match.index);
          }
        }}>{children}</select>
      {expanded && createPortal(<div ref={menu} id={listId} role="listbox"
        aria-label={label}
        aria-labelledby={props["aria-labelledby"]}
        className="echo-select-menu" data-placement={placement} style={{ ...style, minWidth: field.current?.getBoundingClientRect().width }}
        // This popup belongs to its enclosing picker/dialog even though the
        // portal is outside that parent's DOM. Own the press before document
        // outside-click listeners can unmount the parent ahead of selection.
        onPointerDown={event => event.stopPropagation()}
        onMouseDown={event => { event.preventDefault(); event.stopPropagation(); }}>
        {choices.map((choice, index) => <div key={choice.index}>
          {choice.group && choices[index - 1]?.group !== choice.group && <div className="echo-select-menu__group">{choice.group}</div>}
          <div id={`${listId}-${choice.index}`} role="option" aria-selected={field.current?.selectedIndex === choice.index}
            aria-disabled={choice.disabled || undefined} className="echo-select-menu__option"
            data-active={active === choice.index || undefined}
            onMouseMove={() => { if (!choice.disabled) setActive(choice.index); }} onClick={() => choose(choice)}>
            <span>{choice.label}</span>{field.current?.selectedIndex === choice.index && <Check size={14} aria-hidden="true" />}
          </div>
        </div>)}
        {!choices.length && <div className="echo-select-menu__empty">暂无可选项</div>}
      </div>, document.body)}
    </>;
  },
);
