import { useState } from "react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { Trash2, Search } from "lucide-react";

const TIERS = ["working", "episodic", "semantic", "archival"] as const;

interface MemoryEntry {
  id: string;
  content: string;
  type: string;
  tier: string;
  weight: number;
  created_at: string;
}

export function Memory() {
  const [tier, setTier] = useState<string>("working");
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<MemoryEntry[] | null>(null);

  const { data, refetch } = useApi<{ entries: MemoryEntry[]; total: number }>(`/memory?tier=${tier}&limit=100`);

  const handleSearch = async () => {
    if (!searchQuery.trim()) { setSearchResults(null); return; }
    const res = await apiFetch<{ results: MemoryEntry[] }>("/memory/search", {
      method: "POST",
      body: JSON.stringify({ query: searchQuery, limit: 20 }),
    });
    setSearchResults(res.results);
  };

  const handleDelete = async (id: string) => {
    await apiFetch(`/memory/${id}`, { method: "DELETE" });
    refetch();
  };

  const entries = searchResults ?? data?.entries ?? [];

  return (
    <div className="space-y-4">
      <div className="flex gap-2 items-center">
        <input
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleSearch()}
          placeholder="语义搜索..."
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

      <div className="space-y-2">
        {entries.map((entry) => (
          <div key={entry.id} className="bg-white border rounded-lg p-4 flex justify-between items-start">
            <div className="flex-1">
              <div className="text-sm">{entry.content}</div>
              <div className="text-xs text-gray-400 mt-1">
                {entry.type} · weight: {entry.weight?.toFixed(2)} · {entry.created_at}
              </div>
            </div>
            <button onClick={() => handleDelete(entry.id)} className="text-red-400 hover:text-red-600 ml-2">
              <Trash2 size={16} />
            </button>
          </div>
        ))}
        {entries.length === 0 && <div className="text-gray-400 text-center py-8">无记忆条目</div>}
      </div>
    </div>
  );
}
