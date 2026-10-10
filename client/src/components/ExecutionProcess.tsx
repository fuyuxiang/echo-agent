import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Markdown, type MarkdownConfig } from "./markdown/index";
import { ToolCallCard } from "./ToolCallCard";
import { useTheme } from "./ThemeProvider";
import type { MessagePart, ToolCallView } from "@/stores/session-store";
import {
  formatProcessDuration,
  summarizeExecutionProcess,
} from "@/lib/execution-process";
import type { KnowledgeTurnTrace } from "@/stores/knowledge-store";
import {
  forgetExecutionDisclosureChoice,
  getExecutionDisclosureChoice,
  rememberExecutionDisclosureChoice,
} from "@/lib/execution-disclosure";

interface ExecutionProcessProps {
  sessionId?: string;
  messageId?: string;
  parts: MessagePart[];
  active: boolean;
  startedAt?: number;
  completedAt?: number;
  stopReason?: string;
  cancelTrigger?: string;
  cancellationCategory?: string;
  agentResult?: string;
  hasFinalAnswer?: boolean;
  markdownConfig?: MarkdownConfig;
  onOpenTool?: (tool: ToolCallView) => void;
  knowledgeTrace?: KnowledgeTurnTrace;
  onOpenKnowledgePath?: (path: string) => void;
}

type ReasoningPart = Extract<MessagePart, { kind: "text" | "thought" }>;
type ProcessRow =
  | { kind: "reasoning"; parts: ReasoningPart[] }
  | { kind: "tool"; part: Extract<MessagePart, { kind: "tool_call" }> };

interface ReasoningDisclosureProps {
  parts: ReasoningPart[];
  active: boolean;
  hasFinalAnswer: boolean;
  markdownConfig?: MarkdownConfig;
  theme: "light" | "dark";
}

function groupProcessRows(parts: MessagePart[]): ProcessRow[] {
  const rows: ProcessRow[] = [];
  for (const part of parts) {
    if (part.kind === "tool_call") {
      rows.push({ kind: "tool", part });
      continue;
    }
    const previous = rows[rows.length - 1];
    if (previous?.kind === "reasoning") previous.parts.push(part);
    else rows.push({ kind: "reasoning", parts: [part] });
  }
  return rows;
}

function ReasoningDisclosure({
  parts,
  active,
  hasFinalAnswer,
  markdownConfig,
  theme,
}: ReasoningDisclosureProps) {
  const [open, setOpen] = useState(!hasFinalAnswer);
  const userToggled = useRef(false);
  const previousActive = useRef(active);
  const previousHasFinalAnswer = useRef(hasFinalAnswer);

  useLayoutEffect(() => {
    const beganNewRun = active && !previousActive.current;
    if (beganNewRun) userToggled.current = false;
    if (
      !userToggled.current
      && (beganNewRun || previousHasFinalAnswer.current !== hasFinalAnswer)
    ) {
      setOpen(!hasFinalAnswer);
    }
    previousActive.current = active;
    previousHasFinalAnswer.current = hasFinalAnswer;
  }, [active, hasFinalAnswer]);

  return (
    <details className="execution-process__reasoning" open={open}>
      <summary
        onClick={(event) => {
          // Keep the disclosure controlled so streamed parent renders cannot
          // overwrite an explicit user choice.
          event.preventDefault();
          userToggled.current = true;
          setOpen((value) => !value);
        }}
      >
        深度思考
      </summary>
      <div className="execution-process__reasoning-body">
        {parts.map((part, partIndex) => (
          <Markdown
            key={partIndex}
            complete={!active}
            markdownTheme="reasoning"
            theme={theme}
            config={markdownConfig}
          >
            {part.text}
          </Markdown>
        ))}
      </div>
    </details>
  );
}

/**
 * One desktop-friendly process group per assistant turn. It keeps the live
 * state readable, folds automatically when the final answer begins, and delegates
 * dense tool output to the existing right-side detail panel.
 */
export function ExecutionProcess({
  sessionId,
  messageId,
  parts,
  active,
  startedAt,
  completedAt,
  stopReason,
  cancelTrigger,
  cancellationCategory,
  agentResult,
  hasFinalAnswer = false,
  markdownConfig,
  onOpenTool,
  knowledgeTrace,
  onOpenKnowledgePath,
}: ExecutionProcessProps) {
  const { theme } = useTheme();
  const bodyId = useId();
  const summary = useMemo(
    () => summarizeExecutionProcess(
      parts,
      active,
      stopReason,
      cancellationCategory,
      cancelTrigger,
    ),
    [active, cancellationCategory, cancelTrigger, parts, stopReason],
  );
  const needsAttention = summary.state === "attention" || summary.state === "stopped";
  const hasUnfinishedTools = parts.some((part) =>
    part.kind === "tool_call" && part.toolCall.status === "in_progress",
  );
  // The answer can contain interim text while parallel tools are still active.
  // Keep both the status and automatic disclosure live until those operations
  // have ended, even if an embedding caller reports visible answer text.
  const finalAnswerStarted = hasFinalAnswer
    && !hasUnfinishedTools
    && knowledgeTrace?.personal?.state !== "searching";
  // Show work while it is happening. Completed turns stay compact, including
  // tool-only and failed turns; their status remains visible in the header.
  const shouldOpenAutomatically = active && !finalAnswerStarted;
  const savedChoice = getExecutionDisclosureChoice(sessionId, messageId);
  const [open, setOpen] = useState(savedChoice ?? shouldOpenAutomatically);
  const [now, setNow] = useState(() => Date.now());
  const previousActive = useRef(active);
  const previousAutomaticOpen = useRef(shouldOpenAutomatically);
  const userToggled = useRef(savedChoice !== undefined);

  useLayoutEffect(() => {
    if (!startedAt || !active) return;
    const update = () => setNow(Date.now());
    update();
    const timer = window.setInterval(update, 1_000);
    return () => window.clearInterval(timer);
  }, [active, startedAt]);

  useEffect(() => {
    const beganNewRun = active && !previousActive.current;
    if (beganNewRun) {
      userToggled.current = false;
      forgetExecutionDisclosureChoice(sessionId, messageId);
    }

    // Follow the automatic state only until the user makes an explicit choice.
    // In particular, collapse as soon as the first final-answer token arrives,
    // rather than waiting for the entire turn to finish streaming.
    if (
      !userToggled.current
      && (beganNewRun || previousAutomaticOpen.current !== shouldOpenAutomatically)
    ) {
      setOpen(shouldOpenAutomatically);
    }

    previousActive.current = active;
    previousAutomaticOpen.current = shouldOpenAutomatically;
  }, [active, messageId, sessionId, shouldOpenAutomatically]);

  const processRows = useMemo(() => groupProcessRows(parts), [parts]);
  const duration = startedAt
    ? formatProcessDuration(Math.max(0, (completedAt ?? now) - startedAt))
    : null;
  const meta: string[] = [];
  if (summary.toolCount > 0) {
    meta.push(active
      ? `${summary.completedToolCount}/${summary.toolCount} 项`
      : `${summary.toolCount} 项操作`);
  } else if (summary.thoughtCount > 0) {
    meta.push(`${summary.thoughtCount} 段思考过程`);
  }
  if (summary.changedFiles.length > 0) meta.push(`${summary.changedFiles.length} 个文件`);
  if (duration) meta.push(duration);
  if (knowledgeTrace?.personal?.state === "used") {
    meta.unshift(`${knowledgeTrace.personal.resultCount} 个知识片段`);
  }
  const knowledgeOnly = parts.length === 0 && Boolean(knowledgeTrace);
  const title = knowledgeTrace?.personal?.state === "searching"
    ? "正在检索个人知识库"
    : active && finalAnswerStarted && !needsAttention
      ? summary.toolCount > 0 ? "已完成执行过程" : "已完成思考"
    : knowledgeOnly && !active
      ? "知识检索完成"
      : summary.title;
  const displayState = active && finalAnswerStarted && summary.state === "running"
    ? "complete"
    : summary.state;

  return (
    <section className={`execution-process execution-process--${displayState}`}>
      <button
        type="button"
        className="execution-process__header"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => {
          userToggled.current = true;
          const next = !open;
          rememberExecutionDisclosureChoice(sessionId, messageId, next);
          setOpen(next);
        }}
      >
        <span className="execution-process__status" aria-hidden="true" />
        <span className="execution-process__heading">
          <span className="execution-process__title" role="status" aria-live="polite">
            {title}
          </span>
          {meta.length > 0 && (
            <span className="execution-process__meta">{meta.join(" · ")}</span>
          )}
        </span>
        <span
          className={`execution-process__chevron${open ? " execution-process__chevron--open" : ""}`}
          aria-hidden="true"
        >
          ›
        </span>
      </button>

      {open && (
        <div className="execution-process__body" id={bodyId}>
          {!active && agentResult && (
            <p className="execution-process__terminal-detail" role="alert">
              {agentResult}
            </p>
          )}
          {knowledgeTrace && (
            <KnowledgeTrace
              trace={knowledgeTrace}
              onOpenPath={onOpenKnowledgePath}
            />
          )}
          {processRows.map((row, index) => {
            if (row.kind === "tool") {
              return (
                <ToolCallCard
                  key={row.part.toolCall.toolCallId || `tool-${index}`}
                  tc={row.part.toolCall}
                  onOpen={onOpenTool}
                />
              );
            }
            return (
              <ReasoningDisclosure
                key={`reasoning-${index}`}
                parts={row.parts}
                active={active}
                hasFinalAnswer={finalAnswerStarted}
                markdownConfig={markdownConfig}
                theme={theme}
              />
            );
          })}

          {summary.toolCount > 0 && onOpenTool && (
            <p className="execution-process__hint">选择操作可在右侧查看输入、输出和文件变更</p>
          )}
        </div>
      )}
    </section>
  );
}

function KnowledgeTrace({
  trace,
  onOpenPath,
}: {
  trace: KnowledgeTurnTrace;
  onOpenPath?: (path: string) => void;
}) {
  const personal = trace.personal;
  const personalVisible = personal && personal.state !== "idle";
  const organizationUnavailable = trace.organization?.state === "unavailable";
  if (!personalVisible && !organizationUnavailable) return null;

  return (
    <div className="knowledge-trace" aria-label="知识检索过程">
      {personal?.state === "searching" && (
        <div className="knowledge-trace__status is-searching">
          <span aria-hidden="true" />
          正在从个人知识库检索相关片段…
        </div>
      )}
      {personal?.state === "no-match" && (
        <div className="knowledge-trace__status">
          已搜索 {personal.sourceCount} 个个人知识源，未找到相关内容
        </div>
      )}
      {personal?.state === "blocked" && (
        <div className="knowledge-trace__status is-warning">个人知识未使用：{personal.message}</div>
      )}
      {personal?.state === "error" && (
        <div className="knowledge-trace__status is-warning">个人知识检索失败：{personal.message}</div>
      )}
      {personal?.state === "used" && (
        <details className="knowledge-trace__results" open>
          <summary>
            已向模型提供 {personal.resultCount} 个相关片段
            <small>来自 {personal.sourceCount} 个知识源</small>
          </summary>
          <div className="knowledge-trace__items">
            {personal.items.map((item, index) => {
              const lineRange = item.startLine
                ? item.startLine === item.endLine || !item.endLine
                  ? `第 ${item.startLine} 行`
                  : `第 ${item.startLine}–${item.endLine} 行`
                : null;
              const body = (
                <>
                  <strong>{item.title}</strong>
                  {(item.sourceLabel || lineRange) && (
                    <small>{[item.sourceLabel, lineRange].filter(Boolean).join(" · ")}</small>
                  )}
                  {item.snippet && <span>{item.snippet}</span>}
                  {item.path && <code>{item.path}</code>}
                </>
              );
              return item.path && onOpenPath ? (
                <button
                  key={`${item.path}-${index}`}
                  type="button"
                  className="knowledge-trace__item"
                  title={`打开 ${item.path}`}
                  onClick={() => onOpenPath(item.path!)}
                >
                  {body}
                </button>
              ) : (
                <div className="knowledge-trace__item" key={`${item.title}-${index}`}>{body}</div>
              );
            })}
          </div>
        </details>
      )}
      {organizationUnavailable && (
        <div className="knowledge-trace__status is-warning">
          {trace.organization?.message ?? "组织知识库当前不可用，本次未使用"}
        </div>
      )}
    </div>
  );
}
