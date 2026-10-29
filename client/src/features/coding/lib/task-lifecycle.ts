import { confirmedRunPaths, releaseDocumentRun } from "./coding-document-tools";
import { useEffect, useRef, useState } from "react";

import { terminalSessionStatus } from "@/lib/turn-status";
import { useSessionStore } from "@/stores/session-store";

import { codingApi } from "./tauri-api";
import { parseRuntimePlan, runtimePlanFingerprint } from "./workflow-plan";
import { useTaskStore } from "../store/task-store";
import type { CodingTask } from "./types";

/**
 * Bridge between Agent session events and the orchestrator's ChangeSet.
 *
 * Three events matter for the workbench, and all three are driven by the
 * shared session store rather than by parsing tool calls:
 *
 * 1. ACP plan updates are parsed into the persisted execution DAG while the
 *    Agent is working, so progress survives reloads and context compaction.
 * 2. Streaming finishes while the task is Discovering, Implementing or
 *    Repairing → synchronize the final plan and workspace state before the
 *    orchestrator decides whether implementation is complete.
 * 3. Every phase event the orchestrator emits is already mirrored into the
 *    task store (this is wired in the workbench shell).
 *
 * Parsing tool calls to figure out what the Agent did is exactly the kind of
 * fragile inference the old implementation got wrong. Git is used when it is
 * available; ordinary folders use an application-owned checkpoint.
 */
export function useTaskLifecycle(
  cwd: string | undefined,
  runtimeTask?: CodingTask | null,
  onChanged?: () => Promise<void>,
) {
  const selectedTask = useTaskStore((state) => state.task);
  const task = runtimeTask === undefined ? selectedTask : runtimeTask;
  const taskId = task?.id;
  const taskPhase = task?.phase;
  const taskPhaseReason = task?.phaseReason;
  const taskBlocker = task?.blocker;
  const taskSessionId = task?.sessionId;
  const refresh = onChanged ?? useTaskStore.getState().refreshTaskState;
  const streaming = useSessionStore((state) =>
    taskSessionId ? Boolean(state.transcripts[taskSessionId]?.streamingMessageId) : false,
  );
  const runtimePlan = useSessionStore((state) =>
    taskSessionId ? state.transcripts[taskSessionId]?.plan ?? null : null,
  );
  const terminalMessage = useSessionStore((state) => {
    const messages = taskSessionId ? state.transcripts[taskSessionId]?.messages : undefined;
    if (!messages) return undefined;
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      const message = messages[index];
      if (message.role === "assistant" && message.complete && message.stopReason) return message;
    }
    return undefined;
  });
  const planFingerprint = runtimePlanFingerprint(runtimePlan);
  const lastStreamingRef = useRef(streaming);
  const lastPlanFingerprintRef = useRef("");
  const lastSettledCompletionRef = useRef("");
  const settlingRef = useRef(false);
  const [settling, setSettling] = useState(false);

  // A different task/session is a different stream edge detector.
  useEffect(() => {
    lastStreamingRef.current = streaming;
    lastPlanFingerprintRef.current = "";
    lastSettledCompletionRef.current = "";
    settlingRef.current = false;
    setSettling(false);
  }, [taskId, taskSessionId]);

  useEffect(() => {
    if (
      !cwd
      || !taskId
      || !taskSessionId
      || !task?.strictWorkflow
      || !runtimePlan
      || runtimePlan.entries.length === 0
      || !["discovering", "implementing", "repairing"].includes(taskPhase ?? "")
      || planFingerprint === lastPlanFingerprintRef.current
    ) return;
    lastPlanFingerprintRef.current = planFingerprint;
    void codingApi
      .syncPlan(cwd, taskId, parseRuntimePlan(runtimePlan))
      .then(() => refresh())
      .catch(async (error) => {
        lastPlanFingerprintRef.current = "";
        await codingApi.reportStartFailed(
          cwd,
          taskId,
          `执行计划无法持久化：${String(error).replace(/^Error:\s*/, "")}`,
        ).catch(() => undefined);
        await refresh().catch(() => undefined);
      });
  }, [cwd, planFingerprint, refresh, runtimePlan, taskId, taskPhase, taskSessionId]);

  const completionKey = terminalMessage
    ? `${terminalMessage.promptId ?? terminalMessage.id}:${terminalMessage.stopReason}:${terminalMessage.cancelTrigger ?? ""}:${terminalMessage.cancellationCategory ?? ""}`
    : "";

  // A stream ending is not necessarily a successful implementation. Settle
  // the round from its protocol outcome so Stop/Pause/Send-now cannot be
  // mistaken for a clean end_turn.
  useEffect(() => {
    if (lastStreamingRef.current === streaming) return;
    lastStreamingRef.current = streaming;
    if (
      streaming
      || !cwd
      || !taskId
      || !taskSessionId
      || !["discovering", "implementing", "repairing"].includes(taskPhase ?? "")
    ) return;
    if (completionKey && completionKey === lastSettledCompletionRef.current) return;
    if (completionKey) lastSettledCompletionRef.current = completionKey;
    const outcome = terminalMessage
      ? terminalSessionStatus({
          stopReason: terminalMessage.stopReason ?? "end_turn",
          cancelTrigger: terminalMessage.cancelTrigger,
          cancellationCategory: terminalMessage.cancellationCategory,
        })
      : "completed";
    // send_now closes the superseded turn immediately before the replacement
    // starts. It is a hand-off, not a task lifecycle boundary.
    if (outcome === "working") return;

    settlingRef.current = true;
    setSettling(true);
    void (async () => {
      try {
        if (terminalMessage?.promptId) {
          await codingApi.checkpointStart(cwd,taskId,terminalMessage.promptId);
          await codingApi.checkpointFinish(cwd,taskId,terminalMessage.promptId,confirmedRunPaths(taskSessionId,terminalMessage.promptId));
          releaseDocumentRun(taskSessionId,terminalMessage.promptId);
        }
        if (runtimePlan?.entries.length && task?.strictWorkflow) {
          await codingApi.syncPlan(cwd, taskId, parseRuntimePlan(runtimePlan));
        }
        if (outcome === "paused" || outcome === "stopped") {
          await codingApi.reportInterrupted(cwd, taskId, outcome);
        } else if (outcome === "failed") {
          await codingApi.syncChanges(cwd, taskId);
          const detail = terminalMessage?.agentResult?.trim();
          await codingApi.reportStartFailed(
            cwd,
            taskId,
            detail || "Agent 执行异常结束，已保留当前文件变更和执行记录",
          );
        } else {
          await codingApi.syncChanges(cwd, taskId);
          await codingApi.reportImplementation(cwd, taskId, terminalMessage?.promptId);
        }
        await refresh();
      } catch (error) {
        // Continuing after a failed sync could falsely report a clean task.
        await codingApi
          .reportStartFailed(
            cwd,
            taskId,
            `无法同步 Agent 产生的文件变更：${String(error).replace(/^Error:\s*/, "")}`,
          )
          .catch(() => undefined);
        await refresh().catch(() => undefined);
      } finally {
        settlingRef.current = false;
        setSettling(false);
      }
    })();
  }, [
    completionKey,
    cwd,
    runtimePlan,
    refresh,
    streaming,
    taskId,
    taskPhase,
    taskSessionId,
    terminalMessage,
  ]);

  // Repair the one blocker produced by older versions of this bug. The exact
  // persisted reason plus a cancelled terminal outcome makes this safe: real
  // plan, verification and runtime blockers are never rewritten.
  useEffect(() => {
    if (
      streaming
      || settlingRef.current
      || !cwd
      || !taskId
      || taskPhase !== "blocked"
      || taskPhaseReason !== "实现阶段结束但没有代码变更"
      || taskBlocker !== "Agent 结束了实现但没有写入任何文件。请检查是否只在会话里返回了示例代码。"
      || !terminalMessage
    ) return;
    const outcome = terminalSessionStatus({
      stopReason: terminalMessage.stopReason ?? "end_turn",
      cancelTrigger: terminalMessage.cancelTrigger,
      cancellationCategory: terminalMessage.cancellationCategory,
    });
    if (outcome !== "paused" && outcome !== "stopped") return;
    const repairKey = `repair:${completionKey}`;
    if (lastSettledCompletionRef.current === repairKey) return;
    lastSettledCompletionRef.current = repairKey;
    settlingRef.current = true;
    setSettling(true);
    void codingApi
      .reportInterrupted(cwd, taskId, outcome)
      .then(() => refresh())
      .catch(() => undefined)
      .finally(() => {
        settlingRef.current = false;
        setSettling(false);
      });
  }, [
    completionKey,
    cwd,
    refresh,
    streaming,
    taskBlocker,
    taskId,
    taskPhase,
    taskPhaseReason,
    terminalMessage,
  ]);

  return { settling };
}
