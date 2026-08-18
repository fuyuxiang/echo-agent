import { useApi } from "../hooks/use-api";
import { apiFetch, apiUpload } from "../lib/api";
import { relativeTime } from "../lib/datetime";
import { runMutation } from "../stores/toast";
import { useIsAdmin } from "../stores/capabilities";
import { Loadable } from "../components/Loadable";
import { useConfirm } from "../components/ConfirmDialog";
import { Upload, Trash2, RefreshCw } from "lucide-react";
import { useRef } from "react";
import { useTranslation } from "react-i18next";

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
  const { t } = useTranslation(["knowledge", "common"]);
  const confirm = useConfirm();
  // Upload, delete and rebuild are all guarded by _admin_guard server-side; only
  // the reads are open to a plain api token. Probe the token's scope so those
  // controls are visibly disabled instead of rendering as normal buttons that
  // answer 403. null = still probing, treated as allowed to avoid a disabled
  // flash on first paint.
  const isAdmin = useIsAdmin();
  const canWrite = isAdmin !== false;

  // last_rebuild 是索引文件的 mtime,从未构建时为 null。
  const lastRebuild = () => relativeTime(status?.last_rebuild, t("never"));

  const upload = async (file: File) => {
    const ok = await runMutation(async () => {
      const form = new FormData();
      form.append("file", file);
      await apiUpload("/knowledge/upload", form);
    }, { success: t("uploadSuccess"), error: t("uploadFailed") });
    if (ok) {
      if (fileRef.current) fileRef.current.value = "";
      refetch();
      refetchStatus();
    }
  };

  const rebuild = async () => {
    const ok = await runMutation(() => apiFetch("/knowledge/rebuild", { method: "POST" }), {
      success: t("rebuildTriggered"), error: t("rebuildFailed"),
    });
    if (ok) { refetch(); refetchStatus(); }
  };

  const deleteDoc = async (path: string) => {
    // Deleting a document also kicks off a full index rebuild server-side, so
    // an accidental click costs more than the file. Say that in the prompt.
    const confirmed = await confirm({
      title: t("deleteConfirmTitle"),
      message: t("deleteConfirmMessage", { path }),
      confirmLabel: t("common:delete"),
      destructive: true,
    });
    if (!confirmed) return;
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
        <button
          onClick={() => fileRef.current?.click()}
          disabled={!canWrite}
          title={canWrite ? undefined : t("common:adminOnly")}
          className="flex items-center gap-1 bg-blue-600 text-white px-3 py-1.5 rounded text-sm disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <Upload size={16} /> {t("upload")}
        </button>
        <button
          onClick={rebuild}
          disabled={!canWrite}
          title={canWrite ? undefined : t("common:adminOnly")}
          className="flex items-center gap-1 bg-gray-100 px-3 py-1.5 rounded text-sm hover:bg-gray-200 disabled:opacity-50 disabled:cursor-not-allowed"
        >
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
        <input ref={fileRef} type="file" className="hidden" accept=".md,.txt,.rst,.json,.yaml,.yml,.py,.pdf,.docx,.xlsx,.pptx" onChange={(e) => e.target.files?.[0] && upload(e.target.files[0])} />
      </div>

      {!canWrite && (
        <div className="bg-amber-50 text-amber-700 rounded-lg px-4 py-2 text-sm">
          {t("common:adminOnly")}
        </div>
      )}

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
                <button
                  onClick={() => deleteDoc(doc.path)}
                  disabled={!canWrite}
                  aria-label={t("deleteDocAria", { path: doc.path })}
                  title={canWrite ? undefined : t("common:adminOnly")}
                  className="text-red-400 hover:text-red-600 disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:text-red-400"
                >
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
