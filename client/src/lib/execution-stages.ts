import type { MessagePart, ToolCallView } from "@/stores/session-store";
import { activeToolLabel } from "./execution-process";
import { isToolDiscovery } from "./tool-renderers";
import { progressStageTitle } from "./execution-process-preview";
import { executionToolStates, toolExecutionState, type ToolExecutionState } from "./tool-execution-state";

export interface ExecutionStage {
  id: string;
  title: string;
  /** Only real plan snapshots represent numbered task stages. */
  planned: boolean;
  parts: MessagePart[];
  progress: Extract<MessagePart, { kind: "text" }>[];
  thoughts: Extract<MessagePart, { kind: "thought" }>[];
  tools: ToolCallView[];
}

/** Group real events, without inventing a plan, milestones or completion. */
export function executionStages(parts: MessagePart[]): ExecutionStage[] {
  const stages: ExecutionStage[] = [];
  let identity: string | undefined;
  let planTitle: string | undefined;
  let streamIds = new Set<string>();
  for (const part of parts) {
    const key = part.stageTitle ? `plan:${part.stageTitle}` : part.streamId ? `stream:${part.streamId}` : undefined;
    let stage = stages.slice(-1)[0];
    const sameStream = Boolean(part.streamId && streamIds.has(part.streamId));
    const samePlan = Boolean(part.stageTitle && part.stageTitle === planTitle);
    const changedPlan = Boolean(part.stageTitle && planTitle && part.stageTitle !== planTitle);
    if (!stage || changedPlan || key !== identity && key !== undefined && !sameStream && !samePlan
      || !key && !identity && part.kind === "text" && stage.tools.length > 0) {
      identity = key;
      planTitle = undefined;
      streamIds = new Set();
      stage = { id: `${key ?? "legacy"}:${stages.length}`, title: "分析任务", planned: false, parts: [], progress: [], thoughts: [], tools: [] };
      stages.push(stage);
    }
    if (part.streamId) streamIds.add(part.streamId);
    if (part.stageTitle) {
      planTitle = part.stageTitle;
      stage.planned = true;
    }
    stage.parts.push(part);
    if (part.kind === "text" && part.text.trim()) stage.progress.push(part);
    if (part.kind === "thought" && part.text.trim()) stage.thoughts.push(part);
    if (part.kind === "tool_call") stage.tools.push(part.toolCall);
    if (planTitle) stage.title = planTitle;
  }
  for (const stage of stages) {
    if (stage.parts.some(part => part.stageTitle)) continue;
    const description = stage.progress[0] && progressStageTitle(stage.progress[0].text);
    if (description) {
      // Quote a real public stage description, never raw reasoning or an
      // invented plan. Long descriptions remain fully available in the stage.
      stage.title = description;
      continue;
    }
    const activeTool = [...stage.tools].reverse().find(tool => tool.status === "in_progress") ?? stage.tools.slice(-1)[0];
    stage.title = activeTool ? activeToolLabel(activeTool) : "分析任务";
    if (activeTool && !stage.tools.some(tool => tool.status === "in_progress")) stage.title = stage.title.replace(/^正在/, "");
  }
  return stages;
}

/** Last event group is current; older parallel tools stay in their own stage. */
export function executionStageState(stage: ExecutionStage, current: boolean, active: boolean, waiting = false, recoveredIds?: ReadonlySet<string>, execution?: ReadonlyMap<string, ToolExecutionState>): string {
  const state = (tool: ToolCallView) => (execution?.get(tool.toolCallId) ?? toolExecutionState(tool)).state;
  const unfinished = stage.tools.some(tool => state(tool) === "in_progress");
  if (active && current && waiting) return "等待确认";
  if (active && (current || unfinished)) return "进行中";
  if (unfinished) return "未记录完成";
  if (stage.tools.some(tool => state(tool) === "failed" && !recoveredIds?.has(tool.toolCallId))) return "含失败记录";
  if (stage.tools.some(tool => state(tool) === "pending")) return "结果待确认";
  if (stage.tools.some(tool => recoveredIds?.has(tool.toolCallId))) return "失败已恢复";
  return stage.tools.length ? "已执行" : "已记录";
}

export function stageOperationSummary(tools: ToolCallView[], recoveredIds?: ReadonlySet<string>, executionEnded = false, execution = executionToolStates(tools, executionEnded)): string {
  if (!tools.length) return "";
  const states = tools.map(tool => ({ tool, state: execution.get(tool.toolCallId)!.state }));
  const completed = states.filter(({ state }) => state === "completed").length;
  const running = states.filter(({ state }) => state === "in_progress").length;
  const pending = states.filter(({ state }) => state === "pending").length;
  const failed = states.filter(({ tool, state }) => state === "failed" && !recoveredIds?.has(tool.toolCallId)).length;
  const recovered = states.filter(({ tool, state }) => state === "failed" && recoveredIds?.has(tool.toolCallId)).length;
  const noun = tools.every(isToolDiscovery) ? "工具查找" : "操作";
  return [completed ? `已完成 ${completed} 项${noun}` : "",
    running ? `${running} 项进行中` : "", pending ? `${pending} 项结果待确认` : "", failed ? `${failed} 项失败` : "",
    recovered ? `${recovered} 次失败已恢复` : ""].filter(Boolean).join(" · ");
}
