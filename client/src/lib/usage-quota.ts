/**
 * Device-local token accounting and quota enforcement for BYOK providers.
 *
 * The authoritative input is EchoAgent's durable TurnCompleted usage ledger.
 * Records are keyed by session + prompt, so live notifications and history
 * replay are idempotent. This is deliberately described as local usage: it is
 * not a provider invoice and does not include calls made outside this app.
 */

import type { SessionUsage, SessionUsageModel } from "./types";
import type { TurnOrigin } from "./execution-turn";

export interface UsageModelBreakdown {
  promptTokens: number;
  completionTokens: number;
  cachedReadTokens: number;
  cacheCreationTokens: number;
  reasoningTokens: number;
  modelCalls: number;
  apiDurationMs: number;
  /** Trusted provider-reported cost. Undefined means unknown, not free. */
  providerCost?: number;
}

/** 一条用量记录。 */
export interface UsageRecord {
  /** Stable replay-safe key (`sessionId:promptId`) for modern records. */
  id?: string;
  /** ISO 日期(YYYY-MM-DD)。 */
  date: string;
  /** 模型 id。 */
  modelId: string;
  /** 提示 token。 */
  promptTokens: number;
  /** 补全 token。 */
  completionTokens: number;
  /** 估算费用(可选,用户配置费率后计算)。 */
  cost?: number;
  /** 时间戳(ms)。 */
  ts: number;
  /** 会话 id；新记录用于去重与追踪，旧数据可能没有。 */
  sessionId?: string;
  /** Prompt id emitted by the durable TurnCompleted event. */
  promptId?: string;
  eventId?: string;
  projectId?: string;
  turnOrigin?: TurnOrigin;
  turnSourceId?: string;
  /** Actual model requests folded into this prompt. */
  modelCalls?: number;
  /** Main-agent loop rounds; distinct from provider model requests. */
  agentTurns?: number;
  cachedReadTokens?: number;
  cacheCreationTokens?: number;
  reasoningTokens?: number;
  apiDurationMs?: number;
  /** Trusted provider-reported total cost. */
  providerCost?: number;
  /** Per-model exact split when supplied by the runtime ledger. */
  modelUsage?: Record<string, UsageModelBreakdown>;
  /** The runtime warned that folded/subagent usage may be incomplete. */
  incomplete?: boolean;
  source?: "turn" | "cumulative" | "legacy";
}

/** 用量统计汇总。 */
export interface UsageSummary {
  /** 总 token。 */
  totalTokens: number;
  /** 总提示 token。 */
  totalPrompt: number;
  /** 总补全 token。 */
  totalCompletion: number;
  /** 总估算费用。 */
  totalCost: number;
  /** Actual provider model-call count (legacy records conservatively count as one). */
  count: number;
  /** User prompt/turn records in the selected period. */
  requestCount: number;
  modelCalls: number;
  cachedReadTokens: number;
  cacheCreationTokens: number;
  reasoningTokens: number;
  apiDurationMs: number;
  incompleteCount: number;
  /** 按模型分组。 */
  byModel: Record<string, {
    tokens: number;
    promptTokens: number;
    completionTokens: number;
    cachedReadTokens: number;
    reasoningTokens: number;
    cost: number;
    count: number;
  }>;
  /** 按日期分组(YYYY-MM-DD)。 */
  byDate: Record<string, { tokens: number; cost: number; count: number; requests: number }>;
  byProject: Record<string, { tokens: number; cost: number; count: number; requests: number }>;
}

/** 配额配置。 */
export interface QuotaConfig {
  /** 配额周期("daily" | "monthly")。 */
  period: "daily" | "monthly";
  /** token 上限。 */
  tokenLimit: number;
  /** 达到上限后的桌面端手动发送策略；旧配置默认仅提醒。 */
  enforcement?: "warn" | "block";
  /** 费率表(modelId → 每千 token 价格)。 */
  rates?: Record<string, { prompt: number; completion: number }>;
}

const STORAGE_KEY = "echoagent.usage";
const QUOTA_KEY = "echoagent.quota";
const SNAPSHOT_KEY = "echoagent.usage-snapshots.v1";
const QUOTA_ALERT_KEY = "echoagent.quota-alert.v1";
export const USAGE_CHANGED_EVENT = "echoagent:usage-changed";
export const USD_TICKS_PER_USD = 10_000_000_000;

interface CumulativeSnapshot {
  inputTokens: number;
  outputTokens: number;
}

type CumulativeSnapshots = Record<string, CumulativeSnapshot>;

function localDateParts(date: Date): { year: number; month: number; day: number } {
  return {
    year: date.getFullYear(),
    month: date.getMonth() + 1,
    day: date.getDate(),
  };
}

/** 取本机时区的今日日期，避免东八区凌晨被记到前一天。 */
export function todayKey(date = new Date()): string {
  const { year, month, day } = localDateParts(date);
  return `${year}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
}

/** 取本机时区的当月(YYYY-MM)。 */
export function monthKey(date = new Date()): string {
  const { year, month } = localDateParts(date);
  return `${year}-${String(month).padStart(2, "0")}`;
}

function nonNegative(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? Math.floor(value)
    : 0;
}

function trustedCostFromTicks(
  ticks: unknown,
  incomplete = false,
  partial = false,
): number | undefined {
  if (incomplete || partial || typeof ticks !== "number" || !Number.isFinite(ticks) || ticks < 0) {
    return undefined;
  }
  return ticks / USD_TICKS_PER_USD;
}

function normalizeModelUsage(
  rows: Record<string, SessionUsageModel> | undefined,
  incomplete: boolean,
): Record<string, UsageModelBreakdown> | undefined {
  if (!rows || typeof rows !== "object") return undefined;
  const normalized: Record<string, UsageModelBreakdown> = {};
  for (const [modelId, row] of Object.entries(rows)) {
    if (!modelId || !row || typeof row !== "object") continue;
    const promptTokens = nonNegative(row.inputTokens);
    const completionTokens = nonNegative(row.outputTokens);
    normalized[modelId] = {
      promptTokens,
      completionTokens,
      cachedReadTokens: nonNegative(row.cachedReadTokens),
      cacheCreationTokens: nonNegative(row.cacheCreationTokens),
      reasoningTokens: nonNegative(row.reasoningTokens),
      modelCalls: nonNegative(row.modelCalls) || (promptTokens + completionTokens > 0 ? 1 : 0),
      apiDurationMs: nonNegative(row.apiDurationMs),
      providerCost: trustedCostFromTicks(row.costUsdTicks, incomplete, row.costIsPartial),
    };
  }
  return Object.keys(normalized).length > 0 ? normalized : undefined;
}

function emitUsageChanged(): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent(USAGE_CHANGED_EVENT));
}

/** 估算单次调用费用(按费率表)。 */
export function estimateCost(
  modelId: string,
  promptTokens: number,
  completionTokens: number,
  rates?: Record<string, { prompt: number; completion: number }>,
): number | undefined {
  if (!rates || !rates[modelId]) return undefined;
  const r = rates[modelId];
  return (promptTokens / 1000) * r.prompt + (completionTokens / 1000) * r.completion;
}

/** Resolve a record's display cost. Provider cost wins; local rates re-price history. */
export function usageRecordCost(record: UsageRecord, config?: QuotaConfig | null): number {
  if (typeof record.providerCost === "number" && Number.isFinite(record.providerCost)) {
    return Math.max(0, record.providerCost);
  }
  const rates = config?.rates;
  if (record.modelUsage && Object.keys(record.modelUsage).length > 0) {
    let total = 0;
    let allPriced = true;
    for (const [modelId, row] of Object.entries(record.modelUsage)) {
      if (typeof row.providerCost === "number" && Number.isFinite(row.providerCost)) {
        total += Math.max(0, row.providerCost);
        continue;
      }
      const estimate = estimateCost(modelId, row.promptTokens, row.completionTokens, rates);
      if (estimate !== undefined) {
        total += estimate;
      } else {
        allPriced = false;
      }
    }
    if (allPriced) return total;
  }
  const estimate = estimateCost(record.modelId, record.promptTokens, record.completionTokens, rates);
  if (estimate !== undefined) return estimate;
  return typeof record.cost === "number" && Number.isFinite(record.cost) ? Math.max(0, record.cost) : 0;
}

/** 读取全部用量记录(按时间顺序)。 */
export function loadUsage(): UsageRecord[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? (arr as UsageRecord[]) : [];
  } catch {
    return [];
  }
}

/** 追加一条用量记录(自动算费用)。 */
export function recordUsage(
  records: UsageRecord[],
  entry: {
    modelId: string;
    promptTokens: number;
    completionTokens: number;
    sessionId?: string;
    promptId?: string;
    modelCalls?: number;
  },
  config?: QuotaConfig,
): UsageRecord[] {
  const cost = estimateCost(entry.modelId, entry.promptTokens, entry.completionTokens, config?.rates);
  const record: UsageRecord = {
    date: todayKey(),
    modelId: entry.modelId,
    promptTokens: entry.promptTokens,
    completionTokens: entry.completionTokens,
    cost,
    ts: Date.now(),
    sessionId: entry.sessionId,
    promptId: entry.promptId,
    id: entry.sessionId && entry.promptId ? `${entry.sessionId}:${entry.promptId}` : undefined,
    modelCalls: entry.modelCalls ?? 1,
    source: "legacy",
  };
  const next = record.id
    ? [...records.filter((item) => item.id !== record.id), record]
    : [...records, record];
  saveUsage(next);
  return next;
}

/**
 * Persist one exact prompt ledger. Replaying session history is safe because
 * the stable session/prompt key replaces the same record instead of appending.
 */
export function recordTurnUsage(
  records: UsageRecord[],
  entry: {
    sessionId: string;
    promptId: string;
    usage: SessionUsage;
    occurredAt?: number;
    eventId?: string;
    fallbackModelId?: string;
    projectId?: string;
    turnOrigin?: TurnOrigin;
    turnSourceId?: string;
  },
  config?: QuotaConfig,
): UsageRecord[] {
  if (!entry.sessionId || !entry.promptId || !entry.usage) return records;
  const id = `${entry.sessionId}:${entry.promptId}`;
  const previous = records.find(item => item.id === id);
  const ts = typeof entry.occurredAt === "number" && Number.isFinite(entry.occurredAt) && entry.occurredAt > 0
    ? entry.occurredAt
    : previous?.ts ?? Date.now();
  const incomplete = entry.usage.usageIsIncomplete === true;
  const modelUsage = normalizeModelUsage(entry.usage.modelUsage, incomplete);
  const modelIds = modelUsage ? Object.keys(modelUsage) : [];
  const modelId = modelIds.length === 1 ? modelIds[0] : previous?.modelId || entry.fallbackModelId || "unknown";
  const promptTokens = nonNegative(entry.usage.inputTokens);
  const completionTokens = nonNegative(entry.usage.outputTokens);
  const record: UsageRecord = {
    id,
    date: todayKey(new Date(ts)),
    modelId,
    promptTokens,
    completionTokens,
    ts,
    sessionId: entry.sessionId,
    promptId: entry.promptId,
    eventId: entry.eventId ?? previous?.eventId,
    projectId: previous?.projectId ?? entry.projectId,
    turnOrigin: previous?.turnOrigin && previous.turnOrigin !== "user" ? previous.turnOrigin : entry.turnOrigin ?? previous?.turnOrigin,
    turnSourceId: previous?.turnSourceId ?? entry.turnSourceId,
    modelCalls: nonNegative(entry.usage.modelCalls) || (promptTokens + completionTokens > 0 ? 1 : 0),
    agentTurns: nonNegative(entry.usage.numTurns),
    cachedReadTokens: nonNegative(entry.usage.cachedReadTokens),
    cacheCreationTokens: nonNegative(entry.usage.cacheCreationTokens),
    reasoningTokens: nonNegative(entry.usage.reasoningTokens),
    apiDurationMs: nonNegative(entry.usage.apiDurationMs),
    providerCost: trustedCostFromTicks(
      entry.usage.costUsdTicks,
      incomplete,
      entry.usage.costIsPartial,
    ),
    modelUsage,
    incomplete,
    source: "turn",
  };
  const calculatedCost = usageRecordCost(record, config);
  if (calculatedCost > 0) record.cost = calculatedCost;
  // Upgrade an old cumulative/legacy row in place when the durable replay
  // supplies its stable prompt id. The narrow time+token match avoids deleting
  // unrelated equal-sized turns.
  const next = [...records.filter((item) => {
    if (item.id === record.id) return false;
    if (item.id) return true;
    return item.sessionId !== record.sessionId
      || item.promptTokens !== record.promptTokens
      || item.completionTokens !== record.completionTokens
      || Math.abs(item.ts - record.ts) > 120_000;
  }), record]
    .sort((a, b) => a.ts - b.ts);
  saveUsage(next);
  return next;
}

/**
 * EchoAgent 的 `echo.agent/session/usage` 返回会话累计值，不是单轮增量。
 * 这里按会话保存上一个累计快照，只记录差值，避免每轮把历史 token
 * 重复计入配额。引擎重启导致计数器回退时，将当前值视为新起点。
 */
export function recordCumulativeUsage(
  records: UsageRecord[],
  entry: {
    sessionId: string;
    modelId: string;
    inputTokens: number;
    outputTokens: number;
  },
  config?: QuotaConfig,
): UsageRecord[] {
  const snapshots = loadCumulativeSnapshots();
  const previous = snapshots[entry.sessionId];
  const countersReset = !!previous && (
    entry.inputTokens < previous.inputTokens || entry.outputTokens < previous.outputTokens
  );
  const baseInput = previous && !countersReset ? previous.inputTokens : 0;
  const baseOutput = previous && !countersReset ? previous.outputTokens : 0;
  const promptTokens = Math.max(0, entry.inputTokens - baseInput);
  const completionTokens = Math.max(0, entry.outputTokens - baseOutput);

  snapshots[entry.sessionId] = {
    inputTokens: Math.max(0, entry.inputTokens),
    outputTokens: Math.max(0, entry.outputTokens),
  };
  saveCumulativeSnapshots(snapshots);

  if (promptTokens === 0 && completionTokens === 0) return records;
  return recordUsage(records, {
    sessionId: entry.sessionId,
    modelId: entry.modelId,
    promptTokens,
    completionTokens,
    modelCalls: 1,
  }, config);
}

function loadCumulativeSnapshots(): CumulativeSnapshots {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(SNAPSHOT_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" ? parsed as CumulativeSnapshots : {};
  } catch {
    return {};
  }
}

function saveCumulativeSnapshots(snapshots: CumulativeSnapshots): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(SNAPSHOT_KEY, JSON.stringify(snapshots));
  } catch {
    /* quota / 隐私模式 —— 静默降级 */
  }
}

/** 持久化。 */
function saveUsage(records: UsageRecord[]): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(records));
    emitUsageChanged();
  } catch {
    /* quota / 隐私模式 — 静默降级 */
  }
}

/** 清空用量(测试/重置用)。 */
export function clearUsage(): UsageRecord[] {
  saveUsage([]);
  if (typeof window !== "undefined") {
    try {
      window.localStorage.removeItem(SNAPSHOT_KEY);
      window.localStorage.removeItem(QUOTA_ALERT_KEY);
    } catch {
      /* noop */
    }
  }
  emitUsageChanged();
  return [];
}

function emptyModelSummary() {
  return {
    tokens: 0,
    promptTokens: 0,
    completionTokens: 0,
    cachedReadTokens: 0,
    reasoningTokens: 0,
    cost: 0,
    count: 0,
  };
}

function modelRowCost(
  modelId: string,
  row: UsageModelBreakdown,
  config?: QuotaConfig | null,
): number {
  if (typeof row.providerCost === "number" && Number.isFinite(row.providerCost)) {
    return Math.max(0, row.providerCost);
  }
  return estimateCost(modelId, row.promptTokens, row.completionTokens, config?.rates) ?? 0;
}

/** Summarize all records or a local-calendar date range. */
export function summarizeUsage(
  records: UsageRecord[],
  dateFilter?: { from: string; to: string },
  config?: QuotaConfig | null,
): UsageSummary {
  const filtered = dateFilter
    ? records.filter((r) => r.date >= dateFilter.from && r.date <= dateFilter.to)
    : records;
  const byModel: UsageSummary["byModel"] = {};
  const byDate: UsageSummary["byDate"] = {};
  const byProject: UsageSummary["byProject"] = Object.create(null);
  let totalPrompt = 0;
  let totalCompletion = 0;
  let totalCost = 0;
  let modelCalls = 0;
  let cachedReadTokens = 0;
  let cacheCreationTokens = 0;
  let reasoningTokens = 0;
  let apiDurationMs = 0;
  let incompleteCount = 0;
  for (const r of filtered) {
    const tokens = r.promptTokens + r.completionTokens;
    const recordCalls = r.modelCalls ?? 1;
    const recordCost = usageRecordCost(r, config);
    totalPrompt += r.promptTokens;
    totalCompletion += r.completionTokens;
    totalCost += recordCost;
    modelCalls += recordCalls;
    cachedReadTokens += r.cachedReadTokens ?? 0;
    cacheCreationTokens += r.cacheCreationTokens ?? 0;
    reasoningTokens += r.reasoningTokens ?? 0;
    apiDurationMs += r.apiDurationMs ?? 0;
    if (r.incomplete) incompleteCount += 1;

    if (r.modelUsage && Object.keys(r.modelUsage).length > 0) {
      for (const [modelId, row] of Object.entries(r.modelUsage)) {
        if (!byModel[modelId]) byModel[modelId] = emptyModelSummary();
        const target = byModel[modelId];
        target.promptTokens += row.promptTokens;
        target.completionTokens += row.completionTokens;
        target.tokens += row.promptTokens + row.completionTokens;
        target.cachedReadTokens += row.cachedReadTokens;
        target.reasoningTokens += row.reasoningTokens;
        target.cost += modelRowCost(modelId, row, config);
        target.count += row.modelCalls;
      }
    } else {
      const modelId = r.modelId || "unknown";
      if (!byModel[modelId]) byModel[modelId] = emptyModelSummary();
      const target = byModel[modelId];
      target.tokens += tokens;
      target.promptTokens += r.promptTokens;
      target.completionTokens += r.completionTokens;
      target.cachedReadTokens += r.cachedReadTokens ?? 0;
      target.reasoningTokens += r.reasoningTokens ?? 0;
      target.cost += recordCost;
      target.count += recordCalls;
    }
    if (!byDate[r.date]) byDate[r.date] = { tokens: 0, cost: 0, count: 0, requests: 0 };
    byDate[r.date].tokens += tokens;
    byDate[r.date].cost += recordCost;
    byDate[r.date].count += recordCalls;
    byDate[r.date].requests += 1;
    if (r.projectId) {
      const project = byProject[r.projectId] ??= { tokens: 0, cost: 0, count: 0, requests: 0 };
      project.tokens += tokens;
      project.cost += recordCost;
      project.count += recordCalls;
      project.requests += 1;
    }
  }
  return {
    totalTokens: totalPrompt + totalCompletion,
    totalPrompt,
    totalCompletion,
    totalCost,
    count: modelCalls,
    requestCount: filtered.length,
    modelCalls,
    cachedReadTokens,
    cacheCreationTokens,
    reasoningTokens,
    apiDurationMs,
    incompleteCount,
    byModel,
    byDate,
    byProject,
  };
}

/** 检查配额:返回当前周期已用比例(0–1)及是否超限。 */
export function checkQuota(records: UsageRecord[], config: QuotaConfig): {
  used: number;
  limit: number;
  pct: number;
  exceeded: boolean;
  nearLimit: boolean;
} {
  const key = config.period === "daily" ? todayKey() : monthKey();
  const periodRecords = records.filter((r) =>
    config.period === "daily" ? r.date === key : r.date.startsWith(key),
  );
  const used = periodRecords.reduce((s, r) => s + r.promptTokens + r.completionTokens, 0);
  const hasLimit = config.tokenLimit > 0;
  const pct = hasLimit ? used / config.tokenLimit : 0;
  return {
    used,
    limit: config.tokenLimit,
    pct: Math.round(pct * 100),
    exceeded: hasLimit && used >= config.tokenLimit,
    nearLimit: hasLimit && pct >= 0.8 && pct < 1,
  };
}

/** 是否应暂停桌面端手动发送。未配置/0 上限永远不拦截。 */
export function isQuotaBlocking(records: UsageRecord[], config: QuotaConfig | null): boolean {
  return !!config
    && config.enforcement === "block"
    && config.tokenLimit > 0
    && checkQuota(records, config).exceeded;
}

/**
 * 在当前周期首次跨过 80% / 100% 时返回告警级别。
 * 记录阈值状态避免每轮对话重复通知。
 */
export function consumeQuotaAlert(
  records: UsageRecord[],
  config: QuotaConfig | null,
): "near" | "exceeded" | null {
  if (!config || config.tokenLimit <= 0 || typeof window === "undefined") return null;
  const quota = checkQuota(records, config);
  const level = quota.exceeded ? "exceeded" : quota.nearLimit ? "near" : null;
  if (!level) return null;
  const periodKey = config.period === "daily" ? todayKey() : monthKey();
  const key = `${config.period}:${periodKey}:${config.tokenLimit}`;
  try {
    const previous = JSON.parse(window.localStorage.getItem(QUOTA_ALERT_KEY) ?? "null") as
      | { key?: string; level?: "near" | "exceeded" }
      | null;
    if (previous?.key === key && (previous.level === "exceeded" || previous.level === level)) {
      return null;
    }
    window.localStorage.setItem(QUOTA_ALERT_KEY, JSON.stringify({ key, level }));
  } catch {
    // Storage unavailable: preserve the alert rather than hiding a quota event.
  }
  return level;
}

/** 读取配额配置。 */
export function loadQuotaConfig(): QuotaConfig | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(QUOTA_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as QuotaConfig;
  } catch {
    return null;
  }
}

/** 保存配额配置。 */
export function saveQuotaConfig(config: QuotaConfig): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(QUOTA_KEY, JSON.stringify(config));
    window.localStorage.removeItem(QUOTA_ALERT_KEY);
    emitUsageChanged();
  } catch {
    /* noop */
  }
}

function csvCell(value: unknown): string {
  const text = value == null ? "" : String(value);
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

/** Stable, spreadsheet-friendly export of the local prompt ledger. */
export function serializeUsageCsv(records: UsageRecord[], config?: QuotaConfig | null): string {
  const headers = [
    "date", "time", "session_id", "prompt_id", "model", "input_tokens",
    "output_tokens", "total_tokens", "cached_read_tokens", "cache_creation_tokens",
    "reasoning_tokens", "model_calls", "agent_turns", "api_duration_ms", "cost_usd",
    "incomplete", "source",
  ];
  const rows = records
    .slice()
    .sort((a, b) => a.ts - b.ts)
    .map((record) => [
      record.date,
      new Date(record.ts).toISOString(),
      record.sessionId,
      record.promptId,
      record.modelId,
      record.promptTokens,
      record.completionTokens,
      record.promptTokens + record.completionTokens,
      record.cachedReadTokens ?? 0,
      record.cacheCreationTokens ?? 0,
      record.reasoningTokens ?? 0,
      record.modelCalls ?? 1,
      record.agentTurns ?? "",
      record.apiDurationMs ?? 0,
      usageRecordCost(record, config) || "",
      record.incomplete === true,
      record.source ?? "legacy",
    ]);
  return [headers, ...rows].map((row) => row.map(csvCell).join(",")).join("\n");
}

/** JSON export keeps the exact per-model ledger for later audit/import tooling. */
export function serializeUsageJson(records: UsageRecord[], config?: QuotaConfig | null): string {
  return JSON.stringify({
    schemaVersion: 2,
    scope: "device-local",
    exportedAt: new Date().toISOString(),
    summary: summarizeUsage(records, undefined, config),
    records: records.slice().sort((a, b) => a.ts - b.ts),
  }, null, 2);
}
