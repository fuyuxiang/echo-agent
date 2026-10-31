import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from "react";

/**
 * Keep the transcript pinned while the user is following the latest output.
 * This tolerance is only for native scroll rounding when the user returns to
 * the bottom. User intent is detected from scroll direction / input events,
 * never from this distance.
 */
export const STICK_TO_BOTTOM_THRESHOLD = 2;

export function isNearScrollBottom(
  element: HTMLElement,
  threshold = STICK_TO_BOTTOM_THRESHOLD,
): boolean {
  const remaining =
    element.scrollHeight - element.clientHeight - element.scrollTop;
  return remaining <= threshold;
}

interface UseStickToBottomOptions {
  /** A value that changes whenever rendered transcript data changes. */
  contentVersion: unknown;
  /** Whether the focused conversation currently has an active response. */
  streaming: boolean;
  /** Switching conversations must not inherit the old conversation's position. */
  sessionId: string | null;
  threshold?: number;
}

interface StickToBottomRefs {
  scrollRef: RefObject<HTMLDivElement>;
  contentRef: RefObject<HTMLDivElement>;
  /** Whether new transcript content is currently allowed to follow the bottom. */
  following: boolean;
  /** Preserve the reader's current viewport and cancel pending alignment. */
  pauseFollowing: () => void;
  /** Return to the latest content and resume automatic following. */
  scrollToBottom: () => void;
}

/**
 * Bottom-following for a streaming transcript.
 *
 * Message state alone is not a sufficient signal: Markdown renderers, images,
 * tool cards and the composer can all change size after React has committed.
 * ResizeObserver covers those late layout changes, while followRef preserves a
 * user's deliberate position when they scroll up to read earlier messages.
 */
export function useStickToBottom({
  contentVersion,
  streaming,
  sessionId,
  threshold = STICK_TO_BOTTOM_THRESHOLD,
}: UseStickToBottomOptions): StickToBottomRefs {
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const followRef = useRef(true);
  const [following, setFollowing] = useState(true);
  const frameRef = useRef<number | null>(null);
  const lastScrollTopRef = useRef(0);
  const lastScrollRangeRef = useRef(0);
  const touchYRef = useRef<number | null>(null);
  const previousSessionRef = useRef(sessionId);
  type ReadingPosition = { top: number; following: boolean; anchor?: { id: string; offset: number } };
  const positions = useRef(new Map<string | null, ReadingPosition>());
  const rememberPosition = useCallback(() => {
    const viewport = scrollRef.current;
    if (!viewport) return;
    const edge = viewport.getBoundingClientRect().top;
    const row = Array.from(contentRef.current?.querySelectorAll<HTMLElement>("[data-msg-id]") ?? [])
      .find(node => node.getBoundingClientRect().bottom > edge);
    positions.current.set(previousSessionRef.current, { top: viewport.scrollTop, following: followRef.current,
      anchor: row ? { id: row.dataset.msgId!, offset: row.getBoundingClientRect().top - edge } : undefined });
    if (positions.current.size > 50) positions.current.delete(positions.current.keys().next().value!);
  }, []);
  const restoreReadingAnchor = useCallback(() => {
    const viewport = scrollRef.current;
    const position = positions.current.get(previousSessionRef.current);
    if (!viewport || !position || followRef.current) return;
    const row = Array.from(contentRef.current?.querySelectorAll<HTMLElement>("[data-msg-id]") ?? [])
      .find(node => node.dataset.msgId === position.anchor?.id);
    if (row && position.anchor) {
      viewport.scrollTop += row.getBoundingClientRect().top - viewport.getBoundingClientRect().top - position.anchor.offset;
    } else viewport.scrollTop = position.top;
    lastScrollTopRef.current = viewport.scrollTop;
    lastScrollRangeRef.current = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
  }, []);
  const streamingRef = useRef(streaming);
  streamingRef.current = streaming;

  const cancelScheduledAlignment = useCallback(() => {
    if (frameRef.current === null) return;
    window.cancelAnimationFrame(frameRef.current);
    frameRef.current = null;
  }, []);

  const updateFollowing = useCallback((next: boolean) => {
    if (followRef.current === next) return;
    followRef.current = next;
    setFollowing(next);
  }, []);

  const pauseFollowing = useCallback(() => {
    updateFollowing(false);
    rememberPosition();
    cancelScheduledAlignment();
  }, [cancelScheduledAlignment, updateFollowing, rememberPosition]);

  const alignToBottom = useCallback(
    (force = false) => {
      if (force) updateFollowing(true);
      if (!followRef.current) return;

      const element = scrollRef.current;
      if (!element) return;

      // Align during the current layout pass, then once more on the next frame
      // for renderers that finish measuring just after React's commit.
      element.scrollTop = Math.max(
        0,
        element.scrollHeight - element.clientHeight,
      );
      lastScrollTopRef.current = element.scrollTop;
      lastScrollRangeRef.current = Math.max(0, element.scrollHeight - element.clientHeight);
      rememberPosition();
      cancelScheduledAlignment();
      frameRef.current = window.requestAnimationFrame(() => {
        frameRef.current = null;
        if (!followRef.current || scrollRef.current !== element) return;
        element.scrollTop = Math.max(
          0,
          element.scrollHeight - element.clientHeight,
        );
        lastScrollTopRef.current = element.scrollTop;
        lastScrollRangeRef.current = Math.max(0, element.scrollHeight - element.clientHeight);
        rememberPosition();
      });
    },
    [cancelScheduledAlignment, updateFollowing, rememberPosition],
  );

  const scrollToBottom = useCallback(() => {
    alignToBottom(true);
  }, [alignToBottom]);

  // Run before paint so each streamed state update does not visibly flash at
  // the old position. Only switching conversations forces the view to the
  // bottom: an automatically queued turn must not steal a reader's position.
  useLayoutEffect(() => {
    const sessionChanged = previousSessionRef.current !== sessionId;
    previousSessionRef.current = sessionId;

    if (sessionChanged) {
      cancelScheduledAlignment();
      const saved = positions.current.get(sessionId);
      updateFollowing(saved?.following ?? true);
      if (saved && scrollRef.current) scrollRef.current.scrollTop = saved.top;
    }
    if (followRef.current) alignToBottom();
    else restoreReadingAnchor();
  }, [alignToBottom, cancelScheduledAlignment, contentVersion, restoreReadingAnchor, sessionId, streaming, updateFollowing]);

  useEffect(() => {
    const scrollElement = scrollRef.current;
    const contentElement = contentRef.current;
    if (!scrollElement || !contentElement) return;

    lastScrollTopRef.current = scrollElement.scrollTop;
    lastScrollRangeRef.current = Math.max(0, scrollElement.scrollHeight - scrollElement.clientHeight);

    const handleScroll = () => {
      const nextScrollTop = scrollElement.scrollTop;
      const scrollRange = Math.max(0, scrollElement.scrollHeight - scrollElement.clientHeight);
      // Reflow can shrink the scroll range and clamp the viewport to its new
      // bottom before ResizeObserver runs. That is not a request to stop
      // following. Wheel/touch intent still pauses immediately, even here.
      const clampedToBottom = followRef.current
        && scrollRange < lastScrollRangeRef.current
        && nextScrollTop >= scrollRange - 0.5;
      // A decrease is an unambiguous attempt to inspect earlier content. Stop
      // following immediately, including for tiny scrollbar/trackpad moves.
      if (nextScrollTop < lastScrollTopRef.current - 0.5 && !clampedToBottom) {
        pauseFollowing();
      } else if (isNearScrollBottom(scrollElement, threshold)) {
        // Following resumes naturally only after the user reaches the bottom.
        updateFollowing(true);
      }
      lastScrollTopRef.current = nextScrollTop;
      lastScrollRangeRef.current = scrollRange;
      rememberPosition();
    };
    const handleWheel = (event: WheelEvent) => {
      if (event.deltaY < 0 && scrollElement.scrollTop > 0) pauseFollowing();
    };
    const handleTouchStart = (event: TouchEvent) => {
      touchYRef.current = event.touches[0]?.clientY ?? null;
    };
    const handleTouchMove = (event: TouchEvent) => {
      const nextY = event.touches[0]?.clientY;
      const previousY = touchYRef.current;
      // Moving the finger down scrolls the transcript towards older content.
      if (
        nextY !== undefined
        && previousY !== null
        && nextY > previousY
        && scrollElement.scrollTop > 0
      ) {
        pauseFollowing();
      }
      touchYRef.current = nextY ?? null;
    };
    const handleTouchEnd = () => {
      touchYRef.current = null;
    };
    const handleReadingAction = (event: Event) => {
      // Clipboard writes do not emit a native `copy` event. Mark copy affordances
      // explicitly and stop following before their async clipboard work or the
      // next streamed layout update can move the view. `click` also covers
      // keyboard activation; pointer-down handles mouse/touch at the earliest point.
      const target = event.target as Element | null;
      // Expanding records is explicit reading intent, including in completed
      // turns. Pause before their layout grows so the first stage stays put.
      if (target?.closest?.("[data-chat-reading]")
        || streamingRef.current && target?.closest?.("[data-chat-copy]")) pauseFollowing();
    };
    const handleCopy = (event: ClipboardEvent) => {
      // Also cover keyboard/context-menu copying of selected transcript text,
      // without pausing when the user copies from the composer or another pane.
      if (!streamingRef.current) return;
      const eventTarget = event.target as Node | null;
      const selectionAnchor = scrollElement.ownerDocument.defaultView
        ?.getSelection()?.anchorNode ?? null;
      if (
        (eventTarget && scrollElement.contains(eventTarget))
        || (selectionAnchor && scrollElement.contains(selectionAnchor))
      ) {
        pauseFollowing();
      }
    };
    const handleLateLayout = () => {
      if (followRef.current) alignToBottom();
      else restoreReadingAnchor();
    };

    scrollElement.addEventListener("scroll", handleScroll, { passive: true });
    scrollElement.addEventListener("wheel", handleWheel, { passive: true });
    scrollElement.addEventListener("touchstart", handleTouchStart, {
      passive: true,
    });
    scrollElement.addEventListener("touchmove", handleTouchMove, { passive: true });
    scrollElement.addEventListener("touchend", handleTouchEnd, { passive: true });
    scrollElement.addEventListener("touchcancel", handleTouchEnd, { passive: true });
    scrollElement.addEventListener("pointerdown", handleReadingAction, { passive: true });
    scrollElement.addEventListener("click", handleReadingAction, { passive: true });
    scrollElement.ownerDocument.addEventListener("copy", handleCopy);

    // The content observer catches asynchronously expanding Markdown, images,
    // and tool cards. Observing the viewport catches footer/composer resizing.
    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(handleLateLayout);
    resizeObserver?.observe(scrollElement);
    resizeObserver?.observe(contentElement);

    // Also cover image load in older WebViews where ResizeObserver is absent.
    contentElement.addEventListener("load", handleLateLayout, true);

    return () => {
      scrollElement.removeEventListener("scroll", handleScroll);
      scrollElement.removeEventListener("wheel", handleWheel);
      scrollElement.removeEventListener("touchstart", handleTouchStart);
      scrollElement.removeEventListener("touchmove", handleTouchMove);
      scrollElement.removeEventListener("touchend", handleTouchEnd);
      scrollElement.removeEventListener("touchcancel", handleTouchEnd);
      scrollElement.removeEventListener("pointerdown", handleReadingAction);
      scrollElement.removeEventListener("click", handleReadingAction);
      scrollElement.ownerDocument.removeEventListener("copy", handleCopy);
      contentElement.removeEventListener("load", handleLateLayout, true);
      resizeObserver?.disconnect();
    };
  }, [alignToBottom, pauseFollowing, rememberPosition, restoreReadingAnchor, sessionId, threshold, updateFollowing]);

  useEffect(() => cancelScheduledAlignment, [cancelScheduledAlignment]);

  return {
    scrollRef,
    contentRef,
    following,
    pauseFollowing,
    scrollToBottom,
  };
}
