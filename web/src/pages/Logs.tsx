import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { fullTimestamp, timeOfDay } from "../lib/datetime";
import { RefreshCw } from "lucide-react";

const LEVELS = ["DEBUG", "INFO", "WARNING", "ERROR"] as const;

interface LogEntry {
  ts: string;
  level: string;
  message: string;
}

export function Logs() {
  const { t } = useTranslation(["logs", "common"]);
  const [level, setLevel] = useState<string>("");
  const [search, setSearch] = useState("");

  // 实时推送(WS log_entry 事件)后端从未接线,此前的“实时”开关是死功能,已移除;
  // 改为拉取 + 手动刷新。待 dashboard WS broadcast 接线后再恢复实时。
  // 后端已改为倒序分页,offset=0 即最新一页,这里直接按返回顺序渲染。
  const { data, loading, error, refetch } = useApi<{ logs: LogEntry[] }>(
    `/logs?limit=200${level ? `&level=${level}` : ""}${search ? `&q=${encodeURIComponent(search)}` : ""}`
  );

  const entries = data?.logs ?? [];

  const levelColor: Record<string, string> = {
    DEBUG: "text-gray-400", INFO: "text-blue-600", WARNING: "text-yellow-600", ERROR: "text-red-600",
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex flex-wrap gap-2 items-center mb-3">
        <select
          value={level}
          onChange={(e) => setLevel(e.target.value)}
          aria-label={t("allLevels")}
          className="border rounded px-2 py-1 text-sm"
        >
          <option value="">{t("allLevels")}</option>
          {LEVELS.map((l) => <option key={l} value={l}>{l}</option>)}
        </select>
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t("searchPlaceholder")}
          aria-label={t("searchPlaceholder")}
          className="border rounded px-3 py-1 text-sm flex-1 min-w-40"
        />
        <button onClick={() => refetch()} className="flex items-center gap-1 border rounded px-2 py-1 text-sm hover:bg-gray-100" title={t("common:refresh")}>
          <RefreshCw size={14} /> {t("common:refresh")}
        </button>
      </div>

      <div
        role="log"
        aria-live="off"
        className="flex-1 overflow-y-auto bg-gray-900 rounded-lg p-4 font-mono text-xs"
      >
        {error && <div className="text-red-400">{t("common:loadFailed", { error })}</div>}
        {!error && loading && !data && <div className="text-gray-500">{t("common:loading")}</div>}
        {!error && data && entries.length === 0 && <div className="text-gray-500">{t("empty")}</div>}
        {!error && entries.map((entry, i) => (
          <div key={`${entry.ts}-${i}`} className="flex gap-2">
            {/* 时分秒够密集地扫读,完整日期挂在 title 上:此前用 ts.slice(11,19)
                手工切串,既假设了固定 ISO 版式,也让跨天排查分不清是哪一天。 */}
            <span className="text-gray-500 shrink-0" title={fullTimestamp(entry.ts)}>
              {timeOfDay(entry.ts)}
            </span>
            <span className={`shrink-0 w-14 ${levelColor[entry.level] || ""}`}>{entry.level}</span>
            <span className="text-gray-200 whitespace-pre-wrap break-words">{entry.message}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
