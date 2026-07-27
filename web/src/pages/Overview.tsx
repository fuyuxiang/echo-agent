import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
import { statusLabel } from "../stores/kanban";
import { Activity, Radio, Brain, Coins, Plug, AlertCircle, CheckCircle, AlertTriangle } from "lucide-react";

interface HealthData {
  status: string;
  active_sessions: number;
  // 已连接到 /ws 的交互客户端数(TUI、网页 playground)。≥1 表示至少一个 CLI 连着。
  ws_clients: number;
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

/** 概览页轮询间隔。这是首屏“当前状态”视图,此前打开后数字就冻结了,不刷新页面
 *  永远看不到变化。 */
const POLL_INTERVAL_MS = 30_000;

export function Overview() {
  const { t } = useTranslation(["overview", "common"]);
  const { data: health, refetch: refetchHealth } = useApi<HealthData>("/health");
  const { data: channels, refetch: refetchChannels } = useApi<{ channels: ChannelItem[] }>("/channels");
  const { data: tasks, refetch: refetchTasks } = useApi<TasksData>("/tasks?board_id=default");
  const { data: memory, refetch: refetchMemory } = useApi<{ total: number }>("/memory/stats");

  useEffect(() => {
    const timer = window.setInterval(() => {
      refetchHealth();
      refetchChannels();
      refetchTasks();
      refetchMemory();
    }, POLL_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [refetchHealth, refetchChannels, refetchTasks, refetchMemory]);

  // 任务计数额外订阅 WS,让状态变化即时反映而不必等下一轮轮询。只订阅 tasks:
  // _EVENT_CHANNEL_MAP 里还列了 sessions/channels/memory 等,但服务端只有
  // TaskManager 与 Scheduler 接了 broadcast sink(app.py),cron 事件与本页的
  // 计数无关,订阅其余频道仍会做成死功能。
  useWsSubscribe(
    ["tasks"],
    () => { refetchTasks(); },
    ["task_created", "task_transitioned", "task_updated"],
  );

  const statusCounts: Record<string, number> = {};
  // Guard the array too, not just the response: an error/partial payload that
  // omits `tasks` would throw here and blank the page.
  tasks?.tasks?.forEach((task) => {
    statusCounts[task.status] = (statusCounts[task.status] || 0) + 1;
  });

  const onlineChannels = channels?.channels?.filter((c) => c.running).length ?? 0;

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        {STATUS_ICON[health?.status || "unhealthy"]}
        <span className="text-lg font-semibold capitalize">{health?.status || t("status.unknown")}</span>
      </div>

      {/* 5 张卡此前硬编码 grid-cols-5,窄屏(笔记本分屏、平板)直接横向溢出。 */}
      <div className="grid grid-cols-2 sm:grid-cols-3 xl:grid-cols-5 gap-4">
        <MetricCard icon={<Activity size={18} />} label={t("metrics.activeSessions")} value={health?.active_sessions ?? 0} />
        <MetricCard icon={<Radio size={18} />} label={t("metrics.channelsOnline")} value={onlineChannels} />
        <MetricCard icon={<Plug size={18} />} label={t("metrics.cliClients")} value={health?.ws_clients ?? 0} />
        <MetricCard icon={<Brain size={18} />} label={t("metrics.memoryCount")} value={memory?.total ?? 0} />
        <MetricCard icon={<Coins size={18} />} label={t("metrics.runningTasks")} value={statusCounts["running"] ?? 0} />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <section>
          <h2 className="font-semibold mb-2">{t("kanbanSummary")}</h2>
          <div className="flex gap-2 flex-wrap">
            {["pending", "queued", "running", "blocked", "review", "success"].map((s) => (
              <span key={s} className="px-2 py-1 bg-gray-100 rounded text-sm">
                {statusLabel(s)}: {statusCounts[s] ?? 0}
              </span>
            ))}
          </div>
        </section>
        <section>
          <h2 className="font-semibold mb-2">{t("channelStatus")}</h2>
          <div className="space-y-1">
            {channels?.channels?.map((ch) => (
              <div key={ch.name} className="flex items-center gap-2 text-sm">
                <span className={`w-2 h-2 rounded-full ${ch.running ? "bg-green-500" : "bg-gray-300"}`} />
                <span>{ch.name}</span>
                <span className="text-gray-400">{ch.running ? t("common:online") : t("common:offline")}</span>
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
