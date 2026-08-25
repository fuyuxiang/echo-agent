import { useState, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { relativeTime } from "../lib/datetime";
import { useIsAdmin } from "../stores/capabilities";
import { RefreshCw } from "lucide-react";

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
  // 后端展示视图对工具调用 / 工具结果打的标记(session/manager.py:
  // display_messages)。它们确实发生过、也值得能看,但不是对话轮次 —— 以前
  // 全部按“非 user 即 Agent 气泡”渲染,工具输出被当成 Agent 说的话。
  internal?: boolean;
  // 仅 internal 条目有:被调用的工具名。
  name?: string;
}

export function Sessions() {
  const { t } = useTranslation(["sessions", "common"]);
  const { data, loading, error, refetch } = useApi<{ sessions: SessionItem[] }>("/sessions");
  const isAdmin = useIsAdmin();
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [search, setSearch] = useState("");
  const [historyError, setHistoryError] = useState<string | null>(null);
  const seqRef = useRef(0);

  const loadHistory = async (key: string) => {
    setSelected(key);
    setHistoryError(null);
    const seq = ++seqRef.current;
    try {
      const res = await apiFetch<{ messages: Message[] }>(`/sessions/${encodeURIComponent(key)}/history`);
      if (seq !== seqRef.current) return;
      setMessages(res.messages);
    } catch (e: unknown) {
      if (seq !== seqRef.current) return;
      setMessages([]);
      setHistoryError(e instanceof Error ? e.message : String(e));
    }
  };

  // 会话列表与当前会话历史都没有实时推送(sessions 频道后端未接线),而聊天在
  // 其他渠道持续发生 —— 没有刷新入口的话,这一页只能靠切走再切回来更新。
  const refreshAll = () => {
    refetch();
    if (selected) loadHistory(selected);
  };

  const filtered = data?.sessions.filter(
    (s) => !search || s.key.toLowerCase().includes(search.toLowerCase())
  ) ?? [];

  return (
    <div className="flex flex-col md:flex-row h-full gap-4">
      <div className="w-full md:w-72 md:shrink-0 flex flex-col md:border-r md:pr-4 max-h-64 md:max-h-none">
        <div className="flex gap-2 mb-3">
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder={t("searchPlaceholder")}
            aria-label={t("searchPlaceholder")}
            className="border rounded px-3 py-1.5 flex-1 min-w-0"
          />
          <button
            onClick={refreshAll}
            aria-label={t("common:refresh")}
            title={t("common:refresh")}
            className="border rounded px-2 hover:bg-gray-100 shrink-0"
          >
            <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto space-y-1">
          {loading && <div className="text-gray-400 text-sm px-3 py-2">{t("common:loading")}</div>}
          {error && !loading && (
            <div className="text-red-500 text-sm px-3 py-2">
              {isAdmin === false || String(error).includes("403")
                ? t("common:adminOnly")
                : t("common:loadFailed", { error })}
            </div>
          )}
          {!loading && !error && filtered.length === 0 && (
            <div className="text-gray-400 text-sm px-3 py-2">{t("empty")}</div>
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
                {t("messageCount", { count: s.message_count, time: relativeTime(s.updated_at, t("unknownTime")) })}
              </div>
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 min-w-0 overflow-y-auto space-y-3">
        {historyError && (
          <div className="text-red-500 text-sm text-center mt-20">
            {String(historyError).includes("403")
              ? t("common:adminOnly")
              : t("historyFailed", { error: historyError })}
          </div>
        )}
        {!historyError && messages.map((msg, i) => (
          msg.internal ? (
            /* 工具调用与工具结果:折叠成一条居中的细节行,默认收起。既不冒充
               对话气泡,也不丢失 —— 排查问题时这些恰恰是最需要看的内容。 */
            <details key={i} className="mx-auto w-full max-w-[90%] text-xs">
              <summary className="cursor-pointer text-gray-500 hover:text-gray-700 py-1">
                {msg.role === "tool"
                  ? t("toolResult", { name: msg.name || t("unknownTool") })
                  : t("toolCall", { name: msg.name || t("unknownTool") })}
              </summary>
              <div className="whitespace-pre-wrap break-words bg-gray-50 border border-gray-200 rounded p-2 mt-1 text-gray-700 font-mono">
                {msg.content || t("emptyContent")}
              </div>
            </details>
          ) : (
            <div key={i} className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}>
              <div
                className={`max-w-[85%] md:max-w-[70%] rounded-lg px-4 py-2 text-sm ${
                  msg.role === "user" ? "bg-blue-600 text-white" : "bg-gray-100 text-gray-800"
                }`}
              >
                {/* Agent 回复通常是 Markdown(代码块、列表)。这里不引入渲染器,但
                    pre-wrap + break-words 至少保住换行与长行折叠——此前长代码块挤成
                    一行且不换行,基本没法读。 */}
                <div className="whitespace-pre-wrap break-words">{msg.content}</div>
              </div>
            </div>
          )
        ))}
        {!selected && <div className="text-gray-400 text-center mt-20">{t("selectHint")}</div>}
      </div>
    </div>
  );
}
