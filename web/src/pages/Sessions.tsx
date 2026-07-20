import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { formatDistanceToNow } from "date-fns";
import { zhCN } from "date-fns/locale";

interface SessionItem {
  key: string;
  message_count: number;
  // 后端(storage/sqlite.py、session/manager.py)返回的是 updated_at,
  // 不存在 last_active —— 用它做“最近活跃”时间。
  updated_at: string;
}

interface Message {
  role: string;
  content: string;
}

// 时间可能缺失或非法,formatDistanceToNow 遇到 Invalid Date 会抛 RangeError
// 导致整个列表 render 崩溃,这里统一兜底。
function formatLastActive(value: string | undefined): string {
  if (!value) return "时间未知";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "时间未知";
  return formatDistanceToNow(date, { locale: zhCN, addSuffix: true });
}

export function Sessions() {
  const { data, loading, error } = useApi<{ sessions: SessionItem[] }>("/sessions");
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [search, setSearch] = useState("");
  const [historyError, setHistoryError] = useState<string | null>(null);

  const loadHistory = async (key: string) => {
    setSelected(key);
    setHistoryError(null);
    try {
      const res = await apiFetch<{ messages: Message[] }>(`/sessions/${encodeURIComponent(key)}/history`);
      setMessages(res.messages);
    } catch (e: unknown) {
      setMessages([]);
      setHistoryError(e instanceof Error ? e.message : String(e));
    }
  };

  const filtered = data?.sessions.filter(
    (s) => !search || s.key.toLowerCase().includes(search.toLowerCase())
  ) ?? [];

  return (
    <div className="flex h-full gap-4">
      <div className="w-72 flex flex-col border-r pr-4">
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="搜索会话..."
          className="border rounded px-3 py-1.5 mb-3"
        />
        <div className="flex-1 overflow-y-auto space-y-1">
          {loading && <div className="text-gray-400 text-sm px-3 py-2">加载中...</div>}
          {error && !loading && (
            <div className="text-red-500 text-sm px-3 py-2">加载失败：{error}</div>
          )}
          {!loading && !error && filtered.length === 0 && (
            <div className="text-gray-400 text-sm px-3 py-2">暂无会话</div>
          )}
          {filtered.map((s) => (
            <button
              key={s.key}
              onClick={() => loadHistory(s.key)}
              className={`w-full text-left px-3 py-2 rounded text-sm ${
                selected === s.key ? "bg-blue-50 text-blue-700" : "hover:bg-gray-100"
              }`}
            >
              <div className="font-medium truncate">{s.key}</div>
              <div className="text-xs text-gray-500">
                {s.message_count} 条 · {formatLastActive(s.updated_at)}
              </div>
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto space-y-3">
        {historyError && (
          <div className="text-red-500 text-sm text-center mt-20">加载历史失败：{historyError}</div>
        )}
        {!historyError && messages.map((msg, i) => (
          <div key={i} className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}>
            <div
              className={`max-w-[70%] rounded-lg px-4 py-2 text-sm ${
                msg.role === "user" ? "bg-blue-600 text-white" : "bg-gray-100 text-gray-800"
              }`}
            >
              {msg.content}
            </div>
          </div>
        ))}
        {!selected && <div className="text-gray-400 text-center mt-20">选择一个会话查看历史</div>}
      </div>
    </div>
  );
}
