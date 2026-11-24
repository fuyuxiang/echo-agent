import type { AutomationStatus } from "@/lib/automation-client";
import { memo, useCallback, useEffect, useId, useLayoutEffect, useRef, useState, useMemo, type ReactNode } from "react";
import {
  ArrowRight,
  Check,
  CircleAlert,
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
  GitFork,
  Info,
  Sparkles,
} from "lucide-react";
import { Markdown, type MarkdownConfig } from "./markdown/index";
import { LoadingRow } from "./LoadingRow";
import { ExecutionProcess } from "./ExecutionProcess";
import { ActionMenu } from "./ActionMenu";
import { Button } from "./Button";
import { FeedbackDialog } from "./FeedbackDialog";
import { AttachmentList } from "./AttachmentList";
import { useTheme } from "./ThemeProvider";
import { useFeedbackStore, type FeedbackRating } from "@/stores/feedback-store";
import type { ChatMessage, ToolCallView } from "@/stores/session-store";
import { openLocalPath } from "@/lib/agent-client";
import {
  attachmentBasename,
  parseLegacyAttachmentPrompt,
  stripInjectedUserContext,
} from "@/lib/user-message";
import { highlightSegments } from "@/lib/extract-text";
import { copyShareText } from "@/lib/share";
import { exportOfficeDocument, type OfficeFormat } from "@/lib/document-export";
import type { SessionArtifact } from "@/lib/session-artifacts";
import { registerDocumentArtifact } from "@/lib/artifact-catalog";
import { isExecutionSettled, partitionAssistantParts } from "@/lib/execution-process";
import { executionContinuationPrompt, getExecutionNotice } from "@/lib/execution-notice";
import { executionToolStates } from "@/lib/tool-execution-state";
import { computerTaskRunning } from "@/lib/computer-task";
import { formatMessageTime } from "@/lib/message-time";
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

function MessageTime({ time, label }: { time: ReturnType<typeof formatMessageTime>; label: string }) {
  if (!time) return null;
  const description = `${label} ${time.exact}`;
  return <time className="msg__time" dateTime={time.iso} title={description} aria-label={description}>{time.text}</time>;
}

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

type MessageEdit = {
  active: boolean;
  sendDisabled: boolean;
  onOpen: () => void;
  onClose: () => void;
  onSend: (text: string, attachments: string[]) => boolean | void | Promise<boolean | void>;
};

function InlineMessageEditor({ initialText, initialAttachments, cwd, edit, onCancel }: {
  initialText: string;
  initialAttachments: string[];
  cwd?: string;
  edit: MessageEdit;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initialText);
  const [attachments, setAttachments] = useState(initialAttachments);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");
  const input = useRef<HTMLTextAreaElement>(null);
  const inFlight = useRef(false);
  const editDisabled = edit.sendDisabled || (!text.trim() && attachments.length === 0);

  useLayoutEffect(() => {
    const field = input.current;
    if (!field) return;
    field.focus({ preventScroll: true });
    field.setSelectionRange(field.value.length, field.value.length);
  }, []);
  useLayoutEffect(() => {
    const field = input.current;
    if (!field) return;
    const resize = () => {
      field.style.height = "0px";
      field.style.height = `${field.scrollHeight}px`;
    };
    resize();
    let width = field.getBoundingClientRect().width;
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => {
      const next = field.getBoundingClientRect().width;
      if (next === width) return;
      width = next;
      resize();
    });
    observer?.observe(field);
    return () => observer?.disconnect();
  }, [text]);

  async function submit() {
    if (editDisabled || inFlight.current) return;
    inFlight.current = true;
    setSending(true);
    setError("");
    try {
      const accepted = await edit.onSend(text.trim() || "请分析附件。", attachments);
      if (accepted === false) setError("发送未完成，请重试。");
      else edit.onClose();
    } catch {
      setError("发送失败，请重试。");
    } finally {
      inFlight.current = false;
      setSending(false);
    }
  }

  return <form className="msg__inline-edit" aria-label="编辑消息"
    onSubmit={(event) => { event.preventDefault(); void submit(); }}
    onKeyDown={(event) => {
      if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229
        || !event.currentTarget.contains(event.target as Node)) return;
      if (event.key === "Escape" && !inFlight.current) {
        event.preventDefault();
        event.stopPropagation();
        onCancel();
      } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && event.target === input.current) {
        event.preventDefault();
        void submit();
      }
    }}>
    <AttachmentList paths={attachments} cwd={cwd} onRemove={sending ? undefined
      : (path) => setAttachments((files) => files.filter((file) => file !== path))} />
    <textarea ref={input} className="msg__inline-edit-input" aria-label="编辑消息内容" rows={1}
      placeholder="补充消息内容…" value={text} readOnly={sending}
      onChange={(event) => { setText(event.target.value); setError(""); }} />
    <div className="msg__inline-edit-footer">
      {error && <span className="msg__inline-edit-error" role="alert">{error}</span>}
      <div className="msg__inline-edit-actions">
        <Button variant="ghost" size="compact" disabled={sending} onClick={onCancel}>取消</Button>
        <Button type="submit" variant="primary" size="compact" disabled={editDisabled}
          loading={sending} aria-keyshortcuts="Meta+Enter Control+Enter">作为新消息发送</Button>
      </div>
    </div>
  </form>;
}

/**
 * Renders one chat message. Assistant messages are left-aligned with avatar +
 * name row; user messages are right-aligned bubbles with no avatar / name.
 *
 * Message action bar (对齐 EchoAgent):
 *  - user: 复制 / 编辑消息
 *  - assistant: rendered after the complete answer, so read-then-act follows
 *    the visual and keyboard order.
 */
export const MessageItem = memo(function MessageItem({
  message,
  messageDay,
  automationStatus,
  executionLabel,
  executionDescription,
  streaming,
  waitingForModel = true,
  waitingReason,
  markdownConfig,
  cwd,
  sessionId,
  onOpenTool,
  onOpenArtifact,
  compactIdentity = false,
  hideIdentity = false,
  edit,
  onRetry,
  onContinueInNewConversation,
  onLearnWorkflow,
  continuationDisabled = false,
  continuationDisabledReason,
  retrying = false,
  retryDisabled = false,
  retryDisabledReason,
  latest = false,
  onToast,
  findQuery,
  authorName,
  autoExpandProcess = false,
  onReviewProcess,
  runtimeError,
  suppressStoppedNotice = false,
  onContinueExecution,
  onOpenModelSettings,
  onOpenSecuritySettings,
  turnFeedback,
}: {
  message: ChatMessage;
  messageDay?: number;
  executionLabel?: string;
  executionDescription?: string;
  authorName?: string;
  compactIdentity?: boolean;
  /** The containing group message already provides the speaker and role. */
  hideIdentity?: boolean;
  onOpenArtifact?: (artifact: SessionArtifact) => void;
  streaming: boolean;
  /** Suppress model-wait copy while a decision, cancellation or error takes priority. */
  waitingForModel?: boolean;
  waitingReason?: string;
  autoExpandProcess?: boolean;
  onReviewProcess?: () => void;
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
  automationStatus?: AutomationStatus;
  onToast?: (msg: string) => void;
  /** Open tool detail in the right-side panel (Phase 2). */
  onOpenTool?: (tc: ToolCallView) => void;
  /** Inline editor controls for this user message. */
  edit?: MessageEdit;
  /** Retry the latest turn using semantics derived from whether it used tools. */
  onRetry?: (kind: MessageRetryKind) => void;
  onContinueInNewConversation?: () => void;
  onLearnWorkflow?: () => void;
  continuationDisabled?: boolean;
  continuationDisabledReason?: string;
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
  const editTrigger = useRef<HTMLButtonElement>(null);
  const restoreEditFocus = useRef(false);
  useLayoutEffect(() => {
    if (edit?.active || !restoreEditFocus.current) return;
    restoreEditFocus.current = false;
    editTrigger.current?.focus({ preventScroll: true });
  }, [edit?.active]);
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
  const desktopWaiting = automationStatus?.task?.phase === "waiting_user" && !automationStatus.task.authorized && automationStatus.task.outcome;
  const notice = desktopWaiting || (outcome?.kind === "stopped" && suppressStoppedNotice)
    || outcome?.kind === "empty" && automationStatus?.task?.scope ? null : outcome;
  const NoticeIcon = notice?.tone === "neutral" ? Info : CircleAlert;
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

  const messageActive = streaming && !message.complete;
  const messageExecution = useMemo(() => executionToolStates(message.parts.flatMap(part =>
    part.kind === "tool_call" ? [part.toolCall] : []), !messageActive, automationStatus), [message.parts, messageActive, automationStatus]);
  const assistantGroups = message.role === "assistant"
    ? partitionAssistantParts(message.parts, { organizeProgress: true, active: messageActive, execution: messageExecution })
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
    assistantGroups.processParts.length > 0 || hasTerminalProcessStatus || hasKnowledgeTrace || Boolean(message.memorySources?.length) || notice || automationStatus
  ));
  const generatingAnswer = streaming && waitingForModel && !message.complete && hasFinalAnswer
    && !Array.from(messageExecution.values()).some(execution => execution.state === "in_progress")
    && knowledgeTrace?.personal?.state !== "searching";
  const generatingInProcess = generatingAnswer && hasExecutionProcess && !waitingReason
    && automationStatus?.task?.phase !== "waiting_user" && !computerTaskRunning(automationStatus);
  const retryKind = messageRetryKind(message);
  const retryLabel = messageRetryLabel(retryKind, retrying);
  const copyMainLabel = copiedKind === "plain"
    ? "已复制"
    : copiedKind === "markdown"
      ? "已复制 Markdown"
      : "复制纯文本";
  const speechLabel = speaking ? "停止朗读" : "朗读回复";

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
        onToast?.(receipt.editableSourceWarning ?? `已导出“${receipt.path.split(/[\\/]/).pop()}”${sessionId ? "，可在成果中查看" : ""}`);
      }
    } catch (error) {
      onToast?.(`导出失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setExporting(null);
    }
  };

  if (message.role === "user") {
    const attachments = message.attachments ?? [];
    if (edit?.active) {
      const parsed = parseLegacyAttachmentPrompt(plainText);
      return <div className="msg msg--user msg--user-editing">
        <InlineMessageEditor initialText={parsed.text}
          initialAttachments={attachments.length ? attachments : parsed.attachments} cwd={cwd} edit={edit}
          onCancel={() => { restoreEditFocus.current = true; edit.onClose(); }} />
      </div>;
    }
    return (
      <div className="msg msg--user">
        <div>
          <div className={"msg__bubble" + (attachments.length ? " msg__bubble--with-attachments" : "")}>
            <AttachmentList key={`${sessionId ?? ""}:${message.id}`} paths={attachments} cwd={cwd} variant="message" />
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
          <div className="msg__footer">
            <MessageTime time={formatMessageTime(message.sentAt, messageDay)} label="发送于" />
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
              {edit && (plainText || attachments.length > 0) && (
                <button
                  ref={editTrigger}
                  type="button"
                  className="msg__action-btn"
                  onClick={edit.onOpen}
                  data-tip="编辑消息" title="编辑消息" aria-label="编辑消息"
                >
                  <Pencil size={14} />
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    );
  }

  const endTime = message.complete ? formatMessageTime(message.endedAt, messageDay) : undefined;
  const showReplyRetry = onRetry && !continueAction && !retryAction && !notice?.modelSettings && notice?.kind !== "environment";
  const showReplyActions = message.complete && (hasAnswerText || showReplyRetry || onLearnWorkflow);

  return (
    <div className={"msg msg--assistant" + (compactIdentity ? " msg--continuation" : "")}>
      <div>
        {executionLabel && <div className="execution-turn" aria-label="本次处理说明">
          <span className="execution-turn__label">{executionLabel}</span>
          {executionDescription && <p>{executionDescription}</p>}
        </div>}
        {!hideIdentity && (authorName ? <div className="msg__header">
          <span className="msg__avatar" aria-hidden="true">{authorName.slice(0, 1)}</span>
          <span className="msg__name">{authorName}</span>
        </div> : <span className="sr-only">EchoAgent</span>)}
        <div className="msg__body">
          {streaming && waitingForModel && !runtimeError && !message.complete && message.parts.length === 0 && !hasKnowledgeTrace && !automationStatus && (
            <LoadingRow key={`${sessionId}:${message.id}`} startedAt={message.startedAt} />
          )}
          {assistantGroups && hasExecutionProcess
            && (
              <ExecutionProcess
                sessionId={sessionId}
                cwd={cwd}
                messageId={message.id}
                automationStatus={automationStatus}
                parts={assistantGroups.processParts}
                active={streaming && !message.complete}
                settled={isExecutionSettled(message)}
                autoExpand={autoExpandProcess}
                onReview={onReviewProcess}
                startedAt={message.startedAt}
                completedAt={message.completedAt}
                lastActivityAt={message.lastActivityAt}
                stopReason={message.stopReason}
                cancelTrigger={message.cancelTrigger}
                cancellationCategory={message.cancellationCategory}
                agentResult={[...new Set([message.agentResult, runtimeError].filter(Boolean))].join("\n") || undefined}
                resultNotice={notice ?? undefined}
                hasFinalAnswer={hasFinalAnswer}
                generatingAnswer={generatingInProcess}
                waitingReason={waitingReason}
                markdownConfig={markdownConfig}
                findQuery={findQuery}
                onOpenTool={onOpenTool}
                revealRequest={processReveal}
                knowledgeTrace={knowledgeTrace}
                memorySources={message.memorySources}
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
          {generatingAnswer && !generatingInProcess && (
            <div className="msg-progress-row msg__writing" role="status"><span aria-hidden="true" />正在生成回复</div>
          )}
          {notice && <div className={`execution-notice execution-notice--${notice.tone}`} role={notice.tone === "neutral" ? "status" : "alert"}>
            <NoticeIcon className="execution-notice__icon" size={14} strokeWidth={1.75} aria-hidden="true" />
            <div className="execution-notice__content">
              <div className="execution-notice__copy">
                <strong>{notice.title}</strong>
                {notice.description && <p>{notice.description}</p>}
              </div>
              <div className="execution-notice__actions">
                <Button size="compact" variant="ghost" aria-label="查看详情" data-chat-reading="true" onClick={() => {
                  if (notice.tools[0] && onOpenTool) onOpenTool(notice.tools[0]);
                  else setProcessReveal((value) => value + 1);
                }}>详情</Button>
                {notice.modelSettings && onOpenModelSettings && <Button size="compact" variant="ghost" className="execution-notice__recovery" onClick={onOpenModelSettings}>模型设置</Button>}
                {notice.securitySettings && onOpenSecuritySettings && <Button size="compact" variant="ghost" className="execution-notice__recovery" onClick={onOpenSecuritySettings}>打开权限设置</Button>}
                {continueAction && <Button size="compact" variant="ghost" className="execution-notice__recovery" onClick={() => void continueExecution()}
                  disabled={continuingExecution || retrying || retryDisabled} aria-busy={continuingExecution}
                  title={retryDisabledReason ?? "先核对已有结果，再继续未完成的工作"}>
                  {continuingExecution ? "正在继续…" : <>检查并继续<ArrowRight aria-hidden="true" /></>}
                </Button>}
                {retryAction && <Button size="compact" variant="ghost" className="execution-notice__recovery" onClick={() => onRetry?.(retryKind)}
                  disabled={retrying || retryDisabled} aria-busy={retrying} title={retryDisabledReason}>{retryLabel}</Button>}
              </div>
              {continueError && <p className="execution-notice__action-error">{continueError}</p>}
            </div>
          </div>}
          {turnFeedback}
        </div>
        {(showReplyActions || endTime) && <div className="msg__footer">
          {showReplyActions && <div className={"msg__actions msg__actions--footer" + (latest ? " msg__actions--latest" : "") + (speaking || exporting || retrying ? " msg__actions--active" : "")}
            role="group" aria-label="回复操作">
            {hasAnswerText && <button type="button" className="msg__action-btn" data-chat-copy="true"
              onClick={() => void copyText(plainText, "plain")} title={copyMainLabel} data-tip={copyMainLabel} aria-label={copyMainLabel} aria-live="polite">
              {copiedKind ? <Check size={14} /> : <Copy size={14} />}
            </button>}
            {hasAnswerText && <button type="button" className="msg__action-btn" onClick={toggleSpeak}
              aria-label={speechLabel} data-tip={speechLabel} data-active={speaking || undefined}>
              {speaking ? <Square size={14} aria-hidden="true" /> : <Volume2 size={14} aria-hidden="true" />}
            </button>}
            {hasAnswerText && sessionId && <>
              <button type="button" className="msg__action-btn" aria-label="赞" aria-pressed={feedback === "up"}
                data-tip={feedback === "up" ? "取消赞" : "赞"} onClick={() => rateReply("up")}>
                <ThumbsUp size={14} aria-hidden="true" />
              </button>
              <button type="button" className="msg__action-btn" aria-label="踩" aria-pressed={feedback === "down"}
                data-tip={feedback === "down" ? "取消踩" : "踩"} onClick={() => rateReply("down")}>
                <ThumbsDown size={14} aria-hidden="true" />
              </button>
            </>}
            {hasAnswerText && onContinueInNewConversation && <button type="button" className="msg__action-btn"
              aria-label="在新对话中继续" data-tip={continuationDisabledReason ?? "在新对话中继续"}
              disabled={continuationDisabled} title={continuationDisabledReason} onClick={onContinueInNewConversation}>
              <GitFork size={14} aria-hidden="true" />
            </button>}
            {showReplyRetry && <button type="button" className="msg__action-btn" onClick={() => onRetry(retryKind)}
              aria-label={retryLabel} data-tip={retryDisabledReason ?? messageRetryTitle(retryKind)} title={retryDisabledReason ?? messageRetryTitle(retryKind)} disabled={retrying || retryDisabled} aria-busy={retrying}>
              <RefreshCw size={14} className={retrying ? "msg__action-spin" : undefined} />
              {(retryKind === "retry" || !hasAnswerText || message.stopReason && message.stopReason !== "end_turn" || retrying) && retryLabel}
            </button>}
            {exporting && <span className="msg__action-status" role="status">正在导出 {exporting.toUpperCase()}…</span>}
            {(hasAnswerText || onLearnWorkflow) && <ActionMenu label="更多回复操作" menuLabel={actionView === "export" ? "选择导出格式" : "当前回复操作"}
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
                  {hasAnswerText && <><button type="button" role="menuitem" className="topbar-actions__item" data-chat-copy="true" onClick={() => { close(true); void copyText(plainText, "plain"); }}><Copy size={14} />复制纯文本</button>
                  <button type="button" role="menuitem" className="topbar-actions__item" data-chat-copy="true" onClick={() => { close(true); void copyText(markdownText, "markdown"); }}><FileCode2 size={14} />复制 Markdown</button>
                  <button type="button" role="menuitem" className="topbar-actions__item" disabled={Boolean(exporting)} onClick={() => setActionView("export")}><Download size={14} />导出本条回复为办公文件</button></>}
                  {onLearnWorkflow && <button type="button" role="menuitem" className="topbar-actions__item"
                    onClick={() => { close(true); onLearnWorkflow(); }}><Sparkles size={14} />从这次任务提炼</button>}
                </>}
              </>}
            </ActionMenu>}
          </div>}
          <MessageTime time={endTime} label={message.stopReason === "cancelled" ? "执行停止于" : "本轮结束于"} />
        </div>}
        {exportedArtifact && onOpenArtifact && <button type="button" className="msg__exported-artifact" title={exportedArtifact.path}
          onClick={() => onOpenArtifact(exportedArtifact)}><FileText size={15} /><span>{attachmentBasename(exportedArtifact.path)}</span><span>查看成果 →</span></button>}
        {sessionId && <FeedbackDialog open={feedbackDialog !== null} sessionId={sessionId} messageId={message.id}
          rating={feedbackDialog ?? feedback ?? "up"} onClose={() => setFeedbackDialog(null)} />}

      </div>
    </div>
  );
});
