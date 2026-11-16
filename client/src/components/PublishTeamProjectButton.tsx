import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { useAppDialog } from "./AppDialog";
import { useOrgSessionStore } from "@/stores/org-session-store";
import type { ProjectMeta } from "@/stores/projects-store";
import { projectAssetsUse } from "@/lib/agent-client";
import { captureTeamProjectOperation, teamProjectRequest, teamProjectUpload, type TeamProject, type TeamProjectSnapshot } from "@/lib/team-project-client";

interface Publication {
  projectId?: string;
  creating?: boolean;
  complete?: boolean;
  files: string[];
  tasks: string[];
  createdTasks: Record<string, string>;
  selectedFiles: string[];
  selectedTasks: string[];
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
export function PublishTeamProjectButton({ project, onPublished }: { project: ProjectMeta; onPublished?: (id: string) => void }) {
  const account = useOrgSessionStore(s => s.session?.accountId ?? JSON.stringify([s.session?.serverUrl, s.session?.user?.id ?? s.session?.bootstrap?.user.id]));
  const loggedIn = useOrgSessionStore(s => s.session?.loggedIn);
  const key = `echoagent.team-publication.v1:${account}:${project.id}`;
  const publishing = useSyncExternalStore(subscribePublication, () => activePublications.has(key));
  const currentKey = useRef(key);
  const mounted = useRef(true);
  currentKey.current = key;
  const pending = useRef<Publication>(empty());
  const [state, setState] = useState<Publication>(empty());
  const [error, setError] = useState("");
  const { requestConfirmation, requestInput, dialog } = useAppDialog(`${project.id}:${account}`);
  useEffect(() => {
    mounted.current = true;
    pending.current = empty(); setState(empty()); setError("");
    try { pending.current = readPublication(key); setState(pending.current); setError(""); }
    catch (error) { setError(String(error)); }
    return () => { mounted.current = false; };
  }, [key, publishing]);
  if (!loggedIn) return null;
  const save = (value: Publication) => {
    localStorage.setItem(key, JSON.stringify(value));
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
    requestConfirmation({ title: state.projectId ? "继续发布项目内容" : "发布为团队项目", confirmLabel: "发布并管理成员", description: <>
      <p>项目目标和所选内容将共享给项目成员。历史个人会话与本机参与者备注保留在本机。</p>
      <p>{state.projectId ? "继续发布到已有团队项目，不会重复创建。" : "发布后可邀请组织成员。"}</p>
      {project.assets.filter(a => a.kind === "file" && a.path).map(a => <label key={a.id} style={{ display: "block" }}><input type="checkbox" defaultChecked={selectedFiles.has(a.id)} disabled={state.files.includes(a.id)} onChange={e => { if (e.target.checked) selectedFiles.add(a.id); else selectedFiles.delete(a.id); }} /> {a.name}{state.files.includes(a.id) && "（已发布）"}</label>)}
      {project.tasks.map(t => <label key={t.id} style={{ display: "block" }}><input type="checkbox" defaultChecked={selectedTasks.has(t.id)} disabled={state.tasks.includes(t.id)} onChange={e => { if (e.target.checked) selectedTasks.add(t.id); else selectedTasks.delete(t.id); }} /> 工作项：{t.title}{state.tasks.includes(t.id) && "（已发布）"}</label>)}
    </>, action: () => publishOnce(key, async () => {
      identity.assertCurrent();
      const value = pending.current;
      value.selectedFiles = [...selectedFiles]; value.selectedTasks = [...selectedTasks]; value.complete = false;
      save(value);
      if (!value.projectId) {
        if (value.creating) throw new Error("请先确认上次创建结果，避免重复创建项目");
        value.creating = true; save(value);
        try {
          value.projectId = (await teamProjectRequest<TeamProjectSnapshot>("create", undefined, { name: project.name, goal: project.instructions ?? "" }, undefined, identity)).id;
          value.creating = false; save(value);
        } catch (error) {
          throw new Error(`创建结果尚未确认。关闭此提示后点击“确认创建结果”，选择目标再继续发布。${String(error)}`);
        }
      }
      try {
        for (const task of project.tasks.filter(t => selectedTasks.has(t.id) && !value.tasks.includes(t.id))) {
          identity.assertCurrent();
          let taskId = value.createdTasks[task.id];
          if (!taskId) {
            taskId = (await teamProjectRequest<{ id: string }>("createTask", value.projectId, { title: task.title }, undefined, identity)).id;
            value.createdTasks[task.id] = taskId; save(value);
          }
          if (task.status !== "pending") {
            const snapshot = await teamProjectRequest<TeamProjectSnapshot>("snapshot", value.projectId, undefined, undefined, identity);
            const remoteTask = snapshot.tasks.find(t => t.id === taskId);
            if (!remoteTask) throw new Error("已发布工作项已删除，请先打开团队项目核对");
            const status = task.status === "completed" ? "accepted" : task.status === "paused" ? "blocked" : "in_progress";
            if (remoteTask.status !== status) {
              if (remoteTask.revision !== 1) throw new Error("已发布工作项被其他成员更新，请先打开团队项目核对");
              await teamProjectRequest("updateTask", value.projectId, { expectedRevision: remoteTask.revision, status }, taskId, identity);
            }
          }
          value.tasks.push(task.id); save(value);
        }
        for (const asset of project.assets.filter(a => selectedFiles.has(a.id) && a.path && !value.files.includes(a.id))) {
          identity.assertCurrent();
          const [path] = await projectAssetsUse(project.id, [asset.path!]);
          identity.assertCurrent();
          if (!path) throw new Error("所选文件不可用，请重新添加文件");
          await teamProjectUpload(value.projectId, path, undefined, identity);
          value.files.push(asset.id); save(value);
        }
      } catch (error) { throw new Error(`团队项目已创建，部分内容尚未发布；离开页面后也可继续发布到同一项目。${String(error)}`); }
      identity.assertCurrent(); value.complete = true; save(value);
      if (mounted.current && currentKey.current === key) onPublished?.(value.projectId);
    }) });
  };
  return <><div className="team-publication-actions">
    {state.projectId && <button type="button" onClick={() => onPublished?.(state.projectId!)}>打开团队项目</button>}
    {state.creating && !state.projectId ? <button type="button" disabled={publishing} onClick={() => void recoverCreation()}>确认创建结果</button> : <button type="button" disabled={!!error || publishing} onClick={publish}>{state.projectId ? state.complete ? "补充发布内容" : "继续发布" : "发布为团队项目"}</button>}
  </div>{publishing && <p role="status">正在发布，离开页面后会继续处理。</p>}{error && <p role="alert">{error}</p>}{dialog}</>;
}
