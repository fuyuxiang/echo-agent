import { Select } from "@/components/Select";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ArrowLeft, ArrowUpRight, Circle, FileText, Pause, Plus } from "lucide-react";
import { flushProjectPersistence } from "@/stores/projects-store";
import { projectTaskState, PROJECT_TASK_STATES } from "@/lib/project-workspace-model";
import { pauseProjectSession } from "@/lib/pause-project-session";
import type { TaskArtifact } from "@/lib/artifact-catalog";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { projectTaskRows, type UnifiedTask } from "@/lib/project-tasks";
import { projectPublicationLinks } from "@/lib/project-sharing";
import { captureTeamProjectOperation, projectCollaborationCapabilities, TEAM_TASK_LABELS, type StartTeamWork, type OpenTeamExecution } from "@/lib/team-project-client";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useAppDialog } from "./AppDialog";
import { ModelSelector, type ModelOption } from "./ModelSelector";
import { Markdown } from "./markdown/Markdown";
import { displayPath } from "@/lib/display-path";

interface Props {
  project: ProjectMeta; account: string; sharedId?: string; workspace: ProjectWorkspace;
  models: ModelOption[]; defaultModelId?: string; runDisabled?: boolean;
  executionCwd?: string; onSelectExecutionWorkspace?: () => Promise<void>;
  onRun?: (message: string, modelId: string) => Promise<string | undefined>;
  onStartTeamWork?: StartTeamWork; onOpenTeamExecution?: OpenTeamExecution;
  onOpenSession?: (sessionId: string) => void; onRenameSession?: (sessionId: string, title: string) => Promise<void>; onArchiveSession?: (sessionId: string, archived: boolean) => Promise<void>;
  selectedTaskId?: string; onSelectedTaskChange?: (id?: string) => void; inlineDetails?: boolean;
  outputs?: TaskArtifact[]; onPreviewOutput?: (output: TaskArtifact) => void;
}
export function ProjectTasks(props: Props) {
  const { project, account, sharedId, workspace, models, defaultModelId, onRun, runDisabled, onStartTeamWork, onOpenTeamExecution, onOpenSession, onArchiveSession, onRenameSession } = props;
  const session = useOrgSessionStore(s => s.session);
  const summaries = useSessionsStore(s => s.independent);
  const rows = projectTaskRows(project, workspace.snapshot, account, projectPublicationLinks(project.id, account, sharedId).tasks, session?.user?.id, sharedId);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("active");
  const [selected, setSelected] = useState<string>(props.selectedTaskId ?? "");
  useEffect(() => { setSelected(props.selectedTaskId ?? ""); }, [props.selectedTaskId]);
  const select = (id?: string) => { setSelected(id ?? ""); props.onSelectedTaskChange?.(id); };
  const [stateFilter, setStateFilter] = useState("all");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const [modelOverride, setModelOverride] = useState<string>();
  const { requestInput, requestConfirmation, dialog } = useAppDialog(`${project.id}:${account}`);
  const summary = (row: UnifiedTask) => summaries.find(s => s.sessionId === row.sessionId);
  const archived = (row: UnifiedTask) => row.private && (summary(row)?.archived ?? row.local?.sessionArchived ?? false);
  const writable = !sharedId || workspace.snapshot?.role === "admin" || workspace.snapshot?.role === "collaborator";
  const current = rows.find(row => row.id === selected);
  // Publishing a task does not publish its former private conversation. Keep
  // that source reachable separately from native-bound collaborative runs.
  const originalSessionId = current?.local?.sessionId && !current.private && workspace.snapshot
    && !workspace.snapshot.runs?.some(run => run.sessionId === current.local?.sessionId)
    ? current.local.sessionId : undefined;
  const originalArchived = !!originalSessionId && (summaries.find(s => s.sessionId === originalSessionId)?.archived ?? current?.local?.sessionArchived ?? false);
  const state = (row: UnifiedTask) => projectTaskState(row, summary(row));
  const canPause = (row: UnifiedTask) => !!row.sessionId && ["working", "planning", "awaiting_permission", "awaiting_answer", "awaiting_approval"].includes(summary(row)?.status ?? "") && (row.private || row.remote?.status === "in_progress" && row.remote.assigneeId === session?.user?.id && !!workspace.snapshot?.runs?.some(r => r.taskId === row.remote?.id && r.sessionId === row.sessionId && r.status === "running"));
  const visible = rows.filter(row => row.title.toLowerCase().includes(query.toLowerCase()) && (stateFilter === "all" || state(row) === stateFilter) && (filter === "all" || (filter === "archived" ? archived(row) : !archived(row))));
  const run = async (work: () => Promise<unknown>) => {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError("");
    try { await work(); } catch (error) { setError(String(error).replace(/^Error:\s*/, "")); }
    finally { lock.current = false; setBusy(false); }
  };
  const publishTask = async (localId: string) => {
    const local = useProjectsStore.getState().projects.find(p => p.id === project.id)?.tasks.find(t => t.id === localId);
    const link = local?.sharedTasks?.[account];
    if (!local || !sharedId || link?.projectId !== sharedId || !link.requestId) throw new Error("任务关联尚未保存，请重试");
    const identity = captureTeamProjectOperation();
    const caps = await projectCollaborationCapabilities(identity);
    identity.assertCurrent();
    if (!caps.taskIdentity) throw new Error("组织服务需升级后才能新建协作任务；已有任务和历史记录仍保留");
    const created = await workspace.mutate<{ id: string }>("createTask", { title: local.title, description: local.description ?? "", acceptance: local.acceptance ?? "", clientRequestId: link.requestId });
    if (!created?.id) throw new Error("创建结果尚未确认，请重试同步同一任务");
    identity.assertCurrent();
    await useProjectsStore.getState().linkSharedTask(project.id, local.id, account, { ...link, taskId: created.id });
    await workspace.refresh();
  };
  const create = () => requestInput({ title: "新建任务", description: sharedId ? "新任务对项目成员可见。" : undefined,
    fields: [{ name: "title", label: "任务名称", required: true, maxLength: 300 }, { name: "description", label: "任务说明", multiline: true, maxLength: 12000 }, { name: "acceptance", label: "验收要求", multiline: true, maxLength: 4000 }], confirmLabel: "创建任务",
    action: async values => {
      if (sharedId && !writable) throw new Error("当前无法创建协作任务，请刷新项目或检查权限");
      const store = useProjectsStore.getState();
      const id = store.addTask(project.id, values.title.trim());
      store.updateTask(project.id, id, { description: values.description, acceptance: values.acceptance });
      await flushProjectPersistence();
      select(id);
      if (sharedId) {
        await store.linkSharedTask(project.id, id, account, { projectId: sharedId, requestId: crypto.randomUUID() });
        try { await publishTask(id); } catch (error) { setError(`${String(error)}。任务已保存在本机，可在详情中重试同步。`); }
      }
    } });
  const edit = (row: UnifiedTask) => requestInput({ title: "编辑任务", fields: [
    { name: "title", label: "任务名称", required: true, maxLength: 300, defaultValue: row.title },
    { name: "description", label: "任务说明", multiline: true, maxLength: 12000, defaultValue: row.remote?.description ?? row.local?.description ?? "" },
    { name: "acceptance", label: "验收要求", multiline: true, maxLength: 4000, defaultValue: row.remote?.acceptance ?? row.local?.acceptance ?? "" },
  ], confirmLabel: "保存", action: async values => {
    if (row.remote) await workspace.mutate("updateTask", { ...values, expectedRevision: row.remote.revision, ...(row.remote.status === "accepted" && (values.description !== row.remote.description || values.acceptance !== row.remote.acceptance) ? { status: "review" } : {}) }, row.remote.id);
    else if (row.private && row.local) { if (row.sessionId && onRenameSession) await onRenameSession(row.sessionId, values.title); useProjectsStore.getState().updateTask(project.id, row.local.id, values); if (row.sessionId) useProjectsStore.getState().updateConversationTitle(project.id, row.sessionId, values.title); }
    else throw new Error("请先同步任务再编辑");
  } });
  const start = (row: UnifiedTask) => run(async () => {
    const modelId = modelOverride ?? row.local?.modelId ?? defaultModelId;
    if (!modelId || !models.some(m => m.id === modelId)) throw new Error("请先选择可用模型");
    if (row.remote && workspace.snapshot && onStartTeamWork) {
      const identity = captureTeamProjectOperation();
      const sessionId = await onStartTeamWork(workspace.snapshot, row.remote, modelId);
      identity.assertCurrent();
      if (!sessionId) throw new Error("任务尚未启动，请检查模型与配额后重试");
      if (row.local) useProjectsStore.getState().linkTaskSession(project.id, row.local.id, sessionId, modelId);
      await workspace.refresh();
    } else if (row.private && row.local && onRun) {
      const previous = row.local.status;
      useProjectsStore.getState().moveTask(project.id, row.local.id, "in_progress");
      try {
        const sessionId = await onRun(`请执行项目任务「${row.title}」。\n${row.local.description ?? ""}\n验收要求：${row.local.acceptance || "产出可验收结果，如有阻塞请说明。"}`, modelId);
        if (sessionId) useProjectsStore.getState().linkTaskSession(project.id, row.local.id, sessionId, modelId);
        else { useProjectsStore.getState().moveTask(project.id, row.local.id, previous); throw new Error("任务尚未启动，请检查提示后重试。"); }
      } catch (error) { useProjectsStore.getState().moveTask(project.id, row.local.id, previous); throw error; }
    } else throw new Error("请先恢复项目连接后执行");
  });
  const requestStart = (row: UnifiedTask) => {
    const takingOver = row.remote?.assigneeId && row.remote.assigneeId !== session?.user?.id;
    const running = workspace.snapshot?.runs?.some(r => r.taskId === row.remote?.id && r.status === "running");
    if (takingOver || running) requestConfirmation({ title: takingOver ? "接手并执行任务？" : "开始新的执行？", description: "将创建新的执行；旧记录和报告会保留。负责人将变更为你，旧执行的晚到结果不会改变当前任务状态。", confirmLabel: "开始执行", action: () => start(row) });
    else void start(row);
  };
  const taskStatus = (row: UnifiedTask) => PROJECT_TASK_STATES[state(row)].label;
  const inline = props.inlineDetails && !!current;
  return <div className="project-unified-tasks">
    {!inline && <><div className="pd-toolbar"><div className="project-state-filters">{[['all','全部'],['running','进行中'],['attention','需处理'],['review','待验收'],['completed','已验收']].map(([value,label]) => <button type="button" className="pd-btn pd-btn--ghost" key={value} aria-pressed={stateFilter === value} onClick={() => setStateFilter(value)}>{label}</button>)}</div><button type="button" className="pd-btn" onClick={create} disabled={busy || !writable}><Plus size={14} />添加待办</button></div><div className="project-material-search"><span>{rows.length} 项任务</span><div className="pd-toolbar__right"><Select aria-label="任务范围" value={filter} onChange={e => setFilter(e.target.value)}><option value="active">当前任务</option><option value="all">全部任务</option><option value="archived">已归档</option></Select><input className="pd-search-inline" aria-label="搜索任务" placeholder="搜索任务…" value={query} onChange={e => setQuery(e.target.value)} /></div></div></>}
    {error && !current && <p className="project-inline-error" role="alert">{error}</p>}
    {!inline && !visible.length && <div className="project-workspace-empty"><Circle size={24} /><strong>{query || filter === "archived" || stateFilter !== "all" ? "没有符合条件的任务" : "从一个任务开始"}</strong><p>在下方发起新任务，或添加待办后再执行。</p></div>}
    {!inline && <div className="project-task-list">{visible.map(row => <button type="button" className="project-task-row" key={row.id} onClick={() => { select(row.id); setModelOverride(undefined); }}><span className="project-task-dot" data-status={state(row)} /><span className="project-task-row__name">{row.title}<small>{row.remote?.assigneeName ?? (sharedId && row.private ? "仅自己" : !row.private && !row.remote ? "等待同步到项目" : "我")} · {PROJECT_TASK_STATES[state(row)].hint}</small></span><span className="project-status" data-state={state(row)}>{archived(row) ? "已归档" : taskStatus(row)}</span><ArrowUpRight size={14} /></button>)}</div>}
    {current && <TaskSurface inline={!!props.inlineDetails} title={current.title} onClose={() => { if (!busy) select(undefined); }}>
      <div className="project-task-detail-meta"><span>{taskStatus(current)}</span><span>{current.remote?.assigneeName ?? (current.private ? "仅自己" : "项目成员可见")}</span></div>
      <p className="project-task-description">{current.remote?.description || current.local?.description || "尚未填写任务说明"}</p>
      {!!props.outputs?.some(o => o.sessionId === current.sessionId) && <section><h4>交付成果</h4><div className="project-output-grid">{props.outputs.filter(o => o.sessionId === current.sessionId).map(o => <button className="project-output-card" key={o.id} onClick={() => props.onPreviewOutput?.(o)}><FileText size={21} /><span><strong>{o.path.replace(/\\/g, "/").split("/").pop()}</strong><small>打开文件预览</small></span></button>)}</div></section>}
      {(current.remote?.acceptance || current.local?.acceptance) && <section><h4>验收要求</h4><p>{current.remote?.acceptance ?? current.local?.acceptance}</p></section>}
      {originalSessionId && onOpenSession && <section aria-label="本机原会话"><h4>本机原会话</h4><p className="project-workspace-hint">原讨论上下文保留在本机。共享任务与公开执行报告可在本页查看。</p><button className="pd-btn" disabled={busy} onClick={() => void run(async () => {
        if (originalArchived) {
          if (!onArchiveSession) throw new Error("原会话已归档，请先恢复后打开。");
          await onArchiveSession(originalSessionId, false);
        }
        onOpenSession(originalSessionId);
      })}>{originalArchived ? "恢复并打开本机原会话" : "打开本机原会话"}</button></section>}
      <div className="project-task-actions">
        {(current.private || current.remote && writable) && <button className="pd-btn" disabled={busy} onClick={() => edit(current)}>编辑任务</button>}
        {current.sessionId && current.private && onOpenSession && <button className="pd-btn" disabled={busy} onClick={() => void run(async () => { if (archived(current)) { if (!onArchiveSession) throw new Error("请先在任务列表恢复归档"); await onArchiveSession(current.sessionId!, false); } onOpenSession(current.sessionId!); })}>{archived(current) ? "恢复并继续任务" : "继续任务"}</button>}
        {canPause(current) && (current.private || current.remote && writable) && <button className="pd-btn" disabled={busy} onClick={() => void run(() => pauseProjectSession(current.sessionId!))}><Pause size={14} />暂停任务</button>}
        {current.private && current.local && state(current) === "review" && <button className="pd-btn pd-btn--primary" disabled={busy} onClick={() => requestConfirmation({ title: "确认验收任务？", description: "请先检查交付成果并核对验收要求。", confirmLabel: "确认验收", action: () => run(async () => { useProjectsStore.getState().updateTask(project.id, current.local!.id, { status: "completed", acceptedAt: new Date().toISOString() }); useProjectsStore.getState().recordActivity(project.id, { kind: "task", content: `验收了任务「${current.title}」`, resourceId: current.id }); await flushProjectPersistence(); }) })}>确认验收</button>}
        {!current.private && !current.remote && current.local?.sharedTasks?.[account]?.requestId && <button className="pd-btn" disabled={busy || !writable} onClick={() => void run(() => publishTask(current.local!.id))}>重试同步</button>}
        {(current.private && !current.sessionId || current.remote && writable) && <label>工作项状态<Select aria-label="任务状态" disabled={busy || state(current) === "running" || state(current) === "attention"} value={current.remote?.status ?? current.local?.status} onChange={e => {
          const status = e.target.value;
          if (status === "in_progress") return;
          const save = async () => {
            if (current.remote) await workspace.mutate("updateTask", { expectedRevision: current.remote.revision, status }, current.remote.id);
            else { useProjectsStore.getState().updateTask(project.id, current.local!.id, { status: status as "pending" | "completed", acceptedAt: status === "completed" ? new Date().toISOString() : undefined }); await flushProjectPersistence(); }
          };
          if (status === "accepted" || status === "completed") requestConfirmation({ title: "确认验收任务？", description: "请先查看成果并核对验收要求。", confirmLabel: "确认验收", action: () => run(save) }); else void run(save);
        }}>{current.remote ? Object.entries(TEAM_TASK_LABELS).map(([value,label]) => <option value={value} key={value} disabled={value === "in_progress" || value === "accepted" && workspace.snapshot?.role !== "admin"}>{label}</option>) : <><option value="pending">待开始</option><option value="completed">已验收</option></>}</Select></label>}
      </div>
      {(current.private || current.remote && writable) && !archived(current) && <section><h4>交给 Agent</h4>{props.onSelectExecutionWorkspace && (!current.private || !current.sessionId) && <div className="project-task-actions"><span className="project-workspace-hint">执行目录：{props.executionCwd ? displayPath(props.executionCwd) : "自动分配独立任务目录"}</span><button type="button" className="pd-btn" disabled={busy} onClick={() => void run(props.onSelectExecutionWorkspace!)}>选择执行目录</button></div>}<div className="project-task-actions"><ModelSelector models={models} modelId={modelOverride ?? current.local?.modelId ?? defaultModelId} onModelChange={setModelOverride} ariaLabel="任务执行模型" menuZIndex="var(--echo-layer-dialog-popover)" /><button className="pd-btn pd-btn--primary" disabled={busy || runDisabled || (current.remote ? !onStartTeamWork || current.remote.status === "accepted" : !onRun || !!current.sessionId)} onClick={() => requestStart(current)}>开始执行</button></div></section>}
      {current.remote && <section><h4>执行记录与成果</h4>{!workspace.snapshot?.runs?.some(r => r.taskId === current.remote?.id) && <p className="project-collaboration-hint">执行后，进展和报告会显示在这里。</p>}{workspace.snapshot?.runs?.filter(r => r.taskId === current.remote?.id).map((execution, i) => <article className="project-task-run" key={execution.id}><strong>{({ running:"执行中",completed:"本轮已结束",failed:"执行失败",cancelled:"已取消" })[execution.status]}</strong><small>{workspace.snapshot?.members.find(m => m.userId === execution.actorId)?.displayName ?? "原项目成员"} · {new Date(execution.updatedAt).toLocaleString()}</small>{execution.summary && <details open={i === 0}><summary>执行报告</summary><Markdown complete>{execution.summary}</Markdown></details>}{onOpenTeamExecution && writable && execution.actorId === session?.user?.id && <button className="pd-btn" disabled={busy} onClick={() => void run(() => onOpenTeamExecution(workspace.snapshot!, execution))}>打开并继续任务</button>}</article>)}</section>}
      {current.private && current.sessionId && onArchiveSession && <button className="pd-btn" disabled={busy} onClick={() => void run(() => onArchiveSession(current.sessionId!, !archived(current)))}>{archived(current) ? "恢复任务" : "归档任务"}</button>}
      {current.private && current.local && <button className="pd-btn" disabled={busy} onClick={() => requestConfirmation({ title: "从项目移除任务？", description: "仅移除项目中的任务关联，历史会话和原文件会保留。", confirmLabel: "移除任务", danger: true, action: () => { useProjectsStore.getState().removeTask(project.id, current.local!.id); select(undefined); } })}>移除任务</button>}
      {error && <p role="alert" className="project-inline-error">{error}</p>}
    </TaskSurface>}{dialog}
  </div>;
}
function TaskSurface({ inline, title, children, onClose }: { inline: boolean; title: string; children: React.ReactNode; onClose: () => void }) {
  if (!inline) return <TaskDrawer title={title} onClose={onClose}>{children}</TaskDrawer>;
  return <section className="project-task-inline" aria-label={`任务：${title}`}><button className="pd-btn pd-btn--ghost" onClick={onClose}><ArrowLeft size={15} />返回任务列表</button><h2>{title}</h2>{children}</section>;
}
export function TaskDrawer({ title, children, onClose }: { title: string; children: React.ReactNode; onClose: () => void }) {
  const ref = useModalFocus<HTMLDivElement>(true, onClose);
  return createPortal(<div className="modal-overlay" onClick={onClose}><div ref={ref} className="create-colleague-dialog project-task-drawer" role="dialog" aria-modal="true" aria-label={`任务：${title}`} tabIndex={-1} onClick={e => e.stopPropagation()}><div className="create-colleague-header"><h3>{title}</h3><button className="create-colleague-close" aria-label="关闭任务详情" onClick={onClose}>×</button></div><div className="create-colleague-body">{children}</div></div></div>, document.body);
}
