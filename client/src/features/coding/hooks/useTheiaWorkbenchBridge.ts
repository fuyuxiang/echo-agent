import { useCallback, useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { ChatMessage } from "@/stores/session-store";
import { localPreviewUrls } from "../lib/preview-url";
import { idePath, relativeToWorkspace } from "../lib/windows-path";
import type { TheiaIdeFrameHandle } from "../TheiaIdeFrame";

const maximumAgentWidth = (workbenchWidth: number) => Math.max(300, Math.min(800, workbenchWidth - 328));

/** State and layout owned by the embedded IDE bridge, independent of task phases. */
export function useTheiaWorkbenchBridge(
  cwd: string,
  messages: ChatMessage[],
  onToast?: (message: string) => void,
  active = true,
) {
  const [theiaActiveFile, setTheiaActiveFile] = useState<string | null>(null);
  const [theiaActiveSymbol, setTheiaActiveSymbol] = useState<{ path: string; symbol: string } | null>(null);
  const [theiaPreviewInput, setTheiaPreviewInput] = useState("");
  const [previewUrls, setPreviewUrls] = useState<string[]>([]);
  const [theiaPreviewOpen, setTheiaPreviewOpen] = useState(false);
  const [theiaAgentOpen, setTheiaAgentOpen] = useState(true);
  const [preferredAgentWidth, setPreferredAgentWidth] = useState(() => {
    try {
      const saved = Number(window.localStorage.getItem("echo-agent-panel-width"));
      return saved >= 300 && saved <= 800 ? saved : 410;
    } catch {
      return 410;
    }
  });
  const [agentMaximumWidth, setAgentMaximumWidth] = useState(800);
  const [agentPreviewWidth, setAgentPreviewWidth] = useState<number | null>(null);
  const [agentResizing, setAgentResizing] = useState(false);
  const agentWidth = Math.min(agentMaximumWidth, agentPreviewWidth ?? preferredAgentWidth);
  const [theiaDirtyCount, setTheiaDirtyCount] = useState<number | null>(null);
  const [theiaPreviewRequest, setTheiaPreviewRequest] = useState<{ url: string; id: number } | null>(null);
  const [theiaOpenFileRequest, setTheiaOpenFileRequest] = useState<{ path: string; id: number; line?: number } | null>(null);
  const theiaFrameRef = useRef<TheiaIdeFrameHandle>(null);
  const theiaPreviewRef = useRef<HTMLDivElement>(null);
  const workbenchRef = useRef<HTMLDivElement>(null);
  const agentResizeCleanupRef = useRef<(() => void) | null>(null);
  const resizeEnabled = useRef(active && theiaAgentOpen);
  resizeEnabled.current = active && theiaAgentOpen;

  const onDetectedPreviewUrl = useCallback((url: string) => {
    setPreviewUrls((current) => current[0] === url
      ? current : [url, ...current.filter((item) => item !== url)].slice(0, 4));
  }, []);
  useEffect(() => {
    for (const url of localPreviewUrls(messages)) onDetectedPreviewUrl(url);
  }, [messages, onDetectedPreviewUrl]);

  const openTheiaFile = useCallback((rawPath: string, line?: number) => {
    const root = idePath(cwd);
    const path = idePath(rawPath).replace(/^\.\//, "").replace(/\/\.\//g, "/");
    if (!path || path.split("/").includes("..")) {
      onToast?.("文件路径无效");
      return;
    }
    const absolute = path.startsWith("/") || /^[A-Za-z]:\//.test(path) ? path : `${root.replace(/\/+$/, "")}/${path}`;
    if (!relativeToWorkspace(root, absolute)) {
      onToast?.("只能打开当前项目内的文件");
      return;
    }
    setTheiaOpenFileRequest({ path: absolute, line, id: Date.now() });
  }, [cwd, onToast]);

  useEffect(() => {
    if (!theiaPreviewOpen) return;
    const closeOnOutsideClick = (event: MouseEvent) => {
      if (!theiaPreviewRef.current?.contains(event.target as Node)) setTheiaPreviewOpen(false);
    };
    document.addEventListener("mousedown", closeOnOutsideClick);
    return () => document.removeEventListener("mousedown", closeOnOutsideClick);
  }, [theiaPreviewOpen]);

  const widthAtPointer = useCallback((clientX: number) => {
    const element = workbenchRef.current;
    if (!resizeEnabled.current || !element || element.closest("[hidden]")) return null;
    const bounds = element.getBoundingClientRect();
    if (bounds.width <= 0) return null;
    const maximum = maximumAgentWidth(bounds.width);
    setAgentMaximumWidth(maximum);
    return Math.round(Math.max(300, Math.min(maximum, bounds.right - clientX)));
  }, []);
  const startAgentResize = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (!resizeEnabled.current || event.button !== 0 || event.isPrimary === false || agentResizeCleanupRef.current) return;
    event.preventDefault();
    const handle = event.currentTarget;
    const pointerId = event.pointerId;
    let finished = false;
    handle.focus({ preventScroll: true });
    const onMove = (next: PointerEvent) => {
      if (next.pointerId !== pointerId) return;
      if (next.buttons === 0) { finish(); return; }
      const width = widthAtPointer(next.clientX);
      if (width === null) { finish(); return; }
      setAgentPreviewWidth(width);
    };
    const finish = (next?: PointerEvent) => {
      if (finished || next && next.pointerId !== pointerId) return;
      finished = true;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", key, true);
      handle.removeEventListener("lostpointercapture", finish);
      agentResizeCleanupRef.current = null;
      if (next?.type === "pointerup") {
        const width = widthAtPointer(next.clientX);
        if (width !== null) setPreferredAgentWidth(width);
      }
      setAgentPreviewWidth(null);
      setAgentResizing(false);
      if (handle.hasPointerCapture?.(pointerId)) handle.releasePointerCapture(pointerId);
    };
    const cancel = () => finish();
    const key = (next: KeyboardEvent) => {
      if (next.key !== "Escape") return;
      next.preventDefault();
      next.stopPropagation();
      finish();
    };
    agentResizeCleanupRef.current = cancel;
    setAgentResizing(true);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
    window.addEventListener("blur", cancel);
    window.addEventListener("keydown", key, true);
    handle.addEventListener("lostpointercapture", finish);
    try { handle.setPointerCapture(pointerId); } catch { /* Window listeners support older WebViews. */ }
  }, [widthAtPointer]);
  useLayoutEffect(() => () => agentResizeCleanupRef.current?.(), [cwd, theiaAgentOpen, active]);
  useEffect(() => {
    const element = workbenchRef.current;
    if (!element) return;
    const updateWidth = () => {
      const width = element.getBoundingClientRect().width;
      if (width <= 0) { agentResizeCleanupRef.current?.(); return; }
      const maximum = maximumAgentWidth(width);
      setAgentMaximumWidth(maximum);
    };
    updateWidth();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", updateWidth);
      return () => window.removeEventListener("resize", updateWidth);
    }
    const observer = new ResizeObserver(updateWidth);
    observer.observe(element);
    return () => observer.disconnect();
  }, [cwd, active]);
  useEffect(() => {
    try { window.localStorage.setItem("echo-agent-panel-width", String(preferredAgentWidth)); } catch { /* Optional layout preference. */ }
  }, [preferredAgentWidth]);

  const resizeAgentByKey = useCallback((key: string) => {
    if (!resizeEnabled.current || agentResizeCleanupRef.current) return;
    const width = workbenchRef.current?.getBoundingClientRect().width ?? 0;
    const maximum = width > 0 ? maximumAgentWidth(width) : 800;
    setAgentMaximumWidth(maximum);
    setPreferredAgentWidth((current) => Math.max(300, Math.min(maximum, Math.min(current, maximum) + (key === "ArrowLeft" ? 20 : -20))));
  }, []);
  const resetAgentWidth = useCallback(() => {
    if (!resizeEnabled.current || agentResizeCleanupRef.current) return;
    const width = workbenchRef.current?.getBoundingClientRect().width ?? 0;
    const maximum = width > 0 ? maximumAgentWidth(width) : 800;
    setAgentMaximumWidth(maximum);
    setPreferredAgentWidth(Math.min(410, maximum));
  }, []);

  return {
    theiaActiveFile, setTheiaActiveFile, theiaActiveSymbol, setTheiaActiveSymbol,
    theiaPreviewInput, setTheiaPreviewInput, previewUrls, setPreviewUrls,
    theiaPreviewOpen, setTheiaPreviewOpen, theiaAgentOpen, setTheiaAgentOpen,
    agentWidth, agentMaximumWidth, agentResizing, resetAgentWidth, theiaDirtyCount, setTheiaDirtyCount,
    theiaPreviewRequest, setTheiaPreviewRequest, theiaOpenFileRequest,
    theiaFrameRef, theiaPreviewRef, workbenchRef,
    onDetectedPreviewUrl, openTheiaFile, startAgentResize, resizeAgentByKey,
  };
}
