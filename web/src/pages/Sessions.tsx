import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { formatDistanceToNow } from "date-fns";
import { zhCN } from "date-fns/locale";

interface SessionItem {
  key: string;
  message_count: number;
  last_active: string;
}

interface Message {
  role: string;
  content: string;
}

export function Sessions() {
  const { data } = useApi<{ sessions: SessionItem[] }>("/sessions");
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [search, setSearch] = useState("");

  const loadHistory = async (key: string) => {
    setSelected(key);
    const res = await apiFetch<{ messages: Message[] }>(`/sessions/${encodeURIComponent(key)}/history`);
    setMessages(res.messages);
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
                {s.message_count} 条 · {formatDistanceToNow(new Date(s.last_active), { locale: zhCN, addSuffix: true })}
              </div>
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto space-y-3">
        {messages.map((msg, i) => (
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
