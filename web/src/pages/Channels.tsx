import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { Loadable } from "../components/Loadable";
import { RefreshCw } from "lucide-react";

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
              </div>
            ))}
          </div>
        )}
      </Loadable>
    </div>
  );
}
