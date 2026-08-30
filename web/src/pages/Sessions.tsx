import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCw, RotateCcw, ChevronLeft, ChevronRight, History } from "lucide-react";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
import { apiFetch } from "../lib/api";
import { relativeTime, dateTime } from "../lib/datetime";
import { useIsAdmin } from "../stores/capabilities";
import { useConfirm } from "../components/ConfirmDialog";
import { runMutation } from "../stores/toast";

interface SessionItem { key: string; message_count: number; updated_at: string }
interface Message { role: string; content: string; internal?: boolean; name?: string }
interface TurnRun {
  event_id: string; status: string; current_tool: string; error: string;
  created_at: string; completed_at: string; session_key: string;
}

const PAGE_SIZE = 50;
const HISTORY_SIZE = 100;

export function Sessions() {
  const { t } = useTranslation(["sessions", "common"]);
  const confirm = useConfirm();
  const isAdmin = useIsAdmin();
  const [listOffset, setListOffset] = useState(0);
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(search.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [search]);
  const searchParam = query ? `&q=${encodeURIComponent(query)}` : "";
  const { data, loading, error, refetch } = useApi<{
    sessions: SessionItem[]; total: number; has_more: boolean;
  }>(`/sessions?limit=${PAGE_SIZE}&offset=${listOffset}${searchParam}`);
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [turns, setTurns] = useState<TurnRun[]>([]);
  const [hasOlder, setHasOlder] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const seqRef = useRef(0);

  const loadTurns = async (key: string) => {
    try {
      const result = await apiFetch<{ turns: TurnRun[] }>(
        `/sessions/${encodeURIComponent(key)}/turns?limit=30`,
      );
      setTurns(result.turns);
    } catch { setTurns([]); }
  };

  const loadHistory = async (key: string) => {
    setSelected(key); setHistoryError(null); setHistoryLoading(true);
    const seq = ++seqRef.current;
    try {
      const result = await apiFetch<{ messages: Message[]; has_more: boolean }>(
        `/sessions/${encodeURIComponent(key)}/history?limit=${HISTORY_SIZE}&offset=0`,
      );
      if (seq !== seqRef.current) return;
      setMessages(result.messages); setHasOlder(result.has_more);
      await loadTurns(key);
    } catch (e: unknown) {
      if (seq !== seqRef.current) return;
      setMessages([]); setTurns([]);
      setHistoryError(e instanceof Error ? e.message : String(e));
    } finally { if (seq === seqRef.current) setHistoryLoading(false); }
  };

  const loadOlder = async () => {
    if (!selected || historyLoading || !hasOlder) return;
    setHistoryLoading(true);
    try {
      const result = await apiFetch<{ messages: Message[]; has_more: boolean }>(
        `/sessions/${encodeURIComponent(selected)}/history?limit=${HISTORY_SIZE}&offset=${messages.length}`,
      );
      setMessages((current) => [...result.messages, ...current]);
      setHasOlder(result.has_more);
    } catch (e: unknown) { setHistoryError(e instanceof Error ? e.message : String(e)); }
    finally { setHistoryLoading(false); }
  };

  const refreshAll = () => { refetch(); if (selected) loadHistory(selected); };

  useWsSubscribe(["sessions"], (event) => {
    refetch();
    const payload = event.payload as Partial<TurnRun>;
    if (selected && payload?.session_key === selected) {
      loadTurns(selected);
      if (event.type === "session_reset" || ["completed", "incomplete", "failed", "interrupted"].includes(payload.status ?? "")) {
        loadHistory(selected);
      }
    }
  }, ["session_turn_updated", "session_reset"]);

  const resetSession = async () => {
    if (!selected) return;
    const ok = await confirm({ title: t("resetTitle"), message: t("resetMessage", { key: selected }),
      confirmLabel: t("reset"), destructive: true });
    if (!ok) return;
    const done = await runMutation(
      () => apiFetch(`/sessions/${encodeURIComponent(selected)}`, { method: "DELETE" }),
      { success: t("resetSuccess"), error: t("resetFailed") },
    );
    if (done) loadHistory(selected);
  };

  const sessions = data?.sessions ?? [];

  return (
    <div className="flex flex-col md:flex-row h-full gap-4">
      <div className="w-full md:w-72 md:shrink-0 flex flex-col md:border-r md:pr-4 max-h-72 md:max-h-none">
        <div className="flex gap-2 mb-3">
          <input value={search} onChange={(e) => { setSearch(e.target.value); setListOffset(0); }} placeholder={t("searchPlaceholder")}
            aria-label={t("searchPlaceholder")} className="border rounded px-3 py-1.5 flex-1 min-w-0" />
          <button onClick={refreshAll} aria-label={t("common:refresh")} title={t("common:refresh")}
            className="border rounded px-2 hover:bg-gray-100 shrink-0">
            <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto space-y-1">
          {loading && <div className="text-gray-400 text-sm px-3 py-2">{t("common:loading")}</div>}
          {error && !loading && <div className="text-red-500 text-sm px-3 py-2">
            {isAdmin === false || String(error).includes("403") ? t("common:adminOnly") : t("common:loadFailed", { error })}
          </div>}
          {!loading && !error && sessions.length === 0 && <div className="text-gray-400 text-sm px-3 py-2">{t("empty")}</div>}
          {sessions.map((session) => <button key={session.key} onClick={() => loadHistory(session.key)}
            className={`w-full text-left px-3 py-2 rounded text-sm ${selected === session.key ? "bg-blue-50 text-blue-700" : "hover:bg-gray-100"}`}>
            <div className="font-medium truncate">{session.key}</div>
            <div className="text-xs text-gray-500">{t("messageCount", { count: session.message_count,
              time: relativeTime(session.updated_at, t("unknownTime")) })}</div>
          </button>)}
        </div>
        {(data?.total ?? 0) > PAGE_SIZE && <div className="flex justify-between items-center pt-2 text-xs text-gray-500">
          <button disabled={listOffset === 0} onClick={() => setListOffset(Math.max(0, listOffset - PAGE_SIZE))}
            className="p-1 disabled:opacity-30" aria-label={t("common:previous")}><ChevronLeft size={16} /></button>
          <span>{listOffset + 1}-{Math.min(listOffset + sessions.length, data?.total ?? 0)} / {data?.total}</span>
          <button disabled={!data?.has_more} onClick={() => setListOffset(listOffset + PAGE_SIZE)}
            className="p-1 disabled:opacity-30" aria-label={t("common:next")}><ChevronRight size={16} /></button>
        </div>}
      </div>

      <div className="flex-1 min-w-0 flex flex-col overflow-hidden">
        {selected && <div className="flex items-center gap-2 border-b pb-2 mb-2">
          <div className="min-w-0 flex-1"><div className="font-medium text-sm truncate">{selected}</div>
            <div className="text-xs text-gray-400">{t("turnCount", { count: turns.length })}</div></div>
          <button onClick={resetSession} disabled={isAdmin === false}
            className="flex items-center gap-1 text-xs border rounded px-2 py-1 text-red-600 hover:bg-red-50 disabled:opacity-40">
            <RotateCcw size={13} /> {t("reset")}
          </button>
        </div>}
        <div className="flex-1 overflow-y-auto space-y-3 pr-1">
          {selected && hasOlder && <button onClick={loadOlder} disabled={historyLoading}
            className="mx-auto flex items-center gap-1 text-xs text-blue-600 hover:underline disabled:opacity-50">
            <History size={13} /> {historyLoading ? t("common:loading") : t("loadOlder")}
          </button>}
          {historyError && <div className="text-red-500 text-sm text-center mt-10">
            {String(historyError).includes("403") ? t("common:adminOnly") : t("historyFailed", { error: historyError })}
          </div>}
          {!historyError && messages.map((message, index) => message.internal ? (
            <details key={index} className="mx-auto w-full max-w-[90%] text-xs">
              <summary className="cursor-pointer text-gray-500 hover:text-gray-700 py-1">
                {message.role === "tool" ? t("toolResult", { name: message.name || t("unknownTool") }) : t("toolCall", { name: message.name || t("unknownTool") })}
              </summary>
              <div className="whitespace-pre-wrap break-words bg-gray-50 border rounded p-2 mt-1 font-mono">{message.content || t("emptyContent")}</div>
            </details>
          ) : <div key={index} className={`flex ${message.role === "user" ? "justify-end" : "justify-start"}`}>
            <div className={`max-w-[85%] md:max-w-[75%] rounded-lg px-4 py-2 text-sm ${message.role === "user" ? "bg-blue-600 text-white" : "bg-gray-100 text-gray-800"}`}>
              <MarkdownContent content={message.content} />
            </div>
          </div>)}
          {!selected && <div className="text-gray-400 text-center mt-20">{t("selectHint")}</div>}
        </div>
        {selected && turns.length > 0 && <details className="border-t pt-2 mt-2 text-xs">
          <summary className="cursor-pointer text-gray-500">{t("turnLedger")}</summary>
          <div className="mt-2 max-h-40 overflow-y-auto space-y-1">{turns.map((turn) =>
            <div key={turn.event_id} className="flex gap-2 items-start bg-gray-50 rounded p-2">
              <span className={`rounded px-1.5 ${turn.status === "completed" ? "bg-green-100 text-green-700" : turn.status === "failed" ? "bg-red-100 text-red-700" : "bg-blue-100 text-blue-700"}`}>{turn.status}</span>
              <span className="flex-1 truncate">{turn.current_tool || turn.error || turn.event_id}</span>
              <span className="text-gray-400 shrink-0">{dateTime(turn.created_at)}</span>
            </div>)}</div>
        </details>}
      </div>
    </div>
  );
}

function MarkdownContent({ content }: { content: string }) {
  const parts = content.split(/(```[\s\S]*?```)/g);
  return <div className="space-y-2 break-words">{parts.filter(Boolean).map((part, index) => {
    if (part.startsWith("```") && part.endsWith("```")) {
      const body = part.slice(3, -3).replace(/^[^\n]*\n/, "");
      return <pre key={index} className="overflow-x-auto rounded bg-gray-900 text-gray-100 p-3 text-xs"><code>{body}</code></pre>;
    }
    return <div key={index} className="whitespace-pre-wrap">{part}</div>;
  })}</div>;
}
