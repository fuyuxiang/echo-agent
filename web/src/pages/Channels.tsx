import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
import { apiFetch } from "../lib/api";
import { runMutation } from "../stores/toast";
import { useIsAdmin } from "../stores/capabilities";
import { Loadable } from "../components/Loadable";
import { RefreshCw, Power, RotateCw } from "lucide-react";
import { useState } from "react";

interface Channel {
  name: string;
  enabled: boolean;
  running: boolean;
  allow_from_count?: number;
  group_policy?: string;
}

export function Channels() {
  const { t } = useTranslation(["channels", "common"]);
  const { data, loading, error, refetch } = useApi<{ channels: Channel[] }>("/channels");
  const canWrite = useIsAdmin() !== false;
  const [inflight, setInflight] = useState<string | null>(null);

  useWsSubscribe(["channels"], () => refetch(), ["channel_updated"]);

  const lifecycle = async (channel: Channel, action: "start" | "stop" | "restart") => {
    setInflight(`${channel.name}:${action}`);
    const ok = await runMutation(
      () => apiFetch(`/channels/${channel.name}/${action}`, { method: "POST" }),
      { success: t(`actionSuccess.${action}`), error: t(`actionFailed.${action}`) },
    );
    setInflight(null);
    if (ok) refetch();
  };

  const toggleEnabled = async (channel: Channel) => {
    setInflight(`${channel.name}:toggle`);
    if (channel.running && channel.enabled) {
      const stopped = await runMutation(
        () => apiFetch(`/channels/${channel.name}/stop`, { method: "POST" }),
        { error: t("actionFailed.stop") },
      );
      if (!stopped) { setInflight(null); return; }
    }
    const ok = await runMutation(
      () => apiFetch("/config", {
        method: "PATCH",
        body: JSON.stringify({ changes: { [`channels.${channel.name}.enabled`]: !channel.enabled } }),
      }),
      { success: channel.enabled ? t("disabledSuccess") : t("enabledSuccess"), error: t("toggleFailed") },
    );
    if (ok && !channel.enabled) {
      await runMutation(
        () => apiFetch(`/channels/${channel.name}/start`, { method: "POST" }),
        { error: t("actionFailed.start") },
      );
    }
    setInflight(null); refetch();
  };

  return (
    <div className="space-y-3">
      {/* running 状态会随通道重连/掉线变化,但后端没有 channels 频道推送,页面此前
          只在挂载时取一次 —— 一个已经掉线的通道会一直显示“在线”。 */}
      <div className="flex justify-between items-center">
        <h1 className="text-lg font-bold">{t("title")}</h1>
        <button
          onClick={() => refetch()}
          className="flex items-center gap-1 border rounded px-2 py-1 text-sm hover:bg-gray-100"
          title={t("common:refresh")}
        >
          <RefreshCw size={14} className={loading ? "animate-spin" : ""} /> {t("common:refresh")}
        </button>
      </div>

      <Loadable
        loading={loading}
        error={error}
        data={data}
        isEmpty={(d) => d.channels.length === 0}
        emptyText={t("empty")}
      >
        {(d) => (
          <div className="space-y-2">
            {d.channels.map((ch) => (
              <div key={ch.name} className="bg-white border rounded-lg p-4 flex items-center gap-4">
                <span className={`w-3 h-3 rounded-full ${ch.running ? "bg-green-500" : ch.enabled ? "bg-yellow-400" : "bg-gray-300"}`} />
                <div className="flex-1">
                  <div className="font-medium text-sm">{ch.name}</div>
                  <div className="text-xs text-gray-500 flex gap-3">
                    <span>{ch.enabled ? t("common:enabled") : t("common:disabled")}</span>
                    {ch.group_policy && <span>{t("groupPolicy", { policy: ch.group_policy })}</span>}
                    {ch.allow_from_count != null && ch.allow_from_count > 0 && (
                      <span>{t("allowFrom", { count: ch.allow_from_count })}</span>
                    )}
                  </div>
                </div>
                <span className={`text-xs px-2 py-0.5 rounded ${ch.running ? "bg-green-100 text-green-700" : ch.enabled ? "bg-yellow-100 text-yellow-700" : "bg-gray-100 text-gray-500"}`}>
                  {ch.running ? t("common:online") : ch.enabled ? t("notConnected") : t("common:offline")}
                </span>
                <div className="flex items-center gap-1">
                  {ch.enabled && <button onClick={() => lifecycle(ch, ch.running ? "restart" : "start")}
                    disabled={!canWrite || inflight !== null}
                    aria-label={t(ch.running ? "restartAria" : "startAria", { name: ch.name })}
                    className="p-1.5 rounded hover:bg-gray-100 disabled:opacity-40">
                    {ch.running ? <RotateCw size={15} className={inflight === `${ch.name}:restart` ? "animate-spin" : ""} /> : <Power size={15} />}
                  </button>}
                  <button role="switch" aria-checked={ch.enabled} onClick={() => toggleEnabled(ch)}
                    disabled={!canWrite || inflight !== null}
                    aria-label={t(ch.enabled ? "disableAria" : "enableAria", { name: ch.name })}
                    className={`w-10 h-5 rounded-full transition-colors disabled:opacity-40 ${ch.enabled ? "bg-blue-600" : "bg-gray-300"}`}>
                    <div className={`w-4 h-4 bg-white rounded-full shadow transition-transform ${ch.enabled ? "translate-x-5" : "translate-x-0.5"}`} />
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </Loadable>
    </div>
  );
}
