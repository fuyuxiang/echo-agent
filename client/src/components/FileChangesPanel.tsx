/**
 * 文件变更聚合面板 —— 对齐 EchoAgent `echo-chat-ui/file-changes-panel`。
 *
 * 从会话消息的 tool_call diff 聚合出「每个文件的净变更」,展示文件类型图标、
 * 路径、增删行(+绿/-红)、编辑次数。
 */
import { useMemo } from "react";
import { aggregateFileChanges, fileIcon, changeStatus } from "@/lib/file-changes";
import type { ChatMessage } from "@/stores/session-store";

interface FileChangesPanelProps {
  messages: ChatMessage[];
}

export function FileChangesPanel({ messages }: FileChangesPanelProps) {
  const summary = useMemo(() => aggregateFileChanges(messages), [messages]);

  return (
    <div className="file-changes" role="region" aria-label="文件变更">
      <div className="file-changes__head">
        <span className="file-changes__title">文件变更</span>
        {summary.totalFiles > 0 && (
          <span className="file-changes__summary">
            {summary.totalFiles} 个文件 ·{" "}
            {summary.files.some(file => file.approximate) && "约 "}
            <span className="file-changes__added">+{summary.totalAdded}</span>{" "}
            <span className="file-changes__removed">-{summary.totalRemoved}</span>
          </span>
        )}
      </div>
      {summary.totalFiles === 0 ? (
        <div className="runtime-panel-empty">还没有文件变更——需要动手时，结果会列在这里。</div>
      ) : (
        <ul className="file-changes__list">
          {summary.files.map((f) => (
            <li
              key={f.path}
              className={"file-changes__row file-changes__row--" + changeStatus(f)}
              title={f.path}
            >
              <span className="file-changes__icon">{fileIcon(f.ext)}</span>
              <span className="file-changes__name">{f.name}</span>
              {f.edits > 1 && (
                <span className="file-changes__edits" title={`${f.edits} 次编辑`}>
                  ×{f.edits}
                </span>
              )}
              <span className="file-changes__stats">
                {f.approximate && "约 "}
                <span className="file-changes__added">+{f.added}</span>
                <span className="file-changes__removed">-{f.removed}</span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
