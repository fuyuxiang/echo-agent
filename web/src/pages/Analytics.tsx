import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import {
  LineChart, Line, BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer,
} from "recharts";

interface TokenUsage {
  date: string;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
}

interface ChannelUsage {
  channel: string;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
}

/** Recharts 的 formatter 参数可能是 undefined/字符串,签名要照它的类型来。 */
function formatUsd(value: unknown): string {
  const n = Number(value);
  return Number.isFinite(n) ? `$${n.toFixed(4)}` : "-";
}

export function Analytics() {
  const { t } = useTranslation(["analytics", "common"]);
  const [days, setDays] = useState(7);
  const { data: tokens, error } = useApi<{ usage: TokenUsage[] }>(`/analytics/tokens?days=${days}`);
  // 渠道归因:后端 get_channel_usage 是真实 SQL 聚合(cost_ledger_dim 有 channel 列),
  // 此前端点完全没被使用。注:技能维度(get_skill_usage)确实无数据源——成本埋点只记
  // provider/model/channel,cost_ledger_dim 无 skill 列,故不做技能图表。
  const { data: channels, error: channelsError } = useApi<{ channels: ChannelUsage[] }>(
    `/analytics/channels?days=${days}`
  );

  const usage = tokens?.usage ?? [];
  const channelRows = channels?.channels ?? [];
  const totalCost = error ? null : usage.reduce((sum, d) => sum + (d.cost_usd ?? 0), 0);

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-sm text-gray-500">{t("timeRange")}</span>
        {[1, 7, 30].map((d) => (
          <button
            key={d}
            onClick={() => setDays(d)}
            aria-pressed={days === d}
            className={`px-3 py-1 rounded text-sm ${days === d ? "bg-blue-600 text-white" : "bg-gray-100"}`}
          >
            {d === 1 ? t("today") : t("days", { count: d })}
          </button>
        ))}
        <span className="ml-auto text-sm text-gray-600">
          {t("totalCost", { cost: totalCost !== null ? totalCost.toFixed(4) : "-" })}
        </span>
      </div>

      {error && (
        <div className="bg-red-50 text-red-600 border border-red-200 rounded-lg p-4 text-sm">
          {t("common:loadFailed", { error })}
        </div>
      )}

      <section>
        <h2 className="font-semibold mb-2">{t("tokenTrend")}</h2>
        <div className="bg-white border rounded-lg p-4 h-64">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={usage}>
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis dataKey="date" tick={{ fontSize: 12 }} />
              <YAxis tick={{ fontSize: 12 }} />
              <Tooltip />
              <Legend />
              <Line type="monotone" dataKey="input_tokens" stroke="#3b82f6" name={t("inputTokens")} />
              <Line type="monotone" dataKey="output_tokens" stroke="#10b981" name={t("outputTokens")} />
            </LineChart>
          </ResponsiveContainer>
        </div>
      </section>

      {/* 成本此前在类型里声明了 cost_usd 却从未渲染,而成本是运营视角最关心的指标。 */}
      <section>
        <h2 className="font-semibold mb-2">{t("costTrend")}</h2>
        <div className="bg-white border rounded-lg p-4 h-64">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={usage}>
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis dataKey="date" tick={{ fontSize: 12 }} />
              <YAxis tick={{ fontSize: 12 }} unit="$" />
              <Tooltip formatter={formatUsd} />
              <Line type="monotone" dataKey="cost_usd" stroke="#f59e0b" name={t("costUsd")} />
            </LineChart>
          </ResponsiveContainer>
        </div>
      </section>

      <section>
        <h2 className="font-semibold mb-2">{t("channelAttribution")}</h2>
        <div className="bg-white border rounded-lg p-4 h-64">
          {channelsError ? (
            <div className="text-sm text-red-600">{t("common:loadFailed", { error: channelsError })}</div>
          ) : channelRows.length === 0 ? (
            <div className="text-sm text-gray-400">{t("common:noData")}</div>
          ) : (
            <ResponsiveContainer width="100%" height="100%">
              <BarChart data={channelRows}>
                <CartesianGrid strokeDasharray="3 3" />
                <XAxis dataKey="channel" tick={{ fontSize: 12 }} />
                <YAxis tick={{ fontSize: 12 }} unit="$" />
                <Tooltip formatter={formatUsd} />
                <Bar dataKey="cost_usd" fill="#6366f1" name={t("costUsd")} />
              </BarChart>
            </ResponsiveContainer>
          )}
        </div>
      </section>
    </div>
  );
}
