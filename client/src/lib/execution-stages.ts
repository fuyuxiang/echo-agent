import type { MessagePart, ToolCallView } from "@/stores/session-store";
import { activeToolLabel } from "./execution-process";
import { isToolDiscovery } from "./tool-renderers";
import { progressStageTitle } from "./execution-process-preview";

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
export function executionStageState(stage: ExecutionStage, current: boolean, active: boolean, waiting = false, recoveredIds?: ReadonlySet<string>): string {
  if (active && current && waiting) return "等待确认";
  if (active && (current || stage.tools.some(tool => tool.status === "in_progress"))) return "进行中";
  if (stage.tools.some(tool => tool.status === "in_progress")) return "未记录完成";
  if (stage.tools.some(tool => tool.status === "failed" && !recoveredIds?.has(tool.toolCallId))) return "含失败记录";
  if (stage.tools.some(tool => recoveredIds?.has(tool.toolCallId))) return "失败已恢复";
  return stage.tools.length ? "已执行" : "已记录";
}

export function stageOperationSummary(tools: ToolCallView[], recoveredIds?: ReadonlySet<string>, executionEnded = false): string {
  if (!tools.length) return "";
  const completed = tools.filter(tool => tool.status === "completed").length;
  const running = tools.filter(tool => tool.status === "in_progress").length;
  const failed = tools.filter(tool => tool.status === "failed" && !recoveredIds?.has(tool.toolCallId)).length;
  const recovered = tools.filter(tool => tool.status === "failed" && recoveredIds?.has(tool.toolCallId)).length;
  const noun = tools.every(isToolDiscovery) ? "工具查找" : "操作";
  return [completed ? `已完成 ${completed} 项${noun}` : "",
    running ? `${running} 项${executionEnded ? "结果待确认" : "进行中"}` : "", failed ? `${failed} 项失败` : "",
    recovered ? `${recovered} 次失败已恢复` : ""].filter(Boolean).join(" · ");
}
