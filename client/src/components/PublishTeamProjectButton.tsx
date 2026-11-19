import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { useAppDialog } from "./AppDialog";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { projectAssetsUse } from "@/lib/agent-client";
import { captureTeamProjectOperation, PROJECT_DELETED_MESSAGE, teamProjectRequest, teamProjectUpload, type TeamProject, type TeamProjectSnapshot } from "@/lib/team-project-client";
import { projectCollaborationCapabilities } from "@/lib/team-project-client";
import { projectSharingAccount, publicationKey, notifyProjectSharing, sharedProjectId } from "@/lib/project-sharing";
import { withProjectAssetSharing } from "@/lib/project-asset-sharing";

interface Publication {
  projectId?: string;
  creating?: boolean;
  complete?: boolean;
  files: string[];
  tasks: string[];
  createdTasks: Record<string, string>;
  createdAssets?: Record<string, string>;
  createdAssetFamilies?: Record<string, string>;
  taskRequestIds?: Record<string, string>;
  selectedFiles: string[];
  selectedTasks: string[];
  requestId?: string;
}
const empty = (): Publication => ({ files: [], tasks: [], createdTasks: {}, selectedFiles: [], selectedTasks: [] });
const activePublications = new Set<string>();
const publicationListeners = new Set<() => void>();
const subscribePublication = (listener: () => void) => { publicationListeners.add(listener); return () => { publicationListeners.delete(listener); }; };
async function publishOnce(key: string, work: () => Promise<void>) {
  if (activePublications.has(key)) throw new Error("项目仍在发布中，请等待当前发布结束");
  activePublications.add(key); publicationListeners.forEach(listener => listener());
  try { await work(); }
  finally { activePublications.delete(key); publicationListeners.forEach(listener => listener()); }
}
function readPublication(key: string): Publication {
  const raw = localStorage.getItem(key);
  if (!raw) return empty();
  const value = JSON.parse(raw) as Publication;
  if (!Array.isArray(value.files) || !Array.isArray(value.tasks) || !value.createdTasks || !Array.isArray(value.selectedFiles) || !Array.isArray(value.selectedTasks)) {
    throw new Error("发布恢复记录无法读取，请先在团队项目中核对已共享内容");
  }
  return value;
}

/** Persist publication IDs and selection progress by account and local project.
 * Private transcripts and participant notes never enter the publication. */
export function PublishTeamProjectButton({ project, onPublished, onLinked, collaborationMode = false }: { project: ProjectMeta; onPublished?: (id: string) => void; onLinked?: (id: string) => void | Promise<void>; collaborationMode?: boolean }) {
  const live = useProjectsStore(s => s.projects.find(p => p.id === project.id)) ?? project;
  const account = useOrgSessionStore(s => projectSharingAccount(s.session));
  const loggedIn = useOrgSessionStore(s => s.session?.loggedIn);
  const key = publicationKey(project.id, account);
  const deletedProjectId = live.deletedSharedProjects?.[account];
  const linkedProjectId = sharedProjectId(live, account);
  const publishing = useSyncExternalStore(subscribePublication, () => activePublications.has(key));
  const currentKey = useRef(key);
  const mounted = useRef(true);
  const callbacks = useRef({ onLinked, onPublished });
  callbacks.current = { onLinked, onPublished };
  currentKey.current = key;
  const pending = useRef<Publication>(empty());
  const [state, setState] = useState<Publication>(empty());
  const [error, setError] = useState("");
  const { requestConfirmation, requestInput, dialog } = useAppDialog(`${project.id}:${account}:${deletedProjectId ?? ""}`);
  useEffect(() => {
    mounted.current = true;
    pending.current = empty(); setState(empty()); setError("");
    try {
      const publication = readPublication(key);
      const previousId = publication.projectId;
      const targetId = linkedProjectId ?? (previousId === deletedProjectId ? undefined : previousId);
      pending.current = previousId && previousId !== targetId ? empty() : publication;
      pending.current.projectId = targetId;
      setState(pending.current);
    }
    catch (error) { setError(String(error)); }
    return () => { mounted.current = false; };
  }, [key, publishing, deletedProjectId, linkedProjectId]);
  if (!loggedIn) return null;
  const save = (value: Publication) => {
    const currentProject = useProjectsStore.getState().projects.find(item => item.id === project.id) ?? live;
    if (value.projectId && currentProject.deletedSharedProjects?.[account] === value.projectId) {
      throw new Error(PROJECT_DELETED_MESSAGE);
    }
    localStorage.setItem(key, JSON.stringify(value));
    notifyProjectSharing();
    if (mounted.current && currentKey.current === key) {
      pending.current = value;
      setState({ ...value });
    }
  };
  const recoverCreation = async () => {
    try {
      const identity = captureTeamProjectOperation();
      const projects = await teamProjectRequest<TeamProject[]>("list", undefined, undefined, undefined, identity);
      identity.assertCurrent();
      if (!mounted.current || currentKey.current !== key) return;
      const candidates = projects.filter(item => item.role === "admin");
      requestInput({ title: "恢复项目发布", description: "上次创建请求的结果尚未确认。请选择已创建的目标，核对后继续发布；确认服务端未创建时才选择重新创建。", fields: [{ name: "target", label: "发布目标", defaultValue: "", required: true, options: [{ value: "", label: "请选择目标项目" }, ...candidates.map(item => ({ value: item.id, label: `${item.name} · ${item.id.slice(-8)}` })), { value: "new", label: "已核对，没有创建成功，重新创建" }] }], action: async values => {
        identity.assertCurrent();
        if (!values.target || (values.target !== "new" && !candidates.some(item => item.id === values.target))) throw new Error("请选择有效目标");
        save({ ...pending.current, projectId: values.target === "new" ? undefined : values.target, creating: false });
      } });
    } catch (error) { setError(String(error)); }
  };
  const publish = () => {
    const identity = captureTeamProjectOperation();
    const selectedFiles = new Set(pending.current.selectedFiles), selectedTasks = new Set(pending.current.selectedTasks);
    const taskShared = (task: ProjectMeta["tasks"][number]) => state.tasks.includes(task.id)
      || !!state.projectId && task.sharedTasks?.[account]?.projectId === state.projectId && !!task.sharedTasks[account].taskId;
    const assetShared = (asset: ProjectMeta["assets"][number]) => state.files.includes(asset.id)
      || !!state.projectId && asset.sharedAssets?.[account]?.projectId === state.projectId && !!asset.sharedAssets[account].documentId;
    requestConfirmation({ title: collaborationMode ? state.projectId ? "共享项目内容" : "开启项目协作" : state.projectId ? "继续发布项目内容" : "发布为团队项目", confirmLabel: collaborationMode ? "确认共享" : "发布并管理成员", description: <>
      <p>项目目标和所选内容将共享给项目成员。历史个人会话与本机参与者备注保留在本机。</p>
      <p>{state.projectId ? "继续发布到已有团队项目，不会重复创建。" : "发布后可邀请组织成员。"}</p>
      {live.assets.filter(a => a.kind === "file" && a.path).map(a => <label key={a.id} style={{ display: "block" }}><input type="checkbox" defaultChecked={assetShared(a) || selectedFiles.has(a.id)} disabled={assetShared(a)} onChange={e => { if (e.target.checked) selectedFiles.add(a.id); else selectedFiles.delete(a.id); }} /> {a.name}{assetShared(a) && "（已发布）"}</label>)}
      {live.tasks.map(t => <label key={t.id} style={{ display: "block" }}><input type="checkbox" defaultChecked={taskShared(t) || selectedTasks.has(t.id)} disabled={taskShared(t)} onChange={e => { if (e.target.checked) selectedTasks.add(t.id); else selectedTasks.delete(t.id); }} /> 任务：{t.title}{taskShared(t) && "（已发布）"}</label>)}
    </>, action: () => publishOnce(key, async () => {
      identity.assertCurrent();
      const value = pending.current;
      value.selectedFiles = [...selectedFiles]; value.selectedTasks = [...selectedTasks]; value.complete = false;
      save(value);
      const capabilities = collaborationMode ? await projectCollaborationCapabilities(identity) : undefined;
      if (!value.projectId) {
        if (value.creating && !(capabilities?.idempotentCreate && value.requestId)) throw new Error("请先确认上次创建结果，避免重复创建项目");
        if (capabilities?.idempotentCreate) value.requestId ??= crypto.randomUUID();
        value.creating = true; save(value);
        try {
          const created = await teamProjectRequest<TeamProjectSnapshot>("create", undefined, { name: project.name, goal: project.description ?? project.instructions ?? "", ...(value.requestId ? { clientRequestId: value.requestId } : {}) }, undefined, identity);
          if (typeof created?.id !== "string" || !created.id) throw new Error("服务端未返回可确认的项目，原项目数据未修改");
          value.projectId = created.id;
          value.creating = false; save(value);
        } catch (error) {
          throw new Error(`创建结果尚未确认。关闭此提示后点击“确认创建结果”，选择目标再继续发布。${String(error)}`);
        }
      }
      identity.assertCurrent();
      const targetId = value.projectId;
      if (mounted.current && currentKey.current === key) await callbacks.current.onLinked?.(targetId);
      try {
        // Re-read durable links after every await. Sharing from another surface
        // must not create a second remote identity or overwrite current state.
        const latest = () => useProjectsStore.getState().projects.find(p => p.id === project.id) ?? live;
        for (const selected of latest().tasks.filter(t => selectedTasks.has(t.id) && !value.tasks.includes(t.id))) {
          identity.assertCurrent();
          const task = latest().tasks.find(t => t.id === selected.id);
          if (!task) continue;
          const link = task.sharedTasks?.[account];
          const existing = link?.projectId === value.projectId ? link : undefined;
          if (existing?.taskId && value.createdTasks[task.id] !== existing.taskId) {
            value.createdTasks[task.id] = existing.taskId;
            value.tasks.push(task.id); save(value);
            continue;
          }
          let taskId = existing?.taskId ?? value.createdTasks[task.id];
          if (!taskId) {
            if (existing?.requestId || capabilities?.taskIdentity) {
              value.taskRequestIds ??= {}; value.taskRequestIds[task.id] ??= existing?.requestId ?? crypto.randomUUID(); save(value);
              await useProjectsStore.getState().linkSharedTask(project.id, task.id, account, { projectId: value.projectId, requestId: value.taskRequestIds[task.id] });
              identity.assertCurrent();
            }
            taskId = (await teamProjectRequest<{ id: string }>("createTask", value.projectId, { title: task.title, ...(task.description ? { description: task.description } : {}), ...(task.acceptance ? { acceptance: task.acceptance } : {}), ...(value.taskRequestIds?.[task.id] ? { clientRequestId: value.taskRequestIds[task.id] } : {}) }, undefined, identity)).id;
            if (!taskId) throw new Error("创建任务结果未确认，请刷新核对");
            value.createdTasks[task.id] = taskId; save(value);
          }
          identity.assertCurrent();
          await useProjectsStore.getState().linkSharedTask(project.id, task.id, account, { projectId: value.projectId, taskId, requestId: value.taskRequestIds?.[task.id] });
          if (task.status !== "pending") {
            const snapshot = await teamProjectRequest<TeamProjectSnapshot>("snapshot", value.projectId, undefined, undefined, identity);
            const remoteTask = snapshot.tasks.find(t => t.id === taskId);
            if (!remoteTask) throw new Error("已发布任务已删除，请先打开团队项目核对");
            const status = task.status === "completed" ? "accepted" : task.status === "paused" ? "blocked" : "in_progress";
            if (remoteTask.status !== status) {
              if (remoteTask.revision !== 1) throw new Error("已发布任务被其他成员更新，请先打开团队项目核对");
              await teamProjectRequest("updateTask", value.projectId, { expectedRevision: remoteTask.revision, status }, taskId, identity);
            }
          }
          value.tasks.push(task.id); save(value);
        }
        for (const selected of latest().assets.filter(a => selectedFiles.has(a.id) && a.path && !value.files.includes(a.id))) {
          await withProjectAssetSharing(project.id, account, targetId, selected.id, async () => {
            identity.assertCurrent();
            const asset = latest().assets.find(a => a.id === selected.id);
            if (!asset?.path) return;
            const link = asset.sharedAssets?.[account];
            if (link?.projectId === targetId && link.documentId) {
              value.createdAssets ??= {}; value.createdAssets[asset.id] = link.documentId;
              value.files.push(asset.id); save(value);
              return;
            }
            let uploaded: { docId: string; familyId?: string } | undefined = value.createdAssets?.[asset.id]
              ? { docId: value.createdAssets[asset.id], familyId: value.createdAssetFamilies?.[asset.id] } : undefined;
            if (!uploaded) {
              const [path] = await projectAssetsUse(project.id, [asset.path], "share");
              identity.assertCurrent();
              if (!path) throw new Error("所选文件不可用，请重新添加文件");
              const currentLink = latest().assets.find(a => a.id === asset.id)?.sharedAssets?.[account];
              uploaded = currentLink?.projectId === targetId && currentLink.documentId
                ? { docId: currentLink.documentId, familyId: currentLink.familyId }
                : await teamProjectUpload(targetId, path, undefined, identity);
            }
            identity.assertCurrent();
            value.createdAssets ??= {}; value.createdAssets[asset.id] = uploaded.docId;
            if (uploaded.familyId) { value.createdAssetFamilies ??= {}; value.createdAssetFamilies[asset.id] = uploaded.familyId; }
            save(value);
            await useProjectsStore.getState().linkSharedAsset(project.id, asset.id, account, { projectId: targetId, documentId: uploaded.docId, familyId: uploaded.familyId });
            value.files.push(asset.id); save(value);
          });
        }
      } catch (error) { throw new Error(`${collaborationMode ? "协作已开启，部分内容尚未共享；可以继续共享剩余内容。" : "团队项目已创建，部分内容尚未发布；离开页面后也可继续发布到同一项目。"}${String(error)}`); }
      identity.assertCurrent(); value.complete = true; save(value);
      if (mounted.current && currentKey.current === key) callbacks.current.onPublished?.(value.projectId);
    }) });
  };
  return <><div className="team-publication-actions">
    {state.projectId && !collaborationMode && <button type="button" className="capability-button" onClick={() => onPublished?.(state.projectId!)}>打开团队项目</button>}
    {state.creating && !state.projectId && !state.requestId ? <button type="button" className="capability-button" disabled={publishing} onClick={() => void recoverCreation()}>确认创建结果</button> : <button type="button" className="capability-button" disabled={!!error || publishing} onClick={publish}>{collaborationMode ? state.projectId ? "共享更多内容" : state.creating ? "重试开启协作" : "开启协作并选择共享内容" : state.projectId ? state.complete ? "补充发布内容" : "继续发布" : "发布为团队项目"}</button>}
  </div>{publishing && <p role="status">正在发布，离开页面后会继续处理。</p>}{error && <p role="alert">{error}</p>}{dialog}</>;
}
