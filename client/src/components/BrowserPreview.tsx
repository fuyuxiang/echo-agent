import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { ArrowRight, ExternalLink, MoreHorizontal, Search } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { developmentEndpoint, normalizePreviewUrl } from "@/lib/browser-preview";
import { openUrl } from "@/lib/agent-client";
import { friendlyError } from "@/lib/error-format";
import type { BrowserPanelCapabilities } from "@/lib/automation-client";
import { ChevronLeftIcon, ChevronRightIcon, RefreshCwIcon, AddIcon, CloseIcon } from "@/foundation/components/Icon/icons";

export type BrowserOpenRequest = (
  | { kind: "open"; url: string }
  | { kind: "artifact"; path: string }
) & { sequence: number };

interface BrowserPreviewProps {
  sessionId?: string;
  active: boolean;
  request?: BrowserOpenRequest;
  panelCapabilities?: BrowserPanelCapabilities;
  agentWorking?: boolean;
  taskPage?: { url?: string | null; title?: string | null; shared?: boolean };
  onRequestHandled?: (sequence: number) => void;
  onTakeOver?: () => Promise<boolean | void>;
  onEditArtifact?: (context: BrowserArtifactEditContext) => void;
  onShowArtifactSource?: (path: string) => void;
}

export interface BrowserArtifactEditContext {
  path: string;
  tag: string;
  id: string;
  label: string;
}

type ArtifactSelection = Omit<BrowserArtifactEditContext, "path"> & {
  token: string;
  text: string | null;
  color: string;
  fontSize: number;
  bold: boolean;
  image: string | null;
};
type ArtifactChange = Partial<Pick<ArtifactSelection, "text" | "color" | "fontSize" | "bold" | "image">>;

type Bounds = { x: number; y: number; width: number; height: number };
type Open = { kind: "open"; url: string; allowDevelopment?: boolean; newTab?: boolean };
type Action = Open
  | { kind: "artifact"; path: string }
  | { kind: "select" | "close"; id: number }
  | { kind: "layout"; bounds: Bounds | null }
  | { kind: "find"; query: string; backwards?: boolean }
  | { kind: "zoom"; scale: number }
  | { kind: "device"; viewport: DeviceViewport | null }
  | { kind: "editSave"; token: string; change: ArtifactChange }
  | { kind: "mount" | "view" | "new" | "back" | "forward" | "reload" | "capture" | "revokeDevelopment" | "dispose"
    | "editBegin" | "editSelection" | "editEnd" | "editUndo" | "editRedo" | "clearBrowsingData" | "openDownloads" };

type DeviceViewport = { width: number; height: number };

interface PanelView {
  tabs: { id: number; title: string; url: string; artifactPath?: string; canUndo?: boolean; canRedo?: boolean }[];
  activeId: number | null;
  canGoBack: boolean;
  canGoForward: boolean;
  developmentEndpoints: string[];
  allowPrivateNetwork?: boolean;
  inputLocked?: boolean;
  zoom?: number;
  device?: DeviceViewport | null;
  findMatch?: boolean;
  image?: string;
  history?: { title: string; url: string }[];
  canClearBrowsingData?: boolean;
  profileTabCount?: number;
  selection?: ArtifactSelection;
}

const devices = {
  phone: { width: 390, height: 844 },
  tablet: { width: 768, height: 1024 },
  desktop: { width: 1280, height: 800 },
};

function resolveAddress(text: string) {
  const value = text.trim();
  if (!value) return null;
  const normalized = normalizePreviewUrl(value);
  if (normalized && (/^https?:\/\//i.test(value) || /[.:/]/.test(value) || /^localhost$/i.test(value))) {
    return normalized;
  }
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(value) || /^(?:javascript|file|data|about|blob):/i.test(value)) return null;
  const provider = navigator.language.startsWith("zh") ? "https://www.baidu.com/s?wd=" : "https://www.google.com/search?q=";
  return provider + encodeURIComponent(value);
}

let nextPanelOwner = 0;

export function BrowserPreview({
  sessionId, active, request, panelCapabilities, agentWorking = false, taskPage,
  onRequestHandled, onTakeOver, onEditArtifact, onShowArtifactSource,
}: BrowserPreviewProps) {
  const [owner] = useState(() => `${Date.now()}-${++nextPanelOwner}`);
  const [view, setView] = useState<PanelView | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [permission, setPermission] = useState<{ action: Open; endpoint: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [feedback, setFeedback] = useState("");
  const [tools, setTools] = useState(false);
  const [finding, setFinding] = useState(false);
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<boolean | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [editTab, setEditTab] = useState<number | null>(null);
  const [selection, setSelection] = useState<ArtifactSelection>();
  const [change, setChange] = useState<ArtifactChange>({});
  const searchInput = useRef<HTMLInputElement>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const queue = useRef(Promise.resolve());
  const mount = useRef<Promise<PanelView | null>>(Promise.resolve(null));
  const lastRequest = useRef<number | undefined>(undefined);
  const selected = view?.tabs.find(tab => tab.id === view.activeId);
  const shownUrl = selected?.url === "about:blank" ? "" : selected?.url ?? "";
  const normalized = draft === null ? (/^https?:\/\//i.test(shownUrl) ? shownUrl : null) : resolveAddress(draft);
  const history = view?.history ?? [];
  const hasPage = Boolean(selected && shownUrl);
  const sharedBusy = Boolean(view?.inputLocked || agentWorking && taskPage?.shared);
  const artifactPath = selected?.artifactPath;
  const editing = editTab !== null && editTab === selected?.id;
  const webAvailable = panelCapabilities?.web !== false;
  const ready = Boolean(sessionId && active && "__TAURI_INTERNALS__" in window
    && (webAvailable || panelCapabilities?.artifacts));
  const showTask = Boolean(taskPage && !selected && !permission);
  const layoutSync = useRef<() => void>(() => {});

  const callHost = useCallback((action: Action): Promise<PanelView | null> => {
    if (!sessionId || !("__TAURI_INTERNALS__" in window)) return Promise.resolve(null);
    return invoke<PanelView>("browser_panel", { sessionId, owner, action })
      .then(result => Array.isArray(result?.tabs) ? result : null);
  }, [owner, sessionId]);

  const command = useCallback((action: Action) => {
    const mounted = mount.current;
    const result = queue.current.then(async () => {
      await mounted;
      return callHost(action);
    });
    queue.current = result.then(() => {}, () => {});
    return result;
  }, [callHost]);

  useEffect(() => () => {
    void callHost({ kind: "dispose" }).catch(() => {});
  }, [callHost]);

  useLayoutEffect(() => {
    const element = viewport.current;
    if (!ready || !element) return;
    let mounted = true;
    const opening = callHost({ kind: "mount" });
    mount.current = opening;
    void opening.then(next => {
      if (mounted && next) setView(next);
    }).catch(cause => { if (mounted) setError(friendlyError(cause)); });
    let frame = 0;
    let lastBounds = "";
    const measure = () => {
      const rect = element.getBoundingClientRect();
      const shellOverlay = document.querySelector('[aria-modal="true"]:not([hidden]), dialog[open]');
      const x = Math.max(0, rect.left);
      const y = Math.max(0, rect.top);
      const width = Math.min(innerWidth, rect.right) - x;
      const height = Math.min(innerHeight, rect.bottom) - y;
      const bounds = width > 0 && height > 0 && !shellOverlay && document.visibilityState !== "hidden"
        ? { x, y, width, height }
        : null;
      const key = JSON.stringify(bounds);
      if (key === lastBounds) return;
      lastBounds = key;
      void opening.then(() => mounted ? callHost({ kind: "layout", bounds }) : null)
        .catch(cause => { if (mounted) setError(friendlyError(cause)); });
    };
    const sync = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    layoutSync.current = () => {
      lastBounds = "";
      sync();
    };
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(sync);
    observer?.observe(element);
    const overlays = new MutationObserver(sync);
    overlays.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["aria-modal", "hidden", "open"] });
    window.addEventListener("resize", sync);
    window.addEventListener("scroll", sync, true);
    document.addEventListener("visibilitychange", sync);
    sync();
    return () => {
      mounted = false;
      layoutSync.current = () => {};
      cancelAnimationFrame(frame);
      observer?.disconnect();
      overlays.disconnect();
      window.removeEventListener("resize", sync);
      window.removeEventListener("scroll", sync, true);
      document.removeEventListener("visibilitychange", sync);
      void callHost({ kind: "layout", bounds: null }).catch(() => {});
    };
  }, [callHost, ready]);

  useEffect(() => {
    if (!ready || (view?.activeId == null && !taskPage?.shared)) return;
    let current = true;
    const timer = setInterval(() => {
      if (document.visibilityState === "hidden") return;
      void callHost({ kind: "view" }).then(next => {
        if (current && next) setView(next);
      }).catch(() => {});
    }, 1000);
    return () => { current = false; clearInterval(timer); };
  }, [callHost, ready, taskPage?.shared, view?.activeId]);

  const run = useCallback(async (action: Action) => {
    setBusy(true);
    setError("");
    setFeedback("");
    try {
      if (editing && ["artifact", "open", "new", "select", "close", "back", "forward", "reload"].includes(action.kind)) {
        await command({ kind: "editEnd" }).catch(() => {});
        setEditTab(null);
        setSelection(undefined);
      }
      const next = await command(action);
      if (next) setView(next);
      setDraft(null);
      if (action.kind === "clearBrowsingData") setFeedback("已清除内置浏览器数据");
      layoutSync.current();
    } catch (cause) {
      setError(friendlyError(cause));
      if (action.kind === "artifact" && /当前桌面系统不支持内嵌网页/.test(String(cause))) {
        onShowArtifactSource?.(action.path);
      }
    } finally {
      setBusy(false);
    }
  }, [command, editing, onShowArtifactSource]);

  useEffect(() => {
    if (!editing || !active || sharedBusy) return;
    let live = true;
    let timer = 0;
    const poll = async () => {
      try {
        const next = await callHost({ kind: "editSelection" });
        if (live) setSelection(current => current?.token === next?.selection?.token ? current : next?.selection);
      } catch {
        if (live) setSelection(undefined);
      } finally {
        if (live) timer = window.setTimeout(poll, 240);
      }
    };
    void poll();
    return () => { live = false; window.clearTimeout(timer); };
  }, [active, callHost, editing, sharedBusy]);

  useEffect(() => {
    if (editTab !== null && (!active || sharedBusy || editTab !== selected?.id)) {
      if (editing) void callHost({ kind: "editEnd" }).catch(() => {});
      setEditTab(null);
      setSelection(undefined);
    }
  }, [active, callHost, editTab, editing, selected?.id, sharedBusy]);

  useEffect(() => setChange({}), [selection?.token]);

  const open = useCallback((action: Open) => {
    setError("");
    const endpoint = developmentEndpoint(action.url);
    if (endpoint && !view?.allowPrivateNetwork && !view?.developmentEndpoints.includes(endpoint)) {
      setPermission({ action, endpoint });
      return;
    }
    setPermission(null);
    void run(action);
  }, [run, view?.allowPrivateNetwork, view?.developmentEndpoints]);

  useEffect(() => {
    if (!active || busy || sharedBusy || !request || lastRequest.current === request.sequence) return;
    lastRequest.current = request.sequence;
    onRequestHandled?.(request.sequence);
    if (request.kind === "artifact") {
      setPermission(null);
      if (ready && panelCapabilities?.artifacts !== false) void run({ kind: "artifact", path: request.path });
      else onShowArtifactSource?.(request.path);
      return;
    }
    setDraft(request.url);
    if (ready && webAvailable) open({ kind: "open", url: request.url, newTab: true });
    else if (!webAvailable) void openUrl(request.url).catch(cause => setError(friendlyError(cause)));
  }, [active, busy, onRequestHandled, onShowArtifactSource, open, panelCapabilities?.artifacts, ready, request, run, sharedBusy, webAvailable]);

  async function revealTask() {
    if (!sessionId) return;
    setError("");
    try {
      if ((agentWorking || view?.inputLocked) && await onTakeOver?.() === false) {
        setError("暂停尚未确认，请重试");
        return;
      }
      if (taskPage?.shared || view?.inputLocked) {
        const next = await command({ kind: "view" });
        if (next) setView(next);
        return;
      }
      await invoke("automation_focus_browser", { sessionId });
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }

  async function find(backwards = false, text = query) {
    try {
      const next = await command({ kind: "find", query: text, backwards });
      setFound(text && next?.findMatch != null ? next.findMatch : null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }

  function showFind() {
    setFinding(true);
    requestAnimationFrame(() => searchInput.current?.focus());
  }

  function closeFind() {
    setFinding(false);
    setFound(null);
    setQuery("");
    void find(false, "");
  }

  async function capture() {
    setError("");
    setFeedback("");
    const png = command({ kind: "capture" }).then(next => {
      if (!next?.image) throw new Error("网页截图未生成");
      return new Blob([Uint8Array.from(atob(next.image), byte => byte.charCodeAt(0))], { type: "image/png" });
    });
    try {
      if (!navigator.clipboard?.write || typeof ClipboardItem === "undefined") throw new Error("需要保存截图");
      await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
      setFeedback("已复制网页截图");
    } catch {
      try {
        const url = URL.createObjectURL(await png);
        const link = document.createElement("a");
        link.href = url;
        link.download = "网页截图.png";
        link.click();
        window.setTimeout(() => URL.revokeObjectURL(url), 1000);
        setFeedback("已保存网页截图");
      } catch (cause) {
        setError(friendlyError(cause));
      }
    }
  }

  async function edit(action: Extract<Action, { kind: "editSave" }> | {
    kind: "editBegin" | "editEnd" | "editUndo" | "editRedo";
  }) {
    setBusy(true);
    setError("");
    setFeedback("");
    try {
      const next = await command(action);
      if (next) setView(next);
      setSelection(undefined);
      setEditTab(action.kind === "editBegin" ? selected?.id ?? null : null);
      if (action.kind === "editSave") setFeedback("已保存到 HTML 产物");
      if (action.kind === "editUndo") setFeedback("已撤销 HTML 修改");
      if (action.kind === "editRedo") setFeedback("已重做 HTML 修改");
      layoutSync.current();
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setBusy(false);
    }
  }

  function openExternally() {
    if (normalized) void openUrl(normalized).catch(cause => setError(friendlyError(cause)));
  }

  return (
    <div className="browser-preview" role="region" aria-label="网页" onKeyDown={event => {
      if (hasPage && (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "f") {
        event.preventDefault();
        showFind();
      }
    }}>
      <div className="browser-preview__tab-strip">
        <div className="browser-preview__tabs" aria-label="网页标签">
          {view?.tabs.map(tab => (
            <div className="browser-preview__tab" key={tab.id}>
              <button type="button" aria-pressed={tab.id === view.activeId} title={tab.url} disabled={busy || sharedBusy}
                onClick={() => { setPermission(null); void run({ kind: "select", id: tab.id }); }}>
                {tab.title || (tab.url === "about:blank" ? "新标签页" : tab.url)}
              </button>
              <button type="button" aria-label={`关闭 ${tab.title || "标签页"}`} disabled={busy || sharedBusy}
                onClick={() => void run({ kind: "close", id: tab.id })}><CloseIcon size="sm" /></button>
            </div>
          ))}
        </div>
        <button type="button" className="browser-preview__nav-btn" aria-label="网页工具" title="网页工具"
          aria-expanded={tools} onClick={() => setTools(!tools)}><MoreHorizontal size={16} aria-hidden="true" /></button>
        <button type="button" className="browser-preview__nav-btn" aria-label="新建网页标签" title="新建网页标签"
          disabled={busy || !ready || !webAvailable || sharedBusy} onClick={() => { setPermission(null); void run({ kind: "new" }); }}>
          <AddIcon size="sm" />
        </button>
      </div>
      <form className="browser-preview__toolbar" onSubmit={event => {
        event.preventDefault();
        if (!normalized || busy || sharedBusy) return;
        if (ready && webAvailable) open({ kind: "open", url: normalized });
        else if (!webAvailable) openExternally();
      }}>
        <button type="button" className="browser-preview__nav-btn" aria-label="后退" title="后退"
          disabled={busy || sharedBusy || !view?.canGoBack} onClick={() => void run({ kind: "back" })}><ChevronLeftIcon size="sm" /></button>
        <button type="button" className="browser-preview__nav-btn" aria-label="前进" title="前进"
          disabled={busy || sharedBusy || !view?.canGoForward} onClick={() => void run({ kind: "forward" })}><ChevronRightIcon size="sm" /></button>
        <button type="button" className="browser-preview__nav-btn" aria-label="刷新" title="刷新"
          disabled={busy || sharedBusy || !shownUrl} onClick={() => void run({ kind: "reload" })}><RefreshCwIcon size="sm" /></button>
        <input className="browser-preview__input" aria-label="预览网址" value={draft ?? shownUrl} list={history.length ? `preview-history-${owner}` : undefined}
          readOnly={busy || sharedBusy} placeholder="输入网址或搜索词" onChange={event => {
            setDraft(event.target.value);
            setPermission(null);
            setError("");
          }} />
        {history.length > 0 && <datalist id={`preview-history-${owner}`}>
          {history.map(item => <option key={item.url} value={item.url} label={item.title} />)}
        </datalist>}
        <button type="submit" className="browser-preview__nav-btn browser-preview__go" aria-label="打开网址" title="打开网址"
          disabled={!normalized || (!ready && webAvailable) || busy || sharedBusy}><ArrowRight size={16} aria-hidden="true" /></button>
        <button type="button" className="browser-preview__nav-btn" aria-label="在系统浏览器中打开" title="在系统浏览器中打开"
          disabled={!normalized} onClick={openExternally}><ExternalLink size={16} aria-hidden="true" /></button>
      </form>
      {tools && <div className="browser-preview__status browser-preview__status-actions browser-preview__tools" role="group" aria-label="网页工具">
        <button type="button" disabled={!hasPage || sharedBusy} onClick={showFind}><Search size={14} /> 页内查找</button>
        <button type="button" disabled={!hasPage || sharedBusy} onClick={() => void capture()}>截取网页</button>
        <label>缩放 <select aria-label="网页缩放" disabled={!hasPage || sharedBusy}
          value={view?.zoom ?? 1} onChange={event => void run({ kind: "zoom", scale: Number(event.target.value) })}>
          {[0.5, 0.75, 0.9, 1, 1.1, 1.25, 1.5, 2].map(scale => <option key={scale} value={scale}>{scale * 100}%</option>)}
        </select></label>
        <label>视口 <select aria-label="设备视口" disabled={!hasPage || sharedBusy}
          value={Object.entries(devices).find(([, device]) => device.width === view?.device?.width)?.[0] ?? ""}
          onChange={event => void run({ kind: "device", viewport: devices[event.target.value as keyof typeof devices] ?? null })}>
          <option value="">自适应</option><option value="phone">手机</option>
          <option value="tablet">平板</option><option value="desktop">桌面</option>
        </select></label>
        <button type="button" disabled={!selected || busy} onClick={() => void run({ kind: "openDownloads" })}>打开下载目录</button>
        <button type="button" disabled={!view?.canClearBrowsingData || busy || sharedBusy}
          onClick={() => setConfirmClear(true)}>清除浏览数据</button>
      </div>}
      {confirmClear && <div className="browser-preview__status browser-preview__status-actions" role="group" aria-label="清除浏览数据确认">
        <span>清除当前账号在内置浏览器中的登录、缓存和浏览记录，并关闭各会话中的公网网页标签{view?.profileTabCount ? `（${view.profileTabCount} 个）` : ""}。网页上尚未提交的内容可能丢失。</span>
        <button type="button" disabled={busy || sharedBusy} onClick={() => {
          setConfirmClear(false);
          void run({ kind: "clearBrowsingData" });
        }}>清除</button>
        <button type="button" onClick={() => setConfirmClear(false)}>取消</button>
      </div>}
      {finding && <form className="browser-preview__status browser-preview__status-actions" aria-label="页内查找" onSubmit={event => {
        event.preventDefault();
        void find((event.nativeEvent as SubmitEvent).submitter?.getAttribute("data-direction") === "previous");
      }} onKeyDown={event => { if (event.key === "Escape") closeFind(); }}>
        <input ref={searchInput} className="browser-preview__input" aria-label="查找网页内容" value={query}
          onChange={event => { setQuery(event.target.value); setFound(null); }} />
        <button type="submit" disabled={!query} data-direction="previous">上一处</button>
        <button type="submit" disabled={!query}>下一处</button>
        <button type="button" onClick={closeFind}>关闭查找</button>
        {found === false && <span role="status">未找到匹配内容</span>}
      </form>}
      {feedback && <div className="browser-preview__status" role="status">{feedback}</div>}
      {error && <div className="browser-preview__status browser-preview__status--error" role="alert">{error}</div>}
      {permission && <div className="browser-preview__status browser-preview__status-actions" role="group" aria-label="本地端口授权">
        <span>允许在此会话访问本地预览 {permission.endpoint}？</span>
        <button type="button" disabled={busy} onClick={() => {
          setPermission(null);
          void run({ ...permission.action, allowDevelopment: true });
        }}>允许并打开</button>
        <button type="button" onClick={() => setPermission(null)}>取消</button>
      </div>}
      {Boolean(view?.developmentEndpoints.length) && <div className="browser-preview__status browser-preview__status-actions">
        <span>已授权本地端口 {view?.developmentEndpoints.join("、")}</span>
        <button type="button" disabled={busy || sharedBusy} onClick={() => void run({ kind: "revokeDevelopment" })}>撤销全部端口授权</button>
      </div>}
      {(sharedBusy || taskPage && !taskPage.shared) && <div className="browser-preview__status browser-preview__status-actions">
        <span title={taskPage?.url ?? undefined}>{sharedBusy ? "Agent 正在操作网页" : taskPage?.title || taskPage?.url || "Agent 网页任务"}</span>
        <button type="button" onClick={() => void revealTask()}>{sharedBusy ? "暂停并接手" : agentWorking ? "暂停并打开任务浏览器" : "打开任务浏览器"}</button>
      </div>}
      {artifactPath && <div className="browser-preview__status browser-preview__status-actions browser-preview__tools" role="group" aria-label="HTML 产物编辑">
        {onShowArtifactSource && <button type="button" onClick={() => onShowArtifactSource(artifactPath)}>查看源码</button>}
        <button type="button" disabled={busy || sharedBusy} onClick={() => void edit({ kind: editing ? "editEnd" : "editBegin" })}>
          {editing ? "退出编辑" : "编辑网页"}
        </button>
        <button type="button" disabled={busy || sharedBusy || !selected?.canUndo} onClick={() => void edit({ kind: "editUndo" })}>撤销编辑</button>
        <button type="button" disabled={busy || sharedBusy || !selected?.canRedo} onClick={() => void edit({ kind: "editRedo" })}>重做编辑</button>
        {editing && !selection && <span>点击网页上的文字或图片进行编辑。</span>}
      </div>}
      {editing && selection && artifactPath && <form className="browser-preview__status browser-preview__status-actions browser-preview__tools browser-preview__editor"
        aria-label="编辑选中元素" onSubmit={event => {
          event.preventDefault();
          if (!busy && !sharedBusy && Object.keys(change).length) void edit({ kind: "editSave", token: selection.token, change });
        }}>
        <span title={selection.label}>{selection.tag}{selection.id ? `#${selection.id}` : ""}</span>
        {selection.text !== null && <input className="browser-preview__input" aria-label="元素文字" placeholder="输入文字"
          value={change.text ?? selection.text} onChange={event => setChange({ ...change, text: event.target.value })} />}
        {selection.image !== null && <input className="browser-preview__input" aria-label="图片相对路径" placeholder="图片相对路径"
          value={change.image ?? selection.image} onChange={event => setChange({ ...change, image: event.target.value })} />}
        <label>颜色 <input type="color" aria-label="文字颜色" value={change.color ?? selection.color}
          onChange={event => setChange({ ...change, color: event.target.value })} /></label>
        <label>字号 <input type="number" aria-label="字号" min={8} max={240} value={change.fontSize ?? selection.fontSize}
          onChange={event => setChange({ ...change, fontSize: event.target.valueAsNumber || undefined })} /></label>
        <button type="button" aria-pressed={change.bold ?? selection.bold}
          onClick={() => setChange({ ...change, bold: !(change.bold ?? selection.bold) })}>加粗</button>
        <button type="submit" disabled={busy || sharedBusy || !Object.values(change).some(value => value !== undefined)}>保存</button>
        {onEditArtifact && <button type="button" disabled={busy || sharedBusy}
          onClick={() => onEditArtifact({ path: artifactPath, tag: selection.tag, id: selection.id, label: selection.label })}>让 Agent 修改</button>}
      </form>}
      <div className="browser-preview__surface" ref={viewport}>
        {sharedBusy && <p className="browser-preview__empty" role="status">Agent 正在使用此标签。暂停后可在同一网页继续操作。</p>}
        {(!selected || !shownUrl) && !sharedBusy && !showTask && !busy && !permission && !error && (
          <p className="browser-preview__empty">{webAvailable ? "输入网址以打开网页。" : "当前系统支持在系统浏览器中打开网址。"}</p>
        )}
      </div>
    </div>
  );
}
