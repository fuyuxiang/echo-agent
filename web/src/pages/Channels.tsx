import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { Loadable } from "../components/Loadable";
import { RefreshCw } from "lucide-react";

interface Channel {
  name: string;
  // 后端 channels.py 返回 name/enabled/running,没有 type 字段;之前前端读 ch.type
  // 恒为 undefined 导致副标题永远空白。改用 enabled 展示配置状态。
  enabled: boolean;
  running: boolean;
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
                <span className={`w-3 h-3 rounded-full ${ch.running ? "bg-green-500" : "bg-gray-300"}`} />
                <div className="flex-1">
                  <div className="font-medium text-sm">{ch.name}</div>
                  <div className="text-xs text-gray-500">{ch.enabled ? t("common:enabled") : t("common:disabled")}</div>
                </div>
                <span className={`text-xs px-2 py-0.5 rounded ${ch.running ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-500"}`}>
                  {ch.running ? t("common:online") : t("common:offline")}
                </span>
              </div>
            ))}
          </div>
        )}
      </Loadable>
    </div>
  );
}
