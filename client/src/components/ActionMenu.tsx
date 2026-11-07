/** Shared portal menu, extracted from the session menu's positioning and keyboard contract. */
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode, type RefObject, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";

type MenuFocusEdge = "first" | "last";
interface MenuPosition { left: number; top: number; width: number; maxHeight: number; placement: "top" | "bottom" }
const MENU_MARGIN = 8;
const MENU_OFFSET = 6;
const MENU_ESTIMATED_WIDTH = 224;
const MENU_ESTIMATED_HEIGHT = 240;
const MENU_Z_INDEX = "var(--echo-layer-popover)";
const MENU_ITEM_SELECTOR = '[role="menuitem"]:not(:disabled), [role="menuitemradio"]:not(:disabled)';

export interface ActionMenuHandle {
  close: (restoreFocus?: boolean) => void;
  triggerRef: RefObject<HTMLButtonElement>;
}
export function ActionMenu({ label, menuLabel, trigger, resetKey, busy = false,
  triggerClassName = "main-topbar__btn", menuClassName = "topbar-actions__menu", menuZIndex = MENU_Z_INDEX, children, contentKey, onOpenChange, onBack,
}: {
  onBack?: () => void;
  contentKey?: string;
  onOpenChange?: (open: boolean) => void;
  label: string;
  menuLabel: string;
  trigger: ReactNode;
  resetKey?: string | null;
  busy?: boolean;
  triggerClassName?: string;
  menuClassName?: string;
  /** Portalled menus opened inside dialogs use the dialog popover layer. */
  menuZIndex?: CSSProperties["zIndex"];
  children: (handle: ActionMenuHandle) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [menuPosition, setMenuPosition] = useState<MenuPosition | null>(null);
  const menuId = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const pendingFocusRef = useRef<MenuFocusEdge | null>(null);

  const openChangeRef = useRef(onOpenChange);
  openChangeRef.current = onOpenChange;

  const closeMenu = useCallback((restoreFocus = false) => {
    setOpen(false);
    openChangeRef.current?.(false);
    setMenuPosition(null);
    if (restoreFocus) {
      triggerRef.current?.focus({ preventScroll: true });
    }
  }, []);

  const openMenu = useCallback((focusEdge: MenuFocusEdge = "first") => {
    if (busy) return;
    pendingFocusRef.current = focusEdge;
    setOpen(true);
    openChangeRef.current?.(true);
  }, [busy]);

  const updateMenuPosition = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger) return;

    const rect = trigger.getBoundingClientRect();
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;
    if (
      rect.bottom < 0 || rect.top > viewportHeight
      || rect.right < 0 || rect.left > viewportWidth
    ) {
      closeMenu();
      return;
    }

    const measuredWidth = menuRef.current?.getBoundingClientRect().width || menuRef.current?.offsetWidth || MENU_ESTIMATED_WIDTH;
    // scrollHeight keeps the full height even when a small window applies maxHeight.
    const measuredHeight = menuRef.current?.scrollHeight
      || menuRef.current?.offsetHeight
      || MENU_ESTIMATED_HEIGHT;
    const menuWidth = Math.min(
      measuredWidth,
      Math.max(0, viewportWidth - MENU_MARGIN * 2),
    );
    const spaceBelow = viewportHeight - rect.bottom - MENU_OFFSET - MENU_MARGIN;
    const spaceAbove = rect.top - MENU_OFFSET - MENU_MARGIN;
    const placement = spaceBelow >= measuredHeight || spaceBelow >= spaceAbove
      ? "bottom"
      : "top";
    const availableHeight = Math.max(0, placement === "bottom" ? spaceBelow : spaceAbove);
    const renderedHeight = Math.min(measuredHeight, availableHeight);
    const maxLeft = Math.max(MENU_MARGIN, viewportWidth - menuWidth - MENU_MARGIN);
    const left = Math.min(Math.max(MENU_MARGIN, rect.right - menuWidth), maxLeft);
    const desiredTop = placement === "bottom"
      ? rect.bottom + MENU_OFFSET
      : rect.top - MENU_OFFSET - renderedHeight;
    const maxTop = Math.max(MENU_MARGIN, viewportHeight - renderedHeight - MENU_MARGIN);
    const top = Math.min(Math.max(MENU_MARGIN, desiredTop), maxTop);

    setMenuPosition(current => current?.left === left && current.top === top
      && current.width === menuWidth && current.maxHeight === availableHeight && current.placement === placement
      ? current : { left, top, width: menuWidth, maxHeight: availableHeight, placement });
  }, [closeMenu]);

  // Measure before paint, then keep the portal aligned while the desktop window moves/resizes.
  useLayoutEffect(() => {
    if (!open) return;
    pendingFocusRef.current ??= "first";
    updateMenuPosition();
    const frame = window.requestAnimationFrame(updateMenuPosition);
    window.addEventListener("resize", updateMenuPosition);
    window.addEventListener("scroll", updateMenuPosition, true);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(updateMenuPosition);
    if (menuRef.current) observer?.observe(menuRef.current);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("resize", updateMenuPosition);
      window.removeEventListener("scroll", updateMenuPosition, true);
      observer?.disconnect();
    };
  }, [open, updateMenuPosition, contentKey]);

  // A portalled menu is not contained by rootRef, so outside-click checks both trees.
  useEffect(() => {
    if (!open) return;
    const onMouseDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (rootRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      closeMenu();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      closeMenu(true);
    };
    const onFocus = (event: FocusEvent) => {
      const target = event.target as Node;
      if (target !== document.body && !rootRef.current?.contains(target) && !menuRef.current?.contains(target)) closeMenu();
    };
    document.addEventListener("focusin", onFocus);
    document.addEventListener("mousedown", onMouseDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open, closeMenu]);

  useEffect(() => {
    if (!open || !menuPosition || !pendingFocusRef.current) return;
    const items = Array.from(
      menuRef.current?.querySelectorAll<HTMLButtonElement>(MENU_ITEM_SELECTOR) ?? [],
    );
    items[pendingFocusRef.current === "first" ? 0 : items.length - 1]?.focus();
    pendingFocusRef.current = null;
  }, [open, menuPosition, contentKey]);

  // Never leave one session's menu open after navigation reuses this component.
  useEffect(() => {
    closeMenu();
  }, [resetKey, closeMenu]);

  const handleMenuKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "ArrowLeft" && onBack) { event.preventDefault(); onBack(); return; }
    if (event.key === "Tab") {
      triggerRef.current?.focus();
      closeMenu();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeMenu(true);
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;

    const items = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>(MENU_ITEM_SELECTOR),
    );
    if (items.length === 0) return;
    event.preventDefault();
    event.stopPropagation();
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === "Home") items[0].focus();
    else if (event.key === "End") items[items.length - 1].focus();
    else if (event.key === "ArrowDown") items[current < 0 ? 0 : (current + 1) % items.length].focus();
    else items[current < 0 ? items.length - 1 : (current - 1 + items.length) % items.length].focus();
  };

  const menuStyle: CSSProperties = menuPosition
    ? {
        position: "fixed",
        left: menuPosition.left,
        top: menuPosition.top,
        // CSS owns intrinsic width. Freezing the measurement here would also
        // prevent a submenu from shrinking back after its content changes.
        maxHeight: menuPosition.maxHeight,
        overflowY: "auto",
        zIndex: menuZIndex,
      }
    : { position: "fixed", visibility: "hidden" };

  return (
    <div className="topbar-actions" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className={triggerClassName}
        aria-label={label}
        data-tip={busy ? "正在处理" : label}
        disabled={busy}
        aria-busy={busy}
        aria-haspopup="menu"
        aria-controls={open ? menuId : undefined}
        aria-expanded={open}
        onClick={(event) => {
          event.stopPropagation();
          if (open) closeMenu();
          else openMenu();
        }}
        onKeyDown={(event) => {
          if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
          event.preventDefault();
          openMenu(event.key === "ArrowUp" ? "last" : "first");
        }}
      >
        {trigger}
      </button>

      {open && typeof document !== "undefined" && createPortal(
        <div
          ref={menuRef}
          id={menuId}
          className={menuClassName}
          style={menuStyle}
          role="menu"
          aria-label={menuLabel}
          data-placement={menuPosition?.placement}
          onClick={(event) => event.stopPropagation()}
          onKeyDown={handleMenuKeyDown}
        >
          {children({ close: closeMenu, triggerRef })}
        </div>,
        document.body,
      )}
    </div>
  );
}
