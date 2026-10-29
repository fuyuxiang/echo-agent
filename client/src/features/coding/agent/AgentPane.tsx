import { useMemo, type ReactNode } from "react";
import { AlertTriangle, CheckCircle2, Pause, Square } from "lucide-react";

import { ExecutionProcess } from "@/components/ExecutionProcess";
import { Markdown, type MarkdownConfig } from "@/components/Markdown";
import { PermissionInlineCard } from "@/components/PermissionDialog";
import { QuestionInlineCard } from "@/components/QuestionInlineCard";
import { useStickToBottom } from "@/components/use-stick-to-bottom";
import { partitionAssistantParts } from "@/lib/execution-process";
import type { ChatMessage } from "@/stores/session-store";

import { describeTaskProgress } from "../lib/phase";
import type {
  ChangeSet,
  CodingTask,
  DeliveryReport,
  VerificationRecord,
} from "../lib/types";

interface AgentPaneProps {
  task: CodingTask;
  deliveryReport?: DeliveryReport | null;
  changeSet: ChangeSet | null;
  verifications: VerificationRecord[];
  sessionId: string | null;
  messages: ChatMessage[];
  streaming: boolean;
  phaseReason?: string;
  blocker?: string | null;
  awaitingPermission: boolean;
  awaitingQuestion: boolean;
  planApproval?: ReactNode;
  sending: boolean;
  onContinue: () => void | Promise<void>;
  continueDisabled?: boolean;
  executionPending?: string;
  onOpenVerification?: () => void;
  onOpenChanges: () => void;
  onOpenReport: () => void;
  onOpenFile?: (path: string, line?: number) => void;
  /** SP4: called when the user drops file-tree paths onto the agent pane. */
  onPathsDropped?: (paths: string[]) => void;
}

/** Agent activity, interaction requests and delivery summary for one task. */
export function AgentPane({
  task,
  deliveryReport: _deliveryReport,
  changeSet,
  verifications,
  sessionId,
  messages,
  streaming,
  phaseReason,
  blocker,
  awaitingPermission,
  awaitingQuestion,
  planApproval,
  sending,
  onContinue,
  continueDisabled = false,
  executionPending,
  onOpenVerification,
  onOpenChanges,
  onOpenReport: _onOpenReport,
  onOpenFile,
  onPathsDropped,
}: AgentPaneProps) {
  const markdownConfig = useMemo<MarkdownConfig | undefined>(
    () =>
      onOpenFile
        ? {
            pathClickHandler: {
              onPathClick: (path, type, range) => {
                if (type === "file") onOpenFile(path, range?.start);
              },
            },
          }
        : undefined,
    [onOpenFile],
  );

  const phase = describeTaskProgress(task.phase);
  const sessionUnavailable = !sessionId;
  const changes = changeSet?.changes ?? [];
  const latestChecks = new Map<string, VerificationRecord>();
  for (const record of verifications) latestChecks.set(record.command, record);
  const checks = [...latestChecks.values()];
  const verificationSummary =
    checks.length === 0
      ? "检查未执行"
      : `已记录 ${checks.length} 项检查，结果与版本见详情`;

  const { scrollRef, contentRef, following, scrollToBottom } = useStickToBottom(
    {
      contentVersion: messages,
      streaming,
      sessionId,
    },
  );

  return (
    <div
      className="coding-agent"
      onDragOver={(event) => {
        if (event.dataTransfer.types.includes("application/x-echo-paths")) {
          event.preventDefault();
          event.dataTransfer.dropEffect = "copy";
        }
      }}
      onDrop={(event) => {
        if (!event.dataTransfer.types.includes("application/x-echo-paths"))
          return;
        event.preventDefault();
        const raw = event.dataTransfer.getData("application/x-echo-paths");
        if (!raw) return;
        try {
          const parsed = JSON.parse(raw) as string[];
          if (Array.isArray(parsed) && parsed.length > 0 && onPathsDropped) {
            onPathsDropped(parsed);
          }
        } catch {
          /* noop — Tauri drop delivers real paths separately */
        }
      }}
    >
      <div className="coding-agent__body">
        {phaseReason &&
          !blocker &&
          !["paused", "stopped", "delivered"].includes(task.phase) && (
            <div className="coding-agent__reason">{phaseReason}</div>
          )}

        {task.planRevision && task.taskNodes.length > 0 && (
          <details className="coding-agent__workflow-progress">
            <summary>
              执行计划 ·{" "}
              {
                task.taskNodes.filter((node) => node.status === "success")
                  .length
              }
              /{task.taskNodes.length}
            </summary>
            {task.taskNodes.map((node) => (
              <p key={node.id}>
                {node.content} · {node.status}
              </p>
            ))}
          </details>
        )}

        {task.planIssues.length > 0 && (
          <details>
            <summary>计划详情 · {task.planIssues.length} 项提示</summary>
            <div role="status">
              {task.planIssues.map((issue) => (
                <p key={`${issue.code}:${issue.nodeKeys.join(",")}`}>
                  {issue.message}
                </p>
              ))}
            </div>
          </details>
        )}
        {blocker && (
          <div className="coding-agent__blocker" role="alert">
            <AlertTriangle size={13} />
            <div>
              <strong>需要人工介入</strong>
              <p>{blocker}</p>
            </div>
          </div>
        )}

        {executionPending && (
          <div className="coding-agent__decision is-interrupted" role="status">
            <Square size={12} />
            <div className="coding-agent__decision-copy"><strong>仍在停止</strong><span>{executionPending}</span></div>
            {changes.length > 0 && <button type="button" onClick={onOpenChanges}>查看当前变更</button>}
          </div>
        )}
        {!executionPending && (task.phase === "paused" || task.phase === "stopped") && (
          <div className="coding-agent__decision is-interrupted" role="status">
            {task.phase === "paused" ? (
              <Pause size={13} />
            ) : (
              <Square size={12} />
            )}
            <div className="coding-agent__decision-copy">
              <strong>
                {task.phase === "paused" ? "任务已暂停" : "任务已停止"}
              </strong>
              <span>
                {phaseReason ??
                  (task.phase === "paused"
                    ? "会话上下文和当前文件变更已保留。"
                    : "执行记录和当前工作区状态已保留。")}
              </span>
            </div>
            <div className="coding-agent__decision-actions">
              {changes.length > 0 && (
                <button type="button" onClick={onOpenChanges}>
                  查看当前变更
                </button>
              )}
              {continueDisabled && onOpenVerification && (
                <button type="button" onClick={onOpenVerification}>
                  运行检查
                </button>
              )}
              <button
                type="button"
                className="is-primary"
                disabled={sending || sessionUnavailable || continueDisabled}
                onClick={() => void onContinue()}
              >
                {sending
                  ? "正在继续…"
                  : continueDisabled
                    ? "等待检查"
                    : "继续执行"}
              </button>
            </div>
          </div>
        )}

        {!executionPending && task.phase === "delivered" && (
          <div className="coding-agent__decision is-good" role="status">
            <CheckCircle2 size={13} />
            <div className="coding-agent__decision-copy">
              <strong>本轮完成</strong>
              <span>
                {changes.length
                  ? `${changes.length} 个文件变更 · ${verificationSummary}`
                  : "已完成回答"}
              </span>
            </div>
            {changes.length > 0 && (
              <div className="coding-agent__decision-actions">
                <button type="button" onClick={onOpenChanges}>
                  审阅修改
                </button>
                <button type="button" onClick={onOpenVerification}>
                  检查详情
                </button>
              </div>
            )}
          </div>
        )}

        {(awaitingPermission || awaitingQuestion || planApproval) && (
          <div className="coding-agent__interaction" aria-label="等待你的操作">
            {awaitingPermission && (
              <PermissionInlineCard sessionId={sessionId} />
            )}
            {awaitingQuestion && <QuestionInlineCard sessionId={sessionId} />}
            {planApproval}
          </div>
        )}

        <div className="coding-agent__stream-shell">
          <div
            className="coding-agent__stream"
            ref={scrollRef}
            aria-label="Agent 执行消息"
          >
            <div className="coding-agent__stream-content" ref={contentRef}>
              {messages.length === 0 && sessionId && phase.active && (
                <div className="coding-row">Agent 正在准备工程上下文…</div>
              )}
              {messages.length === 0 && !sessionId && (
                <div className="coding-row">Agent 会话未启动。</div>
              )}
              {messages.length === 0 && sessionId && !phase.active && (
                <div className="coding-row">当前任务暂无对话记录。</div>
              )}
              {messages.map((entry) => {
                if (entry.role === "user") {
                  return (
                    <div className="coding-agent__user" key={entry.id}>
                      {entry.parts.map((part, index) =>
                        part.kind === "text" ? (
                          <Markdown
                            key={index}
                            complete
                            config={markdownConfig}
                          >
                            {part.text}
                          </Markdown>
                        ) : null,
                      )}
                    </div>
                  );
                }
                const groups = partitionAssistantParts(entry.parts);
                const hasFinalAnswer = groups.responseParts.some((part) =>
                  part.text.trim(),
                );
                const hasTerminalStatus = Boolean(
                  entry.complete &&
                    entry.stopReason &&
                    entry.stopReason !== "end_turn",
                );
                return (
                  <div className="coding-agent__assistant" key={entry.id}>
                    {(groups.processParts.length > 0 || hasTerminalStatus) && (
                      <ExecutionProcess
                        sessionId={sessionId ?? undefined}
                        messageId={entry.id}
                        parts={groups.processParts}
                        active={streaming && !entry.complete}
                        startedAt={entry.startedAt}
                        completedAt={entry.completedAt}
                        stopReason={entry.stopReason}
                        cancelTrigger={entry.cancelTrigger}
                        cancellationCategory={entry.cancellationCategory}
                        agentResult={entry.agentResult}
                        hasFinalAnswer={hasFinalAnswer}
                        onOpenKnowledgePath={onOpenFile}
                      />
                    )}
                    {groups.responseParts.map((part, index) => (
                      <Markdown
                        key={index}
                        complete={entry.complete}
                        config={markdownConfig}
                      >
                        {part.text}
                      </Markdown>
                    ))}
                  </div>
                );
              })}
            </div>
          </div>
          {streaming && !following && (
            <button
              type="button"
              className="chatview__jump-latest coding-agent__jump-latest"
              onClick={scrollToBottom}
              aria-label="回到最新消息并恢复自动跟随"
              title="回到最新消息并恢复自动跟随"
            >
              <span aria-hidden="true">↓</span>
              回到最新
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
