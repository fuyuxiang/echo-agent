import { memo, useCallback, useEffect, useId, useRef, useState, useMemo, type ReactNode } from "react";
import {
  Check,
  Pencil,
  FileText,
  MoreHorizontal,
  Copy,
  Download,
  FileCode2,
  RefreshCw,
  Square,
  ThumbsDown,
  ThumbsUp,
  Volume2,
} from "lucide-react";
import { Markdown, type MarkdownConfig } from "./markdown/index";
import { LoadingRow } from "./LoadingRow";
import { EchoMark } from "./EchoMark";
import { ExecutionProcess } from "./ExecutionProcess";
import { ActionMenu } from "./ActionMenu";
import { FeedbackDialog } from "./FeedbackDialog";
import { AttachmentVisual } from "./AttachmentVisual";
import { useTheme } from "./ThemeProvider";
import { useFeedbackStore, type FeedbackRating } from "@/stores/feedback-store";
import type { ChatMessage, ToolCallView } from "@/stores/session-store";
import { openLocalPath } from "@/lib/agent-client";
import {
  attachmentBasename,
  isImageAttachment,
  stripInjectedUserContext,
} from "@/lib/user-message";
import { friendlyAttachmentError } from "@/lib/attachment-errors";
import { highlightSegments } from "@/lib/extract-text";
import { copyShareText } from "@/lib/share";
import { exportOfficeDocument, type OfficeFormat } from "@/lib/document-export";
import type { SessionArtifact } from "@/lib/session-artifacts";
import { registerDocumentArtifact } from "@/lib/artifact-catalog";
import { isExecutionSettled, partitionAssistantParts } from "@/lib/execution-process";
import { executionContinuationPrompt, getExecutionNotice } from "@/lib/execution-notice";
import {
  messageRetryKind,
  messageRetryLabel,
  messageRetryTitle,
  type MessageRetryKind,
} from "@/lib/message-retry";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import {
  createWebSpeechTtsProvider,
  getActiveTts,
  registerTtsProvider,
} from "@/lib/voice-contract";

let webSpeechTtsRegistered = false;

function ensureWebSpeechTtsRegistered(): void {
  if (webSpeechTtsRegistered || typeof window === "undefined") return;
  webSpeechTtsRegistered = true;
  if (!("speechSynthesis" in window) || typeof SpeechSynthesisUtterance === "undefined") return;
  registerTtsProvider(createWebSpeechTtsProvider({
    isAvailable: () => "speechSynthesis" in window && typeof SpeechSynthesisUtterance !== "undefined",
    synth: window.speechSynthesis as never,
    createUtterance: (text, lang, opts) => {
      const utterance = new SpeechSynthesisUtterance(text);
      utterance.lang = lang;
      utterance.rate = opts?.rate ?? 1;
      utterance.pitch = opts?.pitch ?? 1;
      utterance.onend = () => opts?.onEnd?.();
      utterance.onerror = () => opts?.onError?.();
      return utterance as never;
    },
  }));
}

function speechText(markdown: string): string {
  return markdown
    .replace(/```[\s\S]*?```/g, "代码块已省略。")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/[#>*_`~-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * Renders one chat message. Assistant messages are left-aligned with avatar +
 * name row; user messages are right-aligned bubbles with no avatar / name.
 *
 * Message action bar (对齐 EchoAgent):
 *  - user: 复制 / 编辑重发
 *  - assistant: rendered after the complete answer, so read-then-act follows
 *    the visual and keyboard order.
 */
export const MessageItem = memo(function MessageItem({
  message,
  streaming,
  waitingForModel = true,
  waitingReason,
  markdownConfig,
  cwd,
  sessionId,
  onOpenTool,
  onOpenArtifact,
  compactIdentity = false,
  onEditResend,
  onRetry,
  retrying = false,
  retryDisabled = false,
  retryDisabledReason,
  latest = false,
  onToast,
  findQuery,
  authorName,
  autoExpandProcess = false,
  runtimeError,
  suppressStoppedNotice = false,
  onContinueExecution,
  onOpenModelSettings,
  onOpenSecuritySettings,
  turnFeedback,
}: {
  message: ChatMessage;
  authorName?: string;
  compactIdentity?: boolean;
  onOpenArtifact?: (artifact: SessionArtifact) => void;
  streaming: boolean;
  /** Suppress model-wait copy while a decision, cancellation or error takes priority. */
  waitingForModel?: boolean;
  waitingReason?: string;
  autoExpandProcess?: boolean;
  runtimeError?: string;
  suppressStoppedNotice?: boolean;
  onContinueExecution?: (prompt: string) => boolean | void | Promise<boolean | void>;
  onOpenModelSettings?: () => void;
  onOpenSecuritySettings?: () => void;
  turnFeedback?: ReactNode;
  markdownConfig?: MarkdownConfig;
  /** Workspace used to resolve relative attachment paths. */
  cwd?: string;
  /** Current session id — needed to key feedback entries. */
  sessionId?: string;
  onToast?: (msg: string) => void;
  /** Open tool detail in the right-side panel (Phase 2). */
  onOpenTool?: (tc: ToolCallView) => void;
  /** Put text back into the composer for re-editing (user messages only). */
  onEditResend?: (text: string, attachments: string[]) => void;
  /** Retry the latest turn using semantics derived from whether it used tools. */
  onRetry?: (kind: MessageRetryKind) => void;
  /** Prevent duplicate retry gestures and expose progress in the local action. */
  retrying?: boolean;
  retryDisabled?: boolean;
  retryDisabledReason?: string;
  /** The latest completed assistant reply keeps its actions more discoverable. */
  latest?: boolean;
  /** 会话内查找的当前关键词,Markdown 渲染时把命中处染出。 */
  findQuery?: string;
}) {
  const { theme } = useTheme();
  const [speaking, setSpeaking] = useState(false);
  const [copiedKind, setCopiedKind] = useState<"plain" | "markdown" | null>(null);
  const [actionView, setActionView] = useState("main");
  const [feedbackDialog, setFeedbackDialog] = useState<FeedbackRating | null>(null);
  const feedback = useFeedbackStore(s => sessionId ? s.entries[`${sessionId}:${message.id}`]?.rating ?? null : null);
  const rateReply = (rating: FeedbackRating) => {
    if (!sessionId) return;
    useFeedbackStore.getState().setRating(sessionId, message.id, rating);
    if (feedback !== rating) setFeedbackDialog(rating);
  };
  const [exportedArtifact, setExportedArtifact] = useState<SessionArtifact | null>(null);
  const [exporting, setExporting] = useState<OfficeFormat | null>(null);
  const [processReveal, setProcessReveal] = useState(0);
  const [continuingExecution, setContinuingExecution] = useState(false);
  const continueGuard = useRef(false);
  const [continueError, setContinueError] = useState<string | null>(null);
  const outcome = useMemo(() => getExecutionNotice(message, streaming, runtimeError), [message, streaming, runtimeError]);
  const notice = outcome?.kind === "stopped" && suppressStoppedNotice ? null : outcome;
  const hasToolWork = message.parts.some((part) => part.kind === "tool_call");
  const continueAction = Boolean(notice && notice.kind !== "environment" && !notice.modelSettings && onContinueExecution && hasToolWork);
  const retryAction = Boolean(notice && notice.kind !== "environment" && !notice.modelSettings && !hasToolWork && onRetry);
  const continueExecution = async () => {
    if (!notice || !onContinueExecution || continueGuard.current || retryDisabled) return;
    continueGuard.current = true;
    setContinuingExecution(true);
    setContinueError(null);
    try {
      if (await onContinueExecution(executionContinuationPrompt(notice)) === false) {
        setContinueError("暂时无法继续，请检查当前任务状态后重试。");
      }
    } catch {
      setContinueError("暂时无法继续，已有内容已保留，请稍后重试。");
    } finally {
      continueGuard.current = false;
      setContinuingExecution(false);
    }
  };
  const knowledgeTrace = useKnowledgeStore((state) => (
    sessionId && message.promptId
      ? state.turnTraces[sessionId]?.[message.promptId]
      : undefined
  ));
  const stopSpeakingRef = useRef<(() => void) | null>(null);
  const copyResetTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const exportDescriptionId = useId();
  const exportSpreadsheetDescriptionId = useId();

  useEffect(() => () => {
    stopSpeakingRef.current?.();
    if (copyResetTimerRef.current) clearTimeout(copyResetTimerRef.current);
  }, []);

  const markCopied = useCallback((kind: "plain" | "markdown") => {
    setCopiedKind(kind);
    if (copyResetTimerRef.current) clearTimeout(copyResetTimerRef.current);
    copyResetTimerRef.current = setTimeout(() => {
      copyResetTimerRef.current = null;
      setCopiedKind(null);
    }, 1600);
  }, []);

  const copyText = useCallback(
    async (text: string, kind: "plain" | "markdown") => {
      try {
        let copied: boolean;
        try {
          copied = await copyShareText(text);
        } catch {
          // Desktop webviews can expose Clipboard API while denying a write.
          // Retry with the DOM fallback before surfacing an error.
          copied = await copyShareText(text, { clipboard: null });
        }
        if (!copied) throw new Error("clipboard unavailable");
        // Success feedback stays on the button. Avoid a global Shell update
        // that re-renders the entire transcript and overlays the composer.
        markCopied(kind);
      } catch {
        onToast?.("复制失败");
      }
    },
    [markCopied, onToast],
  );

  const assistantGroups = message.role === "assistant"
    ? partitionAssistantParts(message.parts, { organizeProgress: true })
    : null;
  const allTextParts = message.parts.filter(
    (part): part is Extract<typeof part, { kind: "text" }> => part.kind === "text",
  );
  const answerParts = assistantGroups ? assistantGroups.responseParts : allTextParts;

  /** Copy the user-visible answer by default; process details stay opt-in. */
  const plainText = (message.role === "assistant" ? answerParts : allTextParts)
    .map((part) => message.role === "user" ? stripInjectedUserContext(part.text) : part.text)
    .join("\n");

  /** Preserve Markdown syntax, but don't leak hidden process details into a normal copy. */
  const markdownText = answerParts.map((part) => part.text).join("\n\n");
  const hasAnswerText = plainText.trim().length > 0;
  // A preserved partial response can be commentary before later reasoning or
  // tools. Only trailing answer text signals that the model has moved on to
  // the final response; do not treat that compatibility fallback as completion.
  const hasFinalAnswer = assistantGroups?.answerStarted ?? false;
  const hasTerminalProcessStatus = Boolean(
    message.complete
      && message.stopReason
      && message.cancelTrigger !== "send_now"
      && (message.stopReason !== "end_turn" || message.cancellationCategory),
  );
  const hasKnowledgeTrace = Boolean(
    knowledgeTrace?.personal && knowledgeTrace.personal.state !== "idle"
      || knowledgeTrace?.organization?.state === "unavailable",
  );
  const hasExecutionProcess = Boolean(assistantGroups && (
    assistantGroups.processParts.length > 0 || hasTerminalProcessStatus || hasKnowledgeTrace || notice
  ));
  const generatingAnswer = streaming && waitingForModel && !message.complete && hasFinalAnswer
    && !assistantGroups?.processParts.some(part => part.kind === "tool_call" && part.toolCall.status === "in_progress")
    && knowledgeTrace?.personal?.state !== "searching";
  const retryKind = messageRetryKind(message);
  const retryLabel = messageRetryLabel(retryKind, retrying);
  const copyMainLabel = copiedKind === "plain"
    ? "已复制"
    : copiedKind === "markdown"
      ? "已复制 Markdown"
      : "复制纯文本";

  const toggleSpeak = useCallback(() => {
    if (speaking) {
      stopSpeakingRef.current?.();
      stopSpeakingRef.current = null;
      setSpeaking(false);
      return;
    }
    ensureWebSpeechTtsRegistered();
    const provider = getActiveTts();
    const text = speechText(plainText);
    if (!provider || !text) {
      onToast?.(provider ? "该回复没有可朗读文本" : "当前系统不支持语音朗读");
      return;
    }
    setSpeaking(true);
    stopSpeakingRef.current = provider.speak(text, "zh-CN", {
      onEnd: () => {
        stopSpeakingRef.current = null;
        setSpeaking(false);
      },
      onError: () => {
        stopSpeakingRef.current = null;
        setSpeaking(false);
        onToast?.("语音朗读失败");
      },
    });
  }, [onToast, plainText, speaking]);

  const exportAnswer = async (format: OfficeFormat) => {
    setExporting(format);
    try {
      const title = markdownText.match(/^#\s+(.+)$/m)?.[1]?.trim()
        || markdownText.split("\n").find((line) => line.trim())?.replace(/^#+\s*/, "").slice(0, 80)
        || "EchoAgent 报告";
      const receipt = await exportOfficeDocument(title, markdownText, format, sessionId ?? undefined);
      if (receipt) {
        if (sessionId) setExportedArtifact(registerDocumentArtifact(receipt, sessionId, title, cwd ?? ""));
        onToast?.(`已导出“${receipt.path.split(/[\\/]/).pop()}”${sessionId ? "，可在成果中查看" : ""}`);
      }
    } catch (error) {
      onToast?.(`导出失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setExporting(null);
    }
  };

  if (message.role === "user") {
    const attachments = message.attachments ?? [];
    return (
      <div className="msg msg--user">
        <div>
          <div className={"msg__bubble" + (attachments.length ? " msg__bubble--with-attachments" : "")}>
            {attachments.length > 0 && (
              <div className="msg__attachments" role="list" aria-label="附件">
                {attachments.map((path) => {
                  const name = attachmentBasename(path);
                  const image = isImageAttachment(path);
                  const extension = name.includes(".")
                    ? name.slice(name.lastIndexOf(".") + 1).toUpperCase()
                    : "FILE";
                  return (
                    <div key={path} className="msg__attachment-item" role="listitem">
                      <button
                        type="button"
                        className={"msg__attachment" + (image ? " msg__attachment--image" : "")}
                        title={path}
                        aria-label={`打开附件 ${name}`}
                        onClick={() => {
                          void openLocalPath(path, cwd).catch((error) => {
                            onToast?.(friendlyAttachmentError(error, path));
                          });
                        }}
                      >
                        <AttachmentVisual path={path} />
                        <span className="msg__attachment-copy">
                          <span className="msg__attachment-name">{name}</span>
                          <span className="msg__attachment-type">
                            {extension} {image ? "图片" : "文件"}
                          </span>
                        </span>
                      </button>
                    </div>
                  );
                })}
              </div>
            )}
            {message.parts.map((p, i) => {
              if (p.kind !== "text") return null;
              const visibleText = stripInjectedUserContext(p.text);
              return (
                <span key={i}>
                  {highlightSegments(visibleText, findQuery ?? "").map((segment, segmentIndex) =>
                    segment.hit
                      ? <mark className="find-hit" key={segmentIndex}>{segment.text}</mark>
                      : <span key={segmentIndex}>{segment.text}</span>
                  )}
                </span>
              );
            })}
          </div>
          {/* Hover actions */}
          <div className="msg__actions">
            <button
              type="button"
              className="msg__action-btn msg__action-btn--copy"
              data-chat-copy="true"
              onClick={() => void copyText(plainText, "plain")}
              title={copiedKind === "plain" ? "已复制" : "复制"}
              aria-label={copiedKind === "plain" ? "已复制" : "复制"}
              aria-live="polite"
            >
              {copiedKind === "plain" ? <Check size={14} /> : <Copy size={14} />}
            </button>
            {onEditResend && (
              <button
                type="button"
                className="msg__action-btn"
                onClick={() => onEditResend(plainText, attachments)}
                data-tip="编辑并重新发送" title="编辑并重新发送" aria-label="编辑并重新发送"
              >
                <Pencil size={14} />
              </button>
            )}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className={"msg msg--assistant" + (compactIdentity ? " msg--continuation" : "")}>
      <div>
        <div className="msg__header">
          <span className={`msg__avatar${authorName ? "" : " msg__avatar--echo"}`}>
            {authorName ? <span aria-hidden="true">{authorName.slice(0, 1)}</span> : (
              <EchoMark active={streaming && waitingForModel && !message.complete && !runtimeError} />
            )}
          </span>
          {authorName ? <span className="msg__name">{authorName}</span> : <span className="sr-only">EchoAgent</span>}
        </div>
        <div className="msg__body">
          {streaming && waitingForModel && !message.complete && message.parts.length === 0 && !hasKnowledgeTrace && (
            <LoadingRow key={`${sessionId}:${message.id}`} startedAt={message.startedAt} />
          )}
          {assistantGroups && hasExecutionProcess
            && (
              <ExecutionProcess
                sessionId={sessionId}
                messageId={message.id}
                parts={assistantGroups.processParts}
                active={streaming && !message.complete}
                settled={isExecutionSettled(message)}
                autoExpand={autoExpandProcess}
                startedAt={message.startedAt}
                completedAt={message.completedAt}
                stopReason={message.stopReason}
                cancelTrigger={message.cancelTrigger}
                cancellationCategory={message.cancellationCategory}
                agentResult={[...new Set([message.agentResult, runtimeError].filter(Boolean))].join("\n") || undefined}
                hasFinalAnswer={hasFinalAnswer}
                waitingReason={waitingReason}
                markdownConfig={markdownConfig}
                findQuery={findQuery}
                onOpenTool={onOpenTool}
                revealRequest={processReveal}
                knowledgeTrace={knowledgeTrace}
                onOpenKnowledgePath={(path) => {
                  void openLocalPath(path, cwd).catch((error) => {
                    onToast?.(`打开知识文件失败：${String(error).replace(/^Error:\s*/, "")}`);
                  });
                }}
              />
            )}
          {assistantGroups?.responseParts.map((part, index) => (
            <Markdown
              key={index}
              complete={message.complete}
              markdownTheme="loose"
              theme={theme}
              config={markdownConfig}
              findQuery={findQuery}
            >
              {part.text}
            </Markdown>
          ))}
          {generatingAnswer && (
            <div className="msg__writing" role="status"><span aria-hidden="true" />正在生成回复</div>
          )}
          {notice && <div className={`execution-notice execution-notice--${notice.tone}`} role={notice.tone === "neutral" ? "status" : "alert"}>
            <div className="execution-notice__copy"><strong>{notice.title}</strong><p>{notice.description}</p></div>
            <div className="execution-notice__actions">
              <button type="button" onClick={() => {
                if (notice.tools[0] && onOpenTool) onOpenTool(notice.tools[0]);
                else setProcessReveal((value) => value + 1);
              }}>{notice.tools.length ? "查看相关操作" : "查看原因"}</button>
              {notice.modelSettings && onOpenModelSettings && <button type="button" onClick={onOpenModelSettings}>模型设置</button>}
              {notice.securitySettings && onOpenSecuritySettings && <button type="button" onClick={onOpenSecuritySettings}>打开权限设置</button>}
              {continueAction && <button type="button" onClick={() => void continueExecution()} disabled={continuingExecution || retrying || retryDisabled} aria-busy={continuingExecution}
                title={retryDisabledReason ?? "先核对已有结果，再继续未完成的工作"}>{continuingExecution ? "正在继续…" : "检查并继续"}</button>}
              {retryAction && <button type="button" onClick={() => onRetry?.(retryKind)} disabled={retrying || retryDisabled} aria-busy={retrying} title={retryDisabledReason}>{retryLabel}</button>}
            </div>
            {continueError && <p className="execution-notice__action-error">{continueError}</p>}
          </div>}
          {turnFeedback}
        </div>
        {message.complete && (hasAnswerText || onRetry && !notice?.modelSettings && notice?.kind !== "environment") && (
          <div className={"msg__actions msg__actions--footer" + (latest ? " msg__actions--latest" : "") + (speaking || exporting || retrying ? " msg__actions--active" : "")}
            role="group" aria-label="回复操作">
            {hasAnswerText && <button type="button" className="msg__action-btn" data-chat-copy="true"
              onClick={() => void copyText(plainText, "plain")} title={copyMainLabel} data-tip={copyMainLabel} aria-label={copyMainLabel} aria-live="polite">
              {copiedKind ? <Check size={14} /> : <Copy size={14} />}
            </button>}
            {hasAnswerText && <button type="button" className="msg__action-btn" onClick={toggleSpeak} aria-label={speaking ? "停止朗读" : "朗读回复"}
              data-tip={speaking ? "停止朗读" : "朗读回复"} aria-pressed={speaking}>{speaking ? <Square size={14} /> : <Volume2 size={14} />}</button>}
            {onRetry && !continueAction && !retryAction && !notice?.modelSettings && notice?.kind !== "environment" && <button type="button" className="msg__action-btn" onClick={() => onRetry(retryKind)}
              aria-label={retryLabel} data-tip={retryDisabledReason ?? messageRetryTitle(retryKind)} title={retryDisabledReason ?? messageRetryTitle(retryKind)} disabled={retrying || retryDisabled} aria-busy={retrying}>
              <RefreshCw size={14} className={retrying ? "msg__action-spin" : undefined} />
              {(retryKind === "retry" || !hasAnswerText || message.stopReason && message.stopReason !== "end_turn" || retrying) && retryLabel}
            </button>}
            {sessionId && hasAnswerText && <>
              <button type="button" className="msg__action-btn" aria-label={feedback === "up" ? "取消赞" : "赞"} data-tip="有帮助" aria-pressed={feedback === "up"} onClick={() => rateReply("up")}><ThumbsUp size={14} /></button>
              <button type="button" className="msg__action-btn" aria-label={feedback === "down" ? "取消踩" : "踩"} data-tip="需要改进" aria-pressed={feedback === "down"} onClick={() => rateReply("down")}><ThumbsDown size={14} /></button>
            </>}
            {exporting && <span className="msg__action-status" role="status">正在导出 {exporting.toUpperCase()}…</span>}
            {hasAnswerText && <ActionMenu label="更多回复操作" menuLabel={actionView === "export" ? "选择导出格式" : "当前回复操作"}
              resetKey={`${sessionId}:${message.id}`} contentKey={actionView} onBack={actionView === "export" ? () => setActionView("main") : undefined}
              onOpenChange={open => { if (!open) setActionView("main"); }}
              triggerClassName="msg__action-btn msg__more-trigger" trigger={<MoreHorizontal size={16} />}
              menuClassName={"topbar-actions__menu msg__more-menu" + (actionView === "export" ? " msg__export-menu" : "")}>
              {({ close }) => <>
                {actionView === "export" ? <>
                  <button type="button" tabIndex={-1} aria-keyshortcuts="ArrowLeft" className="topbar-actions__item" onClick={() => setActionView("main")}>‹ 返回回复操作</button>
                  <p className="msg__export-note" id={exportDescriptionId}>公式和 Mermaid 图表以原始文本保留。图表可从图表菜单下载 SVG。</p>
                  <p className="msg__export-note" id={exportSpreadsheetDescriptionId}>Excel：回复含表格时，仅导出表格。</p>
                  {(["docx", "pdf", "xlsx", "pptx"] as const).map(format => <button type="button" role="menuitem" className="topbar-actions__item" key={format}
                    aria-describedby={format === "xlsx" ? `${exportDescriptionId} ${exportSpreadsheetDescriptionId}` : exportDescriptionId}
                    disabled={Boolean(exporting)} onClick={() => { close(true); void exportAnswer(format); }}>
                    {format === "docx" ? "Word" : format === "xlsx" ? "Excel" : format === "pptx" ? "PPT" : "PDF"}
                  </button>)}
                </> : <>
                  <div className="action-menu__heading">当前回复</div>
                  {hasAnswerText && <>
                    <button type="button" role="menuitem" className="topbar-actions__item" data-chat-copy="true" onClick={() => { close(true); void copyText(plainText, "plain"); }}><Copy size={14} />复制纯文本</button>
                    <button type="button" role="menuitem" className="topbar-actions__item" data-chat-copy="true" onClick={() => { close(true); void copyText(markdownText, "markdown"); }}><FileCode2 size={14} />复制 Markdown</button>
                    <button type="button" role="menuitem" className="topbar-actions__item" disabled={Boolean(exporting)} onClick={() => setActionView("export")}><Download size={14} />导出本条回复为办公文件</button>
                  </>}
                </>}
              </>}
            </ActionMenu>}
          </div>
        )}
        {exportedArtifact && onOpenArtifact && <button type="button" className="msg__exported-artifact" title={exportedArtifact.path}
          onClick={() => onOpenArtifact(exportedArtifact)}><FileText size={15} /><span>{attachmentBasename(exportedArtifact.path)}</span><span>查看成果 →</span></button>}
        {sessionId && <FeedbackDialog open={feedbackDialog !== null} sessionId={sessionId} messageId={message.id}
          rating={feedbackDialog ?? feedback ?? "up"} onClose={() => setFeedbackDialog(null)} />}

      </div>
    </div>
  );
});
