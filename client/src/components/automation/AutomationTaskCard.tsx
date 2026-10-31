import {
  Clock3,
  Play,
  Pencil,
  CheckCircle2,
  AlertCircle,
  ChevronRight,
  Folder,
} from "lucide-react";
import type {
  Automation,
  AutomationRunRecord,
  AutomationStatus,
} from "@/lib/types";
import { Switch } from "./controls";
import { describeSchedule, describeValidity } from "./schedule-utils";
import {
  nextRunLabel,
  orderedRuns,
  runBusy,
  runStatusLabel,
  sessionModeLabel,
  taskState,
  taskStateLabel,
  workspaceName,
} from "./task-presentation";

export function AutomationTaskCard({
  task,
  records,
  busy,
  updating,
  batch,
  selected,
  onSelect,
  onOpen,
  onEdit,
  onRun,
  onToggle,
}: {
  task: Automation;
  records: AutomationRunRecord[];
  busy: boolean;
  updating: boolean;
  batch: boolean;
  selected: boolean;
  onSelect: (id: string) => void;
  onOpen: (task: Automation) => void;
  onEdit: (task: Automation) => void;
  onRun: (id: string) => void;
  onToggle: (id: string, status: AutomationStatus) => void;
}) {
  const last = orderedRuns(records)[0];
  const ended = taskState(task, records) === "ended";
  return (
    <article
      className={`atm-task-card${selected ? " is-selected" : ""}`}
      aria-label={`定时任务 ${task.name}`}
    >
      <div className="atm-task-card__header">
        {batch && (
          <input
            type="checkbox"
            aria-label={`选择定时任务「${task.name}」`}
            checked={selected}
            onChange={() => onSelect(task.id)}
          />
        )}
        <button
          type="button"
          className="atm-task-card__title"
          title={task.name}
          onClick={() => (batch ? onSelect(task.id) : onOpen(task))}
        >
          {task.name}
        </button>
        {!batch && (
          <div className="atm-task-card__toggle">
            {!ended && (
              <Switch
                ariaLabel={`启用定时任务「${task.name}」`}
                checked={task.status === "ACTIVE"}
                disabled={updating}
                onChange={() => onToggle(task.id, task.status)}
              />
            )}
            <span>{taskStateLabel(task, records)}</span>
          </div>
        )}
      </div>
      <p className="atm-task-card__prompt">{task.prompt}</p>
      <div className="atm-task-card__details">
        <div className="atm-task-card__schedule">
          <span>
            <Clock3 size={14} />
            {describeSchedule(task)}
          </span>
          <span className="atm-task-card__next">
            {ended
              ? "计划已结束，可手动运行"
              : task.status === "ACTIVE"
                ? `下次：${nextRunLabel(task.nextRunAt)}`
                : "后续计划已暂停"}
          </span>
        </div>
        <div className="atm-task-card__meta">
          <span title={task.cwds}>
            <Folder size={14} />
            {workspaceName(task.cwds)}
          </span>
          <span>{sessionModeLabel(task)}</span>
          {describeValidity(task) && <span>{describeValidity(task)}</span>}
        </div>
      </div>
      {!batch && (
        <div className="atm-task-card__footer">
          <div
            className={`atm-task-card__activity${last?.needsAttention || last?.status === "failed" ? " needs-attention" : ""}`}
          >
            <span>
              {last ? (
                <>
                  {last.status === "success" ? (
                    <CheckCircle2 size={14} />
                  ) : last.status === "failed" || last.needsAttention ? (
                    <AlertCircle size={14} />
                  ) : (
                    <Clock3 size={14} />
                  )}
                  {runBusy(last) ? "本次运行" : "上次运行"}：{runStatusLabel(last)}
                </>
              ) : (
                "尚未运行"
              )}
            </span>
            <button
              type="button"
              className="atm-task-card__result"
              onClick={() => onOpen(task)}
            >
              {last ? "查看结果" : "查看详情"}
              <ChevronRight size={14} />
            </button>
          </div>
          <div className="atm-task-card__actions">
            <button
              type="button"
              className="atm-toolbar-btn"
              aria-label={`立即运行「${task.name}」`}
              disabled={busy}
              onClick={() => onRun(task.id)}
            >
              <Play size={15} />
              {busy ? "执行中" : "立即运行"}
            </button>
            <button
              type="button"
              className="atm-toolbar-btn"
              aria-label={`编辑定时任务「${task.name}」`}
              onClick={() => onEdit(task)}
            >
              <Pencil size={15} />
              编辑
            </button>
          </div>
        </div>
      )}
    </article>
  );
}
