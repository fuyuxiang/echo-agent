import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, BarChart, Bar } from "recharts";

export function Analytics() {
  const [days, setDays] = useState(7);
  const { data: tokens } = useApi<{ usage: { date: string; input_tokens: number; output_tokens: number; cost_usd: number }[] }>(`/analytics/tokens?days=${days}`);
  const { data: skills } = useApi<{ skills: { skill: string; count: number }[] }>(`/analytics/skills?days=${days}`);

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

      <section>
        <h2 className="font-semibold mb-2">技能调用排行</h2>
        <div className="bg-white border rounded-lg p-4 h-64">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={skills?.skills?.slice(0, 10) ?? []} layout="vertical">
              <CartesianGrid strokeDasharray="3 3" />
              <XAxis type="number" tick={{ fontSize: 12 }} />
              <YAxis type="category" dataKey="skill" tick={{ fontSize: 12 }} width={120} />
              <Tooltip />
              <Bar dataKey="count" fill="#6366f1" />
            </BarChart>
          </ResponsiveContainer>
        </div>
      </section>
    </div>
  );
}
