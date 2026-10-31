/**
 * schedule-utils — 自动化调度模型的构建 / 描述 / 校验。
 *
 * 与 EchoAgent automation-panel/index.tsx 中的逻辑一一对应：
 *  - scheduleMode: periodic(周期) | interval(按间隔) | once(单次)
 *  - periodicMode: day(每天) | week(每周) | biweek(双周) | month(每月) | year(每年)
 *  - 文案取自 EchoAgent zh-cn（automation.schedule.* / automation.validity.*）
 */
import type {
  Automation,
  AutomationSchedule,
} from "@/lib/types";
import { ALL_DAYS, type AutomationTemplate, type WeekdayCode } from "./template-config";

export { ALL_DAYS };
export type { WeekdayCode };

export const DAY_LABELS: Record<WeekdayCode, string> = {
  MO: "周一",
  TU: "周二",
  WE: "周三",
  TH: "周四",
  FR: "周五",
  SA: "周六",
  SU: "周日",
};

export const MONTHS = Array.from({ length: 12 }, (_, i) => i + 1);
export const MONTH_DAYS = Array.from({ length: 31 }, (_, i) => i + 1);

export type ScheduleMode = "periodic" | "interval" | "once";
export type PeriodicMode = "day" | "week" | "biweek" | "month" | "year";

/** 表单草稿：保存前与 Automation 同构（id 为空表示新建）。 */
export type AutomationDraft = Omit<Automation, "createdAt" | "nextRunAt" | "lastRunAt">;

export function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

export function formatDateInputValue(d: Date): string {
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

export function todayDateInput(): string {
  return formatDateInputValue(new Date());
}

export function parseTimeValue(value: string): { hour: number; minute: number } {
  const [h, m] = value.split(":");
  return { hour: Number(h) || 0, minute: Number(m) || 0 };
}

export function sortWeekdays(days: string[]): WeekdayCode[] {
  return [...new Set(days.filter((d): d is WeekdayCode => (ALL_DAYS as readonly string[]).includes(d)))].sort(
    (a, b) => ALL_DAYS.indexOf(a) - ALL_DAYS.indexOf(b),
  );
}

export function sortMonthDays(days: number[]): number[] {
  return [...new Set(days)].filter((d) => d >= 1 && d <= 31).sort((a, b) => a - b);
}

export function sortMonths(months: number[]): number[] {
  return [...new Set(months)].filter((m) => m >= 1 && m <= 12).sort((a, b) => a - b);
}

export function getYearlyMaxDay(month: number): number {
  if ([1, 3, 5, 7, 8, 10, 12].includes(month)) return 31;
  if ([4, 6, 9, 11].includes(month)) return 30;
  return 29;
}

export function scheduleModeOf(draft: Pick<AutomationDraft, "scheduleType" | "schedule">): ScheduleMode {
  if (draft.scheduleType === "once") return "once";
  return draft.schedule.freq === "HOURLY" ? "interval" : "periodic";
}

export function inferPeriodicMode(schedule: AutomationSchedule): PeriodicMode {
  switch (schedule.freq) {
    case "WEEKLY":
      return schedule.interval === 2 ? "biweek" : "week";
    case "MONTHLY":
      return "month";
    case "YEARLY":
      return "year";
    default:
      return "day";
  }
}

export function defaultSchedule(): AutomationSchedule {
  return {
    freq: "DAILY",
    interval: 1,
    byday: [...ALL_DAYS],
    bymonthday: [],
    bymonth: [],
    byhour: 9,
    byminute: 0,
    intervalHours: 1,
  };
}

export function buildDraft(template?: AutomationTemplate, initialCwd = ""): AutomationDraft {
  return {
    id: "",
    name: template?.title ?? "",
    prompt: template?.prompt ?? "",
    cwds: initialCwd,
    status: "ACTIVE",
    modelId: undefined,
    modelIsThinking: false,
    skills: [],
    expertId: undefined,
    expertName: undefined,
    expertPath: undefined,
    connectorIds: [],
    permissionMode: "default",
    scheduleType: template?.scheduleType ?? "recurring",
    schedule: template ? { ...template.schedule, byday: [...template.schedule.byday] } : defaultSchedule(),
    scheduledDate: template?.scheduledDate ?? todayDateInput(),
    scheduledTime: template?.scheduledTime ?? "09:00",
    validFromDate: template?.validFromDate ?? "",
    validUntilDate: template?.validUntilDate ?? "",
    pushToWeChat: template?.pushToChannels ?? false,
    newSessionEachRun: true,
  };
}

export function draftFromAutomation(a: Automation): AutomationDraft {
  return {
    id: a.id,
    name: a.name,
    prompt: a.prompt,
    cwds: a.cwds,
    status: a.status,
    modelId: a.modelId,
    modelIsThinking: a.modelIsThinking ?? false,
    skills: [...(a.skills ?? [])],
    expertId: a.expertId,
    expertName: a.expertName,
    expertPath: a.expertPath,
    connectorIds: [...(a.connectorIds ?? [])],
    permissionMode: a.permissionMode ?? "default",
    scheduleType: a.scheduleType ?? "recurring",
    schedule: {
      ...a.schedule,
      byday: [...a.schedule.byday],
      bymonthday: [...a.schedule.bymonthday],
      bymonth: [...a.schedule.bymonth],
    },
    scheduledDate: a.scheduledDate ?? todayDateInput(),
    scheduledTime: a.scheduledTime ?? "09:00",
    validFromDate: a.validFromDate ?? "",
    validUntilDate: a.validUntilDate ?? "",
    pushToWeChat: a.pushToWeChat ?? false,
    newSessionEachRun: a.newSessionEachRun ?? true,
  };
}

export function automationFromDraft(draft: AutomationDraft, existing?: Automation): Automation {
  return {
    ...draft,
    createdAt: existing?.createdAt ?? "",
    nextRunAt: existing?.nextRunAt,
    lastRunAt: existing?.lastRunAt,
  };
}

// ---------- 描述 ----------

function timeLabel(schedule: AutomationSchedule): string {
  return `${pad2(schedule.byhour)}:${pad2(schedule.byminute)}`;
}

function isWorkdays(byday: string[]): boolean {
  return (
    byday.length === 5 &&
    ["MO", "TU", "WE", "TH", "FR"].every((d) => byday.includes(d))
  );
}

function dayListLabel(byday: string[]): string {
  return sortWeekdays(byday).map((d) => DAY_LABELS[d]).join("/");
}

/** 列表行上的调度摘要（对齐 automation.schedule.* 文案）。 */
export function describeSchedule(a: Pick<Automation, "scheduleType" | "schedule" | "scheduledDate" | "scheduledTime">): string {
  if (a.scheduleType === "once") {
    const date = a.scheduledDate ? a.scheduledDate.slice(5).replace("-", "/") : "";
    return `单次 · ${date} ${a.scheduledTime ?? "09:00"}`;
  }
  const s = a.schedule;
  const t = timeLabel(s);
  switch (s.freq) {
    case "DAILY":
      return `每天 ${t}`;
    case "WEEKLY": {
      if (isWorkdays(s.byday)) return `每个工作日 ${t}`;
      const days = dayListLabel(s.byday);
      return s.interval === 2 ? `双周 ${days} · ${t}` : `每周 ${days} · ${t}`;
    }
    case "MONTHLY":
      return `每月 ${sortMonthDays(s.bymonthday).join("/")} 日 · ${t}`;
    case "YEARLY":
      return `每年 ${s.bymonth[0] ?? 1}月${s.bymonthday[0] ?? 1}日 · ${t}`;
    case "HOURLY":
      return `每 ${Math.max(1, s.intervalHours)} 小时（每天从 ${t} 起）`;
  }
}

/** 生效日期区间摘要（automation.validity.range/from/until）。 */
export function describeValidity(a: Pick<Automation, "validFromDate" | "validUntilDate">): string | null {
  const from = a.validFromDate;
  const until = a.validUntilDate;
  if (from && until) return `生效期 ${from} - ${until}`;
  if (from) return `自 ${from} 生效`;
  if (until) return `至 ${until} 截止`;
  return null;
}

/** 「x天后执行 / x小时后执行 / x分钟后执行 / 即将执行」（automation.row.startsIn）。 */
export function startsInLabel(nextRunAt?: string): string | null {
  if (!nextRunAt) return null;
  const ms = Date.parse(nextRunAt) - Date.now();
  if (!Number.isFinite(ms) || ms <= 0) return null;
  if (ms < 60_000) return "即将执行";
  if (ms < 3_600_000) return `${Math.ceil(ms / 60_000)}分钟后执行`;
  if (ms < 86_400_000) return `${Math.ceil(ms / 3_600_000)}小时后执行`;
  return `${Math.ceil(ms / 86_400_000)}天后执行`;
}

/** 运行记录时间戳 → "MM-DD HH:MM"。 */
export function formatRunTime(iso?: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return `${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

// ---------- 校验（automation.schedule.validation.* / automation.error.*） ----------

export interface ValidateOptions {
  isCreating: boolean;
  /** 编辑态：已有的单次触发时间（未改动时跳过"必须晚于当前时间"）。 */
  existingScheduledAt?: string;
}

export function scheduledAtIso(draft: Pick<AutomationDraft, "scheduledDate" | "scheduledTime">): string | undefined {
  if (!draft.scheduledDate) return undefined;
  const candidate = new Date(`${draft.scheduledDate}T${draft.scheduledTime || "09:00"}:00`);
  return Number.isNaN(candidate.getTime()) ? undefined : candidate.toISOString();
}

export function validateDraft(draft: AutomationDraft, opts: ValidateOptions): string | null {
  if ((draft.expertId || draft.expertName || draft.expertPath) && (!draft.expertPath || !(draft.expertName || draft.expertId))) {
    return "专家引用缺少稳定定义路径，请重新选择专家后保存";
  }
  if (!draft.name.trim()) return "请填写自动化任务名称";
  if (!draft.prompt.trim()) return "请填写提示词";
  if (draft.schedule.byhour < 0 || draft.schedule.byhour > 23 || draft.schedule.byminute < 0 || draft.schedule.byminute > 59) {
    return "请选择有效的执行时间";
  }

  const mode = scheduleModeOf(draft);
  if (mode === "periodic") {
    const periodic = inferPeriodicMode(draft.schedule);
    if ((periodic === "week" || periodic === "biweek") && draft.schedule.byday.length === 0) {
      return "请至少选择一个星期";
    }
    if ((periodic === "week" || periodic === "biweek") && ![1, 2].includes(draft.schedule.interval)) {
      return "每周执行间隔只能是 1 或 2 周";
    }
    if (periodic === "month" && draft.schedule.bymonthday.length === 0) {
      return "请至少选择一个日期";
    }
    if (periodic === "year") {
      if (draft.schedule.bymonth.length === 0) return "请选择月份";
      if (draft.schedule.bymonthday.length === 0) return "请选择日期";
      const month = draft.schedule.bymonth[0];
      const day = draft.schedule.bymonthday[0];
      const candidate = new Date(2000, month - 1, day);
      if (candidate.getMonth() !== month - 1 || candidate.getDate() !== day) return "请选择有效的年度执行日期";
    }
  }
  if (mode === "interval") {
    if (draft.schedule.intervalHours < 1 || draft.schedule.intervalHours > 24) {
      return "按间隔执行需设置为 1 至 24 小时";
    }
    if (draft.schedule.byday.length === 0) return "请至少选择一个星期";
  }
  if (mode === "once") {
    if (!draft.scheduledDate) return "请选择单次执行日期";
    const next = scheduledAtIso(draft);
    if (!next) return "请选择单次执行日期";
    const changed = opts.existingScheduledAt !== next;
    if ((opts.isCreating || changed) && Date.parse(next) <= Date.now()) {
      return "单次执行时间必须晚于当前时间";
    }
  }
  if (mode !== "once") {
    const start = draft.validFromDate ? new Date(`${draft.validFromDate}T00:00:00`) : undefined;
    const end = draft.validUntilDate ? new Date(`${draft.validUntilDate}T23:59:59`) : undefined;
    if (start && Number.isNaN(start.getTime())) return "生效开始日期无效";
    if (end && Number.isNaN(end.getTime())) return "生效结束日期无效";
    if (start && end && start > end) return "生效开始日期不能晚于结束日期";
    if (draft.status === "ACTIVE" && end && end.getTime() < Date.now()) {
      return "生效日期区间已过期，请更新结束日期";
    }
  }
  return null;
}
