import {
  modelDisplayName,
  type PresentableModel,
} from "@/lib/model-presentation";
/** Shared scheduled-task form. Basic scheduling stays visible; execution
 * options live in advanced settings. The parent owns persistence and drafts. */
import { useEffect, useId, useRef, useState } from "react";
import {
  AlarmClockIcon,
  CheckIcon,
  ChevronDownIcon,
  ExpertIcon,
  FolderOpenIcon,
  SkillIcon,
} from "@/foundation/components/Icon/icons";
import type {
  AutomationPermissionMode,
  AutomationRunRecord,
  AutomationSchedule,
} from "@/lib/types";
import type { AgentEntry, SkillInfo } from "@/lib/types";
import { ModelSelector } from "@/components/ModelSelector";
import { localTimeZoneLabel, workspaceName } from "./task-presentation";
import {
  loadDesktopNotifyStatus,
  loadNotifyChannels,
} from "@/lib/notify-channels";
import {
  filesystemPickDirectory,
  type WorkspaceInfo,
} from "@/lib/agent-client";
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
    if (disabled) {
      setIsOpen(false);
      return;
    }
    // WebKit mouse clicks can blur the focused option with relatedTarget=null
    // before click. Only an actual outside pointer/focus target closes the menu.
    const handler = (e: MouseEvent | FocusEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      )
        setIsOpen(false);
    };
    const keyHandler = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setIsOpen(false);
      triggerRef.current?.focus();
    };
    containerRef.current
      ?.querySelector<HTMLButtonElement>(
        ".atm-chip-dropdown__menu button:not(:disabled)",
      )
      ?.focus();
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
    <div
      className={`atm-chip-dropdown${isOpen ? " atm-chip-dropdown--open" : ""}`}
      ref={containerRef}
    >
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
          data-modal-transient
          className="atm-chip-dropdown__menu"
          onClick={(event) => {
            if (
              closeOnSelect &&
              (event.target as HTMLElement).closest("button")
            ) {
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
  return (
    <div className="atm-model-field">
      <button
        type="button"
        className="atm-toolbar-btn"
        aria-pressed={!value}
        disabled={disabled}
        onClick={() => onChange(undefined)}
      >
        跟随默认模型
      </button>
      <ModelSelector
        ariaLabel="选择定时任务模型"
        modelId={value || autoModelId}
        models={models}
        disabled={disabled}
        onModelChange={onChange}
        menuPlacement="bottom"
        menuAlign="start"
        menuZIndex={1400}
      />
      {!value && <small>每次运行使用当前默认模型</small>}
    </div>
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
      {skills.length === 0 && (
        <div className="atm-chip-empty">暂无可用技能</div>
      )}
      {skills.map((s) => (
        <button
          key={s.name}
          type="button"
          className={`atm-chip-option${selected.includes(s.name) ? " active" : ""}`}
          onClick={() => onToggle(s.name)}
        >
          <span className="atm-chip-option-check">
            {selected.includes(s.name) && <CheckIcon size="sm" />}
          </span>
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
          <span className="atm-prompt-chip-label">
            {selectedId
              ? `${selectedName || selectedId}${selectedPath ? "" : " · 需重新选择"}`
              : "召唤专家"}
          </span>
          <ChevronDownIcon size="sm" className="atm-prompt-chip-caret" />
        </>
      }
    >
      <button
        type="button"
        className={`atm-chip-option${!selectedId ? " active" : ""}`}
        onClick={() => onChange(undefined, undefined, undefined)}
      >
        <span className="atm-chip-option-check">
          {!selectedId && <CheckIcon size="sm" />}
        </span>
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
          <span className="atm-chip-option-check">
            {selectedId === e.name && selectedPath === e.path && (
              <CheckIcon size="sm" />
            )}
          </span>
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
  onCommit,
  disabled,
}: {
  workspaces: WorkspaceInfo[];
  value: string;
  onChange: (cwd: string) => void;
  onCommit?: (cwd: string) => void;
  disabled?: boolean;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const [isPicking, setIsPicking] = useState(false);
  const [pickError, setPickError] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);
  const matches = workspaces
    .filter(
      (workspace) =>
        workspace.cwd !== value &&
        workspace.cwd
          .toLocaleLowerCase()
          .includes(value.trim().toLocaleLowerCase()),
    )
    .slice(0, 8);

  useEffect(() => {
    if (!isOpen) return;
    const handler = (e: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      )
        setIsOpen(false);
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
      if (selected) {
        onChange(selected);
        onCommit?.(selected);
      }
    } catch (error) {
      setPickError(
        `选择文件夹失败：${String(error).replace(/^Error:\s*/, "")}`,
      );
    } finally {
      setIsPicking(false);
    }
  };

  return (
    <div
      className="atm-workspace-input"
      ref={containerRef}
      onBlur={(event) => {
        if (
          event.relatedTarget &&
          !containerRef.current?.contains(event.relatedTarget as Node)
        )
          setIsOpen(false);
      }}
    >
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
          onBlur={() => onCommit?.(value)}
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
                onCommit?.(w.cwd);
                setIsOpen(false);
              }}
            >
              <span className="atm-workspace-input__option-path">{w.cwd}</span>
              {w.lastTitle && (
                <span className="atm-workspace-input__option-title">
                  {w.lastTitle}
                </span>
              )}
            </button>
          ))}
        </div>
      )}
      <div className="atm-workspace-input__hint">
        新目录请点击「选择文件夹」确认授权；留空则使用默认工作目录。
      </div>
      {pickError && (
        <div className="atm-workspace-input__error" role="alert">
          {pickError}
        </div>
      )}
    </div>
  );
}

// ============================================================
// 运行历史（编辑态）
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
  workspaces,
  models,
  autoModelId,
  skills,
  experts,
  connectors,
  onSave,
  onClose,
  onBack,
  onTest,
  onDelete,
  onOpenConnectorSettings,
  onOpenNotificationSettings,
  onWorkspaceCommit,
  validationError,
}: {
  mode: "create" | "edit";
  draft: AutomationDraft;
  setDraft: (
    next: AutomationDraft | ((current: AutomationDraft) => AutomationDraft),
  ) => void;
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
  onOpenNotificationSettings?: () => void;
  onWorkspaceCommit?: (cwd: string) => void;
  validationError?: string | null;
  onArchiveRecord: (id: string) => void;
  onRetryRecord: (automationId: string) => void;
  onDeleteRecord: (id: string) => void;
  onOpenSession?: (sessionId: string) => void;
}) {
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [notificationRevision, setNotificationRevision] = useState(0);
  useEffect(() => {
    const update = () => setNotificationRevision((value) => value + 1);
    window.addEventListener("echo:notification-settings-changed", update);
    return () =>
      window.removeEventListener("echo:notification-settings-changed", update);
  }, []);
  const [notificationSummary, setNotificationSummary] =
    useState("正在读取通知设置…");
  useEffect(() => {
    let active = true;
    void Promise.all([loadDesktopNotifyStatus(), loadNotifyChannels()])
      .then(([desktop, channels]) => {
        if (!active) return;
        const names = channels
          .filter((channel) => channel.enabled && channel.kind !== "desktop")
          .map((channel) => channel.label);
        setNotificationSummary(
          `${desktop.enabled ? "桌面提醒已开启（系统权限以实际投递为准）" : "桌面提醒已关闭"}；${names.length ? `外部渠道：${names.join("、")}` : "未启用外部通知渠道"}。`,
        );
      })
      .catch(() => {
        if (active)
          setNotificationSummary("通知设置暂时无法读取，请检查通知设置。");
      });
    return () => {
      active = false;
    };
  }, [notificationRevision]);
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
            ? draft.schedule.bymonth.length === 0 ||
              draft.schedule.bymonthday.length === 0
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
            ? {
                ...draft.schedule,
                freq: "DAILY",
                interval: 1,
                byday: [...ALL_DAYS],
              }
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
          byday:
            draft.schedule.byday.length > 0
              ? sortWeekdays(draft.schedule.byday)
              : [...ALL_DAYS],
        },
      });
      return;
    }
    setDraft({
      ...draft,
      scheduleType: "once",
      validFromDate: "",
      validUntilDate: "",
    });
  };

  const handlePeriodicModeChange = (val: PeriodicMode) => {
    if (val === "day") {
      setSchedule({ freq: "DAILY", interval: 1, byday: [...ALL_DAYS] });
      return;
    }
    if (val === "week" || val === "biweek") {
      setSchedule({
        freq: "WEEKLY",
        interval: val === "biweek" ? 2 : 1,
        byday: sortWeekdays(draft.schedule.byday),
      });
      return;
    }
    if (val === "month") {
      setSchedule({
        freq: "MONTHLY",
        interval: 1,
        bymonthday: sortMonthDays(draft.schedule.bymonthday),
      });
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
      byday: exists
        ? selected.filter((d) => d !== day)
        : sortWeekdays([...selected, day]),
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
            <button
              type="button"
              className="atm-detail-breadcrumb-link"
              onClick={onBack ?? onClose}
              title="返回列表并保留草稿"
            >
              返回列表
            </button>
            <span className="atm-detail-breadcrumb-sep">/</span>
            <span className="atm-detail-breadcrumb-current">
              {draft.name || (isEditMode ? "编辑定时任务" : "新建定时任务")}
            </span>
          </div>
        </div>
        <div className="atm-detail-header-right">
          <h2>{isEditMode ? "编辑定时任务" : "新建定时任务"}</h2>
        </div>
      </div>

      {/* ===== 表单主体 ===== */}
      <div className="atm-detail-content">
        <div className="atm-modal-body">
          {validationError && (
            <p className="atm-editor-error" role="alert">
              {validationError}
            </p>
          )}
          {/* 名称 */}
          <label className="atm-modal-label" htmlFor={nameId}>
            名称
          </label>
          <input
            id={nameId}
            data-modal-initial-focus
            type="text"
            className="atm-modal-input"
            value={draft.name}
            disabled={saving}
            onChange={(e) => set({ name: e.target.value })}
          />

          {/* 提示词 */}
          <label className="atm-modal-label" htmlFor={promptId}>
            任务要求
          </label>
          <div className="atm-modal-chat-input">
            <textarea
              id={promptId}
              className="atm-prompt-textarea"
              value={draft.prompt}
              disabled={saving}
              onChange={(e) => set({ prompt: e.target.value })}
            />
          </div>

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
                      value={
                        yearlySelectedMonth ? String(yearlySelectedMonth) : ""
                      }
                      placeholder="选择月份"
                      options={MONTHS.map((m) => ({
                        label: `${m}月`,
                        value: String(m),
                      }))}
                      disabled={saving}
                      onChange={(val) => {
                        const month = Number(val) || 0;
                        const nextMax = getYearlyMaxDay(month || 1);
                        const currentDay = draft.schedule.bymonthday[0] || 0;
                        const normalizedDay =
                          currentDay > nextMax ? nextMax : currentDay;
                        setSchedule({
                          bymonth: month ? [month] : [],
                          bymonthday: normalizedDay ? [normalizedDay] : [],
                        });
                      }}
                    />
                    <CustomSelect
                      value={yearlySelectedDay ? String(yearlySelectedDay) : ""}
                      placeholder="选择日期"
                      options={MONTH_DAYS.filter((d) => d <= yearlyMaxDay).map(
                        (d) => ({ label: `${d}日`, value: String(d) }),
                      )}
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
                  const intervalHours = Math.min(
                    24,
                    Math.max(1, Number(e.target.value) || 1),
                  );
                  setSchedule({
                    freq: "HOURLY",
                    interval: intervalHours,
                    intervalHours,
                    byday:
                      draft.schedule.byday.length > 0
                        ? draft.schedule.byday
                        : [...ALL_DAYS],
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

          <p className="atm-form-help">
            按{localTimeZoneLabel()}执行。电脑需保持开机且 EchoAgent 持续运行。
          </p>
          <div className="atm-execution-summary">
            <span title={draft.cwds}>
              工作目录：{workspaceName(draft.cwds)}
            </span>
            <span>
              模型：
              {draft.modelId
                ? modelDisplayName(
                    models.find((model) => model.id === draft.modelId) ||
                      draft.modelId,
                  )
                : "跟随默认模型"}
            </span>
            <span>
              {draft.permissionMode === "fullAccess" ? "完全访问" : "默认权限"}
            </span>
          </div>
          <button
            type="button"
            className="atm-advanced-trigger"
            aria-expanded={advancedOpen}
            aria-controls={`${nameId}-advanced`}
            onClick={() => setAdvancedOpen((value) => !value)}
          >
            高级设置
            <ChevronDownIcon width={15} height={15} />
          </button>
          {advancedOpen && (
            <div id={`${nameId}-advanced`} className="atm-advanced-settings">
              <div className="atm-option-row">
                <div>
                  <strong>每次运行都开启新会话</strong>
                  <p>
                    {draft.newSessionEachRun !== false
                      ? "每次独立执行，不沿用之前的对话。工作目录中的文件与共享记忆仍可能被读取。"
                      : "后续运行沿用此任务的专属会话，可参考之前的对话内容。每次结果仍单独保留。"}
                  </p>
                </div>
                <Switch
                  ariaLabel="每次运行都开启新会话"
                  checked={draft.newSessionEachRun !== false}
                  disabled={saving}
                  onChange={(newSessionEachRun) => set({ newSessionEachRun })}
                />
              </div>
              {draft.newSessionEachRun === false && (
                <p className="atm-form-help">
                  切换模式从下一次运行生效；改变工作目录、权限或专家后会开始新的专属会话，历史结果保留。
                </p>
              )}
              {/* 工作空间 */}
              <label
                className="atm-modal-label"
                htmlFor="automation-workspace-path"
              >
                工作目录
                <span className="atm-modal-hint atm-modal-hint-inline">
                  (可选)
                </span>
              </label>
              <WorkspaceInput
                workspaces={workspaces}
                value={selectedCwd}
                onCommit={onWorkspaceCommit}
                disabled={saving}
                onChange={(cwd) => set({ cwds: cwd })}
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
                  onChange={(id, name, path) =>
                    set({ expertId: id, expertName: name, expertPath: path })
                  }
                />
                <AutomationPermissionPicker
                  value={draft.permissionMode as AutomationPermissionMode}
                  disabled={saving}
                  onChange={(permissionMode) => set({ permissionMode })}
                />
              </div>

              {/* 连接器 */}
              <ConnectorSelector
                options={connectors}
                selectedIds={draft.connectorIds}
                onChange={(connectorIds) => set({ connectorIds })}
                onManageConnectors={onOpenConnectorSettings}
                disabled={saving}
              />

              <p className="atm-form-help">
                默认权限下，敏感操作可能暂停并等待确认。
              </p>
              {/* 生效日期区间 */}
              {scheduleMode !== "once" && (
                <>
                  <label className="atm-modal-label">
                    生效日期区间
                    <span className="atm-modal-hint atm-modal-hint-inline">
                      （可选，留空则不限制生效日期。）
                    </span>
                  </label>
                  <ValidityRangePicker
                    startDate={draft.validFromDate}
                    endDate={draft.validUntilDate}
                    disabled={saving}
                    onChange={(validFromDate, validUntilDate) =>
                      set({ validFromDate, validUntilDate })
                    }
                  />
                </>
              )}

              <div className="atm-option-row">
                <div>
                  <strong>完成后通知我</strong>
                  <p>
                    每次执行单独记录结果。开启后按通知设置发送完成、失败或停止提醒。
                  </p>
                  <p>{notificationSummary}</p>
                  {onOpenNotificationSettings && (
                    <button
                      type="button"
                      className="atm-text-button"
                      onClick={onOpenNotificationSettings}
                    >
                      通知设置
                    </button>
                  )}
                </div>
                <Switch
                  ariaLabel="完成后通知我"
                  checked={draft.pushToWeChat ?? false}
                  disabled={saving}
                  onChange={(pushToWeChat) => set({ pushToWeChat })}
                />
              </div>
              {isEditMode && onDelete && (
                <button
                  type="button"
                  className="atm-text-button atm-editor-delete"
                  disabled={saving}
                  onClick={onDelete}
                >
                  删除
                </button>
              )}
            </div>
          )}
        </div>
      </div>
      <div className="atm-editor-footer">
        <p>保存后按计划执行。“立即运行”会真实执行一次。</p>
        <div>
          <button
            type="button"
            className="atm-detail-btn atm-detail-btn--secondary"
            disabled={saving}
            onClick={onClose}
          >
            取消
          </button>
          {isEditMode && onTest && (
            <button
              type="button"
              className="atm-detail-btn atm-detail-btn--secondary"
              disabled={saving || isRecurringSelectionEmpty}
              onClick={onTest}
            >
              保存并立即运行
            </button>
          )}
          <button
            type="button"
            className="atm-detail-btn atm-detail-btn--primary"
            disabled={saving || isRecurringSelectionEmpty}
            onClick={onSave}
          >
            {saving ? "保存中…" : isEditMode ? "保存修改" : "创建任务"}
          </button>
        </div>
      </div>
    </div>
  );
}
