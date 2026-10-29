import { useEffect, useState } from "react";
import {
  Check,
  ChevronDown,
  ChevronRight,
  FolderGit2,
  GitCommitHorizontal,
  LoaderCircle,
  Merge,
  RotateCcw,
  Trash2,
} from "lucide-react";

import type { ChangeSet, CommitHunk, FileChange } from "../lib/types";
import { useCodingActionScope } from "../hooks/useCodingActionScope";

interface ChangeSetViewProps {
  changeSet: ChangeSet | null;
  hasTask: boolean;
  busyPath?: string | null;
  committing?: boolean;
  canCommit?: boolean;
  canRollback?: boolean;
  canDiscard?: boolean;
  isolatedSource?: string | null;
  integratedHash?: string | null;
  integrating?: boolean;
  onOpenDiff: (change: FileChange) => void;
  onDiscard: (change: FileChange) => void;
  onCommit: (paths: string[], hunks: Record<string, string[]>) => void;
  onLoadHunks?: (path: string) => Promise<CommitHunk[]>;
  onDiscardHunk?: (path: string, hunkId: string) => void;
  onRollback: () => void;
  onIntegrate?: () => void;
}

const STATUS_LETTER: Record<FileChange["kind"], string> = {
  added: "A",
  modified: "M",
  deleted: "D",
  renamed: "R",
};

/**
 * The task's change set, which is the unit the workbench commits and rolls back.
 *
 * Files already dirty at task start carry a visible marker. Their exact starting
 * bytes can be restored, but automatic commit stays disabled to avoid sweeping
 * unrelated personal changes into the task commit.
 */
export function ChangeSetView({
  changeSet,
  hasTask,
  busyPath,
  committing = false,
  canCommit = false,
  canRollback = true,
  canDiscard = true,
  isolatedSource,
  integratedHash,
  integrating = false,
  onOpenDiff,
  onDiscard,
  onCommit,
  onLoadHunks,
  onDiscardHunk,
  onRollback,
  onIntegrate,
}: ChangeSetViewProps) {
  const availablePaths = (changeSet?.changes ?? [])
    .filter((change) => !change.preExisting)
    .map((change) => change.path);
  const [excludedPaths, setExcludedPaths] = useState<string[]>([]);
  const [expandedPath, setExpandedPath] = useState<string | null>(null);
  const [hunksByPath, setHunksByPath] = useState<Record<string, CommitHunk[]>>(
    {},
  );
  const [selectedHunks, setSelectedHunks] = useState<Record<string, string[]>>(
    {},
  );
  const [hunkError, setHunkError] = useState<string | null>(null);
  const [loadingHunks, setLoadingHunks] = useState(false);
  const [selectionTaskId, setSelectionTaskId] = useState<string | undefined>();
  const isCurrent = useCodingActionScope("", changeSet?.taskId);
  const availableKey = availablePaths.join("\0");
  const selectedPaths = availablePaths.filter(
    (path) => !excludedPaths.includes(path),
  );
  useEffect(() => {
    if (selectionTaskId !== changeSet?.taskId) {
      setSelectionTaskId(changeSet?.taskId);
      setExcludedPaths([]);
      setExpandedPath(null);
      setHunksByPath({});
      setSelectedHunks({});
      setLoadingHunks(false);
      setHunkError(null);
      return;
    }
    const currentPaths = availableKey ? availableKey.split("\0") : [];
    setExcludedPaths((current) =>
      current.filter((path) => currentPaths.includes(path)),
    );
  }, [changeSet?.taskId, availableKey, selectionTaskId]);

  if (!hasTask) {
    return (
      <div className="coding-explorer-view coding-explorer-view--empty">
        <FolderGit2 size={22} />
        <p>新建开发任务后，Agent 与你的改动会集中显示在这里。</p>
      </div>
    );
  }

  const changes = changeSet?.changes ?? [];
  const reviewed = new Set(changeSet?.reviewedFiles ?? []);
  const totalAdded = changes.reduce((sum, change) => sum + change.added, 0);
  const totalRemoved = changes.reduce((sum, change) => sum + change.removed, 0);
  const unreviewed = changes.filter(
    (change) => !reviewed.has(change.path),
  ).length;
  const hasProtectedChanges = changes.some((change) => change.preExisting);
  const selectedHunkPaths = Object.entries(selectedHunks)
    .filter(([, ids]) => ids.length > 0)
    .map(([path]) => path);
  const selectedFileCount = selectedPaths.length + selectedHunkPaths.length;
  const toggleHunks = async (path: string) => {
    if (expandedPath === path) {
      setExpandedPath(null);
      return;
    }
    setExpandedPath(path);
    setHunkError(null);
    if (!onLoadHunks) return;
    setLoadingHunks(true);
    try {
      const hunks = await onLoadHunks(path);
      if (!isCurrent()) return;
      setHunksByPath((current) => ({ ...current, [path]: hunks }));
      setSelectedHunks((current) => ({
        ...current,
        [path]: (current[path] ?? []).filter((id) =>
          hunks.some((hunk) => hunk.id === id),
        ),
      }));
    } catch (error) {
      if (!isCurrent()) return;
      setHunkError(String(error).replace(/^Error:\s*/, ""));
    } finally {
      if (isCurrent()) setLoadingHunks(false);
    }
  };
  const filesystemCheckpoint = changeSet?.baselineMode === "filesystem";
  const unsafeFiles = new Set(changeSet?.rollbackUnsafeFiles ?? []);
  const hasUnsafeChanges = unsafeFiles.size > 0;

  return (
    <div className="coding-explorer-view">
      <div className="coding-changeset__summary">
        <span>
          <b>{changes.length}</b> 个文件
        </span>
        <span className="is-added">+{totalAdded}</span>
        <span className="is-removed">-{totalRemoved}</span>
        <span>
          已审阅 {changes.length - unreviewed}/{changes.length}
        </span>
        <span
          title={
            filesystemCheckpoint
              ? "无需 Git；由 EchoAgent 保存任务开始时的文件检查点"
              : "使用 Git HEAD 与任务开始时的未提交内容建立基线"
          }
        >
          {filesystemCheckpoint ? "本地检查点" : "Git 基线"}
        </span>
      </div>

      <div className="coding-changeset__actions">
        <button
          type="button"
          disabled={
            filesystemCheckpoint ||
            selectedFileCount === 0 ||
            committing ||
            !canCommit
          }
          onClick={() =>
            onCommit(
              selectedPaths,
              Object.fromEntries(
                Object.entries(selectedHunks).filter(
                  ([, ids]) => ids.length > 0,
                ),
              ),
            )
          }
          title={
            filesystemCheckpoint
              ? "当前任务使用本地检查点；可手工提交，或初始化 Git 并新建任务后使用应用内提交"
              : !canCommit
                ? "任务完成后才能提交"
                : undefined
          }
        >
          {committing ? (
            <LoaderCircle size={12} className="is-spinning" />
          ) : (
            <GitCommitHorizontal size={12} />
          )}
          提交选中 {selectedFileCount} 个文件
        </button>
        {isolatedSource && (
          <button
            type="button"
            disabled={
              !changeSet?.committedHash ||
              Boolean(integratedHash) ||
              integrating
            }
            onClick={onIntegrate}
            title={
              integratedHash
                ? "已应用到原项目"
                : `将已提交任务应用到 ${isolatedSource}`
            }
          >
            {integrating ? (
              <LoaderCircle size={12} className="is-spinning" />
            ) : (
              <Merge size={12} />
            )}
            {integratedHash ? "已应用" : "应用到原项目"}
          </button>
        )}
        <button
          type="button"
          disabled={changes.length === 0 || !canRollback || hasUnsafeChanges}
          onClick={onRollback}
          title={
            hasUnsafeChanges ? "有文件超出安全快照范围，请手动检查" : undefined
          }
        >
          <RotateCcw size={12} />
          回滚任务
        </button>
      </div>

      {isolatedSource && (
        <div className="coding-explorer__note">
          隔离任务 · 原项目：{isolatedSource}
        </div>
      )}

      {changeSet?.committedHash && (
        <div className="coding-row">
          最近提交：{changeSet.committedHash.slice(0, 12)} · 剩余{" "}
          {changes.length} 个变更
        </div>
      )}

      {changes.length === 0 && (
        <div className="coding-row">本任务尚未产生代码变更</div>
      )}

      <div className="coding-changeset__list">
        {changes.map((change) => (
          <div key={change.path} className="coding-changeset__entry">
            <div className="coding-changeset__row">
              <input
                type="checkbox"
                aria-label={`选择提交 ${change.path}`}
                title={
                  change.preExisting
                    ? "此文件包含任务开始前的改动，不能自动提交"
                    : "选择提交此文件"
                }
                checked={selectedPaths.includes(change.path)}
                disabled={Boolean(change.preExisting || filesystemCheckpoint)}
                onChange={(event) => {
                  const checked = event.target.checked;
                  setExcludedPaths((current) =>
                    checked
                      ? current.filter((path) => path !== change.path)
                      : [...new Set([...current, change.path])],
                  );
                  if (checked)
                    setSelectedHunks((current) => ({
                      ...current,
                      [change.path]: [],
                    }));
                }}
              />
              <button
                type="button"
                onClick={() => onOpenDiff(change)}
                title={change.path}
              >
                <em className={`is-${change.kind}`}>
                  {STATUS_LETTER[change.kind]}
                </em>
                <span>{change.path.split("/").pop()}</span>
                <small>{change.path}</small>
                {change.preExisting && (
                  <small title="任务开始时此文件已有未提交内容">
                    起始时已修改
                  </small>
                )}
                {reviewed.has(change.path) && (
                  <Check size={11} aria-label="已审阅" />
                )}
                <b>
                  <span className="is-added">+{change.added}</span>
                  <span className="is-removed">-{change.removed}</span>
                </b>
              </button>
              <button
                type="button"
                className="coding-changeset__discard"
                disabled={
                  !canDiscard ||
                  busyPath === change.path ||
                  unsafeFiles.has(change.path)
                }
                onClick={() => onDiscard(change)}
                aria-label={`丢弃 ${change.path} 的改动`}
                title={
                  unsafeFiles.has(change.path)
                    ? "此文件没有可安全恢复的完整快照"
                    : "丢弃此文件的改动"
                }
              >
                {busyPath === change.path ? (
                  <LoaderCircle size={11} className="is-spinning" />
                ) : (
                  <Trash2 size={11} />
                )}
              </button>
              {!filesystemCheckpoint && onLoadHunks && (
                <button
                  type="button"
                  className="coding-changeset__hunk-toggle"
                  onClick={() => void toggleHunks(change.path)}
                  aria-label={`选择 ${change.path} 的任务差异块`}
                  aria-expanded={expandedPath === change.path}
                  title="仅选择本任务新增的差异块"
                >
                  {expandedPath === change.path ? (
                    <ChevronDown size={12} />
                  ) : (
                    <ChevronRight size={12} />
                  )}
                </button>
              )}
            </div>
            {expandedPath === change.path && (
              <div className="coding-changeset__hunks">
                {loadingHunks && <div>正在读取差异块…</div>}
                {hunkError && <div role="alert">{hunkError}</div>}
                {change.kind === "deleted" && onDiscardHunk && (
                  <div className="coding-explorer__note">
                    已删除文件请按文件恢复，以保留原有权限和可执行位；仍可选择差异块提交。
                  </div>
                )}
                {!loadingHunks &&
                  !hunkError &&
                  (hunksByPath[change.path] ?? []).length === 0 && (
                    <div>没有可单独提交的文本差异块</div>
                  )}
                {!loadingHunks &&
                  !hunkError &&
                  (hunksByPath[change.path] ?? []).map((hunk, index) => (
                    <label key={hunk.id} className="coding-changeset__hunk">
                      <span>
                        <input
                          type="checkbox"
                          aria-label={`任务差异块 ${index + 1}`}
                          checked={(selectedHunks[change.path] ?? []).includes(
                            hunk.id,
                          )}
                          disabled={committing}
                          onChange={(event) => {
                            if (event.target.checked)
                              setExcludedPaths((current) => [
                                ...new Set([...current, change.path]),
                              ]);
                            setSelectedHunks((current) => {
                              const ids = current[change.path] ?? [];
                              return {
                                ...current,
                                [change.path]: event.target.checked
                                  ? [...ids, hunk.id]
                                  : ids.filter((id) => id !== hunk.id),
                              };
                            });
                          }}
                        />
                        任务差异块 {index + 1}
                      </span>
                      <pre>{hunk.preview}</pre>
                      {onDiscardHunk && (
                        <button
                          type="button"
                          disabled={
                            !canDiscard ||
                            change.kind === "deleted" ||
                            busyPath === change.path ||
                            committing
                          }
                          aria-label={`丢弃 ${change.path} 的差异块 ${index + 1}`}
                          title={change.kind === "deleted" ? "请按文件恢复以保留原有权限和可执行位" : undefined}
                          onClick={() => onDiscardHunk(change.path, hunk.id)}
                        >
                          撤销此块
                        </button>
                      )}
                    </label>
                  ))}
              </div>
            )}
          </div>
        ))}
      </div>

      {hasProtectedChanges && (
        <div className="coding-explorer__note">
          带“起始时已修改”标记的文件不会整文件提交。展开文件并勾选本任务差异块；与原项目内容冲突的差异块会被拒绝，不会带入旧改动。
        </div>
      )}

      {hasUnsafeChanges && (
        <div className="coding-explorer__note">
          {changeSet?.rollbackUnsafeFiles?.join("、")}{" "}
          超出本地快照的安全恢复范围，已停用自动丢弃和整体回滚，避免覆盖原文件。
        </div>
      )}

      {filesystemCheckpoint && (
        <div className="coding-explorer__note">
          本地检查点覆盖源码与配置文件；依赖缓存、构建产物和版本库元数据不纳入自动回滚。
        </div>
      )}
    </div>
  );
}
