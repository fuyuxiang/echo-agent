import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ArrowUpRight, Paperclip, RotateCw, X } from "lucide-react";
import { useModalFocus } from "@/lib/use-modal-focus";
import { loadAttachmentPreview } from "@/lib/attachment-preview";
import { authorizeDocumentArtifact, type TaskArtifact } from "@/lib/artifact-catalog";
import { authorizeArtifactFile, isUnauthorizedPathError } from "@/lib/artifact-access";
import { orgPreviewDocument } from "@/lib/org-client";
import { captureTeamProjectOperation } from "@/lib/team-project-client";
import { projectAssetsUse } from "@/lib/agent-client";
import { flushProjectPersistence, type ProjectMeta } from "@/stores/projects-store";
import { detectPreviewKind } from "@/lib/file-kind";
import type { UnifiedAsset } from "@/lib/project-tasks";

const FilePreview = lazy(() => import("./FilePreview").then(m => ({ default: m.FilePreview })));
export type ProjectResource = { output: TaskArtifact; asset?: never } | { asset: UnifiedAsset; output?: never };

export function ProjectResourcePreview({ project, resource, onClose, onUse, onOpenSession }: {
  project: ProjectMeta; resource: ProjectResource; onClose: () => void;
  onUse: (resource: ProjectResource) => Promise<void>; onOpenSession?: (id: string, cwd?: string) => void;
}) {
  const focus = useModalFocus<HTMLDivElement>(true, onClose);
  const name = resource.output ? resource.output.path.replace(/\\/g, "/").split("/").pop()! : resource.asset.name;
  const [content, setContent] = useState<string>();
  const [error, setError] = useState("");
  const [unauthorized, setUnauthorized] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [busy, setBusy] = useState(false);
  const operation = useRef(false);
  useEffect(() => {
    let active = true; setContent(undefined); setError(""); setUnauthorized(false);
    void (async () => {
      if (resource.output) { await authorizeDocumentArtifact(resource.output.sessionId, resource.output); return loadAttachmentPreview(resource.output.canonicalPath ?? resource.output.path, resource.output.cwd); }
      if (resource.asset.remote) {
        const identity = captureTeamProjectOperation();
        const result = await orgPreviewDocument(resource.asset.remote.id, false, ["markdown", "code", "text"].includes(detectPreviewKind(name)) ? "txt" : "binary", name);
        identity.assertCurrent();
        return result.content;
      }
      if (!resource.asset.local?.path) throw new Error("文件副本不可用，请重新添加");
      await flushProjectPersistence();
      await projectAssetsUse(project.id, [resource.asset.local.path]);
      return loadAttachmentPreview(resource.asset.local.path);
    })().then(value => { if (active) setContent(value); }).catch(e => { if (active) { setError(String(e).replace(/^Error:\s*/, "")); setUnauthorized(!!resource.output && isUnauthorizedPathError(e)); } });
    return () => { active = false; };
  }, [project.id, resource, name, attempt]);
  const use = async () => {
    if (operation.current) return; operation.current = true; setBusy(true); setError("");
    try { await onUse(resource); } catch (e) { setError(String(e).replace(/^Error:\s*/, "")); }
    finally { operation.current = false; setBusy(false); }
  };
  const sourceSessionId = resource.output?.sessionId ?? resource.asset?.local?.sourceSessionId;
  return createPortal(<div className="modal-overlay" onClick={() => { if (!busy) onClose(); }}><div ref={focus} className="project-resource-preview" role="dialog" aria-modal="true" aria-label={`预览：${name}`} tabIndex={-1} onClick={e => e.stopPropagation()}>
    <header><div><h2>{name}</h2><small>{resource.output ? `来自任务：${resource.output.sessionTitle}` : resource.asset.remote ? `共享资料 · v${resource.asset.remote.version}` : "本机项目资料"}</small></div><button className="pd-btn pd-btn--ghost" aria-label="关闭文件预览" disabled={busy} onClick={onClose}><X size={18} /></button></header>
    <div className="project-resource-preview__body">{content !== undefined ? <Suspense fallback={<p role="status">正在准备文档预览…</p>}><FilePreview filename={name} content={content} path={resource.output?.canonicalPath ?? resource.output?.path ?? (resource.asset?.remote ? undefined : resource.asset?.local?.path)} onReload={() => setAttempt(n => n + 1)} /></Suspense> : !error && <p role="status">正在读取文件…</p>}{error && <div className="project-preview-error" role="alert"><p>{error}</p><button className="pd-btn" onClick={() => setAttempt(n => n + 1)}><RotateCw size={14} />重新加载</button>{unauthorized && resource.output && <button className="pd-btn" onClick={() => { void authorizeArtifactFile(resource.output.canonicalPath ?? resource.output.path, resource.output.cwd).then(result => { if (result === "authorized") setAttempt(n => n + 1); else if (result === "mismatch") setError("所选文件与成果不一致，请重试"); }).catch(e => setError(String(e))); }}>选择并授权原文件</button>}</div>}</div>
    <footer>{sourceSessionId && onOpenSession && <button className="pd-btn" onClick={() => { onClose(); onOpenSession(sourceSessionId, resource.output?.cwd); }}>查看来源任务<ArrowUpRight size={14} /></button>}<button className="pd-btn pd-btn--primary" disabled={busy} onClick={() => void use()}><Paperclip size={14} />{busy ? "准备文件中…" : "用于新任务"}</button></footer>
  </div></div>, document.body);
}
