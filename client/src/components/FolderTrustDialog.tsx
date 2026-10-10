import { useCallback, useEffect, useRef, useState } from "react";
import { ShieldCheckIcon, ShieldAlertIcon } from "@/foundation/components/Icon/icons";
import {
  agentListPendingInteractions,
  folderTrustRespond,
  type FolderTrustRequest,
} from "@/lib/agent-client";
import { useModalFocus } from "@/lib/use-modal-focus";

interface LegacyTrustRequest {
  requestId?: string;
  sessionId?: string;
  cwd?: string;
  workspace?: string;
  configKinds?: string[];
  reason?: string;
  [key: string]: unknown;
}

interface FolderTrustDialogProps {
  /** Legacy event hint from App. The canonical queue is replayed from Rust. */
  request: LegacyTrustRequest | null;
  onResolve: () => void;
  onToast?: (msg: string) => void;
}

/** Ordered folder-trust queue backed by the Rust ExtMethod registry. */
export function FolderTrustDialog({ request, onResolve, onToast }: FolderTrustDialogProps) {
  const [queue, setQueue] = useState<FolderTrustRequest[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const refreshGenerationRef = useRef(0);

  const refresh = useCallback(async () => {
    const generation = ++refreshGenerationRef.current;
    setLoading(true);
    try {
      const pending = await agentListPendingInteractions();
      if (refreshGenerationRef.current !== generation) return null;
      setQueue(pending.folderTrustRequests);
      setError(null);
      return pending.folderTrustRequests;
    } catch (cause) {
      if (refreshGenerationRef.current !== generation) return null;
      console.error("list pending folder trust requests failed", cause);
      setError(`读取待处理信任请求失败：${String(cause).replace(/^Error:\s*/, "")}`);
      return null;
    } finally {
      if (refreshGenerationRef.current === generation) {
        setLoaded(true);
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
    return () => {
      refreshGenerationRef.current += 1;
    };
  }, [refresh, request]);

  const current = queue[0] ?? null;

  const respond = useCallback(async (trusted: boolean) => {
    if (!current) return;
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const acknowledged = await folderTrustRespond(current.requestId, trusted);
      if (!acknowledged) throw new Error("后端未找到该信任请求，请重试");
      setQueue((pending) => pending.filter((item) => item.requestId !== current.requestId));
      // Re-read the canonical registry before resolving the parent hint. This
      // keeps the dialog mounted while more requests are queued and closes the
      // subscribe-after-emit window for a request arriving during the ACK.
      await refresh();
    } catch (cause) {
      const message = String(cause).replace(/^Error:\s*/, "");
      setError(message);
      onToast?.(`信任响应失败：${message}`);
    } finally {
      setBusy(false);
    }
  }, [busy, current, onToast, refresh]);

  // Bootstrap polling is invisible without an event hint. Once a request is
  // known, loading/errors remain modal until it is safely resolved.
  const dialogOpen = Boolean(current || (request && (loading || error)));
  const modalRef = useModalFocus<HTMLDivElement>(
    dialogOpen,
    () => {
      if (current && !busy) void respond(false);
    },
  );

  useEffect(() => {
    if (current || error) {
      modalRef.current?.querySelector<HTMLElement>("[data-modal-initial-focus]")?.focus();
    }
  }, [current?.requestId, error, modalRef]);

  useEffect(() => {
    if (request && loaded && !loading && !error && queue.length === 0) onResolve();
  }, [error, loaded, loading, onResolve, queue.length, request]);

  if (!dialogOpen) return null;

  const displayPath = current?.workspace || current?.cwd || request?.workspace || request?.cwd || "(unknown)";

  return (
    <div className="modal-overlay trust-dialog__overlay">
      <div
        className="trust-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-label="文件夹信任"
        aria-busy={busy || loading}
        ref={modalRef}
        tabIndex={-1}
      >
        <header className="trust-dialog__header">
          <div className="trust-dialog__icon">
            <ShieldAlertIcon size="lg" />
          </div>
          <h2 className="trust-dialog__title">信任此工作区？</h2>
        </header>
        <div className="trust-dialog__body">
          <p className="trust-dialog__desc">
            该目录包含项目级配置。仅当你信任仓库内容时，才允许 EchoAgent
            加载并运行这些配置。
          </p>
          <div className="trust-dialog__path" title={displayPath}>
            <code>{displayPath}</code>
          </div>
          {current && current.configKinds.length > 0 && (
            <p className="trust-dialog__reason">
              将启用：{current.configKinds.join("、")}
            </p>
          )}
          {current && queue.length > 1 && (
            <p className="trust-dialog__reason">还有 {queue.length - 1} 个信任请求等待处理</p>
          )}
          {loading && !current && <p className="trust-dialog__reason" role="status">正在读取信任请求…</p>}
          {error && <p className="trust-dialog__error" role="alert">{error}</p>}
        </div>
        <footer className="trust-dialog__footer">
          {current ? (
            <div className="trust-dialog__actions">
              <button
                className="btn btn--ghost"
                onClick={() => void respond(false)}
                disabled={busy}
                data-modal-initial-focus
              >
                不信任
              </button>
              <button className="btn btn--primary" onClick={() => void respond(true)} disabled={busy}>
                <ShieldCheckIcon size="sm" /> 信任并加载
              </button>
            </div>
          ) : error ? (
            <div className="trust-dialog__actions">
              <button
                className="btn btn--primary"
                type="button"
                onClick={() => void refresh()}
                disabled={loading}
                data-modal-initial-focus
              >
                {loading ? "重试中…" : "重试"}
              </button>
            </div>
          ) : null}
          <p className="trust-dialog__hint">
            信任按工作区生效；项目 MCP、插件与 hooks 会随即重载。
          </p>
        </footer>
      </div>
    </div>
  );
}
