import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";

import type { ConfirmationOptions } from "@/components/AppDialog";

import { useTaskStore } from "../store/task-store";
import { codingApi } from "./tauri-api";
import type { CodingTask, DetectedCommand } from "./types";
import { codingRuntimeKey, codingWorkspaceCancellationBlocked, codingWorkspaceCancellationPending, useCodingRuntimeStore } from "./coding-task-runtime";
import { useCodingActionScope } from "../hooks/useCodingActionScope";

interface VerificationRunnerOptions {
  cwd: string;
  task: CodingTask | null;
  commands: DetectedCommand[];
  detectedReady: boolean;
  setCommandOutput: Dispatch<SetStateAction<string>>;
  requestConfirmation: (options: ConfirmationOptions) => void;
  prepareEditor?: () => Promise<string | void>;
  onToast?: (message: string) => void;
}

/**
 * Owns verification consent, process lifecycle and automatic phase handling.
 * Keeping this state machine outside the workbench prevents navigation and
 * editor rendering concerns from becoming coupled to command execution.
 */
export function useVerificationRunner({
  cwd,
  task,
  commands,
  detectedReady,
  setCommandOutput,
  requestConfirmation,
  onToast,
  prepareEditor,
}: VerificationRunnerOptions) {
  const runningVerification = useCodingRuntimeStore((state) => task
    ? Boolean(state.verificationBatches[codingRuntimeKey(cwd, task.id)]) : false);
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const activeRunIdRef = useRef<string | null>(null);
  const autoVerificationRef = useRef<string | null>(null);
  const isCurrent = useCodingActionScope(cwd, task?.id);
  const cancellationBlocked = useCodingRuntimeStore(state => !state.documentCancellationReady || codingWorkspaceCancellationPending(cwd, state));

  useEffect(() => {
    activeRunIdRef.current = null;
    setActiveRunId(null);
    autoVerificationRef.current = null;
    setCommandOutput("");
  }, [cwd, task?.id, setCommandOutput]);

  const executeVerifications = useCallback(
    async (batch: DetectedCommand[]) => {
      if (!isCurrent() || !cwd || !task || runningVerification) return;
      const taskId = task.id;
      const key = codingRuntimeKey(cwd, taskId);
      const epoch = useCodingRuntimeStore.getState().actionEpochs[key] ?? 0;
      const allowed = () => !useCodingRuntimeStore.getState().stopping[key]
        && !codingWorkspaceCancellationBlocked(cwd)
        && epoch === (useCodingRuntimeStore.getState().actionEpochs[key] ?? 0);
      if (!allowed()) return;
      if (!["verifying", "paused", "stopped", "blocked", "delivered"].includes(task.phase)) {
        onToast?.("当前任务正在执行，暂不能启动新的验证批次");
        return;
      }
      // Claim before beginVerification emits its phase event. Otherwise the
      // background manager can start the same batch and clear its evidence.
      const owner = useCodingRuntimeStore.getState().claimVerificationBatch(cwd, taskId);
      if (!owner) return;
      const refreshCurrent = async () => {
        const selected = useTaskStore.getState();
        if (selected.root === cwd && selected.task?.id === taskId) await selected.refreshTaskState();
      };
      try {
        const savedRevision = await prepareEditor?.();
        if (!isCurrent() || !allowed()) { useCodingRuntimeStore.getState().releaseVerificationBatch(cwd, taskId, owner); return; }
        useCodingRuntimeStore.getState().setEditorBlock(cwd, taskId, null);
        const startedTask = await codingApi.beginVerification(cwd, taskId, savedRevision || task.updatedAt);
        if (isCurrent()) autoVerificationRef.current = `${cwd}:${taskId}:${startedTask.updatedAt}`;
        await refreshCurrent();
      } catch (error) {
        if (isCurrent() && allowed()) onToast?.(`无法启动验证：${String(error).replace(/^Error:\s*/, "")}`);
        await refreshCurrent().catch(() => undefined);
        useCodingRuntimeStore.getState().releaseVerificationBatch(cwd, taskId, owner);
        return;
      }
      if (isCurrent()) setCommandOutput("");
      let mayReport = batch.length === 0;
      try {
        for (const command of batch) {
          if (!allowed()) { mayReport = false; break; }
          const runId = crypto.randomUUID();
          useCodingRuntimeStore.getState().setActiveRunId(cwd, taskId, runId);
          if (isCurrent()) {
            activeRunIdRef.current = runId;
            setActiveRunId(runId);
          }
          try {
            const approvalToken = command.requiresApproval
              ? await codingApi.approvePlanCommand(cwd, taskId, command.command)
              : undefined;
            if (!allowed()) { mayReport = false; break; }
            const record = await codingApi.runVerification(
              cwd,
              taskId,
              command.kind,
              command.command,
              undefined,
              runId,
              approvalToken,
            );
            mayReport = record.status !== "cancelled";
            if (record.status !== "passed") break;
          } catch (error) {
            if (!allowed()) { mayReport = false; break; }
            const detail = String(error).replace(/^Error:\s*/, "");
            if (isCurrent()) onToast?.(`执行 ${command.command} 失败：${detail}`);
            await codingApi.reportStartFailed(
              cwd,
              taskId,
              `验证命令“${command.command}”未能执行：${detail}`,
            ).catch(() => undefined);
            await refreshCurrent().catch(() => undefined);
            mayReport = false;
            break;
          }
        }
        if (mayReport && allowed()) {
          await codingApi.reportVerification(cwd, taskId);
          await refreshCurrent();
        }
      } catch (error) {
        if (!allowed()) return;
        const detail = String(error).replace(/^Error:\s*/, "");
        if (isCurrent()) onToast?.(`无法更新验证结果：${detail}`);
        await codingApi.reportStartFailed(
          cwd,
          taskId,
          `无法收敛本轮验证结果：${detail}`,
        ).catch(() => undefined);
        await refreshCurrent().catch(() => undefined);
      } finally {
        useCodingRuntimeStore.getState().setActiveRunId(cwd, taskId, null);
        useCodingRuntimeStore.getState().releaseVerificationBatch(cwd, taskId, owner);
        if (isCurrent()) {
          activeRunIdRef.current = null;
          setActiveRunId(null);
        }
      }
    },
    [cwd, isCurrent, onToast, prepareEditor, runningVerification, setCommandOutput, task],
  );

  const runVerifications = useCallback((batch: DetectedCommand[]) => {
    const planCommands = batch.filter((command) => command.requiresApproval);
    if (planCommands.length === 0) {
      void executeVerifications(batch);
      return;
    }
    requestConfirmation({
      title: "确认运行计划中的验证命令",
      description: (
        <div>
          <p>以下命令由 Agent 执行计划声明，其中可能与工程清单命令重合：</p>
          <ul>
            {planCommands.map((command) => (
              <li key={command.command}><code>{command.command}</code></li>
            ))}
          </ul>
          <p>命令将在当前工作区运行。请仅在确认命令内容可信后继续。</p>
        </div>
      ),
      confirmLabel: "确认并运行",
      danger: true,
      action: () => executeVerifications(batch),
    });
  }, [executeVerifications, onToast, requestConfirmation]);

  useEffect(() => {
    if (task?.phase !== "verifying") {
      autoVerificationRef.current = null;
      return;
    }
    if (!detectedReady || runningVerification || cancellationBlocked || !commands.some((command) => command.requiresApproval)) return;
    const key = `${cwd}:${task.id}:${task.updatedAt}`;
    if (autoVerificationRef.current === key) return;
    autoVerificationRef.current = key;
    runVerifications(commands);
  }, [commands, cwd, detectedReady, runVerifications, runningVerification, task, cancellationBlocked]);

  return {
    activeRunId,
    activeRunIdRef,
    runningVerification,
    runVerifications,
  };
}
