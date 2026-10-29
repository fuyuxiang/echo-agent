import { useEffect, useState } from "react";
import { ClipboardCopy, LoaderCircle } from "lucide-react";
import { codingApi } from "../../lib/tauri-api";
import type { DeliveryReport, TddEvidence } from "../../lib/types";
interface Props {
  root: string;
  taskId: string | null;
  revision?: number;
  onOpenFile: (path: string) => void;
  onToast?: (message: string) => void;
  onReviewChanged?: () => void;
  onTddChanged?: (evidence: TddEvidence) => void;
}
/** Optional export of the same persisted evidence. It never introduces completion state. */
export function DeliveryReportTab({
  root,
  taskId,
  revision = 0,
  onOpenFile,
  onToast,
}: Props) {
  const [report, setReport] = useState<DeliveryReport | null>(null);
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);
  useEffect(() => {
    let disposed = false;
    setReport(null);
    setError("");
    if (taskId)
      void codingApi
        .deliveryReport(root, taskId)
        .then((value) => {
          if (!disposed) setReport(value);
        })
        .catch((error) => {
          if (!disposed) setError(String(error));
        });
    return () => {
      disposed = true;
    };
  }, [root, taskId, revision, reload]);
  if (!taskId)
    return <div className="coding-panel__empty">选择会话后可导出摘要</div>;
  if (error)
    return (
      <div role="alert">
        摘要读取失败：{error}
        <button type="button" onClick={() => setReload((value) => value + 1)}>
          重试
        </button>
      </div>
    );
  if (!report)
    return (
      <div className="coding-panel__empty">
        <LoaderCircle size={15} />
        正在读取真实记录…
      </div>
    );
  const text = [
    `## ${report.task.name}`,
    "",
    report.task.requirement,
    "",
    "### 修改",
    ...report.changes.map(
      (change) => `- ${change.path} (+${change.added} -${change.removed})`,
    ),
    "",
    "### 检查记录（历史结果不代表当前版本）",
    ...(report.verifications.length
      ? report.verifications.map(
          (record) =>
            `- ${record.command}: ${record.status}, exit=${record.exitCode ?? "无"}, revision=${record.contentRevision ?? "旧记录未绑定"}`,
        )
      : ["- 未执行"]),
    "",
    ...report.problems.map(
      (problem) =>
        `- ${problem.file ?? ""}${problem.line ? `:${problem.line}` : ""} ${problem.message}`,
    ),
  ].join("\n");
  return (
    <div className="coding-panel coding-report">
      <div className="coding-panel__actions">
        <button
          type="button"
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(text);
              onToast?.("PR 摘要已复制");
            } catch {
              onToast?.("复制失败，请选择下方文本");
            }
          }}
        >
          <ClipboardCopy size={13} />
          复制 PR 摘要
        </button>
      </div>
      <p>
        摘要引用会话真实变更和检查历史。提交会重新核对所选内容，无需填写交付确认表。
      </p>
      {report.changes.map((change) => (
        <button
          key={change.path}
          type="button"
          onClick={() => onOpenFile(change.path)}
        >
          {change.path}
        </button>
      ))}
      <pre className="coding-context-preview">{text}</pre>
    </div>
  );
}
