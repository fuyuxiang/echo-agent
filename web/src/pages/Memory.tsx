import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { runMutation } from "../stores/toast";
import { Loadable } from "../components/Loadable";
import { Trash2, Search } from "lucide-react";

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
  const { t } = useTranslation("memory");
  const [tier, setTier] = useState<string>("working");
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<MemoryEntry[] | null>(null);

  const { data, loading, error, refetch } = useApi<{ entries: MemoryEntry[]; total: number }>(
    `/memory?tier=${tier}&limit=100`
  );

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

  const handleDelete = async (id: string) => {
    const ok = await runMutation(() => apiFetch(`/memory/${id}`, { method: "DELETE" }), {
      success: t("deleteSuccess"), error: t("deleteFailed"),
    });
    if (ok) {
      // 搜索结果视图下本地剔除,列表视图下重新拉取。
      if (searchResults) setSearchResults((prev) => prev?.filter((e) => e.id !== id) ?? null);
      else refetch();
    }
  };

  const renderEntries = (entries: MemoryEntry[]) => (
    <div className="space-y-2">
      {entries.length === 0 && <div className="text-gray-400 text-center py-8">{t("empty")}</div>}
      {entries.map((entry) => (
        <div key={entry.id} className="bg-white border rounded-lg p-4 flex justify-between items-start">
          <div className="flex-1">
            <div className="text-sm">{entry.content}</div>
            <div className="text-xs text-gray-400 mt-1">
              {entry.type} · weight: {entry.importance?.toFixed(2) ?? "-"} · {entry.created_at}
            </div>
          </div>
          <button onClick={() => handleDelete(entry.id)} className="text-red-400 hover:text-red-600 ml-2">
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
          className="border rounded px-3 py-1.5 flex-1"
        />
        <button onClick={handleSearch} className="p-2 bg-gray-100 rounded hover:bg-gray-200">
          <Search size={18} />
        </button>
      </div>

      <div className="flex gap-1 border-b">
        {TIERS.map((t) => (
          <button
            key={t}
            onClick={() => { setTier(t); setSearchResults(null); }}
            className={`px-4 py-2 text-sm border-b-2 ${
              tier === t ? "border-blue-600 text-blue-600" : "border-transparent text-gray-500"
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      {searchResults !== null ? (
        renderEntries(searchResults)
      ) : (
        <Loadable loading={loading} error={error} data={data} emptyText={t("empty")}>
          {(d) => renderEntries(d.entries)}
        </Loadable>
      )}
    </div>
  );
}
