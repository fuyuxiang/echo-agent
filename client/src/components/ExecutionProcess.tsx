import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Markdown, type MarkdownConfig } from "./markdown/index";
import { ToolCallCard } from "./ToolCallCard";
import { documentProgressSummary } from "@/lib/document-progress";
import { executionProgressText } from "@/lib/execution-process-preview";
import { executionStages, executionStageState, stageOperationSummary } from "@/lib/execution-stages";
import { useTheme } from "./ThemeProvider";
import type { MessagePart, ToolCallView } from "@/stores/session-store";
import { formatProcessDuration, summarizeExecutionProcess } from "@/lib/execution-process";
import type { KnowledgeTurnTrace } from "@/stores/knowledge-store";
import type { ExecutionNotice } from "@/lib/execution-notice";
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
  settled?: boolean;
  autoExpand?: boolean;
  startedAt?: number;
  completedAt?: number;
  lastActivityAt?: number;
  stopReason?: string;
  cancelTrigger?: string;
  cancellationCategory?: string;
  agentResult?: string;
  resultNotice?: Pick<ExecutionNotice, "title" | "tone">;
  hasFinalAnswer?: boolean;
  waitingReason?: string;
  markdownConfig?: MarkdownConfig;
  findQuery?: string;
  onOpenTool?: (tool: ToolCallView) => void;
  knowledgeTrace?: KnowledgeTurnTrace;
  onOpenKnowledgePath?: (path: string) => void;
  revealRequest?: number;
}

type ReasoningPart = Extract<MessagePart, { kind: "thought" }>;
type ToolPart = Extract<MessagePart, { kind: "tool_call" }>;
type ProcessRow =
  | { kind: "thought"; parts: ReasoningPart[]; index: number }
  | { kind: "tools"; parts: ToolPart[]; index: number }
  | { kind: "part"; part: Extract<MessagePart, { kind: "text" }>; index: number };

/** Preserve actual event order, including commentary between retries. */
function groupStageRows(parts: MessagePart[]): ProcessRow[] {
  const rows: ProcessRow[] = [];
  parts.forEach((part, index) => {
    if (part.kind !== "tool_call" && !part.text.trim()) return;
    if (part.kind === "thought") {
      const previous = rows[rows.length - 1];
      if (previous?.kind === "thought") previous.parts.push(part);
      else rows.push({ kind: "thought", parts: [part], index });
    } else if (part.kind === "tool_call") {
      const previous = rows[rows.length - 1];
      if (previous?.kind === "tools") previous.parts.push(part);
      else rows.push({ kind: "tools", parts: [part], index });
    } else rows.push({ kind: "part", part, index });
  });
  return rows;
}

function ReasoningDisclosure({ parts, active, markdownConfig, theme }: {
  parts: ReasoningPart[];
  active: boolean;
  markdownConfig?: MarkdownConfig;
  theme: "light" | "dark";
}) {
  const [open, setOpen] = useState(false);
  const [full, setFull] = useState(false);
  const contentId = useId();
  const text = parts.map(part => part.text).join("\n\n");
  const preview = Array.from(text.split("\n").slice(0, 10).join("\n")).slice(0, 600).join("");
  const shortened = preview.length < text.length;
  return <details className="execution-process__reasoning" open={open}>
    <summary onClick={event => { event.preventDefault(); setOpen(value => !value); }}>分析详情</summary>
    {open && <>
      {shortened && !full && <p className="execution-process__hint">内容较长，当前展示开头片段。</p>}
      <div id={contentId} className="execution-process__reasoning-body">
        <Markdown complete={!active} markdownTheme="reasoning" theme={theme}
          config={markdownConfig}>{shortened && !full ? `${preview}\n\n…` : text}</Markdown>
      </div>
      {shortened && <button type="button" className="execution-process__read-more"
        aria-expanded={full} aria-controls={contentId} onClick={() => setFull(value => !value)}>
        {full ? "收起全文" : "展开全文"}
      </button>}
    </>}
  </details>;
}

function StageTools({ parts, recoveredById, onOpenTool, active }: {
  parts: ToolPart[];
  recoveredById: Map<string, string>;
  onOpenTool?: (tool: ToolCallView) => void;
  active: boolean;
}) {
  // Records have one disclosure level. Every operation is immediately reachable
  // after opening records, without a second "view operations" toggle.
  return <div className={`execution-process__tool-group${parts.length === 1 ? " execution-process__tool" : ""}`}>
    {parts.map(({ toolCall }) => <div key={toolCall.toolCallId} className="execution-process__tool">
      <ToolCallCard tc={toolCall} onOpen={onOpenTool} showStatusLabel executionEnded={!active} />
      {recoveredById.has(toolCall.toolCallId) && <span className="execution-process__recovery-detail">该次失败已通过后续重试恢复</span>}
    </div>)}
  </div>;
}

/** Task progress first; one disclosure for the complete chronological record. */
export function ExecutionProcess({
  sessionId, messageId, parts, active, settled, autoExpand = false, startedAt, completedAt,
  lastActivityAt, stopReason, cancelTrigger, cancellationCategory, agentResult,
  resultNotice, hasFinalAnswer = false, waitingReason, markdownConfig, findQuery, onOpenTool,
  knowledgeTrace, onOpenKnowledgePath, revealRequest = 0,
}: ExecutionProcessProps) {
  const { theme } = useTheme();
  const bodyId = useId();
  const bodyRef = useRef<HTMLDivElement>(null);
  const headerRef = useRef<HTMLButtonElement>(null);
  const returnToOverview = useRef(false);
  const summary = useMemo(() => summarizeExecutionProcess(parts, active, stopReason,
    cancellationCategory, cancelTrigger, { settled }),
  [active, cancellationCategory, cancelTrigger, parts, settled, stopReason]);
  const hasUnfinishedTools = parts.some(part => part.kind === "tool_call" && part.toolCall.status === "in_progress");
  const finalAnswerStarted = hasFinalAnswer && !waitingReason && !hasUnfinishedTools
    && knowledgeTrace?.personal?.state !== "searching";
  const shouldOpenAutomatically = autoExpand && active && !finalAnswerStarted;
  const savedChoice = getExecutionDisclosureChoice(sessionId, messageId);
  const [open, setOpen] = useState(savedChoice ?? shouldOpenAutomatically);
  const [collapsedSearch, setCollapsedSearch] = useState<string>();
  const searchMatch = Boolean(findQuery && parts.some(part => part.kind === "text"
    && part.phase !== "commentary" && part.text.toLocaleLowerCase().includes(findQuery.toLocaleLowerCase())));
  const expanded = open || searchMatch && collapsedSearch !== findQuery;
  const [now, setNow] = useState(() => Date.now());
  const previousActive = useRef(active);
  const previousAutomaticOpen = useRef(shouldOpenAutomatically);
  const userToggled = useRef(savedChoice !== undefined);
  const [resumedAt, setResumedAt] = useState(0);
  const previousWaiting = useRef(waitingReason);

  useEffect(() => { setCollapsedSearch(undefined); }, [findQuery]);

  useEffect(() => {
    if (!revealRequest) return;
    userToggled.current = true;
    setOpen(true);
    rememberExecutionDisclosureChoice(sessionId, messageId, true);
    headerRef.current?.focus({ preventScroll: true });
    headerRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [revealRequest, sessionId, messageId]);

  useLayoutEffect(() => {
    if (!active) return;
    const update = () => setNow(Date.now());
    update();
    const timer = window.setInterval(update, 1_000);
    return () => window.clearInterval(timer);
  }, [active]);

  useLayoutEffect(() => {
    if (previousWaiting.current && !waitingReason) setResumedAt(Date.now());
    previousWaiting.current = waitingReason;
  }, [waitingReason]);

  useEffect(() => {
    const beganNewRun = active && !previousActive.current;
    if (beganNewRun) {
      userToggled.current = false;
      setResumedAt(Date.now());
      forgetExecutionDisclosureChoice(sessionId, messageId);
    }
    if (!userToggled.current && (beganNewRun || previousAutomaticOpen.current !== shouldOpenAutomatically)) {
      setOpen(shouldOpenAutomatically);
    }
    previousActive.current = active;
    previousAutomaticOpen.current = shouldOpenAutomatically;
  }, [active, messageId, sessionId, shouldOpenAutomatically]);

  useLayoutEffect(() => {
    if (!expanded) return;
    const body = bodyRef.current;
    return () => {
      if (body?.contains(document.activeElement)) headerRef.current?.focus({ preventScroll: true });
    };
  }, [expanded]);

  useLayoutEffect(() => {
    if (expanded || !returnToOverview.current) return;
    returnToOverview.current = false;
    // A footer collapse removes the reader's current position. Return to the
    // surviving control after layout, including when activated by keyboard.
    headerRef.current?.focus({ preventScroll: true });
    headerRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [expanded]);

  const stages = useMemo(() => executionStages(parts), [parts]);
  const currentStage = stages[stages.length - 1];
  const progress = useMemo(() => executionProgressText(currentStage?.progress ?? []), [currentStage]);
  const tools = useMemo(() => parts.flatMap(part => part.kind === "tool_call" ? [part.toolCall] : []), [parts]);
  const recoveredById = new Map(summary.recoveredTools.map(item => [item.failedToolCallId, item.successfulToolCallId]));
  const recoveredIds = new Set(recoveredById.keys());
  const recovered = summary.state === "complete" && recoveredIds.size > 0;
  const title = !active ? resultNotice?.title ?? (summary.state === "complete" ? "本轮已完成" : summary.title)
    : waitingReason ?? (knowledgeTrace?.personal?.state === "searching" ? "正在检索个人知识库"
      : finalAnswerStarted ? "正在生成回复"
        : currentStage && (currentStage.planned || currentStage.progress.length > 0) ? currentStage.title : summary.title);
  const duration = startedAt != null && (active || completedAt != null)
    ? formatProcessDuration(Math.max(0, (completedAt ?? now) - startedAt)) : null;
  const activityAt = lastActivityAt ?? startedAt;
  const idleMs = activityAt == null ? 0 : Math.max(0, now - Math.max(activityAt, resumedAt));
  const longWait = active && !finalAnswerStarted && !waitingReason && idleMs >= 30_000;
  const waitingFor = knowledgeTrace?.personal?.state === "searching" ? "知识库" : hasUnfinishedTools ? "操作" : "模型";
  const description = progress.startsWith(title)
    ? progress.slice(title.length).replace(/^[。！？!?\s]+/, "") : progress;
  const showProgress = active && !finalAnswerStarted && Boolean(description);
  const meta = [stageOperationSummary(tools, recoveredIds, !active)];
  if (summary.changedFiles.length) meta.push(`${summary.changedFiles.length} 个文件已更新`);
  const documentSummary = documentProgressSummary(tools.flatMap(tool => tool.documentProgress ? [tool.documentProgress] : []));
  if (documentSummary) meta.unshift(documentSummary);
  if (knowledgeTrace?.personal?.state === "used") meta.push(`${knowledgeTrace.personal.resultCount} 个知识片段`);
  const changeOpen = (next: boolean) => {
    userToggled.current = true;
    setCollapsedSearch(!next && searchMatch ? findQuery : undefined);
    rememberExecutionDisclosureChoice(sessionId, messageId, next);
    setOpen(next);
  };
  let plannedStageNumber = 0;
  const stageNumbers = stages.map(stage => stage.planned ? ++plannedStageNumber : undefined);

  const state = waitingReason && active ? "waiting"
    : !active && summary.state === "complete" && resultNotice?.tone === "warning" ? "attention" : summary.state;
  const disclosureLabel = expanded ? "收起过程" : active ? "展开过程" : "查看过程";
  return <section className={`execution-process execution-process--${state}${recovered ? " execution-process--recovered" : ""}`}
    aria-label="任务进展">
    <div className="execution-process__overview">
      <span className="execution-process__status" aria-hidden="true" />
      <div className="execution-process__heading">
        <span className="execution-process__title" role={finalAnswerStarted ? undefined : "status"}>{title}</span>
        {duration && <span className="execution-process__meta">{active ? "已用时 " : "用时 "}{duration}</span>}
      </div>
      <button ref={headerRef} type="button" className="execution-process__header"
        data-chat-reading="true"
        aria-label={[title, duration, disclosureLabel].filter(Boolean).join(" · ")}
        aria-expanded={expanded} aria-controls={expanded ? bodyId : undefined}
        aria-describedby={expanded && summary.description ? `${bodyId}-summary` : undefined}
        onClick={() => changeOpen(!expanded)}>
        <span>{disclosureLabel}</span>
        <span className={`execution-process__chevron${expanded ? " execution-process__chevron--open" : active ? " execution-process__chevron--expand" : ""}`} aria-hidden="true">›</span>
      </button>
    </div>
    {showProgress && <div className="execution-process__excerpt" data-stage-id={currentStage?.id} aria-label="当前阶段进展">
      <p>{description}</p>
    </div>}
    {meta.some(Boolean) && <p className="execution-process__metrics">{meta.filter(Boolean).join(" · ")}</p>}
    {longWait && <p className="execution-process__waiting-note" role="note">
      已 {formatProcessDuration(idleMs)} 未收到新进展，正在等待{waitingFor}返回。可继续等待，或停止本次执行。
    </p>}
    {expanded && <div className="execution-process__body" id={bodyId} ref={bodyRef}
      data-chat-reading="true"
      tabIndex={0} role="region" aria-label="执行过程详情">
      {summary.description && <p id={`${bodyId}-summary`} className="execution-process__outcome-detail">{summary.description}</p>}
      {summary.recoveredTools.length > 0 && <p className="execution-process__recovery-detail">
        {summary.recoveredTools.length} 次中间失败已由后续同一操作的重试恢复，原始记录保留如下。
      </p>}
      {!active && agentResult && <p className="execution-process__terminal-detail">{agentResult}</p>}
      {knowledgeTrace && <KnowledgeTrace trace={knowledgeTrace} active={active} onOpenPath={onOpenKnowledgePath} />}
      <ol className="execution-process__stages" aria-label="执行记录">
        {stages.map((stage, index) => {
          const rows = groupStageRows(stage.parts);
          const headingPart = !stage.planned ? stage.progress[0] : undefined;
          const headingIsProgress = Boolean(headingPart && stage.parts[0] === headingPart
            && headingPart.text.trim().replace(/[。！？!?]+$/, "") === stage.title);
          const state = executionStageState(stage, index === stages.length - 1, active, Boolean(waitingReason), recoveredIds);
          return <li key={stage.id} data-stage-id={stage.id} data-stage-state={state}
            className={`execution-process__stage${stage.planned ? "" : " execution-process__stage--activity"}${active && index === stages.length - 1 ? " execution-process__stage--current" : ""}`}>
            <div className="execution-process__stage-heading">
              <h4><span className="execution-process__stage-index" aria-hidden="true">{stageNumbers[index] ?? "·"}</span>{headingIsProgress && headingPart ? headingPart.text.trim() : stage.title.replace(/^正在/, "")}</h4>
              <span className="execution-process__stage-state">{state}</span>
            </div>
            {rows.map((row, rowIndex) => {
              const rowActive = active && index === stages.length - 1 && rowIndex === rows.length - 1;
              if (row.kind === "thought") return <ReasoningDisclosure key={row.index} parts={row.parts} active={rowActive} markdownConfig={markdownConfig} theme={theme} />;
              if (row.kind === "tools") return <StageTools key={row.index} parts={row.parts} recoveredById={recoveredById} onOpenTool={onOpenTool} active={active} />;
              if (headingIsProgress && row.part === headingPart && !findQuery) return null;
              return <div className="execution-process__progress" key={row.index}>
                <Markdown complete={!rowActive} theme={theme} config={markdownConfig}
                  findQuery={row.part.phase !== "commentary" ? findQuery : undefined}>{row.part.text}</Markdown>
              </div>;
            })}
          </li>;
        })}
      </ol>
      <div className="execution-process__footer">
        {summary.toolCount > 0 && onOpenTool && <p className="execution-process__hint">选择操作可在右侧查看输入、输出和文件变更</p>}
        <button type="button" className="execution-process__collapse"
          aria-expanded={true} aria-controls={bodyId}
          onClick={() => { returnToOverview.current = true; changeOpen(false); }}>
          <span>收起过程</span>
          <span className="execution-process__chevron execution-process__chevron--open" aria-hidden="true">›</span>
        </button>
      </div>
    </div>}
  </section>;
}

function KnowledgeTrace({
  trace,
  active,
  onOpenPath,
}: {
  trace: KnowledgeTurnTrace;
  active: boolean;
  onOpenPath?: (path: string) => void;
}) {
  const personal = trace.personal;
  const personalVisible = personal && personal.state !== "idle";
  const organizationUnavailable = trace.organization?.state === "unavailable";
  if (!personalVisible && !organizationUnavailable) return null;

  return (
    <div className="knowledge-trace" aria-label="知识检索过程">
      {personal?.state === "searching" && (
        <div className={`knowledge-trace__status${active ? " is-searching" : ""}`}>
          <span aria-hidden="true" />
          {active ? "正在从个人知识库检索相关片段…" : "个人知识检索未记录完成结果"}
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
