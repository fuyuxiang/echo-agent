/**
 * 嵌入式网页预览 —— 对齐 EchoAgent `context-viewer-components/browser-preview`。
 *
 * 输入 URL,经安全校验后在 sandbox iframe 中预览。无效/不安全 URL 显示提示。
 * 纯展示组件,核心校验逻辑在 lib/browser-preview(已测)。
 *
 * 增强(对齐 EchoAgent Toolbar)：后退/前进/刷新 + URL 历史 + 外部浏览器打开。
 * 由于浏览器 iframe 无法直接拦截目标页导航,这里维护一个「已访问 URL 栈」,
 * 回退/前进在栈内移动,刷新用 key 强制 iframe 重载。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  normalizePreviewUrl,
  previewTitle,
  PREVIEW_SANDBOX,
} from "@/lib/browser-preview";
import { invoke } from "@tauri-apps/api/core";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  RefreshCwIcon,
  OpenExternalIcon,
} from "@/foundation/components/Icon/icons";

interface BrowserPreviewProps {
  /** 初始 URL。 */
  url: string;
  /** URL 变更回调(可选)。 */
  onUrlChange?: (url: string) => void;
}

export function BrowserPreview({ url, onUrlChange }: BrowserPreviewProps) {
  // 初始 URL：仅当通过安全校验时才进入历史栈。
  const initialSafe = normalizePreviewUrl(url);
  const [input, setInput] = useState(url);
  // 已访问历史栈 + 当前指针（只存已规整的安全 URL）。
  const [history, setHistory] = useState<string[]>(initialSafe ? [initialSafe] : []);
  const [cursor, setCursor] = useState(initialSafe ? 0 : -1);
  // 刷新 key：递增以强制 iframe 重载。
  const [reloadKey, setReloadKey] = useState(0);
  const [externalState, setExternalState] = useState<"idle" | "opening" | "opened" | "failed">("idle");
  const [frameLoading, setFrameLoading] = useState(Boolean(initialSafe));
  const externalAttempt = useRef(0);
  const externalPending = useRef(false);

  // 当前实际加载的 URL（history[cursor] 或空）。
  const current = cursor >= 0 ? history[cursor] ?? "" : "";
  useEffect(() => {
    externalAttempt.current += 1;
    externalPending.current = false;
    setExternalState("idle");
    return () => { externalAttempt.current += 1; };
  }, [current]);

  // 外部 url prop 变化时同步（如标签切换带来的 URL 变化）。
  useEffect(() => {
    const safe = normalizePreviewUrl(url);
    if (safe && safe !== current) {
      pushHistory(safe);
      setInput(safe);
      setFrameLoading(true);
    } else if (!safe) {
      // A cleared selection must not keep another tab/session's page visible.
      setInput(url);
      setHistory([]);
      setCursor(-1);
      setFrameLoading(false);
    }
    setExternalState("idle");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [url]);

  const normalized = normalizePreviewUrl(input);

  const pushHistory = useCallback(
    (next: string) => {
      setHistory((prev) => {
        // 截断当前指针之后的历史（前进分支被覆盖）。
        const base = prev.slice(0, cursor + 1);
        // 去重连续相同。
        if (base.length > 0 && base[base.length - 1] === next) return base;
        const updated = [...base, next];
        setCursor(updated.length - 1);
        return updated;
      });
    },
    [cursor],
  );

  const navigate = useCallback(
    (target: string) => {
      const safe = normalizePreviewUrl(target);
      if (!safe) return;
      if (safe === current) setReloadKey((key) => key + 1);
      pushHistory(safe);
      setInput(safe);
      setFrameLoading(true);
      setExternalState("idle");
      onUrlChange?.(safe);
    },
    [onUrlChange, pushHistory, current],
  );

  const handleGo = useCallback(() => {
    if (normalized) navigate(normalized);
  }, [normalized, navigate]);

  const goBack = useCallback(() => {
    setCursor((c) => {
      if (c <= 0) return c;
      const next = c - 1;
      const u = history[next];
      setInput(u);
      setFrameLoading(true);
      setExternalState("idle");
      onUrlChange?.(u);
      return next;
    });
  }, [history, onUrlChange]);

  const goForward = useCallback(() => {
    setCursor((c) => {
      if (c >= history.length - 1) return c;
      const next = c + 1;
      const u = history[next];
      setInput(u);
      setFrameLoading(true);
      setExternalState("idle");
      onUrlChange?.(u);
      return next;
    });
  }, [history, onUrlChange]);

  const refresh = useCallback(() => {
    setReloadKey((k) => k + 1);
    setFrameLoading(true);
  }, []);

  const openExternal = useCallback(async () => {
    if (!current || externalPending.current) return;
    externalPending.current = true;
    const attempt = ++externalAttempt.current;
    setExternalState("opening");
    try {
      await invoke("open_url", { url: current });
      if (externalAttempt.current === attempt) setExternalState("opened");
    } catch {
      if (externalAttempt.current === attempt) setExternalState("failed");
    } finally {
      if (externalAttempt.current === attempt) externalPending.current = false;
    }
  }, [current]);

  const canGoBack = cursor > 0;
  const canGoForward = cursor >= 0 && cursor < history.length - 1;

  const frameTitle = useMemo(
    () => (current ? previewTitle(current) : "网页预览"),
    [current],
  );

  return (
    <div className="browser-preview" role="region" aria-label="网页预览">
      <div className="browser-preview__toolbar">
        <button
          type="button"
          className="browser-preview__nav-btn"
          onClick={goBack}
          disabled={!canGoBack}
          title="后退"
          aria-label="后退"
        >
          <ChevronLeftIcon size="sm" />
        </button>
        <button
          type="button"
          className="browser-preview__nav-btn"
          onClick={goForward}
          disabled={!canGoForward}
          title="前进"
          aria-label="前进"
        >
          <ChevronRightIcon size="sm" />
        </button>
        <button
          type="button"
          className="browser-preview__nav-btn"
          onClick={refresh}
          disabled={!current}
          title="刷新"
          aria-label="刷新"
        >
          <RefreshCwIcon size="sm" />
        </button>
        <input
          className="browser-preview__input"
          type="text"
          value={input}
          placeholder="输入网址预览(https://…)"
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.nativeEvent.isComposing && e.keyCode !== 229) handleGo();
          }}
          aria-label="预览网址"
        />
        <button
          type="button"
          className="browser-preview__go"
          onClick={handleGo}
          disabled={!normalized}
        >
          预览
        </button>
        <button
          type="button"
          className="browser-preview__nav-btn"
          onClick={() => void openExternal()}
          disabled={!current || externalState === "opening"}
          aria-busy={externalState === "opening"}
          title="用系统浏览器打开"
          aria-label="用系统浏览器打开"
        >
          <OpenExternalIcon size="sm" />
        </button>
      </div>
      {externalState !== "idle" && <p className={"browser-preview__status" + (externalState === "failed" ? " browser-preview__status--error" : "")} role={externalState === "failed" ? "alert" : "status"}>
        {externalState === "opening" ? "正在打开系统浏览器…" : externalState === "opened" ? "已在系统浏览器中打开" : "打开失败，请重试或复制网址到浏览器。"}
      </p>}
      {frameLoading && current && <p className="browser-preview__status" role="status">正在加载网页…</p>}
      {current ? (
        <iframe
          key={reloadKey}
          className="browser-preview__frame"
          src={current}
          title={frameTitle}
          sandbox={PREVIEW_SANDBOX}
          referrerPolicy="no-referrer"
          onLoad={() => setFrameLoading(false)}
        />
      ) : (
        <div className="browser-preview__empty">
          {input.trim()
            ? "该网址不可预览(仅允许 http/https 公网地址,拒绝本地/内网)。"
            : "输入一个 https 网址以预览。"}
        </div>
      )}
    </div>
  );
}
