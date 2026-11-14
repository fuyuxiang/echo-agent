import { BookOpen, ChartNoAxesCombined, Check, ChevronRight, CloudSun, FilePenLine, FileSearch,
  Image, Mail, MessageSquare, Search, Terminal, Users, Wrench, type LucideIcon } from "lucide-react";
import { ToolCallCard } from "./ToolCallCard";
import type { ExecutionActivity as Activity } from "@/lib/execution-activity";
import type { ToolRenderer } from "@/lib/tool-renderers";
import type { ToolCallView } from "@/stores/session-store";
import { stageOperationSummary } from "@/lib/execution-stages";
import { toolExecutionState, type ToolExecutionState } from "@/lib/tool-execution-state";

const icons: Partial<Record<ToolRenderer, LucideIcon>> = {
  command: Terminal, read: FileSearch, edit: FilePenLine, search: Search, weather: CloudSun,
  task: Users, "team-create": Users, "team-delete": Users, "team-status": Users,
  "image-gen": Image, visualizer: ChartNoAxesCombined, "send-message": MessageSquare,
  "agent-mail": Mail, knowledge: BookOpen, "personal-knowledge": BookOpen,
};

export function ExecutionActivity({ activity, onOpenTool, onOpenRecords, executionEnded, execution }: {
  activity: Activity;
  onOpenTool?: (tool: ToolCallView) => void;
  onOpenRecords: () => void;
  executionEnded: boolean;
  execution?: ReadonlyMap<string, ToolExecutionState>;
}) {
  const Icon = icons[activity.renderer] ?? Wrench;
  if (activity.tools.length === 1) return <li className="execution-process__activity" data-execution-activity-id={activity.id}>
    <ToolCallCard tc={activity.tools[0]} onOpen={onOpenTool ?? onOpenRecords}
      appearance="activity" activityLabel={activity.label} activityIcon={<Icon size={15} />}
      executionState={execution?.get(activity.tools[0].toolCallId)}
      executionEnded={executionEnded} />
  </li>;
  const completed = activity.tools.every(tool => (execution?.get(tool.toolCallId) ?? toolExecutionState(tool, executionEnded)).state === "completed");
  const status = completed ? "已完成" : stageOperationSummary(activity.tools, undefined, executionEnded, execution);
  return <li className="execution-process__activity" data-execution-activity-id={activity.id}>
    <button type="button" className="execution-process__activity-group"
      onClick={onOpenRecords} aria-label={`${activity.label} · ${status}，查看全部过程`}>
      <Icon size={15} aria-hidden="true" />
      <span>{activity.label}</span>
      {completed ? <Check size={13} className="execution-process__activity-check" aria-hidden="true" />
        : <small>{status}</small>}
      <ChevronRight size={13} className="execution-process__activity-more" aria-hidden="true" />
    </button>
  </li>;
}
