import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";
import { Markdown, type MarkdownConfig } from "./markdown/index";
import { ToolCallCard } from "./ToolCallCard";
import { documentProgressSummary } from "@/lib/document-progress";
import { executionProcessPreview } from "@/lib/execution-process-preview";
import { executionStages, stageOperationSummary } from "@/lib/execution-stages";
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
  settled?: boolean;
  autoExpand?: boolean;
  startedAt?: number;
  completedAt?: number;
  stopReason?: string;
  cancelTrigger?: string;
  cancellationCategory?: string;
  agentResult?: string;
  hasFinalAnswer?: boolean;
  waitingReason?: string;
  markdownConfig?: MarkdownConfig;
  onOpenTool?: (tool: ToolCallView) => void;
  knowledgeTrace?: KnowledgeTurnTrace;
  onOpenKnowledgePath?: (path: string) => void;
  revealRequest?: number;
}

type ReasoningPart = Extract<MessagePart, { kind: "thought" }>;
type ProcessRow =
  | { kind: "thought"; parts: ReasoningPart[]; index: number }
  | { kind: "part"; part: Exclude<MessagePart, ReasoningPart>; index: number };

/** Keep commentary, operations and disclosures in their original event order. */
function groupStageRows(parts: MessagePart[]): ProcessRow[] {
  const rows: ProcessRow[] = [];
  parts.forEach((part, index) => {
    if (part.kind !== "tool_call" && !part.text.trim()) return;
    if (part.kind === "thought") {
      const previous = rows[rows.length - 1];
      if (previous?.kind === "thought") previous.parts.push(part);
      else rows.push({ kind: "thought", parts: [part], index });
    } else rows.push({ kind: "part", part, index });
  });
  return rows;
}

interface ReasoningDisclosureProps {
  parts: ReasoningPart[];
  active: boolean;
  markdownConfig?: MarkdownConfig;
  theme: "light" | "dark";
}

function ReasoningDisclosure({ parts, active, markdownConfig, theme }: ReasoningDisclosureProps) {
  const [open, setOpen] = useState(false);
  return (
    <details className="execution-process__reasoning" open={open}>
      <summary
        onClick={(event) => {
          // Keep the disclosure controlled so streamed parent renders cannot
          // overwrite an explicit user choice.
          event.preventDefault();
          setOpen((value) => !value);
        }}
      >
        思考摘要
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

function ExecutionExcerpt({ text }: { text: string }) {
  const textRef = useRef<HTMLParagraphElement>(null);
  const followLatest = () => {
    const element = textRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  };
  useLayoutEffect(followLatest, [text]);
  useLayoutEffect(() => {
    if (!textRef.current) return;
    // Width and font changes can wrap the same excerpt into more lines.
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(followLatest);
    observer?.observe(textRef.current);
    // WebKit can coalesce a quick font down/up into the same final box size.
    // Its intermediate layout still clamps scrollTop, without a resize event.
    const rootObserver = typeof MutationObserver === "undefined" ? undefined : new MutationObserver(followLatest);
    rootObserver?.observe(document.documentElement, { attributes: true, attributeFilter: ["style", "class"] });
    window.addEventListener("resize", followLatest);
    return () => {
      observer?.disconnect();
      rootObserver?.disconnect();
      window.removeEventListener("resize", followLatest);
    };
  }, []);
  return <div className="execution-process__excerpt">
    <p ref={textRef}>{text}</p>
  </div>;
}

function ExecutionPreview({ parts, toolCount, searchingKnowledge, onOpenTool, onExpand, headerRef }: {
  parts: MessagePart[];
  toolCount: number;
  searchingKnowledge: boolean;
  onOpenTool?: (tool: ToolCallView) => void;
  onExpand: () => void;
  headerRef: RefObject<HTMLButtonElement>;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [focusedToolCallId, setFocusedToolCallId] = useState<string>();
  const [toolsOpen, setToolsOpen] = useState(false);
  const toolsId = useId();
  const latestStage = useMemo(() => executionStages(parts).slice(-1)[0], [parts]);
  const progress = useMemo(() => executionProcessPreview(latestStage?.progress ?? []), [latestStage]);
  const allTools = useMemo(() => parts.flatMap(part => part.kind === "tool_call" ? [part.toolCall] : []), [parts]);
  const preview = useMemo(() => executionProcessPreview(parts, focusedToolCallId), [parts, focusedToolCallId]);
  useLayoutEffect(() => {
    const container = containerRef.current;
    return () => {
      // Answer/completion transitions remove the preview, but must not drop a
      // keyboard user's focus or steal focus back from an already-open drawer.
      if (container?.contains(document.activeElement)) headerRef.current?.focus({ preventScroll: true });
    };
  }, [headerRef]);

  return <div ref={containerRef} className="execution-process__preview" aria-label="实时执行进展">
    {progress.excerpt && <ExecutionExcerpt text={progress.excerpt} />}
    {!progress.excerpt && !toolCount && !searchingKnowledge && <button type="button" className="execution-process__preview-more" onClick={onExpand}>查看思考摘要</button>}
    {searchingKnowledge && <p className="execution-process__preview-note">正在从个人知识库检索相关片段…</p>}
    {toolCount > 0 && <button type="button" className="execution-process__operations-toggle"
      aria-expanded={toolsOpen} aria-controls={toolsOpen ? toolsId : undefined}
      onClick={() => setToolsOpen(value => !value)}>
      <span aria-hidden="true">{toolsOpen ? "⌄" : "›"}</span> {stageOperationSummary(allTools)}
    </button>}
    {toolsOpen && preview.tools.length > 0 && <div id={toolsId} className="execution-process__recent-tools"
      onFocusCapture={event => {
        const id = (event.target as HTMLElement).closest<HTMLElement>("[data-tool-call-id]")?.dataset.toolCallId;
        if (id) setFocusedToolCallId(id);
      }}
      onBlurCapture={event => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocusedToolCallId(undefined);
      }}
    >
      {preview.tools.map(tool => <ToolCallCard key={tool.toolCallId} tc={tool} onOpen={onOpenTool} showStatusLabel />)}
    </div>}
    {toolsOpen && preview.hiddenToolCount > 0 && <button type="button" className="execution-process__preview-more" onClick={onExpand}>
      {preview.hiddenRunningCount > 0 ? `另有 ${preview.hiddenRunningCount} 项操作正在进行 · ` : ""}
      共 {toolCount} 项操作，展开查看全部
    </button>}
  </div>;
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
  settled,
  autoExpand = false,
  startedAt,
  completedAt,
  stopReason,
  cancelTrigger,
  cancellationCategory,
  agentResult,
  hasFinalAnswer = false,
  waitingReason,
  markdownConfig,
  onOpenTool,
  knowledgeTrace,
  onOpenKnowledgePath,
  revealRequest = 0,
}: ExecutionProcessProps) {
  const { theme } = useTheme();
  const bodyId = useId();
  const headerRef = useRef<HTMLButtonElement>(null);
  const summary = useMemo(
    () => summarizeExecutionProcess(
      parts,
      active,
      stopReason,
      cancellationCategory,
      cancelTrigger,
      { settled },
    ),
    [active, cancellationCategory, cancelTrigger, parts, settled, stopReason],
  );
  const hasUnfinishedTools = parts.some((part) =>
    part.kind === "tool_call" && part.toolCall.status === "in_progress",
  );
  // The answer can contain interim text while parallel tools are still active.
  // Keep both the status and automatic disclosure live until those operations
  // have ended, even if an embedding caller reports visible answer text.
  const finalAnswerStarted = hasFinalAnswer && !waitingReason
    && !hasUnfinishedTools
    && knowledgeTrace?.personal?.state !== "searching";
  // Completed turns keep a neutral disclosure. Diagnostics remain opt-in;
  // actionable result notices are rendered beside the answer.
  const shouldOpenAutomatically = autoExpand && active && !finalAnswerStarted;
  const savedChoice = getExecutionDisclosureChoice(sessionId, messageId);
  const [open, setOpen] = useState(savedChoice ?? shouldOpenAutomatically);
  const [now, setNow] = useState(() => Date.now());
  const previousActive = useRef(active);
  const previousAutomaticOpen = useRef(shouldOpenAutomatically);
  const userToggled = useRef(savedChoice !== undefined);

  useEffect(() => {
    if (!revealRequest) return;
    userToggled.current = true;
    setOpen(true);
    rememberExecutionDisclosureChoice(sessionId, messageId, true);
    headerRef.current?.focus({ preventScroll: true });
    headerRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [revealRequest, sessionId, messageId]);

  useLayoutEffect(() => {
    if (startedAt == null || !active) return;
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

  const stages = useMemo(() => executionStages(parts), [parts]);
  let plannedStageNumber = 0;
  const stageNumbers = stages.map(stage => stage.planned ? ++plannedStageNumber : undefined);
  const showPreview = active && !finalAnswerStarted && !open;
  const duration = startedAt != null && (active || completedAt != null)
    ? formatProcessDuration(Math.max(0, (completedAt ?? now) - startedAt))
    : null;
  const meta: string[] = [];
  if (summary.toolCount > 0) {
    meta.push(active
      ? `${summary.completedToolCount + summary.failedToolCount}/${summary.toolCount} 项`
      : `${summary.toolCount} 项操作`);
  } else if (summary.thoughtCount > 0) {
    meta.push(`${summary.thoughtCount} 段思考过程`);
  }
  if (summary.changedFiles.length > 0) meta.push(`${summary.changedFiles.length} 个文件`);
  const documentSummary = documentProgressSummary(parts.flatMap(part =>
    part.kind === "tool_call" && part.toolCall.documentProgress ? [part.toolCall.documentProgress] : []));
  if (documentSummary) meta.unshift(documentSummary);
  if (knowledgeTrace?.personal?.state === "used") {
    meta.unshift(`${knowledgeTrace.personal.resultCount} 个知识片段`);
  }
  const title = !active ? "查看过程" : waitingReason ?? (knowledgeTrace?.personal?.state === "searching"
    ? "正在检索个人知识库"
    : active && finalAnswerStarted
      ? "正在生成回复"
      : stages.slice(-1)[0]?.parts.some(part => part.stageTitle || part.kind === "text")
        ? stages[stages.length - 1].title : summary.title);
  const recovered = summary.state === "complete" && summary.recoveredTools.length > 0;
  const recoveredById = new Map(summary.recoveredTools.map(recovery => [recovery.failedToolCallId,recovery.successfulToolCallId]));
  const changeOpen = (next: boolean) => {
    userToggled.current = true;
    rememberExecutionDisclosureChoice(sessionId, messageId, next);
    setOpen(next);
  };

  return (
    <section className={`execution-process execution-process--${waitingReason && active ? "waiting" : summary.state}${showPreview ? " execution-process--previewing" : ""}${recovered ? " execution-process--recovered" : ""}`}>
      <button
        ref={headerRef}
        type="button"
        className="execution-process__header"
        aria-label={[title, duration].filter(Boolean).join(" · ")}
        aria-expanded={open}
        aria-controls={open ? bodyId : undefined}
        aria-describedby={open && summary.description ? `${bodyId}-summary` : undefined}
        onClick={() => changeOpen(!open)}
      >
        <span className="execution-process__status" aria-hidden="true" />
        <span className="execution-process__heading">
          <span className="execution-process__title" title={title} role={finalAnswerStarted ? undefined : "status"}>
            {title}
          </span>
          {duration && (
            <span className="execution-process__meta">· {duration}</span>
          )}
        </span>
        {active && <span className="execution-process__toggle-label" aria-hidden="true">{open ? "收起过程" : "展开过程"}</span>}
        <span
          className={`execution-process__chevron${open ? " execution-process__chevron--open" : ""}`}
          aria-hidden="true"
        >
          ›
        </span>
      </button>

      {showPreview && <ExecutionPreview parts={parts} toolCount={summary.toolCount}
        searchingKnowledge={knowledgeTrace?.personal?.state === "searching"}
        onOpenTool={onOpenTool} onExpand={() => changeOpen(true)} headerRef={headerRef} />}

      {open && (
        <div className="execution-process__body" id={bodyId}>
          {meta.length > 0 && <p className="execution-process__hint">{meta.join(" · ")}</p>}
          {!active && <p className="execution-process__diagnostic-title">{summary.title}</p>}
          {summary.description && <p id={`${bodyId}-summary`} className="execution-process__outcome-detail">{summary.description}</p>}
          {summary.recoveredTools.length > 0 && <p className="execution-process__recovery-detail">
            {summary.recoveredTools.length} 次中间失败已由后续同一操作的重试恢复，原始记录保留如下。
          </p>}
          {!active && agentResult && (
            <p className="execution-process__terminal-detail">
              {agentResult}
            </p>
          )}
          {knowledgeTrace && (
            <KnowledgeTrace
              trace={knowledgeTrace}
              onOpenPath={onOpenKnowledgePath}
            />
          )}
          <ol className="execution-process__stages" aria-label="执行记录">
            {stages.map((stage, index) => {
              const rows = groupStageRows(stage.parts);
              return <li key={stage.id} className={`execution-process__stage${stage.planned ? "" : " execution-process__stage--activity"}`}>
                <h4><span className="execution-process__stage-index" aria-hidden="true">{stageNumbers[index] ?? "·"}</span>{stage.title.replace(/^正在/, "")}</h4>
                {stage.tools.length > 0 && <p className="execution-process__stage-operations">{stageOperationSummary(stage.tools)}</p>}
                {rows.map((row, rowIndex) => {
                  const rowActive = active && index === stages.length - 1 && rowIndex === rows.length - 1;
                  if (row.kind === "thought") return <ReasoningDisclosure key={row.index} parts={row.parts} active={rowActive}
                    markdownConfig={markdownConfig} theme={theme} />;
                  const part = row.part;
                  if (part.kind === "text") return <div className="execution-process__progress" key={row.index}>
                    <Markdown complete={!rowActive} theme={theme} config={markdownConfig}>{part.text}</Markdown>
                  </div>;
                  return <div key={row.index} className="execution-process__tool">
                    <ToolCallCard tc={part.toolCall} onOpen={onOpenTool} showStatusLabel />
                    {recoveredById.has(part.toolCall.toolCallId) && <span className="execution-process__recovery-detail">该次失败已通过后续重试恢复</span>}
                  </div>;
                })}
              </li>;
            })}
          </ol>

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
