import { Select } from "@/components/Select";
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { CalendarClock, Pause, Play, Plus, Trash2 } from "lucide-react";
import { automationsDelete, automationsRun, automationsSave, automationsSetStatus, automationsSnapshot, filesystemPickDirectory, skillsList } from "@/lib/agent-client";
import { resolveAutomationSkills } from "@/lib/automation-skills";
import { AdditionalAutomationDirectories } from "./automation/AdditionalAutomationDirectories";
import { buildDraft, describeSchedule } from "./automation/schedule-utils";
import type { Automation, AutomationSnapshot } from "@/lib/types";
import { flushProjectPersistence, projectOwnsSession, useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { TaskDrawer } from "./ProjectTasks";
import { useAppDialog } from "./AppDialog";
import type { ModelOption } from "./ModelSelector";

export function useProjectAutomations(project: ProjectMeta) {
  const [snapshot, setSnapshot] = useState<AutomationSnapshot>({ automations: [], records: [] });
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const generation = useRef(0);
  const refresh = useCallback(async () => {
    const current = ++generation.current; setLoading(true);
    try {
      const value = await automationsSnapshot();
      if (generation.current !== current) return;
      if (!value || !Array.isArray(value.automations) || !Array.isArray(value.records)) throw new Error("定时工作数据不可用");
      const latest = useProjectsStore.getState().projects.find(p => p.id === project.id);
      if (!latest) return;
      const owned = value.automations.filter(a => a.projectId === project.id || latest.automationIds?.includes(a.id));
      const ids = new Set([...owned.map(a => a.id), ...(latest.automationIds ?? [])]);
      const records = value.records.filter(r => ids.has(r.automationId));
      setSnapshot({ automations: owned, records }); setError("");
      for (const record of records) {
        const live = useProjectsStore.getState().projects.find(p => p.id === project.id);
        if (record.sessionId && live && !projectOwnsSession(live, record.sessionId)) useProjectsStore.getState().addConversation(project.id, { sessionId: record.sessionId, title: `定时：${record.automationName}`, createdAt: record.startedAt, modelId: record.modelId });
      }
    } catch (e) { if (generation.current === current) setError(String(e).replace(/^Error:\s*/, "")); }
    finally { if (generation.current === current) setLoading(false); }
  }, [project.id, project.automationIds]);
  useEffect(() => {
    let active = true; let unlisten: (() => void) | undefined;
    void refresh();
    void listen("agent://automation-update", () => { if (active) void refresh(); }).then(fn => { if (active) unlisten = fn; else fn(); }).catch(() => {});
    const timer = window.setInterval(() => { if (document.visibilityState !== "hidden") void refresh(); }, 15000);
    window.addEventListener("focus", refresh);
    return () => { active = false; ++generation.current; unlisten?.(); clearInterval(timer); window.removeEventListener("focus", refresh); };
  }, [refresh]);
  return { ...snapshot, error, loading, refresh };
}

export function ProjectAutomationsDialog({ project, data, models, defaultModelId, onClose, onOpenSession }: {
  project: ProjectMeta; data: ReturnType<typeof useProjectAutomations>; models: ModelOption[]; defaultModelId?: string;
  onClose: () => void; onOpenSession?: (id: string, cwd?: string) => void;
}) {
  const [editing, setEditing] = useState<Automation | "new">();
  const [name, setName] = useState("项目周报");
  const [prompt, setPrompt] = useState("汇总本周项目进展、交付成果和待处理事项，生成项目周报。");
  const [cwd, setCwd] = useState(project.cwd ?? "");
  const [additionalCwds, setAdditionalCwds] = useState<string[]>([]);
  const [day, setDay] = useState("FR");
  const [time, setTime] = useState("09:00");
  const [modelId, setModelId] = useState(defaultModelId ?? "");
  const [busy, setBusy] = useState(false);
  const operation = useRef(false);
  const [error, setError] = useState("");
  const { requestConfirmation, dialog } = useAppDialog(`${project.id}:automations`);
  const run = async (work: () => Promise<void>) => {
    if (operation.current) return; operation.current = true; setBusy(true); setError("");
    try { await work(); await data.refresh(); } catch (e) { setError(String(e).replace(/^Error:\s*/, "")); }
    finally { operation.current = false; setBusy(false); }
  };
  const edit = (automation?: Automation) => {
    setEditing(automation ?? "new"); setName(automation?.name ?? "项目周报"); setPrompt(automation?.prompt ?? "汇总本周项目进展、交付成果和待处理事项，生成项目周报。"); setCwd(automation?.cwds[0] ?? project.cwd ?? ""); setModelId(automation?.modelId ?? defaultModelId ?? "");
    setAdditionalCwds(automation?.cwds.slice(1) ?? []);
    setDay(automation?.schedule.byday[0] ?? "FR"); setTime(`${String(automation?.schedule.byhour ?? 9).padStart(2, "0")}:${String(automation?.schedule.byminute ?? 0).padStart(2, "0")}`);
  };
  const save = () => run(async () => {
    const original = editing && editing !== "new" ? editing : undefined;
    if (!name.trim() || !prompt.trim() || !cwd || !modelId || (original?.status ?? "ACTIVE") === "ACTIVE" && !models.some(model => model.id === modelId)) throw new Error("请填写名称、执行内容，并选择工作目录与可用模型");
    await flushProjectPersistence();
    const [hour, minute] = time.split(":").map(Number);
    if (!Number.isInteger(hour) || hour < 0 || hour > 23 || !Number.isInteger(minute) || minute < 0 || minute > 59) throw new Error("请选择有效的执行时间");
    const draft = original ?? { ...buildDraft(undefined, cwd), createdAt: "" };
    const selectedSkills = original?.skills ?? project.skills.map(s => s.id);
    const skills = selectedSkills.length && (original?.status ?? "ACTIVE") === "ACTIVE"
      ? resolveAutomationSkills(selectedSkills, await skillsList(cwd)) : selectedSkills;
    const scheduleChanged = !original || day !== (original.schedule.byday[0] ?? "FR") || hour !== original.schedule.byhour || minute !== original.schedule.byminute;
    const saved = await automationsSave({ ...draft, projectId: project.id, name: name.trim(), prompt: prompt.trim(), cwds: [cwd, ...additionalCwds], modelId, status: original?.status ?? "ACTIVE", newSessionEachRun: original?.newSessionEachRun ?? true,
      skills, connectorIds: original?.connectorIds ?? project.connectors.map(c => c.id),
      scheduleType: scheduleChanged ? "recurring" : draft.scheduleType, schedule: scheduleChanged ? { ...draft.schedule, freq: "WEEKLY", interval: 1, byday: [day], byhour: hour, byminute: minute } : draft.schedule });
    setEditing(saved);
    const latest = useProjectsStore.getState().projects.find(p => p.id === project.id);
    if (latest) { useProjectsStore.getState().updateConfig(project.id, { automationIds: [...new Set([...(latest.automationIds ?? []), saved.id])] }); await flushProjectPersistence(); }
    setEditing(undefined);
  });
  const close = () => {
    if (busy) return;
    if (editing) requestConfirmation({ title: "舍弃定时工作草稿？", description: "尚未保存的内容会被舍弃。", confirmLabel: "舍弃修改", action: onClose }); else onClose();
  };
  return <><TaskDrawer title="项目定时工作" onClose={close}><p className="project-workspace-hint">由本机调度器运行，每次读取最新的本机项目指令与已确认决定。运行记录与成果会回到本项目。电脑需要保持运行。</p>
    {data.loading && <p role="status">正在读取定时工作…</p>}{data.error && <p role="alert">{data.error}<button className="pd-btn" onClick={() => void data.refresh()}>重试</button></p>}
    {!editing ? <><button className="pd-btn" onClick={() => edit()}><Plus size={15} />新建定时工作</button>{data.automations.map(a => <article className="project-automation-row" key={a.id}><CalendarClock size={18} /><div><strong>{a.name}</strong><small>{describeSchedule(a)} · {a.status === "ACTIVE" ? "已启用" : "已暂停"}</small></div><button className="pd-btn" disabled={busy} onClick={() => edit(a)}>编辑</button><button className="pd-btn" disabled={busy} onClick={() => void run(async () => { await automationsSetStatus(a.id, a.status === "ACTIVE" ? "PAUSED" : "ACTIVE"); })}>{a.status === "ACTIVE" ? <Pause size={14} /> : <Play size={14} />}{a.status === "ACTIVE" ? "暂停" : "启用"}</button><button className="pd-btn" disabled={busy} onClick={() => void run(async () => { await automationsRun(a.id); })}>立即运行</button><button className="pd-btn pd-btn--ghost" aria-label={`删除定时工作 ${a.name}`} disabled={busy} onClick={() => requestConfirmation({ title: `删除「${a.name}」？`, description: "之后不再自动运行，已有执行记录与文件保留。", confirmLabel: "删除定时工作", danger: true, action: () => run(() => automationsDelete(a.id)) })}><Trash2 size={14} /></button></article>)}
      <h4>最近执行</h4>{!data.records.length && <p className="project-workspace-hint">尚无执行记录。</p>}{data.records.slice(0, 10).map(r => <div className="project-automation-record" key={r.id}><span>{r.automationName}<small>{new Date(r.startedAt).toLocaleString()} · {({ queued: "排队中", running: "执行中", success: "本轮已结束", failed: "失败", stopped: "已停止", interrupted: "已中断" } as Record<string, string>)[r.status] ?? r.status}</small>{r.error && <small role="alert">{r.error}</small>}</span>{r.sessionId && onOpenSession && <button className="pd-btn" onClick={() => onOpenSession(r.sessionId!, r.cwd)}>打开任务</button>}</div>)}</> : <div className="project-settings-fields"><label className="project-settings-field">名称<input value={name} maxLength={100} onChange={e => setName(e.target.value)} /></label><label className="project-settings-field">执行内容<textarea rows={4} value={prompt} onChange={e => setPrompt(e.target.value)} /></label><div className="project-task-actions"><label>重复<Select value={day} onChange={e => setDay(e.target.value)}>{[['MO','周一'],['TU','周二'],['WE','周三'],['TH','周四'],['FR','周五'],['SA','周六'],['SU','周日']].map(([id,label]) => <option value={id} key={id}>每{label}</option>)}</Select></label><label>时间<input type="time" value={time} onChange={e => setTime(e.target.value)} /></label></div><label className="project-settings-field">模型<Select value={modelId} onChange={e => setModelId(e.target.value)}><option value="">请选择可用模型</option>{models.map(m => <option key={m.id} value={m.id}>{m.label}</option>)}</Select></label><div className="project-settings-field">工作目录<span>{cwd || "尚未选择"}</span><button className="pd-btn" disabled={busy} onClick={() => void run(async () => { const selected = await filesystemPickDirectory(); if (selected) setCwd(selected); })}>选择文件夹</button></div><AdditionalAutomationDirectories paths={additionalCwds} disabled={busy} onRemove={index => setAdditionalCwds(paths => paths.filter((_, i) => i !== index))} /><div className="project-task-actions"><button className="pd-btn" disabled={busy} onClick={() => setEditing(undefined)}>取消编辑</button><button className="pd-btn pd-btn--primary" disabled={busy} onClick={() => void save()}>{busy ? "保存中…" : "保存定时工作"}</button></div></div>}
    {error && <p role="alert" className="project-inline-error">{error}</p>}
  </TaskDrawer>{dialog}</>;
}
