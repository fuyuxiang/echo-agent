import { useRef, useState } from "react";
import { ArrowRight, ArrowUpRight, Check, Circle, CircleCheck, FileText, LoaderCircle, Pencil, Plus, SlidersHorizontal, CalendarClock, PanelRightClose } from "lucide-react";
import { flushProjectPersistence, useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import type { UnifiedTask } from "@/lib/project-tasks";
import type { TaskArtifact } from "@/lib/artifact-catalog";
import { projectFeed, projectTaskState, PROJECT_TASK_STATES, type ProjectTaskState } from "@/lib/project-workspace-model";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
import { displayPath } from "@/lib/display-path";
import { useAppDialog } from "./AppDialog";
import { TaskDrawer } from "./ProjectTasks";
import { projectCollaborationCapabilities } from "@/lib/team-project-client";

export function ProjectStatus({ status }: { status: ProjectTaskState }) {
  return <span className="project-status" data-state={status}><span aria-hidden="true" />{PROJECT_TASK_STATES[status].label}</span>;
}

export function ProjectOverview({ rows, outputs, onOpenTask, onOpenOutput, onTasks, onFiles, onAddFiles }: {
  project: ProjectMeta; rows: UnifiedTask[]; outputs: TaskArtifact[];
  onOpenTask: (id: string) => void; onOpenOutput: (output: TaskArtifact) => void; onTasks: () => void; onFiles: () => void; onAddFiles: () => void;
}) {
  const summaries = useSessionsStore(s => s.independent);
  const visible = rows.filter(r => !summaries.find(s => s.sessionId === r.sessionId)?.archived && !r.local?.sessionArchived);
  const state = (r: UnifiedTask) => projectTaskState(r, summaries.find(s => s.sessionId === r.sessionId));
  const attention = visible.filter(r => ["attention", "review", "blocked", "syncing"].includes(state(r)));
  const running = visible.filter(r => ["running", "paused"].includes(state(r)));
  const recent = (running.length ? running : visible).slice(0, 3);
  return <div className="project-overview">
    {!!attention.length && <section><div className="project-section-heading"><h2>需要你处理 <span>{attention.length}</span></h2></div>
      {attention.slice(0, 2).map(row => <div className="project-attention" key={row.id}><CircleCheck size={18} /><div><strong>{row.title}</strong><p>{PROJECT_TASK_STATES[state(row)].hint}</p></div><button className="pd-btn" onClick={() => onOpenTask(row.id)}>{state(row) === "review" ? "检查成果" : "查看任务"}<ArrowUpRight size={14} /></button></div>)}
    </section>}
    {!!recent.length && <section><div className="project-section-heading"><h2>{running.length ? "正在推进" : "最近任务"} <span>{running.length || visible.length}</span></h2><button className="pd-btn pd-btn--ghost" onClick={onTasks}>全部任务<ArrowRight size={14} /></button></div>
      {recent.map(row => <button type="button" className="project-overview-task" key={row.id} onClick={() => onOpenTask(row.id)}><span className="project-overview-task__icon">{state(row) === "running" ? <LoaderCircle size={17} /> : state(row) === "completed" ? <CircleCheck size={17} /> : <Circle size={17} />}</span><span><strong>{row.title}</strong><small>{row.remote?.assigneeName ?? "我"} · {PROJECT_TASK_STATES[state(row)].hint}</small></span><ProjectStatus status={state(row)} /><ArrowUpRight size={14} /></button>)}
    </section>}
    {!!outputs.length && <section><div className="project-section-heading"><h2>最近成果</h2><button className="pd-btn pd-btn--ghost" onClick={onFiles}>全部成果<ArrowRight size={14} /></button></div><div className="project-output-grid">{outputs.slice(0, 2).map(output => <button className="project-output-card" key={`${output.sessionId}:${output.id}`} onClick={() => onOpenOutput(output)}><FileText size={23} /><span><strong>{output.path.replace(/\\/g, "/").split("/").pop()}</strong><small>{output.sessionTitle} · {new Date(output.updatedAt).toLocaleDateString()}</small></span></button>)}</div></section>}
    {!rows.length && <div className="project-workspace-empty"><FileText size={28} /><h2>从第一项工作开始</h2><p>在下方描述你想得到的结果，新任务会沿用项目背景与指令。</p><button className="pd-btn" onClick={onAddFiles}><Plus size={15} />先添加参考资料</button></div>}
    {!outputs.length && !!rows.length && <p className="project-workspace-hint">任务生成的文件会汇集到「资料与成果」，并保留来源任务。</p>}
  </div>;
}

export function ProjectContextSidebar({ project, workspace, modelName, onClose, onSettings, onFiles, onAddFiles, onDecisions, onAutomations, automationCount }: {
  project: ProjectMeta; workspace: ProjectWorkspace; modelName: string; onClose: () => void; onSettings: () => void;
  onFiles: () => void; onAddFiles: () => void; onDecisions: () => void; onAutomations: () => void; automationCount: number;
}) {
  const decisions = workspace.snapshot?.decisions ?? project.decisions ?? [];
  const active = decisions.filter(d => d.status === "active");
  const proposed = decisions.filter(d => d.status === "proposed");
  return <aside className="project-workspace-context" aria-label="项目上下文">
    <div className="project-context-heading"><h2>项目上下文</h2><button className="pd-btn pd-btn--ghost" aria-label="收起项目上下文" onClick={onClose}><PanelRightClose size={16} /></button></div>
    <section><div className="project-context-heading"><h3>项目指令</h3><button className="pd-btn pd-btn--ghost" aria-label="编辑项目指令" onClick={onSettings}><Pencil size={14} /></button></div><p className="project-context-prose">{project.instructions || "添加背景、工作规范与交付要求，供项目任务沿用。"}</p><small>适用于项目中的新任务</small></section>
    <section><div className="project-context-heading"><h3>参考资料 <span>{workspace.snapshot?.assets.length ?? project.assets.length}</span></h3><button className="pd-btn pd-btn--ghost" aria-label="添加参考资料" onClick={onAddFiles}><Plus size={15} /></button></div>{project.assets.slice(0, 2).map(a => <button className="project-context-file" key={a.id} onClick={onFiles}><FileText size={14} />{a.name}</button>)}<button className="pd-btn pd-btn--ghost" onClick={onFiles}>查看全部资料<ArrowUpRight size={13} /></button></section>
    <section><div className="project-context-heading"><h3>执行偏好</h3><button className="pd-btn pd-btn--ghost" aria-label="配置执行偏好" onClick={onSettings}><SlidersHorizontal size={14} /></button></div><div className="project-context-model"><span>模型</span><span>{modelName}</span></div><div className="project-context-capabilities">{[...project.experts, ...project.skills].slice(0, 3).map((r, i) => <span key={`${r.id}:${i}`}>{r.name}</span>)}</div><small>{project.experts.length} 位专家 · {project.skills.length} 项技能 · {project.connectors.length} 个连接器</small></section>
    <section><div className="project-context-heading"><h3>项目记忆与决定</h3><button className="pd-btn pd-btn--ghost" aria-label="管理项目记忆与决定" onClick={onDecisions}><ArrowUpRight size={14} /></button></div><small>{active.length} 条已确认{proposed.length > 0 && ` · ${proposed.length} 条待确认`}</small>{active[0] && <p className="project-context-prose">{active[0].content}</p>}</section>
    <section><div className="project-context-heading"><h3>定时工作</h3><button className="pd-btn pd-btn--ghost" aria-label="管理项目定时工作" onClick={onAutomations}><Plus size={15} /></button></div><button className="project-context-file" onClick={onAutomations}><CalendarClock size={16} />{automationCount ? `${automationCount} 项定时工作` : "设置周期任务"}</button><small>{project.cwd ? displayPath(project.cwd) : "使用独立任务目录"}</small></section>
  </aside>;
}

export function ProjectActivity({ project, sharedId, workspace, onOpenTask, draftScope = project.id }: { project: ProjectMeta; sharedId?: string; workspace: ProjectWorkspace; onOpenTask: (id: string) => void; draftScope?: string }) {
  const summaries = useSessionsStore(s => s.independent);
  const draftKey = `project-comment:${draftScope}`;
  const identityKey = `${draftKey}:request`;
  const [text, setText] = useState(() => useSessionsStore.getState().drafts[draftKey] ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const requestIdentity = useRef<{ text: string; id: string }>();
  const updateText = (value: string) => { setText(value); useSessionsStore.getState().setDraft(draftKey, value); };
  const feed = projectFeed(project, workspace.snapshot, summaries);
  const submit = async () => {
    if (!text.trim() || busy) return;
    setBusy(true); setError("");
    try {
      if (!requestIdentity.current) {
        try { requestIdentity.current = JSON.parse(useSessionsStore.getState().drafts[identityKey] ?? "null") ?? undefined; } catch { /* Ignore a damaged cached draft identity. */ }
      }
      if (!requestIdentity.current || requestIdentity.current.text !== text.trim()) requestIdentity.current = { text: text.trim(), id: crypto.randomUUID() };
      useSessionsStore.getState().setDraft(identityKey, JSON.stringify(requestIdentity.current));
      if (sharedId) {
        if (!workspace.snapshot) throw new Error("项目连接不可用，请刷新后重试");
        await workspace.mutate("createComment", { content: text.trim(), clientRequestId: requestIdentity.current.id });
      } else { useProjectsStore.getState().recordActivity(project.id, { id: requestIdentity.current.id, kind: "comment", content: text.trim() }); await flushProjectPersistence(); }
      requestIdentity.current = undefined;
      useSessionsStore.getState().setDraft(identityKey, "");
      updateText("");
    }
    catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const canComment = !sharedId || !!workspace.snapshot && Array.isArray(workspace.snapshot.comments) && workspace.snapshot.role !== "viewer";
  return <div className="project-activity"><div className="project-activity-feed">{feed.map(e => <article className="project-activity-entry" key={e.id}><span className="project-actor-avatar">{e.actor.slice(-1)}</span><div><p><strong>{e.actor}</strong> {e.content}</p><small>{new Date(e.time).toLocaleString()}</small>{e.taskId && <button className="pd-btn pd-btn--ghost" onClick={() => onOpenTask(e.taskId!)}>查看任务<ArrowUpRight size={13} /></button>}</div></article>)}</div>{canComment ? <div className="project-activity-composer"><label htmlFor="project-comment">给项目留个言{workspace.snapshot && "（项目成员可见）"}</label><textarea id="project-comment" value={text} maxLength={8000} disabled={busy} onChange={e => updateText(e.target.value)} placeholder="记录进展、补充背景或提出问题…" rows={3} /><button className="pd-btn pd-btn--primary" disabled={busy || !text.trim()} onClick={() => void submit()}>{busy ? "保存中…" : "发布留言"}</button>{error && <p role="alert">{error}</p>}</div> : <p className="project-workspace-hint">{workspace.snapshot?.role === "viewer" ? "当前为仅查看权限。" : "组织服务升级后可发布共享项目留言。"}</p>}</div>;
}

export function ProjectDecisionsDialog({ project, sharedId, workspace, onClose }: { project: ProjectMeta; sharedId?: string; workspace: ProjectWorkspace; onClose: () => void }) {
  const { requestInput, requestConfirmation, dialog } = useAppDialog(`${project.id}:decisions`);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const decisions = sharedId ? workspace.snapshot?.decisions ?? [] : project.decisions ?? [];
  const writable = !sharedId || workspace.snapshot?.role === "admin";
  const canPropose = writable || workspace.snapshot?.role === "collaborator";
  const save = (id?: string) => {
    const current = decisions.find(d => d.id === id);
    requestInput({ title: current ? "更正项目决定" : "记录项目决定", fields: [
      { name: "content", label: "决定内容", defaultValue: current?.content, required: true, multiline: true, maxLength: 8000 },
      { name: "rationale", label: "理由与依据", defaultValue: current?.rationale, multiline: true, maxLength: 8000 },
    ], action: async values => {
      if (sharedId) {
        if (current && !(await projectCollaborationCapabilities()).decisionRevisions) throw new Error("组织服务需升级后才能更正项目决定");
        await workspace.mutate(current ? "updateDecision" : "createDecision", { ...values, ...(current && "revision" in current ? { expectedRevision: current.revision } : {}) }, current?.id);
      }
      else { useProjectsStore.getState().saveDecision(project.id, { content: values.content, rationale: values.rationale }, current?.id); useProjectsStore.getState().recordActivity(project.id, { kind: "decision", content: `${current ? "更正" : "记录"}了项目决定：${values.content}` }); await flushProjectPersistence(); }
    } });
  };
  const changeStatus = async (id: string, active: boolean) => {
    setBusy(true); setError("");
    try {
      const d = decisions.find(d => d.id === id)!;
      if (sharedId && "revision" in d) await workspace.mutate("updateDecision", { expectedRevision: d.revision, status: active ? "active" : "retired" }, id);
      else { useProjectsStore.getState().setDecisionRetired(project.id, id, !active); await flushProjectPersistence(); }
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  return <><TaskDrawer title="项目记忆与决定" onClose={onClose}><p className="project-workspace-hint">已确认的内容作为后续任务依据；更正与作废保留历史记录。</p>{canPropose && <button className="pd-btn" onClick={() => save()}><Plus size={14} />记录决定</button>}{!decisions.length && <p className="project-workspace-hint">尚未记录项目决定。</p>}{decisions.map(d => <article className="project-decision" key={d.id}><span className="project-status" data-state={d.status === "active" ? "completed" : "paused"}>{({ active: "已确认", proposed: "待确认", retired: "已作废", superseded: "已替代" })[d.status]}</span><p>{d.content}</p>{d.rationale && <small>{d.rationale}</small>}{"history" in d && !!d.history?.length && <details><summary>更正记录</summary>{d.history.map((h, i) => <p key={i}>{h.content} · {new Date(h.updatedAt).toLocaleString()}</p>)}</details>}{writable && <div className="project-task-actions"><button className="pd-btn pd-btn--ghost" disabled={busy} onClick={() => save(d.id)}>更正</button>{d.status !== "active" ? <button className="pd-btn" disabled={busy || d.status === "superseded"} onClick={() => void changeStatus(d.id, true)}><Check size={14} />{d.status === "proposed" ? "确认采用" : "恢复"}</button> : <button className="pd-btn pd-btn--ghost" disabled={busy} onClick={() => requestConfirmation({ title: "作废这条项目决定？", description: "作废后不再作为后续任务依据，原记录保留。", confirmLabel: "作废决定", action: () => changeStatus(d.id, false) })}>作废</button>}</div>}</article>)}{error && <p role="alert" className="project-inline-error">{error}</p>}</TaskDrawer>{dialog}</>;
}
