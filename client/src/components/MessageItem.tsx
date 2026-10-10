import { memo, useCallback, useEffect, useRef, useState } from "react";
import {
  Check,
  ChevronDown,
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
import { ExecutionProcess } from "./ExecutionProcess";
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
import { registerDocumentArtifact } from "@/lib/artifact-catalog";
import { partitionAssistantParts } from "@/lib/execution-process";
import {
  messageRetryKind,
  messageRetryLabel,
  messageRetryTitle,
  type MessageRetryKind,
} from "@/lib/message-retry";
import { useKnowledgeStore } from "@/stores/knowledge-store";
const logoMarkUrl = "/app-icon.png";
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
  markdownConfig,
  cwd,
  sessionId,
  onOpenTool,
  onEditResend,
  onRetry,
  retrying = false,
  latest = false,
  onToast,
  findQuery,
}: {
  message: ChatMessage;
  streaming: boolean;
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
  /** The latest completed assistant reply keeps its actions more discoverable. */
  latest?: boolean;
  /** 会话内查找的当前关键词,Markdown 渲染时把命中处染出。 */
  findQuery?: string;
}) {
  const { theme } = useTheme();
  const [speaking, setSpeaking] = useState(false);
  const [copiedKind, setCopiedKind] = useState<"plain" | "markdown" | null>(null);
  const [copyMenuOpen, setCopyMenuOpen] = useState(false);
  const [exportMenuOpen, setExportMenuOpen] = useState(false);
  const [exporting, setExporting] = useState<OfficeFormat | null>(null);
  const knowledgeTrace = useKnowledgeStore((state) => (
    sessionId && message.promptId
      ? state.turnTraces[sessionId]?.[message.promptId]
      : undefined
  ));
  const stopSpeakingRef = useRef<(() => void) | null>(null);
  const copyResetTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const copyMenuRef = useRef<HTMLDivElement>(null);
  const copyMenuTriggerRef = useRef<HTMLButtonElement>(null);
  const copyMenuListRef = useRef<HTMLDivElement>(null);
  const exportMenuRef = useRef<HTMLDivElement>(null);
  const exportMenuTriggerRef = useRef<HTMLButtonElement>(null);
  const exportMenuListRef = useRef<HTMLDivElement>(null);

  useEffect(() => () => {
    stopSpeakingRef.current?.();
    if (copyResetTimerRef.current) clearTimeout(copyResetTimerRef.current);
  }, []);

  const closeCopyMenu = useCallback((restoreFocus = false) => {
    setCopyMenuOpen(false);
    if (restoreFocus) copyMenuTriggerRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!copyMenuOpen) return;
    copyMenuListRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();

    const closeOnOutside = (event: PointerEvent) => {
      if (!copyMenuRef.current?.contains(event.target as Node)) closeCopyMenu();
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      closeCopyMenu(true);
    };
    document.addEventListener("pointerdown", closeOnOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [closeCopyMenu, copyMenuOpen]);

  useEffect(() => {
    if (!exportMenuOpen) return;
    exportMenuListRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus();
    const closeOnOutside = (event: PointerEvent) => {
      if (!exportMenuRef.current?.contains(event.target as Node)) setExportMenuOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setExportMenuOpen(false);
        exportMenuTriggerRef.current?.focus();
      }
    };
    document.addEventListener("pointerdown", closeOnOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [exportMenuOpen]);

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
    ? partitionAssistantParts(message.parts)
    : null;
  const allTextParts = message.parts.filter(
    (part): part is Extract<typeof part, { kind: "text" }> => part.kind === "text",
  );
  const answerParts = assistantGroups?.responseParts.length
    ? assistantGroups.responseParts
    : allTextParts;

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
  const lastMeaningfulPart = [...message.parts].reverse().find((part) =>
    part.kind === "tool_call" || part.text.trim().length > 0,
  );
  const hasFinalAnswer = lastMeaningfulPart?.kind === "text"
    && Boolean(assistantGroups?.responseParts.some((part) => part.text.trim()));
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
    setExportMenuOpen(false);
    setExporting(format);
    try {
      const title = markdownText.match(/^#\s+(.+)$/m)?.[1]?.trim()
        || markdownText.split("\n").find((line) => line.trim())?.replace(/^#+\s*/, "").slice(0, 80)
        || "EchoAgent 报告";
      const receipt = await exportOfficeDocument(title, markdownText, format);
      if (receipt) {
        if (sessionId) registerDocumentArtifact(receipt, sessionId, title, cwd ?? "");
        onToast?.(`已导出到 ${receipt.path}`);
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
              {copiedKind === "plain" ? "已复制" : "复制"}
            </button>
            {onEditResend && (
              <button
                type="button"
                className="msg__action-btn"
                onClick={() => onEditResend(plainText, attachments)}
                title="编辑并重新发送"
              >
                编辑
              </button>
            )}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="msg msg--assistant">
      <div>
        <div className="msg__header">
          <span className="msg__avatar">
            <img src={logoMarkUrl} alt="" aria-hidden="true" draggable={false} />
          </span>
          <span className="msg__name">EchoAgent</span>
        </div>
        <div className="msg__body">
          {message.parts.length === 0 && !message.complete && (
            <LoadingRow startedAt={message.startedAt} />
          )}
          {assistantGroups
            && (assistantGroups.processParts.length > 0 || hasTerminalProcessStatus || hasKnowledgeTrace)
            && (
              <ExecutionProcess
                sessionId={sessionId}
                messageId={message.id}
                parts={assistantGroups.processParts}
                active={streaming && !message.complete}
                startedAt={message.startedAt}
                completedAt={message.completedAt}
                stopReason={message.stopReason}
                cancelTrigger={message.cancelTrigger}
                cancellationCategory={message.cancellationCategory}
                agentResult={message.agentResult}
                hasFinalAnswer={hasFinalAnswer}
                markdownConfig={markdownConfig}
                onOpenTool={onOpenTool}
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
          {streaming &&
            message.complete === false &&
            (assistantGroups?.responseParts.length ?? 0) > 0 && (
              <span className="msg__caret">▋</span>
            )}
        </div>
        {message.complete && (hasAnswerText || onRetry) && (
          <div
            className={
              "msg__actions msg__actions--footer" +
              (latest ? " msg__actions--latest" : "")
            }
            role="group"
            aria-label="回复操作"
          >
            <div className="msg__action-cluster">
              {hasAnswerText && (
                <>
                  <div
                    className="msg__copy-split"
                    ref={copyMenuRef}
                    onBlur={(event) => {
                      if (!event.currentTarget.contains(event.relatedTarget)) closeCopyMenu();
                    }}
                  >
                    <button
                      type="button"
                      className="msg__action-btn msg__copy-main"
                      data-chat-copy="true"
                      onClick={() => {
                        closeCopyMenu();
                        void copyText(plainText, "plain");
                      }}
                      title={copyMainLabel}
                      aria-label={copyMainLabel}
                      aria-live="polite"
                    >
                      {copiedKind ? <Check size={14} /> : <Copy size={14} />}
                      <span>{copiedKind ? "已复制" : "复制"}</span>
                    </button>
                    <button
                      type="button"
                      className="msg__action-btn msg__copy-menu-trigger"
                      ref={copyMenuTriggerRef}
                      aria-label="更多复制选项"
                      aria-haspopup="menu"
                      aria-expanded={copyMenuOpen}
                      title="更多复制选项"
                      onClick={() => setCopyMenuOpen((open) => !open)}
                      onKeyDown={(event) => {
                        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                          event.preventDefault();
                          setCopyMenuOpen(true);
                        }
                      }}
                    >
                      <ChevronDown size={13} />
                    </button>
                    {copyMenuOpen && (
                      <div
                        className="msg__copy-menu"
                        role="menu"
                        aria-label="选择复制格式"
                        ref={copyMenuListRef}
                        onKeyDown={(event) => {
                          const items = Array.from(
                            copyMenuListRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? [],
                          );
                          const index = items.indexOf(document.activeElement as HTMLButtonElement);
                          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                            event.preventDefault();
                            const delta = event.key === "ArrowDown" ? 1 : -1;
                            items[(index + delta + items.length) % items.length]?.focus();
                          } else if (event.key === "Home" || event.key === "End") {
                            event.preventDefault();
                            items[event.key === "Home" ? 0 : items.length - 1]?.focus();
                          }
                        }}
                      >
                        <button
                          type="button"
                          role="menuitem"
                          data-chat-copy="true"
                          onClick={() => {
                            closeCopyMenu();
                            void copyText(plainText, "plain");
                          }}
                        >
                          <Copy size={14} />
                          <span>复制纯文本</span>
                        </button>
                        <button
                          type="button"
                          role="menuitem"
                          data-chat-copy="true"
                          onClick={() => {
                            closeCopyMenu();
                            void copyText(markdownText, "markdown");
                          }}
                        >
                          <FileCode2 size={14} />
                          <span>复制 Markdown</span>
                        </button>
                      </div>
                    )}
                  </div>
                  <button
                    type="button"
                    className="msg__action-btn"
                    onClick={toggleSpeak}
                    title={speaking ? "停止朗读" : "朗读回复"}
                    aria-label={speaking ? "停止朗读" : "朗读回复"}
                    aria-pressed={speaking}
                  >
                    {speaking ? <Square size={13} /> : <Volume2 size={14} />}
                    <span>{speaking ? "停止" : "朗读"}</span>
                  </button>
                  <div className="msg__copy-split" ref={exportMenuRef}
                    onBlur={(event) => {
                      if (!event.currentTarget.contains(event.relatedTarget)) setExportMenuOpen(false);
                    }}>
                    <button
                      type="button"
                      className="msg__action-btn"
                      ref={exportMenuTriggerRef}
                      aria-haspopup="menu"
                      aria-expanded={exportMenuOpen}
                      aria-label={exporting ? `正在导出 ${exporting.toUpperCase()}` : "导出回复为办公文件"}
                      title="导出回复为办公文件"
                      disabled={exporting !== null}
                      onClick={() => setExportMenuOpen((open) => !open)}
                      onKeyDown={(event) => {
                        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                          event.preventDefault();
                          setExportMenuOpen(true);
                        }
                      }}
                    >
                      <Download size={14} />
                      <span>{exporting ? "导出中…" : "导出"}</span>
                      <ChevronDown size={13} />
                    </button>
                    {exportMenuOpen && (
                      <div className="msg__copy-menu" role="menu" aria-label="选择导出格式"
                        ref={exportMenuListRef}
                        onKeyDown={(event) => {
                          const items = Array.from(exportMenuListRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? []);
                          const index = items.indexOf(document.activeElement as HTMLButtonElement);
                          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                            event.preventDefault();
                            const delta = event.key === "ArrowDown" ? 1 : -1;
                            items[(index + delta + items.length) % items.length]?.focus();
                          } else if (event.key === "Home" || event.key === "End") {
                            event.preventDefault();
                            items[event.key === "Home" ? 0 : items.length - 1]?.focus();
                          }
                        }}>
                        {(["docx", "pdf", "xlsx", "pptx"] as const).map((format) => (
                          <button
                            type="button"
                            role="menuitem"
                            key={format}
                            onClick={() => void exportAnswer(format)}
                          >
                            <span>{format === "docx" ? "Word" : format === "xlsx" ? "Excel" : format === "pptx" ? "PPT" : "PDF"}</span>
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                </>
              )}
              {onRetry && (
                <button
                  type="button"
                  className="msg__action-btn"
                  onClick={() => onRetry(retryKind)}
                  title={messageRetryTitle(retryKind)}
                  disabled={retrying}
                  aria-busy={retrying}
                >
                  <RefreshCw size={14} className={retrying ? "msg__action-spin" : undefined} />
                  <span>{retryLabel}</span>
                </button>
              )}
            </div>
            {sessionId && hasAnswerText && (
              <FeedbackButtons sessionId={sessionId} messageId={message.id} />
            )}
          </div>
        )}
      </div>
    </div>
  );
});

/**
 * 反馈按钮(👍/👎)—— 对齐 EchoAgent message-feedback。
 *
 * 本地持久化(toggle:再点同向取消)。无后端上报(EchoAgent 是 BYOK,无可上报通道)。
 * 选中的方向高亮(填充),未选中保持描边。
 */
function FeedbackButtons({
  sessionId,
  messageId,
}: {
  sessionId: string;
  messageId: string;
}) {
  const entry = useFeedbackStore(
    (s) => s.entries[`${sessionId}:${messageId}`] ?? null,
  );
  const setRating = useFeedbackStore((s) => s.setRating);
  const current = entry?.rating ?? null;
  // 点赞/踩:记录方向并打开完整评分弹窗(对齐 EchoAgent rating bar + 弹窗)。
  const [dialogOpen, setDialogOpen] = useState<FeedbackRating | null>(null);
  const click = (r: FeedbackRating) => {
    // 再点已选中方向 → 取消(不弹窗)。
    if (current === r) {
      setRating(sessionId, messageId, r);
      return;
    }
    setRating(sessionId, messageId, r);
    setDialogOpen(r);
  };
  return (
    <span className="msg__feedback">
      <button
        type="button"
        className={
          "msg__action-btn msg__feedback-btn" +
          (current === "up" ? " msg__feedback-btn--active" : "")
        }
        onClick={() => click("up")}
        title={current === "up" ? "取消赞" : "赞"}
        aria-label={current === "up" ? "取消赞" : "赞"}
        aria-pressed={current === "up"}
      >
        <ThumbsUp size={14} fill={current === "up" ? "currentColor" : "none"} />
      </button>
      <button
        type="button"
        className={
          "msg__action-btn msg__feedback-btn" +
          (current === "down" ? " msg__feedback-btn--active" : "")
        }
        onClick={() => click("down")}
        title={current === "down" ? "取消踩" : "踩"}
        aria-label={current === "down" ? "取消踩" : "踩"}
        aria-pressed={current === "down"}
      >
        <ThumbsDown size={14} fill={current === "down" ? "currentColor" : "none"} />
      </button>
      <FeedbackDialog
        open={dialogOpen !== null}
        sessionId={sessionId}
        messageId={messageId}
        rating={dialogOpen ?? current ?? "up"}
        onClose={() => setDialogOpen(null)}
      />
    </span>
  );
}
