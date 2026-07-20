import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { RefreshCw } from "lucide-react";

const LEVELS = ["DEBUG", "INFO", "WARNING", "ERROR"] as const;

interface LogEntry {
  ts: string;
  level: string;
  message: string;
}

export function Logs() {
  const [level, setLevel] = useState<string>("");
  const [search, setSearch] = useState("");

  // 实时推送(WS log_entry 事件)后端从未接线,此前的“实时”开关是死功能,已移除;
  // 改为拉取 + 手动刷新。待 dashboard WS broadcast 接线后再恢复实时。
  const { data, loading, error, refetch } = useApi<{ logs: LogEntry[] }>(
    `/logs?limit=200${level ? `&level=${level}` : ""}${search ? `&q=${search}` : ""}`
  );

  const entries = data?.logs ?? [];

  const levelColor: Record<string, string> = {
    DEBUG: "text-gray-400", INFO: "text-blue-600", WARNING: "text-yellow-600", ERROR: "text-red-600",
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex gap-2 items-center mb-3">
        <select value={level} onChange={(e) => setLevel(e.target.value)} className="border rounded px-2 py-1 text-sm">
          <option value="">全部级别</option>
          {LEVELS.map((l) => <option key={l} value={l}>{l}</option>)}
        </select>
        <input value={search} onChange={(e) => setSearch(e.target.value)} placeholder="搜索..." className="border rounded px-3 py-1 text-sm flex-1" />
        <button onClick={() => refetch()} className="flex items-center gap-1 border rounded px-2 py-1 text-sm hover:bg-gray-100" title="刷新">
          <RefreshCw size={14} /> 刷新
        </button>
      </div>

      <div className="flex-1 overflow-y-auto bg-gray-900 rounded-lg p-4 font-mono text-xs">
        {error && <div className="text-red-400">加载失败：{error}</div>}
        {!error && loading && !data && <div className="text-gray-500">加载中...</div>}
        {!error && data && entries.length === 0 && <div className="text-gray-500">暂无日志</div>}
        {entries.map((entry, i) => (
          <div key={i} className="flex gap-2">
            <span className="text-gray-500 shrink-0">{entry.ts?.slice(11, 19)}</span>
            <span className={`shrink-0 w-14 ${levelColor[entry.level] || ""}`}>{entry.level}</span>
            <span className="text-gray-200">{entry.message}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
