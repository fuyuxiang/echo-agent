import { useApi } from "../hooks/use-api";
import { Activity, Radio, Brain, Coins, AlertCircle, CheckCircle, AlertTriangle } from "lucide-react";

interface HealthData {
  status: string;
  active_sessions: number;
}

interface TasksData {
  tasks: { status: string }[];
  total: number;
}

// 后端 channels.py 返回 name/enabled/running,没有 type 字段。
interface ChannelItem {
  name: string;
  enabled: boolean;
  running: boolean;
}

const STATUS_ICON: Record<string, React.ReactNode> = {
  healthy: <CheckCircle className="text-green-500" size={20} />,
  degraded: <AlertTriangle className="text-yellow-500" size={20} />,
  unhealthy: <AlertCircle className="text-red-500" size={20} />,
};

export function Overview() {
  const { data: health } = useApi<HealthData>("/health");
  const { data: channels } = useApi<{ channels: ChannelItem[] }>("/channels");
  const { data: tasks } = useApi<TasksData>("/tasks?board_id=default");
  const { data: memory } = useApi<{ total: number }>("/memory/stats");

  const statusCounts: Record<string, number> = {};
  tasks?.tasks.forEach((t) => {
    statusCounts[t.status] = (statusCounts[t.status] || 0) + 1;
  });

  const onlineChannels = channels?.channels.filter((c) => c.running).length ?? 0;

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        {STATUS_ICON[health?.status || "unhealthy"]}
        <span className="text-lg font-semibold capitalize">{health?.status || "unknown"}</span>
      </div>

      <div className="grid grid-cols-4 gap-4">
        <MetricCard icon={<Activity size={18} />} label="活跃会话" value={health?.active_sessions ?? 0} />
        <MetricCard icon={<Radio size={18} />} label="通道在线" value={onlineChannels} />
        <MetricCard icon={<Brain size={18} />} label="记忆条数" value={memory?.total ?? 0} />
        <MetricCard icon={<Coins size={18} />} label="Running 任务" value={statusCounts["running"] ?? 0} />
      </div>

      <div className="grid grid-cols-2 gap-6">
        <section>
          <h2 className="font-semibold mb-2">看板摘要</h2>
          <div className="flex gap-2 flex-wrap">
            {["pending", "queued", "running", "blocked", "review", "success"].map((s) => (
              <span key={s} className="px-2 py-1 bg-gray-100 rounded text-sm">
                {s}: {statusCounts[s] ?? 0}
              </span>
            ))}
          </div>
        </section>
        <section>
          <h2 className="font-semibold mb-2">通道状态</h2>
          <div className="space-y-1">
            {channels?.channels.map((ch) => (
              <div key={ch.name} className="flex items-center gap-2 text-sm">
                <span className={`w-2 h-2 rounded-full ${ch.running ? "bg-green-500" : "bg-gray-300"}`} />
                <span>{ch.name}</span>
                <span className="text-gray-400">{ch.running ? "在线" : "离线"}</span>
              </div>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}

function MetricCard({ icon, label, value }: { icon: React.ReactNode; label: string; value: number }) {
  return (
    <div className="bg-white rounded-lg border p-4 flex items-center gap-3">
      <div className="text-gray-500">{icon}</div>
      <div>
        <div className="text-2xl font-bold">{value}</div>
        <div className="text-sm text-gray-500">{label}</div>
      </div>
    </div>
  );
}