import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
import { fullTimestamp, timeOfDay } from "../lib/datetime";
import { RefreshCw, ChevronLeft, ChevronRight } from "lucide-react";

const LEVELS = ["DEBUG", "INFO", "WARNING", "ERROR"] as const;
const PAGE_SIZES = [50, 100, 200, 500] as const;

interface LogEntry {
  ts: string;
  level: string;
  message: string;
}

export function Logs() {
  const { t } = useTranslation(["logs", "common"]);
  const [level, setLevel] = useState<string>("");
  const [search, setSearch] = useState("");
  const [debouncedSearch, setDebouncedSearch] = useState("");
  const [live, setLive] = useState(true);
  const [pageSize, setPageSize] = useState<number>(200);
  const [offset, setOffset] = useState(0);
  const [liveRefreshPending, setLiveRefreshPending] = useState(false);
  const searchPending = search !== debouncedSearch;
  const liveActive = live && offset === 0 && !searchPending;

  useEffect(() => {
    // Only commit an actual query change. A timer on initial mount otherwise
    // sends users back to page one if they paginate during the first 300 ms.
    if (search === debouncedSearch) return;
    const timer = window.setTimeout(() => { setDebouncedSearch(search); setOffset(0); }, 300);
    return () => window.clearTimeout(timer);
  }, [search, debouncedSearch]);

  // 后端已改为倒序分页,offset=0 即最新一页,这里直接按返回顺序渲染。
  // offset 分页此前完全没接:limit 固定 200,缓冲区里更早的日志在界面上无法到达,
  // 而 total 一直在响应里返回着。
  const { data, loading, error, refetch } = useApi<{ logs: LogEntry[]; total: number }>(
    `/logs?limit=${pageSize}&offset=${offset}${level ? `&level=${level}` : ""}${debouncedSearch ? `&q=${encodeURIComponent(debouncedSearch)}` : ""}`
  );
  // Pending events belong to this query and live mode. A new query already
  // fetches its own snapshot; old events must not trigger an old refetch later.
  useEffect(() => { setLiveRefreshPending(false); }, [refetch, liveActive]);

  useWsSubscribe(["logs"], () => {
    if (liveActive) setLiveRefreshPending(true);
  }, ["log_entry"]);
  useEffect(() => {
    // Keep events received during a slow request, then refresh once it settles.
    // Repeated events share one timer instead of aborting the in-flight request
    // or continually postponing the refresh under a steady stream of logs.
    if (!liveActive || !liveRefreshPending || loading) return;
    const timer = window.setTimeout(() => {
      setLiveRefreshPending(false);
      void refetch();
    }, 500);
    return () => window.clearTimeout(timer);
  }, [liveActive, liveRefreshPending, loading, refetch]);

  const refresh = () => { setLiveRefreshPending(false); void refetch(); };

  const entries = data?.logs ?? [];
  const total = data?.total ?? 0;

  // 改筛选条件/页大小后必须回到第一页:否则 offset 可能已经越过新的结果集,
  // 界面会莫名空白。
  const changeFilter = (fn: () => void) => { fn(); setOffset(0); };

  const levelColor: Record<string, string> = {
    DEBUG: "text-gray-400", INFO: "text-blue-600", WARNING: "text-yellow-600", ERROR: "text-red-600",
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex flex-wrap gap-2 items-center mb-3">
        <select
          value={level}
          onChange={(e) => changeFilter(() => setLevel(e.target.value))}
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
        <select
          value={pageSize}
          onChange={(e) => changeFilter(() => setPageSize(Number(e.target.value)))}
          aria-label={t("pageSize")}
          className="border rounded px-2 py-1 text-sm"
        >
          {PAGE_SIZES.map((n) => <option key={n} value={n}>{n}</option>)}
        </select>
        <button onClick={refresh} disabled={loading || searchPending} className="flex items-center gap-1 border rounded px-2 py-1 text-sm hover:bg-gray-100 disabled:opacity-40 disabled:cursor-not-allowed" title={t("common:refresh")}>
          <RefreshCw size={14} className={loading ? "animate-spin" : undefined} /> {t("common:refresh")}
        </button>
        <label className="flex items-center gap-1 text-xs text-gray-600 select-none">
          <input type="checkbox" checked={live} onChange={(event) => {
            const enabled = event.target.checked;
            setLive(enabled);
            if (enabled && offset === 0 && !searchPending) refresh();
          }} /> {t("live")}
        </label>
        {live && offset !== 0 && <span role="status" className="text-xs text-gray-500">{t("livePaused")}</span>}
      </div>

      <div
        role="log"
        aria-live="off"
        aria-busy={loading || searchPending}
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

      {!error && total > 0 && (
        <div className="flex items-center justify-end gap-2 mt-2 text-xs text-gray-500">
          {loading ? <span>{t("common:loading")}</span> : <span>
            {t("range", {
              from: offset + 1,
              to: Math.min(offset + entries.length, total),
              total,
            })}
          </span>}
          <button
            onClick={() => setOffset(Math.max(0, offset - pageSize))}
            disabled={loading || searchPending || offset === 0}
            className="flex items-center gap-0.5 border rounded px-2 py-1 hover:bg-gray-100 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <ChevronLeft size={14} /> {t("prev")}
          </button>
          <button
            onClick={() => setOffset(offset + pageSize)}
            disabled={loading || searchPending || offset + entries.length >= total}
            className="flex items-center gap-0.5 border rounded px-2 py-1 hover:bg-gray-100 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {t("next")} <ChevronRight size={14} />
          </button>
        </div>
      )}
    </div>
  );
}
