import { useState, useRef, useEffect } from "react";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";

const LEVELS = ["DEBUG", "INFO", "WARNING", "ERROR"] as const;

interface LogEntry {
  ts: string;
  level: string;
  message: string;
}

export function Logs() {
  const [level, setLevel] = useState<string>("");
  const [search, setSearch] = useState("");
  const [live, setLive] = useState(true);
  const [liveEntries, setLiveEntries] = useState<LogEntry[]>([]);
  const bottomRef = useRef<HTMLDivElement>(null);

  const { data } = useApi<{ logs: LogEntry[] }>(`/logs?limit=200${level ? `&level=${level}` : ""}${search ? `&q=${search}` : ""}`);

  useWsSubscribe(["logs"], (ev) => {
    if (ev.type === "log_entry" && live) {
      setLiveEntries((prev) => [...prev.slice(-500), ev.payload]);
    }
  }, ["log_entry"]);

  useEffect(() => {
    if (live) bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [liveEntries, live]);

  const entries = live ? [...(data?.logs ?? []), ...liveEntries] : (data?.logs ?? []);

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
        <label className="flex items-center gap-1 text-sm">
          <input type="checkbox" checked={live} onChange={(e) => setLive(e.target.checked)} />
          实时
        </label>
      </div>

      <div className="flex-1 overflow-y-auto bg-gray-900 rounded-lg p-4 font-mono text-xs">
        {entries.map((entry, i) => (
          <div key={i} className="flex gap-2">
            <span className="text-gray-500 shrink-0">{entry.ts?.slice(11, 19)}</span>
            <span className={`shrink-0 w-14 ${levelColor[entry.level] || ""}`}>{entry.level}</span>
            <span className="text-gray-200">{entry.message}</span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
