/**
 * 右侧统一工作区面板 —— 对齐 EchoAgent `SidebarNext / DetailPanel`。
 *
 * 在原 3 模式（tool/artifacts/preview）基础上升级为统一工作区：
 *  - ViewSelector 下拉切换视图：产物 / 文件树 / 浏览器 / 变更
 *  - 统一标签页（useUnifiedTabs）：跨视图共享、自动开/关、可拖拽排序、会话持久化
 *  - 可调宽（Sash）+ 钉住左列 + 最大化 + 收起
 *  - 浏览器预览（BrowserPreview，含后退/前进/刷新/外开）
 *  - 文件树（FileTreeView，懒加载目录）
 *
 * 兼容性：保留原导出名 `ToolSidePanel` / `ToolSidePanelMode` 与 ChatView 的 props，
 * 新增内部状态管理视图/标签/宽度。原 "tool" 模式仍用于展示单个工具调用详情。
 */
import { useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { SidebarLayoutContext } from "@/lib/sidebar-layout";
import type { ToolCallView } from "@/stores/session-store";
import type { SessionArtifact } from "@/lib/session-artifacts";
import type { FileChange } from "@/lib/file-changes";
import { aggregateFileChanges } from "@/lib/file-changes";
import type { ChatMessage } from "@/stores/session-store";
import {
  useUnifiedTabs,
  type WorkspaceView,
} from "@/lib/use-unified-tabs";
import { ToolCallDetailBody } from "./ToolCallCard";
import { openLocalPath } from "@/lib/markdown-host";
import { FilePreview as RichFilePreview } from "./FilePreview";
import { copyShareText } from "@/lib/share";
import { detectPreviewKind, type PreviewKind } from "@/lib/file-kind";
import {
  authorizeArtifactFile,
  errorMessage,
  isUnauthorizedPathError,
} from "@/lib/artifact-access";
import { invoke } from "@tauri-apps/api/core";
import { IS_MACOS } from "@/lib/platform";
import { ViewSelector, defaultViews } from "./workspace-panel/ViewSelector";
import { ArtifactTabsBar } from "./workspace-panel/ArtifactTabsBar";
import { FileTreeView } from "./workspace-panel/FileTreeView";
import { BrowserPreview } from "./BrowserPreview";
import {
  EchoPinIcon,
  EchoUnpinIcon,
  MaximizeIcon,
  RestoreIcon,
  CloseIcon,
  ChevronLeftIcon,
} from "@/foundation/components/Icon/icons";

/** 向后兼容：原模式 + 新视图。tool 为单工具详情，其余映射到工作区视图。 */
export type ToolSidePanelMode =
  | "tool"
  | "artifacts"
  | "preview"
  | "fileTree"
  | "browser"
  | "changes";

// ---------- 持久化常量 ----------
const WIDTH_KEY = "tool-side-panel-width";
const NAV_WIDTH_KEY = "tool-side-panel-nav-width";
const DEFAULT_WIDTH = 380;
const DEFAULT_NAV_WIDTH = 200;
const MIN_WIDTH = 280;
const MAX_WIDTH_RATIO = 0.6; // 占视口 60%
const MIN_NAV_WIDTH = 140;
const MAX_NAV_WIDTH = 360;
const MIN_MAIN_WIDTH = 120;
const NAV_SASH_WIDTH = 5;

interface ToolSidePanelProps {
  open: boolean;
  mode: ToolSidePanelMode;
  toolCall?: ToolCallView | null;
  artifacts: SessionArtifact[];
  previewPath?: string | null;
  cwd?: string;
  /** 会话消息（用于聚合文件变更）。 */
  messages?: ChatMessage[];
  /** 会话 id（用于标签页会话隔离）。 */
  sessionId?: string;
  onToast?: (msg: string) => void;
  onClose: () => void;
  onSelectTool: (tc: ToolCallView) => void;
  onSelectArtifact: (a: SessionArtifact) => void;
  onOpenArtifacts: () => void;
  findToolCall?: (id: string) => ToolCallView | undefined;
}

export function ToolSidePanel({
  open,
  mode,
  toolCall,
  artifacts,
  previewPath,
  cwd,
  messages,
  sessionId,
  onToast,
  onClose,
  onSelectTool,
  onSelectArtifact,
  findToolCall,
}: ToolSidePanelProps) {
  // ---- 视图状态：把外部 mode 映射到内部 WorkspaceView ----
  const [view, setView] = useState<WorkspaceView>("artifacts");
  useEffect(() => {
    if (mode === "artifacts") setView("artifacts");
    else if (mode === "preview") setView("fileTree"); // 单文件预览映射到文件树视图
    else if (mode === "fileTree") setView("fileTree");
    else if (mode === "browser") setView("preview");
    else if (mode === "changes") setView("changes");
    // "tool" 模式不改 view（工具详情在主列覆盖渲染）
  }, [mode]);

  // ---- selection 状态 ----
  const [selectedArtifactId, setSelectedArtifactId] = useState<string | undefined>();
  const [selectedFilePath, setSelectedFilePath] = useState<string | undefined>();
  const [browserUrl, setBrowserUrl] = useState<string | undefined>();

  // 文件变更数据。
  const changes = useMemo<FileChange[]>(
    () => (messages ? aggregateFileChanges(messages).files : []),
    [messages],
  );
  const hasChanges = changes.length > 0;

  // ---- 视图切换 ----
  const handleViewChange = useCallback((v: WorkspaceView) => {
    setView(v);
  }, []);

  const handleArtifactSelect = useCallback(
    (id?: string) => {
      setSelectedArtifactId(id);
      if (id) {
        const a = artifacts.find((x) => x.id === id);
        if (a) onSelectArtifact(a);
      }
    },
    [artifacts, onSelectArtifact],
  );

  const handleFileSelect = useCallback(
    (path?: string) => {
      setSelectedFilePath(path);
      if (path) {
        // 复用 onSelectArtifact 把路径包成 SessionArtifact，驱动主列预览。
        onSelectArtifact({
          id: path,
          path,
          kind: "file",
          title: basename(path),
          toolCallId: "",
          status: "completed",
        });
      }
    },
    [onSelectArtifact],
  );

  // ---- 统一标签页 ----
  const tabsApi = useUnifiedTabs({
    resetKey: sessionId,
    enabled: open,
    currentView: view,
    selectedArtifactId,
    selectedFilePath,
    browserUrl,
    artifacts,
    changes,
    onViewChange: handleViewChange,
    onArtifactSelect: handleArtifactSelect,
    onFileSelect: handleFileSelect,
    onBrowserUrlChange: setBrowserUrl,
  });

  // ---- 面板布局状态 ----
  const shellLayout = useContext(SidebarLayoutContext);
  const [viewportWidth, setViewportWidth] = useState(getViewportWidth);
  const panelMaxWidth = Math.min(getPanelMaxWidth(viewportWidth), shellLayout?.panelMaxWidth ?? Infinity);
  const [width, setWidth] = useState<number>(() => {
    return readStoredDimension(
      WIDTH_KEY,
      DEFAULT_WIDTH,
      MIN_WIDTH,
      getPanelMaxWidth(getViewportWidth()),
    );
  });
  const [navWidth, setNavWidth] = useState<number>(() => {
    return readStoredDimension(
      NAV_WIDTH_KEY,
      DEFAULT_NAV_WIDTH,
      MIN_NAV_WIDTH,
      MAX_NAV_WIDTH,
    );
  });
  const [maximized, setMaximized] = useState(false);
  const [pinned, setPinned] = useState(false);
  const [navCollapsed, setNavCollapsed] = useState(false);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const registerPanel = shellLayout?.registerPanel;
  useLayoutEffect(() => {
    registerPanel?.(open && !maximized ? { minimumWidth: MIN_WIDTH, close: () => closeRef.current() } : null);
    return () => registerPanel?.(null);
  }, [open, maximized, registerPanel]);

  const renderedPanelWidth = clamp(width, MIN_WIDTH, panelMaxWidth);
  const availablePanelWidth = maximized ? viewportWidth : renderedPanelWidth;
  const navMaxWidth = Math.max(
    MIN_NAV_WIDTH,
    Math.min(MAX_NAV_WIDTH, availablePanelWidth - MIN_MAIN_WIDTH - NAV_SASH_WIDTH),
  );
  const renderedNavWidth = clamp(navWidth, MIN_NAV_WIDTH, navMaxWidth);

  // 切换会话时重置 selection（避免跨会话残留）。
  useEffect(() => {
    setSelectedArtifactId(undefined);
    setSelectedFilePath(undefined);
    setBrowserUrl(undefined);
  }, [sessionId]);

  // 窗口缩放后重新约束面板，避免持久化尺寸在小窗口中挤坏布局。
  useEffect(() => {
    const handleResize = () => setViewportWidth(getViewportWidth());
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  useEffect(() => {
    // Shell constraints are temporary; closing / resizing the left sidebar
    // should restore the workspace panel's preferred width automatically.
    if (!shellLayout) setWidth((current) => clamp(current, MIN_WIDTH, panelMaxWidth));
  }, [panelMaxWidth, shellLayout]);

  useEffect(() => {
    if (!shellLayout) setNavWidth((current) => clamp(current, MIN_NAV_WIDTH, navMaxWidth));
  }, [navMaxWidth, shellLayout]);

  // 持久化经过校验的宽度；同时兼容无痕模式等 localStorage 不可写场景。
  useEffect(() => {
    writeStoredDimension(WIDTH_KEY, width);
  }, [width]);
  useEffect(() => {
    writeStoredDimension(NAV_WIDTH_KEY, navWidth);
  }, [navWidth]);

  const effectiveNavCollapsed = pinned ? false : navCollapsed;
  const handlePinnedToggle = useCallback(() => {
    // 钉住/取消钉住只改变后续能否收起，不应沿用历史收起状态或改变当前布局。
    setNavCollapsed(false);
    setPinned((current) => !current);
  }, []);

  if (!open) return null;

  const views = defaultViews({ hasChanges });
  // tool 模式：主列渲染工具详情；其余按工作区视图渲染。
  const showToolDetail = mode === "tool";

  return (
    <aside
      className={
        "tool-side-panel" +
        (maximized ? " tool-side-panel--maximized" : "")
      }
      style={
        maximized
          ? undefined
          : { width: `${renderedPanelWidth}px` }
      }
      aria-label="工作区面板"
    >
      {/* 面板左边缘：全宽拖拽（非最大化时）。 */}
      {!maximized && (
        <div
          className="tool-side-panel__edge-sash"
          onPointerDown={(e) =>
            startResizeEdge(e, renderedPanelWidth, panelMaxWidth, setWidth)
          }
        />
      )}
      {/* 左导航列 */}
      <div
        className={
          "tool-side-panel__nav" +
          (effectiveNavCollapsed ? " tool-side-panel__nav--collapsed" : "")
        }
        style={
          effectiveNavCollapsed ? undefined : { width: `${renderedNavWidth}px` }
        }
      >
        {/* macOS 贴窗口顶边：空白处支持拖动/双击缩放（同 header）。 */}
        <div
          className="tool-side-panel__nav-header"
          {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})}
        >
          {effectiveNavCollapsed ? (
            <button
              type="button"
              className="tool-side-panel__icon-btn tool-side-panel__nav-expand"
              onClick={() => setNavCollapsed(false)}
              title="展开导航"
              aria-label="展开导航"
            >
              <ChevronLeftIcon size="sm" className="tool-side-panel__icon--flip" />
            </button>
          ) : (
            <>
              <ViewSelector view={view} views={views} onChange={handleViewChange} />
              <button
                type="button"
                className="tool-side-panel__icon-btn"
                onClick={handlePinnedToggle}
                title={pinned ? "取消钉住" : "钉住左列"}
                aria-label={pinned ? "取消钉住" : "钉住左列"}
                aria-pressed={pinned}
              >
                {pinned ? <EchoUnpinIcon size="sm" /> : <EchoPinIcon size="sm" />}
              </button>
              {!pinned && (
                <button
                  type="button"
                  className="tool-side-panel__icon-btn"
                  onClick={() => setNavCollapsed(true)}
                  title="收起导航"
                  aria-label="收起导航"
                >
                  <ChevronLeftIcon size="sm" />
                </button>
              )}
            </>
          )}
        </div>
        {!effectiveNavCollapsed && (
          <div className="tool-side-panel__nav-body">
            <NavContent
              view={view}
              artifacts={artifacts}
              changes={changes}
              cwd={cwd}
              selectedArtifactId={selectedArtifactId}
              selectedFilePath={selectedFilePath}
              onArtifactSelect={handleArtifactSelect}
              onFileSelect={handleFileSelect}
              onToast={onToast}
            />
          </div>
        )}
      </div>

      {/* Sash：调整左列宽度 */}
      {!effectiveNavCollapsed && (
        <div
          className="tool-side-panel__sash"
          onPointerDown={(e) =>
            startResizeNav(e, renderedNavWidth, navMaxWidth, setNavWidth)
          }
        />
      )}

      {/* 主内容列 */}
      <div className="tool-side-panel__main">
        {/* macOS 上这行 header 贴窗口顶边（Overlay 标题栏）：空白处需要
            data-tauri-drag-region 才能拖动窗口 / 双击缩放（红绿灯右侧的
            标签条区域）。tab 本身是子元素，不会成为拖拽目标，点击/拖拽
            排序不受影响。Windows 有自绘 TitleBar，不需要。 */}
        <header
          className="tool-side-panel__header"
          {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})}
        >
          <div
            className="tool-side-panel__tabs"
            {...(IS_MACOS ? { "data-tauri-drag-region": true } : {})}
          >
            <ArtifactTabsBar
              tabs={tabsApi.tabs}
              activeTabId={tabsApi.activeTabId}
              onSelect={tabsApi.setActiveTab}
              onClose={tabsApi.closeTab}
              onReorder={tabsApi.reorderTabs}
            />
          </div>
          <div className="tool-side-panel__actions">
            <button
              type="button"
              className="tool-side-panel__icon-btn"
              onClick={() => setMaximized((m) => !m)}
              title={maximized ? "恢复" : "最大化"}
              aria-label={maximized ? "恢复" : "最大化"}
            >
              {maximized ? <RestoreIcon size="sm" /> : <MaximizeIcon size="sm" />}
            </button>
            <button
              type="button"
              className="tool-side-panel__icon-btn"
              onClick={onClose}
              aria-label="关闭面板"
              title="关闭"
            >
              <CloseIcon size="sm" />
            </button>
          </div>
        </header>

        <div className="tool-side-panel__body">
          {showToolDetail ? (
            toolCall ? (
              <ToolCallDetailBody
                tc={toolCall}
                onOpenPath={(path) => {
                  onSelectArtifact({
                    id: path,
                    path,
                    kind: toolCall.kind,
                    title: toolCall.title,
                    toolCallId: toolCall.toolCallId,
                    status: toolCall.status,
                  });
                }}
              />
            ) : (
              <p className="tool-side-panel__empty">在对话中点击工具行查看详情</p>
            )
          ) : (
            <MainContent
              view={view}
              artifacts={artifacts}
              cwd={cwd}
              previewPath={previewPath}
              selectedArtifactId={selectedArtifactId}
              selectedFilePath={selectedFilePath}
              browserUrl={browserUrl}
              onArtifactSelect={(a) => {
                const tc = findToolCall?.(a.toolCallId);
                if (tc) {
                  onSelectTool(tc);
                } else {
                  onToast?.("关联工具记录已不可用，可继续预览文件内容");
                }
              }}
              onBrowserUrlChange={setBrowserUrl}
              onOpenOs={(path) => openLocalPath(path, {
                cwd,
                type: "file",
                onToast,
                revealFile: false,
              })}
              onToast={onToast}
            />
          )}
        </div>
      </div>
    </aside>
  );
}

// ---------- 左导航内容：按视图渲染列表 ----------

function NavContent({
  view,
  artifacts,
  changes,
  cwd,
  selectedArtifactId,
  selectedFilePath,
  onArtifactSelect,
  onFileSelect,
  onToast,
}: {
  view: WorkspaceView;
  artifacts: SessionArtifact[];
  changes: FileChange[];
  cwd?: string;
  selectedArtifactId?: string;
  selectedFilePath?: string;
  onArtifactSelect: (id?: string) => void;
  onFileSelect: (path?: string) => void;
  onToast?: (msg: string) => void;
}) {
  if (view === "artifacts") {
    return (
      <ArtifactsNavList
        artifacts={artifacts}
        selectedId={selectedArtifactId}
        onSelect={(a) => onArtifactSelect(a.id)}
      />
    );
  }
  if (view === "changes") {
    return (
      <ChangesNavList
        changes={changes}
        selectedPath={selectedArtifactId}
        onSelect={(path) => onArtifactSelect(path)}
      />
    );
  }
  if (view === "fileTree") {
    return (
      <FileTreeView
        rootPath={cwd}
        selectedPath={selectedFilePath}
        onFileSelect={(p) => onFileSelect(p)}
        onToast={onToast}
      />
    );
  }
  // preview 视图：导航列显示历史/提示。
  return (
    <div className="tool-side-panel__empty">
      输入网址后在右侧预览。
    </div>
  );
}

function ArtifactsNavList({
  artifacts,
  selectedId,
  onSelect,
}: {
  artifacts: SessionArtifact[];
  selectedId?: string;
  onSelect: (a: SessionArtifact) => void;
}) {
  if (artifacts.length === 0) {
    return (
      <p className="tool-side-panel__empty">
        本会话还没有可展示的文件产物。工具写入/修改文件后会出现在这里。
      </p>
    );
  }
  return (
    <ul className="artifacts-list">
      {artifacts.map((a) => (
        <li key={a.id} className="artifacts-list__item">
          <button
            type="button"
            className={
              "artifacts-list__main" +
              (a.id === selectedId ? " artifacts-list__main--active" : "")
            }
            onClick={() => onSelect(a)}
            title={a.path}
          >
            <span className="artifacts-list__name">{basename(a.path)}</span>
            <span className="artifacts-list__path">{a.path}</span>
            <span className="artifacts-list__meta">
              {a.kind}
              {a.status === "failed" ? " · 失败" : ""}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

function ChangesNavList({
  changes,
  selectedPath,
  onSelect,
}: {
  changes: FileChange[];
  selectedPath?: string;
  onSelect: (path: string) => void;
}) {
  if (changes.length === 0) {
    return <p className="tool-side-panel__empty">本会话暂无文件变更。</p>;
  }
  return (
    <ul className="artifacts-list">
      {changes.map((f) => (
        <li key={f.path} className="artifacts-list__item">
          <button
            type="button"
            className={
              "artifacts-list__main" +
              (f.path === selectedPath ? " artifacts-list__main--active" : "")
            }
            onClick={() => onSelect(f.path)}
            title={f.path}
          >
            <span className="artifacts-list__name">{f.name}</span>
            <span className="artifacts-list__path">{f.path}</span>
            <span className="artifacts-list__meta">
              +{f.added} / -{f.removed}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

// ---------- 主内容：按视图渲染详情 ----------

function MainContent({
  view,
  artifacts,
  cwd,
  previewPath,
  selectedArtifactId,
  selectedFilePath,
  browserUrl,
  onArtifactSelect,
  onBrowserUrlChange,
  onOpenOs,
  onToast,
}: {
  view: WorkspaceView;
  artifacts: SessionArtifact[];
  cwd?: string;
  previewPath?: string | null;
  selectedArtifactId?: string;
  selectedFilePath?: string;
  browserUrl?: string;
  onArtifactSelect: (a: SessionArtifact) => void;
  onBrowserUrlChange: (url?: string) => void;
  onOpenOs: (path: string) => Promise<boolean>;
  onToast?: (msg: string) => void;
}) {
  if (view === "preview") {
    return (
      <BrowserPreview
        url={browserUrl ?? ""}
        onUrlChange={onBrowserUrlChange}
      />
    );
  }
  if (view === "fileTree") {
    const path = selectedFilePath ?? previewPath ?? null;
    if (!path) {
      return <p className="tool-side-panel__empty">选择文件查看内容</p>;
    }
    return (
      <ArtifactFilePreview path={path} cwd={cwd} onToast={onToast} onOpenOs={() => onOpenOs(path)} />
    );
  }
  if (view === "changes") {
    const path = selectedArtifactId;
    if (!path) {
      return <p className="tool-side-panel__empty">在左侧选择文件查看变更</p>;
    }
    return (
      <ArtifactFilePreview path={path} cwd={cwd} onToast={onToast} onOpenOs={() => onOpenOs(path)} />
    );
  }
  // artifacts：选中产物时预览其文件。
  if (view === "artifacts") {
    const id = selectedArtifactId;
    if (!id) {
      return (
        <p className="tool-side-panel__empty">在左侧选择产物查看内容</p>
      );
    }
    const a = artifacts.find((x) => x.id === id);
    const path = a?.path ?? previewPath ?? null;
    if (!path) return <p className="tool-side-panel__empty">无文件路径</p>;
    return (
      <div className="tool-side-panel__preview-wrap">
        {a && (
          <button
            type="button"
            className="tool-side-panel__link"
            onClick={() => onArtifactSelect(a)}
            title="查看产生此文件的工具调用"
          >
            查看关联工具
          </button>
        )}
        <ArtifactFilePreview path={path} cwd={cwd} onToast={onToast} onOpenOs={() => onOpenOs(path)} />
      </div>
    );
  }
  return null;
}

// ---------- ArtifactFilePreview ----------

type PreviewState =
  | { type: "loading" }
  | { type: "ready"; content: string }
  | { type: "unauthorized"; message: string }
  | { type: "error"; message: string; canOpen: boolean };

function ArtifactFilePreview({
  path,
  cwd,
  onToast,
  onOpenOs,
}: {
  path: string;
  cwd?: string;
  onToast?: (msg: string) => void;
  onOpenOs: () => Promise<boolean>;
}) {
  const [state, setState] = useState<PreviewState>({ type: "loading" });
  const [retry, setRetry] = useState(0);
  const [authorizing, setAuthorizing] = useState(false);
  const [opening, setOpening] = useState(false);
  const kind = detectPreviewKind(path);

  useEffect(() => {
    let cancelled = false;
    setState({ type: "loading" });
    (async () => {
      try {
        const content = await loadPreviewContent(path, cwd, kind);
        if (!cancelled) setState({ type: "ready", content });
      } catch (e) {
        if (!cancelled) {
          const message = errorMessage(e);
          setState(isUnauthorizedPathError(e)
            ? { type: "unauthorized", message }
            : { type: "error", message, canOpen: !isMissingPathError(message) });
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [path, cwd, kind, retry]);

  const authorize = async () => {
    setAuthorizing(true);
    try {
      const result = await authorizeArtifactFile(path, cwd);
      if (result === "authorized") {
        onToast?.("已授权该文件，可以在面板中预览");
        setRetry((value) => value + 1);
      } else if (result === "mismatch") {
        onToast?.("所选文件与当前产物不一致，请选择列表中显示的原文件");
      }
    } catch (error) {
      onToast?.(`授权失败：${errorMessage(error)}`);
    } finally {
      setAuthorizing(false);
    }
  };

  const openWithSystem = async () => {
    setOpening(true);
    try {
      if (await onOpenOs()) onToast?.("已用系统应用打开文件");
    } finally {
      setOpening(false);
    }
  };

  return (
    <div className="artifact-file-preview">
      <div className="file-preview__bar">
        <span className="file-preview__path" title={path}>
          {path}
        </span>
        {(state.type === "ready" || (state.type === "error" && state.canOpen)) && (
          <button
            type="button"
            className="file-preview__open"
            onClick={() => void openWithSystem()}
            disabled={opening}
          >
            {opening ? "正在打开…" : "用系统应用打开"}
          </button>
        )}
      </div>
      {state.type === "loading" && <p className="tool-side-panel__empty">加载中…</p>}
      {state.type === "unauthorized" && (
        <div className="file-preview__err file-preview__err--authorization" role="alert">
          <p>该成果位于当前工作区之外。为保护本地文件，需要你确认授权后才能预览或打开。</p>
          <p className="file-preview__detail">{state.message}</p>
          <button
            type="button"
            className="file-preview__open"
            onClick={() => void authorize()}
            disabled={authorizing}
          >
            {authorizing ? "等待选择…" : "选择该文件并授权预览"}
          </button>
        </div>
      )}
      {state.type === "error" && (
        <div className="file-preview__err" role="alert">
          <p>无法在面板内预览：{state.message}</p>
          {state.canOpen && (
            <p className="file-preview__detail">文件仍可使用系统应用打开。</p>
          )}
        </div>
      )}
      {state.type === "ready" && (
        <RichFilePreview
          filename={basename(path)}
          content={state.content}
          onCopyText={(content) => {
            return copyShareText(content).then((ok) => {
              onToast?.(ok ? "已复制文件内容" : "复制失败，请检查剪贴板权限");
              return ok;
            });
          }}
        />
      )}
    </div>
  );
}

async function loadPreviewContent(
  path: string,
  cwd: string | undefined,
  kind: PreviewKind,
): Promise<string> {
  if (kind === "markdown" || kind === "code" || kind === "text") {
    return invoke<string>("read_text_file", {
      path,
      cwd: cwd ?? null,
      maxBytes: 256 * 1024,
    });
  }
  if (kind === "binary") {
    // Validate existence and native authorization without decoding binary data.
    await invoke("path_stat", { path, cwd: cwd ?? null });
    return "";
  }
  const absolutePath = await resolveAuthorizedPath(path, cwd);
  const base64 = await invoke<string>("read_file_base64", {
    path: absolutePath,
    maxBytes: 1024 * 1024,
  });
  return `data:${previewMimeType(path, kind)};base64,${base64}`;
}

async function resolveAuthorizedPath(path: string, cwd?: string): Promise<string> {
  const stat = await invoke<{ absolute: string }>("path_stat", {
    path,
    cwd: cwd ?? null,
  });
  return stat.absolute;
}

function previewMimeType(path: string, kind: PreviewKind): string {
  const extension = path.toLowerCase().match(/\.([a-z0-9]+)$/)?.[1] ?? "";
  const byExtension: Record<string, string> = {
    png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
    webp: "image/webp", svg: "image/svg+xml", bmp: "image/bmp", ico: "image/x-icon",
    pdf: "application/pdf", mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg",
    flac: "audio/flac", m4a: "audio/mp4", aac: "audio/aac", mp4: "video/mp4",
    webm: "video/webm", mov: "video/quicktime", mkv: "video/x-matroska", avi: "video/x-msvideo",
    docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    xls: "application/vnd.ms-excel",
  };
  return byExtension[extension]
    ?? (kind === "image" ? "image/*" : "application/octet-stream");
}

function isMissingPathError(message: string): boolean {
  return /路径不存在|不是文件|找不到|not found|does not exist/i.test(message);
}

// ---------- 工具函数 ----------

function basename(p: string): string {
  const norm = p.replace(/\\/g, "/");
  const i = norm.lastIndexOf("/");
  return i >= 0 ? norm.slice(i + 1) : norm;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function getViewportWidth(): number {
  return typeof window === "undefined"
    ? DEFAULT_WIDTH / MAX_WIDTH_RATIO
    : window.innerWidth;
}

function getPanelMaxWidth(viewportWidth: number): number {
  return Math.max(MIN_WIDTH, viewportWidth * MAX_WIDTH_RATIO);
}

function readStoredDimension(
  key: string,
  fallback: number,
  min: number,
  max: number,
): number {
  if (typeof window === "undefined") return fallback;
  try {
    const raw = window.localStorage.getItem(key);
    if (raw == null) return fallback;
    const parsed = Number(raw);
    return Number.isFinite(parsed) ? clamp(parsed, min, max) : fallback;
  } catch {
    return fallback;
  }
}

function writeStoredDimension(key: string, value: number): void {
  try {
    window.localStorage.setItem(key, String(value));
  } catch {
    // localStorage 被禁用不影响当前会话内的面板使用。
  }
}

/** 启动左列宽度拖拽（pointer 事件，松开时持久化由外层 effect 处理）。 */
function startResizeNav(
  e: React.PointerEvent<HTMLDivElement>,
  currentWidth: number,
  maxWidth: number,
  setWidth: (w: number) => void,
) {
  e.preventDefault();
  const startX = e.clientX;
  const pointerId = e.pointerId;
  const onMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return;
    const delta = ev.clientX - startX;
    const next = Math.min(
      Math.max(currentWidth + delta, MIN_NAV_WIDTH),
      maxWidth,
    );
    setWidth(next);
  };
  const onEnd = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return;
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onEnd);
    window.removeEventListener("pointercancel", onEnd);
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onEnd);
  window.addEventListener("pointercancel", onEnd);
}

/** 启动全宽拖拽（面板左边缘，向左拖增大宽度）。 */
function startResizeEdge(
  e: React.PointerEvent<HTMLDivElement>,
  currentWidth: number,
  maxWidth: number,
  setWidth: (w: number) => void,
) {
  e.preventDefault();
  const startX = e.clientX;
  const pointerId = e.pointerId;
  const onMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return;
    // 向左拖（delta 负）→ 宽度变大。
    const delta = ev.clientX - startX;
    const next = Math.min(
      Math.max(currentWidth - delta, MIN_WIDTH),
      maxWidth,
    );
    setWidth(next);
  };
  const onEnd = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return;
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onEnd);
    window.removeEventListener("pointercancel", onEnd);
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onEnd);
  window.addEventListener("pointercancel", onEnd);
}
