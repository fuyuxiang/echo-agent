import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
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
interface SkillUsage { skill: string; calls: number; successes: number; failures: number; success_rate: number }
interface SkillUsageResponse { skills: SkillUsage[]; available: boolean; unavailable_reason?: string }

/** Recharts 的 formatter 参数可能是 undefined/字符串,签名要照它的类型来。 */
function formatUsd(value: unknown): string {
  const n = Number(value);
  return Number.isFinite(n) ? `$${n.toFixed(4)}` : "-";
}

export function Analytics() {
  const { t } = useTranslation(["analytics", "common"]);
  const [days, setDays] = useState(7);
  const { data: tokens, error, refetch: refetchTokens } = useApi<{ usage: TokenUsage[] }>(`/analytics/tokens?days=${days}`);
  // 渠道归因与技能调用质量都来自持久化聚合，页面通过实时事件刷新。
  const { data: channels, error: channelsError, refetch: refetchChannels } = useApi<{ channels: ChannelUsage[] }>(
    `/analytics/channels?days=${days}`
  );
  const { data: skills, error: skillsError, refetch: refetchSkills } = useApi<SkillUsageResponse>(
    `/analytics/skills?days=${days}`,
  );
  useWsSubscribe(["analytics"], () => {
    refetchTokens(); refetchChannels(); refetchSkills();
  }, ["analytics_updated"]);

  const usage = tokens?.usage ?? [];
  const channelRows = channels?.channels ?? [];
  const skillRows = Array.isArray(skills?.skills) ? skills.skills : [];
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

      <section>
        <h2 className="font-semibold mb-2">{t("skillUsage")}</h2>
        <div className="bg-white border rounded-lg overflow-x-auto">
          {skillsError ? <div className="p-4 text-sm text-red-600">{t("common:loadFailed", { error: skillsError })}</div>
            : skills?.available === false ? <div className="p-4 text-sm text-amber-700">{t("skillUnavailable")}</div>
            : skillRows.length === 0 ? <div className="p-4 text-sm text-gray-400">{t("common:noData")}</div>
            : <table className="w-full text-sm">
              <thead className="bg-gray-50 text-gray-500"><tr>
                <th className="text-left p-3">{t("skill")}</th><th className="text-right p-3">{t("calls")}</th>
                <th className="text-right p-3">{t("successes")}</th><th className="text-right p-3">{t("failures")}</th>
                <th className="text-right p-3">{t("successRate")}</th>
              </tr></thead>
              <tbody>{skillRows.map((row) => <tr key={row.skill} className="border-t">
                <td className="p-3 font-medium">{row.skill}</td><td className="p-3 text-right tabular-nums">{row.calls}</td>
                <td className="p-3 text-right text-green-700 tabular-nums">{row.successes}</td>
                <td className="p-3 text-right text-red-700 tabular-nums">{row.failures}</td>
                <td className="p-3 text-right tabular-nums">{(row.success_rate * 100).toFixed(1)}%</td>
              </tr>)}</tbody>
            </table>}
        </div>
      </section>
    </div>
  );
}
