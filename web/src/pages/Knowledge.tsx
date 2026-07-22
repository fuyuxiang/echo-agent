import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { runMutation } from "../stores/toast";
import { Loadable } from "../components/Loadable";
import { Upload, Trash2, RefreshCw } from "lucide-react";
import { useRef } from "react";
import { useTranslation } from "react-i18next";

interface Document {
  path: string;
  size: number;
  indexed: boolean;
}

export function Knowledge() {
  const { data, loading, error, refetch } = useApi<{ documents: Document[] }>("/knowledge/documents");
  const { data: status } = useApi<{ indexed_count: number; last_rebuild: string }>("/knowledge/status");
  const fileRef = useRef<HTMLInputElement>(null);
  const { t } = useTranslation("knowledge");

  const upload = async (file: File) => {
    const ok = await runMutation(async () => {
      const form = new FormData();
      form.append("file", file);
      // multipart 上传不走 apiFetch(它强制 JSON Content-Type),这里手动带 token,
      // 并显式检查响应状态,否则失败会被静默忽略。
      const resp = await fetch("/api/v1/knowledge/upload", {
        method: "POST",
        headers: { Authorization: `Bearer ${localStorage.getItem("echo_token")}` },
        body: form,
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: resp.statusText }));
        throw new Error(err.error || resp.statusText);
      }
    }, { success: t("uploadSuccess"), error: t("uploadFailed") });
    if (ok) refetch();
  };

  const rebuild = async () => {
    const ok = await runMutation(() => apiFetch("/knowledge/rebuild", { method: "POST" }), {
      success: t("rebuildTriggered"), error: t("rebuildFailed"),
    });
    if (ok) refetch();
  };

  const deleteDoc = async (path: string) => {
    const ok = await runMutation(
      () => apiFetch(`/knowledge/documents/${encodeURIComponent(path)}`, { method: "DELETE" }),
      { success: t("deleteSuccess"), error: t("deleteFailed") },
    );
    if (ok) refetch();
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4">
        <button onClick={() => fileRef.current?.click()} className="flex items-center gap-1 bg-blue-600 text-white px-3 py-1.5 rounded text-sm">
          <Upload size={16} /> {t("upload")}
        </button>
        <button onClick={rebuild} className="flex items-center gap-1 bg-gray-100 px-3 py-1.5 rounded text-sm hover:bg-gray-200">
          <RefreshCw size={16} /> {t("rebuild")}
        </button>
        <span className="text-sm text-gray-500">
          {t("status", { count: status?.indexed_count ?? 0, last: status?.last_rebuild || t("never") })}
        </span>
        <input ref={fileRef} type="file" className="hidden" onChange={(e) => e.target.files?.[0] && upload(e.target.files[0])} />
      </div>

      <Loadable
        loading={loading}
        error={error}
        data={data}
        isEmpty={(d) => d.documents.length === 0}
        emptyText={t("empty")}
      >
        {(d) => (
          <div className="space-y-2">
            {d.documents.map((doc) => (
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
        )}
      </Loadable>
    </div>
  );
}
