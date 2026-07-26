import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { relativeTime } from "../lib/datetime";
import { runMutation } from "../stores/toast";
import { Loadable } from "../components/Loadable";
import { useConfirm } from "../components/ConfirmDialog";
import { Trash2, Search, X } from "lucide-react";

const TIERS = ["working", "episodic", "semantic", "archival"] as const;

interface MemoryEntry {
  id: string;
  content: string;
  type: string;
  tier: string;
  // 后端 MemoryEntry.to_dict() 的字段是 importance,没有 weight;
  // 之前前端读 entry.weight 恒为 undefined。
  importance: number;
  created_at: string;
}

// 搜索接口返回 {results:[{entry, score}]},与 list 的 {entries:[...]} 结构不同。
interface SearchResult {
  entry: MemoryEntry;
  score: number;
}

export function Memory() {
  const { t } = useTranslation(["memory", "common"]);
  const confirm = useConfirm();
  const [tier, setTier] = useState<string>("working");
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<MemoryEntry[] | null>(null);

  const { data, loading, error, refetch } = useApi<{ entries: MemoryEntry[]; total: number }>(
    `/memory?tier=${tier}&limit=100`
  );

  // 搜索是全局的(all_scopes: true),与 tier 分层正交。此前切 tier 会静默清掉搜索
  // 结果、而搜索框里的关键词还留着,用户无法判断当前看的是哪一种视图。改为:搜索
  // 激活时 tier 页签禁用并显式提示“正在看搜索结果”,清除搜索才回到分层浏览。
  const searching = searchResults !== null;

  const handleSearch = async () => {
    if (!searchQuery.trim()) { setSearchResults(null); return; }
    await runMutation(async () => {
      // 管理端全局检索:后端要求带 session_key 或 all_scopes=true(布尔),否则 400。
      const res = await apiFetch<{ results: SearchResult[] }>("/memory/search", {
        method: "POST",
        body: JSON.stringify({ query: searchQuery, limit: 20, all_scopes: true }),
      });
      // 解包 {entry, score} → MemoryEntry[](按相关度已由后端排序)。
      setSearchResults(res.results.map((r) => r.entry));
    }, { error: t("searchFailed") });
  };

  const clearSearch = () => {
    setSearchQuery("");
    setSearchResults(null);
  };

  const handleDelete = async (entry: MemoryEntry) => {
    const confirmed = await confirm({
      title: t("deleteConfirmTitle"),
      message: t("deleteConfirmMessage", { content: entry.content.slice(0, 120) }),
      confirmLabel: t("common:delete"),
      destructive: true,
    });
    if (!confirmed) return;
    const ok = await runMutation(() => apiFetch(`/memory/${entry.id}`, { method: "DELETE" }), {
      success: t("deleteSuccess"), error: t("deleteFailed"),
    });
    if (ok) {
      // 搜索结果视图下本地剔除,列表视图下重新拉取。
      if (searchResults) setSearchResults((prev) => prev?.filter((e) => e.id !== entry.id) ?? null);
      else refetch();
    }
  };

  const renderEntries = (entries: MemoryEntry[]) => (
    <div className="space-y-2">
      {entries.length === 0 && <div className="text-gray-400 text-center py-8">{t("empty")}</div>}
      {entries.map((entry) => (
        <div key={entry.id} className="bg-white border rounded-lg p-4 flex justify-between items-start">
          <div className="flex-1">
            <div className="text-sm whitespace-pre-wrap break-words">{entry.content}</div>
            <div className="text-xs text-gray-400 mt-1">
              {entry.type} · {t("importance")}: {entry.importance?.toFixed(2) ?? "-"} ·{" "}
              {relativeTime(entry.created_at, t("unknownTime"))}
              {searching && entry.tier && <> · {t(`tier.${entry.tier}`, { defaultValue: entry.tier })}</>}
            </div>
          </div>
          <button
            onClick={() => handleDelete(entry)}
            aria-label={t("deleteEntryAria")}
            className="text-red-400 hover:text-red-600 ml-2"
          >
            <Trash2 size={16} />
          </button>
        </div>
      ))}
    </div>
  );

  return (
    <div className="space-y-4">
      <div className="flex gap-2 items-center">
        <input
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleSearch()}
          placeholder={t("searchPlaceholder")}
          aria-label={t("searchPlaceholder")}
          className="border rounded px-3 py-1.5 flex-1"
        />
        <button
          onClick={handleSearch}
          aria-label={t("searchAria")}
          className="p-2 bg-gray-100 rounded hover:bg-gray-200"
        >
          <Search size={18} />
        </button>
      </div>

      <div className="flex gap-1 border-b items-center">
        {TIERS.map((tierKey) => (
          <button
            key={tierKey}
            onClick={() => setTier(tierKey)}
            disabled={searching}
            title={searching ? t("tierDisabledHint") : undefined}
            className={`px-4 py-2 text-sm border-b-2 disabled:opacity-40 disabled:cursor-not-allowed ${
              tier === tierKey ? "border-blue-600 text-blue-600" : "border-transparent text-gray-500"
            }`}
          >
            {t(`tier.${tierKey}`)}
          </button>
        ))}
      </div>

      {searching && (
        <div className="flex items-center gap-2 text-sm bg-blue-50 border border-blue-200 rounded px-3 py-2">
          <span className="flex-1 text-blue-800">
            {t("searchResultCount", { count: searchResults.length })}
          </span>
          <button
            onClick={clearSearch}
            className="flex items-center gap-1 text-blue-700 hover:text-blue-900"
          >
            <X size={14} /> {t("clearSearch")}
          </button>
        </div>
      )}

      {searching ? (
        renderEntries(searchResults)
      ) : (
        <Loadable loading={loading} error={error} data={data} emptyText={t("empty")}>
          {(d) => renderEntries(d.entries)}
        </Loadable>
      )}
    </div>
  );
}
