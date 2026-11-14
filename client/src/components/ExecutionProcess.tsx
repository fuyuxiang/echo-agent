import type { AutomationStatus } from "@/lib/automation-client";
import { ComputerExecutionDetails } from "./ComputerExecutionDetails";
import { computerTaskLabel } from "@/lib/computer-task";
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Markdown, type MarkdownConfig } from "./markdown/index";
import { ToolCallCard } from "./ToolCallCard";
import { ExecutionActivity } from "./ExecutionActivity";
import { executionActivities, type ExecutionActivity as Activity } from "@/lib/execution-activity";
import { executionToolStates, type ToolExecutionState } from "@/lib/tool-execution-state";
import { documentProgressSummary } from "@/lib/document-progress";
import { executionProgressText } from "@/lib/execution-process-preview";
import { executionStages, executionStageState, stageOperationSummary } from "@/lib/execution-stages";
import { useTheme } from "./ThemeProvider";
import type { MessagePart, ToolCallView } from "@/stores/session-store";
import { formatProcessDuration, summarizeExecutionProcess } from "@/lib/execution-process";
import type { KnowledgeTurnTrace } from "@/stores/knowledge-store";
import type { ExecutionNotice } from "@/lib/execution-notice";
import { isNearScrollBottom } from "./use-stick-to-bottom";
import {
  forgetExecutionDisclosureChoice,
  getExecutionDisclosureChoice,
  getExecutionReadingPosition,
  getExecutionReasoningChoices,
  rememberExecutionDisclosureChoice,
  rememberExecutionReadingPosition,
  rememberExecutionReasoningChoice,
  type ReasoningDisclosureChoice,
} from "@/lib/execution-disclosure";

interface ExecutionProcessProps {
  sessionId?: string;
  automationStatus?: AutomationStatus;
  messageId?: string;
  parts: MessagePart[];
  active: boolean;
  settled?: boolean;
  autoExpand?: boolean;
  onReview?: () => void;
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

function ReasoningDisclosure({ parts, active, markdownConfig, theme, savedChoice, onChange }: {
  parts: ReasoningPart[];
  active: boolean;
  markdownConfig?: MarkdownConfig;
  theme: "light" | "dark";
  savedChoice?: ReasoningDisclosureChoice;
  onChange: (choice: ReasoningDisclosureChoice) => void;
}) {
  const [choice, setChoice] = useState(savedChoice ?? { open: false });
  const { open } = choice;
  const change = (next: ReasoningDisclosureChoice) => {
    setChoice(next);
    onChange(next);
  };
  const text = parts.map(part => part.text).join("\n\n");
  return <details className="execution-process__reasoning" open={open}>
    <summary onClick={event => { event.preventDefault(); change({ open: !open }); }}>分析详情</summary>
    {open && <div className="execution-process__reasoning-body">
      <Markdown complete={!active} markdownTheme="reasoning" theme={theme}
        config={markdownConfig}>{text}</Markdown>
    </div>}
  </details>;
}

function StageTools({ parts, recoveredById, onOpenTool, active, execution }: {
  parts: ToolPart[];
  recoveredById: Map<string, string>;
  onOpenTool?: (tool: ToolCallView) => void;
  active: boolean;
  execution: ReadonlyMap<string, ToolExecutionState>;
}) {
  // Records have one disclosure level. Every operation is immediately reachable
  // after opening records, without a second "view operations" toggle.
  return <div className={`execution-process__tool-group${parts.length === 1 ? " execution-process__tool" : ""}`}>
    {parts.map(({ toolCall }) => <div key={toolCall.toolCallId} className="execution-process__tool">
      <ToolCallCard tc={toolCall} onOpen={onOpenTool} showStatusLabel executionEnded={!active} executionState={execution.get(toolCall.toolCallId)} />
      {recoveredById.has(toolCall.toolCallId) && <span className="execution-process__recovery-detail">该次失败已通过后续重试恢复</span>}
    </div>)}
  </div>;
}

function hasRecordSelection(body: HTMLDivElement): boolean {
  const selection = body.ownerDocument.getSelection();
  return Boolean(selection?.rangeCount && !selection.isCollapsed && selection.getRangeAt(0).intersectsNode(body));
}

/** Task progress first; one disclosure for the complete chronological record. */
export function ExecutionProcess({
  sessionId, messageId, automationStatus, parts, active, settled, autoExpand = false, onReview, startedAt, completedAt,
  lastActivityAt, stopReason, cancelTrigger, cancellationCategory, agentResult,
  resultNotice, hasFinalAnswer = false, waitingReason, markdownConfig, findQuery, onOpenTool,
  knowledgeTrace, onOpenKnowledgePath, revealRequest = 0,
}: ExecutionProcessProps) {
  const { theme } = useTheme();
  const bodyId = useId();
  const detailsRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const headerRef = useRef<HTMLButtonElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const [heldActivities, setHeldActivities] = useState<Activity[]>([]);
  const restoreReadingTop = useRef(getExecutionReadingPosition(sessionId, messageId));
  const [reviewing, setReviewing] = useState(restoreReadingTop.current !== undefined);
  const reviewingRef = useRef(reviewing);
  const lastScrollTop = useRef(0);
  const followWhileBodyFocused = useRef(false);
  const summary = useMemo(() => summarizeExecutionProcess(parts, active, stopReason,
    cancellationCategory, cancelTrigger, { settled }),
  [active, cancellationCategory, cancelTrigger, parts, settled, stopReason]);
  const hasUnfinishedTools = summary.unfinishedToolCount > 0;
  const finalAnswerStarted = hasFinalAnswer && !waitingReason && !hasUnfinishedTools
    && knowledgeTrace?.personal?.state !== "searching";
  const shouldOpenAutomatically = active && autoExpand && (Boolean(waitingReason) || !finalAnswerStarted);
  const savedChoice = getExecutionDisclosureChoice(sessionId, messageId);
  const reasoningChoices = getExecutionReasoningChoices(sessionId, messageId);
  const [open, setOpen] = useState(savedChoice ?? (reviewing || shouldOpenAutomatically));
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

  const updateReviewing = useCallback((next: boolean) => {
    if (!next) restoreReadingTop.current = undefined;
    rememberExecutionReadingPosition(sessionId, messageId, next ? bodyRef.current?.scrollTop : undefined);
    if (reviewingRef.current === next) return;
    reviewingRef.current = next;
    setReviewing(next);
    if (next) onReview?.();
  }, [messageId, onReview, sessionId]);

  const hasReadingInteraction = useCallback(() => {
    const body = bodyRef.current;
    if (!body) return false;
    const focused = body.ownerDocument.activeElement;
    return (detailsRef.current?.contains(focused) && !(focused === body && followWhileBodyFocused.current))
      || hasRecordSelection(body);
  }, []);

  const scrollToLatest = useCallback(() => {
    const body = bodyRef.current;
    if (!body) return;
    body.scrollTop = body.scrollHeight - body.clientHeight;
    lastScrollTop.current = body.scrollTop;
  }, []);

  const followLatest = useCallback(() => {
    if (reviewingRef.current) return;
    if (hasReadingInteraction()) updateReviewing(true);
    else scrollToLatest();
  }, [hasReadingInteraction, scrollToLatest, updateReviewing]);

  const alignRecords = useCallback(() => {
    const target = restoreReadingTop.current;
    if (target === undefined) {
      if (active) followLatest();
      return;
    }
    const body = bodyRef.current;
    if (!body) return;
    body.scrollTop = target;
    lastScrollTop.current = body.scrollTop;
    // Markdown images can extend the scroll range after the initial render.
    if (body.scrollTop >= target - 1) restoreReadingTop.current = undefined;
  }, [active, followLatest]);

  const cancelPendingRestore = () => {
    if (restoreReadingTop.current === undefined) return;
    restoreReadingTop.current = undefined;
    updateReviewing(true);
  };

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

  useLayoutEffect(() => {
    const beganNewRun = active && !previousActive.current;
    const endedRun = !active && previousActive.current;
    if (beganNewRun) {
      userToggled.current = false;
      updateReviewing(false);
      setResumedAt(Date.now());
      forgetExecutionDisclosureChoice(sessionId, messageId);
    }
    if (!userToggled.current && (beganNewRun || endedRun || previousAutomaticOpen.current !== shouldOpenAutomatically)) {
      if (shouldOpenAutomatically) setOpen(true);
      else if (hasReadingInteraction()) updateReviewing(true);
      else if (!reviewingRef.current) {
        const body = bodyRef.current;
        if (body && body === body.ownerDocument.activeElement) headerRef.current?.focus({ preventScroll: true });
        setOpen(false);
      }
    }
    previousActive.current = active;
    previousAutomaticOpen.current = shouldOpenAutomatically;
  }, [active, hasReadingInteraction, messageId, sessionId, shouldOpenAutomatically, updateReviewing]);

  useLayoutEffect(() => {
    if (expanded && !searchMatch) alignRecords();
  }, [alignRecords, expanded, knowledgeTrace, parts, searchMatch]);

  useLayoutEffect(() => {
    const body = bodyRef.current;
    const content = contentRef.current;
    if ((!active && restoreReadingTop.current === undefined) || !expanded || searchMatch
      || !body || !content || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(alignRecords);
    observer.observe(body);
    observer.observe(content);
    return () => observer.disconnect();
  }, [active, alignRecords, expanded, searchMatch]);

  const stages = useMemo(() => executionStages(parts), [parts]);
  const currentStage = stages[stages.length - 1];
  const progress = useMemo(() => executionProgressText(currentStage?.progress ?? []), [currentStage]);
  const tools = useMemo(() => parts.flatMap(part => part.kind === "tool_call" ? [part.toolCall] : []), [parts]);
  const toolStates = useMemo(() => executionToolStates(tools, !active), [tools, active]);
  const activities = useMemo(() => executionActivities(parts, heldActivities), [parts, heldActivities]);
  const recentActivities = activities.filter((activity, index) => index >= activities.length - 3
    || heldActivities.some(held => held.id === activity.id)
    || active && activity.tools.some(tool => toolStates.get(tool.toolCallId)?.state === "in_progress"));
  const recoveredById = new Map(summary.recoveredTools.map(item => [item.failedToolCallId, item.successfulToolCallId]));
  const recoveredIds = new Set(recoveredById.keys());
  const uncertainToolCount = summary.pendingToolCount;
  const failedToolCount = summary.unresolvedToolCount;
  const recovered = summary.state === "complete" && recoveredIds.size > 0;
  const title = automationStatus ? computerTaskLabel(automationStatus) : !active ? resultNotice?.title ?? (summary.state === "complete" ? "本次处理已结束" : summary.title)
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
  const showPreview = !expanded && active && !finalAnswerStarted;
  // A focused action or selected text must not disappear at the answer boundary.
  const [retainedPreview, setRetainedPreview] = useState(false);
  const [readingDescription, setReadingDescription] = useState<string>();
  const updatePreviewInteraction = useCallback((focused?: EventTarget | null) => {
    const preview = previewRef.current;
    if (!preview) return;
    const target = focused === undefined ? preview.ownerDocument.activeElement : focused;
    const focusId = target instanceof Element
      ? target.closest<HTMLElement>("[data-execution-activity-id]")?.dataset.executionActivityId : undefined;
    const selection = preview.ownerDocument.getSelection();
    const range = selection?.rangeCount && !selection.isCollapsed ? selection.getRangeAt(0) : undefined;
    const selecting = hasRecordSelection(preview);
    const selectedIds = new Set(Array.from(preview.querySelectorAll<HTMLElement>("[data-execution-activity-id]"))
      .filter(row => range?.intersectsNode(row)).map(row => row.dataset.executionActivityId));
    const held = activities.filter(activity => activity.id === focusId || selectedIds.has(activity.id));
    setHeldActivities(previous => previous.length === held.length && previous.every((activity, index) => activity.id === held[index].id)
      ? previous : held);
    const focusedInside = target instanceof Node && preview.contains(target);
    setRetainedPreview(selecting || focusedInside);
    const excerpt = preview.querySelector(".execution-process__excerpt");
    setReadingDescription(previous => excerpt && range?.intersectsNode(excerpt) ? previous ?? description : undefined);
    if (selecting || focusedInside) onReview?.();
  }, [activities, description, onReview]);
  useEffect(() => {
    if (expanded || !showPreview && !retainedPreview) return;
    const update = () => updatePreviewInteraction();
    document.addEventListener("selectionchange", update);
    return () => document.removeEventListener("selectionchange", update);
  }, [expanded, retainedPreview, showPreview, updatePreviewInteraction]);
  const previewDescription = readingDescription ?? description;
  const showProgress = (showPreview || retainedPreview) && Boolean(previewDescription);
  const meta = [stageOperationSummary(tools, recoveredIds, !active, toolStates)];
  if (summary.changedFiles.length) meta.push(`${summary.changedFiles.length} 个文件已更新`);
  const documentSummary = documentProgressSummary(tools.flatMap(tool => tool.documentProgress ? [tool.documentProgress] : []));
  if (documentSummary) meta.unshift(documentSummary);
  if (knowledgeTrace?.personal?.state === "used") meta.push(`${knowledgeTrace.personal.resultCount} 个知识片段`);
  const changeOpen = (next: boolean) => {
    userToggled.current = true;
    if (!next) updateReviewing(false);
    setReadingDescription(undefined);
    setRetainedPreview(false);
    setHeldActivities([]);
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
    {!expanded && (showPreview || retainedPreview) && (showProgress || recentActivities.length > 0) && <div ref={previewRef} className="execution-process__live" data-chat-reading="true"
      onFocusCapture={event => updatePreviewInteraction(event.target)}
      onBlurCapture={event => updatePreviewInteraction(event.relatedTarget)}>
      {showProgress && <div className="execution-process__excerpt" data-stage-id={currentStage?.id} aria-label="当前阶段进展">
        <p>{previewDescription}</p>
      </div>}
      {recentActivities.length > 0 && <ol className="execution-process__activities" aria-label="最近执行进展">
        {recentActivities.map(activity => <ExecutionActivity key={activity.id} activity={activity}
          onOpenTool={onOpenTool} executionEnded={!active} execution={toolStates} onOpenRecords={() => {
            changeOpen(true);
            headerRef.current?.focus({ preventScroll: true });
          }} />)}
      </ol>}
      {activities.length > recentActivities.length && <p className="execution-process__earlier">
        还有 {activities.filter(activity => !recentActivities.includes(activity)).reduce((count, activity) => count + activity.tools.length, 0)} 项较早操作，可展开查看完整记录
      </p>}
    </div>}
    {(expanded || !active || documentSummary) && meta.some(Boolean) && <p className="execution-process__metrics">{meta.filter(Boolean).join(" · ")}</p>}
    {!expanded && knowledgeTrace && <KnowledgeTrace trace={knowledgeTrace} active={active} compact onOpenPath={onOpenKnowledgePath} />}
    {!expanded && active && (failedToolCount > 0 || uncertainToolCount > 0) && <p className="execution-process__attention" role="note">
      {[failedToolCount ? `${failedToolCount} 项操作失败` : "", uncertainToolCount ? `${uncertainToolCount} 项操作结果待确认` : ""].filter(Boolean).join(" · ")}，{waitingReason ? "等待你处理后继续" : "执行仍在继续"}。
      <button type="button" onClick={() => { changeOpen(true); headerRef.current?.focus({ preventScroll: true }); }}>查看记录</button>
    </p>}
    {longWait && <p className="execution-process__waiting-note" role="note">
      已 {formatProcessDuration(idleMs)} 未收到新进展，正在等待{waitingFor}返回。可继续等待，或停止本次执行。
    </p>}
    {expanded && <div className="execution-process__details" ref={detailsRef}>
      {automationStatus && <ComputerExecutionDetails status={automationStatus} />}
      <div className="execution-process__body" id={bodyId} ref={bodyRef}
        data-chat-reading="true" tabIndex={0} role="region" aria-label="执行过程详情"
        onFocus={cancelPendingRestore}
        onPointerDown={cancelPendingRestore}
        onBlur={event => {
          if (event.target === event.currentTarget) followWhileBodyFocused.current = false;
        }}
        onScroll={event => {
          const body = event.currentTarget;
          const movedDown = body.scrollTop > lastScrollTop.current;
          lastScrollTop.current = body.scrollTop;
          if (searchMatch || restoreReadingTop.current !== undefined) return;
          if (!isNearScrollBottom(body)) updateReviewing(true);
          else if (movedDown && !hasRecordSelection(body)) {
            followWhileBodyFocused.current = body === body.ownerDocument.activeElement;
            updateReviewing(false);
          }
        }}
        onWheel={event => {
          cancelPendingRestore();
          if (event.deltaY < 0 && event.currentTarget.scrollTop > 0) updateReviewing(true);
        }}
        onKeyDown={event => {
          const body = event.currentTarget;
          if (event.target !== body || event.defaultPrevented || event.altKey || event.metaKey || event.ctrlKey
            || (event.key !== "ArrowUp" && event.key !== "ArrowDown") || body.scrollHeight <= body.clientHeight) return;
          cancelPendingRestore();
          event.preventDefault();
          const up = event.key === "ArrowUp";
          if (up && body.scrollTop > 0) updateReviewing(true);
          const lineHeight = parseFloat(getComputedStyle(body).lineHeight) || 24;
          body.scrollTop += (up ? -1 : 1) * lineHeight;
          if (!up && isNearScrollBottom(body) && !hasRecordSelection(body)) {
            followWhileBodyFocused.current = true;
            updateReviewing(false);
          }
        }}
        onClick={event => {
          if (active && event.target instanceof Element && event.target.closest("summary")) {
            updateReviewing(true);
          }
        }}>
        <div ref={contentRef}>
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
              const state = executionStageState(stage, index === stages.length - 1, active, Boolean(waitingReason), recoveredIds, toolStates);
              return <li key={stage.id} data-stage-id={stage.id} data-stage-state={state}
                className={`execution-process__stage${stage.planned ? "" : " execution-process__stage--activity"}${active && index === stages.length - 1 ? " execution-process__stage--current" : ""}`}>
                <div className="execution-process__stage-heading">
                  <h4><span className="execution-process__stage-index" aria-hidden="true">{stageNumbers[index] ?? "·"}</span>{headingIsProgress && headingPart ? headingPart.text.trim() : stage.title.replace(/^正在/, "")}</h4>
                  <span className="execution-process__stage-state">{state}</span>
                </div>
                {rows.map((row, rowIndex) => {
                  const rowActive = active && index === stages.length - 1 && rowIndex === rows.length - 1;
                  if (row.kind === "thought") {
                    const rowId = `${stage.id}:${row.index}`;
                    return <ReasoningDisclosure key={row.index} parts={row.parts} active={rowActive}
                      markdownConfig={markdownConfig} theme={theme} savedChoice={reasoningChoices?.get(rowId)}
                      onChange={choice => rememberExecutionReasoningChoice(sessionId, messageId, rowId, choice)} />;
                  }
                  if (row.kind === "tools") return <StageTools key={row.index} parts={row.parts} recoveredById={recoveredById} onOpenTool={onOpenTool} active={active} execution={toolStates} />;
                  if (headingIsProgress && row.part === headingPart && !findQuery) return null;
                  return <div className="execution-process__progress" key={row.index}>
                    <Markdown complete={!rowActive} theme={theme} config={markdownConfig}
                      findQuery={row.part.phase !== "commentary" ? findQuery : undefined}>{row.part.text}</Markdown>
                  </div>;
                })}
              </li>;
            })}
          </ol>
        </div>
      </div>
      {(summary.toolCount > 0 && onOpenTool || reviewing) && <div className="execution-process__footer">
        {summary.toolCount > 0 && onOpenTool && <p className="execution-process__hint">选择操作可在右侧查看输入、输出和文件变更</p>}
        {reviewing && <button type="button" className="execution-process__latest" aria-controls={bodyId}
          title="回到最新进展并恢复执行记录跟随"
          onClick={() => {
            const body = bodyRef.current;
            if (!body) return;
            if (hasRecordSelection(body)) body.ownerDocument.getSelection()?.removeAllRanges();
            body.focus({ preventScroll: true });
            followWhileBodyFocused.current = true;
            updateReviewing(false);
            scrollToLatest();
          }}>回到最新进展</button>}
      </div>}
    </div>}
  </section>;
}

function KnowledgeTrace({
  trace,
  active,
  onOpenPath,
  compact = false,
}: {
  trace: KnowledgeTurnTrace;
  active: boolean;
  onOpenPath?: (path: string) => void;
  compact?: boolean;
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
          {personal.keywordOnly ? "已按关键词搜索" : "已搜索"} {personal.sourceCount} 个个人知识源，未找到相关内容
        </div>
      )}
      {(personal?.state === "used" || personal?.state === "no-match") && personal.notice && (
        <div className="knowledge-trace__status is-warning">{personal.notice}</div>
      )}
      {personal?.state === "blocked" && (
        <div className="knowledge-trace__status is-warning">个人知识未使用：{personal.message}</div>
      )}
      {personal?.state === "error" && (
        <div className="knowledge-trace__status is-warning">个人知识检索失败：{personal.message}</div>
      )}
      {personal?.state === "used" && compact && <div className="knowledge-trace__status">
        已向模型提供 {personal.resultCount} 个相关片段 · 来自 {personal.sourceCount} 个知识源
      </div>}
      {personal?.state === "used" && !compact && (
        <details className="knowledge-trace__results" open>
          <summary>
            已向模型提供 {personal.resultCount} 个相关片段
            <small>来自 {personal.sourceCount} 个知识源{personal.keywordOnly && " · 关键词检索"}</small>
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
