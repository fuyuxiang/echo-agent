import { create } from "zustand";
import type {
  Plan,
  PlanUpdate,
  PromptComplete,
  SessionUpdate,
  ToolCallContent,
  ToolCallUpdate,
  UsageUpdate,
} from "@/lib/types";
import {
  ATTACHMENTS_META_KEY,
  normalizeAttachmentPaths,
  parseLegacyAttachmentPrompt,
  stripAttachmentTransportContext,
  stripInjectedUserContext,
} from "@/lib/user-message";
import {
  persistSessionControl,
  readPersistedSessionControl,
  type SessionControl,
  type SessionControlAction,
} from "@/lib/session-control";
import { terminalSessionStatus } from "@/lib/turn-status";
import { officeReceiptFromRawOutput, type DocumentExportReceipt } from "@/lib/document-export";

/**
 * A single chat message in the transcript the UI renders.
 *
 * `assistant` messages accumulate `agent_message_chunk` text deltas and may
 * carry tool-call cards interleaved with text. We model the body as an
 * ordered list of "parts" so streaming stays in order.
 */
export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  parts: MessagePart[];
  /** Local paths attached to a user prompt, in selection order. */
  attachments?: string[];
  /** Exact model-facing prompt before Runtime execution. Hidden expert/project
   * context may differ from the visible text and must survive regeneration. */
  agentText?: string;
  /** ACP prompt index used to merge replayed text/image chunks into one turn. */
  promptIndex?: number;
  /** Stable terminal id used to deduplicate live and durable completion rails. */
  promptId?: string;
  /** False while the assistant is still streaming this message. */
  complete: boolean;
  /** Local wall-clock metadata for live turns. Historical replay may omit it. */
  startedAt?: number;
  completedAt?: number;
  /** True only for an assistant reconstructed from durable history replay.
   *  This lets the UI close a truncated historical tail without cancelling a
   *  genuinely live turn that is still running in the background. */
  replayed?: boolean;
  /** Terminal reason when the bridge supplied one (for cancelled/error UX). */
  stopReason?: string;
  cancelTrigger?: string;
  cancellationCategory?: string;
  agentResult?: string;
}

export type MessagePart =
  | { kind: "text"; text: string; streamId?: string }
  | { kind: "thought"; text: string; streamId?: string }
  | { kind: "tool_call"; toolCall: ToolCallView; streamId?: string };

/** A tool-call card rendered inline. Mirrors a subset of ToolCallUpdate. */
export interface ToolCallView {
  toolCallId: string;
  title: string;
  kind: string;
  status: "in_progress" | "completed" | "failed";
  content: ToolCallUpdate["content"];
  rawInput?: unknown;
  /** Native Office receipt derived from the Runtime's trusted MCP rawOutput. */
  officeReceipt?: DocumentExportReceipt;
}

interface Usage {
  promptTokens?: number;
  completionTokens?: number;
  totalTokens?: number;
}

export type AgentMode = "default" | "ask" | "plan" | "browser_use" | "computer_use";

/** One parked `echo.agent/exit_plan_mode` reverse request. */
export interface PlanApprovalRequest {
  requestId: string;
  sessionId: string;
  toolCallId: string;
  planContent?: string;
}

/**
 * Per-session transcript — the single source of truth.
 *
 * Historically the store kept ONE global `messages`/`streamingMessageId` and
 * wiped them on every session switch, which lost the locally-optimistic user
 * bubbles and had nowhere to accumulate a session that kept streaming in the
 * background after the user switched away. Now every session owns its own
 * transcript, and the top-level `messages`/`streaming`/`usage`/`plan` fields
 * are just a *mirror* of the currently-focused session's transcript (so every
 * existing reader keeps working unchanged).
 *
 * `suppressReplay` is set when we switch back to a session we already have a
 * cached transcript for: the cache IS the truth, so the history replay that
 * `agentLoadSession` re-streams must be ignored for the message-streaming
 * cases (otherwise turns merge and historical usage overwrites the live one).
 */
export interface SessionTranscript {
  messages: ChatMessage[];
  /** Non-null while this session has an in-flight assistant message. Doubles
   *  as the per-session "is streaming" flag. */
  streamingMessageId: string | null;
  /** A replacement prompt accepted by the desktop while the previous turn is
   *  being cancelled atomically. It becomes the next streaming assistant as
   *  soon as the runtime echoes the replacement turn. */
  pendingSendNowPromptId: string | null;
  usage: Usage;
  plan: Plan | null;
  /** Local plan edits awaiting submission. Lives with the cached transcript. */
  planRevisionDirty?: boolean;
  /** Authoritative mode from ACP CurrentModeUpdate. */
  planMode: boolean;
  /** Full authoritative mode, including Browser Use and Computer Use. */
  agentMode?: AgentMode;
  /** Ordered, replayable approvals owned by this session. */
  planApprovals: PlanApprovalRequest[];
  suppressReplay: boolean;
  /** Prompt ids whose pause/stop was explicitly resumed. Late duplicate
   * terminal events for these turns must not re-arm the control state. */
  dismissedControlPromptIds: string[];
  /** Session-scoped pause/stop barrier. While present, late live chunks cannot
   * recreate a streaming assistant after the user ended the turn. */
  control?: SessionControl;
}

interface SessionState {
  /** Currently focused session. Top-level mirrors below reflect this one. */
  sessionId: string | null;
  /** Per-session transcripts (single source of truth). */
  transcripts: Record<string, SessionTranscript>;

  // --- mirrors of transcripts[sessionId] (read by the UI) ---
  messages: ChatMessage[];
  /** True between `agent_send` and `agent://complete` for the focused session. */
  streaming: boolean;
  /** True during the short cancel-and-replace hand-off between two turns. */
  sendNowPending: boolean;
  /** Last assistant message id being streamed in the focused session. */
  streamingMessageId: string | null;
  usage: Usage;
  plan: Plan | null;
  planRevisionDirty: boolean;
  planApproval: PlanApprovalRequest | null;
  /** Focused session's pause/stop state. Kept outside ChatView so navigation
   * and background automation cannot discard or overwrite it. */
  control?: SessionControl;

  error: string | null;
  /** Plan mode on/off — mirror of the focused session's authoritative mode. */
  planMode: boolean;
  /** Full authoritative Runtime mode for the focused session. */
  agentMode: AgentMode;

  // --- lifecycle ---
  setSession: (id: string | null) => void;
  reset: () => void;
  /** Start an assistant placeholder in a specific transcript. Defaults to the
   *  focused conversation for ordinary composer sends. */
  startStreaming: (sessionId?: string, promptId?: string) => void;
  /** Keep the current assistant attached while Runtime atomically replaces it. */
  requestSendNow: (promptId: string, sessionId?: string) => void;
  /** Roll back a replacement request rejected before Runtime accepted it. */
  rejectSendNow: (promptId: string, sessionId?: string) => void;
  markComplete: (p: PromptComplete) => void;
  /** Arm a pause/stop barrier before sending the native cancel notification. */
  requestControl: (
    sessionId: string,
    action: SessionControlAction,
    promptId?: string,
  ) => void;
  /** Finalize an accepted control request locally; prompt_complete may follow. */
  confirmControl: (sessionId: string, action: SessionControlAction) => void;
  /** Roll back the barrier when the native request could not be sent. */
  rejectControl: (sessionId: string, action: SessionControlAction) => void;
  /** Explicitly restore a paused/stopped session for a future turn. */
  resumeSession: (sessionId: string) => void;
  /** Reinstate the exact control barrier when optimistic prompt admission is
   *  rejected by the native runtime. */
  restoreControl: (sessionId: string, control: SessionControl) => void;
  /** Finalize every in-flight transcript after a process-wide agent failure. */
  failAllStreaming: (reason?: string, agentResult?: string) => void;
  setError: (e: string | null) => void;
  /** Stop a session's stream locally (cancel button): keep any text already
   *  streamed, mark the in-flight message complete, and clear its streaming
   *  flag. Defaults to the focused session. Does NOT talk to the backend. */
  stopStreaming: (sessionId?: string) => void;
  /** Drop a session's cached transcript so the next focus reloads it from
   *  EchoAgent (used after a rewind that rewrites backend history). */
  dropSessionCache: (id: string) => void;
  /** Re-enable replay ingestion for a session once its agentLoadSession call
   *  has finished (so a *new* turn's updates aren't suppressed). */
  clearReplaySuppression: (id?: string) => void;
  /** Finish only a replay-created, unterminated historical tail after
   *  agentLoadSession returns. A process exit can leave the durable log at a
   *  tool_call with no turn_completed event; that must not look live forever.
   *  A persisted completed status confirms the turn ended normally even when
   *  the replay omitted its terminal update. */
  finalizeIncompleteReplay: (id?: string, knownCompleted?: boolean) => void;

  // --- transcript ops ---
  /** Append a user message (sent optimistically before the round-trip). */
  pushUser: (
    text: string,
    attachments?: string[],
    sessionId?: string,
    agentText?: string,
  ) => void;
  /** Remove the most recent optimistic user message and its empty assistant
   *  placeholder when `agent_send` rejects before the turn starts. */
  rollbackPendingTurn: () => void;
  /** Remove a visible turn before retrying a request that the Runtime never
   * accepted and therefore cannot expose as a rewindable conversation turn. */
  discardMessagesFrom: (sessionId: string, messageId: string) => void;
  /** Append an assistant message (for preview mode simulation). */
  pushAssistant: (text: string) => void;
  /** Apply a streamed session/update from the backend. The update is routed
   *  to the transcript it belongs to (`__sessionId`, falling back to the
   *  focused session) so background sessions keep accumulating. */
  applyUpdate: (u: SessionUpdate & { __sessionId?: string }) => void;
  /** Bulk-replace the focused session's messages (history load fallback). */
  setMessages: (msgs: ChatMessage[]) => void;
  /** Replace a plan; only explicit local revisions need submission. */
  setPlan: (plan: Plan | null, options?: { sessionId?: string; localRevision?: boolean }) => void;
  /** Clear only the submitted revision, preserving edits made during the ACK. */
  markPlanRevisionSynced: (plan: Plan, sessionId?: string) => void;
  /** Apply an authoritative mode value to a session (focused by default). */
  setPlanMode: (enabled: boolean, sessionId?: string) => void;
  setAgentMode: (mode: AgentMode, sessionId?: string) => void;
  requestPlanApproval: (request: PlanApprovalRequest) => void;
  dismissPlanApproval: (requestId: string, sessionId?: string) => void;
}

let seq = 0;
const nextId = () => `m${Date.now()}_${seq++}`;

const EMPTY_TRANSCRIPT: SessionTranscript = {
  messages: [],
  streamingMessageId: null,
  pendingSendNowPromptId: null,
  usage: {},
  plan: null,
  planMode: false,
  agentMode: "default",
  planApprovals: [],
  suppressReplay: false,
  dismissedControlPromptIds: [],
};

const MAX_MESSAGE_TEXT_CHARS = 2_000_000;
const MAX_TOOL_CONTENT_BLOCKS = 256;
const MAX_TOOL_TEXT_CHARS = 1_000_000;
const MAX_TOOL_DIFF_CHARS = 1_000_000;
const MAX_TOOL_IMAGE_BASE64_CHARS = 16 * 1024 * 1024;
const TRUNCATED_OUTPUT_MARKER = "\n\n… 输出过大，已停止在界面中继续累积 …";

function boundedString(value: unknown, maxChars: number): string {
  if (typeof value !== "string") return "";
  return value.length <= maxChars ? value : value.slice(0, maxChars);
}

function boundedRawInput(value: unknown): unknown {
  if (value === undefined) return undefined;
  try {
    const encoded = JSON.stringify(value);
    if (!encoded || encoded.length <= 256_000) return value;
    return {
      truncated: true,
      preview: encoded.slice(0, 64_000),
      message: "工具参数过大，仅显示前 64,000 个字符",
    };
  } catch {
    return "(工具参数无法显示)";
  }
}

// Side-channel update listeners: keyed by session id. The inspiration panel
// registers one to accumulate EchoAgent's streamed JSON output for a side session.
// When present, applyUpdate forwards matching updates to the listener IN
// ADDITION to (not instead of) routing them into that session's transcript —
// but the inspiration session is never focused, so the transcript stays inert.
const foreignUpdateListeners = new Map<string, (u: SessionUpdate) => void>();

/** Register a side-channel listener for a specific session id. Returns an
 *  unsubscribe function. */
export function registerForeignUpdateListener(
  sessionId: string,
  cb: (u: SessionUpdate) => void,
): () => void {
  foreignUpdateListeners.set(sessionId, cb);
  return () => {
    // Only delete if still ours (avoids clobbering a re-registration).
    if (foreignUpdateListeners.get(sessionId) === cb) {
      foreignUpdateListeners.delete(sessionId);
    }
  };
}

/**
 * Normalize ACP wire-format `ToolCallContent[]` into the shape the frontend
 * `ToolCallCard` expects.
 *
 * ACP (agent-client-protocol-schema 0.11.x) serializes content as:
 *   - text:  { type: "content", content: { type: "text", text: "…" } }
 *   - image: { type: "content", content: { type: "image", data, mimeType, uri? } }
 *   - link:  { type: "content", content: { type: "resource_link", name, uri } }
 *   - diff:  { type: "diff", path, oldText, newText }
 *   - term:  { type: "terminal", terminalId }
 *
 * The frontend expects:
 *   - text:  { type: "text", text: "…" }
 *   - image: { type: "image", data, mimeType, uri? }
 *   - diff:  { type: "diff", diff: { path, old, new } }
 *   - cmd:   { type: "command_output", command?, output }
 *
 * Variants we don't model (audio, embedded resources) degrade to a text
 * fallback carrying their name/uri instead of silently vanishing.
 */
function normalizeToolCallContent(raw: unknown): ToolCallContent[] {
  if (!Array.isArray(raw)) return [];
  return (raw as unknown[]).slice(0, MAX_TOOL_CONTENT_BLOCKS).map((rawItem) => {
    const item = rawItem && typeof rawItem === "object"
      ? rawItem as Record<string, unknown>
      : {};
    const t = item.type as string;

    // ACP wraps text/images/etc inside { type:"content", content: ContentBlock }.
    if (t === "content") {
      const inner = item.content as Record<string, unknown> | undefined;
      if (inner?.type === "text") {
        return { type: "text" as const, text: boundedString(inner.text, MAX_TOOL_TEXT_CHARS) };
      }
      // EchoAgent's read_file returns ImageContent for image/PDF files
      // (acp_conversion.rs): base64 `data` + `mimeType` (+ optional `uri`).
      if (inner?.type === "image") {
        return {
          type: "image" as const,
          data: boundedString(inner.data, MAX_TOOL_IMAGE_BASE64_CHARS),
          mimeType: boundedString(inner.mimeType, 128) || "image/png",
          uri: typeof inner.uri === "string" ? boundedString(inner.uri, 4_096) : undefined,
        };
      }
      // resource_link carries a human name + URI — degrade to text so the
      // card still shows something useful.
      if (inner?.type === "resource_link") {
        const name = boundedString(inner.name, 4_096);
        const uri = boundedString(inner.uri, 4_096);
        return { type: "text" as const, text: boundedString(uri ? `${name}\n${uri}` : name, MAX_TOOL_TEXT_CHARS) };
      }
      // Embedded resource with inline text — surface the text.
      if (inner?.type === "resource") {
        const res = inner.resource as Record<string, unknown> | undefined;
        if (res && typeof res.text === "string") {
          return { type: "text" as const, text: boundedString(res.text, MAX_TOOL_TEXT_CHARS) };
        }
        return { type: "text" as const, text: "(binary resource)" };
      }
      return { type: "text" as const, text: "" };
    }

    // ACP diff uses flat oldText/newText; frontend expects nested diff.old/new.
    if (t === "diff") {
      const diff = item.diff && typeof item.diff === "object"
        ? item.diff as Record<string, unknown>
        : {};
      return {
        type: "diff" as const,
        diff: {
          path: boundedString(diff.path ?? item.path, 4_096),
          old: boundedString(diff.old ?? item.oldText, MAX_TOOL_DIFF_CHARS),
          new: boundedString(diff.new ?? item.newText, MAX_TOOL_DIFF_CHARS),
        },
      };
    }

    // ACP terminal → frontend command_output placeholder.
    if (t === "terminal") {
      return {
        type: "command_output" as const,
        command: undefined,
        output: `[terminal ${boundedString(item.terminalId, 4_096)}]`,
      };
    }

    if (t === "text") {
      return { type: "text" as const, text: boundedString(item.text, MAX_TOOL_TEXT_CHARS) };
    }
    if (t === "image") {
      return {
        type: "image" as const,
        data: boundedString(item.data, MAX_TOOL_IMAGE_BASE64_CHARS),
        mimeType: boundedString(item.mimeType, 128) || "image/png",
        uri: typeof item.uri === "string" ? boundedString(item.uri, 4_096) : undefined,
      };
    }
    if (t === "command_output") {
      return {
        type: "command_output" as const,
        command: typeof item.command === "string" ? boundedString(item.command, 16_384) : undefined,
        output: boundedString(item.output, MAX_TOOL_TEXT_CHARS),
        exitCode: typeof item.exitCode === "number" || item.exitCode === null
          ? item.exitCode
          : undefined,
      };
    }
    return { type: "text" as const, text: "(不支持的工具输出类型)" };
  });
}

function ensureStreamingAssistant(
  messages: ChatMessage[],
  streamingMessageId: string | null,
  promptId?: string,
  replayed = false,
): { messages: ChatMessage[]; id: string } {
  // Reuse the existing streaming assistant message if it's still incomplete
  // and its last part isn't a terminal tool_call.
  if (streamingMessageId) {
    const idx = messages.findIndex((m) => m.id === streamingMessageId);
    if (
      idx !== -1
      && !messages[idx].complete
      && (!promptId || messages[idx].promptId === promptId)
    ) {
      return { messages, id: streamingMessageId };
    }
  }
  const id = nextId();
  const asst: ChatMessage = {
    id,
    role: "assistant",
    parts: [],
    ...(promptId ? { promptId } : {}),
    complete: false,
    ...(replayed ? { replayed: true } : { startedAt: Date.now() }),
  };
  return { messages: [...messages, asst], id };
}

function appendText(
  msg: ChatMessage,
  kind: "text" | "thought",
  delta: string,
  streamId?: string,
): ChatMessage {
  const parts = [...msg.parts];
  const last = parts[parts.length - 1];
  if (last && last.kind === kind && last.streamId === streamId) {
    if (last.text.endsWith(TRUNCATED_OUTPUT_MARKER)) return msg;
    const available = MAX_MESSAGE_TEXT_CHARS - last.text.length - TRUNCATED_OUTPUT_MARKER.length;
    const text = delta.length <= Math.max(0, available)
      ? last.text + delta
      : last.text + delta.slice(0, Math.max(0, available)) + TRUNCATED_OUTPUT_MARKER;
    parts[parts.length - 1] = { kind, text, ...(streamId ? { streamId } : {}) } as MessagePart;
  } else {
    const available = MAX_MESSAGE_TEXT_CHARS - TRUNCATED_OUTPUT_MARKER.length;
    const text = delta.length <= available
      ? delta
      : delta.slice(0, available) + TRUNCATED_OUTPUT_MARKER;
    parts.push({ kind, text, ...(streamId ? { streamId } : {}) } as MessagePart);
  }
  return { ...msg, parts };
}

function userMessageText(message: ChatMessage): string {
  return message.parts
    .filter((part): part is Extract<MessagePart, { kind: "text" }> => part.kind === "text")
    .map((part) => part.text)
    .join("\n");
}

function mergePaths(existing: string[] | undefined, incoming: string[]): string[] {
  return [...new Set([...(existing ?? []), ...incoming])];
}

interface CompletionMetadata {
  promptId?: string;
  stopReason?: string;
  cancelTrigger?: string;
  cancellationCategory?: string;
  agentResult?: string;
  completedAt?: number;
}

function completeStreamingAssistant(
  transcript: SessionTranscript,
  completion: CompletionMetadata = {},
): SessionTranscript {
  const completedAt = completion.completedAt ?? Date.now();
  const preserveEmptyTerminal = Boolean(
    completion.stopReason
      && completion.cancelTrigger !== "send_now"
      && (completion.stopReason !== "end_turn" || completion.cancellationCategory),
  );
  const terminalMetadata = {
    ...(completion.promptId ? { promptId: completion.promptId } : {}),
    ...(completion.stopReason ? { stopReason: completion.stopReason } : {}),
    ...(completion.cancelTrigger ? { cancelTrigger: completion.cancelTrigger } : {}),
    ...(completion.cancellationCategory
      ? { cancellationCategory: completion.cancellationCategory }
      : {}),
    ...(completion.agentResult ? { agentResult: completion.agentResult } : {}),
  };
  const activeIndex = transcript.streamingMessageId
    ? transcript.messages.findIndex((message) => message.id === transcript.streamingMessageId)
    : -1;
  let targetIndex = activeIndex;

  // The bridge can deliver both live `prompt_complete` and durable
  // `turn_completed` notifications. Match by prompt id before touching the
  // currently-streaming bubble: a late duplicate for the superseded turn must
  // never close the replacement turn that has already started.
  if (completion.promptId) {
    const matchingIncomplete = transcript.messages.findIndex(
      (message) =>
        message.role === "assistant"
        && !message.complete
        && message.promptId === completion.promptId,
    );
    if (matchingIncomplete !== -1) {
      targetIndex = matchingIncomplete;
    } else {
      let matchingCompleted = -1;
      for (let index = transcript.messages.length - 1; index >= 0; index -= 1) {
        const message = transcript.messages[index];
        if (
          message.role === "assistant"
          && message.complete
          && message.promptId === completion.promptId
        ) {
          matchingCompleted = index;
          break;
        }
      }
      if (matchingCompleted !== -1) {
        const messages = [...transcript.messages];
        messages[matchingCompleted] = {
          ...messages[matchingCompleted],
          ...terminalMetadata,
          ...(completion.completedAt != null ? { completedAt } : {}),
        };
        return { ...transcript, messages };
      }

      const active = activeIndex >= 0 ? transcript.messages[activeIndex] : undefined;
      if (active?.promptId && active.promptId !== completion.promptId) {
        targetIndex = -1;
      }
    }
  } else if (
    completion.cancelTrigger === "send_now"
    && !transcript.pendingSendNowPromptId
    && activeIndex >= 0
    && transcript.messages[activeIndex].promptId
  ) {
    // A legacy durable cancellation may omit promptId. Once the replacement
    // has an identified active bubble, that cancellation necessarily belongs
    // to the superseded turn. Complete an older unfinished bubble if present;
    // otherwise treat it as a duplicate instead of closing the replacement.
    targetIndex = -1;
    for (let index = activeIndex - 1; index >= 0; index -= 1) {
      const message = transcript.messages[index];
      if (message.role === "assistant" && !message.complete) {
        targetIndex = index;
        break;
      }
    }
  }

  if (targetIndex < 0) {
    if (!preserveEmptyTerminal) return transcript;
    return {
      ...transcript,
      messages: [
        ...transcript.messages,
        {
          id: nextId(),
          role: "assistant",
          parts: [],
          complete: true,
          ...terminalMetadata,
          ...(completion.completedAt != null ? { completedAt } : {}),
        },
      ],
    };
  }

  const targetId = transcript.messages[targetIndex].id;
  const completedMessage: ChatMessage = {
    ...transcript.messages[targetIndex],
    complete: true,
    ...terminalMetadata,
    ...(transcript.messages[targetIndex].startedAt != null ? { completedAt } : {}),
  };
  const messages = [...transcript.messages];
  if (completedMessage.parts.length === 0 && !preserveEmptyTerminal) {
    messages.splice(targetIndex, 1);
  } else {
    messages[targetIndex] = completedMessage;
  }
  return {
    ...transcript,
    messages,
    streamingMessageId:
      transcript.streamingMessageId === targetId ? null : transcript.streamingMessageId,
  };
}

interface ReplayedUserChunk {
  text: string;
  agentText?: string;
  attachments: string[];
  promptIndex?: number;
  hasText: boolean;
  hidden: boolean;
}

/** Normalize one ACP UserMessageChunk, including pre-metadata legacy prompts. */
function normalizeUserChunk(update: Record<string, unknown>): ReplayedUserChunk {
  const content = (update.content ?? {}) as Record<string, unknown>;
  const contentMeta = (content._meta ?? {}) as Record<string, unknown>;
  const chunkMeta = (update._meta ?? {}) as Record<string, unknown>;
  const promptIndexValue = chunkMeta.promptIndex ?? chunkMeta.prompt_index;
  const promptIndex = typeof promptIndexValue === "number" ? promptIndexValue : undefined;
  const hidden = chunkMeta.hideFromScrollback === true || chunkMeta.hide_from_scrollback === true;
  const structuredAttachments = normalizeAttachmentPaths(contentMeta[ATTACHMENTS_META_KEY]);
  const imagePath = content.type === "image" && typeof content.uri === "string"
    ? [content.uri]
    : [];

  if (content.type !== "text") {
    return {
      text: "",
      agentText: undefined,
      attachments: mergePaths(structuredAttachments, imagePath),
      promptIndex,
      hasText: false,
      hidden,
    };
  }

  const rawText = typeof content.text === "string" ? content.text : "";
  const displayText = typeof contentMeta.displayText === "string"
    ? stripInjectedUserContext(contentMeta.displayText)
    : undefined;
  const legacy = parseLegacyAttachmentPrompt(rawText);
  const transport = stripAttachmentTransportContext(rawText);
  const visibleText = displayText ?? legacy.text;
  return {
    text: visibleText,
    agentText: transport.text && transport.text !== visibleText ? transport.text : undefined,
    attachments: mergePaths(structuredAttachments, legacy.attachments),
    promptIndex,
    hasText: true,
    hidden,
  };
}

function applyUserChunk(transcript: SessionTranscript, chunk: ReplayedUserChunk): SessionTranscript {
  const messages = [...transcript.messages];

  // A prompt may replay as text followed by one or more image blocks. Merge
  // every block carrying the same promptIndex into the already-created turn.
  if (chunk.promptIndex !== undefined) {
    const matchingIndex = messages.findIndex(
      (message) => message.role === "user" && message.promptIndex === chunk.promptIndex,
    );
    if (matchingIndex !== -1) {
      const current = messages[matchingIndex];
      const nextText = chunk.hasText && chunk.text && userMessageText(current) !== chunk.text
        ? [...current.parts, { kind: "text" as const, text: chunk.text }]
        : current.parts;
      messages[matchingIndex] = {
        ...current,
        parts: nextText,
        agentText: chunk.agentText ?? current.agentText,
        attachments: mergePaths(current.attachments, chunk.attachments),
      };
      return { ...transcript, messages };
    }
  }

  // Live sends are inserted optimistically just before an empty assistant
  // placeholder. Reconcile the ACP echo with that message instead of adding a
  // duplicate bubble; structured replay metadata enriches it with attachments.
  const placeholderIndex = transcript.streamingMessageId
    ? messages.findIndex((message) => message.id === transcript.streamingMessageId)
    : -1;
  const optimisticIndex = placeholderIndex > 0 ? placeholderIndex - 1 : -1;
  const optimistic = optimisticIndex >= 0 ? messages[optimisticIndex] : undefined;
  if (
    !transcript.pendingSendNowPromptId
    &&
    optimistic?.role === "user"
    && optimistic.promptIndex === undefined
    && (!chunk.hasText || userMessageText(optimistic) === chunk.text)
  ) {
    messages[optimisticIndex] = {
      ...optimistic,
      promptIndex: chunk.promptIndex,
      agentText: chunk.agentText ?? optimistic.agentText,
      attachments: mergePaths(optimistic.attachments, chunk.attachments),
    };
    return { ...transcript, messages };
  }

  // A new replayed user turn closes the previous assistant turn. Insert it
  // before an empty live placeholder if one exists, otherwise append normally.
  const completed = completeStreamingAssistant({ ...transcript, messages });
  const user: ChatMessage = {
    id: nextId(),
    role: "user",
    parts: chunk.text ? [{ kind: "text", text: chunk.text }] : [],
    agentText: chunk.agentText,
    attachments: chunk.attachments,
    promptIndex: chunk.promptIndex,
    complete: true,
  };
  return { ...completed, messages: [...completed.messages, user] };
}

/** Turn an admitted send-now hand-off into the next assistant placeholder. */
function activatePendingSendNow(transcript: SessionTranscript): SessionTranscript {
  const promptId = transcript.pendingSendNowPromptId;
  if (!promptId) return transcript;
  const { messages, id } = ensureStreamingAssistant(
    transcript.messages,
    transcript.streamingMessageId,
    promptId,
  );
  return {
    ...transcript,
    messages,
    streamingMessageId: id,
    pendingSendNowPromptId: null,
  };
}

/** Close the visible turn after a pause/stop request was accepted. The control
 * barrier remains, so a queued late chunk cannot reopen this transcript. */
function finalizeControlledTranscript(
  transcript: SessionTranscript,
  control: SessionControl,
): SessionTranscript {
  const completedAt = Date.now();
  const messages = transcript.messages
    .map((message) =>
      message.id === transcript.streamingMessageId
        ? {
            ...message,
            complete: true,
            stopReason: "cancelled",
            cancelTrigger: control.action,
            ...(message.startedAt != null ? { completedAt } : {}),
          }
        : message
    )
    .filter(
      (message) => !(message.id === transcript.streamingMessageId && message.parts.length === 0),
    );
  return {
    ...transcript,
    control,
    messages,
    streamingMessageId: null,
    pendingSendNowPromptId: null,
    planApprovals: [],
  };
}

function rememberDismissedControlPrompt(transcript: SessionTranscript): string[] {
  const promptId = transcript.control?.promptId;
  if (!promptId || transcript.dismissedControlPromptIds.includes(promptId)) {
    return transcript.dismissedControlPromptIds;
  }
  return [...transcript.dismissedControlPromptIds, promptId].slice(-32);
}

function upsertToolCall(msg: ChatMessage, tc: ToolCallView, streamId?: string): ChatMessage {
  const parts = [...msg.parts];
  const idx = parts.findIndex(
    (p) => p.kind === "tool_call" && p.toolCall.toolCallId === tc.toolCallId
  );
  if (idx === -1) {
    parts.push({ kind: "tool_call", toolCall: tc, ...(streamId ? { streamId } : {}) });
  } else {
    const current = parts[idx];
    parts[idx] = {
      kind: "tool_call",
      toolCall: tc,
      ...(current.streamId || streamId ? { streamId: current.streamId ?? streamId } : {}),
    };
  }
  return { ...msg, parts };
}

/** Derive the top-level mirror fields from a (possibly null) transcript. */
function mirrorOf(t: SessionTranscript | undefined) {
  return {
    messages: t?.messages ?? [],
    streamingMessageId: t?.streamingMessageId ?? null,
    streaming: !t?.control && (
      (t?.streamingMessageId ?? null) != null
      || (t?.pendingSendNowPromptId ?? null) != null
    ),
    sendNowPending: (t?.pendingSendNowPromptId ?? null) != null,
    usage: t?.usage ?? {},
    plan: t?.plan ?? null,
    planRevisionDirty: t?.planRevisionDirty ?? false,
    planMode: t?.planMode ?? false,
    agentMode: t?.agentMode ?? (t?.planMode ? "plan" : "default"),
    planApproval: t?.planApprovals?.[0] ?? null,
    control: t?.control,
  };
}

export const useSessionStore = create<SessionState>((set, get) => {
  /**
   * Apply a pure reducer to one session's transcript and, if that session is
   * the focused one, refresh the top-level mirror in the same `set` call so
   * readers never see a half-updated pair.
   */
  const applyToTranscript = (
    sid: string | null,
    reducer: (t: SessionTranscript) => SessionTranscript,
  ) =>
    set((s) => {
      if (!sid) return s; // nowhere to route — drop
      const prev = s.transcripts[sid] ?? {
        ...EMPTY_TRANSCRIPT,
        control: readPersistedSessionControl(sid),
      };
      const next = reducer(prev);
      if (next === prev) return s;
      const transcripts = { ...s.transcripts, [sid]: next };
      // If we mutated the focused session, keep the mirror in lock-step.
      if (sid === s.sessionId) {
        return { transcripts, ...mirrorOf(next) };
      }
      return { transcripts };
    });

  return {
    sessionId: null,
    transcripts: {},
    messages: [],
    streaming: false,
    sendNowPending: false,
    streamingMessageId: null,
    usage: {},
    plan: null,
    planRevisionDirty: false,
    planApproval: null,
    control: undefined,
    error: null,
    planMode: false,
    agentMode: "default",

    setSession: (id) =>
      set((s) => {
        // Switching focus never destroys transcripts. If we have a cached
        // transcript for the target, it's the truth: arm replay suppression
        // so agentLoadSession's re-streamed history can't merge/overwrite it,
        // and mirror it (streaming stays true if it was mid-stream). If we
        // don't (first open / after restart), seed an empty, non-suppressed
        // transcript that the upcoming replay will fill.
        const hasCache =
          id != null && Object.prototype.hasOwnProperty.call(s.transcripts, id);
        let transcripts = s.transcripts;
        if (id != null && !hasCache) {
          transcripts = {
            ...s.transcripts,
            [id]: {
              ...EMPTY_TRANSCRIPT,
              control: readPersistedSessionControl(id),
            },
          };
        } else if (id != null && hasCache) {
          const t = s.transcripts[id];
          if (!t.suppressReplay) {
            transcripts = {
              ...s.transcripts,
              [id]: { ...t, suppressReplay: true },
            };
          }
        }
        const focused = id != null ? transcripts[id] : undefined;
        return {
          sessionId: id,
          transcripts,
          ...mirrorOf(focused),
          error: null,
        };
      }),

    reset: () =>
      set(() => ({
        sessionId: null,
        // Keep transcripts (so a stray background stream can still land and a
        // later refocus restores it); just clear the focused mirror.
        ...mirrorOf(undefined),
        error: null,
      })),

    startStreaming: (sessionId, promptId) => {
      // Optimistically insert an empty assistant placeholder so the avatar +
      // "preparing" loading row appears immediately after the user message,
      // instead of a blank gap until the first streamed chunk arrives.
      const sid = sessionId ?? get().sessionId;
      if (!sid) return;
      // A new prompt is an explicit user action to continue this session.
      persistSessionControl(sid);
      applyToTranscript(sid, (t) => {
        const id = nextId();
        const placeholder: ChatMessage = {
          id,
          role: "assistant",
          parts: [],
          ...(promptId ? { promptId } : {}),
          complete: false,
          startedAt: Date.now(),
        };
        return {
          ...t,
          dismissedControlPromptIds: rememberDismissedControlPrompt(t),
          control: undefined,
          streamingMessageId: id,
          messages: [...t.messages, placeholder],
        };
      });
      // Local error banner doesn't belong to the transcript; clear it globally.
      set({ error: null });
    },

    requestSendNow: (promptId, sessionId) => {
      const sid = sessionId ?? get().sessionId;
      if (!sid || !promptId) return;
      applyToTranscript(sid, (t) => ({
        ...t,
        pendingSendNowPromptId: promptId,
      }));
      set({ error: null });
    },

    rejectSendNow: (promptId, sessionId) => {
      const sid = sessionId ?? get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (t) =>
        t.pendingSendNowPromptId === promptId
          ? { ...t, pendingSendNowPromptId: null }
          : t
      );
    },

    requestControl: (sessionId, action, promptId) => {
      applyToTranscript(sessionId, (transcript) => ({
        ...transcript,
        control: {
          action,
          phase: action === "pause" ? "pausing" : "stopping",
          ...(promptId ? { promptId } : {}),
          requestedAt: Date.now(),
        },
      }));
      set({ error: null });
    },

    confirmControl: (sessionId, action) => {
      const current = get().transcripts[sessionId]?.control;
      // A terminal event may have won the race and cleared this request.
      if (!current || current.action !== action) return;
      const stable: SessionControl = {
        ...current,
        phase: action === "pause" ? "paused" : "stopped",
      };
      persistSessionControl(sessionId, stable);
      applyToTranscript(sessionId, (transcript) =>
        transcript.control?.action === action
          ? finalizeControlledTranscript(transcript, stable)
          : transcript
      );
    },

    rejectControl: (sessionId, action) => {
      const current = get().transcripts[sessionId]?.control;
      if (!current || current.action !== action) return;
      persistSessionControl(sessionId);
      applyToTranscript(sessionId, (transcript) => ({
        ...transcript,
        control: undefined,
      }));
    },

    resumeSession: (sessionId) => {
      persistSessionControl(sessionId);
      applyToTranscript(sessionId, (transcript) =>
        transcript.control
          ? {
              ...transcript,
              dismissedControlPromptIds: rememberDismissedControlPrompt(transcript),
              control: undefined,
            }
          : transcript
      );
    },

    restoreControl: (sessionId, control) => {
      persistSessionControl(sessionId, control);
      applyToTranscript(sessionId, (transcript) => ({
        ...transcript,
        control,
      }));
    },

    rollbackPendingTurn: () => {
      const sid = get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (t) => {
        const placeholderIndex = t.messages.findIndex(
          (message) => message.id === t.streamingMessageId,
        );
        if (placeholderIndex < 0) return t;
        const placeholder = t.messages[placeholderIndex];
        if (placeholder.parts.length > 0) return t;
        const messages = [...t.messages];
        messages.splice(placeholderIndex, 1);
        const previous = messages[placeholderIndex - 1];
        if (previous?.role === "user") messages.splice(placeholderIndex - 1, 1);
        return { ...t, messages, streamingMessageId: null };
      });
    },

    discardMessagesFrom: (sessionId, messageId) => {
      applyToTranscript(sessionId, (transcript) => {
        const index = transcript.messages.findIndex((message) => message.id === messageId);
        if (index < 0) return transcript;
        return {
          ...transcript,
          messages: transcript.messages.slice(0, index),
          streamingMessageId: null,
          pendingSendNowPromptId: null,
          planApprovals: [],
        };
      });
    },

    markComplete: (p) => {
      // Route by the complete's own sessionId so a background session finishing
      // after we switched away finalizes ITS transcript (clearing its streaming
      // flag) instead of clobbering the focused one.
      const target = (p as { sessionId?: string }).sessionId ?? get().sessionId;
      const targetTranscript = target ? get().transcripts[target] : undefined;
      const requested = targetTranscript?.control;
      const requestedAction = p.stopReason === "cancelled" ? requested?.action : undefined;
      const activeMessage = targetTranscript?.messages.find(
        (message) => message.id === targetTranscript.streamingMessageId,
      );
      const activePromptId = targetTranscript?.pendingSendNowPromptId ?? activeMessage?.promptId;
      const isDismissedControl = !!p.promptId
        && !!targetTranscript?.dismissedControlPromptIds.includes(p.promptId);
      const targetsOlderTurn = !!p.promptId && !!activePromptId && p.promptId !== activePromptId;
      const preserveCurrentControl = isDismissedControl || targetsOlderTurn;
      const terminalAction = preserveCurrentControl
        ? undefined
        : p.cancelTrigger === "pause" || p.cancelTrigger === "stop"
          ? p.cancelTrigger
          : requestedAction ?? (terminalSessionStatus(p) === "stopped" ? "stop" : undefined);
      if (target && !preserveCurrentControl) {
        if (terminalAction) {
          persistSessionControl(target, {
            action: terminalAction,
            phase: terminalAction === "pause" ? "paused" : "stopped",
            ...(p.promptId ? { promptId: p.promptId } : {}),
            requestedAt: requested?.requestedAt ?? Date.now(),
          });
        } else if (p.cancelTrigger !== "send_now") {
          persistSessionControl(target);
        }
      }
      applyToTranscript(target, (t) => {
        const completed = completeStreamingAssistant(t, {
          promptId: p.promptId,
          stopReason: p.stopReason,
          cancelTrigger: p.cancelTrigger,
          cancellationCategory: p.cancellationCategory,
          agentResult: p.agentResult,
        });
        const control: SessionControl | undefined = preserveCurrentControl
          ? completed.control
          : terminalAction
            ? {
                action: terminalAction,
                phase: terminalAction === "pause" ? "paused" : "stopped",
                ...(p.promptId ? { promptId: p.promptId } : {}),
                requestedAt: requested?.requestedAt ?? Date.now(),
              }
            : p.cancelTrigger === "send_now"
              ? completed.control
              : undefined;
        const preservePendingSendNow = preserveCurrentControl
          || (p.stopReason === "cancelled" && p.cancelTrigger === "send_now");
        return {
          ...completed,
          control,
          pendingSendNowPromptId: preservePendingSendNow
            ? completed.pendingSendNowPromptId
            : null,
          usage: { ...t.usage, ...p.usage },
        };
      });
    },

    failAllStreaming: (reason = "error", agentResult) =>
      set((state) => {
        let changed = false;
        const transcripts: Record<string, SessionTranscript> = Object.fromEntries(
          Object.entries(state.transcripts).map(([sessionId, transcript]) => {
            if (
              !transcript.streamingMessageId
              && !transcript.pendingSendNowPromptId
              && transcript.planApprovals.length === 0
            ) {
              return [sessionId, transcript];
            }
            changed = true;
            return [
              sessionId,
              {
                ...completeStreamingAssistant(transcript, {
                  stopReason: reason,
                  agentResult,
                }),
                pendingSendNowPromptId: null,
                planApprovals: [],
              },
            ];
          }),
        );
        if (!changed) return state;
        const focused = state.sessionId ? transcripts[state.sessionId] : undefined;
        return { transcripts, ...mirrorOf(focused) };
      }),

    // A banner is presentation state, not a lifecycle signal. Callers must use
    // markComplete/failAllStreaming/stopStreaming for an actual turn boundary;
    // otherwise a failed cancel request or an early turn-error notification
    // would incorrectly kill a stream that is still owned by the agent.
    setError: (error) => set({ error }),

    stopStreaming: (sessionId) => {
      // Cancel button: keep whatever already streamed, just close the turn.
      const sid = sessionId ?? get().sessionId;
      if (!sid) {
        set({ error: null });
        return;
      }
      applyToTranscript(sid, (t) => {
        if (t.streamingMessageId == null && t.pendingSendNowPromptId == null) return t;
        const completedAt = Date.now();
        const messages = t.messages
          .map((m) =>
            m.id === t.streamingMessageId
              ? {
                  ...m,
                  complete: true,
                  stopReason: "cancelled",
                  ...(m.startedAt != null ? { completedAt } : {}),
                }
              : m
          )
          .filter(
            (m) => !(m.id === t.streamingMessageId && m.parts.length === 0)
          );
        return {
          ...t,
          messages,
          streamingMessageId: null,
          pendingSendNowPromptId: null,
        };
      });
    },

    dropSessionCache: (id) =>
      set((s) => {
        if (!Object.prototype.hasOwnProperty.call(s.transcripts, id)) return {};
        const transcripts = { ...s.transcripts };
        delete transcripts[id];
        // If we just dropped the focused session, refresh the mirror to empty
        // (a subsequent setSession+load will refill it from EchoAgent).
        if (id === s.sessionId) {
          return { transcripts, ...mirrorOf(undefined) };
        }
        return { transcripts };
      }),

    clearReplaySuppression: (id) => {
      const target = id ?? get().sessionId;
      if (!target) return;
      applyToTranscript(target, (t) =>
        t.suppressReplay ? { ...t, suppressReplay: false } : t
      );
    },

    finalizeIncompleteReplay: (id, knownCompleted = false) => {
      const target = id ?? get().sessionId;
      if (!target) return;
      applyToTranscript(target, (transcript) => {
        const activeId = transcript.streamingMessageId;
        if (!activeId) return transcript;
        const active = transcript.messages.find((message) => message.id === activeId);
        // Cached live turns do not have replayed=true. Never terminate them:
        // they may still be producing updates while the user changes pages.
        if (!active?.replayed) return transcript;
        const hasUnfinishedTool = active.parts.some(
          (part) => part.kind === "tool_call" && part.toolCall.status === "in_progress",
        );
        const hasVisibleReply = active.parts.some(
          (part) => part.kind === "text" && part.text.trim().length > 0,
        );
        const messages = transcript.messages.map((message) => {
          if (message.id !== activeId) return message;
          const parts = message.parts.map((part) => {
            if (part.kind !== "tool_call" || part.toolCall.status !== "in_progress") {
              return part;
            }
            return {
              ...part,
              toolCall: {
                ...part.toolCall,
                status: "failed" as const,
                content: [
                  ...part.toolCall.content,
                  {
                    type: "text" as const,
                    text: "历史会话未记录该操作的完成结果；恢复时已结束此未完成轮次。",
                  },
                ],
              },
            };
          });
          return {
            ...message,
            parts,
            complete: true,
            ...(knownCompleted ? { stopReason: "end_turn" } : {}),
            ...(!knownCompleted && (!hasVisibleReply || hasUnfinishedTool)
              ? {
                  stopReason: "cancelled",
                  cancellationCategory: "session_replay_incomplete",
                  agentResult: "上次执行在应用退出或 Runtime 中断前未留下完成事件，已结束历史恢复中的假运行状态。",
                }
              : {}),
          };
        });
        return {
          ...transcript,
          messages,
          streamingMessageId: null,
          pendingSendNowPromptId: null,
          planApprovals: [],
        };
      });
    },

    pushUser: (text, attachments = [], sessionId, agentText) => {
      const sid = sessionId ?? get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (t) => ({
        ...t,
        messages: [
          ...t.messages,
          {
            id: nextId(),
            role: "user",
            parts: [{ kind: "text", text }],
            attachments: normalizeAttachmentPaths(attachments),
            ...(agentText && agentText !== text ? { agentText } : {}),
            complete: true,
          },
        ],
      }));
    },

    pushAssistant: (text) => {
      const sid = get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (t) => ({
        ...t,
        streamingMessageId: null,
        pendingSendNowPromptId: null,
        messages: [
          ...t.messages,
          {
            id: nextId(),
            role: "assistant",
            parts: [{ kind: "text", text }],
            complete: true,
          },
        ],
      }));
    },

    applyUpdate: (u) => {
      const foreignSid = (u as { __sessionId?: string }).__sessionId;
      // Side-channel (inspiration panel) still gets a copy when registered.
      if (foreignSid) {
        const cb = foreignUpdateListeners.get(foreignSid);
        if (cb) cb(u);
      }
      // Route into the transcript this update belongs to. No attribution and
      // no focused session → nowhere to put it, drop (don't pollute).
      const target = foreignSid ?? get().sessionId;
      if (!target) return;

      // ACP's SessionUpdate uses `sessionUpdate` as the tag field (not `type`).
      // Some updates may use `type` (older path); accept both.
      const t = ((u as { sessionUpdate?: string }).sessionUpdate ??
        (u as { type?: string }).type) as string;

      // Replay-suppression gate: when we refocused a cached transcript, the
      // history EchoAgent re-streams must NOT touch the message stream (it would
      // merge turns / overwrite usage). Usage/plan are also part of the cache,
      // so suppress them too during replay.
      const REPLAY_SUPPRESSED = new Set([
        "user_message_chunk",
        "agent_message_chunk",
        "agent_thought_chunk",
        "tool_call",
        "tool_call_update",
        "usage_update",
        "plan",
        "turn_completed",
      ]);
      const CONTROL_SUPPRESSED = new Set([
        "user_message_chunk",
        "agent_message_chunk",
        "agent_thought_chunk",
        "tool_call",
        "tool_call_update",
        "usage_update",
        "plan",
        "plan_approval_request",
      ]);
      const updateMeta = (u as unknown as { _meta?: Record<string, unknown> })._meta;
      const isReplay = updateMeta?.isReplay === true;
      const streamId = typeof updateMeta?.streamStartMs === "number"
        && Number.isFinite(updateMeta.streamStartMs)
        ? String(updateMeta.streamStartMs)
        : typeof updateMeta?.streamStartMs === "string" && updateMeta.streamStartMs
          ? updateMeta.streamStartMs
          : undefined;
      const updatePromptId = typeof updateMeta?.promptId === "string"
        ? updateMeta.promptId
        : undefined;

      applyToTranscript(target, (tr) => {
        if (tr.suppressReplay && REPLAY_SUPPRESSED.has(t)) return tr;
        if (
          !isReplay
          && updatePromptId
          && tr.dismissedControlPromptIds.includes(updatePromptId)
          && CONTROL_SUPPRESSED.has(t)
        ) {
          return tr;
        }
        // Pause/stop is a session-level event fence. EchoAgent can still have
        // already-buffered live updates in the renderer queue after cancel;
        // those must never recreate a streaming message. Historical replay is
        // allowed so reopening a controlled session can still show its history.
        if (tr.control && !isReplay && CONTROL_SUPPRESSED.has(t)) return tr;

        // Extract text delta from a content field that may be a single
        // TextContent object ({type:"text",text:"..."}) OR an array of them.
        const extractDelta = (content: unknown): string => {
          if (!content) return "";
          if (Array.isArray(content)) {
            // Cap while extracting, before allocating a joined intermediate.
            // A malformed/provider-controlled update may contain an enormous
            // number of blocks or many multi-megabyte strings; appendText's
            // final cap alone would be too late to avoid the temporary spike.
            const chunks: string[] = [];
            let remaining = MAX_MESSAGE_TEXT_CHARS;
            for (const block of content.slice(0, MAX_TOOL_CONTENT_BLOCKS)) {
              if (remaining === 0) break;
              const text = boundedString(
                (block as { text?: unknown } | null)?.text,
                remaining,
              );
              if (!text) continue;
              chunks.push(text);
              remaining -= text.length;
            }
            return chunks.join("");
          }
          return boundedString(
            (content as { text?: unknown }).text,
            MAX_MESSAGE_TEXT_CHARS,
          );
        };

        switch (t) {
          case "user_message_chunk": {
            const chunk = normalizeUserChunk(u as unknown as Record<string, unknown>);
            if (chunk.hidden || (!chunk.text && chunk.attachments.length === 0)) return tr;
            return activatePendingSendNow(applyUserChunk(tr, chunk));
          }
          case "agent_message_chunk": {
            const delta = extractDelta((u as { content?: unknown }).content);
            if (!delta) return tr;
            const { messages, id } = ensureStreamingAssistant(
              tr.messages,
              tr.streamingMessageId,
              tr.pendingSendNowPromptId ?? undefined,
              isReplay,
            );
            const idx = messages.findIndex((m) => m.id === id);
            messages[idx] = appendText(messages[idx], "text", delta, streamId);
            return {
              ...tr,
              messages: [...messages],
              streamingMessageId: id,
              pendingSendNowPromptId: null,
            };
          }
          case "agent_thought_chunk": {
            const delta = extractDelta((u as { content?: unknown }).content);
            if (!delta) return tr;
            const { messages, id } = ensureStreamingAssistant(
              tr.messages,
              tr.streamingMessageId,
              tr.pendingSendNowPromptId ?? undefined,
              isReplay,
            );
            const idx = messages.findIndex((m) => m.id === id);
            messages[idx] = appendText(messages[idx], "thought", delta, streamId);
            return {
              ...tr,
              messages: [...messages],
              streamingMessageId: id,
              pendingSendNowPromptId: null,
            };
          }
          case "tool_call": {
            const raw = u as unknown as Record<string, unknown>;
            const { messages, id } = ensureStreamingAssistant(
              tr.messages,
              tr.streamingMessageId,
              tr.pendingSendNowPromptId ?? undefined,
              isReplay,
            );
            const idx = messages.findIndex((m) => m.id === id);
            // ACP omits `kind` when it's "other" and `status` when it's
            // "pending" (the defaults). Provide sensible fallbacks.
            const status = (raw.status as string) || "in_progress";
            const officeReceipt = officeReceiptFromRawOutput(raw.rawOutput ?? raw.raw_output);
            const view: ToolCallView = {
              toolCallId:
                boundedString(raw.toolCallId ?? raw.tool_call_id, 4_096),
              title: boundedString(raw.title, 4_096),
              kind: boundedString(raw.kind, 128) || "other",
              status: status === "completed" || status === "failed"
                ? status
                : "in_progress",
              content: [
                ...normalizeToolCallContent(raw.content),
                ...(officeReceipt ? [{ type: "text" as const, text: JSON.stringify(officeReceipt) }] : []),
              ],
              rawInput: boundedRawInput(raw.rawInput ?? raw.raw_input),
              officeReceipt: officeReceipt ?? undefined,
            };
            messages[idx] = upsertToolCall(messages[idx], view, streamId);
            return {
              ...tr,
              messages: [...messages],
              streamingMessageId: id,
              pendingSendNowPromptId: null,
            };
          }
          case "tool_call_update": {
            // ACP serializes ToolCallUpdate with `#[serde(flatten)]` on the
            // fields struct, so status/content/etc. sit at the TOP LEVEL of
            // the JSON alongside toolCallId — there is no nested `update` key.
            const raw = u as unknown as Record<string, unknown>;
            const tcId =
              (raw.toolCallId as string) ?? (raw.tool_call_id as string);
            const deltaFields: Record<string, unknown> = {};
            if (raw.kind !== undefined) deltaFields.kind = boundedString(raw.kind, 128) || "other";
            if (raw.title !== undefined) deltaFields.title = boundedString(raw.title, 4_096);
            if (raw.status !== undefined) {
              deltaFields.status = raw.status === "completed" || raw.status === "failed"
                ? raw.status
                : "in_progress";
            }
            if (raw.content !== undefined) deltaFields.content = raw.content;
            if (raw.rawInput !== undefined) deltaFields.rawInput = boundedRawInput(raw.rawInput);
            if (raw.raw_input !== undefined && deltaFields.rawInput === undefined)
              deltaFields.rawInput = boundedRawInput(raw.raw_input);
            if (deltaFields.content !== undefined) {
              deltaFields.content = normalizeToolCallContent(
                deltaFields.content
              );
            }
            const officeReceipt = officeReceiptFromRawOutput(raw.rawOutput ?? raw.raw_output);
            if (officeReceipt) {
              deltaFields.officeReceipt = officeReceipt;
              deltaFields.content = [
                ...((deltaFields.content as ToolCallContent[] | undefined) ?? []),
                { type: "text", text: JSON.stringify(officeReceipt) },
              ];
            }
            // Patch the matching tool card across the transcript (not only the
            // streaming message — a late update may target an older turn).
            const messages = tr.messages.map((m) => {
              const has = m.parts.some(
                (p) =>
                  p.kind === "tool_call" && p.toolCall.toolCallId === tcId
              );
              if (!has) return m;
              const parts = m.parts.map((p) => {
                if (p.kind !== "tool_call") return p;
                if (p.toolCall.toolCallId !== tcId) return p;
                return {
                  ...p,
                  toolCall: { ...p.toolCall, ...deltaFields } as ToolCallView,
                };
              });
              return { ...m, parts };
            });
            return { ...tr, messages };
          }
          case "usage_update": {
            const uu = u as unknown as UsageUpdate;
            return { ...tr, usage: { ...tr.usage, ...uu.usage } };
          }
          case "plan": {
            const uu = u as unknown as PlanUpdate;
            return { ...tr, plan: uu.plan ?? null, planRevisionDirty: false };
          }
          case "current_mode_update": {
            const raw = u as unknown as Record<string, unknown>;
            const modeId = raw.currentModeId ?? raw.current_mode_id;
            const knownModes: AgentMode[] = [
              "default",
              "ask",
              "plan",
              "browser_use",
              "computer_use",
            ];
            const agentMode = typeof modeId === "string" && knownModes.includes(modeId as AgentMode)
              ? modeId as AgentMode
              : "default";
            return { ...tr, planMode: agentMode === "plan", agentMode };
          }
          case "plan_approval_request": {
            const request = u as unknown as PlanApprovalRequest;
            const existing = tr.planApprovals ?? [];
            if (existing.some((item) => item.requestId === request.requestId)) return tr;
            return { ...tr, planApprovals: [...existing, request], planRevisionDirty: false };
          }
          case "plan_approval_resolved": {
            const requestId = (u as unknown as { requestId?: string }).requestId;
            if (!requestId) return tr;
            return {
              ...tr,
              planApprovals: (tr.planApprovals ?? []).filter(
                (item) => item.requestId !== requestId,
              ),
            };
          }
          case "turn_completed":
            {
              const raw = u as unknown as Record<string, unknown>;
              const meta = raw._meta && typeof raw._meta === "object"
                ? raw._meta as Record<string, unknown>
                : {};
              const completedAt = typeof meta.agentTimestampMs === "number"
                ? meta.agentTimestampMs
                : undefined;
              const completed = completeStreamingAssistant(tr, {
                promptId: boundedString(raw.prompt_id ?? raw.promptId, 4_096),
                stopReason: boundedString(raw.stop_reason ?? raw.stopReason, 128),
                cancelTrigger: boundedString(meta.cancelTrigger, 128),
                cancellationCategory: boundedString(meta.cancellationCategory, 128),
                agentResult: boundedString(raw.agent_result ?? raw.agentResult, MAX_MESSAGE_TEXT_CHARS),
                completedAt,
              });
              return {
                ...completed,
                pendingSendNowPromptId:
                  (raw.stop_reason ?? raw.stopReason) === "cancelled"
                  && meta.cancelTrigger === "send_now"
                    ? completed.pendingSendNowPromptId
                    : null,
              };
            }
          default:
            return tr;
        }
      });
    },

    setMessages: (msgs) => {
      const sid = get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (t) => ({ ...t, messages: msgs }));
    },

    setPlan: (plan, options) => {
      const sid = options?.sessionId ?? get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (t) => ({ ...t, plan, planRevisionDirty: Boolean(plan && options?.localRevision) }));
    },

    markPlanRevisionSynced: (plan, sessionId) => {
      const sid = sessionId ?? get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (t) => t.plan === plan && t.planRevisionDirty
        ? { ...t, planRevisionDirty: false }
        : t);
    },

    setPlanMode: (enabled, sessionId) => {
      const sid = sessionId ?? get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (transcript) => ({
        ...transcript,
        planMode: enabled,
        // A delayed legacy "plan disabled" notification can arrive after a
        // CurrentModeUpdate for Browser/Computer Use. Only leave `plan`; never
        // overwrite a newer authoritative non-plan mode.
        agentMode: enabled
          ? "plan"
          : transcript.agentMode === "plan"
            ? "default"
            : transcript.agentMode ?? "default",
      }));
    },

    setAgentMode: (mode, sessionId) => {
      const sid = sessionId ?? get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (transcript) => ({
        ...transcript,
        planMode: mode === "plan",
        agentMode: mode,
      }));
    },

    requestPlanApproval: (request) => {
      applyToTranscript(request.sessionId, (transcript) => {
        const pending = transcript.planApprovals ?? [];
        if (pending.some((item) => item.requestId === request.requestId)) return transcript;
        return { ...transcript, planApprovals: [...pending, request], planRevisionDirty: false };
      });
    },

    dismissPlanApproval: (requestId, sessionId) => {
      const sid = sessionId ?? get().sessionId;
      if (!sid) return;
      applyToTranscript(sid, (transcript) => ({
        ...transcript,
        planApprovals: (transcript.planApprovals ?? []).filter(
          (item) => item.requestId !== requestId,
        ),
      }));
    },
  };
});

export type { ToolCallUpdate };
