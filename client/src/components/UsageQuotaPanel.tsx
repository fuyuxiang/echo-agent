import { modelDisplayName, modelSourceLabel } from "@/lib/model-presentation";
import { SettingsDisclosure } from "./SettingsDisclosure";
import { useCallback, useEffect, useMemo, useState } from "react";
import { AlertTriangle, Download, RefreshCw, Trash2 } from "lucide-react";
import { exportTextFile } from "@/lib/agent-client";
import {
  USAGE_CHANGED_EVENT,
  checkQuota,
  clearUsage,
  loadQuotaConfig,
  loadUsage,
  monthKey,
  saveQuotaConfig,
  serializeUsageCsv,
  serializeUsageJson,
  summarizeUsage,
  todayKey,
  usageRecordCost,
  type QuotaConfig,
  type UsageRecord,
} from "@/lib/usage-quota";
import { stripUpstreamBrandedIds } from "@/lib/model-branding";
import { useAppDialog } from "./AppDialog";

type Period = "daily" | "monthly";
type RateDraft = { prompt: string; completion: string };

const EMPTY_CONFIG: QuotaConfig = {
  period: "daily",
  tokenLimit: 0,
  enforcement: "warn",
  rates: {},
};

function formatTokens(value: number): string {
  return value.toLocaleString();
}

/**
 * Narrow columns (the 14-day trend is ~50px wide) cannot fit a grouped
 * 7-9 digit token count, so abbreviate there and keep the exact value in the
 * surrounding tooltip.
 */
function formatTokensCompact(value: number): string {
  if (value < 1000) return String(value);
  if (value < 1_000_000) {
    const k = value / 1000;
    return `${k < 100 ? k.toFixed(1) : Math.round(k)}K`;
  }
  const m = value / 1_000_000;
  return `${m < 10 ? m.toFixed(2) : m < 100 ? m.toFixed(1) : Math.round(m)}M`;
}

function formatDuration(ms: number): string {
  if (ms <= 0) return "—";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  return `${(ms / 60_000).toFixed(1)} min`;
}

function lastLocalDateKeys(days: number): string[] {
  const result: string[] = [];
  const now = new Date();
  for (let offset = days - 1; offset >= 0; offset -= 1) {
    result.push(todayKey(new Date(now.getFullYear(), now.getMonth(), now.getDate() - offset)));
  }
  return result;
}

/**
 * 明细行的模型列文案。带上游品牌词的 id 显示为中性文案 —— 历史用量记录可能是在源头
 * 防护生效之前落盘的,过滤保证旧数据也不会把品牌名带到界面上。
 */
function recordModels(record: UsageRecord): string {
  const ids = Object.keys(record.modelUsage ?? {});
  if (ids.length > 0) {
    const labels = new Set(ids.map((id) => `${modelDisplayName(id)} · ${modelSourceLabel(id)}`));
    return Array.from(labels).join(", ");
  }
  return record.modelId ? `${modelDisplayName(record.modelId)} · ${modelSourceLabel(record.modelId)}` : "unknown";
}

export function UsageQuotaPanel() {
  const [records, setRecords] = useState<UsageRecord[]>([]);
  const [config, setConfig] = useState<QuotaConfig>(EMPTY_CONFIG);
  const [period, setPeriod] = useState<Period>("daily");
  const [rateDrafts, setRateDrafts] = useState<Record<string, RateDraft>>({});
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const { requestConfirmation, dialog } = useAppDialog();

  const refresh = useCallback(() => {
    setRecords(loadUsage());
    setConfig(loadQuotaConfig() ?? EMPTY_CONFIG);
  }, []);

  useEffect(() => {
    const stored = loadQuotaConfig() ?? EMPTY_CONFIG;
    setRecords(loadUsage());
    setConfig(stored);
    setPeriod(stored.period);
    const onChanged = () => refresh();
    const onStorage = (event: StorageEvent) => {
      if (!event.key || event.key.startsWith("echoagent.usage") || event.key === "echoagent.quota") {
        refresh();
      }
    };
    window.addEventListener(USAGE_CHANGED_EVENT, onChanged);
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(USAGE_CHANGED_EVENT, onChanged);
      window.removeEventListener("storage", onStorage);
    };
  }, [refresh]);

  const key = period === "daily" ? todayKey() : monthKey();
  const filtered = useMemo(() => records.filter((record) =>
    period === "daily" ? record.date === key : record.date.startsWith(key),
  ), [key, period, records]);
  const summary = useMemo(() => summarizeUsage(filtered, undefined, config), [config, filtered]);
  const quota = config.tokenLimit > 0 ? checkQuota(records, config) : null;
  const cacheHitPct = summary.totalPrompt > 0
    ? Math.min(100, (summary.cachedReadTokens / summary.totalPrompt) * 100)
    : 0;

  const trendKeys = useMemo(() => lastLocalDateKeys(14), [records]);
  const trendSummary = useMemo(() => summarizeUsage(records, {
    from: trendKeys[0],
    to: trendKeys[trendKeys.length - 1],
  }, config), [config, records, trendKeys]);
  const trendMax = Math.max(1, ...trendKeys.map((date) => trendSummary.byDate[date]?.tokens ?? 0));

  // Display friendly names while keeping the original IDs as rate-table keys.
  const modelIds = useMemo(() => stripUpstreamBrandedIds(Array.from(new Set([
    ...Object.keys(summarizeUsage(records, undefined, config).byModel),
    ...Object.keys(config.rates ?? {}),
  ]))).sort(), [config, records]);

  useEffect(() => {
    setRateDrafts(Object.fromEntries(modelIds.map((modelId) => {
      const rate = config.rates?.[modelId];
      return [modelId, {
        prompt: rate ? String(rate.prompt * 1000) : "",
        completion: rate ? String(rate.completion * 1000) : "",
      }];
    })));
  }, [config.rates, modelIds.join("\u0000")]);

  const persistConfig = (next: QuotaConfig, syncPeriod = false) => {
    saveQuotaConfig(next);
    setConfig(next);
    if (syncPeriod) setPeriod(next.period);
  };

  const saveRates = () => {
    const rates: NonNullable<QuotaConfig["rates"]> = {};
    for (const [modelId, draft] of Object.entries(rateDrafts)) {
      const promptPerMillion = Number(draft.prompt);
      const completionPerMillion = Number(draft.completion);
      const prompt = Number.isFinite(promptPerMillion) && promptPerMillion >= 0
        ? promptPerMillion / 1000
        : 0;
      const completion = Number.isFinite(completionPerMillion) && completionPerMillion >= 0
        ? completionPerMillion / 1000
        : 0;
      if (prompt > 0 || completion > 0) rates[modelId] = { prompt, completion };
    }
    persistConfig({ ...config, rates });
    setMessage("本地估算费率已保存，历史记录已按新费率重新计算。");
  };

  const handleExport = async (format: "csv" | "json") => {
    if (records.length === 0) {
      setMessage("暂无可导出的用量记录。");
      return;
    }
    setBusy(true);
    try {
      const content = format === "csv"
        ? serializeUsageCsv(records, config)
        : serializeUsageJson(records, config);
      const path = await exportTextFile(
        `echoagent-usage-${todayKey()}.${format}`,
        content,
        format,
      );
      if (!path) return;
      setMessage(`已导出到 ${path}`);
    } catch (error) {
      setMessage(`导出失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setBusy(false);
    }
  };

  const handleClear = () => {
    if (records.length === 0) return;
    requestConfirmation({
      title: "清空所有 Token 用量记录？",
      description: "此操作只会清空本机记录，且无法撤销。建议先导出备份。",
      confirmLabel: "清空记录",
      danger: true,
      action: () => {
        clearUsage();
        setRecords([]);
        setMessage("本机用量记录已清空。");
      },
    });
  };

  const recent = filtered.slice().sort((a, b) => b.ts - a.ts).slice(0, 20);

  return (
    <section className="quota-panel" aria-label="Token 用量与配额" tabIndex={0}>
      <div className="quota-panel__head">
        <div>
          <h3 className="quota-panel__title">用量概览</h3>
          <p className="quota-panel__subtitle">仅统计本机记录的调用，自动按所选周期汇总。</p>
        </div>
        <div className="quota-panel__actions">
          <button type="button" className="settings-btn quota-panel__action" onClick={refresh} title="刷新" aria-label="刷新用量"><RefreshCw size={14} /></button>
          <button type="button" className="settings-btn quota-panel__action" onClick={() => void handleExport("csv")} disabled={busy}><Download size={14} /> CSV</button>

        </div>
      </div>

      <div className="quota-panel__toolbar">
        <div className="quota-panel__period" aria-label="统计周期">
          {(["daily", "monthly"] as const).map((value) => (
            <button key={value} type="button" className={`quota-panel__period-btn${period === value ? " active" : ""}`} onClick={() => setPeriod(value)} aria-pressed={period === value}>
              {value === "daily" ? "今日" : "本月"}
            </button>
          ))}
        </div>
        <span className="quota-panel__scope">已记录 {records.length.toLocaleString()} 个轮次</span>
      </div>

      <div className="quota-panel__stats">
        <div className="quota-panel__stat"><strong>{formatTokens(summary.totalTokens)}</strong><span>总 Token</span></div>
        <div className="quota-panel__stat"><strong>{formatTokens(summary.totalPrompt)}</strong><span>输入 Token</span></div>
        <div className="quota-panel__stat"><strong>{formatTokens(summary.totalCompletion)}</strong><span>输出 Token</span></div>
        <div className="quota-panel__stat"><strong>{summary.totalCost > 0 ? `$${summary.totalCost.toFixed(4)}` : "—"}</strong><span>费用</span></div>
      </div>

      {summary.incompleteCount > 0 && (
        <div className="quota-panel__notice quota-panel__notice--warn"><AlertTriangle size={15} /> {summary.incompleteCount} 个轮次被运行时标记为用量不完整，Token 可能偏低，费用不会被当作可信账单值。</div>
      )}
      {message && <div className="quota-panel__notice">{message}</div>}

      {quota && (
        <div className="quota-panel__quota">
          <div className="quota-panel__quota-head">
            <span>{config.period === "daily" ? "每日" : "每月"}配额</span>
            <span className={`quota-panel__quota-pct${quota.exceeded ? " exceeded" : quota.nearLimit ? " near" : ""}`}>{formatTokens(quota.used)} / {formatTokens(quota.limit)} ({quota.pct}%)</span>
          </div>
          <div className="quota-panel__quota-bar" role="progressbar" aria-valuenow={Math.min(100, quota.pct)} aria-valuemin={0} aria-valuemax={100}>
            <div className={`quota-panel__quota-fill${quota.exceeded ? " exceeded" : quota.nearLimit ? " near" : ""}`} style={{ width: `${Math.min(100, quota.pct)}%` }} />
          </div>
          <p className="quota-panel__config-note">仅限制本机手动发送；自动化和其他客户端不受此限制。</p>
          {(quota.exceeded || quota.nearLimit) && <span className="quota-panel__quota-warn">{quota.exceeded ? "已达上限" : "已达 80%"}</span>}
        </div>
      )}

      <div className="quota-panel__section">
        <h3>14 天趋势</h3>
        <div className="quota-panel__trend" aria-label="近 14 天 Token 趋势">
          {trendKeys.map((date) => {
            const value = trendSummary.byDate[date]?.tokens ?? 0;
            return (
              <div key={date} className="quota-panel__trend-item" title={`${date}: ${formatTokens(value)} Token`}>
                <span className="quota-panel__trend-value">{value > 0 ? formatTokensCompact(value) : ""}</span>
                <span className="quota-panel__trend-track"><span style={{ height: `${Math.max(value > 0 ? 5 : 0, (value / trendMax) * 100)}%` }} /></span>
                <span className="quota-panel__trend-label">{date.slice(5)}</span>
              </div>
            );
          })}
        </div>
      </div>

      {Object.keys(summary.byModel).length > 0 && (
        <div className="quota-panel__section">
          <h3>按模型</h3>
          <div className="quota-panel__model-table" role="table">
            <div className="quota-panel__model-row quota-panel__model-row--head" role="row"><span>模型</span><span>输入</span><span>输出</span><span>缓存</span><span>调用</span><span>费用</span></div>
            {Object.entries(summary.byModel).sort((a, b) => b[1].tokens - a[1].tokens).map(([modelId, row]) => (
              <div key={modelId} className="quota-panel__model-row" role="row">
                <span className="quota-panel__model-name" title={`${modelDisplayName(modelId)} · ${modelSourceLabel(modelId)}`}><span>{modelDisplayName(modelId)}</span><small> · {modelSourceLabel(modelId)}</small></span><span>{formatTokens(row.promptTokens)}</span><span>{formatTokens(row.completionTokens)}</span><span>{formatTokens(row.cachedReadTokens)}</span><span>{formatTokens(row.count)}</span><span>{row.cost > 0 ? `$${row.cost.toFixed(4)}` : "—"}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      <SettingsDisclosure title="用量提醒与限额" description={config.tokenLimit > 0 ? `${config.period === "daily" ? "每日" : "每月"} ${formatTokens(config.tokenLimit)} Token · ${config.enforcement === "block" ? "达到上限暂停手动发送" : "达到上限提醒"}` : "未设置上限 · 可选的本机手动发送限制"}>
      <div className="quota-panel__section quota-panel__config">
        <div className="quota-panel__config-grid">
          <label className="quota-panel__config-row"><span>配额周期</span><select className="form-control" value={config.period} onChange={(event) => persistConfig({ ...config, period: event.target.value as Period }, true)}><option value="daily">每日</option><option value="monthly">每月</option></select></label>
          <label className="quota-panel__config-row"><span>Token 上限</span><input className="form-control" type="number" min="0" value={config.tokenLimit > 0 ? config.tokenLimit : ""} placeholder="不限" onChange={(event) => persistConfig({ ...config, tokenLimit: Math.max(0, Number.parseInt(event.target.value, 10) || 0) })} /></label>
          <label className="quota-panel__config-row"><span>达到上限</span><select className="form-control" value={config.enforcement ?? "warn"} onChange={(event) => persistConfig({ ...config, enforcement: event.target.value as "warn" | "block" })}><option value="warn">仅提醒</option><option value="block">暂停手动发送</option></select></label>
        </div>
        <p className="quota-panel__config-note">配额只限制本桌面端的手动发送；自动化和其他客户端仍可继续消耗服务商配额。</p>
      </div>

      </SettingsDisclosure>
      <SettingsDisclosure title="成本估算费率" description="可选 · 为没有返回成本的模型设置本地估算价格">
      <div className="quota-panel__section quota-panel__rates">
        <p>仅在运行时没有返回可信成本时使用，单位为 USD / 百万 Token；修改后会即时重新估算历史记录。</p>
        {modelIds.length === 0 ? <div className="quota-panel__empty">完成一次模型调用后，这里会列出可配置的模型。</div> : (
          <>
            <div className="quota-panel__rate-table">
              <div className="quota-panel__rate-row quota-panel__rate-row--head"><span>模型</span><span>输入 $/M</span><span>输出 $/M</span></div>
              {modelIds.map((modelId) => (
                <div className="quota-panel__rate-row" key={modelId}>
                  <span title={modelDisplayName(modelId)}>{modelDisplayName(modelId)} · {modelSourceLabel(modelId)}</span>
                  <input className="form-control" aria-label={`${modelDisplayName(modelId)} · ${modelSourceLabel(modelId)} 输入费率`} type="number" min="0" step="0.01" placeholder="未设置" value={rateDrafts[modelId]?.prompt ?? ""} onChange={(event) => setRateDrafts((current) => ({ ...current, [modelId]: { ...(current[modelId] ?? { completion: "" }), prompt: event.target.value } }))} />
                  <input className="form-control" aria-label={`${modelDisplayName(modelId)} · ${modelSourceLabel(modelId)} 输出费率`} type="number" min="0" step="0.01" placeholder="未设置" value={rateDrafts[modelId]?.completion ?? ""} onChange={(event) => setRateDrafts((current) => ({ ...current, [modelId]: { ...(current[modelId] ?? { prompt: "" }), completion: event.target.value } }))} />
                </div>
              ))}
            </div>
            <button type="button" className="settings-btn settings-btn--primary quota-panel__save-rates" onClick={saveRates}>保存费率</button>
          </>
        )}
      </div>

      </SettingsDisclosure>
      <SettingsDisclosure title="用量明细与导出" description={`已记录 ${records.length} 个轮次 · 查看最近记录、导出 JSON 或清空本机记录`}>
      <div className="quota-panel__stats quota-panel__stats--details">
        <div className="quota-panel__stat"><strong>{formatTokens(summary.modelCalls)}</strong><span>模型调用</span></div>
        <div className="quota-panel__stat"><strong>{summary.requestCount.toLocaleString()}</strong><span>用户轮次</span></div>
        <div className="quota-panel__stat"><strong>{cacheHitPct.toFixed(1)}%</strong><span>缓存命中</span></div>
      </div>
      <div className="quota-panel__actions">
          <button type="button" className="settings-btn quota-panel__action" onClick={() => void handleExport("json")} disabled={busy}><Download size={14} /> JSON</button>
          <button type="button" className="settings-btn settings-btn--danger quota-panel__action" onClick={handleClear} disabled={records.length === 0}><Trash2 size={14} /> 清空</button>
      </div>
      <div className="quota-panel__section">
        <h3>最近轮次 <span>{recent.length} / {filtered.length}</span></h3>
        {recent.length === 0 ? <div className="quota-panel__empty">当前周期暂无用量。</div> : (
          <div className="quota-panel__records">
            <div className="quota-panel__record quota-panel__record--head"><span>时间</span><span>模型</span><span>输入 / 输出</span><span>调用</span><span>耗时</span><span>费用</span></div>
            {recent.map((record) => {
              const cost = usageRecordCost(record, config);
              return (
                <div className="quota-panel__record" key={record.id ?? `${record.ts}:${record.modelId}`}>
                  <span>{new Date(record.ts).toLocaleString()}</span><span title={recordModels(record)}>{recordModels(record)}</span><span>{formatTokens(record.promptTokens)} / {formatTokens(record.completionTokens)}</span><span>{record.modelCalls ?? 1}{record.incomplete ? " *" : ""}</span><span>{formatDuration(record.apiDurationMs ?? 0)}</span><span>{cost > 0 ? `$${cost.toFixed(4)}` : "—"}</span>
                </div>
              );
            })}
          </div>
        )}
      </div>
      </SettingsDisclosure>
      {dialog}
    </section>
  );
}
