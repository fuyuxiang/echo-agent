/**
 * 文件树视图 —— 对齐 EchoAgent `context-viewer-components/DetailPanel/FileTree`。
 *
 * 左列：可展开/折叠的目录树（懒加载 `listDir`，展开时按需拉取子目录）。
 * 选中文件由父组件通过 onFileSelect 回调驱动主区域预览。
 *
 * 根目录取 cwd（会话工作区）。隐藏/构建目录已在后端过滤。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { listDir, type DirEntry } from "@/lib/agent-client";
import { formatFileSize } from "@/lib/file-utils";
import { ChevronRightIcon } from "@/foundation/components/Icon/icons";
import { InlineRenameField } from "./InlineRenameField";
import { FileTypeIcon } from "@/lib/file-type-icon";
import { useFileTreeSelectionStore } from "@/stores/file-tree-selection-store";
import { FixedSizeList, type FixedSizeListHandle } from "@/components/FixedSizeList";
import { useElementSize } from "@/lib/use-element-size";

/** 已加载的目录条目缓存：path → entries。 */
type LoadedMap = Map<string, DirEntry[]>;
type VisibleTreeRow =
  | { kind: "entry"; entry: DirEntry; depth: number }
  | { kind: "loading"; directoryPath: string; depth: number }
  | { kind: "error"; directoryPath: string; depth: number; message: string };

function normalize(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+$/, "");
}

/**
 * Refresh the closest directory already represented in the tree. If an Agent
 * creates `src/new/module.ts` before `src/new` has ever been loaded, `src` (or
 * ultimately the root) is refreshed so the new intermediate directory still
 * appears.
 */
function closestLoadedParent(root: string, changedPath: string, loaded: LoadedMap): string {
  const normalizedRoot = normalize(root);
  let relative = normalize(changedPath);
  if (relative === normalizedRoot) return root;
  if (relative.startsWith(`${normalizedRoot}/`)) {
    relative = relative.slice(normalizedRoot.length + 1);
  }
  const parts = relative.replace(/^\.?\//, "").split("/").filter(Boolean);
  if (parts.length > 0) parts.pop();
  while (parts.length > 0) {
    const candidate = `${root.replace(/[\\/]+$/, "")}/${parts.join("/")}`;
    const loadedPath = [...loaded.keys()].find((path) => normalize(path) === normalize(candidate));
    if (loadedPath) return loadedPath;
    parts.pop();
  }
  return root;
}

interface FileTreeViewProps {
  /** 工作区根目录（绝对路径）。 */
  rootPath?: string;
  /** 当前选中的文件路径（高亮）。 */
  selectedPath?: string;
  /** 代码工作台中用作新建位置的目录。 */
  selectedDirectoryPath?: string;
  /** 选中文件回调。 */
  onFileSelect: (path: string) => void;
  /** 可选的目录选中回调；不传时保持原有文件树行为。 */
  onDirectorySelect?: (path: string) => void;
  /** 错误/提示回调。 */
  onToast?: (msg: string) => void;
  /** Increment to invalidate the lazy directory cache after creating entries. */
  refreshKey?: number;
  /** Changed workspace paths associated with refreshKey, for targeted reloads. */
  refreshPaths?: string[];
  /** Increment to collapse every expanded directory. */
  collapseKey?: number;
  /** File to reveal by expanding its ancestors and scrolling it into view. */
  revealPath?: string;
  /** Increment to repeat a reveal request for the same path. */
  revealKey?: number;
  /** Expansion state restored when this tree is mounted for a workspace. */
  initialExpandedPaths?: string[];
  /** Persist expansion state in the owning workspace. */
  onExpandedPathsChange?: (paths: string[]) => void;
  /** Paths that are cut and awaiting paste; rendered with reduced opacity. */
  cutPaths?: Set<string>;
  /** Right-click on a tree node. The parent typically opens a context menu. */
  onContextMenu?: (event: React.MouseEvent, entry: DirEntry) => void;
  /** Path currently being inline-renamed; if set the matching node shows the input. */
  renamingPath?: string | null;
  /** Submit a new name for `renamingPath`. Resolves on success; rejects keep editing. */
  onRenameSubmit?: (path: string, newName: string) => Promise<void>;
  /** Cancel the inline rename (e.g. Escape, blur). */
  onRenameCancel?: () => void;
  /** SP2: include hidden dotfile entries (still honouring .gitignore / .echoagentignore). */
  includeHidden?: boolean;
  /** SP2: top-level entries above this count trigger virtualisation. */
  topLevelThreshold?: number;
  /** SP2: row height used by the virtual list, in px. */
  virtualItemHeight?: number;
  /** Case-insensitive substring filter applied to entry.name; empty = no filter. */
  filter?: string;
  /** Workspace-relative file index used to discover matches below collapsed folders. */
  indexedPaths?: string[];
  /** Invoked when the cwd-empty state "选择其他目录" action is clicked. */
  onSelectDirectory?: () => void;
  showRootInEmptyState?: boolean;
  /** Use the workspace's scroll owner so view switching restores the same position. */
  viewportRef?: RefObject<HTMLDivElement>;
}

export function FileTreeView({
  rootPath,
  selectedPath,
  selectedDirectoryPath,
  onFileSelect,
  onDirectorySelect,
  onToast,
  refreshKey = 0,
  refreshPaths = [],
  collapseKey = 0,
  revealPath,
  revealKey = 0,
  initialExpandedPaths = [],
  onExpandedPathsChange,
  cutPaths,
  onContextMenu,
  renamingPath,
  onRenameSubmit,
  onRenameCancel,
  includeHidden = false,
  topLevelThreshold = 200,
  virtualItemHeight = 26,
  filter,
  indexedPaths = [],
  onSelectDirectory,
  showRootInEmptyState = true,
  viewportRef,
}: FileTreeViewProps) {
  const [loaded, setLoaded] = useState<LoadedMap>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loadingDirs, setLoadingDirs] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Map<string, string>>(new Map());
  const loadedRef = useRef<LoadedMap>(new Map());
  const loadingRef = useRef<Set<string>>(new Set());
  const initializedRootRef = useRef<string | null>(null);
  const scopeGenerationRef = useRef(0);
  const requestGenerationRef = useRef(new Map<string, number>());
  const reportedErrorsRef = useRef(new Map<string, string>());
  const previousRefreshKeyRef = useRef(refreshKey);
  const previousCollapseKeyRef = useRef(collapseKey);
  const previousRevealKeyRef = useRef<number | null>(null);
  const onToastRef = useRef(onToast);
  const onExpandedPathsChangeRef = useRef(onExpandedPathsChange);

  const root = rootPath ?? "";
  const rootLoaded = loaded.has(root);

  useEffect(() => {
    onToastRef.current = onToast;
    onExpandedPathsChangeRef.current = onExpandedPathsChange;
  }, [onExpandedPathsChange, onToast]);

  const updateLoaded = useCallback((updater: (current: LoadedMap) => LoadedMap) => {
    setLoaded((current) => {
      const next = updater(current);
      loadedRef.current = next;
      return next;
    });
  }, []);

  const updateLoading = useCallback((updater: (current: Set<string>) => Set<string>) => {
    setLoadingDirs((current) => {
      const next = updater(current);
      loadingRef.current = next;
      return next;
    });
  }, []);

  // A force refresh deliberately bypasses the cache but keeps its last good
  // value visible. Per-directory generations make "latest response wins"
  // deterministic when several saves arrive close together.
  const loadDir = useCallback(
    async (dirPath: string, force = false) => {
      if (!force && (loadedRef.current.has(dirPath) || loadingRef.current.has(dirPath))) return;
      const scopeGeneration = scopeGenerationRef.current;
      const requestGeneration = (requestGenerationRef.current.get(dirPath) ?? 0) + 1;
      requestGenerationRef.current.set(dirPath, requestGeneration);
      updateLoading((current) => new Set(current).add(dirPath));
      setErrors((current) => {
        if (!current.has(dirPath)) return current;
        const next = new Map(current);
        next.delete(dirPath);
        return next;
      });
      try {
        const entries = await listDir(dirPath, root, undefined, includeHidden);
        if (
          scopeGeneration !== scopeGenerationRef.current
          || requestGenerationRef.current.get(dirPath) !== requestGeneration
        ) return;
        updateLoaded((current) => {
          const next = new Map(current);
          next.set(dirPath, entries);
          return next;
        });
        reportedErrorsRef.current.delete(dirPath);
      } catch (e) {
        if (
          scopeGeneration !== scopeGenerationRef.current
          || requestGenerationRef.current.get(dirPath) !== requestGeneration
        ) return;
        const msg = String(e).replace(/^Error:\s*/, "");
        setErrors((current) => {
          if (current.get(dirPath) === msg) return current;
          const next = new Map(current);
          next.set(dirPath, msg);
          return next;
        });
        if (reportedErrorsRef.current.get(dirPath) !== msg) {
          reportedErrorsRef.current.set(dirPath, msg);
          onToastRef.current?.(`读取目录失败：${msg}`);
        }
      } finally {
        if (
          scopeGeneration === scopeGenerationRef.current
          && requestGenerationRef.current.get(dirPath) === requestGeneration
        ) {
          updateLoading((current) => {
            const next = new Set(current);
            next.delete(dirPath);
            return next;
          });
        }
      }
    },
    [includeHidden, root, updateLoaded, updateLoading],
  );

  // A workspace switch is the only operation that clears the visible tree.
  // Late responses from the old workspace are invalidated before state resets.
  useEffect(() => {
    if (initializedRootRef.current === root) return;
    initializedRootRef.current = root;
    scopeGenerationRef.current += 1;
    requestGenerationRef.current.clear();
    reportedErrorsRef.current.clear();
    loadedRef.current = new Map();
    loadingRef.current = new Set();
    setLoaded(loadedRef.current);
    setLoadingDirs(loadingRef.current);
    const restoredExpanded = new Set(initialExpandedPaths);
    setExpanded(restoredExpanded);
    onExpandedPathsChangeRef.current?.([...restoredExpanded]);
    setErrors(new Map());
    previousRefreshKeyRef.current = refreshKey;
    if (!root) return;
    void (async () => {
      await loadDir(root, true);
      await Promise.all(
        initialExpandedPaths
          .filter((path) => normalize(path).startsWith(`${normalize(root)}/`))
          .map((path) => loadDir(path)),
      );
    })();
  }, [loadDir, root]); // refreshKey/initial expansion are snapshotted for the new root.

  // Toggling hidden files refreshes every loaded directory in place. Keeping
  // the cache visible avoids collapsing the tree or flashing a loading state.
  const previousIncludeHiddenRef = useRef(includeHidden);
  useEffect(() => {
    if (!root) return;
    if (previousIncludeHiddenRef.current === includeHidden) return;
    previousIncludeHiddenRef.current = includeHidden;
    const directories = [...loadedRef.current.keys()];
    if (directories.length === 0) directories.push(root);
    for (const directory of directories) void loadDir(directory, true);
  }, [includeHidden, loadDir, root]);

  // File writes refresh in the background. Existing entries and expansion
  // state stay mounted, so code generation never flashes an empty explorer.
  useEffect(() => {
    if (!root || previousRefreshKeyRef.current === refreshKey) return;
    previousRefreshKeyRef.current = refreshKey;
    const current = loadedRef.current;
    const targets = refreshPaths.length > 0
      ? new Set(refreshPaths.map((path) => closestLoadedParent(root, path, current)))
      : new Set(current.keys());
    if (targets.size === 0) targets.add(root);
    for (const dirPath of targets) void loadDir(dirPath, true);
  }, [loadDir, refreshKey, refreshPaths, root]);

  useEffect(() => {
    if (previousCollapseKeyRef.current === collapseKey) return;
    previousCollapseKeyRef.current = collapseKey;
    setExpanded(new Set());
    onExpandedPathsChangeRef.current?.([]);
  }, [collapseKey]);

  useEffect(() => {
    if (!root || !revealPath || previousRevealKeyRef.current === revealKey) return;
    previousRevealKeyRef.current = revealKey;
    const normalizedRoot = normalize(root);
    const normalizedTarget = normalize(revealPath);
    if (!normalizedTarget.startsWith(`${normalizedRoot}/`)) return;

    const segments = normalizedTarget.slice(normalizedRoot.length + 1).split("/").filter(Boolean);
    segments.pop();
    const parents: string[] = [];
    let current = root.replace(/[\\/]+$/, "");
    for (const segment of segments) {
      current = `${current}/${segment}`;
      parents.push(current);
    }

    let cancelled = false;
    void (async () => {
      for (const parent of parents) {
        await loadDir(parent);
        if (cancelled) return;
      }
      setExpanded((value) => {
        const next = new Set(value);
        for (const parent of parents) next.add(parent);
        onExpandedPathsChangeRef.current?.([...next]);
        return next;
      });
    })();
    return () => {
      cancelled = true;
    };
  }, [loadDir, revealKey, revealPath, root]);

  const toggleDir = useCallback(
    async (dirPath: string) => {
      const opening = !expanded.has(dirPath);
      setExpanded((prev) => {
        const next = new Set(prev);
        if (next.has(dirPath)) next.delete(dirPath);
        else next.add(dirPath);
        onExpandedPathsChangeRef.current?.([...next]);
        return next;
      });
      if (opening) await loadDir(dirPath);
    },
    [expanded, loadDir],
  );

  // Flatten visible tree for shift-click range selection. Must be defined
  // BEFORE any conditional return so the hook order is stable across renders.
  const rootEntries = root ? loaded.get(root) ?? [] : [];
  const filterLower = (filter ?? "").trim().toLowerCase();

  // The workspace index also contains files below collapsed folders. Load the
  // ancestors of real matches so filtering is useful without first expanding
  // every directory manually. Broad queries are capped to protect large repos.
  useEffect(() => {
    if (!root || !filterLower || indexedPaths.length === 0) return;
    let cancelled = false;
    const matches = indexedPaths
      .filter((path) => path
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.toLowerCase()
        .includes(filterLower))
      .slice(0, 200);
    void (async () => {
      for (const relativePath of matches) {
        const parts = relativePath.replace(/\\/g, "/").split("/").filter(Boolean);
        parts.pop();
        let parent = root.replace(/[\\/]+$/, "");
        for (const part of parts) {
          parent = `${parent}/${part}`;
          await loadDir(parent);
          if (cancelled) return;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [filterLower, indexedPaths, loadDir, root]);

  const visibleRows = useMemo(() => {
    const out: VisibleTreeRow[] = [];
    const walk = (entries: DirEntry[] | undefined, depth: number) => {
      if (!entries) return;
      for (const entry of entries) {
        out.push({ kind: "entry", entry, depth });
        if (entry.kind === "directory" && expanded.has(entry.path)) {
          const children = loaded.get(entry.path);
          if (children) {
            walk(children, depth + 1);
          } else if (loadingDirs.has(entry.path)) {
            out.push({ kind: "loading", directoryPath: entry.path, depth: depth + 1 });
          } else {
            const message = errors.get(entry.path);
            if (message) {
              out.push({ kind: "error", directoryPath: entry.path, depth: depth + 1, message });
            }
          }
        }
      }
    };
    walk(rootEntries, 0);
    return out;
  }, [rootEntries, expanded, loaded, loadingDirs, errors]);
  const filteredRows = useMemo(() => {
    if (!filterLower) return visibleRows;
    const walk = (entries: DirEntry[] | undefined, depth: number): VisibleTreeRow[] => {
      if (!entries) return [];
      const result: VisibleTreeRow[] = [];
      for (const entry of entries) {
        const ownMatch = entry.name.toLowerCase().includes(filterLower);
        if (entry.kind !== "directory") {
          if (ownMatch) result.push({ kind: "entry", entry, depth });
          continue;
        }
        const childRows = walk(loaded.get(entry.path), depth + 1);
        if (ownMatch || childRows.length > 0) {
          result.push({ kind: "entry", entry, depth }, ...childRows);
        }
      }
      return result;
    };
    return walk(rootEntries, 0);
  }, [filterLower, loaded, rootEntries, visibleRows]);
  const visiblePaths = useMemo(
    () => filteredRows.flatMap((row) => row.kind === "entry" ? [row.entry.path] : []),
    [filteredRows],
  );
  const renderedExpanded = useMemo(() => {
    if (!filterLower) return expanded;
    const next = new Set(expanded);
    filteredRows.forEach((row, index) => {
      const following = filteredRows[index + 1];
      if (
        row.kind === "entry"
        && row.entry.kind === "directory"
        && following
        && following.depth > row.depth
      ) {
        next.add(row.entry.path);
      }
    });
    return next;
  }, [expanded, filterLower, filteredRows]);

  // SP2: virtualisation + container size hooks must run before any early
  // return so the hook order is stable across renders.
  const treeRef = useRef<HTMLDivElement | null>(null);
  const listHandleRef = useRef<FixedSizeListHandle | null>(null);
  const pendingFocusFrame = useRef<number>();
  const [scrollTop, setScrollTop] = useState(0);
  const [focusedPath, setFocusedPath] = useState<string>();
  const shouldVirtualize = filteredRows.length >= topLevelThreshold;
  const containerHeight = useElementSize(viewportRef ?? treeRef, "height", shouldVirtualize && rootLoaded && rootEntries.length > 0);
  const virtualized = shouldVirtualize && (Boolean(containerHeight) || Boolean(viewportRef && typeof ResizeObserver !== "undefined"));
  const tabStopPath = useMemo(() => {
    const preferred = [focusedPath, selectedPath, selectedDirectoryPath];
    const choose = (paths: string[]) => preferred.find(path => path !== undefined && paths.includes(path)) ?? paths[0];
    if (!virtualized) return choose(visiblePaths);
    if (!containerHeight || virtualItemHeight <= 0) return undefined;
    const offset = Math.max(0, Math.min(scrollTop, filteredRows.length * virtualItemHeight - containerHeight));
    const start = Math.floor(offset / virtualItemHeight);
    const end = Math.ceil((offset + containerHeight) / virtualItemHeight);
    const visibleEntries = filteredRows.slice(start, end).flatMap(row => row.kind === "entry" ? [row.entry.path] : []);
    return choose(visibleEntries);
  }, [containerHeight, filteredRows, focusedPath, scrollTop, selectedDirectoryPath, selectedPath, virtualItemHeight, virtualized, visiblePaths]);

  const focusPath = useCallback((path: string | undefined) => {
    if (!path) return;
    const findNode = () => Array.from(treeRef.current?.querySelectorAll<HTMLElement>("[data-tree-path]") ?? [])
      .find(node => normalize(node.dataset.treePath ?? "") === normalize(path));
    const focus = (node: HTMLElement) => {
      node.scrollIntoView?.({ block: "nearest" });
      node.focus({ preventScroll: true });
    };
    if (pendingFocusFrame.current !== undefined) window.cancelAnimationFrame(pendingFocusFrame.current);
    const node = findNode();
    if (node) {
      focus(node);
      return;
    }
    const index = filteredRows.findIndex(row => row.kind === "entry" && normalize(row.entry.path) === normalize(path));
    if (index < 0 || !listHandleRef.current) return;
    listHandleRef.current.scrollToIndex(index);
    pendingFocusFrame.current = window.requestAnimationFrame(() => {
      pendingFocusFrame.current = undefined;
      const rendered = findNode();
      if (rendered) focus(rendered);
    });
  }, [filteredRows]);

  useEffect(() => () => {
    if (pendingFocusFrame.current !== undefined) window.cancelAnimationFrame(pendingFocusFrame.current);
  }, [root]);

  useEffect(() => {
    if (revealPath && previousRevealKeyRef.current === revealKey) focusPath(revealPath);
  }, [revealKey, revealPath, focusPath, virtualized]);

  if (!root) {
    return (
      <div className="file-tree__empty">尚未设置工作目录。</div>
    );
  }

  if (!rootLoaded && errors.has(root)) {
    return (
      <div className="file-tree__empty file-tree__error" role="alert">
        <span>无法读取工作区文件。</span>
        <button type="button" onClick={() => void loadDir(root, true)}>重试</button>
      </div>
    );
  }

  if (!rootLoaded) {
    return <div className="file-tree__empty">加载文件树中…</div>;
  }

  if (rootEntries.length === 0) {
    return (
      <div className="file-tree__empty file-tree__empty--cwd-hint">
        <span>{showRootInEmptyState ? <>当前工作区 <code>{root}</code> 没有可见文件。</> : "当前目录暂无可见文件。"}</span>
        <div className="file-tree__empty-actions">
          {onSelectDirectory && <button type="button" onClick={onSelectDirectory}>
            选择其他目录
          </button>}
        </div>
      </div>
    );
  }

  const renderTreeNode = (entry: DirEntry, depth: number) => {
    return (
      <TreeNode
        key={entry.path}
        entry={entry}
        depth={depth}
        tabbable={entry.path === tabStopPath}
        expanded={renderedExpanded}
        selectedPath={selectedPath}
        selectedDirectoryPath={selectedDirectoryPath}
        cutPaths={cutPaths}
        renamingPath={renamingPath ?? null}
        visiblePaths={visiblePaths}
        onContextMenu={onContextMenu}
        onRenameSubmit={onRenameSubmit}
        onRenameCancel={onRenameCancel}
        onToggleDir={toggleDir}
        onFocusPath={focusPath}
        onTakeFocus={setFocusedPath}
        onFileSelect={onFileSelect}
        onDirectorySelect={onDirectorySelect}
      />
    );
  };

  return (
    <div
      ref={treeRef}
      className={"file-tree" + (shouldVirtualize ? " file-tree--virtual" : "")}
      role="tree"
      aria-label="工作区文件树"
      aria-busy={loadingDirs.size > 0 || undefined}
      tabIndex={!tabStopPath && visiblePaths.length ? 0 : undefined}
      onKeyDown={event => {
        if (event.target !== event.currentTarget || !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        const path = event.key === "End" || event.key === "ArrowUp" ? visiblePaths[visiblePaths.length - 1] : visiblePaths[0];
        focusPath(path);
      }}
    >
      {virtualized ? (
        <FixedSizeList
          ref={listHandleRef}
          items={filteredRows}
          itemHeight={virtualItemHeight}
          height={containerHeight ?? 0}
          viewportRef={viewportRef}
          onScroll={setScrollTop}
          renderItem={(row) => {
            if (row.kind === "entry") return renderTreeNode(row.entry, row.depth);
            if (row.kind === "loading") {
              return (
                <div
                  className="file-tree__node file-tree__node--loading"
                  style={{ paddingInlineStart: `${row.depth * 14 + 8}px` }}
                >
                  …
                </div>
              );
            }
            return (
              <button
                type="button"
                className="file-tree__retry"
                title={row.message}
                style={{ marginInlineStart: `${row.depth * 14 + 8}px` }}
                onClick={() => void loadDir(row.directoryPath, true)}
              >
                加载失败，重试
              </button>
            );
          }}
          getKey={(row) => row.kind === "entry"
            ? row.entry.path
            : `${row.kind}:${row.directoryPath}`}
          role="presentation"
        />
      ) : filteredRows.length > 0 ? (
        filteredRows.map((row) => {
          if (row.kind === "entry") return renderTreeNode(row.entry, row.depth);
          if (row.kind === "loading") {
            return (
              <div
                key={`loading:${row.directoryPath}`}
                className="file-tree__node file-tree__node--loading"
              >
                …
              </div>
            );
          }
          return (
            <button
              key={`error:${row.directoryPath}`}
              type="button"
              className="file-tree__retry"
              onClick={() => void loadDir(row.directoryPath, true)}
            >
              读取失败，点击重试
            </button>
          );
        })
      ) : (
        <div className="file-tree__empty">未找到匹配“{filter}”的文件</div>
      )}
    </div>
  );
}

/** 文件或目录在可见树中的一行。 */
function TreeNode({
  entry,
  depth,
  tabbable,
  expanded,
  selectedPath,
  selectedDirectoryPath,
  cutPaths,
  renamingPath,
  visiblePaths,
  onContextMenu,
  onRenameSubmit,
  onRenameCancel,
  onToggleDir,
  onFocusPath,
  onTakeFocus,
  onFileSelect,
  onDirectorySelect,
}: {
  entry: DirEntry;
  depth: number;
  tabbable: boolean;
  expanded: Set<string>;
  selectedPath?: string;
  selectedDirectoryPath?: string;
  cutPaths?: Set<string>;
  renamingPath: string | null;
  visiblePaths: string[];
  onContextMenu?: (event: React.MouseEvent, entry: DirEntry) => void;
  onRenameSubmit?: (path: string, newName: string) => Promise<void>;
  onRenameCancel?: () => void;
  onToggleDir: (path: string) => void;
  onFocusPath: (path: string | undefined) => void;
  onTakeFocus: (path: string) => void;
  onFileSelect: (path: string) => void;
  onDirectorySelect?: (path: string) => void;
}) {
  const isDir = entry.kind === "directory";
  const isExpanded = expanded.has(entry.path);
  const isSingleSelected =
    entry.path === selectedPath || (isDir && entry.path === selectedDirectoryPath);
  const isMultiSelected = useFileTreeSelectionStore(
    (s) => s.selectedPaths.has(entry.path) || isSingleSelected,
  );
  const isCut = cutPaths?.has(entry.path) ?? false;
  const isRenaming = renamingPath === entry.path;
  const isSymlink = entry.kind === "symlink";

  const handleClick = (event: React.MouseEvent) => {
    if (isSymlink) return;
    const sel = useFileTreeSelectionStore.getState();
    if (event.shiftKey) {
      if (sel.anchorPath && visiblePaths.length > 0) {
        sel.rangeSelect(visiblePaths, sel.anchorPath, entry.path);
      } else {
        sel.select([entry.path], entry.path);
      }
    } else if (event.metaKey || event.ctrlKey) {
      sel.toggle(entry.path);
    } else {
      sel.select([entry.path], entry.path);
    }
    if (isDir) {
      onDirectorySelect?.(entry.path);
      onToggleDir(entry.path);
    } else {
      onFileSelect(entry.path);
    }
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const index = visiblePaths.indexOf(entry.path);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const nextIndex = event.key === "ArrowDown"
        ? Math.min(visiblePaths.length - 1, index + 1)
        : Math.max(0, index - 1);
      const path = visiblePaths[nextIndex];
      if (path) useFileTreeSelectionStore.getState().select([path], path);
      onFocusPath(path);
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      const path = event.key === "Home" ? visiblePaths[0] : visiblePaths[visiblePaths.length - 1];
      if (path) useFileTreeSelectionStore.getState().select([path], path);
      onFocusPath(path);
      return;
    }
    if (event.key === "ArrowRight" && isDir) {
      event.preventDefault();
      if (!isExpanded) onToggleDir(entry.path);
      else onFocusPath(visiblePaths[index + 1]);
      return;
    }
    if (event.key === "ArrowLeft" && isDir && isExpanded) {
      event.preventDefault();
      onToggleDir(entry.path);
      return;
    }
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      const parent = normalize(entry.path).split("/").slice(0, -1).join("/");
      onFocusPath(visiblePaths.find((path) => normalize(path) === parent));
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      handleClick(event as unknown as React.MouseEvent);
    }
  };

  const handleContextMenu = (event: React.MouseEvent) => {
    if (!onContextMenu || isSymlink) return;
    event.preventDefault();
    onContextMenu(event, entry);
  };

  // SP5: large / binary / symlink classification. Done here so the badges
  // and the dimmed title both see a single source of truth.
  const tip = (() => {
    if (isSymlink) return `符号链接：${entry.path}`;
    if (entry.isBinary) return `二进制文件（${entry.size} 字节），双击将无法打开`;
    if (entry.isLarge) return `大文件：${formatFileSize(entry.size)}，超过 2 MB`;
    return entry.path;
  })();

  const classes = [
    "file-tree__node",
    isDir ? "file-tree__node--dir" : "file-tree__node--file",
    isSymlink ? "file-tree__node--disabled" : "",
    isMultiSelected ? "file-tree__node--selected" : "",
    isCut ? "file-tree__node--cut" : "",
    isRenaming ? "file-tree__node--renaming" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <>
      <div
        className={classes}
        style={{ paddingInlineStart: `${depth * 14 + 8}px` }}
        role="treeitem"
        aria-level={depth + 1}
        tabIndex={tabbable ? 0 : -1}
        aria-expanded={isDir ? isExpanded : undefined}
        aria-selected={isMultiSelected}
        data-tree-path={entry.path}
        data-cut={isCut || undefined}
        draggable={!isSymlink}
        aria-disabled={isSymlink || undefined}
        onClick={handleClick}
        onFocus={() => onTakeFocus(entry.path)}
        onKeyDown={handleKeyDown}
        onContextMenu={handleContextMenu}
        onDragStart={(event) => {
          if (isSymlink) {
            event.preventDefault();
            return;
          }
          // SP4: file tree → Composer / AgentPane drag bridge. Carry the
          // (possibly multi-select) paths in our own MIME so receivers can
          // round-trip them without touching Tauri events.
          const sel = useFileTreeSelectionStore.getState().selectedPaths;
          const paths = sel.has(entry.path) ? [...sel] : [entry.path];
          event.dataTransfer.setData(
            "application/x-echo-paths",
            JSON.stringify(paths),
          );
          // Fallback for components that only inspect text/plain.
          event.dataTransfer.setData("text/plain", paths.join("\n"));
          event.dataTransfer.effectAllowed = "copy";
        }}
        title={tip}
      >
        {isDir ? (
          <ChevronRightIcon
            size="sm"
            className={
              "file-tree__chevron" + (isExpanded ? " file-tree__chevron--open" : "")
            }
          />
        ) : (
          <span className="file-tree__chevron-placeholder" />
        )}
        <span className="file-tree__icon file-tree__icon--svg">
          <FileTypeIcon name={entry.name} kind={isDir ? "directory" : "file"} size={14} />
        </span>
        {isRenaming && onRenameSubmit && onRenameCancel ? (
          <InlineRenameField
            initialName={entry.name}
            onSubmit={(name) => onRenameSubmit(entry.path, name)}
            onCancel={onRenameCancel}
          />
        ) : (
          <>
            <span className="file-tree__name">{entry.name}</span>
            {/* SP5: large / binary / symlink badges, in priority order */}
            {!isSymlink && entry.isLarge ? (
              <span
                className="file-tree__large-badge"
                title={`大文件：${formatFileSize(entry.size)}`}
                data-testid="file-tree-large-badge"
              >
                L
              </span>
            ) : null}
            {!isSymlink && entry.isBinary ? (
              <span
                className="file-tree__binary-badge"
                title="二进制文件，AI 评审会跳过"
                data-testid="file-tree-binary-badge"
              >
                B
              </span>
            ) : null}
            {isSymlink ? (
              <span
                className="file-tree__symlink-badge"
                title="符号链接"
                data-testid="file-tree-symlink-badge"
              >
                ↪
              </span>
            ) : null}
          </>
        )}
      </div>
    </>
  );
}
