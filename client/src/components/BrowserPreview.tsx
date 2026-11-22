import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";
import { ExternalLink } from "lucide-react";
import {
  automationBrowserAction,
  automationBrowserView,
  type BrowserPageInput,
  type BrowserPanelAction,
  type BrowserPanelView,
} from "@/lib/automation-client";
import { developmentEndpoint, normalizePreviewUrl } from "@/lib/browser-preview";
import { openUrl } from "@/lib/agent-client";
import { friendlyError } from "@/lib/error-format";
import {
  ChevronLeftIcon, ChevronRightIcon, RefreshCwIcon, AddIcon, CloseIcon,
} from "@/foundation/components/Icon/icons";

export interface BrowserOpenRequest {
  url: string;
  sequence: number;
}

interface BrowserPreviewProps {
  sessionId?: string;
  active: boolean;
  request?: BrowserOpenRequest;
  agentWorking?: boolean;
  browserAvailable?: boolean;
  onRequestHandled?: (sequence: number) => void;
  onTakeOver?: () => Promise<boolean | void>;
}

type Navigation = Extract<BrowserPanelAction, { url: string }>;
type Gesture = { pointerId: number; x: number; y: number; start: [number, number]; frame: Page; moved: boolean };
type Page = NonNullable<BrowserPanelView["page"]>;

function modifiers(event: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }) {
  return Number(event.altKey) | (Number(event.ctrlKey) << 1)
    | (Number(event.metaKey) << 2) | (Number(event.shiftKey) << 3);
}

export function BrowserPreview({
  sessionId, active, request, agentWorking = false, browserAvailable = true, onRequestHandled, onTakeOver,
}: BrowserPreviewProps) {
  const [view, setView] = useState<BrowserPanelView | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [permission, setPermission] = useState<{ action: Navigation; endpoint: string; target: string | null } | null>(null);
  const [busy, setBusy] = useState(false);
  const [takingOver, setTakingOver] = useState(false);
  const [externalOpening, setExternalOpening] = useState(false);
  const [externalOpened, setExternalOpened] = useState(false);
  const [keyboardFocused, setKeyboardFocused] = useState(false);
  const [actionError, setActionError] = useState("");
  const [viewError, setViewError] = useState("");
  const viewport = useRef<HTMLDivElement>(null);
  const image = useRef<HTMLImageElement>(null);
  const keyboard = useRef<HTMLTextAreaElement>(null);
  const address = useRef<HTMLInputElement>(null);
  const queue = useRef(Promise.resolve());
  const pendingInput = useRef(0);
  const handled = useRef<number | undefined>(undefined);
  const generation = useRef(0);
  const mountedActive = useRef(false);
  const queueOwner = useRef(sessionId);
  const refreshNow = useRef(() => {});
  const latestPage = useRef<Page | null>(null);
  const gesture = useRef<Gesture | null>(null);
  const lastMove = useRef(0);
  const lastClick = useRef<{ point: [number, number]; time: number; count: number; target: string } | null>(null);

  const page = view?.page;
  const tab = view?.tabs.find(item => item.id === view.activeId);
  const currentUrl = page?.document.url ?? tab?.url ?? "";
  const shownUrl = currentUrl === "about:blank" ? "" : currentUrl;
  const normalized = normalizePreviewUrl(draft ?? shownUrl);
  const disabled = !sessionId || busy || agentWorking;
  const navigationDisabled = disabled || !browserAvailable || view?.external || !view?.activeId;

  useLayoutEffect(() => {
    if (queueOwner.current !== sessionId || !active) {
      queueOwner.current = sessionId;
      generation.current++;
      setPermission(null);
      setBusy(false);
    }
    mountedActive.current = active;
    return () => {
      mountedActive.current = false;
      gesture.current = null;
      lastClick.current = null;
    };
  }, [active, sessionId]);

  useEffect(() => {
    if (!active || !sessionId || !("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    let running = false;
    let timer: ReturnType<typeof setTimeout>;
    let resizeTimer: ReturnType<typeof setTimeout>;
    let size: [number, number] | null = null;
    let failures = 0;
    let lastRefresh = 0;
    const refresh = async () => {
      if (disposed || running) return;
      clearTimeout(timer);
      running = true;
      lastRefresh = performance.now();
      try {
        if (document.visibilityState === "hidden") return;
        const known = latestPage.current;
        const next = await automationBrowserView(sessionId, size, known?.frameId ?? null);
        if (disposed || !next || !Array.isArray(next.tabs)) return;
        // Keep the last usable frame while Chromium is navigating or scrolling within the same tab.
        if (!next.external && !next.page && next.activeId === known?.document.targetId) return;
        if (next.page?.frameId && next.page.frameId === known?.frameId && !next.page.image) {
          next.page = { ...next.page, image: known.image };
        }
        latestPage.current = next.page;
        failures = 0;
        setView(next);
        setViewError("");
      } catch (error) {
        if (!disposed) {
          failures++;
          setViewError(friendlyError(error));
        }
      } finally {
        running = false;
        if (!disposed) timer = setTimeout(refresh, failures ? 1500 : agentWorking ? 950 : 400);
      }
    };
    const measure = () => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        const bounds = viewport.current?.getBoundingClientRect();
        if (bounds && bounds.width > 0 && bounds.height > 0) {
          const next = [Math.round(bounds.width), Math.round(bounds.height)] as const;
          if (size?.[0] !== next[0] || size?.[1] !== next[1]) {
            size = [...next];
            void refresh();
          }
        }
      }, 160);
    };
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    if (viewport.current) observer?.observe(viewport.current);
    refreshNow.current = () => {
      if (running) return;
      clearTimeout(timer);
      timer = setTimeout(refresh, Math.max(0, 120 - (performance.now() - lastRefresh)));
    };
    measure();
    void refresh();
    return () => {
      disposed = true;
      observer?.disconnect();
      clearTimeout(timer);
      clearTimeout(resizeTimer);
      refreshNow.current = () => {};
    };
  }, [active, sessionId, agentWorking]);

  const perform = useCallback((action: BrowserPanelAction, target: string | null) => {
    if (!sessionId || !active) return;
    const input = action.kind === "input";
    const owner = generation.current;
    if (input) {
      if ((action.input.kind === "move" && pendingInput.current > 0)
        || (action.input.kind === "wheel" && pendingInput.current >= 12)) return;
      if (pendingInput.current >= 48) {
        setActionError("网页响应较慢，部分输入未发送，请稍后重试");
        return;
      }
      pendingInput.current++;
    } else {
      setBusy(true);
      setPermission(null);
      setActionError("");
      setExternalOpened(false);
    }
    queue.current = queue.current.then(async () => {
      try {
        if (!mountedActive.current || generation.current !== owner) return;
        await automationBrowserAction(sessionId, target, action);
        if (!input && action.kind !== "revokeDevelopment" && mountedActive.current && generation.current === owner) setDraft(null);
      } catch (error) {
        if (mountedActive.current && generation.current === owner) setActionError(friendlyError(error));
      } finally {
        if (input) pendingInput.current--;
        if (!mountedActive.current || generation.current !== owner) return;
        if (!input) setBusy(false);
        if (!input || action.input.kind !== "move") refreshNow.current();
      }
    });
  }, [active, sessionId]);

  const navigate = useCallback((action: Navigation) => {
    if (agentWorking || view?.external) {
      setActionError(agentWorking ? "暂停并接手后可在面板打开此网址，也可以用系统浏览器打开。" : "可在已连接的浏览器中操作，或用系统浏览器打开此网址。");
      return;
    }
    if (!browserAvailable) {
      setActionError("当前设备没有可用的任务浏览器，可用系统浏览器打开此网址。");
      return;
    }
    const target = action.kind === "navigate" && !viewError ? view?.activeId ?? null : null;
    const endpoint = developmentEndpoint(action.url);
    if (endpoint && !view?.allowPrivateNetwork && !view?.developmentEndpoints?.includes(endpoint)) {
      setPermission({ action, endpoint, target });
      return;
    }
    setPermission(null);
    perform(action, target);
  }, [agentWorking, browserAvailable, perform, view, viewError]);

  useEffect(() => {
    if (!active || !sessionId || busy || !request || handled.current === request.sequence) return;
    handled.current = request.sequence;
    onRequestHandled?.(request.sequence);
    setDraft(request.url);
    setExternalOpened(false);
    setPermission(null);
    const url = normalizePreviewUrl(request.url);
    if (!url) {
      setActionError("网址格式无效");
      return;
    }
    navigate({ kind: "open", url });
  }, [active, busy, navigate, onRequestHandled, request, sessionId]);

  const sendInput = useCallback((input: BrowserPageInput, frame = page) => {
    if (!frame?.image || !frame.frameId || disabled || viewError) return;
    perform({ kind: "input", document: frame.document, frameId: frame.frameId, input }, frame.document.targetId);
  }, [disabled, page, perform, viewError]);

  const point = useCallback((x: number, y: number): [number, number] | null => {
    const element = image.current;
    const rect = element?.getBoundingClientRect();
    if (!page || !element?.complete || !element.naturalWidth || !element.naturalHeight
      || !rect?.width || !rect.height || !page.width || !page.height) return null;
    const scale = Math.min(rect.width / element.naturalWidth, rect.height / element.naturalHeight);
    const width = element.naturalWidth * scale;
    const height = element.naturalHeight * scale;
    const fractionX = (x - rect.left - (rect.width - width) / 2) / width;
    const fractionY = (y - rect.top - (rect.height - height) / 2) / height;
    if (fractionX < 0 || fractionY < 0 || fractionX >= 1 || fractionY >= 1) return null;
    return [fractionX * page.width, fractionY * page.height];
  }, [page]);

  useEffect(() => {
    const element = viewport.current;
    if (!element || !active || disabled) return;
    const onWheel = (event: WheelEvent) => {
      const location = point(event.clientX, event.clientY);
      if (!location) return;
      event.preventDefault();
      const [x, y] = location;
      const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? (page?.height ?? 600) : 1;
      sendInput({ kind: "wheel", x, y, dx: event.deltaX * unit, dy: event.deltaY * unit });
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  }, [active, disabled, page?.height, point, sendInput]);

  function onPointerDown(event: PointerEvent<HTMLImageElement>) {
    if (disabled || viewError || !page?.frameId || event.button > 2) return;
    const start = point(event.clientX, event.clientY);
    if (!start) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture?.(event.pointerId);
    gesture.current = { pointerId: event.pointerId, x: event.clientX, y: event.clientY, start, frame: page, moved: false };
  }

  function onPointerMove(event: PointerEvent<HTMLImageElement>) {
    const pending = gesture.current;
    if (pending?.pointerId === event.pointerId) {
      pending.moved ||= Math.hypot(event.clientX - pending.x, event.clientY - pending.y) > 5;
      return;
    }
    if (event.buttons || performance.now() - lastMove.current < 85) return;
    const location = point(event.clientX, event.clientY);
    if (!location) return;
    lastMove.current = performance.now();
    sendInput({ kind: "move", x: location[0], y: location[1] });
  }

  function onPointerUp(event: PointerEvent<HTMLImageElement>) {
    const pending = gesture.current;
    gesture.current = null;
    if (!pending || pending.pointerId !== event.pointerId || disabled || viewError || !page?.frameId) return;
    const end = point(event.clientX, event.clientY);
    if (!end) return;
    if (pending.frame.document.targetId !== page.document.targetId
      || pending.frame.document.loaderId !== page.document.loaderId
      || pending.frame.width !== page.width || pending.frame.height !== page.height) {
      setActionError("网页画面已变化，请重试");
      return;
    }
    const [x, y] = pending.start;
    const moved = pending.moved || Math.hypot(event.clientX - pending.x, event.clientY - pending.y) > 5;
    if (moved && event.button !== 0) return;
    const button = event.button === 0 ? "left" : event.button === 1 ? "middle" : "right";
    const previous = lastClick.current;
    const time = performance.now();
    const count = button === "left" && !moved && previous?.target === page.document.targetId
      && time - previous.time < 400 && Math.hypot(x - previous.point[0], y - previous.point[1]) < 5
      ? previous.count % 3 + 1 : 1;
    lastClick.current = button === "left" && !moved ? { point: pending.start, time, count, target: page.document.targetId } : null;
    if (button === "left") keyboard.current?.focus({ preventScroll: true });
    setActionError("");
    sendInput({ kind: "pointer", x, y, destination: moved ? end : null, button, count, modifiers: modifiers(event) }, pending.frame);
  }

  function onPageKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    if (event.key === "Escape" && !event.shiftKey) {
      event.preventDefault();
      address.current?.focus();
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "v") return;
    event.preventDefault();
    const flags = event.key === "Escape" ? modifiers(event) & ~8 : modifiers(event);
    sendInput({ kind: "key", key: event.key, code: event.code, virtualKey: event.keyCode, modifiers: flags });
  }

  async function takeOver() {
    setTakingOver(true);
    setActionError("");
    try {
      if (await onTakeOver?.() === false) setActionError("暂停尚未确认，请重试");
      refreshNow.current();
    } catch (error) {
      setActionError(friendlyError(error));
    } finally {
      setTakingOver(false);
    }
  }

  async function openExternally() {
    if (!normalized) return;
    setExternalOpening(true);
    setExternalOpened(false);
    setActionError("");
    try {
      await openUrl(normalized);
      setExternalOpened(true);
    } catch (error) {
      setActionError(friendlyError(error));
    } finally {
      setExternalOpening(false);
    }
  }

  return (
    <div className="browser-preview" role="region" aria-label="网页">
      {!!view?.tabs.length && <div className="browser-preview__tabs" aria-label="网页标签">
        {view.tabs.map(item => <div className="browser-preview__tab" key={item.id}>
          <button type="button" aria-pressed={item.id === view.activeId} title={item.url}
            disabled={disabled || view.external} onClick={() => perform({ kind: "select", id: item.id }, null)}>
            {item.title || (item.url === "about:blank" ? "新标签页" : item.url)}
          </button>
          {!view.external && <button type="button" aria-label={`关闭 ${item.title || "标签页"}`} disabled={disabled}
            onClick={() => perform({ kind: "close", id: item.id }, null)}><CloseIcon size="sm" /></button>}
        </div>)}
      </div>}
      <form className="browser-preview__toolbar" onSubmit={event => {
        event.preventDefault();
        if (normalized && !disabled && !view?.external) navigate({ kind: "navigate", url: normalized });
      }}>
        <button type="button" className="browser-preview__nav-btn" aria-label="后退" title="后退"
          disabled={navigationDisabled || !page?.canGoBack} onClick={() => perform({ kind: "back" }, view?.activeId ?? null)}>
          <ChevronLeftIcon size="sm" />
        </button>
        <button type="button" className="browser-preview__nav-btn" aria-label="前进" title="前进"
          disabled={navigationDisabled || !page?.canGoForward} onClick={() => perform({ kind: "forward" }, view?.activeId ?? null)}>
          <ChevronRightIcon size="sm" />
        </button>
        <button type="button" className="browser-preview__nav-btn" aria-label="刷新" title="刷新"
          disabled={navigationDisabled || !shownUrl || !!viewError} onClick={() => perform({ kind: "reload" }, view?.activeId ?? null)}>
          <RefreshCwIcon size="sm" />
        </button>
        <input ref={address} className="browser-preview__input" aria-label="预览网址" value={draft ?? shownUrl}
          readOnly={busy} placeholder="输入网址" onChange={event => {
            setDraft(event.target.value);
            setExternalOpened(false);
            setPermission(null);
            setActionError("");
          }}
          onKeyDown={event => {
            if (event.key === "Enter" && (event.nativeEvent.isComposing || event.keyCode === 229)) event.preventDefault();
            if (event.key === "Escape") {
              setDraft(null);
              setPermission(null);
            }
          }} />
        <button type="submit" className="browser-preview__go" disabled={disabled || !browserAvailable || !normalized || view?.external}>打开</button>
        <button type="button" className="browser-preview__nav-btn" aria-label="新建网页标签" title="新建网页标签"
          disabled={disabled || !browserAvailable || view?.external} onClick={() => perform({ kind: "new" }, null)}><AddIcon size="sm" /></button>
        <button type="button" className="browser-preview__nav-btn" aria-label="用系统浏览器打开" title="用系统浏览器打开地址栏中的网址"
          disabled={!normalized || externalOpening} aria-busy={externalOpening} onClick={() => void openExternally()}>
          <ExternalLink size={14} aria-hidden="true" />
        </button>
      </form>
      {(actionError || viewError || page?.error) && <p className="browser-preview__status browser-preview__status--error" role="alert">
        {actionError || viewError || "网页加载失败。可以重试或用系统浏览器打开。"}
      </p>}
      {externalOpened && <p className="browser-preview__status" role="status">已在系统浏览器中打开。</p>}
      {agentWorking && <div className="browser-preview__status browser-preview__status-actions">
        <span role="status">Agent 正在控制当前任务。</span>
        <button type="button" disabled={!onTakeOver || takingOver} onClick={() => void takeOver()}>
          {takingOver ? "正在暂停…" : "暂停并接手"}
        </button>
      </div>}
      {permission && <div className="browser-preview__status browser-preview__status-actions" role="group" aria-label="本地端口授权">
        <span>允许本任务访问 {permission.endpoint}？该权限也适用于此任务的 Agent 和其他标签页。</span>
        <button type="button" disabled={disabled} onClick={() => {
          setPermission(null);
          perform({ ...permission.action, allowDevelopment: true }, permission.target);
        }}>允许并打开</button>
        <button type="button" onClick={() => setPermission(null)}>取消</button>
      </div>}
      {!!view?.developmentEndpoints?.length && <div className="browser-preview__status browser-preview__status-actions">
        <span title={view.developmentEndpoints.join("、")}>已授权端口 {view.developmentEndpoints.join("、")}</span>
        <button type="button" disabled={busy} onClick={() => perform({ kind: "revokeDevelopment" }, null)}>撤销本地端口授权</button>
      </div>}
      <div className="browser-preview__surface" ref={viewport} aria-busy={busy}>
        {page?.image ? <>
          <img className="browser-preview__frame" ref={image} alt={tab?.title || "当前网页"}
            src={`data:image/jpeg;base64,${page.image}`} draggable={false}
            onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp}
            onPointerCancel={() => { gesture.current = null; }}
            onContextMenu={event => event.preventDefault()} />
          <textarea ref={keyboard} className="browser-preview__keyboard" aria-label="网页键盘输入" tabIndex={-1}
            disabled={disabled} onFocus={() => setKeyboardFocused(true)} onBlur={() => setKeyboardFocused(false)}
            onKeyDown={onPageKey} onCompositionEnd={event => {
              sendInput({ kind: "text", text: event.data });
              event.currentTarget.value = "";
            }} onPaste={event => {
              event.preventDefault();
              sendInput({ kind: "text", text: event.clipboardData.getData("text/plain") });
            }} />
        </> : <p className="browser-preview__empty">
          {busy ? "正在打开网页…" : view?.external ? "在已连接的浏览器中查看和操作网页。" : !browserAvailable ? "可用系统浏览器打开地址栏中的网址。" : shownUrl && !page?.error ? "正在加载网页…" : "输入网址以打开网页。"}
        </p>}
      </div>
      {page?.image && <div className="browser-preview__keyboard-bar">
        <button type="button" disabled={disabled} onClick={() => keyboard.current?.focus({ preventScroll: true })}>使用网页键盘</button>
        {keyboardFocused && <span role="status">Esc 返回工具栏，Shift + Esc 将 Esc 发送给网页</span>}
      </div>}
    </div>
  );
}
