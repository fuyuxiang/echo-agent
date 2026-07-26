import { useApi } from "../hooks/use-api";
import { apiFetch, getToken } from "../lib/api";
import { runMutation } from "../stores/toast";
import { Loadable } from "../components/Loadable";
import { Upload, Trash2, RefreshCw } from "lucide-react";
import { useRef } from "react";
import { useTranslation } from "react-i18next";
import { formatDistanceToNow } from "date-fns";
import { zhCN, enUS } from "date-fns/locale";

interface Document {
  path: string;
  size: number;
  modified: number;
}

// Mirrors KnowledgeIndex.status(). The page previously declared
// indexed_count/last_rebuild, neither of which the backend ever returned, so
// the status line was pinned at "0 indexed · never" no matter how many
// documents were in the index.
interface KnowledgeStatus {
  documents: number;
  chunks: number;
  stale: boolean;
  last_rebuild: string | null;
}

export function Knowledge() {
  const { data, loading, error, refetch } = useApi<{ documents: Document[] }>("/knowledge/documents");
  const { data: status, refetch: refetchStatus } = useApi<KnowledgeStatus>("/knowledge/status");
  const fileRef = useRef<HTMLInputElement>(null);
  const { t, i18n } = useTranslation("knowledge");

  // last_rebuild is the index file's mtime; null when never built. Guard the
  // parse the way Sessions does — an Invalid Date would make date-fns throw
  // and take the whole page down.
  const dfLocale = i18n.resolvedLanguage === "en" ? enUS : zhCN;
  const lastRebuild = (): string => {
    if (!status?.last_rebuild) return t("never");
    const date = new Date(status.last_rebuild);
    if (Number.isNaN(date.getTime())) return t("never");
    return formatDistanceToNow(date, { locale: dfLocale, addSuffix: true });
  };

  const upload = async (file: File) => {
    const ok = await runMutation(async () => {
      const form = new FormData();
      form.append("file", file);
      // multipart 上传不走 apiFetch(它强制 JSON Content-Type),这里手动带 token,
      // 并显式检查响应状态,否则失败会被静默忽略。
      const resp = await fetch("/api/v1/knowledge/upload", {
        method: "POST",
        headers: { Authorization: `Bearer ${getToken()}` },
        body: form,
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ error: resp.statusText }));
        throw new Error(err.error || resp.statusText);
      }
    }, { success: t("uploadSuccess"), error: t("uploadFailed") });
    // Every mutation here rebuilds the index server-side, so the status line
    // (document/chunk counts, last rebuild) is stale unless it refetches too.
    if (ok) { refetch(); refetchStatus(); }
  };

  const rebuild = async () => {
    const ok = await runMutation(() => apiFetch("/knowledge/rebuild", { method: "POST" }), {
      success: t("rebuildTriggered"), error: t("rebuildFailed"),
    });
    if (ok) { refetch(); refetchStatus(); }
  };

  const deleteDoc = async (path: string) => {
    // Encode each segment separately: the backend lists nested docs as
    // "sub/doc.md" and routes the delete on a tail match, so the separators
    // must survive as real slashes. Encoding the whole path would send %2F,
    // which some proxies reject outright before it reaches the gateway.
    const encoded = path.split("/").map(encodeURIComponent).join("/");
    const ok = await runMutation(
      () => apiFetch(`/knowledge/documents/${encoded}`, { method: "DELETE" }),
      { success: t("deleteSuccess"), error: t("deleteFailed") },
    );
    if (ok) { refetch(); refetchStatus(); }
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
          {t("status", {
            count: status?.documents ?? 0,
            chunks: status?.chunks ?? 0,
            last: lastRebuild(),
          })}
          {status?.stale && <span className="ml-2 text-amber-600">{t("stale")}</span>}
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
