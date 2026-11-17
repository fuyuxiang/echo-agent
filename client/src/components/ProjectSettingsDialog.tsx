import { useState } from "react";
import { FolderOpen, X } from "lucide-react";
import { useProjectsStore, flushProjectPersistence, type ProjectMeta, type RefItem } from "@/stores/projects-store";
import { filesystemPickDirectory } from "@/lib/agent-client";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { ModelSelector, type ModelOption } from "./ModelSelector";
import { RefPickerDialog, type ProjectPickerOptions } from "./project-picker";
import { findModelDisplayName } from "@/lib/model-presentation";
import { displayPath } from "@/lib/display-path";

export function ProjectSettingsDialog({ project, sharedId, workspace, picker, models, defaultModelId, onClose, onOpenModelSettings }: {
  project: ProjectMeta; sharedId?: string; workspace: ProjectWorkspace;
  picker: { options: ProjectPickerOptions; loading: boolean; error: string | null };
  models: ModelOption[]; defaultModelId?: string; onClose: () => void; onOpenAutomation?: () => void; onOpenModelSettings?: () => void;
}) {
  const [initial] = useState(() => ({ name: workspace.snapshot?.name ?? project.name, description: sharedId ? workspace.snapshot?.goal ?? "" : project.description ?? "", instructions: project.instructions ?? "", cwd: project.cwd, model: project.defaultModelId, refs: { experts: project.experts, skills: project.skills, connectors: project.connectors }, revision: workspace.snapshot?.revision }));
  const [name, setName] = useState(initial.name);
  const [description, setDescription] = useState(initial.description);
  const [instructions, setInstructions] = useState(initial.instructions);
  const [cwd, setCwd] = useState(initial.cwd);
  const [model, setModel] = useState(initial.model);
  const [refs, setRefs] = useState(initial.refs);
  const [pickerFor, setPickerFor] = useState<keyof typeof refs>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dirty = JSON.stringify({ name, description, instructions, cwd, model, refs }) !== JSON.stringify({ name: initial.name, description: initial.description, instructions: initial.instructions, cwd: initial.cwd, model: initial.model, refs: initial.refs });
  const { requestClose, closeDialog } = useUnsavedClose({ dirty, busy, onClose, scopeKey: project.id });
  const focus = useModalFocus<HTMLDivElement>(true, requestClose);
  const sharedWritable = !sharedId || workspace.snapshot?.role === "admin";
  const labels = { experts: "专家", skills: "技能", connectors: "连接器" };
  const save = async () => {
    if (busy) return;
    if (!name.trim()) { setError("请填写项目名称"); return; }
    setBusy(true); setError("");
    try {
      if (sharedId && (description !== initial.description || name.trim() !== initial.name)) {
        if (!workspace.snapshot || !sharedWritable || !initial.revision) throw new Error("请由管理员刷新项目后修改共享设置");
        await workspace.mutate("update", { name: name.trim(), goal: description, expectedRevision: initial.revision });
      }
      useProjectsStore.getState().updateConfig(project.id, { name: name.trim(), description: sharedId ? project.description : description, instructions, cwd, defaultModelId: model, ...refs });
      useProjectsStore.getState().recordActivity(project.id, { kind: "settings", content: "更新了项目设置" });
      await flushProjectPersistence(); onClose();
    } catch (e) { setError(String(e).replace(/^Error:\s*/, "")); }
    finally { setBusy(false); }
  };
  const pickDirectory = async () => {
    if (busy) return; setBusy(true); setError("");
    try { const path = await filesystemPickDirectory(); if (path) setCwd(path); }
    catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  return <><div className="modal-overlay" onClick={requestClose}><div ref={focus} className="create-colleague-dialog project-settings-dialog" role="dialog" aria-modal="true" aria-label="项目设置" tabIndex={-1} onClick={e => e.stopPropagation()}>
    <div className="create-colleague-header"><h3>项目设置</h3><button className="create-colleague-close" aria-label="关闭项目设置" onClick={requestClose}><X size={18} /></button></div>
    <div className="create-colleague-body project-settings-fields">
      <label className="project-settings-field">项目名称<input value={name} maxLength={120} disabled={busy || !sharedWritable} onChange={e => setName(e.target.value)} /></label>
      <label className="project-settings-field">项目说明<textarea rows={3} value={description} maxLength={12000} disabled={busy || !sharedWritable} onChange={e => setDescription(e.target.value)} placeholder="项目目标、背景和交付要求" /></label>
      <label className="project-settings-field">{sharedId ? "本机项目指令" : "项目指令"}<textarea rows={5} value={instructions} maxLength={128000} disabled={busy} onChange={e => setInstructions(e.target.value)} placeholder="长期适用的工作规范、风格与输出要求" /><small>{sharedId ? "这些指令与以下偏好仅用于你在本机发起的新任务。共享项目背景由管理员维护。" : "新任务自动沿用；已有任务保留原会话上下文。"}</small></label>
      <section><h4>默认工作目录</h4><div className="project-settings-choice"><span>{cwd ? displayPath(cwd) : "为任务分配独立目录"}</span><button className="pd-btn" disabled={busy} onClick={() => void pickDirectory()}><FolderOpen size={14} />选择文件夹</button>{cwd && <button className="pd-btn pd-btn--ghost" disabled={busy} onClick={() => setCwd(undefined)}>清除</button>}</div><p className="project-workspace-hint">只影响新任务，历史任务继续使用原工作目录。</p></section>
      <section className="project-settings-defaults"><h4>新任务执行偏好</h4>
        <div className="project-settings-choice"><span>模型</span><ModelSelector models={models} modelId={model ?? defaultModelId} onModelChange={setModel} ariaLabel="选择项目默认模型" menuPlacement="bottom" menuAlign="end" menuZIndex="var(--echo-layer-dialog-popover)" onBeforeManageModels={requestClose} />{model && <button type="button" className="pd-btn pd-btn--small" disabled={busy} onClick={() => setModel(undefined)}>跟随应用默认</button>}</div>
        {model && !models.some(m => m.id === model) && <p role="alert">原模型「{findModelDisplayName(models, model)}」暂不可用，配置已保留。</p>}
        {!models.length && <button className="pd-btn" onClick={() => { requestClose(); if (!dirty) onOpenModelSettings?.(); }}>配置模型</button>}
        {(Object.keys(labels) as (keyof typeof refs)[]).map(key => <div className="project-settings-ref" key={key}><div className="project-settings-choice"><span>{labels[key]}</span><button type="button" className="pd-btn pd-btn--small" disabled={busy} onClick={() => setPickerFor(key)}>选择{labels[key]}</button></div><div className="project-settings-tags">{refs[key].length ? refs[key].map((item: RefItem) => <span key={item.id}>{item.name}</span>) : <small>未选择，按任务需要使用可用能力</small>}</div></div>)}
      </section>
      {(project.members.length > 0 || !!project.legacyPlans?.length) && <details className="project-settings-advanced"><summary>历史记录</summary>{project.members.length > 0 && <section><h4>原参与者备注</h4>{project.members.map((note, i) => <p key={i}>{note}</p>)}</section>}{!!project.legacyPlans?.length && <section><h4>原计划记录</h4>{project.legacyPlans.map((plan, i) => <p key={`${plan.id}:${i}`}>{plan.title}</p>)}</section>}</details>}
      {error && <p role="alert" className="project-inline-error">{error}</p>}
    </div><div className="create-colleague-footer"><button className="pd-btn" disabled={busy} onClick={requestClose}>取消</button><button className="pd-btn pd-btn--primary" disabled={busy} onClick={() => void save()}>{busy ? "保存中…" : "保存设置"}</button></div>
  </div></div>{pickerFor && <RefPickerDialog title={labels[pickerFor]} options={picker.options[pickerFor]} selected={refs[pickerFor]} emptyHint={picker.loading ? "正在读取…" : picker.error ? `读取失败：${picker.error}` : "没有可用项"} onCancel={() => setPickerFor(undefined)} onConfirm={items => { setRefs(previous => ({ ...previous, [pickerFor]: items })); setPickerFor(undefined); }} />}{closeDialog}</>;
}
