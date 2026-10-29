import { useEffect, useState } from "react";
import { codingApi } from "../lib/tauri-api";
import { Markdown } from "@/components/Markdown";
import type { CommitEvidence, ReviewRecord, RunCheckpoint } from "../lib/types";
import { withWorkspaceDiskMutation } from "../lib/workspace-editor";
import { useCodingActionScope } from "../hooks/useCodingActionScope";

export function RunHistory({
  root,
  taskId,
  revision,
  busy,
  onChanged,
  currentRevision,
  dirty = false,
  onFixReview,
  onOpenFile,
}: {
  root: string;
  taskId: string;
  revision: string;
  busy: boolean;
  onChanged: () => Promise<void>;
  currentRevision?: string;
  dirty?: boolean;
  onFixReview?: (review: ReviewRecord) => Promise<void>;
  onOpenFile?: (path: string, line?: number) => void;
}) {
  const [runs, setRuns] = useState<RunCheckpoint[]>([]);
  const [commits, setCommits] = useState<CommitEvidence[]>([]);
  const [reviews, setReviews] = useState<ReviewRecord[]>([]);
  const [error, setError] = useState("");
  const [notice,setNotice] = useState("");
  const [working, setWorking] = useState(false);
  const isCurrent = useCodingActionScope(root,taskId);
  const [reload,setReload] = useState(0);
  useEffect(()=>{setNotice("");setWorking(false);},[root,taskId]);
  useEffect(() => {
    let disposed = false;
    setRuns([]);setCommits([]);setReviews([]);setError("");
    void Promise.all([
      codingApi.checkpoints(root, taskId),
      codingApi.commitHistory(root, taskId),
      codingApi.reviews(root, taskId),
    ])
      .then(([runs, commits, reviews]) => {
        if (
          !Array.isArray(runs) ||
          !Array.isArray(commits) ||
          !Array.isArray(reviews)
        )
          throw new Error("历史响应无效，请重新打开");
        if (!disposed) {
          setRuns(runs);
          setCommits(commits);
          setReviews(reviews);
          setError("");
        }
      })
      .catch((error) => {
        if (!disposed) setError(String(error));
      });
    return () => {
      disposed = true;
    };
  }, [root, taskId, revision,reload]);
  return (
    <details className="coding-run-history">
      <summary>恢复点、提交与可选审查</summary>
      <p>
        仅恢复确认属于该轮且之后未重叠修改的文件；来源不明的外部变化保留。未保存内容会阻止磁盘恢复。
      </p>
      {runs.length === 0 && <p>暂无运行检查点</p>}
      {[...runs].reverse().map((run) => (
        <div key={run.runId}>
          <code>{run.runId.slice(0, 8)}</code> · {run.createdAt} ·{" "}
          {run.confirmedPaths.length} 个 Agent 文件 / {run.externalPaths.length}{" "}
          个外部变化
          <button
            type="button"
            disabled={
              busy ||
              working ||
              !!run.restoredAt ||
              run.confirmedPaths.length === 0
            }
            onClick={async () => {
              setWorking(true);
              try {
                const result = await withWorkspaceDiskMutation(root, () =>
                  codingApi.restoreCheckpoint(root, taskId, run.runId),
                );
                if (!isCurrent()) return;
                setNotice(
                  result.conflicts.length
                    ? `以下文件存在之后的修改，已保留：${result.conflicts.join("、")}`
                    : "",
                );
                await onChanged();
                if (isCurrent()) setReload(value=>value+1);
              } catch (error) {
                if(isCurrent()) setError(String(error));
              } finally {
                if(isCurrent()) setWorking(false);
              }
            }}
          >
            {run.restoredAt
              ? "已恢复"
              : run.finishedAt
                ? "恢复本轮"
                : "核对并恢复本轮"}
          </button>
        </div>
      ))}
      {commits.map((commit) => (
        <p key={commit.hash}>
          <code>{commit.hash.slice(0, 12)}</code> · {commit.paths.length} 个文件
          · {commit.verified ? "候选内容已验证" : "未验证"}
          {commit.hookChangedTree ? " · hook 改变了候选内容" : ""}
        </p>
      ))}
      {[...reviews].reverse().map((review, index) => (
        <details key={`${review.runId ?? review.confirmedAt}:${index}`}>
          <summary>
            {review.runtimeReview ? "Agent 代码审查" : "历史人工确认"} ·{" "}
            {review.confirmedAt} ·{" "}
            {!review.runtimeReview
              ? "不作为模型审查证据"
              : dirty ||
                  !currentRevision ||
                  review.contentRevision !== currentRevision
                ? "已过期"
                : "当前版本"}
          </summary>
          <code>revision: {review.contentRevision}</code>
          {review.summary && (
            <Markdown
              complete
              config={
                onOpenFile
                  ? {
                      pathClickHandler: {
                        onPathClick: (path, type, range) => {
                          if (type === "file") onOpenFile(path, range?.start);
                        },
                      },
                    }
                  : undefined
              }
            >
              {review.summary}
            </Markdown>
          )}
          {review.runtimeReview && review.summary && onFixReview && (
            <button
              type="button"
              disabled={busy || working}
              onClick={async () => {
                setWorking(true);
                try {
                  await onFixReview(review);
                } catch (error) {
                  if(isCurrent()) setError(String(error));
                } finally {
                  if(isCurrent()) setWorking(false);
                }
              }}
            >
              交给 Agent 核对并修复
            </button>
          )}
        </details>
      ))}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
    </details>
  );
}
