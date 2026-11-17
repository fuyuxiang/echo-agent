import { ProjectParticipantsButton } from "./ProjectParticipantsButton";
import { ProjectTasks } from "./ProjectTasks";
import { ProjectMaterials } from "./ProjectMaterials";
import { ProjectSettingsDialog } from "./ProjectSettingsDialog";
import { ProjectOverview, ProjectContextSidebar, ProjectActivity, ProjectDecisionsDialog } from "./ProjectWorkspaceSections";
import { ProjectResourcePreview, type ProjectResource } from "./ProjectResourcePreview";
import { ProjectAutomationsDialog, useProjectAutomations } from "./ProjectAutomationsDialog";
import { useProjectArtifacts } from "@/lib/use-project-artifacts";
import { projectTaskRows } from "@/lib/project-tasks";
import { projectPublicationLinks } from "@/lib/project-sharing";
import { importProjectOutput } from "@/lib/project-files";
import type { TaskArtifact } from "@/lib/artifact-catalog";
import { orgDownloadDocument } from "@/lib/org-client";
import { PanelRight, Settings2, Users, LockKeyhole, Check } from "lucide-react";
import { useProjectWorkspace } from "@/lib/use-project-workspace";
import { useSharedProjectId, projectSharingAccount } from "@/lib/project-sharing";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { captureTeamProjectOperation, projectCollaborationCapabilities, teamProjectRequest, type TeamProjectSnapshot, type StartTeamWork, type OpenTeamExecution } from "@/lib/team-project-client";
import { findModelDisplayName } from "@/lib/model-presentation";
import { PageToolbar } from "./PageToolbar";
import { useSessionsStore } from "@/stores/sessions-store";

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  useProjectsStore,
  type ProjectMeta,
  type ProjectTab,
  canonicalProjectTab,
  type RefItem,
} from "@/stores/projects-store";
import {
  type ProjectPickerOptions,
} from "./project-picker";
import { Composer } from "./Composer";
import { type ModelOption } from "./ModelSelector";
import { FolderIcon } from "@/foundation/components/Icon/icons";
import { useAppDialog } from "./AppDialog";
import type { AgentEntry } from "@/lib/types";
import type { SlashCommandInvocation } from "@/lib/slash-commands";
import { useWorkspaceMentions } from "@/lib/use-workspace-mentions";
import { projectAssetsUse, projectAssetsImport, filesystemAttachmentStats, type WorkspaceInfo } from "@/lib/agent-client";
import { friendlyError, isTaskWorkspaceError } from "@/lib/error-format";
import { draftAttachments } from "@/lib/draft-attachments";
import { draftRevision, touchDraft } from "@/lib/draft-lifecycle";
import { displayPath } from "@/lib/display-path";
import { hasRelativeWorkspaceMention } from "@/lib/at-commands";
import type { StartProjectConversation } from "@/lib/project-context";
import { selectProjectPermissionMode, usePermissionModeStore } from "@/stores/permission-mode-store";
import { flushProjectPersistence } from "@/stores/projects-store";
import "@/styles/project-context.css";
import "@/styles/project-workspace.css";

export function ProjectDetailView({
  onTeamPublished,
  onStartTeamWork,
  onOpenTeamExecution,
  project,
  initialTab = "overview",
  draftFocusRequest,
  workspaces = [],
  onBack,
  onToast,
  onStartConversation,
  onOpenSession,
  onRenameSession,
  onOpenProjectWorkItemSession,
  onArchiveSession,
  picker,
  onOpenAutomation,
  models = [],
  defaultModelId,
  onOpenModelSettings,
  onClientSlashCommand,
  onNavigateConnectors,
  onOpenKnowledgeBase,
  onOpenMeetingMinutes,
  onOpenOrganization,
}: {
  project: ProjectMeta;
  onTeamPublished?: (id: string) => void;
  onStartTeamWork?: StartTeamWork;
  onOpenTeamExecution?: OpenTeamExecution;
  initialTab?: ProjectTab;
  draftFocusRequest?: number;
  workspaces?: WorkspaceInfo[];
  onBack: () => void;
  onToast?: (msg: string) => void;
  /** Start a new conversation within this project (creates a real EchoAgent session). */
  onStartConversation?: StartProjectConversation;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  onOpenProjectWorkItemSession?: (projectId: string, sessionId: string, cwd?: string) => void;
  onRenameSession?: (sessionId: string, title: string, cwd?: string) => Promise<void>;
  onArchiveSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  picker: { options: ProjectPickerOptions; loading: boolean; error: string | null };
  onOpenAutomation?: () => void;
  models?: ModelOption[];
  /** Application default, used only by projects that do not yet have a persisted choice. */
  defaultModelId?: string;
  onOpenModelSettings?: () => void;
  onClientSlashCommand?: (invocation: SlashCommandInvocation) => boolean | void | Promise<boolean | void>;
  onNavigateConnectors?: () => void;
  onOpenKnowledgeBase?: () => void;
  onOpenMeetingMinutes?: (modelId?: string) => void;
  onOpenOrganization?: () => void;
}) {
  // 读最新（交互后 store 更新，父传入的快照可能过期）。
  const live = useProjectsStore((s) => s.projects.find((p) => p.id === project.id)) ?? project;
  const updateConfig = useProjectsStore((s) => s.updateConfig);

  const draftKey = `project:${project.id}`;
  const workspaceDraftKey = `project-workspace:${project.id}`;
  const assetDraftKey = `project-assets:${project.id}`;
  const projectDraft = useSessionsStore((state) => state.drafts[draftKey] ?? "");
  const workspaceOverride = useSessionsStore((state) => state.drafts[workspaceDraftKey]);
  const taskCwd = workspaceOverride || live.cwd;
  const selectedAssetDraft = useSessionsStore(state => state.drafts[assetDraftKey] ?? "[]");
  let selectedAssetIds: string[] = [];
  try { const parsed: unknown = JSON.parse(selectedAssetDraft); if (Array.isArray(parsed)) selectedAssetIds = parsed.filter((id): id is string => typeof id === "string"); } catch { /* An invalid optional selection never changes the text draft. */ }
  const selectedAssets = live.assets.filter(asset => selectedAssetIds.includes(asset.id) && asset.kind === "file" && asset.path);
  const hasInvalidAssetSelection = selectedAssetIds.some(id => !selectedAssets.some(asset => asset.id === id));
  const selectAssets = (ids: string[]) => {
    touchDraft(assetDraftKey);
    useSessionsStore.getState().setDraft(assetDraftKey, ids.length ? JSON.stringify(ids) : "");
  };
  const selectAsset = (id: string) => selectAssets([...new Set([...selectedAssetIds, id])]);
  const prepareProjectAttachments = async (attachments: string[] = []) => {
    if (hasInvalidAssetSelection) throw new Error("部分已选项目资料已删除或没有文件，请移除失效选择后重试");
    await flushProjectPersistence();
    const granted = selectedAssets.length ? await projectAssetsUse(live.id, selectedAssets.map(asset => asset.path!)) : [];
    const all = [...new Set([...attachments, ...granted])];
    if (all.length > 20) throw new Error("本次附件与项目资料合计不能超过20个文件");
    if (granted.length) {
      const stats = await filesystemAttachmentStats(all);
      if (stats.rejected.length) throw new Error(stats.rejected.map(file => file.reason).join("；"));
      if (stats.files.length !== all.length || stats.files.reduce((bytes, file) => bytes + file.sizeBytes, 0) > 64 * 1024 * 1024) throw new Error("本次附件与项目资料总大小不能超过64MB");
    }
    return all;
  };
  const [tab, setTab] = useState<ProjectTab>(canonicalProjectTab(initialTab));
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [contextOpen, setContextOpen] = useState(() => { try { const saved = localStorage.getItem("echoagent.project-context-hidden"); return saved === null ? window.innerWidth >= 900 : saved !== "1"; } catch { return true; } });
  const [selectedTaskId, setSelectedTaskId] = useState<string>();
  const [assetFilter, setAssetFilter] = useState<"all" | "reference" | "output">("all");
  const [addFilesRequested, setAddFilesRequested] = useState(false);
  const [resource, setResource] = useState<ProjectResource>();
  const [decisionsOpen, setDecisionsOpen] = useState(false);
  const [automationsOpen, setAutomationsOpen] = useState(false);
  const [localFocus, setLocalFocus] = useState(0);
  const sharedId = useSharedProjectId(live);
  const sharingAccount = useOrgSessionStore(state => projectSharingAccount(state.session));
  const scopeKey = `${live.id}:${sharingAccount}:${sharedId ?? ""}`;
  const currentScope = useRef(scopeKey); currentScope.current = scopeKey;
  useEffect(() => { setResource(undefined); setSettingsOpen(false); setDecisionsOpen(false); setAutomationsOpen(false); setSelectedTaskId(undefined); }, [scopeKey]);
  const linkShared = async (id: string) => {
    await useProjectsStore.getState().linkSharedProject(live.id, sharingAccount, id);
    onTeamPublished?.(id);
  };
  useEffect(() => {
    // Upgrade the old localStorage association without modifying any source data.
    if (sharedId && live.sharedProjects?.[sharingAccount] !== sharedId) {
      void useProjectsStore.getState().linkSharedProject(live.id, sharingAccount, sharedId).catch(error => onToast?.(`协作关联待保存：${String(error)}`));
    }
  }, [sharedId, live.id, sharingAccount, live.sharedProjects, onToast]);
  const workspace = useProjectWorkspace(sharedId);
  const { artifacts: outputs, error: outputError, refresh: refreshOutputs } = useProjectArtifacts(live);
  const automation = useProjectAutomations(live);
  const openProjectSession = (id: string, cwd?: string) => {
    const actualCwd = useSessionsStore.getState().independent.find(s => s.sessionId === id)?.cwd ?? cwd ?? live.cwd;
    if (onOpenProjectWorkItemSession) onOpenProjectWorkItemSession(live.id, id, actualCwd);
    else onOpenSession?.(id, actualCwd);
  };
  const actorId = useOrgSessionStore(state => state.session?.user?.id);
  const rows = projectTaskRows(live, workspace.snapshot, sharingAccount, projectPublicationLinks(live.id, sharingAccount, sharedId).tasks, actorId, sharedId);
  const persisting = useProjectsStore(state => state.persisting);
  const persistError = useProjectsStore(state => state.persistError);
  const toggleContext = () => setContextOpen(value => { try { localStorage.setItem("echoagent.project-context-hidden", value ? "1" : "0"); } catch {} return !value; });
  const openTask = (id: string) => { setSelectedTaskId(rows.find(r => r.id === id || r.remoteId === id.replace(/^shared:/, ""))?.id ?? id); setTab("task"); };
  const showFiles = (filter: "all" | "reference" | "output" = "all") => { setAssetFilter(filter); setTab("asset"); };
  const addFiles = () => { showFiles(); setAddFilesRequested(true); };
  const useOutput = async (output: TaskArtifact) => {
    const scope = scopeKey;
    const asset = await importProjectOutput(live.id, output);
    if (currentScope.current !== scope) return;
    if (asset) { selectAsset(asset.id); setResource(undefined); setTab("overview"); setLocalFocus(n => n + 1); }
  };
  const useResource = async (chosen: ProjectResource) => {
    const scope = scopeKey;
    if (chosen.output) return useOutput(chosen.output);
    const row = chosen.asset;
    let asset = row.local;
    const currentRemote = row.remote && asset?.sharedAssets?.[sharingAccount]?.documentId === row.remote.id;
    if (row.remote && !currentRemote) {
      const identity = captureTeamProjectOperation();
      const path = await orgDownloadDocument(row.remote.id, false, row.name, identity.expectedGeneration);
      identity.assertCurrent(); if (!path) return;
      const imported = await projectAssetsImport(live.id, [path]);
      identity.assertCurrent();
      useProjectsStore.getState().addAssets(live.id, imported);
      asset = useProjectsStore.getState().projects.find(p => p.id === live.id)?.assets.find(a => a.path === imported[0]?.path);
      if (asset && sharedId) await useProjectsStore.getState().linkSharedAsset(live.id, asset.id, sharingAccount, { projectId: sharedId, documentId: row.remote.id, familyId: row.remote.familyId });
      identity.assertCurrent();
    }
    if (currentScope.current !== scope) return;
    if (!asset?.path || asset.kind !== "file") throw new Error("当前资料不是可引用的文件");
    selectAsset(asset.id); setResource(undefined); setTab("overview"); setLocalFocus(n => n + 1);
  };
  // Composer 失败原因。Composer 内部不持有"失败 alert",必须由调用方展示,
  // 否则用户看到"按了发送但什么都没发生"。
  const [sendError, setSendError] = useState<string | null>(null);
  const [workspaceError, setWorkspaceError] = useState<{ cwd?: string; message: string }>();
  const [composerExpert, setComposerExpert] = useState<AgentEntry | null>(null);
  const { requestConfirmation, dialog } = useAppDialog(live.id);
  const sharedDraft = useRef<{ text: string; taskId: string; sharedId: string; account: string }>();
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const permissionDraft = useProjectsStore(state => state.draftPermissions[live.id]);
  const projectPermissionMode = usePermissionModeStore(state => selectProjectPermissionMode(state, permissionDraft?.mode));
  const workItemPermissionMode = usePermissionModeStore(selectProjectPermissionMode);
  const mentionCandidates = useWorkspaceMentions(taskCwd);
  const configuredProjectModelAvailable = !live.defaultModelId
    || models.some((model) => model.id === live.defaultModelId);
  const inheritedModelId = defaultModelId && models.some((model) => model.id === defaultModelId)
    ? defaultModelId
    : models[0]?.id;
  const projectModelId = live.defaultModelId
    ? configuredProjectModelAvailable ? live.defaultModelId : undefined
    : inheritedModelId;
  const setProjectModel = (modelId: string) => {
    updateConfig(live.id, { defaultModelId: modelId });
  };

  const addComposerExpert = (agent: AgentEntry) => {
    const item: RefItem = {
      id: agent.path || agent.name,
      name: agent.name,
      scope: agent.scope,
    };
    if (!live.experts.some((expert) => expert.id === item.id || expert.name === item.name)) {
      updateConfig(live.id, { experts: [...live.experts, item] });
    }
    setComposerExpert(agent);
  };

  const selectWorkspace = (cwd: string, trigger?: HTMLButtonElement | null) => {
    if (!mounted.current) return;
    const next = cwd || live.cwd;
    const apply = () => {
      if (next !== taskCwd) touchDraft(workspaceDraftKey);
      useSessionsStore.getState().setDraft(workspaceDraftKey, next === live.cwd ? "" : cwd);
      setWorkspaceError(undefined);
    };
    if (next === taskCwd || !hasRelativeWorkspaceMention(useSessionsStore.getState().drafts[draftKey] ?? "")) {
      apply();
      return;
    }
    requestConfirmation({
      title: "更改草稿的工作目录？",
      description: <>从「{taskCwd ? displayPath(taskCwd) : "默认任务目录"}」切换至「{next ? displayPath(next) : "默认任务目录"}」。草稿中的相对 @ 引用会按新目录解释，请在发送前核对这些引用。</>,
      confirmLabel: "切换目录",
      returnFocus: trigger,
      action: apply,
    });
  };

  const runProjectConversation = async (
    text: string,
    modelId: string,
    attachments: string[],
    fromDraft: boolean,
  ): Promise<string | undefined> => {
    if (!onStartConversation && !(fromDraft && sharedId && onStartTeamWork)) {
      onToast?.("当前项目无法启动会话");
      return;
    }
    const expectedPermissionMode = fromDraft ? projectPermissionMode : workItemPermissionMode;
    if (expectedPermissionMode === undefined) return;
    setSendError(null);
    setWorkspaceError(undefined);
    const submittedRevision = draftRevision(draftKey);
    const workspaceRevision = draftRevision(workspaceDraftKey);
    const assetRevision = draftRevision(assetDraftKey);
    try {
      const options = {
        cwd: taskCwd ?? null,
        requestedPermissionMode: fromDraft ? permissionDraft?.mode : undefined,
        expectedPermissionMode,
        prepareAttachments: () => prepareProjectAttachments(attachments),
        rememberModel: fromDraft,
        sourceTab: "task" as const,
      };
      let sessionId: string | undefined;
      if (fromDraft && sharedId) {
        const identity = captureTeamProjectOperation();
        if (!onStartTeamWork || !workspace.snapshot || workspace.snapshot.role === "viewer") throw new Error("项目连接或编辑权限不可用，请刷新后重试");
        const caps = await projectCollaborationCapabilities(identity);
        identity.assertCurrent();
        if (!caps.taskIdentity) throw new Error("组织服务需升级后才能新建协作任务");
        const store = useProjectsStore.getState();
        let pending = sharedDraft.current;
        if (!pending || pending.text !== text || pending.sharedId !== sharedId || pending.account !== sharingAccount) {
          const existing = store.projects.find(p => p.id === live.id)?.tasks.find(task => task.source === "project-draft" && task.description === text && task.modelId === modelId && !task.sessionId && task.sharedTasks?.[sharingAccount]?.projectId === sharedId && task.sharedTasks[sharingAccount].requestId);
          const taskId = existing?.id ?? store.addTask(live.id, text.split("\n")[0].slice(0, 120) || "新任务", modelId);
          if (!existing) {
            store.updateTask(live.id, taskId, { description: text, source: "project-draft" });
            await store.linkSharedTask(live.id, taskId, sharingAccount, { projectId: sharedId, requestId: crypto.randomUUID() });
          }
          pending = { text, taskId, sharedId, account: sharingAccount }; sharedDraft.current = pending;
        }
        const local = useProjectsStore.getState().projects.find(p => p.id === live.id)?.tasks.find(t => t.id === pending.taskId);
        const link = local?.sharedTasks?.[sharingAccount];
        if (!local || !link) throw new Error("本机任务关联不可用，请从任务列表继续");
        let remoteId = link.taskId;
        if (!remoteId) {
          const created = await workspace.mutate<{id: string}>("createTask", { title: local.title, description: text, clientRequestId: link.requestId });
          if (!created.id) throw new Error("创建任务结果未确认，请重试同一任务");
          remoteId = created.id;
          await store.linkSharedTask(live.id, local.id, sharingAccount, { ...link, taskId: remoteId });
        }
        const snapshot = await teamProjectRequest<TeamProjectSnapshot>("snapshot", sharedId, undefined, undefined, identity);
        identity.assertCurrent();
        const task = snapshot.tasks.find(task => task.id === remoteId);
        if (!task) throw new Error("任务不可用，请刷新后核对");
        if (snapshot.runs?.some(run => run.taskId === task.id && run.actorId === useOrgSessionStore.getState().session?.user?.id)) throw new Error("任务已有执行记录，请从任务详情打开后继续，草稿已保留");
        try {
          sessionId = await onStartTeamWork(snapshot, task, modelId, { localProjectId: live.id, options });
          identity.assertCurrent();
          if (sessionId) { store.linkTaskSession(live.id, local.id, sessionId, modelId); sharedDraft.current = undefined; }
        } finally { await workspace.refresh(); }
      } else sessionId = await onStartConversation!(live.id, text, modelId, options);
      if (!sessionId) {
        setSendError(fromDraft ? "消息尚未发送，请查看提示后重试。" : "任务尚未启动，请查看提示后重试。");
      } else {
        if (fromDraft && permissionDraft) {
          useProjectsStore.getState().consumeDraftPermission(live.id, permissionDraft);
        }
        const drafts = useSessionsStore.getState();
        const draftUnchanged = draftRevision(draftKey) === submittedRevision;
        const hasSeparateDraft = !fromDraft && Boolean(drafts.drafts[draftKey]?.trim() || draftAttachments(draftKey).length);
        if (!hasSeparateDraft && draftUnchanged) {
          if (draftRevision(workspaceDraftKey) === workspaceRevision) drafts.setDraft(workspaceDraftKey, "");
          if (draftRevision(assetDraftKey) === assetRevision) drafts.setDraft(assetDraftKey, "");
        }
      }
      return sessionId;
    } catch (error) {
      const failureLabel = fromDraft ? "发送失败" : "启动任务失败";
      const workspaceFailure = isTaskWorkspaceError(error);
      const detail = workspaceFailure || error instanceof Error ? String(error).replace(/^Error:\s*/, "") : friendlyError(error);
      if (!mounted.current) {
        onToast?.(`项目「${live.name}」${failureLabel}：${detail}`);
        return undefined;
      }
      if (workspaceFailure) {
        const recovery = taskCwd ? "工作目录不可用，请重新选择。" : "默认任务目录无法分配，请选择一个现有文件夹后重试。";
        setWorkspaceError({ cwd: taskCwd, message: `${recovery} ${detail}` });
      } else {
        setSendError(`${failureLabel}：${detail}`);
      }
      return undefined;
    }
  };

  return (
    <div className="pd-page project-workspace">
      <PageToolbar className="pd-topbar">
        <div className="pd-crumb">
          <FolderIcon size="sm" />
          <button className="pd-crumb__link" onClick={onBack}>项目</button>
          <span className="pd-crumb__sep">/</span>
          <span className="pd-crumb__name">{live.name}</span>
        </div>
        <div className="project-detail-members"><span className="project-members-avatars" aria-label={`${workspace.snapshot?.members.length ?? 1} 位参与者`}>{(workspace.snapshot?.members.slice(0, 3) ?? [{ userId: "self", displayName: "我" }]).map(m => <span key={m.userId}>{m.displayName.slice(-1)}</span>)}</span><ProjectParticipantsButton project={live} sharedId={sharedId} onLinked={linkShared} onOpenOrganization={onOpenOrganization} label="邀请" /><button type="button" className="pd-btn pd-btn--ghost" onClick={toggleContext} aria-label={contextOpen ? "收起项目上下文" : "展开项目上下文"} aria-pressed={contextOpen}><PanelRight size={17} /></button><button type="button" className="pd-btn pd-btn--ghost" aria-label="项目设置" disabled={!!sharedId && !workspace.snapshot} onClick={() => setSettingsOpen(true)}><Settings2 size={17} /></button></div>
      </PageToolbar>

      <div className={`pd-body project-unified-body project-workspace-body${contextOpen ? "" : " project-workspace-body--wide"}`}>
        <div className="pd-main">
          <div className="project-workspace-title"><h1>{workspace.snapshot?.name ?? live.name}</h1><p>{workspace.snapshot?.goal || live.description || "集中管理相关任务、背景资料与交付成果。"}</p><div><span>{sharedId ? <Users size={14} /> : <LockKeyhole size={14} />}{sharedId ? "共享项目" : "仅自己可见"}</span><span>{workspace.snapshot?.members.length ?? 1} 位参与者</span><span><Check size={13} />{persistError ? "本机保存失败" : persisting ? "正在保存…" : "本机更改已保存"}</span></div></div>
          <div className="pd-tabs-row">
            <nav className="pd-tabs" aria-label="项目内容" role="tablist">
              {([['overview','概览'],['task','任务'],['asset','资料与成果'],['activity','动态']] as const).map(([id,label]) => <button key={id} type="button" role="tab" id={`project-tab-${id}`} aria-label={label} aria-selected={tab === id} aria-controls="project-content-panel" className={`pd-tab-btn${tab === id ? " pd-tab-btn--on" : ""}`} onClick={() => { setTab(id); setSelectedTaskId(undefined); }}>{label}{id === "task" && <span>{rows.length}</span>}{id === "asset" && <span>{live.assets.length + outputs.length}</span>}</button>)}
            </nav>
            {sharedId && <div className="project-sync-state"><span>{workspace.loading ? "同步中…" : workspace.error ? "同步失败" : workspace.snapshot ? "已同步" : "待连接"}</span><button className="pd-btn pd-btn--small" disabled={workspace.loading} onClick={() => void workspace.refresh()}>刷新</button></div>}
          </div>
          {workspace.error && <p className="project-inline-error" role="alert">{workspace.error}</p>}
          {workspace.snapshot?.syncWarning && <p className="project-inline-error" role="alert">{workspace.snapshot.syncWarning}</p>}
          {persistError && <p className="project-inline-error" role="alert">保存失败：{persistError}<button className="pd-btn" onClick={() => useProjectsStore.getState().retryPersist()}>重试保存</button></p>}
          <div className="pd-tab-content" id="project-content-panel" role="tabpanel" aria-labelledby={`project-tab-${tab}`}>
            {tab === "overview" && <ProjectOverview project={live} rows={rows} outputs={outputs} onOpenTask={openTask} onOpenOutput={output => setResource({ output })} onTasks={() => setTab("task")} onFiles={() => showFiles("output")} onAddFiles={addFiles} />}
            {tab === "activity" && <ProjectActivity key={scopeKey} draftScope={scopeKey} project={live} sharedId={sharedId} workspace={workspace} onOpenTask={openTask} />}
            {tab === "task" && <ProjectTasks key={`${live.id}:${sharingAccount}`} inlineDetails project={live} account={sharingAccount} sharedId={sharedId} workspace={workspace} models={models} defaultModelId={projectModelId} runDisabled={workItemPermissionMode === undefined} selectedTaskId={selectedTaskId} onSelectedTaskChange={setSelectedTaskId} outputs={outputs} onPreviewOutput={output => setResource({ output })}
              onRun={onStartConversation ? (message, modelId) => runProjectConversation(message, modelId, [], false) : undefined}
              onStartTeamWork={onStartTeamWork ? (snapshot, task, modelId) => onStartTeamWork(snapshot, task, modelId, { localProjectId: live.id, options: { cwd: taskCwd ?? null, expectedPermissionMode: workItemPermissionMode!, prepareAttachments: () => prepareProjectAttachments([]) } }) : undefined} onOpenTeamExecution={onOpenTeamExecution}
              onOpenSession={onOpenProjectWorkItemSession || onOpenSession ? openProjectSession : undefined}
              onRenameSession={onRenameSession && ((sessionId, title) => onRenameSession(sessionId, title, useSessionsStore.getState().independent.find(s => s.sessionId === sessionId)?.cwd ?? live.cwd))}
              onArchiveSession={onArchiveSession && ((sessionId, archived) => onArchiveSession(sessionId, archived, useSessionsStore.getState().independent.find(s => s.sessionId === sessionId)?.cwd ?? live.cwd))} />}
            {tab === "asset" && <><ProjectMaterials key={`${live.id}:${sharingAccount}`} project={live} account={sharingAccount} sharedId={sharedId} workspace={workspace} selectedAssetIds={selectedAssetIds} onUseAsset={asset => { selectAsset(asset.id); setTab("overview"); setLocalFocus(n => n + 1); }} outputs={outputs} onPreview={setResource} onUseOutput={useOutput} initialFilter={assetFilter} addRequested={addFilesRequested} onAddHandled={() => setAddFilesRequested(false)} />{outputError && <p className="project-inline-error" role="alert">部分成果读取失败：{outputError}<button className="pd-btn" onClick={refreshOutputs}>重试读取成果</button></p>}</>}
          </div>

          {(tab === "overview" || tab === "task" && !selectedTaskId) && <div className="pd-composer-shell">
            <Composer
              draftKey={draftKey}
              draft={projectDraft}
              focusRequest={(draftFocusRequest ?? 0) + localFocus}
              onDraftChange={(value) => useSessionsStore.getState().setDraft(draftKey, value)}
              draftAttachmentControls={selectedAssetIds.length > 0 ? (
                <div
                  className="composer-blocks project-draft-assets"
                  role="group"
                  aria-label="本次项目资料"
                  onClick={event => event.stopPropagation()}
                >
                  {selectedAssets.map(asset => (
                    <span className="composer-blocks__chip" key={asset.id}>
                      <span className="project-draft-assets__name" title={asset.name}>{asset.name}</span>
                      <button
                        type="button"
                        className="composer-blocks__chip-remove"
                        onClick={() => selectAssets(selectedAssetIds.filter(id => id !== asset.id))}
                        aria-label={`移除本次资料 ${asset.name}`}
                      >
                        <span aria-hidden="true">×</span>
                      </button>
                    </span>
                  ))}
                  {hasInvalidAssetSelection && (
                    <>
                      <span role="alert">部分项目资料已失效</span>
                      <button type="button" className="pd-btn pd-btn--small" onClick={() => selectAssets(selectedAssets.map(asset => asset.id))}>
                        移除失效选择
                      </button>
                    </>
                  )}
                  {selectedAssets.length > 1 && (
                    <button type="button" className="pd-btn pd-btn--small" onClick={() => selectAssets([])}>
                      清除本次资料选择
                    </button>
                  )}
                </div>
              ) : null}
              streaming={false}
              onSend={async (text, attachments) => Boolean(await runProjectConversation(text, projectModelId!, attachments ?? [], true))}
              sendDisabled={projectPermissionMode === undefined || !!sharedId && (!workspace.snapshot || workspace.snapshot.role === "viewer")}
              onCancel={() => { /* 项目页 composer 不流式,onCancel 为 Composer prop 必传的占位。 */ }}
              placeholder={sharedId ? "接下来，要推进什么工作？执行报告对项目成员可见（Shift+Enter 换行）" : "接下来，要推进什么工作？（Shift+Enter 换行）"}
              apiReady={!!projectModelId && models.length > 0}
              setupHint={
                models.length === 0
                  ? "请先在「设置 → 模型」配置模型"
                  : !projectModelId && live.defaultModelId
                    ? `原项目模型「${findModelDisplayName(models, live.defaultModelId)}」已不可用，请重新选择`
                    : undefined
              }
              onOpenSettings={onOpenModelSettings}
              onToast={onToast}
              modelId={projectModelId}
              modelLoading={false}
              models={models}
              onModelChange={setProjectModel}
              cwd={taskCwd}
              workspaces={workspaces}
              onSelectWorkspace={selectWorkspace}
              projectWorkspaceDefault={{ cwd: live.cwd }}
              workspaceError={workspaceError?.cwd === taskCwd ? workspaceError?.message : undefined}
              automaticWorkspace={!taskCwd}
              showMeta
              compact
              projectDraftPermission={{ mode: permissionDraft?.mode, onChange: mode => useProjectsStore.getState().setDraftPermission(live.id, mode) }}
              onSelectExpert={addComposerExpert}
              activeExpertName={composerExpert?.name}
              onDismissExpert={() => setComposerExpert(null)}
              onNavigateConnectors={onNavigateConnectors}
              onOpenKnowledgeBase={onOpenKnowledgeBase}
              onOpenMeetingMinutes={() => onOpenMeetingMinutes?.(projectModelId)}
              onOpenOrganization={onOpenOrganization}
              onClientSlashCommand={onClientSlashCommand}
              filePaths={mentionCandidates.filePaths}
              workspaceSymbols={mentionCandidates.workspaceSymbols}
            />
            {(sendError || (!projectModelId && live.defaultModelId)) && (
              <div className="pd-composer-warning" role="alert">
                <span>
                  {sendError
                    ?? `原项目模型「${findModelDisplayName(models, live.defaultModelId)}」已不可用，请重新选择。`}
                </span>
                {models.length === 0 && onOpenModelSettings && (
                  <button type="button" onClick={onOpenModelSettings}>
                    前往设置模型
                  </button>
                )}
              </div>
            )}
          </div>}
        </div>
        {contextOpen && <ProjectContextSidebar project={live} workspace={workspace} modelName={live.defaultModelId ? findModelDisplayName(models, live.defaultModelId) : "跟随应用默认"} onClose={toggleContext} onSettings={() => setSettingsOpen(true)} onFiles={() => showFiles("reference")} onAddFiles={addFiles} onDecisions={() => setDecisionsOpen(true)} onAutomations={() => setAutomationsOpen(true)} automationCount={automation.automations.length} />}
      </div>
      {settingsOpen && createPortal(<ProjectSettingsDialog key={`${live.id}:${sharingAccount}`} project={live} sharedId={sharedId} workspace={workspace} picker={picker} models={models} defaultModelId={inheritedModelId} onClose={() => setSettingsOpen(false)} onOpenAutomation={onOpenAutomation} onOpenModelSettings={onOpenModelSettings} />, document.body)}
      {decisionsOpen && <ProjectDecisionsDialog project={live} sharedId={sharedId} workspace={workspace} onClose={() => setDecisionsOpen(false)} />}
      {automationsOpen && <ProjectAutomationsDialog project={live} data={automation} models={models} defaultModelId={projectModelId} onClose={() => setAutomationsOpen(false)} onOpenSession={openProjectSession} />}
      {resource && <ProjectResourcePreview key={`${sharingAccount}:${resource.output?.id ?? resource.asset?.id}`} project={live} resource={resource} onClose={() => setResource(undefined)} onUse={useResource} onOpenSession={openProjectSession} />}
      {dialog}
    </div>
  );
}
