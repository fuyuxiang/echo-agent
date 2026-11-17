import { useEffect, useRef, useState } from "react";
import { FileText, Folder, Plus } from "lucide-react";
import type { TaskArtifact } from "@/lib/artifact-catalog";
import type { ProjectResource } from "./ProjectResourcePreview";
import { flushProjectPersistence, useProjectsStore, type ProjectMeta, type AssetItem } from "@/stores/projects-store";
import { filesystemPickFiles, projectAssetsImport, projectAssetsUse, openLocalPath, projectAssetRemove } from "@/lib/agent-client";
import { orgDownloadDocument } from "@/lib/org-client";
import { captureTeamProjectOperation, teamProjectUpload, type SharedAsset } from "@/lib/team-project-client";
import { projectAssetRows } from "@/lib/project-tasks";
import { projectPublicationLinks } from "@/lib/project-sharing";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
import { useAppDialog } from "./AppDialog";
import { withProjectAssetSharing } from "@/lib/project-asset-sharing";

export function ProjectMaterials({ project, account, sharedId, workspace, selectedAssetIds, onUseAsset, outputs = [], onPreview, onUseOutput, initialFilter = "all", addRequested, onAddHandled }: {
  project: ProjectMeta; account: string; sharedId?: string; workspace: ProjectWorkspace; selectedAssetIds: string[]; onUseAsset: (asset: AssetItem) => void;
  outputs?: TaskArtifact[]; onPreview?: (resource: ProjectResource) => void; onUseOutput?: (output: TaskArtifact) => Promise<void>;
  initialFilter?: "all" | "reference" | "output"; addRequested?: boolean; onAddHandled?: () => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState(initialFilter);
  useEffect(() => setFilter(initialFilter), [initialFilter]);
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const [error, setError] = useState("");
  const { requestConfirmation, dialog } = useAppDialog(`${project.id}:${account}`);
  const writable = !sharedId || workspace.snapshot?.role === "admin" || workspace.snapshot?.role === "collaborator";
  const rows = projectAssetRows(project, workspace.snapshot, account, projectPublicationLinks(project.id, account, sharedId).assets).filter(row => row.name.toLowerCase().includes(query.toLowerCase()) && (filter === "all" || (filter === "output") === (row.local?.purpose === "output")));
  const visibleOutputs = filter === "reference" ? [] : outputs.filter(o => o.path.toLowerCase().includes(query.toLowerCase()) && !project.assets.some(a => a.sourceSessionId === o.sessionId && a.sourcePath === o.path && a.sourceUpdatedAt === (o.producedAt ?? o.updatedAt) && a.sourceToolCallId === o.toolCallId));
  const run = async (work: () => Promise<void>) => {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError("");
    try { await work(); } catch (e) { setError(String(e).replace(/^Error:\s*/, "")); }
    finally { lock.current = false; setBusy(false); }
  };
  const upload = (previous?: SharedAsset) => run(async () => {
    const identity = sharedId ? captureTeamProjectOperation() : undefined;
    const paths = await filesystemPickFiles(previous ? { title: `为「${previous.title}」选择新版本`, multiple: false, maxFiles: 1 } : { title: sharedId ? "添加资料（项目成员可见）" : "添加项目资料" });
    identity?.assertCurrent();
    if (!paths.length) return;
    if (previous && paths.length !== 1) throw new Error("请选择一个文件作为新版本");
    const imported = await projectAssetsImport(project.id, paths);
    useProjectsStore.getState().addAssets(project.id, imported);
    useProjectsStore.getState().recordActivity(project.id, { kind: "file", content: `添加了 ${imported.length} 份项目资料` });
    await flushProjectPersistence();
    identity?.assertCurrent();
    const assets = useProjectsStore.getState().projects.find(p => p.id === project.id)?.assets ?? [];
    if (sharedId) {
      let published = 0;
      try {
        for (const file of imported) {
          identity!.assertCurrent();
          const local = assets.find(a => a.path === file.path);
          if (!local?.path) throw new Error("文件副本未就绪");
          const source = local.path;
          await withProjectAssetSharing(project.id, account, sharedId, local.id, async () => {
            identity!.assertCurrent();
            const currentLink = useProjectsStore.getState().projects.find(p => p.id === project.id)?.assets.find(a => a.id === local.id)?.sharedAssets?.[account];
            if (currentLink?.projectId === sharedId && currentLink.documentId) return;
            const [path] = await projectAssetsUse(project.id, [source], "share");
            identity!.assertCurrent();
            if (!path) throw new Error("文件副本授权尚未完成");
            const result = await teamProjectUpload(sharedId, path, previous, identity);
            identity!.assertCurrent();
            await useProjectsStore.getState().linkSharedAsset(project.id, local.id, account, { projectId: sharedId, documentId: result.docId, familyId: result.familyId ?? previous?.familyId });
          });
          published++;
        }
      } catch (e) { throw new Error(`已共享 ${published} 份，其余副本保留在本机。请刷新核对后再共享。${String(e)}`); }
      finally { await workspace.refresh(); }
    }
  });
  const shareLocal = (asset: AssetItem) => requestConfirmation({ title: `共享「${asset.name}」到项目？`, description: "项目成员将可以访问此文件，原任务的私人对话不会上传。", confirmLabel: "共享文件", action: () => run(async () => {
    if (!sharedId || !asset.path || !writable) throw new Error("项目连接或共享权限不可用，请刷新后重试");
    const identity = captureTeamProjectOperation();
    const source = asset.path;
    try {
      await withProjectAssetSharing(project.id, account, sharedId, asset.id, async () => {
        identity.assertCurrent();
        const currentLink = useProjectsStore.getState().projects.find(p => p.id === project.id)?.assets.find(a => a.id === asset.id)?.sharedAssets?.[account];
        if (currentLink?.projectId === sharedId && currentLink.documentId) return;
        await flushProjectPersistence();
        identity.assertCurrent();
        await projectAssetsUse(project.id, [source], "share");
        identity.assertCurrent();
        const result = await teamProjectUpload(sharedId, source, undefined, identity);
        identity.assertCurrent();
        await useProjectsStore.getState().linkSharedAsset(project.id, asset.id, account, { projectId: sharedId, documentId: result.docId, familyId: result.familyId });
      });
    } finally { await workspace.refresh(); }
  }) });
  useEffect(() => { if (addRequested) { onAddHandled?.(); void upload(); } }, [addRequested]);
  const removeLocal = (asset: AssetItem) => requestConfirmation({ title: `移除资料「${asset.name}」？`, description: "将删除本机项目中的文件副本。原始文件和已共享版本保留。", confirmLabel: "移除本机副本", danger: true, action: async () => { if (asset.path) await projectAssetRemove(project.id, asset.path); useProjectsStore.getState().removeAsset(project.id, asset.id); } });
  return <div className="project-unified-materials"><div className="project-material-toolbar"><div className="project-state-filters" aria-label="资料分类">{([['all','全部'],['reference','参考资料'],['output','任务成果']] as const).map(([value,label]) => <button key={value} className="pd-btn pd-btn--ghost" aria-pressed={filter === value} onClick={() => setFilter(value)}>{label}</button>)}</div><button className="pd-btn" disabled={busy || !writable} onClick={() => void upload()}><Plus size={14} />添加资料</button></div><div className="project-material-search"><span>{sharedId ? "新添加的资料对项目成员可见；本机成果需明确共享" : "资料保留本机副本，成果关联原任务"}</span><input className="pd-search-inline" aria-label="搜索资料" placeholder="搜索文件…" value={query} onChange={e => setQuery(e.target.value)} /></div>
    {error && <p role="alert" className="project-inline-error">{error}</p>}
    {!rows.length && !visibleOutputs.length && <div className="project-workspace-empty"><FileText size={28} /><strong>{filter === "output" ? "暂无任务成果" : "暂无资料"}</strong><p>{filter === "output" ? "任务生成的文件会显示在这里，也可从原任务中继续处理。" : "添加参考文件，供后续任务使用。"}</p></div>}
    {visibleOutputs.map(output => <article className="project-material-row" key={`${output.sessionId}:${output.id}`}><FileText className="project-file-glyph" size={24} /><div className="project-material-name"><button className="project-file-title" onClick={() => onPreview?.({ output })}>{output.path.replace(/\\/g, "/").split("/").pop()}</button><small>任务成果 · {output.sessionTitle} · {new Date(output.updatedAt).toLocaleString()}</small></div><div className="project-material-actions"><button className="pd-btn pd-btn--small" disabled={busy} onClick={() => onPreview?.({ output })}>预览</button>{onUseOutput && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => void run(() => onUseOutput(output))}>用于新任务</button>}</div></article>)}
    {rows.map(row => { const localDocumentId = row.local?.sharedAssets?.[account]?.documentId;
      const oldCopy = !!row.remote && !!localDocumentId && localDocumentId !== row.remote.id;
      return <article className="project-material-row" key={row.id}>{row.local?.kind === "folder" ? <Folder className="project-file-glyph" size={24} /> : <FileText className="project-file-glyph" size={24} />}<div className="project-material-name"><button className="project-file-title" onClick={() => onPreview?.({ asset: row })}>{row.name}</button><small>{row.local?.purpose === "output" ? "任务成果 · " : ""}{row.remote ? `v${row.remote.version} · ${row.remote.status === "ready" ? "已就绪" : row.remote.status === "failed" ? "处理失败" : "处理中"}` : sharedId ? "仅自己" : row.local?.ext ?? "文件"}{row.remote?.currentDocumentId && row.remote.currentDocumentId !== row.remote.id ? " · 上一有效版本可用" : ""}</small>{row.local?.sourceSessionId && <small>来自原任务 · 可在预览中打开来源</small>}</div><div className="project-material-actions">
      {onPreview && row.local?.kind !== "folder" && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => onPreview({ asset: row })}>预览</button>}
      {row.local?.path && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => void run(() => openLocalPath(row.local!.path!))}>{oldCopy ? "打开本机旧版" : "打开"}</button>}
      {row.remote && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => void run(async () => { const identity = captureTeamProjectOperation(); await orgDownloadDocument(row.remote!.id, false, row.name, identity.expectedGeneration); })}>下载</button>}
      {row.remote && !row.local?.path && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => onPreview?.({ asset: row })}>用于新任务</button>}
      {row.local?.path && row.local.kind === "file" && <button className="pd-btn pd-btn--small" disabled={selectedAssetIds.includes(row.local.id)} onClick={() => onUseAsset(row.local!)}>{selectedAssetIds.includes(row.local.id) ? "已选入任务" : oldCopy ? "用于任务（本机旧版）" : "用于任务"}</button>}
      {sharedId && writable && !row.remote && row.local?.path && row.local.kind === "file" && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => shareLocal(row.local!)}>共享到项目</button>}
      {row.remote && writable && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => void upload(row.remote)}>更新版本</button>}
      {row.local && <button className="pd-btn pd-btn--small" disabled={busy} onClick={() => removeLocal(row.local!)}>移除副本</button>}
    </div>{!!row.previousLocals?.length && <details className="project-material-history"><summary>本机历史版本（{row.previousLocals.length}）</summary>{row.previousLocals.map(file => <div key={file.id}><span>{file.name}</span><button className="pd-btn pd-btn--small" disabled={!file.path || busy} onClick={() => void run(() => openLocalPath(file.path!))}>打开旧副本</button></div>)}</details>}</article>; })}{dialog}</div>;
}
