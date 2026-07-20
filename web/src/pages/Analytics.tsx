import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer } from "recharts";

export function Analytics() {
  const [days, setDays] = useState(7);
  const { data: tokens, error } = useApi<{ usage: { date: string; input_tokens: number; output_tokens: number; cost_usd: number }[] }>(`/analytics/tokens?days=${days}`);
  // 注:技能调用排行(get_skill_usage)暂无数据源(成本埋点只记 provider/model/channel,
  // 未记 skill 维度),后端返回空;故此处先不渲染技能图表,待补埋点后再加回。

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        <span className="text-sm text-gray-500">时间范围:</span>
        {[1, 7, 30].map((d) => (
          <button
            key={d}
            onClick={() => setDays(d)}
            className={`px-3 py-1 rounded text-sm ${days === d ? "bg-blue-600 text-white" : "bg-gray-100"}`}
          >
            {d === 1 ? "今天" : `${d}天`}
          </button>
        ))}
      </div>

      {error && (
        <div className="bg-red-50 text-red-600 border border-red-200 rounded-lg p-4 text-sm">
          加载失败：{error}
        </div>
      )}

      <section>
        <h2 className="font-semibold mb-2">Token 消耗趋势</h2>
        <div className="bg-white border rounded-lg p-4 h-64">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={tokens?.usage ?? []}>
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis dataKey="date" tick={{ fontSize: 12 }} />
              <YAxis tick={{ fontSize: 12 }} />
              <Tooltip />
              <Line type="monotone" dataKey="input_tokens" stroke="#3b82f6" name="Input" />
              <Line type="monotone" dataKey="output_tokens" stroke="#10b981" name="Output" />
            </LineChart>
          </ResponsiveContainer>
        </div>
      </section>
    </div>
  );
}
