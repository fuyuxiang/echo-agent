import { artifactFileKey } from "@/lib/artifact-file-key";
/** One session-scoped container for resources, runtime details and tool inspection.
 * Narrow panels show a list or preview; a split requires a measured 360px preview.
 */
import { useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { SidebarLayoutContext } from "@/lib/sidebar-layout";
import type { ToolCallView } from "@/stores/session-store";
import type { SessionArtifact } from "@/lib/session-artifacts";
import { authorizeDocumentArtifact } from "@/lib/artifact-catalog";
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
import { ViewSelector, defaultViews } from "./workspace-panel/ViewSelector";
import { ArtifactTabsBar } from "./workspace-panel/ArtifactTabsBar";
import { FileTreeView } from "./workspace-panel/FileTreeView";
import { ActionMenu } from "./ActionMenu";
import { ArrowLeft, Settings2 } from "lucide-react";
import { BrowserPreview } from "./BrowserPreview";
import {
  EchoPinIcon,
  EchoUnpinIcon,
  MaximizeIcon,
  RestoreIcon,
  CloseIcon,
} from "@/foundation/components/Icon/icons";

/** The outer navigation and panel use the same view identifiers. */
export type ToolSidePanelMode = WorkspaceView | "tool";
interface PanelSelection {
  artifact?: string; file?: string; url?: string; list: boolean; scroll: number;
  listScroll?: Partial<Record<WorkspaceView, number>>;
  bodyScroll?: Partial<Record<WorkspaceView, number>>;
  fileTree?: { root?: string; expandedPaths: string[] };
}
const sessionSelections = new Map<string, PanelSelection>();

// ---------- 持久化常量 ----------
const WIDTH_KEY = "tool-side-panel-width";
const NAV_WIDTH_KEY = "tool-side-panel-nav-width";
const DEFAULT_WIDTH = 420;
const DEFAULT_NAV_WIDTH = 180;
const MIN_WIDTH = 280;
const MAX_WIDTH_RATIO = 0.6; // 占视口 60%
const MIN_NAV_WIDTH = 180;
const MAX_NAV_WIDTH = 220;
const MIN_MAIN_WIDTH = 360;
const NAV_SASH_WIDTH = 5;

interface ToolSidePanelProps {
  open: boolean;
  mode: ToolSidePanelMode;
  toolCall?: ToolCallView | null;
  artifacts: SessionArtifact[];
  previewPath?: string | null;
  previewRequestId?: number;
  navigationRequestId?: number;
  cwd?: string;
  workspaceInfo?: ReactNode;
  workspaceAvailable?: boolean;
  /** 会话消息（用于聚合文件变更）。 */
  messages?: ChatMessage[];
  /** 会话 id（用于标签页会话隔离）。 */
  sessionId?: string;
  onToast?: (msg: string) => void;
  onClose: () => void;
  onBack?: () => void;
  onModeChange?: (mode: ToolSidePanelMode) => void;
  onSelectTool: (tc: ToolCallView) => void;
  onSelectArtifact: (a: SessionArtifact) => void;
  onOpenArtifacts: () => void;
  findToolCall?: (id: string) => ToolCallView | undefined;
  details?: ReactNode;
  detailsRef?: React.RefObject<HTMLDivElement>;
}

export function ToolSidePanel({
  open,
  workspaceInfo,
  workspaceAvailable = true,
  mode,
  toolCall,
  artifacts,
  previewPath,
  previewRequestId,
  navigationRequestId,
  cwd,
  messages,
  sessionId,
  onToast,
  onClose,
  onBack,
  onModeChange,
  onSelectTool,
  onSelectArtifact,
  findToolCall,
  details,
  detailsRef,
}: ToolSidePanelProps) {
  const toolExecutionEnded = useMemo(() => open && toolCall ? [...(messages ?? [])].reverse().find(message => message.parts.some(
    part => part.kind === "tool_call" && part.toolCall.toolCallId === toolCall?.toolCallId,
  ))?.complete ?? false : false, [open, messages, toolCall?.toolCallId]);
  const [legacyView, setView] = useState<WorkspaceView>(mode === "tool" ? "artifacts" : mode);
  const view: WorkspaceView = onModeChange && mode !== "tool" ? mode : legacyView;
  useEffect(() => { if (mode !== "tool") setView(mode); }, [mode]);
  const browserVisited = useRef(false);
  if (open && view === "browser") browserVisited.current = true;
  const fileTreeVisit = useRef({ sessionId, visited: false });
  if (fileTreeVisit.current.sessionId !== sessionId) fileTreeVisit.current = { sessionId, visited: false };
  if (open && view === "fileTree") fileTreeVisit.current.visited = true;
  const saved = sessionId ? sessionSelections.get(sessionId) : undefined;
  const [selectedArtifactId, setSelectedArtifactId] = useState<string | undefined>(saved?.artifact);
  const [selectedFilePath, setSelectedFilePath] = useState<string | undefined>(saved?.file);
  const [browserUrl, setBrowserUrl] = useState<string | undefined>(saved?.url);
  const [showList, setShowList] = useState(saved?.list ?? true);
  const [selectionOwner, setSelectionOwner] = useState(sessionId);
  const listRef = useRef<HTMLDivElement>(null);
  const listContentRef = useRef<HTMLDivElement>(null);
  const restoringListRef = useRef(false);
  const internalBodyRef = useRef<HTMLDivElement>(null);
  const bodyRef = detailsRef ?? internalBodyRef;
  const panelRef = useRef<HTMLElement>(null);
  const [measuredWidth, setMeasuredWidth] = useState(0);
  const selectionRef = useRef<{ owner?: string; value: PanelSelection }>({ owner: sessionId, value: { list: true, scroll: 0 } });
  if (selectionOwner !== sessionId) {
    const previous = selectionRef.current;
    if (previous.owner) sessionSelections.set(previous.owner, previous.value);
    selectionRef.current = { owner: sessionId, value: saved ?? { list: true, scroll: 0 } };
    setSelectionOwner(sessionId);
    setSelectedArtifactId(saved?.artifact);
    setSelectedFilePath(saved?.file);
    setBrowserUrl(saved?.url);
    setShowList(saved?.list ?? true);
  } else {
    selectionRef.current = { owner: sessionId, value: { artifact: selectedArtifactId, file: selectedFilePath,
      url: browserUrl, list: showList,
      bodyScroll: selectionRef.current.value.bodyScroll ?? saved?.bodyScroll,
      listScroll: selectionRef.current.value.listScroll ?? saved?.listScroll,
      fileTree: selectionRef.current.value.fileTree ?? saved?.fileTree,
      scroll: selectionRef.current.value.scroll } };
  }
  useEffect(() => () => {
    const { owner, value } = selectionRef.current;
    if (!owner) return;
    sessionSelections.set(owner, value);
    if (sessionSelections.size > 50) sessionSelections.delete(sessionSelections.keys().next().value!);
  }, []);
  useLayoutEffect(() => {
    if (bodyRef.current) bodyRef.current.scrollTop = saved?.bodyScroll?.[view] ?? 0;
  }, [showList, view, sessionId]);
  const handledPreview = useRef<{ path?: string | null; request?: number }>({});
  const handledNavigation = useRef(navigationRequestId);
  useLayoutEffect(() => {
    if (handledNavigation.current === navigationRequestId) return;
    handledNavigation.current = navigationRequestId;
    setShowList(true);
  }, [navigationRequestId]);
  useEffect(() => {
    if (handledPreview.current.path === previewPath && handledPreview.current.request === previewRequestId) return;
    handledPreview.current = { path: previewPath, request: previewRequestId };
    if (!previewPath) return;
    const key = artifactFileKey(previewPath, cwd);
    if (view === "artifacts") setSelectedArtifactId(artifacts.find(item => artifactFileKey(item.path, cwd) === key)?.id ?? key);
    else if (view === "fileTree") setSelectedFilePath(previewPath);
    else return;
    setShowList(false);
  }, [previewPath, previewRequestId, view, cwd, sessionId, artifacts]);

  // 文件变更数据。
  const changes = useMemo<FileChange[]>(
    () => (messages ? aggregateFileChanges(messages).files : []),
    [messages],
  );
  const hasChanges = changes.length > 0;

  // ---- 视图切换 ----
  const handleViewChange = useCallback((v: WorkspaceView) => {
    if (onModeChange) onModeChange(v);
    else setView(v);
    setShowList(true);
  }, [onModeChange]);

  const handleArtifactSelect = useCallback(
    (id?: string) => {
      setSelectedArtifactId(id);
      setShowList(!id);
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
      setShowList(!path);
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
    [onSelectArtifact, cwd],
  );

  // ---- 统一标签页 ----
  const tabsApi = useUnifiedTabs({
    resetKey: sessionId,
    enabled: open,
    currentView: view,
    selectedArtifactId: view === "changes" && details ? undefined : selectedArtifactId,
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
  const fullView = maximized || viewportWidth < 1180 || panelMaxWidth < 380;
  const [pinned, setPinned] = useState(false);
  const [navCollapsed, setNavCollapsed] = useState(false);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const registerPanel = shellLayout?.registerPanel;
  useLayoutEffect(() => {
    registerPanel?.(open && !fullView ? { minimumWidth: MIN_WIDTH, close: () => closeRef.current() } : null);
    return () => registerPanel?.(null);
  }, [open, fullView, registerPanel]);

  const renderedPanelWidth = clamp(width, MIN_WIDTH, panelMaxWidth);
  const availablePanelWidth = fullView ? measuredWidth || viewportWidth : renderedPanelWidth;
  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    const measure = () => setMeasuredWidth(panel.clientWidth);
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(panel);
    return () => observer?.disconnect();
  }, [open, fullView]);
  const navMaxWidth = Math.max(
    MIN_NAV_WIDTH,
    Math.min(MAX_NAV_WIDTH, availablePanelWidth - MIN_MAIN_WIDTH - NAV_SASH_WIDTH),
  );
  const renderedNavWidth = clamp(navWidth, MIN_NAV_WIDTH, navMaxWidth);

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

  const views = defaultViews({ hasChanges });
  const showToolDetail = mode === "tool";
  const runtimeView = Boolean(details) && (view === "plan" || view === "subagents" || view === "teams" || view === "changes");
  const listView = !showToolDetail && (view === "artifacts" || view === "fileTree");
  const split = listView && availablePanelWidth >= MIN_MAIN_WIDTH + renderedNavWidth + NAV_SASH_WIDTH && !effectiveNavCollapsed;
  useLayoutEffect(() => {
    const viewport = listRef.current;
    if (!viewport || !open || !listView || !split && !showList) return;
    const positions = selectionRef.current.value.listScroll;
    const target = positions ? positions[view] ?? 0 : saved?.scroll ?? 0;
    let active = true;
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => restore());
    const cancel = () => { active = false; restoringListRef.current = false; observer?.disconnect(); };
    const restore = () => {
      if (!active) return;
      viewport.scrollTop = target;
      if (Math.abs(viewport.scrollTop - target) < 1) cancel();
    };
    restoringListRef.current = true;
    if (listContentRef.current) observer?.observe(listContentRef.current);
    restore();
    // Directory children can arrive after the panel's first layout. Wait for
    // their actual height, but let any new reading intent take precedence.
    const events = ["wheel", "pointerdown", "touchstart", "keydown"] as const;
    for (const event of events) viewport.addEventListener(event, cancel, {capture:true,passive:true});
    return () => {
      cancel();
      for (const event of events) viewport.removeEventListener(event, cancel, true);
    };
  }, [open, listView, split, showList, view, sessionId]);
  const list = <>
    {view !== "fileTree" && <NavContent view={view} artifacts={artifacts} changes={changes}
      selectedArtifactId={selectedArtifactId}
      onArtifactSelect={handleArtifactSelect} />}
    {fileTreeVisit.current.visited && <div hidden={view !== "fileTree"} className="task-details__file-tree">
      {workspaceAvailable ? <FileTreeView key={`${sessionId}:${cwd}`} rootPath={cwd} selectedPath={selectedFilePath} showRootInEmptyState={false}
        onFileSelect={handleFileSelect} onToast={onToast}
        initialExpandedPaths={selectionRef.current.value.fileTree?.root === cwd ? selectionRef.current.value.fileTree?.expandedPaths ?? [] : []}
        onExpandedPathsChange={expandedPaths => {
          selectionRef.current.value.fileTree = { root: cwd, expandedPaths };
          if (sessionId) sessionSelections.set(sessionId, selectionRef.current.value);
        }} /> : <p className="tool-side-panel__empty">工作目录暂不可用，请先恢复连接</p>}
    </div>}
  </>;
  const selectedName = basename((view === "artifacts" ? artifacts.find(a => a.id === selectedArtifactId)?.path : selectedFilePath) ?? previewPath ?? "");
  const singleTab = tabsApi.tabs.length === 1 ? tabsApi.tabs[0] : undefined;
  const showingContent = !showToolDetail && !runtimeView && (!listView || split || !showList);
  const activeContentTab = showingContent ? tabsApi.tabs.find(tab => tab.id === tabsApi.activeTabId) : undefined;
  const singleCurrentTab = singleTab && activeContentTab?.id === singleTab.id ? singleTab : undefined;
  const closeSingleContent = () => {
    if (!singleTab) return;
    tabsApi.closeTab(singleTab.id);
    if (listView) setShowList(true);
    window.requestAnimationFrame(() => {
      const target = listRef.current?.querySelector<HTMLButtonElement>("button")
        ?? panelRef.current?.querySelector<HTMLButtonElement>('.workspace-view-tabs [aria-selected="true"]');
      target?.focus({preventScroll:true});
    });
  };
  const closeContentButton = singleCurrentTab && !showToolDetail && !runtimeView
    ? <button type="button" className="tool-side-panel__icon-btn" aria-label={`关闭标签 ${singleCurrentTab.label}`} data-tip="关闭当前内容" onClick={closeSingleContent}><CloseIcon size="sm" /></button>
    : null;
  return <aside id="session-workspace" hidden={!open} ref={panelRef} className={"tool-side-panel task-details" + (fullView ? " tool-side-panel--maximized" : "")}
    style={fullView ? undefined : { width: `${renderedPanelWidth}px` }} aria-label="工作区面板">
    {!fullView && <div className="tool-side-panel__edge-sash" onPointerDown={e => startResizeEdge(e, renderedPanelWidth, panelMaxWidth, setWidth)} />}
    <div className="task-details__shell">
      <header className="tool-side-panel__header">
        <ViewSelector view={view} views={views} onChange={handleViewChange} />
        <div className="tool-side-panel__actions">
          {listView && availablePanelWidth >= MIN_MAIN_WIDTH + renderedNavWidth + NAV_SASH_WIDTH && <ActionMenu label="详情布局" menuLabel="详情布局选项" trigger={<Settings2 size={16} />}>
            {({close}) => <>
              <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { handlePinnedToggle(); close(true); }}>{pinned ? <EchoUnpinIcon size="sm" /> : <EchoPinIcon size="sm" />}{pinned ? "取消钉住" : "钉住列表"}</button>
              <button type="button" role="menuitem" className="topbar-actions__item" disabled={pinned} title={pinned ? "先取消钉住列表" : undefined} onClick={() => { setNavCollapsed(v => !v); close(true); }}>{navCollapsed ? "展开列表" : "收起列表"}</button>
            </>}
          </ActionMenu>}
          <button type="button" className="tool-side-panel__icon-btn" onClick={() => setMaximized(m => !m)} aria-label={maximized ? "恢复" : "最大化"} title={maximized ? "恢复" : "最大化"}>{maximized ? <RestoreIcon size="sm" /> : <MaximizeIcon size="sm" />}</button>
          <button type="button" className="tool-side-panel__icon-btn" onClick={onClose} aria-label="关闭面板" title="返回会话"><CloseIcon size="sm" /></button>
        </div>
      </header>
      {!showToolDetail && <ViewSelector view={view} views={views} onChange={handleViewChange} secondary />}
      {view === "fileTree" && !showToolDetail && workspaceInfo}
      {onBack && <div className="task-details__breadcrumb"><button type="button" onClick={onBack}><ArrowLeft size={15} />返回上一视图</button><span>{showToolDetail ? "操作详情" : singleTab?.label ?? selectedName}</span>{closeContentButton}</div>}
      {showToolDetail && !onBack && <div className="task-details__breadcrumb"><strong>操作详情</strong></div>}
      {tabsApi.tabs.length > 0 && !singleCurrentTab && !showToolDetail && !runtimeView && <div className="task-details__tabs"><ArtifactTabsBar tabs={tabsApi.tabs} activeTabId={activeContentTab?.id} onSelect={tabsApi.setActiveTab} onClose={tabsApi.closeTab} onReorder={tabsApi.reorderTabs} /></div>}
      {singleCurrentTab && !showToolDetail && !runtimeView && !onBack && <div className="task-details__single-content">
        {listView && !showList && !split && <button type="button" className="task-details__back-list" onClick={() => setShowList(true)}><ArrowLeft size={15} />返回{view === "artifacts" ? "成果列表" : "文件列表"}</button>}
        <span title={singleCurrentTab.subtitle}>{singleCurrentTab.label}</span>{closeContentButton}
      </div>}
      {listView && !showList && !split && !singleCurrentTab && !onBack && <div className="task-details__breadcrumb"><button type="button" onClick={() => setShowList(true)}><ArrowLeft size={15} />返回{view === "artifacts" ? "成果列表" : "文件列表"}</button><span title={selectedName}>{selectedName}</span></div>}
      <div className="task-details__content">
        <div hidden={!listView || !split && !showList} className={split ? "task-details__list task-details__list--split" : "task-details__list"} style={split ? {width: renderedNavWidth} : undefined} ref={listRef} onScroll={e => {
          if (restoringListRef.current || !listView || !split && !showList) return;
          const value = selectionRef.current.value;
          value.scroll = e.currentTarget.scrollTop;
          value.listScroll = {...value.listScroll, [view]: value.scroll};
          if (sessionId) sessionSelections.set(sessionId, value);
        }}><div ref={listContentRef}>{list}</div></div>
        {split && <div className="tool-side-panel__sash" onPointerDown={e => startResizeNav(e, renderedNavWidth, navMaxWidth, setNavWidth)} />}
        <div hidden={listView && !split && showList} ref={bodyRef} onScroll={e => {
          const value = selectionRef.current.value;
          value.bodyScroll = {...value.bodyScroll, [view]:e.currentTarget.scrollTop};
          if (sessionId) sessionSelections.set(sessionId,value);
        }} className="tool-side-panel__body task-details__body">
          <div hidden={!runtimeView || showToolDetail}>{details}</div>
          {browserVisited.current && <div className="task-details__browser" hidden={view !== "browser" || showToolDetail}><BrowserPreview url={browserUrl ?? ""} onUrlChange={setBrowserUrl} /></div>}
          {(!listView || split || !showList) && ((!runtimeView && view !== "browser") || showToolDetail) && (showToolDetail ? toolCall ? <ToolCallDetailBody tc={toolCall} executionEnded={toolExecutionEnded} onOpenPath={path => onSelectArtifact({id:path,path,kind:toolCall.kind,title:toolCall.title,toolCallId:toolCall.toolCallId,status:toolCall.status})} /> : <p className="tool-side-panel__empty">选择操作查看详情</p> : <MainContent view={view} artifacts={artifacts} sessionId={sessionId} cwd={cwd} previewPath={previewPath}
            selectedArtifactId={selectedArtifactId} selectedFilePath={selectedFilePath} browserUrl={browserUrl}
            onArtifactSelect={a => { const tc = findToolCall?.(a.toolCallId); if (tc) onSelectTool(tc); else onToast?.("关联工具记录已不可用，可继续预览文件内容"); }}
            onBrowserUrlChange={setBrowserUrl} onOpenOs={path => openLocalPath(path,{cwd,type:"file",onToast,revealFile:false})} onToast={onToast} />)}
        </div>
      </div>
    </div>
  </aside>;
}

// ---------- 左导航内容：按视图渲染列表 ----------

function NavContent({
  view,
  artifacts,
  changes,
  selectedArtifactId,
  onArtifactSelect,
}: {
  view: WorkspaceView;
  artifacts: SessionArtifact[];
  changes: FileChange[];
  selectedArtifactId?: string;
  onArtifactSelect: (id?: string) => void;
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
        本任务暂无可展示的文件。工具写入或修改文件后，相关记录会显示在这里。
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
              {(basename(a.path).split(".").pop() ?? "文件").toUpperCase()} · {a.status === "failed" ? "生成失败" : a.kind === "edit" || a.kind === "edit_file" ? "已更新" : "已生成"}
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
  sessionId,
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
  sessionId?: string;
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
  if (view === "browser") {
    return (
      <BrowserPreview
        url={browserUrl ?? ""}
        onUrlChange={onBrowserUrlChange}
      />
    );
  }
  if (view === "fileTree") {
    const path = selectedFilePath ?? null;
    if (!path) {
      return <p className="tool-side-panel__empty">选择文件查看内容</p>;
    }
    return (
      <ArtifactFilePreview path={path} cwd={cwd} sessionId={sessionId}
          artifact={artifacts.find(item => artifactFileKey(item.path, cwd) === artifactFileKey(path, cwd))} onToast={onToast} onOpenOs={() => onOpenOs(path)} />
    );
  }
  if (view === "changes") {
    const path = selectedArtifactId;
    if (!path) {
      return <p className="tool-side-panel__empty">在左侧选择文件查看变更</p>;
    }
    return (
      <ArtifactFilePreview path={path} cwd={cwd} sessionId={sessionId}
          artifact={artifacts.find(item => artifactFileKey(item.path, cwd) === artifactFileKey(path, cwd))} onToast={onToast} onOpenOs={() => onOpenOs(path)} />
    );
  }
  // artifacts：选中成果时预览其文件。
  if (view === "artifacts") {
    const id = selectedArtifactId;
    if (!id) {
      return (
        <p className="tool-side-panel__empty">在左侧选择成果查看内容</p>
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
        <ArtifactFilePreview path={path} cwd={cwd} sessionId={sessionId}
          artifact={artifacts.find(item => artifactFileKey(item.path, cwd) === artifactFileKey(path, cwd))} onToast={onToast} onOpenOs={() => onOpenOs(path)} />
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
  sessionId,
  artifact,
  onToast,
  onOpenOs,
}: {
  path: string;
  cwd?: string;
  sessionId?: string;
  artifact?: SessionArtifact;
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
      let authorized = false;
      try {
        if (artifact) {
          if (!sessionId && artifact.kind === "document_export") throw new Error("会话信息不可用，请重新打开原会话");
          if (sessionId) await authorizeDocumentArtifact(sessionId, artifact);
        }
        authorized = true;
        if (cancelled) return;
        const content = await loadPreviewContent(path, cwd, kind);
        if (!cancelled) setState({ type: "ready", content });
      } catch (e) {
        if (!cancelled) {
          const message = errorMessage(e);
          setState(authorized && isUnauthorizedPathError(e)
            ? { type: "unauthorized", message }
            : { type: "error", message, canOpen: authorized && !isMissingPathError(message) });
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [path, cwd, kind, retry, sessionId, artifact?.kind, artifact?.toolCallId]);

  const authorize = async () => {
    setAuthorizing(true);
    try {
      const result = await authorizeArtifactFile(path, cwd);
      if (result === "authorized") {

        setRetry((value) => value + 1);
      } else if (result === "mismatch") {
        onToast?.("所选文件与当前成果不一致，请选择列表中显示的原文件");
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
      if (await onOpenOs()) {}
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
        {(state.type === "ready" || (state.type === "error" && state.canOpen)) && <button
          type="button" className="file-preview__open" disabled={opening}
          onClick={() => void openLocalPath(path, { cwd, type: "file", onToast, revealFile: true, revealOnly: true })}>在文件夹中显示</button>}
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
    // Generated CJK PDFs embed fonts and commonly exceed the generic 1 MiB cap.
    // Native code independently enforces the PDF-only bound after authorization.
    maxBytes: kind === "pdf" ? 32 * 1024 * 1024 : 1024 * 1024,
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
