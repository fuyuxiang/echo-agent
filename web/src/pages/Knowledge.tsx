import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { Upload, Trash2, RefreshCw } from "lucide-react";
import { useRef } from "react";

interface Document {
  path: string;
  size: number;
  indexed: boolean;
}

export function Knowledge() {
  const { data, refetch } = useApi<{ documents: Document[] }>("/knowledge/documents");
  const { data: status } = useApi<{ indexed_count: number; last_rebuild: string }>("/knowledge/status");
  const fileRef = useRef<HTMLInputElement>(null);

  const upload = async (file: File) => {
    const form = new FormData();
    form.append("file", file);
    await fetch("/api/v1/knowledge/upload", {
      method: "POST",
      headers: { Authorization: `Bearer ${localStorage.getItem("echo_token")}` },
      body: form,
    });
    refetch();
  };

  const rebuild = async () => {
    await apiFetch("/knowledge/rebuild", { method: "POST" });
    refetch();
  };

  const deleteDoc = async (path: string) => {
    await apiFetch(`/knowledge/documents/${encodeURIComponent(path)}`, { method: "DELETE" });
    refetch();
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4">
        <button onClick={() => fileRef.current?.click()} className="flex items-center gap-1 bg-blue-600 text-white px-3 py-1.5 rounded text-sm">
          <Upload size={16} /> 上传文档
        </button>
        <button onClick={rebuild} className="flex items-center gap-1 bg-gray-100 px-3 py-1.5 rounded text-sm hover:bg-gray-200">
          <RefreshCw size={16} /> 重建索引
        </button>
        <span className="text-sm text-gray-500">
          已索引 {status?.indexed_count ?? 0} 篇 · 最后重建: {status?.last_rebuild || "从未"}
        </span>
        <input ref={fileRef} type="file" className="hidden" onChange={(e) => e.target.files?.[0] && upload(e.target.files[0])} />
      </div>

      <div className="space-y-2">
        {data?.documents.map((doc) => (
          <div key={doc.path} className="flex items-center justify-between bg-white border rounded p-3">
            <div>
              <div className="text-sm font-medium">{doc.path}</div>
              <div className="text-xs text-gray-400">{(doc.size / 1024).toFixed(1)} KB</div>
            </div>
            <button onClick={() => deleteDoc(doc.path)} className="text-red-400 hover:text-red-600">
              <Trash2 size={16} />
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
