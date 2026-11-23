import { Select } from "@/components/Select";
import { useCallback, useEffect, useRef, useState } from "react";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { agentsList, filesystemPickFiles } from "@/lib/agent-client";
import { orgDownloadDocument, orgFetchDocument } from "@/lib/org-client";
import { groupTemplates, type TeamTemplate } from "@/features/groups/group-client";
import type { AgentEntry } from "@/lib/types";
import type { ModelOption } from "./ModelSelector";
import { useAppDialog } from "./AppDialog";
import { Markdown } from "./markdown/Markdown";
import { canEditTeamProject, captureTeamProjectOperation, projectRoleUnavailable, TEAM_TASK_LABELS, teamProjectRequest, teamProjectUpload, type OpenTeamExecution, type ProjectAnswer, type ProjectCitation, type SharedAsset, type StartTeamWork, type TeamProject, type TeamProjectSnapshot, type TeamTask, type TeamTaskStatus } from "@/lib/team-project-client";
import "@/styles/team-projects.css";
import { ProjectParticipantsButton } from "./ProjectParticipantsButton";

interface Props { embedded?: boolean; selectedId: string | null; onSelect: (id: string | null) => void; onStartWork?: StartTeamWork; onOpenExecution?: OpenTeamExecution; models?: ModelOption[]; defaultModelId?: string; onOpenOrganization?: () => void }

/** Server snapshots stay in memory and are discarded on every account boundary. */
export function TeamProjectsPanel(props: Props) {
  const boundary = useOrgSessionStore(state => state.boundaryRevision);
  return <TeamProjectsWorkspace key={`${boundary}:${props.selectedId ?? "list"}`} {...props} />;
}
function TeamProjectsWorkspace({ embedded = false, selectedId, onSelect, onStartWork, onOpenExecution, models = [], defaultModelId, onOpenOrganization }: Props) {
  const session = useOrgSessionStore(s => s.session);
  const boundary = useOrgSessionStore(s => s.boundaryRevision);
  const [projects, setProjects] = useState<TeamProject[]>([]);
  const [project, setProject] = useState<TeamProjectSnapshot | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<"overview" | "tasks" | "assets" | "runs">("overview");
  const [answer, setAnswer] = useState<ProjectAnswer | null>(null);
  const [question, setQuestion] = useState("");
  const [preview, setPreview] = useState<{ title: string; text: string } | null>(null);
  const [experts, setExperts] = useState<AgentEntry[]>([]);
  const [teams, setTeams] = useState<TeamTemplate[]>([]);
  const [overrideModel, setOverrideModel] = useState(false);
  const [capability, setCapability] = useState("");
  const [modelId, setModelId] = useState(defaultModelId ?? models[0]?.id ?? "");
  const [replacement, setReplacement] = useState("");
  const [runTaskId, setRunTaskId] = useState("");
  const generation = useRef(0);
  const snapshotRevision = useRef<number | null>(null);
  const operation = useRef(false);
  useEffect(() => {
    if (!models.some(model => model.id === modelId)) {
      setModelId(models.find(model => model.id === defaultModelId)?.id ?? models[0]?.id ?? "");
      setOverrideModel(false);
    }
  }, [models, defaultModelId, modelId]);
  const { requestInput, requestConfirmation, dialog } = useAppDialog(`${boundary}:${selectedId ?? "team-projects"}`);
  const reload = useCallback(async () => {
    const current = generation.current;
    if (!session?.loggedIn) return;
    if (selectedId) {
      try {
        const value = await teamProjectRequest<TeamProjectSnapshot>("snapshot", selectedId);
        if (current === generation.current) {
          if (snapshotRevision.current !== value.revision) setPreview(null);
          snapshotRevision.current = value.revision;
          setProject(value);
          setAnswer(previous => previous?.projectRevision === value.revision ? previous : null);
        }
      } catch (error) {
        if (current === generation.current) { setProject(null); setAnswer(null); setPreview(null); }
        throw error;
      }
    } else {
      const value = await teamProjectRequest<TeamProject[]>("list");
      if (current === generation.current) setProjects(value);
    }
  }, [session?.loggedIn, selectedId]);
  useEffect(() => {
    generation.current += 1;
    snapshotRevision.current = null;
    setProjects([]); setProject(null); setAnswer(null); setPreview(null); setError(""); setQuestion(""); setRunTaskId(""); setCapability(""); setOverrideModel(false); setExperts([]); setTeams([]); setBusy(false); operation.current = false;
    void reload().catch(e => setError(String(e)));
    return () => { generation.current += 1; };
  }, [boundary, reload]);
  useEffect(() => {
    if (!selectedId || !session?.loggedIn) return;
    const current = generation.current;
    void Promise.all([agentsList(), groupTemplates()]).then(([localExperts, templates]) => {
      if (generation.current === current) { setExperts(localExperts); setTeams(templates); }
    }).catch(() => { if (generation.current === current) { setExperts([]); setTeams([]); } });
  }, [selectedId, session?.loggedIn, boundary]);
  useEffect(() => {
    if (!selectedId || !session?.loggedIn) return;
    const refresh = () => {
      if (document.visibilityState !== "hidden" && !operation.current) void reload().catch(e => setError(String(e)));
    };
    const timer = window.setInterval(refresh, 15000);
    window.addEventListener("focus", refresh);
    return () => { clearInterval(timer); window.removeEventListener("focus", refresh); };
  }, [selectedId, session?.loggedIn, reload, boundary]);
  const run = async (work: () => Promise<void>) => {
    if (operation.current) return;
    const current = generation.current; operation.current = true; setBusy(true); setError("");
    try { await work(); }
    catch (e) { if (current === generation.current) { setError(String(e)); setAnswer(null); setPreview(null); } }
    finally { if (current === generation.current) { operation.current = false; setBusy(false); } }
  };
  const mutate = async (action: Parameters<typeof teamProjectRequest>[0], body?: unknown, resource?: string) => {
    const identity = captureTeamProjectOperation();
    const current = generation.current;
    await teamProjectRequest(action, selectedId!, body, resource, identity);
    identity.assertCurrent();
    if (current !== generation.current) return;
    setAnswer(null); await reload();
  };
  const ask = (text: string) => run(async () => {
    const current = generation.current;
    const value = await teamProjectRequest<ProjectAnswer>("ask", selectedId!, { question: text });
    if (current === generation.current) { setAnswer(value); setTab("overview"); }
  });
  const openCitation = (citation: ProjectCitation) => run(async () => {
    const current = generation.current;
    const identity = captureTeamProjectOperation();
    // Reauthorize even structured citations before showing their saved excerpt.
    const latest = await teamProjectRequest<TeamProjectSnapshot>("snapshot", selectedId!);
    if (latest.revision !== answer?.projectRevision) {
      if (current === generation.current) { setProject(latest); setAnswer(null); setPreview(null); }
      throw new Error("项目依据已更新，请重新提问后打开来源");
    }
    identity.assertCurrent();
    if (current !== generation.current) return;
    const text = citation.kind.startsWith("project") ? citation.quote : (await orgFetchDocument(citation.resourceId, undefined, undefined, identity.expectedGeneration)).text;
    if (current === generation.current) setPreview({ title: citation.title, text });
  });
  const upload = (asset?: SharedAsset) => run(async () => {
    const identity = captureTeamProjectOperation();
    const current = generation.current;
    const files = await filesystemPickFiles(asset ? { title: `为「${asset.title}」选择新版本`, multiple: false, maxFiles: 1 } : { title: "选择要发布的资料或成果" });
    identity.assertCurrent();
    if (current !== generation.current) return;
    if (asset && files.length > 1) throw new Error("每份资料一次只能发布一个新版本，请重新选择");
    if (!files.length) return;
    let published = 0;
    try {
      for (const file of files) {
        identity.assertCurrent();
        if (current !== generation.current) return;
        await teamProjectUpload(selectedId!, file, asset, identity);
        published += 1;
      }
    } catch (error) {
      throw new Error(`${published ? `已发布 ${published} 份文件，其余文件尚未发布。` : "文件尚未发布。"}${String(error)}`);
    } finally {
      if (current === generation.current) { setAnswer(null); await reload(); }
    }
  });
  const create = () => requestInput({ title: "新建团队项目", fields: [{ name: "name", label: "项目名称", required: true, maxLength: 120 }, { name: "goal", label: "项目目标", multiline: true, maxLength: 12000 }], confirmLabel: "创建团队项目", action: async values => {
    const identity = captureTeamProjectOperation();
    const current = generation.current;
    const value = await teamProjectRequest<TeamProjectSnapshot>("create", undefined, values, undefined, identity);
    identity.assertCurrent(); if (current === generation.current) onSelect(value.id);
  } });

  const addTask = () => requestInput({ title: "新增工作项", fields: [{ name: "title", label: "工作项标题", required: true, maxLength: 300 }, { name: "description", label: "工作说明", multiline: true, maxLength: 12000 }, { name: "acceptance", label: "验收要求", multiline: true, maxLength: 4000 }], confirmLabel: "创建", action: async values => { await mutate("createTask", values); } });
  const editTask = (task: TeamTask) => requestInput({ title: "编辑工作项", description: task.status === "accepted" ? "修改工作说明或验收要求后，将重新进入待验收。" : undefined, fields: [
    { name: "title", label: "工作项标题", required: true, defaultValue: task.title, maxLength: 300 },
    { name: "description", label: "工作说明", multiline: true, defaultValue: task.description, maxLength: 12000 },
    { name: "acceptance", label: "验收要求", multiline: true, defaultValue: task.acceptance, maxLength: 4000 },
  ], confirmLabel: "保存工作项", action: async values => {
    const reopen = task.status === "accepted" && (values.description !== task.description || values.acceptance !== task.acceptance);
    await mutate("updateTask", { ...values, expectedRevision: task.revision, ...(reopen ? { status: "review" } : {}) }, task.id);
  } });
  const proposeDecision = () => requestInput({ title: "记录决策候选", description: "候选经项目管理员确认后，成为当前项目问答的正式依据。", fields: [{ name: "content", label: "决策内容", required: true, multiline: true, maxLength: 8000 }, { name: "rationale", label: "决策理由", multiline: true, maxLength: 8000 }], confirmLabel: "记录候选", action: async values => { await mutate("createDecision", values); } });
  const start = (task: TeamTask) => run(async () => {
    if (!project || !onStartWork) throw new Error("项目执行入口尚未准备好");
    if (!canEditTeamProject(project.role)) throw new Error(project.role === "viewer" ? "当前项目权限只允许查看，请使用项目问答" : projectRoleUnavailable);
    if (!models.some(model => model.id === modelId)) throw new Error("请先选择可用模型");
    const expert = experts.find(e => `expert:${e.path}` === capability), team = teams.find(t => `team:${t.id}` === capability);
    if (capability && !expert && !team) throw new Error("所选专家或专家团已不可用，请重新选择执行能力");
    const current = generation.current;
    try {
      const execution = await onStartWork(project, task, modelId, { expert, team, ...(overrideModel ? { modelOverride: modelId } : {}) });
      if (!execution) throw new Error("执行未能启动，工作项尚未开始执行，请检查模型和配额后重试");
    } finally {
      if (current === generation.current) await reload();
    }
  });
  if (!session?.loggedIn) return <section className="team-projects"><h2>团队项目</h2><p>登录组织后，与成员共享项目资料、进展和工作成果。</p><button type="button" onClick={onOpenOrganization}>登录组织</button></section>;
  if (!selectedId) return <section className="team-projects" aria-label="团队项目"><header><h2>团队项目</h2><button type="button" onClick={() => void run(reload)} disabled={busy}>刷新团队项目</button><button type="button" onClick={create}>新建团队项目</button></header>
    {error && <p role="alert">{error}</p>}{!projects.length && !error && <p>暂无已加入的团队项目。</p>}
    <div className="team-project-grid">{projects.map(p => <button type="button" className="team-project-card" key={p.id} onClick={() => onSelect(p.id)}><strong>{p.name}</strong><span>{p.goal || "待补充项目目标"}</span><small>组织项目 · {p.role === "viewer" ? "查看者" : p.role === "admin" ? "管理员" : p.role === "collaborator" ? "协作者" : "角色待确认"}</small></button>)}</div>{dialog}</section>;
  if (!project) return <section className="team-projects"><button onClick={() => onSelect(null)}>返回项目</button>{error ? <p role="alert">{error}</p> : <p role="status">正在读取团队项目…</p>}<button onClick={() => void run(reload)}>重试</button></section>;
  const writable = canEditTeamProject(project.role), admin = project.role === "admin";
  const runs = project.runs ?? [];
  const requestStart = (task: TeamTask) => {
    const running = runs.some(run => run.taskId === task.id && run.status === "running");
    const takingOver = task.assigneeId && task.assigneeId !== session.user?.id;
    if (running || takingOver) requestConfirmation({ title: takingOver ? `接手「${task.title}」？` : "开始新的执行？",
      description: `${takingOver ? `负责人将从${task.assigneeName ?? "原成员"}变更为你。` : "此工作项已有执行正在运行。"}本次将创建新的执行，旧执行的晚到结果不会改变当前工作状态。可先在执行记录中查看已有成果。`,
      confirmLabel: takingOver ? "接手并执行" : "新建执行", action: () => start(task) });
    else void start(task);
  };
  const showRuns = (taskId = "") => { setRunTaskId(taskId); setTab("runs"); };
  return <section className="team-projects team-projects--detail" aria-label={`团队项目 ${project.name}`}>
    {!embedded && <header><button type="button" onClick={() => onSelect(null)}>返回项目</button><h1>{project.name}</h1><ProjectParticipantsButton sharedId={selectedId} onOpenOrganization={onOpenOrganization} /></header>}
    <div className="project-collaboration-status"><span>已共享 · 版本 {project.revision} · {new Date(project.updatedAt).toLocaleString()}</span><button type="button" onClick={() => void run(reload)} disabled={busy}>刷新共享内容</button></div>
    {error && <p role="alert">{error}</p>}{project.syncWarning && <p role="alert">{project.syncWarning}</p>}
    {!writable && project.role !== "viewer" && <p role="status">{projectRoleUnavailable}</p>}
    <nav aria-label="团队项目页面">{([["overview", "概览"], ["tasks", "工作项"], ["assets", "资料与成果"], ["runs", "执行记录"]] as const).map(([key, label]) => <button type="button" key={key} aria-current={tab === key ? "page" : undefined} onClick={() => setTab(key)}>{label}</button>)}</nav>
    {tab === "overview" && <>
      <section><header><h2>项目简报</h2>{admin && <button type="button" disabled={busy} onClick={() => requestInput({ title: "维护项目目标", fields: [{ name: "goal", label: "目标与约束", multiline: true, defaultValue: project.goal, maxLength: 12000 }], action: async values => { await mutate("update", { name: project.name, goal: values.goal, expectedRevision: project.revision }); } })}>编辑目标</button>}</header><p>{project.goal || "项目目标尚未填写"}</p>
        <p>{project.tasks.filter(t => t.status === "accepted").length} 项已验收 · {project.tasks.filter(t => t.status === "review").length} 项待验收 · {project.tasks.filter(t => t.status === "blocked").length} 项阻塞</p>
        <button type="button" disabled={busy} onClick={() => void ask("我是新加入的项目成员。请说明项目目标、当前进展、有效方案和理由、待验收与阻塞事项、我应该先接手什么，并引用依据。")}>新成员快速了解项目</button>
      </section>
      <section><h2>问项目</h2><form onSubmit={e => { e.preventDefault(); if (question.trim()) void ask(question.trim()); }}><label>项目问题<textarea value={question} maxLength={8000} onChange={e => setQuestion(e.target.value)} placeholder="项目做到哪里了？为什么采用当前方案？" /></label><button disabled={busy || !question.trim()}>查询项目</button></form>
        {answer && <article aria-label="项目问答结果"><p>依据项目版本 {answer.projectRevision} · {answer.verification === "supported" ? "经来源核验" : "来源摘录，尚未形成完整回答"}</p><div className="team-project-answer">{answer.answer}</div>{answer.gaps.length > 0 && <ul>{answer.gaps.map(gap => <li key={gap}>{gap}</li>)}</ul>}<div className="team-project-citations">{answer.citations.map(c => <button type="button" key={c.id} onClick={() => void openCitation(c)} disabled={busy}>{c.id} · {c.title}</button>)}</div></article>}
      </section>
      {runs.length > 0 && <section><header><h2>最近执行</h2><button type="button" onClick={() => showRuns()}>查看全部执行记录</button></header><p>{runs.filter(run => run.status === "running").length} 项执行中 · {runs.filter(run => run.status === "completed").length} 项已完成</p></section>}
      <section><header><h2>项目决策</h2>{writable && <button type="button" onClick={proposeDecision} disabled={busy}>记录决策</button>}</header>
        {admin && <label>确认新决策时替代<Select value={replacement} onChange={e => setReplacement(e.target.value)}><option value="">保留现有决策</option>{project.decisions.filter(d => d.status === "active").map(d => <option value={d.id} key={d.id}>{d.content.slice(0, 60)}</option>)}</Select></label>}
        {project.decisions.map(d => <article className="team-project-record" key={d.id}><strong>{({ proposed: "待确认", active: "有效", superseded: "已替代", retired: "已撤销" })[d.status]}</strong><p>{d.content}</p><p>{d.rationale}</p>{admin && d.status === "proposed" && <button disabled={busy} onClick={() => void run(async () => { const old = project.decisions.find(item => item.id === replacement); await mutate("updateDecision", { expectedRevision: d.revision, status: "active", ...(old ? { supersedesId: old.id, supersedesRevision: old.revision } : {}) }, d.id); setReplacement(""); })}>确认决策</button>}{admin && d.status === "active" && <button disabled={busy} onClick={() => void run(() => mutate("updateDecision", { expectedRevision: d.revision, status: "retired" }, d.id))}>撤销决策</button>}</article>)}
      </section>

    </>}
    {tab === "tasks" && <section>
      <header><h2>团队工作项</h2>{writable && <button onClick={addTask} disabled={busy}>新增工作项</button>}</header>
      <p>执行报告会共享给项目成员；本机生成的文件需在“资料与成果”中发布。已有执行可从执行记录打开，继续原任务。</p>
      {writable && <div className="team-project-execution">
        <label>本次模型<Select disabled={busy} value={modelId} onChange={e => { setModelId(e.target.value); setOverrideModel(true); }}><option value="">请选择模型</option>{models.map(m => <option key={m.id} value={m.id}>{m.label ?? m.id}</option>)}</Select></label>
        <label>执行能力<Select disabled={busy} value={capability} onChange={e => setCapability(e.target.value)}><option value="">通用助手</option>{experts.map(e => <option key={e.path} value={`expert:${e.path}`}>{e.name}</option>)}{teams.map(t => <option key={t.id} value={`team:${t.id}`}>专家团 · {t.name}</option>)}</Select></label>
        {capability && <label className="team-project-checkbox"><input type="checkbox" disabled={busy} checked={overrideModel} onChange={e => setOverrideModel(e.target.checked)} />覆盖所选能力的默认模型</label>}
      </div>}
      {capability && <p className="team-project-muted">未覆盖时优先使用专家的默认模型。专家团成员单独配置的模型会保留。</p>}
      {!project.tasks.length && <p>暂无工作项。添加工作说明和验收要求后，即可安排执行。</p>}
      {project.tasks.map(task => {
        const latest = runs.find(run => run.taskId === task.id);
        return <article className="team-project-record" key={task.id}>
          <header><h3>{task.title}</h3><span className={`team-project-status team-project-status--${task.status}`}>{TEAM_TASK_LABELS[task.status]}</span></header>
          <p>{task.description || "尚未填写工作说明"}</p><p>负责人：{task.assigneeName ?? "未分配"}</p><p>验收要求：{task.acceptance || "尚未填写"}</p>
          {latest && <div className="team-project-result"><strong>最近执行 · {({ running: "执行中", completed: "已完成", failed: "失败", cancelled: "已取消" })[latest.status]}</strong>
            <p>{latest.summary ? latest.summary.replace(/^#{1,6}\s+/gm, "").replace(/\*\*([^*]+)\*\*/g, "$1").slice(0, 240) + (latest.summary.length > 240 ? "…" : "") : latest.status === "running" ? "结果会在本轮执行结束后同步。" : "暂无公开报告，请联系执行者核对。"}</p>
            <button type="button" disabled={busy} onClick={() => showRuns(task.id)}>查看执行记录与成果</button>
          </div>}
          {writable && <div className="team-project-task-actions">
            <button type="button" disabled={busy} onClick={() => editTask(task)}>编辑工作项</button>
            <label>工作状态<Select disabled={busy} value={task.status} onChange={e => {
              const status = e.target.value as TeamTaskStatus;
              if (status === "accepted") requestConfirmation({ title: `验收 ${task.title}？`, description: "请先查看执行记录与成果，确认符合验收要求。执行结束不自动代表验收通过。", confirmLabel: "确认验收", action: () => mutate("updateTask", { expectedRevision: task.revision, status }, task.id) });
              else void run(() => mutate("updateTask", { expectedRevision: task.revision, status }, task.id));
            }}>{Object.entries(TEAM_TASK_LABELS).map(([key, label]) => <option key={key} value={key} disabled={key === "accepted" && !admin}>{label}</option>)}</Select></label>
            {task.status !== "accepted" && <button disabled={busy || !modelId || !onStartWork} onClick={() => requestStart(task)}>{task.assigneeId === session.user?.id ? "新建执行" : "接手并执行"}</button>}
          </div>}
        </article>;
      })}
    </section>}
    {tab === "runs" && <section aria-label="项目执行记录">
      <header><h2>执行记录与成果</h2><label>工作项筛选<Select value={runTaskId} onChange={e => setRunTaskId(e.target.value)}><option value="">全部工作项</option>{project.tasks.map(task => <option value={task.id} key={task.id}>{task.title}</option>)}</Select></label></header>
      <p>这里展示已共享的公开报告。执行完成后，请结合“资料与成果”的文件进行验收；本机执行可以打开后继续。</p>
      {!runs.some(run => !runTaskId || run.taskId === runTaskId) && <p>暂无执行记录。发起执行后，可在这里查看进展和报告。</p>}
      {runs.filter(run => !runTaskId || run.taskId === runTaskId).map((execution, index) => <article className="team-project-record team-project-run" key={execution.id}>
        <header><h3>{project.tasks.find(task => task.id === execution.taskId)?.title ?? "历史工作项"}</h3><strong>{({ running: "执行中", completed: "已完成", failed: "执行失败", cancelled: "已取消" })[execution.status]}</strong></header>
        <p className="team-project-muted">{project.members.find(member => member.userId === execution.actorId)?.displayName ?? "原项目成员"} · {execution.expertName ?? "通用助手"}{execution.modelId && ` · ${models.find(model => model.id === execution.modelId)?.label ?? execution.modelId}`} · {new Date(execution.updatedAt).toLocaleString()}</p>
        {execution.summary ? <details open={index === 0}><summary>公开执行报告</summary><Markdown complete>{execution.summary}</Markdown></details> : <p>{execution.status === "running" ? "执行仍在进行，报告将在结束后同步。" : "本次执行未产生公开报告。"}</p>}
        {writable && onOpenExecution && execution.actorId === session.user?.id && <button type="button" disabled={busy} onClick={() => void run(() => onOpenExecution(project, execution))}>打开本机执行</button>}
      </article>)}
    </section>}
    {tab === "assets" && <section><header><h2>资料与成果</h2>{writable && <button type="button" disabled={busy} onClick={() => void upload()}>发布资料或成果</button>}</header><p>发布的文件对项目成员可见。处理完成后参与问答；新版本未就绪时继续使用上一有效版本。</p>
      {project.assets.map(a => <article className="team-project-record" key={a.id}><strong>{a.title} · v{a.version}</strong><p>{a.status === "ready" ? "可检索" : a.status === "failed" ? `处理失败：${a.failReason ?? "请重新发布"}` : "已共享，正在处理"}{a.currentDocumentId && a.currentDocumentId !== a.id && " · 上一有效版本仍可用于问答"}</p><button disabled={busy} onClick={() => void run(async () => { const identity = captureTeamProjectOperation(); identity.assertCurrent(); await orgDownloadDocument(a.id, false, a.title, identity.expectedGeneration); })}>下载原件</button>{writable && <button disabled={busy} onClick={() => void upload(a)}>发布新版本</button>}</article>)}
    </section>}
    {preview && <section aria-label="项目来源原文"><header><h2>{preview.title}</h2><button onClick={() => setPreview(null)}>关闭来源</button></header><pre>{preview.text}</pre></section>}
    {busy && <p role="status">正在处理项目请求…</p>}{dialog}
  </section>;
}
