import { useWorkspaceMentions } from "@/lib/use-workspace-mentions";
import { useEffect, useMemo, useRef, useState } from "react";
import { draftAttachments, saveDraftAttachments } from "@/lib/draft-attachments";
import { draftRevision, useDraftRevision } from "@/lib/draft-lifecycle";
import { useStorageHealth } from "@/lib/durable-ui-state";
import { ArrowUp, Square, Globe2, Mic, Monitor, X, type LucideIcon } from "lucide-react";
import { ChevronDownIcon } from "@/foundation/components/Icon/icons";
import { ModelSelector, type ModelOption } from "./ModelSelector";
import { ThumbImg } from "./experts-panel/shared/ThumbImg";
import { ContextUsagePill } from "./ContextUsagePill";
import { estimateSendCost } from "@/lib/token-estimate";
import {
  blocks,
  assemblePrompt,
  blockLabel,
} from "@/lib/content-blocks";
import {
  createInputHistory,
  pushHistory,
  navigateHistory,
  type InputHistory,
} from "@/lib/input-history";
import { WorkspacePicker } from "./WorkspacePicker";
import { PermissionPicker } from "./PermissionPicker";
import { AutomationBoundaryNotice, type PermissionRole } from "./AutomationBoundaryNotice";
import { usePermissionModeStore } from "@/stores/permission-mode-store";
import { SlashCommands, type SlashCommandsHandle } from "./SlashCommands";
import { AtMentionMenu, type AtMentionHandle, type AtMentionSymbol } from "@/components/AtMentionMenu";
import {
  isClientSlashCommand,
  parseSlashInvocation,
  replaceSlashToken,
  slashTokenAtCursor,
  type SlashCommandInvocation,
} from "@/lib/slash-commands";
import { atTokenAtCursor, replaceAtToken } from "@/lib/at-commands";
import { InputAddMenu } from "./InputAddMenu";
import { useMeetingTranscriptionCapability } from "@/lib/use-meeting-transcription-capability";
import { KnowledgePicker } from "./KnowledgePicker";
import {
  automationCapabilities,
  type AutomationCapabilities,
  type AutomationMode,
} from "@/lib/automation-client";
import {
  registerAsrProvider,
  getActiveAsr,
  createWebSpeechAsrProvider,
} from "@/lib/voice-contract";
import type { AgentEntry } from "@/lib/types";
import {
  filesystemPickFiles,
  filesystemAttachmentStats,
  discardAttachmentBlob,
  saveAttachmentBlob,
  type AttachmentFileStat,
  type WorkspaceInfo,
} from "@/lib/agent-client";
import {
  extractFilesFromClipboard,
  blobToBytes,
} from "@/lib/clipboard-paste";
import { AttachmentKind, classifyAttachment } from "@/lib/user-message";
import {
  parseSessionControlIntent,
  type SessionControlAction,
} from "@/lib/session-control";
import { getCurrentWebview } from "@tauri-apps/api/webview";

const MAX_ATTACHMENT_COUNT = 20;
const MAX_ATTACHMENT_FILE_BYTES = 20 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES = 64 * 1024 * 1024;
const MAX_ATTACHMENT_INSPECTION_COUNT = 100;

interface AttachmentAdmissionSummary {
  added: number;
  acceptedPaths: string[];
  duplicates: number;
  oversized: number;
  countLimited: number;
  totalLimited: number;
}

interface DraftSubmission {
  scope: string | undefined;
  scopeEpoch: number;
  revision: number;
  localRevision: number;
}

function discardUnsentAttachments(paths: string[]) {
  for (const path of paths) {
    void discardAttachmentBlob(path).catch(() => undefined);
  }
}

/**
 * EchoAgent 风格输入卡片(圆角16):左下 +,右下 Auto 下拉/麦克风/发送;
 * showMeta 时卡片内部底部显示“选择工作目录/默认权限”。
 * showDisclaimer 时卡片下方渲染免责声明行。
 * apiReady=false 时输入禁用,点击卡片引导打开设置。
 */
export function Composer({
  streaming,
  cancelling = false,
  sendNowPending = false,
  awaitingQuestion = false,
  disabled,
  sendDisabled = false,
  filePaths,
  workspaceSymbols,
  onSend,
  onCancel,
  onControl,
  placeholder,
  apiReady = true,
  setupHint = "请先在「设置 → 模型」检查可用模型和连接",
  onOpenSettings,
  onToast,
  showMeta = false,
  showDisclaimer = false,
  permissionInline = false,
  // Model picker
  modelId,
  modelSummary,
  modelLoading = false,
  models,
  onModelChange,
  // Workspace picker
  cwd,
  workspaces,
  onSelectWorkspace,
  automaticWorkspace,
  defaultWorkspaceCwd,
  // Seed text (from HomePage chips). Consumed once, then cleared via callback.
  initialText,
  onInitialTextConsumed,
  // 不可编辑的"操作类型"标签(首页选中能力分类时插入),显示在输入框内首行。
  sceneTag,
  onClearSceneTag,
  // 受控填充:externalTextNonce 变化时把 externalText 写入输入框(用于点击模板)。
  externalText,
  externalAttachments,
  externalTextNonce,
  // 按会话持久化的草稿:切换 sessionId 时按 draft 回填,每次输入回写 store。
  // 不传这三者时退化为纯组件内 state(向后兼容旧调用方/测试)。
  draft,
  draftKey,
  onDraftChange,
  onSelectExpert,
  onSelectSkill,
  onNavigateConnectors,
  automationMode = "default",
  automationModeDisabled = false,
  showAutomationModeBadge = false,
  onAutomationModeChange,
  // Slash-command context and desktop-owned command execution.
  commandSessionId,
  slashCommandsEnabled = true,
  commandRefreshKey,
  onClientSlashCommand,
  /** While a turn is active, submissions go to the durable follow-up queue. */
  onEnqueue,
  /** Name of the expert currently bound to this session (shown as badge in footer). */
  activeExpertName,
  /** Local avatar path for the expert badge. */
  activeExpertAvatar,
  /** Dismiss the active expert (clear pending selection). Called from the × on either chip. */
  onDismissExpert,
  /** Session id powering the context-usage pill (omit on the home page). */
  usageSessionId,
  usageMsgCount,
  knowledgeSessionId,
  knowledgeDeferred = false,
  onOpenKnowledgeBase,
  onOpenMeetingMinutes,
  onOpenOrganization,
  mentionMembers,
  onMentionMember,
}: {
  streaming: boolean;
  /** A cancellation request is in flight; keep the stop action single-shot. */
  cancelling?: boolean;
  /** Runtime is cancelling the old turn and admitting its replacement. */
  sendNowPending?: boolean;
  /** A structured AskUserQuestion card owns input until it is resolved. */
  awaitingQuestion?: boolean;
  disabled?: boolean;
  /** Block execution while preserving an editable draft (e.g. a missing task directory). */
  sendDisabled?: boolean;
  /** SP4: workspace-relative file paths to back the `@` candidate picker. */
  filePaths?: string[];
  /** SP4: workspace symbols (mirrors `SymbolIndexClient.symbols()`). */
  workspaceSymbols?: AtMentionSymbol[];
  mentionMembers?: { id: string; name: string; role?: string }[];
  onMentionMember?: (memberId: string) => void;
  onSend: (text: string, attachments?: string[]) => boolean | void | Promise<boolean | void>;
  onCancel: () => boolean | void | Promise<boolean | void>;
  /** Exact local pause/stop commands are handled without sending them to AI. */
  onControl?: (
    action: SessionControlAction,
  ) => boolean | void | Promise<boolean | void>;
  placeholder?: string;
  apiReady?: boolean;
  /** Message shown when the composer is unavailable. */
  setupHint?: string;
  onOpenSettings?: () => void;
  onPlaceholder?: (label: string) => void;
  /** Surface transient feedback (permission rule save errors, etc.). */
  onToast?: (msg: string) => void;
  showMeta?: boolean;
  /** Show "内容由 AI 生成" disclaimer below card (chat page). */
  showDisclaimer?: boolean;
  /** 把权限选择器放进卡片内 footer（+ 之后），匹配 EchoAgent 本地助理页；为 true 时不再渲染卡片外 meta 行。 */
  permissionInline?: boolean;
  /** Currently selected model id (shown on the model trigger). */
  modelId?: string;
  /** Label when a container manages several member models outside this input. */
  modelSummary?: string;
  modelLoading?: boolean;
  /** Available models for the picker. */
  models?: ModelOption[];
  onModelChange?: (id: string) => void;
  /** Currently active working directory. */
  cwd?: string;
  workspaces?: WorkspaceInfo[];
  onSelectWorkspace?: (cwd: string) => void;
  automaticWorkspace?: boolean;
  defaultWorkspaceCwd?: string;
  /** Optional initial text to seed the input (one-shot, then cleared). */
  initialText?: string;
  onInitialTextConsumed?: () => void;
  /**
   * 首页"操作类型"标签(复刻 EchoAgent 的 scene tag):选中能力分类后插入
   * 到输入框内首行的黑色标签,带图标与 × 删除按钮。发送时作为上下文前缀。
   */
  sceneTag?: { label: string; icon: LucideIcon } | null;
  /** 点击标签 × 时清空该标签(并清空相关输入)。 */
  onClearSceneTag?: () => void;
  /** 受控填充的内容(通常是某个模板对应的完整 prompt)。 */
  externalText?: string;
  /** Files restored together with externalText (for edit-and-resend). */
  externalAttachments?: string[];
  /** 递增的 nonce;变化时把 externalText 写入输入框并聚焦。 */
  externalTextNonce?: number;
  /**
   * 持久化草稿:切到某会话时(draftKey 变化)把 draft 回填到输入框。
   * 与 externalText 不同,这是"用户已经敲下的字",回填时不触发 onDraftChange。
   */
  draft?: string;
  /** 草稿作用域标识(通常是 sessionId 或哨兵)。变化时触发回填。 */
  draftKey?: string | number;
  /** 用户输入时回调,父组件据此把草稿写回 store。 */
  onDraftChange?: (text: string) => void;
  /** 加号菜单:选择专家。 */
  onSelectExpert?: (agent: AgentEntry) => void;
  /** 加号菜单:选择技能(插入 /skillName)。 */
  onSelectSkill?: (skillName: string) => void;
  /** 加号菜单:跳转到连接器管理面板。 */
  onNavigateConnectors?: () => void;
  /** Optional task-scoped webpage/computer capability exposed from the + menu. */
  automationMode?: AutomationMode;
  /** Prevent capability changes while a task is being created or is running. */
  automationModeDisabled?: boolean;
  /** Home shows the selected capability as a removable chip; chats use the status toolbar. */
  showAutomationModeBadge?: boolean;
  onAutomationModeChange?: (mode: AutomationMode) => void | Promise<void>;
  /** Current session used to request the runtime's context-aware command catalog. */
  commandSessionId?: string;
  /** Containers with several Runtime sessions expose their own controls. */
  slashCommandsEnabled?: boolean;
  /** Incremented when the runtime publishes available_commands_update. */
  commandRefreshKey?: number;
  /** Execute commands owned by the desktop shell (/new, /settings, /plan, ...). */
  onClientSlashCommand?: (
    invocation: SlashCommandInvocation,
  ) => boolean | void | Promise<boolean | void>;
  /** 流式时额外提供「加入待发送队列」。 */
  onEnqueue?: (text: string, attachments?: string[]) => boolean | void | Promise<boolean | void>;
  /** Name of the expert currently bound to this session (shown as badge in footer). */
  activeExpertName?: string;
  /** Local avatar path for the expert badge. */
  activeExpertAvatar?: string;
  /** Dismiss the active expert (clear pending selection). Called from the × on either chip. */
  onDismissExpert?: () => void;
  /** Session id powering the context-usage pill (omit on the home page). */
  usageSessionId?: string;
  /** Triggers pill re-fetch when messages change. */
  usageMsgCount?: number;
  /** Existing session scope for per-task knowledge preferences. Omit on Home. */
  knowledgeSessionId?: string;
  knowledgeDeferred?: boolean;
  /** Opens the personal knowledge management page. */
  onOpenKnowledgeBase?: () => void;
  /** Opens meeting history and the recording/transcription workbench. */
  onOpenMeetingMinutes?: () => void;
  /** Opens organization login/connection management. */
  onOpenOrganization?: () => void;
}) {
  const meetingModel = models?.find((model) => model.id === modelId);
  useMeetingTranscriptionCapability(
    onOpenMeetingMinutes ? meetingModel : undefined,
    models,
  );
  const storageErrors = useStorageHealth((state) => state.errors);
  const draftScopeRef = useRef(draftKey === undefined ? undefined : String(draftKey));
  const persistedRevision = useDraftRevision(draftKey === undefined ? undefined : String(draftKey));
  const localRevisionRef = useRef(0);
  const scopeEpochRef = useRef(0);
  const isCurrentDraftEpoch = (epoch: number) => mountedRef.current && scopeEpochRef.current === epoch;
  const [text, setText] = useState("");
  const textRef = useRef("");
  const [stopping, setStopping] = useState(false);
  const stopPendingRef = useRef<number | null>(null);
  const [attachments, setAttachments] = useState<string[]>([]);
  const [automationSupport, setAutomationSupport] = useState<AutomationCapabilities | null>(null);
  const automationModeAvailable = Boolean(onAutomationModeChange);
  const attachmentsRef = useRef<string[]>([]);
  const attachmentSizesRef = useRef(new Map<string, number>());
  // Only clipboard blobs created by this Composer are disposable. Picker,
  // drop and edit-resend paths can belong to the workspace or chat history.
  const ownedAttachmentPathsRef = useRef(new Set<string>());
  const discardOwnedAttachments = (paths: Iterable<string>) => {
    const disposable: string[] = [];
    for (const path of paths) {
      if (ownedAttachmentPathsRef.current.delete(path)) disposable.push(path);
    }
    discardUnsentAttachments(disposable);
  };
  const beginAttachmentSubmission = (paths: string[]): string[] => {
    const pending: string[] = [];
    for (const path of paths) {
      if (ownedAttachmentPathsRef.current.delete(path)) pending.push(path);
    }
    return pending;
  };
  const rejectAttachmentSubmission = (paths: string[], scope = draftScopeRef.current) => {
    const current = new Set(attachmentsRef.current);
    const persisted = new Set(scope === undefined ? [] : draftAttachments(scope));
    for (const path of paths) {
      if (mountedRef.current && draftScopeRef.current === scope && current.has(path)) {
        ownedAttachmentPathsRef.current.add(path);
      } else if (!persisted.has(path)) {
        discardUnsentAttachments([path]);
      }
    }
  };
  const updateAttachments = (
    next: string[] | ((previous: string[]) => string[]),
  ) => {
    const value = typeof next === "function" ? next(attachmentsRef.current) : next;
    if (value.length !== attachmentsRef.current.length || value.some((path, index) => path !== attachmentsRef.current[index])) {
      localRevisionRef.current += 1;
    }
    attachmentsRef.current = value;
    const retained = new Set(value);
    for (const path of attachmentSizesRef.current.keys()) {
      if (!retained.has(path)) attachmentSizesRef.current.delete(path);
    }
    setAttachments(value);
    if (draftScopeRef.current !== undefined) saveDraftAttachments(draftScopeRef.current, value);
  };

  /** Resolve missing sizes natively and only return a total for a stable list. */
  const currentAttachmentBytes = async (epoch = scopeEpochRef.current): Promise<number> => {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      if (!isCurrentDraftEpoch(epoch)) throw new Error("任务已切换");
      const snapshot = [...attachmentsRef.current];
      const missing = snapshot.filter((path) => !attachmentSizesRef.current.has(path));
      if (missing.length > 0) {
        const inspected = await filesystemAttachmentStats(missing);
        if (!isCurrentDraftEpoch(epoch)) throw new Error("任务已切换");
        if (inspected.rejected.length > 0) {
          throw new Error(inspected.rejected[0].reason);
        }
        for (const file of inspected.files) {
          attachmentSizesRef.current.set(file.inputPath, file.sizeBytes);
          attachmentSizesRef.current.set(file.path, file.sizeBytes);
        }
      }
      if (
        snapshot.length === attachmentsRef.current.length
        && snapshot.every((path, index) => path === attachmentsRef.current[index])
      ) {
        return snapshot.reduce(
          (total, path) => total + (attachmentSizesRef.current.get(path) ?? 0),
          0,
        );
      }
    }
    throw new Error("附件列表变化过快，请稍后重试");
  };

  /**
   * Atomically admit already-inspected files against count, per-file and total
   * limits. The final calculation happens synchronously after the last await,
   * so overlapping paste/drop operations cannot both over-admit.
   */
  const admitAttachmentFiles = async (
    files: AttachmentFileStat[],
    epoch = scopeEpochRef.current,
  ): Promise<AttachmentAdmissionSummary> => {
    await currentAttachmentBytes(epoch);
    if (!isCurrentDraftEpoch(epoch)) throw new Error("任务已切换");
    const next = [...attachmentsRef.current];
    // Another admission can finish while this call is waiting for native size
    // inspection. Recompute from the latest ref after the final await so count
    // and byte limits are both applied to one current, synchronous snapshot.
    let totalBytes = next.reduce(
      (total, path) => total + (attachmentSizesRef.current.get(path) ?? 0),
      0,
    );
    const seen = new Set(next);
    const summary: AttachmentAdmissionSummary = {
      added: 0,
      acceptedPaths: [],
      duplicates: 0,
      oversized: 0,
      countLimited: 0,
      totalLimited: 0,
    };
    for (const file of files) {
      if (seen.has(file.path)) {
        summary.duplicates += 1;
        continue;
      }
      if (file.sizeBytes > MAX_ATTACHMENT_FILE_BYTES) {
        summary.oversized += 1;
        continue;
      }
      if (next.length >= MAX_ATTACHMENT_COUNT) {
        summary.countLimited += 1;
        continue;
      }
      if (totalBytes + file.sizeBytes > MAX_ATTACHMENT_TOTAL_BYTES) {
        summary.totalLimited += 1;
        continue;
      }
      seen.add(file.path);
      next.push(file.path);
      attachmentSizesRef.current.set(file.path, file.sizeBytes);
      totalBytes += file.sizeBytes;
      summary.added += 1;
      summary.acceptedPaths.push(file.path);
    }
    if (summary.added > 0) updateAttachments(next);
    return summary;
  };

  const attachmentAdmissionMessage = (
    summary: AttachmentAdmissionSummary,
    unsupported = 0,
    unreadable = 0,
  ): string => {
    const parts: string[] = [];
    if (summary.added > 0) parts.push(`已添加 ${summary.added} 个`);
    if (summary.duplicates > 0) parts.push(`忽略 ${summary.duplicates} 个重复文件`);
    if (unsupported > 0) parts.push(`跳过 ${unsupported} 个不支持的类型`);
    if (unreadable > 0) parts.push(`跳过 ${unreadable} 个无法读取的文件`);
    if (summary.oversized > 0) parts.push(`${summary.oversized} 个文件超过 20MB`);
    if (summary.countLimited > 0) parts.push(`附件最多 ${MAX_ATTACHMENT_COUNT} 个`);
    if (summary.totalLimited > 0) parts.push(`附件总大小最多 64MB`);
    return parts.join("，") || "没有可添加的附件";
  };

  const addPathAttachments = async (paths: string[]) => {
    const epoch = scopeEpochRef.current;
    const unique = [...new Set(paths)];
    const supported = unique.filter(
      (path) => classifyAttachment(path) !== AttachmentKind.Unsupported,
    );
    const unsupported = unique.length - supported.length;
    const inspectable = supported.slice(0, MAX_ATTACHMENT_INSPECTION_COUNT);
    const uninspected = supported.length - inspectable.length;
    try {
      const inspected = await filesystemAttachmentStats(inspectable);
      const summary = await admitAttachmentFiles(inspected.files, epoch);
      summary.countLimited += uninspected;
      onToast?.(
        attachmentAdmissionMessage(summary, unsupported, inspected.rejected.length),
      );
    } catch (error) {
      if (!isCurrentDraftEpoch(epoch)) return;
      onToast?.(`添加附件失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };
  // 发送前成本预估(对齐 EchoAgent credit-estimate):纯本地 token 估算。
  // ctxUsed/ctxTotal 由 ContextUsagePill 异步获取,这里不耦合;徽章在占比未知时
  // 仍显示 +N(新增 token),有占比信息时叠加(此处保守不取,避免与 pill 抢请求)。
  const cost = useMemo(() => estimateSendCost(text), [text]);
  const boundaryRole: PermissionRole | null = usePermissionModeStore((state) =>
    commandSessionId ? state.statuses[commandSessionId]?.permissionMode ?? null : state.homeMode,
  );
  // 输入历史(arrow-key recall,对齐 EchoAgent use-input-history):内存中按发送追加,
  // ↑/↓ 在输入框回溯。draftRef 暂存「回到输入框」时恢复的草稿。
  const histRef = useRef<InputHistory>(createInputHistory(50));
  const histCursorRef = useRef<number>(0);
  const draftRef = useRef<string>("");
  // 多块提示预览(对齐 EchoAgent content-blocks):仅显示专家/技能
  // 引用。附件在下方有可删除的独立 chip，不再重复渲染一份。
  const blockList = useMemo(() => {
    const list = [];
    if (activeExpertName) list.push(blocks.expert({ name: activeExpertName, path: "", scope: "local", raw: "" }));
    if (sceneTag) list.push(blocks.skill(sceneTag.label));
    return list;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeExpertName, sceneTag]);
  const hasRefs = blockList.length > 0;
  const [listening, setListening] = useState(false);
  const recognitionRef = useRef<VoiceRecognition | null>(null);
  const mountedRef = useRef(true);
  const onDraftChangeRef = useRef(onDraftChange);
  const ref = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (!automationModeAvailable || !("__TAURI_INTERNALS__" in window)) return;
    let disposed = false;
    void automationCapabilities()
      .then((support) => {
        if (!disposed) setAutomationSupport(support);
      })
      .catch(() => {
        // Session creation and mode switching still perform an authoritative
        // backend check. Keep browser previews usable when Tauri IPC is absent.
      });
    return () => { disposed = true; };
  }, [automationModeAvailable]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (draftScopeRef.current === undefined) discardOwnedAttachments([...ownedAttachmentPathsRef.current]);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    onDraftChangeRef.current = onDraftChange;
  }, [onDraftChange]);

  // 统一更新入口:每次写入输入框内容时同步把草稿推给父组件(若启用持久化)。
  // 回填(draftKey 变化)时不走这里,避免把"恢复出来的字"再当成用户输入回写。
  const updateText = (next: string | ((prev: string) => string)) => {
    const value = typeof next === "function" ? next(textRef.current) : next;
    if (value !== textRef.current) localRevisionRef.current += 1;
    textRef.current = value;
    setText(value);
    onDraftChangeRef.current?.(value);
  };

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const resize = () => {
      el.style.height = "0px";
      // CSS owns the viewport-dependent maximum; scrollHeight measures the
      // entire draft, so deleting text also shrinks the field correctly.
      el.style.height = `${el.scrollHeight}px`;
    };
    resize();
    let width = el.getBoundingClientRect().width;
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => {
      const next = el.getBoundingClientRect().width;
      if (next !== width) { width = next; resize(); }
    });
    observer?.observe(el);
    window.addEventListener("resize", resize);
    return () => { observer?.disconnect(); window.removeEventListener("resize", resize); };
  }, [text]);

  // Restore before applying explicit template/edit commands. A same-scope
  // external reset must reach the textarea; ordinary typing already has the
  // same value and must not move its selection or rewrite the persisted draft.
  useEffect(() => {
    const scope = draftKey === undefined ? undefined : String(draftKey);
    const changedScope = draftScopeRef.current !== scope;
    if (changedScope) {
      scopeEpochRef.current += 1;
      const recognition = recognitionRef.current;
      if (recognition?.abort) recognition.abort(); else recognition?.stop();
      recognitionRef.current = null;
      setListening(false);
      ownedAttachmentPathsRef.current.clear();
      attachmentSizesRef.current.clear();
      histRef.current = createInputHistory();
      histCursorRef.current = 0;
      draftRef.current = "";
      localRevisionRef.current += 1;
      setSending(false);
      stopPendingRef.current = null;
      setStopping(false);
    }
    draftScopeRef.current = scope;
    if (scope !== undefined && textRef.current !== (draft ?? "")) {
      textRef.current = draft ?? "";
      setText(textRef.current);
      setCursorPos(textRef.current.length);
      localRevisionRef.current += 1;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draftKey, draft]);

  useEffect(() => {
    if (draftScopeRef.current === undefined) return;
    const restored = draftAttachments(draftScopeRef.current);
    if (restored.length === attachmentsRef.current.length && restored.every((path, index) => path === attachmentsRef.current[index])) return;
    attachmentsRef.current = restored;
    setAttachments(restored);
    localRevisionRef.current += 1;
  }, [draftKey, persistedRevision]);

  // One-shot seed: when the parent supplies initialText, fill the textarea and
  // focus it so the user can immediately edit/send.
  useEffect(() => {
    if (initialText !== undefined && initialText !== null) {
      updateText(initialText);
      setCursorPos(initialText.length);
      onInitialTextConsumed?.();
      requestAnimationFrame(() => ref.current?.focus());
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialText]);

  // 受控填充:点击模板/切换标签时由父组件驱动,把内容写入输入框并聚焦。
  // 用 nonce 而不是 externalText 本身做依赖,这样连续点同一个模板也能重新触发。
  useEffect(() => {
    // Zero is the idle value used by HomePage and ChatView, not a command.
    if (externalTextNonce === undefined || externalTextNonce === 0) return;
    const next = externalText ?? "";
    updateText(next); // 同步草稿:模板写入也算当前草稿内容。
    if (externalAttachments !== undefined) {
      const replacement = [...new Set(externalAttachments)].slice(0, MAX_ATTACHMENT_COUNT);
      discardOwnedAttachments(
        attachmentsRef.current.filter((path) => !replacement.includes(path)),
      );
      // Edit-resend attachments already belong to chat history and must
      // survive removal or navigation from this Composer.
      for (const path of replacement) ownedAttachmentPathsRef.current.delete(path);
      updateAttachments(replacement);
    }
    setCursorPos(next.length);
    requestAnimationFrame(() => {
      const el = ref.current;
      if (!el) return;
      el.focus();
      el.selectionStart = el.selectionEnd = next.length;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [externalTextNonce]);

  // Voice input uses the provider-agnostic ASR registry, with Web Speech as
  // the built-in browser implementation.
  const toggleVoice = async () => {
    if (listening) {
      recognitionRef.current?.stop();
      return;
    }
    const epoch = scopeEpochRef.current;
    // 优先走 provider-agnostic 注册表(对齐 EchoAgent asr:* 契约):外部 provider
    // 注册后优先级更高；非桌面环境回落到内建 Web Speech。
    ensureWebSpeechAsrRegistered();
    const provider = getActiveAsr();
    if (provider) {
      const baseText = text;
      const separator = baseText && !/\s$/.test(baseText) ? " " : "";
      let committedText = "";
      const stop = provider.listen("zh-CN", {
        onInterim: (interim) => {
          if (!isCurrentDraftEpoch(epoch)) return;
          updateText(`${baseText}${separator}${committedText}${interim}`);
        },
        onFinal: (finalDelta) => {
          if (!isCurrentDraftEpoch(epoch)) return;
          committedText += finalDelta;
          updateText(`${baseText}${separator}${committedText}`);
        },
        onError: (reason) => {
          if (!isCurrentDraftEpoch(epoch)) return;
          setListening(false);
          const msg = reason === "not-allowed" || reason === "service-not-allowed"
            ? "未授予麦克风或语音识别权限"
            : reason === "audio-capture"
              ? "未检测到可用麦克风"
              : reason === "no-speech"
                ? "没有检测到语音，请重试"
                : `语音识别错误：${reason}`;
          onToast?.(msg);
        },
        onEnd: () => { if (isCurrentDraftEpoch(epoch)) setListening(false); },
      });
      // 用 recognitionRef 持有 stop 句柄,与既有「再次点击停止」逻辑兼容。
      recognitionRef.current = {
        lang: "zh-CN",
        interimResults: true,
        continuous: false,
        start: () => {},
        stop,
      } as VoiceRecognition;
      setListening(true);
      return;
    }
    const Ctor = getSpeechRecognitionCtor();
    if (!Ctor) {
      onToast?.("当前环境不支持语音输入");
      return;
    }
    const rec = new Ctor();
    rec.lang = "zh-CN";
    rec.interimResults = true;
    rec.continuous = false;
    const baseText = text;
    const separator = baseText && !/\s$/.test(baseText) ? " " : "";
    let committedText = "";
    rec.onresult = (event: SpeechRecognitionEventLike) => {
      if (!isCurrentDraftEpoch(epoch)) return;
      let interim = "";
      for (let i = event.resultIndex; i < event.results.length; i++) {
        const r = event.results[i];
        if (r.isFinal) committedText += r[0].transcript;
        else interim += r[0].transcript;
      }
      // Replace the trailing interim segment on every update; finalized text
      // is accumulated separately and therefore cannot be duplicated.
      updateText(`${baseText}${separator}${committedText}${interim}`);
    };
    rec.onerror = (e: SpeechRecognitionErrorEventLike) => {
      if (!isCurrentDraftEpoch(epoch)) return;
      setListening(false);
      const msg = e.error === "not-allowed"
        ? "未授予麦克风权限"
        : `语音识别错误：${e.error}`;
      onToast?.(msg);
    };
    rec.onend = () => { if (isCurrentDraftEpoch(epoch)) setListening(false); };
    recognitionRef.current = rec;
    try {
      rec.start();
      setListening(true);
    } catch {
      onToast?.("无法启动语音识别");
    }
  };

  useEffect(() => {
    return () => {
      const recognition = recognitionRef.current;
      if (recognition?.abort) recognition.abort();
      else recognition?.stop();
    };
  }, []);

  // Drag-hover state drives the drop overlay. The actual path collection is
  // driven by Tauri's native onDragDropEvent below — the DOM-level
  // onDragEnter/Over/Leave only sees File blobs without local paths, so we
  // never trust them for attachment ingestion.
  const [dragHovering, setDragHovering] = useState(false);

  // Tauri 2 delivers file drop events with absolute local paths through
  // `getCurrentWebview().onDragDropEvent`. This is the only reliable way to
  // obtain real paths in a desktop webview; DOM `onDrop` only sees File
  // blobs. The listener lives for the lifetime of this Composer instance.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      try {
        const webview = getCurrentWebview();
        const dispose = await webview.onDragDropEvent((event) => {
          const payload = (event as { payload?: unknown }).payload as
            | { type: string; paths?: string[] }
            | undefined;
          if (!payload) return;
          switch (payload.type) {
            case "enter":
            case "over":
              setDragHovering(true);
              break;
            case "leave":
              setDragHovering(false);
              break;
            case "drop": {
              const paths = payload.paths ?? [];
              if (paths.length === 0) {
                setDragHovering(false);
                return;
              }
              setDragHovering(false);
              void addPathAttachments(paths);
              break;
            }
            default:
              break;
          }
        });
        if (cancelled) {
          dispose();
        } else {
          unlisten = dispose;
        }
      } catch (error) {
        // 非桌面环境或权限被拒 —— 让粘贴路径继续工作,不影响主流程。
        if (!cancelled) {
          onToast?.(`拖拽监听不可用：${String(error).replace(/^Error:\s*/, "")}`);
        }
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Handle supported clipboard files without disturbing ordinary text/HTML paste. */
  const handlePaste = async (
    event: React.ClipboardEvent<HTMLTextAreaElement>,
  ) => {
    const epoch = scopeEpochRef.current;
    const items = event.clipboardData?.items;
    const extracted = extractFilesFromClipboard(
      items as unknown as ArrayLike<{ kind: string; type: string; getAsFile(): File | Blob | null }> | undefined,
    );
    // 拆分:接受的多类型 + 拒绝的不可执行/不可支持文件
    const supported = extracted.filter((item) => item.kind !== AttachmentKind.Unsupported);
    const unsupported = extracted.filter((item) => item.kind === AttachmentKind.Unsupported);
    if (supported.length === 0 && unsupported.length === 0) return;
    // 全部被拒:给用户明确反馈,告知至少一个不被支持
    if (supported.length === 0) {
      event.preventDefault();
      onToast?.(`剪贴板中的文件类型不支持（${unsupported[0].suggestedName}）`);
      return;
    }
    event.preventDefault();
    let currentBytes: number;
    try {
      currentBytes = await currentAttachmentBytes(epoch);
    } catch (error) {
      if (!isCurrentDraftEpoch(epoch)) return;
      onToast?.(`无法检查现有附件：${String(error).replace(/^Error:\s*/, "")}`);
      return;
    }
    if (!isCurrentDraftEpoch(epoch)) return;
    const preflight: AttachmentAdmissionSummary = {
      added: 0,
      acceptedPaths: [],
      duplicates: 0,
      oversized: 0,
      countLimited: 0,
      totalLimited: 0,
    };
    preflight.oversized = supported.filter(
      (item) => item.blob.size > MAX_ATTACHMENT_FILE_BYTES,
    ).length;
    if (attachmentsRef.current.length + supported.length > MAX_ATTACHMENT_COUNT) {
      preflight.countLimited = supported.length;
    }
    const pastedBytes = supported.reduce((total, item) => total + item.blob.size, 0);
    if (currentBytes + pastedBytes > MAX_ATTACHMENT_TOTAL_BYTES) {
      preflight.totalLimited = supported.length;
    }
    // Clipboard batches are atomic: silently accepting only part of a paste is
    // hard to notice and can make the model answer from incomplete context.
    if (preflight.oversized || preflight.countLimited || preflight.totalLimited) {
      onToast?.(attachmentAdmissionMessage(preflight, unsupported.length));
      return;
    }
    const saved: string[] = [];
    const savedStats: AttachmentFileStat[] = [];
    let failed = 0;
    for (const item of supported) {
      if (!isCurrentDraftEpoch(epoch)) break;
      try {
        const bytes = await blobToBytes(item.blob);
        if (!isCurrentDraftEpoch(epoch)) break;
        const path = await saveAttachmentBlob({
          bytes,
          mime: item.mime,
          suggestedName: item.suggestedName,
        });
        saved.push(path);
        savedStats.push({ inputPath: path, path, sizeBytes: item.blob.size });
      } catch (error) {
        failed += 1;
        // 单张失败不影响其它文件继续落盘。
      }
    }
    if (!isCurrentDraftEpoch(epoch)) {
      discardUnsentAttachments(saved);
      return;
    }
    let admitted: AttachmentAdmissionSummary = {
      added: 0,
      acceptedPaths: [],
      duplicates: 0,
      oversized: 0,
      countLimited: 0,
      totalLimited: 0,
    };
    if (savedStats.length > 0) {
      try {
        admitted = await admitAttachmentFiles(savedStats, epoch);
      } catch {
        failed += savedStats.length;
      }
      const accepted = new Set(admitted.acceptedPaths);
      for (const path of admitted.acceptedPaths) {
        ownedAttachmentPathsRef.current.add(path);
      }
      discardUnsentAttachments(saved.filter((path) => !accepted.has(path)));
    }
    if (!isCurrentDraftEpoch(epoch)) return;
    admitted.oversized += preflight.oversized;
    admitted.countLimited += preflight.countLimited;
    admitted.totalLimited += preflight.totalLimited;
    onToast?.(attachmentAdmissionMessage(admitted, unsupported.length, failed));
  };

  const [sending, setSending] = useState(false);
  const [workspacePicking, setWorkspacePicking] = useState(false);
  const workspacePickingRef = useRef(false);
  const stop = async () => {
    if (stopPendingRef.current !== null || cancelling || disabled || sendNowPending || workspacePickingRef.current || submissionLock.current === scopeEpochRef.current) return;
    const epoch = scopeEpochRef.current;
    stopPendingRef.current = epoch;
    setStopping(true);
    try {
      await onCancel();
    } catch (error) {
      if (isCurrentDraftEpoch(epoch)) onToast?.(`停止失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      if (stopPendingRef.current === epoch) {
        stopPendingRef.current = null;
        if (isCurrentDraftEpoch(epoch)) setStopping(false);
      }
    }
  };
  const submissionLock = useRef<number | null>(null);

  const captureSubmission = (): DraftSubmission => ({
    scope: draftScopeRef.current,
    scopeEpoch: scopeEpochRef.current,
    revision: draftScopeRef.current === undefined ? 0 : draftRevision(draftScopeRef.current),
    localRevision: localRevisionRef.current,
  });

  const finishAcceptedSubmission = (submittedText: string, submission: DraftSubmission) => {
    const { scope } = submission;
    if (scope !== undefined && draftRevision(scope) !== submission.revision) return;
    if (scope !== draftScopeRef.current || !isCurrentDraftEpoch(submission.scopeEpoch)) {
      // Consume only the submitted version. A new draft may reuse this scope
      // after navigation, even when its text happens to be identical.
      onDraftChange?.("");
      if (scope !== undefined) saveDraftAttachments(scope, []);
      return;
    }
    if (localRevisionRef.current !== submission.localRevision) return;
    if (submittedText.trim()) {
      histRef.current = pushHistory(histRef.current, submittedText);
      histCursorRef.current = histRef.current.items.length;
      draftRef.current = "";
    }
    discardOwnedAttachments([...ownedAttachmentPathsRef.current]);
    updateText("");
    updateAttachments([]);
    onClearSceneTag?.();
  };

  const dispatchControlIntent = async (submittedText: string): Promise<boolean> => {
    if (submissionLock.current === scopeEpochRef.current || awaitingQuestion || disabled || sendDisabled || cancelling || stopPendingRef.current !== null || workspacePickingRef.current) return false;
    if (attachmentsRef.current.length > 0 || !onControl) return false;
    const action = parseSessionControlIntent(submittedText);
    if (!action) return false;
    submissionLock.current = scopeEpochRef.current;
    setSending(true);
    const submission = captureSubmission();
    try {
      const result = onControl(action);
      const accepted = result && typeof (result as PromiseLike<boolean | void>).then === "function"
        ? await result
        : result;
      if (accepted !== false) finishAcceptedSubmission(submittedText, submission);
    } catch (error) {
      onToast?.(`${action === "pause" ? "暂停" : "停止"}失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      if (submissionLock.current === submission.scopeEpoch) submissionLock.current = null;
      if (isCurrentDraftEpoch(submission.scopeEpoch)) setSending(false);
    }
    return true;
  };

  const send = async () => {
    const t = textRef.current.trim();
    // A scene tag is contextual metadata, not a user prompt. Require actual
    // text or at least one attachment for every submission path.
    if ((!t && attachmentsRef.current.length === 0) || sending || disabled || sendDisabled || cancelling || stopPendingRef.current !== null || workspacePickingRef.current || awaitingQuestion || submissionLock.current === scopeEpochRef.current) return;
    if (t && attachmentsRef.current.length === 0 && onControl && parseSessionControlIntent(t)) {
      await dispatchControlIntent(t);
      return;
    }
    if (streaming || !apiReady) return;

    // Desktop-owned slash commands never enter the model transcript. Runtime
    // commands and Skills deliberately fall through to onSend/ACP unchanged.
    const invocation = parseSlashInvocation(t);
    if (slashCommandsEnabled && invocation && isClientSlashCommand(invocation.name)) {
      if (!onClientSlashCommand) {
        onToast?.(`当前页面无法执行 /${invocation.name}`);
        return;
      }
      submissionLock.current = scopeEpochRef.current;
      setSending(true);
      const submission = captureSubmission();
      try {
        const result = onClientSlashCommand(invocation);
        const accepted = result && typeof (result as PromiseLike<boolean | void>).then === "function"
          ? await result
          : result;
        if (accepted === false) return;
        finishAcceptedSubmission(t, submission);
      } catch (error) {
        onToast?.(`命令执行失败：${String(error).replace(/^Error:\s*/, "")}`);
      } finally {
        if (submissionLock.current === submission.scopeEpoch) submissionLock.current = null;
        if (isCurrentDraftEpoch(submission.scopeEpoch)) setSending(false);
      }
      return;
    }

    let body = t;
    // 把"操作类型"标签作为上下文前缀一并发出(后端正文仍是可运行的 prompt)。
    if (sceneTag) {
      body = body ? `【${sceneTag.label}】${body}` : `【${sceneTag.label}】`;
    }
    submissionLock.current = scopeEpochRef.current;
    setSending(true);
    const submittedAttachments = [...attachmentsRef.current];
    const submission = captureSubmission();
    const pendingOwnedAttachments = beginAttachmentSubmission(submittedAttachments);
    try {
      const result = onSend(body || "请分析附件。", submittedAttachments);
      const accepted = result && typeof (result as PromiseLike<boolean | void>).then === "function"
        ? await result
        : result;
      if (accepted === false) {
        rejectAttachmentSubmission(pendingOwnedAttachments, submission.scope);
        return;
      }
    } catch (error) {
      rejectAttachmentSubmission(pendingOwnedAttachments, submission.scope);
      onToast?.(`发送失败：${String(error).replace(/^Error:\s*/, "")}`);
      return;
    } finally {
      if (submissionLock.current === submission.scopeEpoch) submissionLock.current = null;
      if (isCurrentDraftEpoch(submission.scopeEpoch)) setSending(false);
    }
    finishAcceptedSubmission(body, submission);
  };

  /** Preserve the draft until durable queue admission succeeds. */
  const enqueue = async () => {
    const t = textRef.current.trim();
    if ((!t && attachmentsRef.current.length === 0) || sending || disabled || sendDisabled || cancelling || stopPendingRef.current !== null || workspacePickingRef.current || awaitingQuestion || submissionLock.current === scopeEpochRef.current) return;
    if (t && attachmentsRef.current.length === 0 && onControl && parseSessionControlIntent(t)) {
      await dispatchControlIntent(t);
      return;
    }
    if (
      !streaming
      || !onEnqueue
      || sendNowPending
      || awaitingQuestion
      || disabled
      || !apiReady
    ) return;

    let body = t;
    if (sceneTag) body = body ? `【${sceneTag.label}】${body}` : `【${sceneTag.label}】`;
    submissionLock.current = scopeEpochRef.current;
    setSending(true);
    const submittedAttachments = [...attachmentsRef.current];
    const submission = captureSubmission();
    const pendingOwnedAttachments = beginAttachmentSubmission(submittedAttachments);
    try {
      const result = onEnqueue(body || "请分析附件。", submittedAttachments);
      const accepted = result && typeof (result as PromiseLike<boolean | void>).then === "function"
        ? await result
        : result;
      if (accepted === false) {
        rejectAttachmentSubmission(pendingOwnedAttachments, submission.scope);
        return;
      }
      finishAcceptedSubmission(body, submission);
    } catch (error) {
      rejectAttachmentSubmission(pendingOwnedAttachments, submission.scope);
      if (isCurrentDraftEpoch(submission.scopeEpoch)) onToast?.(`加入待发送队列失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      if (submissionLock.current === submission.scopeEpoch) submissionLock.current = null;
      if (isCurrentDraftEpoch(submission.scopeEpoch)) setSending(false);
    }
  };

  const pickFiles = async () => {
    const epoch = scopeEpochRef.current;
    try {
      const paths = await filesystemPickFiles({ maxFiles: 20 });
      if (!isCurrentDraftEpoch(epoch) || paths.length === 0) return;
      await addPathAttachments(paths);
    } catch (error) {
      if (!isCurrentDraftEpoch(epoch)) return;
      onToast?.(`选择附件失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };

  // Cursor tracking for slash-command autocomplete.
  const [cursorPos, setCursorPos] = useState(0);
  const mentionCandidates = useWorkspaceMentions(cwd, Boolean(atTokenAtCursor(text, cursorPos)) && filePaths === undefined);
  const slashCommandsRef = useRef<SlashCommandsHandle>(null);
  const atMenuRef = useRef<AtMentionHandle>(null);
  const composingRef = useRef(false);
  // Slash completion is limited to the first token, matching runtime command
  // resolution. The shared parser supports qualified names such as
  // `/plugin-name:skill-name`.
  const slashVisible = slashCommandsEnabled && !!slashTokenAtCursor(text, cursorPos) && apiReady && !streaming;

  const handleSlashPick = (command: string) => {
    const replacement = replaceSlashToken(text, cursorPos, command);
    if (!replacement) return;
    updateText(replacement.text);
    setCursorPos(replacement.cursor);
    // Refocus + put caret at the insertion point.
    requestAnimationFrame(() => {
      const el = ref.current;
      if (!el) return;
      el.focus();
      el.selectionStart = el.selectionEnd = replacement.cursor;
    });
  };

  const showModelPicker = !!onModelChange && !!models;
  const showWorkspacePicker = !!onSelectWorkspace && !!workspaces;

  const composerCls = [
    "echo-composer",
    !apiReady && "echo-composer--disabled",
    !apiReady && !onOpenSettings && "echo-composer--waiting",
    showMeta && "echo-composer--home",
  ].filter(Boolean).join(" ");
  const hasDraft = Boolean(text.trim() || attachments.length);
  const stopAction = streaming && !hasDraft;
  const stopBusy = cancelling || stopping;
  const controlIntent = attachments.length === 0 && onControl && parseSessionControlIntent(text);
  const primaryLabel = stopAction ? stopBusy ? "正在停止生成" : "停止生成" : streaming ? "加入待发送队列" : "发送";
  const primaryDisabled = stopAction
    ? Boolean(disabled || workspacePicking || sendNowPending || sending || stopBusy)
    : Boolean(disabled || sendDisabled || workspacePicking || !apiReady || sending || stopBusy || awaitingQuestion || sendNowPending || !hasDraft || streaming && !onEnqueue && !controlIntent);
  const primaryTitle = stopAction ? "停止当前执行，保留会话记录"
    : !apiReady ? setupHint
    : controlIntent ? `${controlIntent === "pause" ? "暂停" : "停止"}当前执行`
    : streaming ? "加入队列，当前任务完成后发送（Enter）"
    : sending ? "正在发送" : "发送消息";

  return (
    <div
      className={
        "echo-composer-wrap" + (showMeta ? " echo-composer-wrap--home" : "")
      }
    >
      {Object.keys(storageErrors).length > 0 && <div role="alert" className="composer-storage-error">本机保存失败，当前输入仍保留在窗口中。请先复制内容或到「设置 → 数据管理 → 备份与目录」导出备份，再重试保存。{Object.values(storageErrors)[0]}</div>}
      <section
        className={composerCls}
        onClick={(event) => {
          if (!apiReady) { onOpenSettings?.(); return; }
          const target = event.target as Element;
          if (target.closest('button, input, textarea, select, a, [role="button"], [role="menu"], [role="listbox"], [contenteditable], .composer-attachments, .composer-blocks')) return;
          const input = ref.current;
          if (!input || input.disabled || input.selectionStart !== input.selectionEnd) return;
          if (window.getSelection()?.isCollapsed === false) return;
          input.focus({ preventScroll: true });
        }}
        onDragOver={(event) => {
          // SP4: HTML5 fallback for in-webview drag from the file tree.
          // Tauri also fires `onDragDropEvent` with real paths; this branch
          // covers the JS-only event with `application/x-echo-paths` mime.
          if (event.dataTransfer.types.includes("application/x-echo-paths")) {
            event.preventDefault();
            event.dataTransfer.dropEffect = "copy";
          }
        }}
        onDrop={(event) => {
          if (!event.dataTransfer.types.includes("application/x-echo-paths")) return;
          event.preventDefault();
          const raw = event.dataTransfer.getData("application/x-echo-paths");
          if (!raw) return;
          try {
            const parsed = JSON.parse(raw) as string[];
            if (Array.isArray(parsed) && parsed.length > 0) {
              void addPathAttachments(parsed);
            }
          } catch {
            // Tauri drop fires its own event with real paths; ignore duplicate
            // payloads carrying only a text/plain representation here.
          }
        }}
      >
        {!apiReady && (
          <div
            className="echo-composer__setup-hint"
            role={onOpenSettings ? "button" : "status"}
            tabIndex={onOpenSettings ? 0 : undefined}
            onKeyDown={(event) => {
              if (onOpenSettings && (event.key === "Enter" || event.key === " ")) {
                event.preventDefault();
                onOpenSettings();
              }
            }}
          >
            {setupHint}
          </div>
        )}

        {/* 多块提示预览(对齐 EchoAgent content-blocks):引用块 chip 行 */}
        {hasRefs && (
          <div className="composer-blocks" title={assemblePrompt(blockList)}>
            {blockList.map((b) => {
              const isExpert = b.kind === "expert";
              const dismissable = isExpert && onDismissExpert;
              return (
                <span key={b.id} className="composer-blocks__chip">
                  {blockLabel(b)}
                  {dismissable && (
                    <button
                      type="button"
                      className="composer-blocks__chip-remove"
                      onClick={(e) => {
                        e.stopPropagation();
                        onDismissExpert?.();
                      }}
                      aria-label="移除已选专家"
                      title="移除已选专家"
                    >
                      <X size={11} strokeWidth={2} />
                    </button>
                  )}
                </span>
              );
            })}
          </div>
        )}

        {/* Attachment chips */}
        {attachments.length > 0 && (
          <div className="composer-attachments">
            {attachments.map((path) => (
              <span key={path} className="composer-attachments__chip" title={path}>
                <span className="composer-attachments__chip-name">
                  {path.replace(/\\/g, "/").split("/").pop()}
                </span>
                <button
                  type="button"
                  className="composer-attachments__chip-remove"
                  onClick={(e) => {
                    e.stopPropagation();
                    discardOwnedAttachments([path]);
                    updateAttachments((prev) => prev.filter((p) => p !== path));
                  }}
                  aria-label="移除附件"
                >
                  <X size={12} strokeWidth={2} />
                </button>
              </span>
            ))}
          </div>
        )}

        {/* "操作类型"黑色标签(首页选中能力分类后插入,× 可删除) */}
        {sceneTag && (
          <div className="echo-composer__scene-tag" role="group" aria-label={`操作类型 ${sceneTag.label}`}>
            <span className="echo-composer__scene-tag-icon" aria-hidden="true">
              <sceneTag.icon size={14} />
            </span>
            <span className="echo-composer__scene-tag-text">{sceneTag.label}</span>
            <button
              type="button"
              className="echo-composer__scene-tag-remove"
              aria-label={`移除 ${sceneTag.label}`}
              onClick={(e) => {
                e.stopPropagation();
                onClearSceneTag?.();
              }}
            >
              <X size={12} strokeWidth={2} />
            </button>
          </div>
        )}

        {/* 拖拽悬停时显示的提示层。仅在 Tauri 投递的 enter/over 事件期间出现,
            DOM 级别的 dragenter 不会触发它,所以非桌面环境自动降级为「无提示」。

            文案同步支持类型范围:之前只说「松开以添加为附件」暗示仅图片,
            现在明确列出支持的类型(图片 / PDF / 现代 Office / 代码 / 文本 / 数据),
            并在 hover/over 时如能拿到 paths 则即时给出「已添加 N / 跳过 M」的
            预览反馈(只是 hover 提示,真正的反馈在 drop 后的 toast)。 */}
        {dragHovering && (
          <div
            className="echo-composer__dropzone"
            role="status"
            aria-live="polite"
            data-testid="composer-dropzone"
          >
            <div className="echo-composer__dropzone-title">松开以添加为附件</div>
            <div className="echo-composer__dropzone-hint">
              支持图片、PDF、DOCX、XLSX、PPTX、代码、文本与数据文件
            </div>
          </div>
        )}

        <textarea
          ref={ref}
          className="echo-composer__input"
          rows={1}
          value={text}
          disabled={!apiReady || awaitingQuestion || disabled}
          placeholder={
            awaitingQuestion
              ? "请先处理上方需要确认的事项"
              : apiReady
              ? sceneTag
                ? "" // 有操作类型标签时不显示占位文案(匹配 EchoAgent)
                : placeholder ?? "描述想完成的任务，可添加文件"
              : ""
          }
          onChange={(e) => {
            updateText(e.target.value);
            setCursorPos(e.target.selectionStart ?? e.target.value.length);
            // 手动输入时把历史游标重置回末尾(回到「输入框」态)。
            histCursorRef.current = histRef.current.items.length;
          }}
          onSelect={(e) =>
            setCursorPos((e.target as HTMLTextAreaElement).selectionStart ?? cursorPos)
          }
          onPaste={handlePaste}
          onCompositionStart={() => { composingRef.current = true; }}
          onCompositionEnd={() => { composingRef.current = false; }}
          onBlur={() => { composingRef.current = false; }}
          onClick={(e) =>
            setCursorPos((e.target as HTMLTextAreaElement).selectionStart ?? cursorPos)
          }
          onKeyUp={(e) =>
            setCursorPos((e.target as HTMLTextAreaElement).selectionStart ?? cursorPos)
          }
          onKeyDown={(e) => {
            // WebView2 can report the IME's final Enter with isComposing=false
            // and keyCode=229. Do not send, choose a menu row or navigate
            // history until composition has actually finished.
            if (composingRef.current || e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
            if (
              !e.nativeEvent.isComposing
              && atMenuRef.current?.handleKeyDown(e as unknown as KeyboardEvent)
            ) return;
            if (
              !e.nativeEvent.isComposing
              && slashCommandsRef.current?.handleKeyDown(e)
            ) {
              return;
            }
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              if (streaming) void enqueue();
              else void send();
              return;
            }
            // 输入历史 arrow-key recall(对齐 EchoAgent use-input-history)。
            // 仅在未组合输入(中文输入法)且非 slash 菜单可见时响应。
            if (!slashVisible && !e.nativeEvent.isComposing) {
              const el = e.target as HTMLTextAreaElement;
              const atFirstLine = el.selectionStart === 0 || text.length === 0;
              const atLastLine = el.selectionStart === text.length;
              // 已在历史导航中(cursor < items.length)时,↑/↓ 持续翻页,不受光标位置约束。
              const navigating = histCursorRef.current < histRef.current.items.length;
              if (e.key === "ArrowUp" && (atFirstLine || navigating)) {
                if (histCursorRef.current === histRef.current.items.length) {
                  draftRef.current = text; // 进入历史前暂存当前草稿
                }
                const r = navigateHistory(histRef.current, histCursorRef.current, "up", draftRef.current);
                if (r.text !== text) {
                  e.preventDefault();
                  histCursorRef.current = r.cursor;
                  updateText(r.text);
                  requestAnimationFrame(() => {
                    const t = ref.current;
                    if (t) {
                      t.selectionStart = t.selectionEnd = r.text.length;
                    }
                  });
                }
              } else if (e.key === "ArrowDown" && (atLastLine || navigating)) {
                const r = navigateHistory(histRef.current, histCursorRef.current, "down", draftRef.current);
                if (r.cursor !== histCursorRef.current) {
                  e.preventDefault();
                  histCursorRef.current = r.cursor;
                  updateText(r.text);
                  requestAnimationFrame(() => {
                    const t = ref.current;
                    if (t) {
                      t.selectionStart = t.selectionEnd = r.text.length;
                    }
                  });
                }
              }
            }
          }}
        />
        {/* Slash-command autocomplete */}
        {slashCommandsEnabled && apiReady && !streaming && (
          <SlashCommands
            ref={slashCommandsRef}
            text={text}
            cursor={cursorPos}
            sessionId={commandSessionId}
            cwd={cwd}
            refreshKey={commandRefreshKey}
            onPick={handleSlashPick}
          />
        )}
        {/* SP4: @-mention autocomplete */}
        <AtMentionMenu
          ref={atMenuRef}
          text={text}
          cursor={cursorPos}
          filePaths={filePaths ?? mentionCandidates.filePaths}
          workspaceSymbols={workspaceSymbols ?? mentionCandidates.workspaceSymbols}
          members={mentionMembers}
          onPick={(mention, _replacement, memberId) => {
            if (memberId) onMentionMember?.(memberId);
            const result = replaceAtToken(text, cursorPos, mention);
            if (result) {
              updateText(result.text);
              setCursorPos(result.cursor);
            }
          }}
        />
        {permissionInline && (
          <AutomationBoundaryNotice role={boundaryRole} automationMode={automationMode} />
        )}
        <div className="echo-composer__footer">
          <InputAddMenu
            disabled={!apiReady || awaitingQuestion || disabled}
            allowMeetingHistoryWhileDisabled={!apiReady && !awaitingQuestion && !disabled}
            onPickFiles={pickFiles}
            onSelectExpert={onSelectExpert}
            onSelectSkill={(name) => {
              onSelectSkill?.(name);
              if (!onSelectSkill) {
                updateText((prev) => {
                  const prefix = prev.endsWith(" ") || prev === "" ? "" : " ";
                  return prev + prefix + `/${name} `;
                });
                requestAnimationFrame(() => ref.current?.focus());
              }
            }}
            onNavigateConnectors={onNavigateConnectors}
            meetingMinutesAvailable={Boolean(onOpenMeetingMinutes)}
            onOpenMeetingMinutes={onOpenMeetingMinutes}
            automationMode={automationMode}
            automationCapabilities={automationSupport}
            automationModeDisabled={automationModeDisabled}
            onAutomationModeChange={onAutomationModeChange}
          />
          {showAutomationModeBadge && automationMode !== "default" && (
            <span
              className={`echo-composer__automation-badge echo-composer__automation-badge--${automationMode}`}
              title={automationMode === "browser_use"
                ? "本任务可以在独立浏览器中操作网页"
                : "本任务可以查看屏幕并操作本机应用"}
            >
              {automationMode === "browser_use"
                ? <Globe2 size={14} strokeWidth={1.8} aria-hidden="true" />
                : <Monitor size={14} strokeWidth={1.8} aria-hidden="true" />}
              <span>{automationMode === "browser_use" ? "操作网页" : "操作电脑"}</span>
              <button
                type="button"
                className="echo-composer__automation-badge-remove"
                disabled={automationModeDisabled}
                onClick={(event) => {
                  event.stopPropagation();
                  void onAutomationModeChange?.("default");
                }}
                aria-label={`关闭${automationMode === "browser_use" ? "操作网页" : "操作电脑"}`}
              >
                <X size={11} strokeWidth={2} />
              </button>
            </span>
          )}
          {activeExpertName && (
            <span className="echo-composer__expert-badge" title={`当前专家：${activeExpertName}`}>
              <ThumbImg name={activeExpertName} local={activeExpertAvatar} size={18} shape="circle" />
              {activeExpertName}
              {onDismissExpert && (
                <button
                  type="button"
                  className="echo-composer__expert-badge__remove"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDismissExpert?.();
                  }}
                  aria-label="移除已选专家"
                  title="移除已选专家"
                >
                  <X size={11} strokeWidth={2} />
                </button>
              )}
            </span>
          )}
          {permissionInline && (
            <PermissionPicker onToast={onToast} sessionId={commandSessionId} />
          )}
          <KnowledgePicker
            sessionId={knowledgeSessionId}
            deferRuntimeSync={knowledgeDeferred}
            disabled={!apiReady || awaitingQuestion || disabled || sendDisabled || workspacePickingRef.current || streaming}
            onManage={onOpenKnowledgeBase}
            onOpenOrganization={onOpenOrganization}
            onToast={onToast}
          />
          <div className="echo-composer__spacer" />
          <div className="composer-primary-tools">
          <ContextUsagePill key={usageSessionId ?? "new-task"} sessionId={usageSessionId} onRefreshSignal={usageMsgCount}
            estimate={text.trim() ? cost : undefined} />
          {showModelPicker ? (
            <ModelSelector
              modelId={modelId}
              modelLoading={modelLoading}
              models={models!}
              onModelChange={onModelChange!}
            />
          ) : (
            <button
              className="echo-composer__model"
              onClick={(e) => {
                e.stopPropagation();
                onOpenSettings?.();
              }}
            >
              {modelSummary ?? "Auto"} <ChevronDownIcon size="sm" />
            </button>
          )}
          <button
            className={
              "echo-composer__tool" + (listening ? " echo-composer__tool--active" : "")
            }
            onClick={(e) => {
              e.stopPropagation();
              void toggleVoice();
            }}
            disabled={!apiReady || awaitingQuestion || disabled}
            aria-label="语音输入"
            title={listening ? "正在聆听…点击停止" : "语音输入"}
          >
            <Mic size={16} />
          </button>
          <button type="button"
            className={"echo-composer__send" + (stopAction ? " echo-composer__send--stop" : streaming ? " echo-composer__send--queue" : !hasDraft ? " echo-composer__send--empty" : "")}
            onClick={event => {
              event.stopPropagation();
              // A double-click must not enqueue and then stop the active turn
              // when this same button switches back to its empty-draft action.
              if (event.detail > 1) return;
              if (stopAction) void stop();
              else if (streaming) void enqueue();
              else void send();
            }}
            disabled={primaryDisabled} aria-label={primaryLabel} aria-busy={sending || stopBusy || sendNowPending}
            title={primaryTitle}>
            {stopAction ? <Square size={14} fill="currentColor" /> : <ArrowUp size={18} />}
          </button>
          </div>
        </div>
      </section>
      {/* EchoAgent: meta 行在白卡外下方,透明背景,与卡片间距4px(仅首页) */}
      {showMeta && !permissionInline && (
        <div className="echo-composer-meta">
          {showWorkspacePicker ? (
            <WorkspacePicker
              cwd={cwd}
              workspaces={workspaces!}
              onSelectWorkspace={onSelectWorkspace!}
              automaticWorkspace={automaticWorkspace}
              defaultWorkspaceCwd={defaultWorkspaceCwd}
              disabled={disabled || sending}
              onBusyChange={busy => { workspacePickingRef.current = busy; setWorkspacePicking(busy); }}
            />
          ) : null}
          <AutomationBoundaryNotice
            role={boundaryRole}
            automationMode={automationMode}
          />
          <PermissionPicker onToast={onToast} sessionId={commandSessionId} />
        </div>
      )}
      {showDisclaimer && (
        <div className="echo-composer__disclaimer">
          AI 可能出错，请核实重要信息
        </div>
      )}
    </div>
  );
}

// ---------- SpeechRecognition minimal typing ----------
// The browser SpeechRecognition API isn't in the TS DOM lib by default, and
// vendor prefixes vary. We type only the surface we use and resolve the ctor
// defensively at runtime.
interface SpeechRecognitionResultLike {
  isFinal: boolean;
  0: { transcript: string };
}
interface SpeechRecognitionEventLike {
  resultIndex: number;
  results: ArrayLike<SpeechRecognitionResultLike>;
}
interface SpeechRecognitionErrorEventLike {
  error: string;
}
interface VoiceRecognition {
  lang: string;
  interimResults: boolean;
  continuous: boolean;
  start: () => void;
  stop: () => void;
  abort?: () => void;
  onresult: (e: SpeechRecognitionEventLike) => void;
  onerror: (e: SpeechRecognitionErrorEventLike) => void;
  onend: () => void;
}
type VoiceRecognitionCtor = new () => VoiceRecognition;

function getSpeechRecognitionCtor(): VoiceRecognitionCtor | null {
  if (typeof window === "undefined") return null;
  const w = window as unknown as {
    SpeechRecognition?: VoiceRecognitionCtor;
    webkitSpeechRecognition?: VoiceRecognitionCtor;
  };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

/**
 * 注册内建 Web Speech ASR provider 到 voice-contract 注册表(provider-agnostic,
 * 对齐 EchoAgent `asr:*` 契约)。外部 provider(如云端 STT)注册后会因其更高
 * 优先级而被优先使用。仅在首次调用时注册一次。
 */
let webSpeechAsrRegistered = false;
function ensureWebSpeechAsrRegistered(): void {
  if (webSpeechAsrRegistered) return;
  webSpeechAsrRegistered = true;
  const Ctor = getSpeechRecognitionCtor();
  if (!Ctor) return;
  registerAsrProvider(
    createWebSpeechAsrProvider({
      isAvailable: () => getSpeechRecognitionCtor() !== null,
      createRecognition: (lang) => {
        const rec = new Ctor();
        rec.lang = lang;
        rec.interimResults = true;
        rec.continuous = false;
        return rec as never;
      },
    }),
  );
}
