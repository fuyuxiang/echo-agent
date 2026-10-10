import {
  useEffect,
  useRef,
  useSyncExternalStore,
  type RefObject,
} from "react";

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "a[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex=\"-1\"])",
].join(",");

// Dialogs can be nested (for example Settings -> model editor, or project
// editor -> reference picker). Only the top-most mounted dialog may consume
// keyboard input; otherwise one Escape could close multiple layers and the
// outer trap could move focus behind the active inner dialog.
const modalStack: HTMLElement[] = [];
const modalPresenceListeners = new Set<() => void>();

function emitModalPresenceChange(previousLength: number): void {
  if ((previousLength === 0) === (modalStack.length === 0)) return;
  for (const listener of modalPresenceListeners) listener();
}

function subscribeModalPresence(listener: () => void): () => void {
  modalPresenceListeners.add(listener);
  return () => modalPresenceListeners.delete(listener);
}

function dismissTransientSurfaces(): void {
  // Menus and pickers consistently treat an outside pointer/mouse press as a
  // dismissal. Programmatic dialogs have no real press, so emit the same
  // semantic boundary before taking focus. This prevents a portalled menu
  // from reappearing after the dialog closes (notably Cmd/Ctrl+K).
  document.body.dispatchEvent(new Event("pointerdown", { bubbles: true, cancelable: true }));
  document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
}

/**
 * Reports whether any focus-managed modal is mounted anywhere in the app.
 *
 * Global hover surfaces use this to leave the interaction layer while a
 * dialog owns the screen. The snapshot intentionally changes only on the
 * zero/non-zero boundary; nested dialogs should not cause unrelated chrome
 * to flicker as their stack depth changes.
 */
export function useModalPresence(): boolean {
  return useSyncExternalStore(
    subscribeModalPresence,
    () => modalStack.length > 0,
    () => false,
  );
}

function focusableElements(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) => {
      if (element.tabIndex < 0 || element.matches(':disabled, input[type="hidden"]')) return false;
      if (element.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
      // Checking computed visibility also works in jsdom, unlike geometry
      // alone. Opacity is intentionally ignored: custom checkboxes retain
      // a transparent native input so keyboard users can still toggle them.
      for (let current: HTMLElement | null = element; current; current = current.parentElement) {
        const style = window.getComputedStyle(current);
        if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse") return false;
      }
      return true;
    },
  );
}

function ownedTransient(container: HTMLElement, target: EventTarget | null): {
  surface: HTMLElement;
  trigger: HTMLElement | null;
} | null {
  const targetSurface = target instanceof Element
    ? target.closest<HTMLElement>('[role="menu"], [role="listbox"]')
    : null;
  const triggers = Array.from(container.querySelectorAll<HTMLElement>('[aria-expanded="true"][aria-controls]'));
  for (const trigger of triggers) {
    const surface = document.getElementById(trigger.getAttribute("aria-controls") ?? "");
    if (surface && surface.matches('[role="menu"], [role="listbox"]') && (!targetSurface || surface.contains(targetSurface))) {
      return { surface, trigger };
    }
  }
  return targetSurface && container.contains(targetSurface)
    ? { surface: targetSurface, trigger: null }
    : null;
}

/**
 * Keeps keyboard focus inside an open modal and restores it to the invoking
 * control when the modal closes. `onEscape` is intentionally supplied by the
 * caller because security prompts may need Escape to mean an explicit deny.
 */
export function useModalFocus<T extends HTMLElement>(
  open: boolean,
  onEscape: () => void,
  /** Explicit invoker for actions launched from a short-lived portalled menu. */
  returnFocus?: HTMLElement | null,
): RefObject<T> {
  const containerRef = useRef<T>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const onEscapeRef = useRef(onEscape);
  onEscapeRef.current = onEscape;

  useEffect(() => {
    if (!open) return;

    const container = containerRef.current;
    if (!container) return;
    returnFocusRef.current = returnFocus?.isConnected
      ? returnFocus
      : document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;

    dismissTransientSurfaces();
    const previousLength = modalStack.length;
    modalStack.push(container);
    emitModalPresenceChange(previousLength);
    const initialItems = focusableElements(container);
    const preferred = container.querySelector<HTMLElement>("[data-modal-initial-focus]");
    const initialTarget = preferred && initialItems.includes(preferred)
      ? preferred
      : initialItems[0] ?? container;
    initialTarget.focus();

    const onKeyDown = (event: KeyboardEvent) => {
      if (modalStack[modalStack.length - 1] !== container) return;
      if (event.isComposing || event.keyCode === 229) return;
      const transient = ownedTransient(container, event.target);
      if (event.key === "Escape") {
        // A selector opened by this modal gets first refusal, including when
        // its options live in a portal. One Escape dismisses only that layer.
        if (transient) return;
        event.preventDefault();
        event.stopPropagation();
        event.stopImmediatePropagation();
        onEscapeRef.current();
        return;
      }
      if (event.key !== "Tab") return;

      const items = focusableElements(container);
      if (items.length === 0) {
        event.preventDefault();
        container.focus();
        return;
      }
      const firstItem = items[0];
      const lastItem = items[items.length - 1];
      const active = document.activeElement;
      if (transient?.trigger && !container.contains(active)) {
        // Browser Tab order follows the portal at the end of body. Resume at
        // the invoking selector's next/previous field in the modal instead.
        event.preventDefault();
        const triggerIndex = items.indexOf(transient.trigger);
        const index = (triggerIndex + (event.shiftKey ? -1 : 1) + items.length) % items.length;
        dismissTransientSurfaces();
        items[index].focus();
        return;
      }
      if (event.shiftKey && (active === firstItem || !container.contains(active))) {
        event.preventDefault();
        lastItem.focus();
      } else if (!event.shiftKey && (active === lastItem || !container.contains(active))) {
        event.preventDefault();
        firstItem.focus();
      }
    };

    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      const cleanupPreviousLength = modalStack.length;
      const stackIndex = modalStack.lastIndexOf(container);
      if (stackIndex >= 0) modalStack.splice(stackIndex, 1);
      emitModalPresenceChange(cleanupPreviousLength);
      const returnTarget = returnFocusRef.current;
      if (returnTarget?.isConnected) returnTarget.focus();
    };
  }, [open]);

  return containerRef;
}
