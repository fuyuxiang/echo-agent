import { modelChoiceLabel, modelDisplayName, type PresentableModel } from "@/lib/model-presentation";
/**
 * AutomationEditPage — 添加 / 编辑自动化任务（全页表单，截图 2 复刻）。
 *
 * 对应 EchoAgent automation-panel/index.tsx 的 EditModal：
 *  - 头部：闹钟图标面包屑「自动化 / 添加自动化任务」+ 取消 / 保存（黑）
 *  - 名称、工作空间(可选)、提示词（底部工具条：模型/技能/召唤专家/权限）
 *  - 连接器、执行频率（周期/按间隔/单次）、生效日期区间、完成通知
 *  - 编辑态：创建时间 + 运行历史
 */
import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  AlarmClockIcon,
  ArchiveIcon,
  CheckBoldIcon,
  CheckIcon,
  ChevronDownIcon,
  DeleteIcon,
  ErrorCircleIcon,
  ExpertIcon,
  FolderOpenIcon,
  GlobeIcon,
  PlayIcon,
  RunningStatusIcon,
  SkillIcon,
} from "@/foundation/components/Icon/icons";
import type {
  AutomationPermissionMode,
  AutomationRunRecord,
  AutomationSchedule,
} from "@/lib/types";
import type { AgentEntry, SkillInfo } from "@/lib/types";
import { filesystemPickDirectory, type WorkspaceInfo } from "@/lib/agent-client";
import {
  CustomSelect,
  IntervalDayChips,
  MonthdayMultiPicker,
  Segmented,
  SingleDatePicker,
  Switch,
  TimePicker,
  ValidityRangePicker,
  WeekdayMultiPicker,
} from "./controls";
import { ConnectorSelector, type ConnectorOption } from "./ConnectorSelector";
import { AutomationPermissionPicker } from "./AutomationPermissionPicker";
import {
  ALL_DAYS,
  MONTH_DAYS,
  MONTHS,
  getYearlyMaxDay,
  inferPeriodicMode,
  pad2,
  parseTimeValue,
  scheduleModeOf,
  sortMonthDays,
  sortMonths,
  sortWeekdays,
  type AutomationDraft,
  type PeriodicMode,
  type ScheduleMode,
  type WeekdayCode,
} from "./schedule-utils";

export type ModelOption = PresentableModel;

// ============================================================
// 提示词工具条通用 chip 下拉
// ============================================================

function ToolbarDropdown({
  trigger,
  children,
  disabled,
  closeOnSelect = false,
}: {
  trigger: React.ReactNode;
  children: React.ReactNode;
  disabled?: boolean;
  closeOnSelect?: boolean;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuId = useId();

  useEffect(() => {
    if (!isOpen) return;
    if (disabled) { setIsOpen(false); return; }
    // WebKit mouse clicks can blur the focused option with relatedTarget=null
    // before click. Only an actual outside pointer/focus target closes the menu.
    const handler = (e: MouseEvent | FocusEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) setIsOpen(false);
    };
    const keyHandler = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setIsOpen(false);
      triggerRef.current?.focus();
    };
    containerRef.current?.querySelector<HTMLButtonElement>(".atm-chip-dropdown__menu button:not(:disabled)")?.focus();
    document.addEventListener("mousedown", handler);
    document.addEventListener("focusin", handler);
    document.addEventListener("keydown", keyHandler);
    return () => {
      document.removeEventListener("mousedown", handler);
      document.removeEventListener("focusin", handler);
      document.removeEventListener("keydown", keyHandler);
    };
  }, [isOpen, disabled]);

  return (
    <div className={`atm-chip-dropdown${isOpen ? " atm-chip-dropdown--open" : ""}`} ref={containerRef}>
      <button
        type="button"
        ref={triggerRef}
        className="atm-prompt-chip"
        aria-expanded={isOpen}
        aria-controls={isOpen ? menuId : undefined}
        disabled={disabled}
        onClick={() => setIsOpen((v) => !v)}
      >
        {trigger}
      </button>
      {isOpen && (
        <div
          id={menuId}
          className="atm-chip-dropdown__menu"
          onClick={(event) => {
            if (closeOnSelect && (event.target as HTMLElement).closest("button")) {
              setIsOpen(false);
              triggerRef.current?.focus();
            }
          }}
        >
          {children}
        </div>
      )}
    </div>
  );
}

function ModelChip({
  models,
  value,
  autoModelId,
  onChange,
  disabled,
}: {
  models: ModelOption[];
  value?: string;
  autoModelId?: string;
  onChange: (id?: string) => void;
  disabled?: boolean;
}) {
  const selected = models.find((m) => m.id === value);
  const autoModel = models.find((m) => m.id === autoModelId);
  const invalidSelection = !!value && !selected;
  return (
    <ToolbarDropdown
      disabled={disabled}
      closeOnSelect
      trigger={
        <>
          <GlobeIcon size="sm" />
          <span className={`atm-prompt-chip-label${invalidSelection ? " atm-prompt-chip-label--invalid" : ""}`}>
            {selected ? modelChoiceLabel(selected, models) : (invalidSelection ? `模型不可用：${modelDisplayName(value)}` : `Auto${autoModel ? ` · ${modelDisplayName(autoModel)}` : ""}`)}
          </span>
          <ChevronDownIcon size="sm" className="atm-prompt-chip-caret" />
        </>
      }
    >
      <button type="button" className={`atm-chip-option${!value ? " active" : ""}`} onClick={() => onChange(undefined)}>
        <span className="atm-chip-option-check">{!value && <CheckIcon size="sm" />}</span>
        <span className="atm-chip-option-copy">
          <span>Auto</span>
          <small>{autoModel ? `每次运行使用当前默认模型：${modelChoiceLabel(autoModel, models)}` : "当前没有可用的默认模型"}</small>
        </span>
      </button>
      {invalidSelection && (
        <div className="atm-chip-invalid">原模型“{modelDisplayName(value)}”已移除或不可用，请重新选择。</div>
      )}
      {models.map((m) => (
        <button
          key={m.id}
          type="button"
          className={`atm-chip-option${value === m.id ? " active" : ""}`}
          onClick={() => onChange(m.id)}
        >
          <span className="atm-chip-option-check">{value === m.id && <CheckIcon size="sm" />}</span>
          <span>{modelChoiceLabel(m, models)}</span>
        </button>
      ))}
    </ToolbarDropdown>
  );
}

function SkillChip({
  skills,
  selected,
  onToggle,
  disabled,
}: {
  skills: SkillInfo[];
  selected: string[];
  onToggle: (name: string) => void;
  disabled?: boolean;
}) {
  return (
    <ToolbarDropdown
      disabled={disabled}
      trigger={
        <>
          <SkillIcon size="sm" />
          <span className="atm-prompt-chip-label">
            技能{selected.length > 0 ? `(${selected.length})` : ""}
          </span>
          <ChevronDownIcon size="sm" className="atm-prompt-chip-caret" />
        </>
      }
    >
      {skills.length === 0 && <div className="atm-chip-empty">暂无可用技能</div>}
      {skills.map((s) => (
        <button
          key={s.name}
          type="button"
          className={`atm-chip-option${selected.includes(s.name) ? " active" : ""}`}
          onClick={() => onToggle(s.name)}
        >
          <span className="atm-chip-option-check">{selected.includes(s.name) && <CheckIcon size="sm" />}</span>
          <span>{s.displayName || s.name}</span>
        </button>
      ))}
    </ToolbarDropdown>
  );
}

function ExpertChip({
  experts,
  selectedId,
  selectedName,
  selectedPath,
  onChange,
  disabled,
}: {
  experts: AgentEntry[];
  selectedId?: string;
  selectedName?: string;
  selectedPath?: string;
  onChange: (id?: string, name?: string, path?: string) => void;
  disabled?: boolean;
}) {
  return (
    <ToolbarDropdown
      disabled={disabled}
      trigger={
        <>
          <ExpertIcon size="sm" />
          <span className="atm-prompt-chip-label">{selectedId ? `${selectedName || selectedId}${selectedPath ? "" : " · 需重新选择"}` : "召唤专家"}</span>
          <ChevronDownIcon size="sm" className="atm-prompt-chip-caret" />
        </>
      }
    >
      <button type="button" className={`atm-chip-option${!selectedId ? " active" : ""}`} onClick={() => onChange(undefined, undefined, undefined)}>
        <span className="atm-chip-option-check">{!selectedId && <CheckIcon size="sm" />}</span>
        <span>不使用专家</span>
      </button>
      {experts.map((e) => (
        <button
          key={e.path}
          type="button"
          title={e.path}
          className={`atm-chip-option${selectedId === e.name && selectedPath === e.path ? " active" : ""}`}
          onClick={() => onChange(e.name, e.name, e.path)}
        >
          <span className="atm-chip-option-check">{selectedId === e.name && selectedPath === e.path && <CheckIcon size="sm" />}</span>
          <span>{e.name}</span>
        </button>
      ))}
    </ToolbarDropdown>
  );
}

// ============================================================
// 工作空间输入（全宽输入框 + 可选下拉，对齐截图 2）
// ============================================================

function WorkspaceInput({
  workspaces,
  value,
  onChange,
  disabled,
}: {
  workspaces: WorkspaceInfo[];
  value: string;
  onChange: (cwd: string) => void;
  disabled?: boolean;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const [isPicking, setIsPicking] = useState(false);
  const [pickError, setPickError] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);
  const matches = workspaces.filter((workspace) => (
    workspace.cwd !== value
    && workspace.cwd.toLocaleLowerCase().includes(value.trim().toLocaleLowerCase())
  )).slice(0, 8);

  useEffect(() => {
    if (!isOpen) return;
    const handler = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) setIsOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [isOpen]);

  const pickFolder = async () => {
    setIsOpen(false);
    setPickError("");
    setIsPicking(true);
    try {
      const selected = await filesystemPickDirectory(value.trim() || undefined);
      if (selected) onChange(selected);
    } catch (error) {
      setPickError(`选择文件夹失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setIsPicking(false);
    }
  };

  return (
    <div className="atm-workspace-input" ref={containerRef} onBlur={(event) => {
      if (!containerRef.current?.contains(event.relatedTarget as Node)) setIsOpen(false);
    }}>
      <div className="atm-workspace-input__field">
        <input
          id="automation-workspace-path"
          type="text"
          className="atm-modal-input"
          value={value}
          disabled={disabled || isPicking}
          placeholder="输入已授权的文件夹路径，或从历史目录选择"
          autoComplete="off"
          spellCheck={false}
          onChange={(event) => {
            onChange(event.target.value);
            setPickError("");
            setIsOpen(true);
          }}
          onFocus={() => setIsOpen(true)}
          onKeyDown={(event) => {
            if (event.key === "Escape") setIsOpen(false);
          }}
        />
        <button
          type="button"
          className="atm-workspace-input__browse"
          disabled={disabled || isPicking}
          onClick={() => void pickFolder()}
        >
          <FolderOpenIcon size="sm" />
          <span>{isPicking ? "选择中…" : "选择文件夹"}</span>
        </button>
      </div>
      {isOpen && matches.length > 0 && (
        <div className="atm-workspace-input__dropdown">
          {matches.map((w) => (
            <button
              key={w.cwd}
              type="button"
              className="atm-workspace-input__option"
              title={w.cwd}
              onClick={() => {
                onChange(w.cwd);
                setIsOpen(false);
              }}
            >
              <span className="atm-workspace-input__option-path">{w.cwd}</span>
              {w.lastTitle && <span className="atm-workspace-input__option-title">{w.lastTitle}</span>}
            </button>
          ))}
        </div>
      )}
      <div className="atm-workspace-input__hint">新目录请点击「选择文件夹」确认授权；留空则使用默认工作目录。</div>
      {pickError && <div className="atm-workspace-input__error" role="alert">{pickError}</div>}
    </div>
  );
}

// ============================================================
// 运行历史（编辑态）
// ============================================================

type HistoryFilter = "all" | "success" | "failed" | "running" | "archived";

const HISTORY_FILTERS: { key: HistoryFilter; label: string }[] = [
  { key: "all", label: "全部" },
  { key: "success", label: "成功" },
  { key: "failed", label: "失败" },
  { key: "running", label: "运行中" },
  { key: "archived", label: "已归档" },
];

function RunStatusIcon({ status }: { status: string }) {
  if (status === "running") {
    return (
      <span className="atm-status-icon-spinning" style={{ display: "inline-flex" }}>
        <RunningStatusIcon size={16} color="#00C29A" />
      </span>
    );
  }
  if (status === "success") return <CheckBoldIcon size={16} color="var(--echo-color-text-disabled, #000)" />;
  if (status === "failed") return <ErrorCircleIcon size={16} />;
  return <CheckIcon size={16} />;
}

function RunHistory({
  records,
  retrying,
  onRetry,
  onArchive,
  onDelete,
  onOpenSession,
}: {
  records: AutomationRunRecord[];
  retrying?: boolean;
  onRetry: (automationId: string) => void;
  onArchive: (id: string) => void;
  onDelete: (id: string) => void;
  onOpenSession?: (sessionId: string) => void;
}) {
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const [filterOpen, setFilterOpen] = useState(false);
  const filterRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!filterOpen) return;
    const handler = (e: MouseEvent) => {
      if (filterRef.current && !filterRef.current.contains(e.target as Node)) setFilterOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [filterOpen]);

  const filtered = useMemo(() => {
    const items = records.filter((item) => {
      if (filter === "all") return true;
      if (filter === "archived") return !!item.archived;
      if (filter === "success") return item.status === "success" && !item.archived;
      if (filter === "failed") return item.status === "failed" && !item.archived;
      if (filter === "running") return item.status === "running" && !item.archived;
      return true;
    });
    return [...items].sort((a, b) => (a.archived ? 1 : 0) - (b.archived ? 1 : 0));
  }, [records, filter]);

  return (
    <div className="atm-detail-run-history">
      <div className="atm-detail-run-history-header">
        <span className="atm-detail-run-history-title">运行历史 ({records.length})</span>
        <div className="atm-filter-wrap" ref={filterRef}>
          <button
            type="button"
            className={`atm-detail-filter-btn${filter !== "all" ? " atm-detail-filter-btn--active" : ""}`}
            onClick={() => setFilterOpen((v) => !v)}
          >
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round">
              <path d="M2 4h12M4.5 8h7M6.5 12h3" />
            </svg>
            {filter !== "all" && <span className="atm-filter-dot" />}
          </button>
          {filterOpen && (
            <div className="atm-filter-menu">
              {HISTORY_FILTERS.map((f) => (
                <button
                  key={f.key}
                  type="button"
                  className={`atm-chip-option${filter === f.key ? " active" : ""}`}
                  onClick={() => {
                    setFilter(f.key);
                    setFilterOpen(false);
                  }}
                >
                  <span className="atm-chip-option-check">{filter === f.key && <CheckIcon size="sm" />}</span>
                  <span>{f.label}</span>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
      <div className="atm-detail-run-history-list">
        {filtered.length === 0 && <div className="atm-panel-empty">暂无运行记录</div>}
        {filtered.map((item) => {
          const canArchive = !item.archived && item.status !== "queued" && item.status !== "running";
          const canRetry = !item.archived && item.status === "failed";
          return (
            <div
              key={item.id}
              className={`atm-run-history-item${canArchive ? " atm-run-history-item--archivable" : ""}${item.archived ? " atm-run-history-item--archived" : ""}${item.sessionId ? " atm-run-history-item--openable" : ""}`}
              title={item.error || (item.sessionId ? "打开本次自动化会话" : undefined)}
              role={item.sessionId ? "button" : undefined}
              tabIndex={item.sessionId ? 0 : undefined}
              onClick={() => item.sessionId && onOpenSession?.(item.sessionId)}
              onKeyDown={(event) => {
                if (item.sessionId && (event.key === "Enter" || event.key === " ")) {
                  event.preventDefault();
                  onOpenSession?.(item.sessionId);
                }
              }}
            >
              <span className="atm-run-history-item-name">
                {item.automationName}
                {item.error && <span className="atm-run-history-item-error"> · {item.error}</span>}
              </span>
              <div className="atm-run-history-item-right">
                <span className="atm-run-history-item-time">
                  {new Date(item.finishedAt || item.startedAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                </span>
                <span className={`atm-run-history-item-status ${item.status}`}>
                  {item.archived ? (
                    <ArchiveIcon size={16} color="var(--echo-color-text-disabled, #000)" />
                  ) : (
                    <RunStatusIcon status={item.status} />
                  )}
                </span>
                {canRetry && (
                  <button
                    type="button"
                    className="atm-run-history-item-retry"
                    title={retrying ? "正在重新运行" : "使用当前任务配置重新运行"}
                    aria-label={`重新运行“${item.automationName}”`}
                    disabled={retrying}
                    onClick={(event) => {
                      event.stopPropagation();
                      onRetry(item.automationId);
                    }}
                  >
                    <PlayIcon width={16} height={16} />
                  </button>
                )}
                {canArchive && (
                  <button
                    type="button"
                    className="atm-run-history-item-archive"
                    title="归档"
                    onClick={(e) => {
                      e.stopPropagation();
                      onArchive(item.id);
                    }}
                  >
                    <ArchiveIcon width={16} height={16} />
                  </button>
                )}
                {item.archived && (
                  <button
                    type="button"
                    className="atm-run-history-item-delete"
                    title="删除"
                    onClick={(e) => {
                      e.stopPropagation();
                      onDelete(item.id);
                    }}
                  >
                    <DeleteIcon width={16} height={16} />
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ============================================================
// 编辑页主体
// ============================================================

const PERIODIC_OPTIONS: { label: string; value: PeriodicMode }[] = [
  { label: "每天", value: "day" },
  { label: "每周", value: "week" },
  { label: "双周", value: "biweek" },
  { label: "每月", value: "month" },
  { label: "每年", value: "year" },
];

export function AutomationEditPage({
  mode,
  draft,
  setDraft,
  saving,
  retrying,
  workspaces,
  models,
  autoModelId,
  skills,
  experts,
  connectors,
  records,
  createdAt,
  onSave,
  onClose,
  onBack,
  onTest,
  onDelete,
  onOpenConnectorSettings,
  onArchiveRecord,
  onRetryRecord,
  onDeleteRecord,
  onOpenSession,
}: {
  mode: "create" | "edit";
  draft: AutomationDraft;
  setDraft: (next: AutomationDraft | ((current: AutomationDraft) => AutomationDraft)) => void;
  saving: boolean;
  retrying?: boolean;
  workspaces: WorkspaceInfo[];
  models: ModelOption[];
  autoModelId?: string;
  skills: SkillInfo[];
  experts: AgentEntry[];
  connectors: ConnectorOption[];
  records: AutomationRunRecord[];
  createdAt?: string;
  onSave: () => void;
  onClose: () => void;
  onBack?: () => void;
  onTest?: () => void;
  onDelete?: () => void;
  onOpenConnectorSettings?: () => void;
  onArchiveRecord: (id: string) => void;
  onRetryRecord: (automationId: string) => void;
  onDeleteRecord: (id: string) => void;
  onOpenSession?: (sessionId: string) => void;
}) {
  const nameId = useId();
  const promptId = useId();
  const isEditMode = mode === "edit";
  const set = (patch: Partial<AutomationDraft>) =>
    setDraft((current) => ({ ...current, ...patch }));
  const setSchedule = (patch: Partial<AutomationSchedule>) =>
    setDraft((current) => ({
      ...current,
      schedule: { ...current.schedule, ...patch },
    }));

  const scheduleMode: ScheduleMode = scheduleModeOf(draft);
  const periodicMode = inferPeriodicMode(draft.schedule);

  const isRecurringSelectionEmpty =
    scheduleMode === "periodic"
      ? periodicMode === "week" || periodicMode === "biweek"
        ? draft.schedule.byday.length === 0
        : periodicMode === "month"
          ? draft.schedule.bymonthday.length === 0
          : periodicMode === "year"
            ? draft.schedule.bymonth.length === 0 || draft.schedule.bymonthday.length === 0
            : false
      : scheduleMode === "interval"
        ? draft.schedule.byday.length === 0
        : false;

  const handleScheduleModeChange = (next: ScheduleMode) => {
    if (next === "periodic") {
      setDraft({
        ...draft,
        scheduleType: "recurring",
        schedule:
          draft.schedule.freq === "HOURLY"
            ? { ...draft.schedule, freq: "DAILY", interval: 1, byday: [...ALL_DAYS] }
            : draft.schedule,
      });
      return;
    }
    if (next === "interval") {
      const intervalHours = Math.max(1, draft.schedule.intervalHours || 1);
      setDraft({
        ...draft,
        scheduleType: "recurring",
        schedule: {
          ...draft.schedule,
          freq: "HOURLY",
          interval: intervalHours,
          intervalHours,
          byday: draft.schedule.byday.length > 0 ? sortWeekdays(draft.schedule.byday) : [...ALL_DAYS],
        },
      });
      return;
    }
    setDraft({ ...draft, scheduleType: "once", validFromDate: "", validUntilDate: "" });
  };

  const handlePeriodicModeChange = (val: PeriodicMode) => {
    if (val === "day") {
      setSchedule({ freq: "DAILY", interval: 1, byday: [...ALL_DAYS] });
      return;
    }
    if (val === "week" || val === "biweek") {
      setSchedule({ freq: "WEEKLY", interval: val === "biweek" ? 2 : 1, byday: sortWeekdays(draft.schedule.byday) });
      return;
    }
    if (val === "month") {
      setSchedule({ freq: "MONTHLY", interval: 1, bymonthday: sortMonthDays(draft.schedule.bymonthday) });
      return;
    }
    setSchedule({
      freq: "YEARLY",
      interval: 1,
      bymonth: sortMonths(draft.schedule.bymonth),
      bymonthday: sortMonthDays(draft.schedule.bymonthday),
    });
  };

  const handleIntervalWeekdayToggle = (day: WeekdayCode) => {
    const selected = sortWeekdays(draft.schedule.byday);
    const exists = selected.includes(day);
    if (exists && selected.length === 1) return;
    setSchedule({
      byday: exists ? selected.filter((d) => d !== day) : sortWeekdays([...selected, day]),
    });
  };

  const yearlySelectedMonth = draft.schedule.bymonth[0] || 0;
  const yearlySelectedDay = draft.schedule.bymonthday[0] || 0;
  const yearlyMaxDay = getYearlyMaxDay(yearlySelectedMonth || 1);

  const selectedCwd = draft.cwds;

  return (
    <div className="atm-detail-page">
      {/* ===== 头部：面包屑 + 操作(顶部拖拽条) ===== */}
      <div className="atm-detail-header" data-tauri-drag-region>
        <div className="atm-detail-header-left">
          <div className="atm-detail-breadcrumb">
            <span className="atm-detail-status-icon">
              <AlarmClockIcon className="atm-task-status-icon atm-task-status-icon--scheduled" />
            </span>
            <button type="button" className="atm-detail-breadcrumb-link" onClick={onBack ?? onClose} title="返回列表并保留草稿">
              自动化
            </button>
            <span className="atm-detail-breadcrumb-sep">/</span>
            <span className="atm-detail-breadcrumb-current">
              {draft.name || (isEditMode ? "编辑自动化任务" : "添加自动化任务")}
            </span>
          </div>
        </div>
        <div className="atm-detail-header-right">
          <div className="atm-detail-icon-actions">
            {isEditMode && onTest && (
              <button
                type="button"
                className="atm-detail-icon-btn"
                title="保存修改并测试运行"
                aria-label="测试运行"
                disabled={saving}
                onClick={onTest}
              >
                <PlayIcon width={16} height={16} />
              </button>
            )}
            {isEditMode && onDelete && (
              <button
                type="button"
                className="atm-detail-icon-btn"
                title="删除"
                disabled={saving}
                onClick={onDelete}
              >
                <DeleteIcon width={16} height={16} />
              </button>
            )}
          </div>
          <div className="atm-detail-btn-actions">
            <button
              type="button"
              className="atm-detail-btn atm-detail-btn--secondary"
              disabled={saving}
              onClick={onClose}
            >
              取消
            </button>
            <button
              type="button"
              className="atm-detail-btn atm-detail-btn--primary"
              disabled={saving || isRecurringSelectionEmpty}
              onClick={onSave}
            >
              保存
            </button>
          </div>
        </div>
      </div>

      {/* ===== 表单主体 ===== */}
      <div className="atm-detail-content">
        <div className="atm-modal-body">
          {/* 名称 */}
          <label className="atm-modal-label" htmlFor={nameId}>名称</label>
          <input
            id={nameId}
            type="text"
            className="atm-modal-input"
            value={draft.name}
            disabled={saving}
            onChange={(e) => set({ name: e.target.value })}
          />

          {/* 工作空间 */}
          <label className="atm-modal-label" htmlFor="automation-workspace-path">
            工作目录
            <span className="atm-modal-hint atm-modal-hint-inline">(可选)</span>
          </label>
          <WorkspaceInput
            workspaces={workspaces}
            value={selectedCwd}
            disabled={saving}
            onChange={(cwd) => set({ cwds: cwd })}
          />

          {/* 提示词 */}
          <label className="atm-modal-label" htmlFor={promptId}>任务要求</label>
          <div className="atm-modal-chat-input">
            <textarea
              id={promptId}
              className="atm-prompt-textarea"
              value={draft.prompt}
              disabled={saving}
              onChange={(e) => set({ prompt: e.target.value })}
            />
            <div className="atm-prompt-toolbar">
              <ModelChip
                models={models}
                value={draft.modelId}
                autoModelId={autoModelId}
                disabled={saving}
                onChange={(id) => set({ modelId: id })}
              />
              <SkillChip
                skills={skills}
                selected={draft.skills}
                disabled={saving}
                onToggle={(name) =>
                  set({
                    skills: draft.skills.includes(name)
                      ? draft.skills.filter((s) => s !== name)
                      : [...draft.skills, name],
                  })
                }
              />
              <ExpertChip
                experts={experts}
                selectedId={draft.expertId}
                selectedName={draft.expertName}
                selectedPath={draft.expertPath}
                disabled={saving}
                onChange={(id, name, path) => set({ expertId: id, expertName: name, expertPath: path })}
              />
              <AutomationPermissionPicker
                value={draft.permissionMode as AutomationPermissionMode}
                disabled={saving}
                onChange={(permissionMode) => set({ permissionMode })}
              />
            </div>
          </div>

          {/* 连接器 */}
          <ConnectorSelector
            options={connectors}
            selectedIds={draft.connectorIds}
            onChange={(connectorIds) => set({ connectorIds })}
            onManageConnectors={onOpenConnectorSettings}
            disabled={saving}
          />

          {/* 执行频率 */}
          <label className="atm-modal-label">执行频率</label>
          <Segmented
            className="atm-schedule-tabs"
            value={scheduleMode}
            onChange={(v) => handleScheduleModeChange(v as ScheduleMode)}
            options={[
              { value: "periodic", label: "周期" },
              { value: "interval", label: "按间隔" },
              { value: "once", label: "单次" },
            ]}
          />

          {scheduleMode === "periodic" && (
            <div className="atm-schedule-daily">
              <div className="atm-frequency-controls atm-periodic-controls">
                <CustomSelect
                  value={periodicMode}
                  options={PERIODIC_OPTIONS}
                  disabled={saving}
                  onChange={(v) => handlePeriodicModeChange(v as PeriodicMode)}
                />
                {(periodicMode === "week" || periodicMode === "biweek") && (
                  <WeekdayMultiPicker
                    values={draft.schedule.byday}
                    disabled={saving}
                    requireOne
                    onChange={(byday) => setSchedule({ byday })}
                  />
                )}
                {periodicMode === "month" && (
                  <MonthdayMultiPicker
                    values={draft.schedule.bymonthday}
                    disabled={saving}
                    onChange={(bymonthday) => setSchedule({ bymonthday })}
                  />
                )}
                {periodicMode === "year" && (
                  <div className="atm-yearly-selectors">
                    <CustomSelect
                      value={yearlySelectedMonth ? String(yearlySelectedMonth) : ""}
                      placeholder="选择月份"
                      options={MONTHS.map((m) => ({ label: `${m}月`, value: String(m) }))}
                      disabled={saving}
                      onChange={(val) => {
                        const month = Number(val) || 0;
                        const nextMax = getYearlyMaxDay(month || 1);
                        const currentDay = draft.schedule.bymonthday[0] || 0;
                        const normalizedDay = currentDay > nextMax ? nextMax : currentDay;
                        setSchedule({
                          bymonth: month ? [month] : [],
                          bymonthday: normalizedDay ? [normalizedDay] : [],
                        });
                      }}
                    />
                    <CustomSelect
                      value={yearlySelectedDay ? String(yearlySelectedDay) : ""}
                      placeholder="选择日期"
                      options={MONTH_DAYS.filter((d) => d <= yearlyMaxDay).map((d) => ({ label: `${d}日`, value: String(d) }))}
                      disabled={saving}
                      onChange={(val) => {
                        const day = Number(val) || 0;
                        setSchedule({ bymonthday: day ? [day] : [] });
                      }}
                    />
                  </div>
                )}
                <TimePicker
                  value={`${pad2(draft.schedule.byhour)}:${pad2(draft.schedule.byminute)}`}
                  disabled={saving}
                  onChange={(next) => {
                    const { hour, minute } = parseTimeValue(next);
                    setSchedule({ byhour: hour, byminute: minute });
                  }}
                />
              </div>
            </div>
          )}

          {scheduleMode === "interval" && (
            <div className="atm-schedule-interval">
              <span className="atm-schedule-interval-prefix">每</span>
              <input
                type="number"
                className="atm-modal-input atm-schedule-interval-input"
                min={1}
                max={24}
                value={String(draft.schedule.intervalHours)}
                disabled={saving}
                onChange={(e) => {
                  const intervalHours = Math.min(24, Math.max(1, Number(e.target.value) || 1));
                  setSchedule({
                    freq: "HOURLY",
                    interval: intervalHours,
                    intervalHours,
                    byday: draft.schedule.byday.length > 0 ? draft.schedule.byday : [...ALL_DAYS],
                  });
                }}
              />
              <span className="atm-schedule-interval-unit">小时</span>
              <span className="atm-schedule-interval-unit">每天从</span>
              <TimePicker
                value={`${pad2(draft.schedule.byhour)}:${pad2(draft.schedule.byminute)}`}
                disabled={saving}
                onChange={(next) => {
                  const { hour, minute } = parseTimeValue(next);
                  setSchedule({ byhour: hour, byminute: minute });
                }}
              />
              <span className="atm-schedule-interval-unit">起</span>
              <IntervalDayChips
                values={draft.schedule.byday}
                disabled={saving}
                onToggle={handleIntervalWeekdayToggle}
              />
            </div>
          )}

          {scheduleMode === "once" && (
            <div className="atm-schedule-once">
              <div className="atm-frequency-controls">
                <TimePicker
                  value={draft.scheduledTime ?? "09:00"}
                  disabled={saving}
                  onChange={(next) => set({ scheduledTime: next || "09:00" })}
                />
                <SingleDatePicker
                  value={draft.scheduledDate}
                  disabled={saving}
                  onChange={(scheduledDate) => set({ scheduledDate })}
                />
              </div>
            </div>
          )}

          {/* 生效日期区间 */}
          {scheduleMode !== "once" && (
            <>
              <label className="atm-modal-label">
                生效日期区间
                <span className="atm-modal-hint atm-modal-hint-inline">（可选，留空则不限制生效日期。）</span>
              </label>
              <ValidityRangePicker
                startDate={draft.validFromDate}
                endDate={draft.validUntilDate}
                disabled={saving}
                onChange={(validFromDate, validUntilDate) => set({ validFromDate, validUntilDate })}
              />
            </>
          )}

          {/* 推送到已配置通知渠道 */}
          <div className="atm-schedule-push-toggle">
            <div className="atm-push-toggle-left">
              <span className="atm-toggle-text">完成后推送到通知渠道</span>
              <span
                className="atm-push-info-icon"
                title="开启后，按桌面提醒设置发送系统通知，并同步到「设置 → 通知」中已启用的外部推送渠道。"
              >
                <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" xmlns="http://www.w3.org/2000/svg">
                  <path
                    opacity="0.3"
                    transform="translate(0.667, 0.667)"
                    d="M7.3333 13.3333C10.647 13.3333 13.3333 10.647 13.3333 7.3333C13.3333 4.0196 10.647 1.3333 7.3333 1.3333C4.0196 1.3333 1.3333 4.0196 1.3333 7.3333C1.3333 10.647 4.0196 13.3333 7.3333 13.3333ZM14.6667 7.3333C14.6667 11.3834 11.3834 14.6667 7.3333 14.6667C3.2832 14.6667 0 11.3834 0 7.3333C0 3.2832 3.2832 0 7.3333 0C11.3834 0 14.6667 3.2832 14.6667 7.3333ZM6.6667 11L6.6667 6L8 6L8 11L6.6667 11ZM8 5L6.6641 5L6.6641 3.6641L8 3.6641L8 5Z"
                  />
                </svg>
              </span>
            </div>
            <Switch
              className="atm-push-switch"
              checked={draft.pushToWeChat ?? false}
              disabled={saving}
              onChange={(pushToWeChat) => set({ pushToWeChat })}
            />
          </div>

          {/* 创建时间（编辑态） */}
          {isEditMode && createdAt && (
            <div className="atm-detail-created-at">
              创建时间{" "}
              {new Date(createdAt).toLocaleDateString("zh-CN", {
                year: "numeric",
                month: "numeric",
                day: "numeric",
              })}
            </div>
          )}
        </div>

        {/* 运行历史（编辑态） */}
        {isEditMode && (
          <RunHistory
            records={records}
            retrying={retrying}
            onRetry={onRetryRecord}
            onArchive={onArchiveRecord}
            onDelete={onDeleteRecord}
            onOpenSession={onOpenSession}
          />
        )}
      </div>
    </div>
  );
}
