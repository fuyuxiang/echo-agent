import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { Loadable } from "../components/Loadable";

interface Channel {
  name: string;
  // 后端 channels.py 返回 name/enabled/running,没有 type 字段;之前前端读 ch.type
  // 恒为 undefined 导致副标题永远空白。改用 enabled 展示配置状态。
  enabled: boolean;
  running: boolean;
}

export function Channels() {
  const { t } = useTranslation(["channels", "common"]);
  const { data, loading, error } = useApi<{ channels: Channel[] }>("/channels");

  return (
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
  );
}
