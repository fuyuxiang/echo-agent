import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { workspaceEditor } from "./workspace-editor";
import { idePath } from "./windows-path";
import { listenDocumentTools } from "./coding-document-tools";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { create } from "zustand";

import { beginAgentTurn } from "@/lib/agent-turn";
import { agentLoadSession } from "@/lib/agent-client";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import type { SessionTranscript } from "@/stores/session-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { useCodingFollowupQueue } from "../hooks/useCodingFollowupQueue";
import { codingQueueOwner, codingSessionReady, sendCodingQueuedFollowup } from "./coding-followup";

import { useTaskStore } from "../store/task-store";
import { prepareWorkspaceEditor } from "./workspace-editor";
import { isBusyPhase } from "./phase";
import { useTaskLifecycle } from "./task-lifecycle";
import { codingApi, onPhaseChanged } from "./tauri-api";
import type { CodingTask, Problem } from "./types";
import {
  buildNodeContinuationInstruction,
  buildPlanRevisionInstruction,
  mergeTaskVerificationCommands,
} from "./workflow";

interface TrackedTask { root: string; taskId: string }
export interface DocumentCancellationState { workspace: string; sessionId: string; runId: string; pending: boolean; cancellationId?: string }
export const DOCUMENT_CANCELLATION_PENDING_MESSAGE = "Runtime 已停，编辑器或外部工具尚未确认结束，仍在停止或状态未知；已发生修改保留。";
export const DOCUMENT_CANCELLATION_CHECKING_MESSAGE = "正在核对编辑器或外部工具的停止状态，暂缓新操作。";
interface RuntimeState {
  tasks: Record<string, TrackedTask>;
  settling: Record<string, boolean>;
  activeRunIds: Record<string, string>;
  verificationBatches: Record<string, string>;
  editorBlocks: Record<string, string>;
  stopping: Record<string, boolean>;
  actionEpochs: Record<string, number>;
  documentCancellations: Record<string, DocumentCancellationState>;
  workspaceAliases: Record<string, string>;
  documentCancellationReady: boolean;
  setDocumentCancellation: (event: DocumentCancellationState) => void;
  replaceDocumentCancellations: (events: DocumentCancellationState[]) => void;
  setDocumentCancellationReady: (ready: boolean) => void;
  setStopping: (root: string, taskId: string, value: boolean) => void;
  setEditorBlock: (root: string, taskId: string, message: string | null) => void;
  claimVerificationBatch: (root: string, taskId: string) => string | null;
  releaseVerificationBatch: (root: string, taskId: string, owner: string) => void;
  track: (root: string, taskId: string) => void;
  untrack: (root: string, taskId: string) => void;
  setSettling: (root: string, taskId: string, value: boolean) => void;
  setActiveRunId: (root: string, taskId: string, runId: string | null) => void;
}

export function codingRuntimeKey(root: string, taskId: string): string {
  return JSON.stringify([root, taskId]);
}

function workspacePathKey(root: string): string {
  const path = idePath(root).replace(/\/+$/, "") || "/";
  return /^[A-Za-z]:\//.test(path) || path.startsWith("//") ? path.toLowerCase() : path;
}
function cancellationWorkspace(root: string, aliases?: Record<string, string>): string {
  const normalized = workspacePathKey(workspaceEditor(root)?.workspace ?? root);
  return aliases?.[normalized] ?? normalized;
}
export function codingWorkspaceCancellationPending(root: string, state = useCodingRuntimeStore.getState()): boolean {
  return Object.values(state.documentCancellations).some(event => cancellationWorkspace(event.workspace, state.workspaceAliases) === cancellationWorkspace(root, state.workspaceAliases));
}
export function codingWorkspaceCancellationBlocked(root: string): boolean {
  const state = useCodingRuntimeStore.getState();
  return !state.documentCancellationReady || codingWorkspaceCancellationPending(root, state);
}
export function assertCodingWorkspaceCancellationClear(root: string): void {
  const state = useCodingRuntimeStore.getState();
  if (!state.documentCancellationReady) throw new Error(DOCUMENT_CANCELLATION_CHECKING_MESSAGE);
  if (codingWorkspaceCancellationPending(root, state)) throw new Error(DOCUMENT_CANCELLATION_PENDING_MESSAGE);
}
function cancellationIdentity(event: DocumentCancellationState): string {
  return JSON.stringify([cancellationWorkspace(event.workspace), event.sessionId, event.runId]);
}
function validCancellation(event: unknown): event is DocumentCancellationState {
  if (!event || typeof event !== "object") return false;
  const value = event as DocumentCancellationState;
  return typeof value.workspace === "string" && !!value.workspace && typeof value.sessionId === "string"
    && !!value.sessionId && typeof value.runId === "string" && !!value.runId && typeof value.pending === "boolean"
    && (value.cancellationId === undefined || typeof value.cancellationId === "string" && !!value.cancellationId);
}

export const useCodingRuntimeStore = create<RuntimeState>((set, get) => ({
  tasks: {},
  settling: {},
  activeRunIds: {},
  verificationBatches: {},
  editorBlocks: {},
  stopping: {},
  actionEpochs: {},
  documentCancellations: {},
  workspaceAliases: {},
  documentCancellationReady: false,
  setDocumentCancellationReady: documentCancellationReady => set({ documentCancellationReady }),
  replaceDocumentCancellations: events => {
    set({ documentCancellations: {} });
    for (const event of events) get().setDocumentCancellation(event);
  },
  setDocumentCancellation: event => set(state => {
    const key = cancellationIdentity(event);
    if (!event.pending && !state.documentCancellations[key] || event.pending && state.documentCancellations[key]) return state;
    const documentCancellations = { ...state.documentCancellations };
    if (event.pending) documentCancellations[key] = event; else delete documentCancellations[key];
    const actionEpochs = { ...state.actionEpochs };
    const workspaceAliases = { ...state.workspaceAliases };
    if (event.pending) {
      const selected = useTaskStore.getState();
      const tasks = [...Object.values(state.tasks), ...(selected.task ? [{ root: selected.root, taskId: selected.task.id }] : [])];
      for (const task of tasks) {
        const canonical = workspaceEditor(task.root)?.workspace;
        if (canonical) workspaceAliases[workspacePathKey(task.root)] = cancellationWorkspace(canonical);
        if (cancellationWorkspace(task.root, workspaceAliases) !== cancellationWorkspace(event.workspace, workspaceAliases)) continue;
        const taskKey = codingRuntimeKey(task.root, task.taskId);
        actionEpochs[taskKey] = (state.actionEpochs[taskKey] ?? 0) + 1;
      }
    }
    return { documentCancellations, actionEpochs, workspaceAliases };
  }),
  setStopping: (root, taskId, value) => set(state => ({
    stopping: { ...state.stopping, [codingRuntimeKey(root, taskId)]: value },
    actionEpochs: value && !state.stopping[codingRuntimeKey(root, taskId)]
      ? { ...state.actionEpochs, [codingRuntimeKey(root, taskId)]: (state.actionEpochs[codingRuntimeKey(root, taskId)] ?? 0) + 1 }
      : state.actionEpochs,
  })),
  setEditorBlock: (root, taskId, message) => set((state) => {
    const editorBlocks = { ...state.editorBlocks };
    const key = codingRuntimeKey(root, taskId);
    if (message) editorBlocks[key] = message; else delete editorBlocks[key];
    return { editorBlocks };
  }),
  claimVerificationBatch: (root, taskId) => {
    const key = codingRuntimeKey(root, taskId);
    if (get().verificationBatches[key] || codingWorkspaceCancellationBlocked(root)) return null;
    const owner = crypto.randomUUID();
    set((state) => ({ verificationBatches: { ...state.verificationBatches, [key]: owner } }));
    return owner;
  },
  releaseVerificationBatch: (root, taskId, owner) => set((state) => {
    const key = codingRuntimeKey(root, taskId);
    if (state.verificationBatches[key] !== owner) return state;
    const verificationBatches = { ...state.verificationBatches };
    delete verificationBatches[key];
    return { verificationBatches };
  }),
  track: (root, taskId) => set((state) => {
    const key = codingRuntimeKey(root, taskId);
    const canonical = workspaceEditor(root)?.workspace;
    return { tasks: state.tasks[key] ? state.tasks : { ...state.tasks, [key]: { root, taskId } },
      workspaceAliases: canonical ? {...state.workspaceAliases,[workspacePathKey(root)]:cancellationWorkspace(canonical)} : state.workspaceAliases };
  }),
  untrack: (root, taskId) => set((state) => {
    const key = codingRuntimeKey(root, taskId);
    if (!state.tasks[key]) return state;
    const tasks = { ...state.tasks };
    delete tasks[key];
    return { tasks };
  }),
  setSettling: (root, taskId, value) => set((state) => ({
    settling: { ...state.settling, [codingRuntimeKey(root, taskId)]: value },
  })),
  setActiveRunId: (root, taskId, runId) => set((state) => {
    const activeRunIds = { ...state.activeRunIds };
    const key = codingRuntimeKey(root, taskId);
    if (runId) activeRunIds[key] = runId;
    else delete activeRunIds[key];
    return { activeRunIds };
  }),
}));

function repairInstruction(problems: Problem[]): string {
  const documentation = problems.some((problem) => problem.kind === "documentation");
  const details = problems.length > 0
    ? problems.map((problem, index) =>
      `${index + 1}. [${problem.kind}] ${problem.file ?? "未知文件"}${problem.line ? `:${problem.line}` : ""} — ${problem.message}`,
    ).join("\n")
    : "验证未通过，请检查验证命令的真实输出。";
  let instruction = "上一轮验证未通过。检查真实命令输出，修复问题并重新验证；保留现有正确功能。";
  if (documentation) {
    const messages = problems.map((problem) => problem.message).join("\n");
    if (messages.includes("只读代码解释任务")) {
      instruction = "这是只读代码解释任务。撤销本轮产生的文件变更，保留对话中的证据化分析。";
    } else if (messages.includes("未产生任何文件变更")) {
      instruction = "把用户要求的注释或文档写入工程，不要只在对话里给出示例。";
    } else if (messages.includes("变更集中没有")) {
      instruction = "核对原始需求，补齐缺失的注释或文档产物，保持可执行逻辑不变。";
    } else {
      instruction = "修复注释安全检查发现的越界修改，仅保留要求的注释和文档变更。";
    }
  }
  return `${instruction}\n\n${details}`;
}

async function recoverInterruptedRound(root: string, task: CodingTask, transcript: SessionTranscript): Promise<void> {
  if (task.nextAction || !["discovering", "implementing"].includes(task.phase) || transcript.streamingMessageId) return;
  // A replayed completion may belong to an earlier node, not this unfinished round.
  await codingApi.syncChanges(root, task.id);
  await codingApi.reportInterrupted(root, task.id, "paused");
}

function CodingTaskRuntime({ root, taskId, apiReady, availableModelIds }: TrackedTask & { apiReady: boolean; availableModelIds?: string[] }) {
  const [task, setTask] = useState<CodingTask | null>(null);
  const [problems, setProblems] = useState<Problem[]>([]);
  const [restoring, setRestoring] = useState(false);
  const refreshGeneration = useRef(0);
  const verificationKey = useRef<string | null>(null);
  const agentActionKey = useRef<string | null>(null);
  const mounted = useRef(true);
  const latestTask = useRef(task);
  latestTask.current = task;
  const stopping = useCodingRuntimeStore(state => Boolean(state.stopping[codingRuntimeKey(root, taskId)]));
  const cancellationBlocked = useCodingRuntimeStore(state => !state.documentCancellationReady || codingWorkspaceCancellationPending(root, state));
  const canAct = () => mounted.current && !codingWorkspaceCancellationBlocked(root) && !useCodingRuntimeStore.getState().stopping[codingRuntimeKey(root, taskId)];
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const verificationClaimed = useCodingRuntimeStore((state) => Boolean(state.verificationBatches[codingRuntimeKey(root, taskId)]));
  const refresh = useCallback(async () => {
    const generation = ++refreshGeneration.current;
    const result = await codingApi.orchestratorState(root, taskId);
    if (generation !== refreshGeneration.current) return;
    latestTask.current = result.task;
    setTask(result.task);
    if (["paused", "stopped", "blocked"].includes(result.task.phase)) useCodingRuntimeStore.getState().setStopping(root, taskId, false);
    setProblems(result.problems);
    const selected = useTaskStore.getState();
    if (selected.root === root && selected.task?.id === taskId) {
      await selected.refreshTaskState();
      await selected.refreshSummaries();
    }
  }, [root, taskId]);

  useEffect(() => {
    void refresh().catch(() => undefined);
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void onPhaseChanged((event) => {
      if (!disposed && event.root === root && event.taskId === taskId) {
        latestTask.current = null; // Invalidate pending admissions before asynchronous refresh.
        void refresh().catch(() => undefined);
      }
    }).then((fn) => { if (disposed) fn(); else unlisten = fn; });
    return () => { disposed = true; unlisten?.(); refreshGeneration.current += 1; };
  }, [refresh, root, taskId]);

  const { settling } = useTaskLifecycle(root, restoring ? null : task, refresh);
  useEffect(() => {
    useCodingRuntimeStore.getState().setSettling(root, taskId, settling);
    return () => useCodingRuntimeStore.getState().setSettling(root, taskId, false);
  }, [root, settling, taskId]);

  const sessionId = task?.sessionId;
  const transcript = useSessionStore((state) => sessionId ? state.transcripts[sessionId] : undefined);
  const streaming = Boolean(transcript?.streamingMessageId);
  const awaitingPermission = usePermissionStore((state) => sessionId ? (state.queues[sessionId]?.length ?? 0) > 0 : false);
  const awaitingQuestion = useQuestionStore((state) => sessionId ? (state.queues[sessionId]?.length ?? 0) > 0 : false);
  const sessionModelId = useSessionsStore(state => state.independent.find(entry => entry.sessionId === sessionId)?.currentModelId);
  const queueModelId = sessionModelId ?? task?.modelId;
  const queueEnabled = apiReady && (!availableModelIds || !!queueModelId && availableModelIds.includes(queueModelId));
  const queueEnabledRef = useRef(queueEnabled);
  queueEnabledRef.current = queueEnabled;
  useCodingFollowupQueue({
    workspace: root, taskId, sessionId,
    autoReady: queueEnabled && task?.phase === "delivered" && !!transcript && !transcript.control
      && !restoring && !streaming && !transcript.pendingSendNowPromptId && !transcript.planApprovals?.length
      && !settling && !awaitingPermission && !awaitingQuestion && !stopping && !cancellationBlocked,
    manualReady: false,
    send: (text, contextPaths, promptId, manual, coding) => {
      const epoch = useCodingRuntimeStore.getState().actionEpochs[codingRuntimeKey(root, taskId)] ?? 0;
      return sendCodingQueuedFollowup({ root, taskId, sessionId: sessionId!, text, contextPaths, promptId, manual, coding,
        canDispatch: () => canAct() && queueEnabledRef.current
          && epoch === (useCodingRuntimeStore.getState().actionEpochs[codingRuntimeKey(root, taskId)] ?? 0)
          && !useCodingRuntimeStore.getState().settling[codingRuntimeKey(root, taskId)]
          && codingSessionReady(sessionId!),
        onChanged: refresh,
      });
    },
  });
  const restoredSession = useRef<string | null>(null);
  useEffect(() => {
    if (!sessionId || useSessionStore.getState().transcripts[sessionId] || restoredSession.current === sessionId) return;
    restoredSession.current = sessionId;
    let disposed = false;
    setRestoring(true);
    void agentLoadSession(sessionId, root)
      .then(async () => {
        const store = useSessionStore.getState();
        if (!store.transcripts[sessionId]) {
          throw new Error("会话历史未能载入");
        }
        store.finalizeIncompleteReplay(sessionId);
        const latest = await codingApi.orchestratorState(root, taskId);
        const loadedTranscript = useSessionStore.getState().transcripts[sessionId];
        if (loadedTranscript) await recoverInterruptedRound(root, latest.task, loadedTranscript);
        await refresh();
      })
      .catch(async (error) => {
        await codingApi.reportStartFailed(root, taskId, `无法恢复任务会话：${String(error).replace(/^Error:\s*/, "")}`).catch(() => undefined);
        await refresh().catch(() => undefined);
      })
      .finally(() => { if (!disposed) setRestoring(false); });
    return () => { disposed = true; };
  }, [refresh, root, sessionId, taskId]);

  useEffect(() => {
    if (!task || task.phase !== "verifying" || settling || verificationClaimed || stopping || cancellationBlocked || streaming || transcript?.pendingSendNowPromptId) return;
    const key = `${root}:${taskId}:${task.updatedAt}`;
    if (verificationKey.current === key) return;
    const owner = useCodingRuntimeStore.getState().claimVerificationBatch(root, taskId);
    if (!owner) return;
    verificationKey.current = key;
    let batchKey = key;
    const epoch = useCodingRuntimeStore.getState().actionEpochs[codingRuntimeKey(root, taskId)] ?? 0;
    const allowed = () => {
      const live = task.sessionId ? useSessionStore.getState().transcripts[task.sessionId] : undefined;
      return canAct() && !live?.streamingMessageId && !live?.pendingSendNowPromptId
        && epoch === (useCodingRuntimeStore.getState().actionEpochs[codingRuntimeKey(root, taskId)] ?? 0);
    };
    void (async () => {
      try {
        try {
          if (transcript?.agentMode === "ask" || transcript?.agentMode === "plan") {
            await codingApi.reportInterrupted(root, taskId, "paused");
            useCodingRuntimeStore.getState().setEditorBlock(root, taskId, "只读轮次不会自动执行检查；需要时请手动运行项目检查。");
            await refresh();
            return;
          }
          await prepareWorkspaceEditor(root);
          if (!allowed() || latestTask.current?.phase !== "verifying") return;
          useCodingRuntimeStore.getState().setEditorBlock(root, taskId, null);
        } catch (error) {
          if (!allowed() || latestTask.current?.phase !== "verifying") return;
          useCodingRuntimeStore.getState().setEditorBlock(root, taskId, `等待保存后验证：${String(error).replace(/^Error:\s*/, "")}`);
          // Let the editor's normal phase guard accept the user's save. No
          // command or verification evidence is created by this interruption.
          await codingApi.reportInterrupted(root, taskId, "paused");
          await refresh();
          return;
        }
        const detected = await codingApi.detectCommands(root);
        const config = await codingApi.verificationConfig(root);
        const automatic = new Set(config.checks.filter(check => check.autoRun).map(check => check.command));
        const commands = task.strictWorkflow ? mergeTaskVerificationCommands(detected, task) : detected.filter(command => automatic.has(command.command));
        if (!allowed() || latestTask.current?.phase !== "verifying") return;
        const current = await codingApi.orchestratorState(root, taskId);
        if (!allowed()) return;
        if (current.task.phase !== "verifying" || current.task.updatedAt !== task.updatedAt) { await refresh(); return; }
        // Plan-declared commands require the foreground confirmation dialog.
        if (commands.some((command) => command.requiresApproval)) return;
        const started = await codingApi.beginVerification(root, taskId, task.updatedAt);
        batchKey = `${root}:${taskId}:${started.updatedAt}`;
        verificationKey.current = batchKey;
        await refresh();
        for (const command of commands) {
          if (!allowed() || latestTask.current?.phase !== "verifying") return;
          const runId = crypto.randomUUID();
          useCodingRuntimeStore.getState().setActiveRunId(root, taskId, runId);
          const record = await codingApi.runVerification(root, taskId, command.kind, command.command, undefined, runId);
          useCodingRuntimeStore.getState().setActiveRunId(root, taskId, null);
          if (record.status === "cancelled") return;
          if (record.status !== "passed") break;
        }
        if (!allowed() || latestTask.current?.phase !== "verifying") return;
        await codingApi.reportVerification(root, taskId);
        await refresh();
      } catch (error) {
        if (!allowed() || latestTask.current?.phase !== "verifying") return;
        await codingApi.reportStartFailed(root, taskId, `自动验证未能完成：${String(error).replace(/^Error:\s*/, "")}`).catch(() => undefined);
        await refresh().catch(() => undefined);
      } finally {
        if (!allowed() && verificationKey.current === batchKey) verificationKey.current = null;
        useCodingRuntimeStore.getState().setActiveRunId(root, taskId, null);
        useCodingRuntimeStore.getState().releaseVerificationBatch(root, taskId, owner);
      }
    })();
  }, [refresh, root, settling, task, taskId, verificationClaimed, stopping, cancellationBlocked, streaming, transcript?.pendingSendNowPromptId]);

  useEffect(() => {
    if (!task || !task.strictWorkflow || !["discovering", "implementing", "repairing"].includes(task.phase) || (!task.nextAction && task.phase !== "repairing") || !sessionId || !transcript
      || restoring || streaming || transcript.pendingSendNowPromptId || settling || awaitingPermission || awaitingQuestion || stopping || cancellationBlocked) return;
    const key = JSON.stringify([task.id, task.updatedAt, task.phase, task.nextAction]);
    if (agentActionKey.current === key) return;
    let cancelled = false;
    let dispatched = false;
    const epoch = useCodingRuntimeStore.getState().actionEpochs[codingRuntimeKey(root, taskId)] ?? 0;
    const current = () => {
      const live = latestTask.current;
      const state = useSessionStore.getState().transcripts[sessionId];
      return !cancelled && canAct() && epoch === (useCodingRuntimeStore.getState().actionEpochs[codingRuntimeKey(root, taskId)] ?? 0) && live?.updatedAt === task.updatedAt && live.phase === task.phase
        && live.nextAction === task.nextAction && live.sessionId === sessionId
        && Boolean(state) && !state.streamingMessageId && !state.pendingSendNowPromptId && !(usePermissionStore.getState().queues[sessionId]?.length)
        && !(useQuestionStore.getState().queues[sessionId]?.length);
    };
    const confirmCurrent = async () => {
      if (!current()) return false;
      const native = await codingApi.orchestratorState(root, taskId);
      if (!current()) return false;
      if (native.task.updatedAt !== task.updatedAt || native.task.phase !== task.phase
        || native.task.nextAction !== task.nextAction || native.task.sessionId !== sessionId) {
        await refresh();
        return false;
      }
      return true;
    };
    agentActionKey.current = key;
    void (async () => {
      try {
        const activeNode = task.taskNodes.find(node => node.status === "running");
        if (task.nextAction === "continue_node" && !activeNode) {
          if (await confirmCurrent()) await codingApi.reportStartFailed(root, taskId, "调度器要求继续执行，但没有运行中的节点");
          await refresh();
          return;
        }
        try { await prepareWorkspaceEditor(root); }
        catch (error) {
          if (!(await confirmCurrent())) return;
          useCodingRuntimeStore.getState().setEditorBlock(root, taskId, `等待保存后继续：${String(error).replace(/^Error:\s*/, "")}`);
          await codingApi.reportInterrupted(root, taskId, "paused");
          await refresh();
          return;
        }
        if (!(await confirmCurrent())) return;
        const text = task.nextAction === "revise_plan" ? "正在修订执行计划"
          : task.nextAction === "continue_node" ? `继续执行 ${activeNode?.planKey ?? "下一节点"}` : repairInstruction(problems);
        const prompt = task.nextAction === "revise_plan" ? buildPlanRevisionInstruction(task)
          : task.nextAction === "continue_node" ? buildNodeContinuationInstruction(task, activeNode!) : text;
        useCodingRuntimeStore.getState().setEditorBlock(root, taskId, null);
        dispatched = true;
        if (!beginAgentTurn({ sessionId, promptText: prompt, displayText: text, allowBackgroundSession: true,coding:{mode:"agent",documentToolBridge:true,workspace:root,taskId} })) {
          if (current()) await codingApi.reportStartFailed(root, taskId, "Agent 未能接收自动续跑指令");
          await refresh();
        }
      } catch (error) {
        if (current()) {
          useCodingRuntimeStore.getState().setEditorBlock(root, taskId, `自动续跑尚未启动，请重试：${String(error).replace(/^Error:\s*/, "")}`);
          await codingApi.reportStartFailed(root, taskId, "自动续跑准备失败").catch(() => undefined);
          await refresh().catch(() => undefined);
        }
      }
    })();
    return () => {
      cancelled = true;
      if (!dispatched && agentActionKey.current === key) agentActionKey.current = null;
    };
  }, [awaitingPermission, awaitingQuestion, problems, refresh, restoring, root, sessionId, settling, stopping, cancellationBlocked, streaming, task, taskId, transcript]);

  return null;
}

export function CodingTaskRuntimeManager({ roots, apiReady = true, availableModelIds }: { roots: string[]; apiReady?: boolean; availableModelIds?: string[] }) {
  useEffect(() => {
    let disposed = false; let unlisten: (() => void) | undefined;
    void listenDocumentTools().then(fn => { if (disposed) fn(); else unlisten = fn; });
    let unlistenCancel: (() => void) | undefined;
    let unlistenObserved: (() => void) | undefined;
    let unlistenCancellationState: (() => void) | undefined;
    let refreshing = false;
    let received: DocumentCancellationState[] = [];
    let retryQuery: ReturnType<typeof setTimeout> | undefined;
    const retryCancellations = new Set<ReturnType<typeof setTimeout>>();
    const acknowledging = new Set<string>();
    const confirmCancellation = (pending: DocumentCancellationState & {cancellationId:string}) => {
      if (disposed || acknowledging.has(pending.cancellationId)) return;
      acknowledging.add(pending.cancellationId);
      const acknowledge = async () => {
        if (disposed) return;
        try {
          const editor = workspaceEditor(pending.workspace);
          // A missing renderer registry does not prove that an existing native
          // editor lease has drained its filesystem RPCs.
          const confirmed = editor?.cancelRun ? await editor.cancelRun(pending.runId)
            : (await invoke<boolean>("coding_document_editor_present", {root:pending.workspace})) === false;
          if (disposed) return;
          if (!confirmed) throw new Error("Editor cancellation is not confirmed");
          await invoke("coding_document_cancel_ack", {cancellationId:pending.cancellationId});
          acknowledging.delete(pending.cancellationId);
        } catch {
          if (disposed) return;
          if (useCodingRuntimeStore.getState().documentCancellationReady
            && !useCodingRuntimeStore.getState().documentCancellations[cancellationIdentity(pending)]) {
            acknowledging.delete(pending.cancellationId); return;
          }
          const timer = setTimeout(() => { retryCancellations.delete(timer); void acknowledge(); }, 1_000);
          retryCancellations.add(timer);
        }
      };
      void acknowledge();
    };
    useCodingRuntimeStore.getState().setDocumentCancellationReady(false);
    const refreshCancellations = async () => {
      if (disposed || refreshing) return;
      if (retryQuery) { clearTimeout(retryQuery); retryQuery = undefined; }
      refreshing = true; received = [];
      try {
        const snapshot = await invoke<unknown>("coding_document_cancellation_state", {});
        if (disposed) return;
        if (!Array.isArray(snapshot) || !snapshot.every(validCancellation)) throw new Error("Invalid native cancellation state");
        const state = useCodingRuntimeStore.getState();
        state.replaceDocumentCancellations(snapshot);
        for (const event of received) state.setDocumentCancellation(event);
        state.setDocumentCancellationReady(true);
        // Reconstruct the real handshake after a renderer remount, using the
        // native retained identity. A query never substitutes for editor ACK.
        for (const event of Object.values(useCodingRuntimeStore.getState().documentCancellations)) {
          if (event.cancellationId) confirmCancellation({...event,cancellationId:event.cancellationId});
        }
      } catch {
        if (!disposed) {
          // An unavailable query must not release a previously observed native fence.
          useCodingRuntimeStore.getState().setDocumentCancellationReady(false);
          retryQuery = setTimeout(() => { void refreshCancellations(); }, 5_000);
        }
      } finally { refreshing = false; received = []; }
    };
    void listen<DocumentCancellationState>("agent://document-cancellation-state", event => {
      if (disposed || !validCancellation(event.payload)) return;
      if (refreshing) received.push(event.payload);
      useCodingRuntimeStore.getState().setDocumentCancellation(event.payload);
    }).then(fn => {
      if (disposed) fn(); else { unlistenCancellationState = fn; void refreshCancellations(); }
    }).catch(() => { if (!disposed) useCodingRuntimeStore.getState().setDocumentCancellationReady(false); });
    const onFocus = () => { void refreshCancellations(); };
    window.addEventListener("focus", onFocus);
    void listen<{workspace:string;taskId:string;sessionId:string;runId:string;commandRunId:string;contentRevision:string}>("coding://changes-observed",event=>{
      const state=useTaskStore.getState();
      const owner=workspaceEditor(event.payload.workspace);
      if(state.task?.id===event.payload.taskId && (state.root===event.payload.workspace || owner && owner===workspaceEditor(state.root))) {
        void state.refreshTaskState().catch(()=>undefined);
      }
    }).then(fn=>{if(disposed) fn();else unlistenObserved=fn;});
    void listen<{cancellationId:string;workspace:string;sessionId:string;runId:string}>("agent://document-run-cancelled", event => {
      const pending = { ...event.payload, pending: true };
      if (refreshing) received.push(pending);
      useCodingRuntimeStore.getState().setDocumentCancellation(pending);
      confirmCancellation(pending);
    }).then(fn => { if (disposed) fn(); else unlistenCancel = fn; });
    return () => {
      disposed = true; unlisten?.();unlistenCancel?.();unlistenObserved?.();unlistenCancellationState?.();
      window.removeEventListener("focus", onFocus); if (retryQuery) clearTimeout(retryQuery);
      retryCancellations.forEach(timer => clearTimeout(timer));
    };
  }, []);
  const tracked = useCodingRuntimeStore((state) => state.tasks);
  const queues = useMessageQueueStore(state => state.queues);
  const tasks = useMemo(() => Object.values(tracked), [tracked]);
  const rootsKey = roots.join("\0");
  useEffect(() => {
    // Durable queues keep their owners alive even when no workbench is open.
    for (const key of Object.keys(queues)) {
      const owner = codingQueueOwner(key);
      if (owner) useCodingRuntimeStore.getState().track(owner.root, owner.taskId);
    }
  }, [queues]);
  useEffect(() => {
    let disposed = false;
    const scan = async () => {
      for (const root of roots) {
        const summaries = await codingApi.listTasks(root).catch(() => []);
        if (disposed) return;
        for (const summary of summaries) {
          if (isBusyPhase(summary.phase)) useCodingRuntimeStore.getState().track(root, summary.id);
        }
      }
    };
    void scan();
    const timer = window.setInterval(() => void scan(), 15_000);
    return () => { disposed = true; window.clearInterval(timer); };
  }, [rootsKey]);

  return <>{tasks.map((entry) => <CodingTaskRuntime key={codingRuntimeKey(entry.root, entry.taskId)} {...entry} apiReady={apiReady} availableModelIds={availableModelIds} />)}</>;
}
