import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { BookmarkPlus, Check, GitCompare, Plus, Sparkles, X } from "lucide-react";
import { useModalFocus } from "@/lib/use-modal-focus";
import { modelDisplayName } from "@/lib/model-presentation";
import { agentListWorkspaces, agentAuthStatus } from "@/lib/agent-client";
import { displayPath } from "@/lib/display-path";
import type { Automation } from "@/lib/types";
import {
  fromWorkflowSource, listenWorkflowLibrary, workflowActivate, workflowAutoDraft, workflowBind, workflowCaptureContext,
  workflowDismiss, workflowEvaluate, workflowFindSource, workflowLibrary, workflowPassed, workflowRevise, workflowSave,
  workflowSetAutomatic, workflowSuggest, workflowSuggestRevision, workflowVersions, workflowWorkspaces,
  type Workflow, type WorkflowBinding, type WorkflowCapture, type WorkflowCaptureContext, type WorkflowCase, type WorkflowDraft,
  type WorkflowLibrarySnapshot, type WorkflowSourceMatch, type WorkflowVersion,
} from "@/lib/workflow-skills";

const blankDraft: WorkflowDraft = { title: "", description: "", instructions: "", change: "" };
const errorText = (error: unknown) => String(error).replace(/^Error:\s*/, "");
const sourceStatus = { draft: "有流程草稿", verified: "模型输出已验证", active: "流程已启用" };

function useLibrary(cwd: string) {
  const [snapshot, setSnapshot] = useState<WorkflowLibrarySnapshot>();
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);
  useEffect(() => {
    let current = true;
    let unsubscribe: (() => void) | undefined;
    let generation = 0;
    const load = () => {
      const request = ++generation;
      void workflowLibrary(cwd).then((library) => {
        if (current && request === generation) {
          setSnapshot(library);
          setError("");
        }
      }).catch((cause) => {
        if (current && request === generation) setError(errorText(cause));
      });
    };
    load();
    void listenWorkflowLibrary(load).then((stop) => {
      if (current) unsubscribe = stop;
      else stop();
    }).catch(() => {});
    return () => { current = false; unsubscribe?.(); };
  }, [cwd, reload]);
  return { snapshot, error, retry: () => setReload((value) => value + 1) };
}

function Modal({ title, busy = false, onClose, children, footer }: {
  title: string; busy?: boolean; onClose: () => void; children: ReactNode; footer?: ReactNode;
}) {
  const ref = useModalFocus<HTMLDivElement>(true, () => { if (!busy) onClose(); });
  return createPortal(<div className="modal-overlay workflow-overlay" onClick={(event) => {
    if (!busy && event.target === event.currentTarget) onClose();
  }}>
    <div className="sk-import workflow-dialog" ref={ref} tabIndex={-1} role="dialog" aria-modal="true" aria-label={title}>
      <header className="sk-import-head">
        <h2>{title}</h2>
        <button type="button" className="sk-import-close" onClick={onClose} disabled={busy} aria-label="关闭"><X size={18} /></button>
      </header>
      {children}
      {footer && <footer className="workflow-dialog__footer">{footer}</footer>}
    </div>
  </div>, document.body);
}

interface CommonProps {
  cwd: string;
  modelId?: string;
  task?: Automation;
  onToast?: (message: string) => void;
  onBindingChanged?: () => void;
  onOpenSource?: (sessionId: string, cwd?: string) => void;
}

interface EditorProps extends CommonProps {
  library: WorkflowLibrarySnapshot;
  capture?: WorkflowCapture;
  workflow?: Workflow;
  initialCorrection?: string;
  sourceBinding?: WorkflowBinding | null;
  sourceModelId?: string | null;
  initialVersionNumber?: number;
  onClose: () => void;
}

function EvalResults({ version }: { version: WorkflowVersion }) {
  const result = version.evaluation;
  if (!result) return null;
  const baseline = result.cases.filter((check) => check.baselinePassed).length;
  const candidate = result.cases.filter((check) => check.candidatePassed).length;
  const regressions = result.cases.filter((check) => check.baselinePassed && !check.candidatePassed).length;
  return <section className="workflow-report" aria-label="验证结果">
    <h3>验证结果</h3>
    <p>{modelDisplayName(result.modelId)} · {new Date(result.checkedAt).toLocaleString()}</p>
    <div className="workflow-report__summary">
      <strong>{version.baseVersion ? `版本 ${version.baseVersion}` : "未使用流程"} {baseline}/{result.cases.length} 通过</strong>
      <strong>版本 {version.number} {candidate}/{result.cases.length} 通过</strong>
      {regressions > 0 && <span>{regressions} 项退步</span>}
    </div>
    {result.cases.map((check, index) => <details key={index}>
      <summary>用例 {index + 1} · {check.candidatePassed ? "通过" : "未通过"}</summary>
      <div className="workflow-compare">
        <div><strong>对照输出 · {(check.baselineMs / 1000).toFixed(1)} 秒</strong><pre>{check.baselineOutput}</pre></div>
        <div><strong>候选输出 · {(check.candidateMs / 1000).toFixed(1)} 秒</strong><pre>{check.candidateOutput}</pre></div>
      </div>
      <p>预期输出</p><pre>{version.cases[index]?.expected}</pre>
    </details>)}
  </section>;
}

function WorkflowEditor({ cwd, library, capture, workflow: initial, task, modelId, initialCorrection = "",
  sourceBinding, sourceModelId, initialVersionNumber, onToast, onClose, onBindingChanged, onOpenSource }: EditorProps) {
  const initialVersion = initial && (capture
    ? workflowVersions(initial).find((version) => fromWorkflowSource(version.source, capture))
    : workflowVersions(initial).find((version) => version.number === initialVersionNumber) ?? workflowVersions(initial)[0]);
  const initialBase = initial && workflowVersions(initial).find((entry) =>
    entry.number === (initial.id === sourceBinding?.workflowId ? sourceBinding.version : initial.activeVersion));
  const [workflow, setWorkflow] = useState(initial);
  const [selected, setSelected] = useState(initialVersion?.number);
  const [form, setForm] = useState<WorkflowDraft | null>(initialVersion ? null : initialBase ? {
    title: initialBase.title, description: initialBase.description, instructions: initialBase.instructions, change: "",
  } : blankDraft);
  const [cases, setCases] = useState<WorkflowCase[]>(initialBase?.cases ?? []);
  const [baseNumber, setBaseNumber] = useState<number | null>(
    initialBase?.number ?? null,
  );
  const [extractionModel, setExtractionModel] = useState(sourceModelId ?? modelId ?? "");
  const [correction, setCorrection] = useState(initialCorrection);
  const [busy, setBusy] = useState("");
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const [binding, setBinding] = useState(task?.workflowSkill);
  const operation = useRef(false);
  const revisions = workflow ? workflowVersions(workflow) : [];
  const version = revisions.find((entry) => entry.number === selected);
  const base = revisions.find((entry) => entry.number === version?.baseVersion);
  const isBound = Boolean(workflow && binding?.workflowId === workflow.id && binding?.version === version?.number);
  const modelMismatch = Boolean(task && modelId && version?.evaluation?.modelId !== modelId);
  const extractionModels = [...new Set([sourceModelId, modelId].filter((id): id is string => Boolean(id)))];

  useEffect(() => {
    if (form || operation.current || !workflow?.id) return;
    const updated = library.workflows.find((item) => item.id === workflow.id);
    if (updated) setWorkflow(updated);
  }, [library, workflow?.id]);

  async function perform(label: string, work: () => Promise<void>) {
    if (operation.current) return;
    operation.current = true;
    setBusy(label);
    setNotice("");
    setError("");
    try { await work(); } catch (cause) { setError(errorText(cause)); }
    finally { operation.current = false; setBusy(""); }
  }

  function editVersion(entry: WorkflowVersion) {
    setForm({ title: entry.title, description: entry.description, instructions: entry.instructions, change: entry.change });
    setCases(entry.cases);
    setBaseNumber(entry.number === workflow?.activeVersion || isBound ? entry.number : entry.baseVersion ?? null);
    setError("");
    setNotice("");
  }

  function chooseTarget(id: string) {
    const target = library.workflows.find((item) => item.id === id);
    const targetVersions = target ? workflowVersions(target) : [];
    const pinned = target?.id === sourceBinding?.workflowId ? sourceBinding?.version : undefined;
    const previous = targetVersions.find((version) => version.number === (pinned ?? target?.activeVersion)) ?? targetVersions[0];
    setWorkflow(target);
    setSelected(previous?.number);
    setBaseNumber(previous?.number ?? null);
    setForm(previous ? { title: previous.title, description: previous.description, instructions: previous.instructions, change: "" } : blankDraft);
    setCases(previous?.cases ?? []);
    setError("");
    setNotice("");
  }

  const title = capture && !version ? "保存为可复用流程" : version?.title ?? "编辑流程";
  const footer = form ? <div className="workflow-actions">
          {version && <button type="button" className="capability-button" disabled={Boolean(busy)} onClick={() => setForm(null)}>取消编辑</button>}
          <button type="button" className="capability-button capability-button--primary"
            disabled={Boolean(busy) || !form.title.trim() || !form.description.trim() || !form.instructions.trim()
              || cases.some((item) => !item.input.trim() || !item.expected.trim())}
            onClick={() => void perform("正在保存草稿", async () => {
              const input = { workflowId: workflow?.id, expectedLatest: workflow?.versions.slice(-1)[0]?.number, baseVersion: baseNumber, draft: form, cases };
              const next = capture ? await workflowSave(capture, input) : await workflowRevise(cwd, version!.number, input);
              setWorkflow(next);
              setSelected(next.versions.slice(-1)[0]?.number);
              setForm(null);
              setNotice("流程草稿已保存");
            })}>保存草稿</button>
        </div> : version && workflow ? <div className="workflow-actions">
          <button type="button" className="capability-button" disabled={Boolean(busy)} onClick={() => editVersion(version)}>编辑为新版本</button>
          {!workflowPassed(version) && <button type="button" className="capability-button" disabled={Boolean(busy) || !version.cases.length}
            onClick={() => void perform("正在对照验证", async () => {
              setWorkflow(await workflowEvaluate(cwd, workflow.id, version.number, modelId));
              setNotice("模型输出验证完成");
            })}><GitCompare size={15} />{version.evaluation ? "重新验证" : "运行验证"}</button>}
          {version.evaluation && !workflowPassed(version) && <button type="button" className="capability-button" disabled={Boolean(busy)}
            onClick={() => void perform("正在根据验证提取改进", async () => {
              const next = await workflowSuggestRevision(cwd, workflow.id, version.number);
              if (next) {
                editVersion(version);
                setForm(next);
                setNotice("已根据首个失败用例生成改进草稿。保存前可补充一项新的验收用例。");
              } else setNotice("这次验证没有定位到明确的流程缺陷，可以手动编辑新版本。");
            })}><Sparkles size={15} />根据验证改进</button>}
          {workflow.activeVersion === version.number ? <button type="button" className="capability-button"
            disabled={Boolean(busy)} onClick={() => void perform("正在从技能库停用", async () => {
              setWorkflow(await workflowActivate(cwd, workflow.id, null, workflow.activeVersion));
              setNotice("已从技能库停用");
            })}>从技能库停用</button> : <button type="button" className="capability-button"
            disabled={Boolean(busy) || !workflowPassed(version)}
            onClick={() => void perform("正在启用版本", async () => {
              setWorkflow(await workflowActivate(cwd, workflow.id, version.number, workflow.activeVersion));
              onToast?.("流程已在此工作区的技能库启用");
              setNotice("流程已启用");
            })}>{workflow.activeVersion && workflow.activeVersion > version.number ? "恢复此版本" : "在技能库启用"}</button>}
          {task && !isBound && <button type="button" className="capability-button capability-button--primary"
            disabled={Boolean(busy) || !workflowPassed(version) || modelMismatch || !modelId}
            onClick={() => void perform("正在绑定任务版本", async () => {
              const updated = await workflowBind({ ...task, workflowSkill: binding }, { id: workflow.id, version: version.number });
              setBinding(updated.workflowSkill);
              onBindingChanged?.();
              onToast?.("定时任务已绑定此版本");
              setNotice("定时任务已绑定此版本");
            })}>用于这条定时任务</button>}
          {isBound && <span className="workflow-state"><Check size={15} />已绑定到此任务</span>}
          {!workflowPassed(version) && <button type="button" className="capability-button" disabled={Boolean(busy)}
            onClick={() => void perform("正在忽略草稿", async () => {
              await workflowDismiss(cwd, workflow.id, version.number);
              onClose();
            })}>忽略草稿</button>}
        </div> : null;
  return <Modal title={title} busy={Boolean(busy)} onClose={onClose} footer={footer}>
    <div className="workflow-dialog__body">
      {capture && form && <>
        {library.workflows.some((item) => workflowVersions(item).length) && <label>保存位置
          <select className="form-control" value={workflow?.id ?? ""} disabled={Boolean(busy)} onChange={(event) => chooseTarget(event.target.value)}>
            <option value="">新建流程</option>
            {library.workflows.filter((item) => workflowVersions(item).length).map((item) =>
              <option key={item.id} value={item.id}>{workflowVersions(item)[0].title}</option>)}
          </select>
        </label>}
        <p className="workflow-location">保存到 <span title={displayPath(cwd)}>{displayPath(cwd)}</span></p>
        {extractionModels.length > 1 && <label>提取模型
          <select className="form-control" value={extractionModel} disabled={Boolean(busy)}
            onChange={(event) => setExtractionModel(event.target.value)}>
            {extractionModels.map((id) => <option key={id} value={id}>{modelDisplayName(id)}{id === sourceModelId ? " · 来源模型" : ""}</option>)}
          </select>
        </label>}
        <label>验收结论或纠正（选填）
          <textarea className="form-control" value={correction} rows={2} maxLength={1200}
            onChange={(event) => setCorrection(event.target.value)} disabled={Boolean(busy)} />
        </label>
        <div className="workflow-actions">
          <button type="button" className="capability-button" disabled={Boolean(busy) || !extractionModel} onClick={() => void perform("正在生成草稿", async () => {
            const next = await workflowSuggest(capture, correction, extractionModel, workflow?.id, baseNumber);
            if (next) setForm(next);
            else setNotice("这次任务没有足够依据生成流程。你可以补充验收结论，或直接编辑。");
          })}><Sparkles size={15} />提取流程草稿</button>
          <small>{extractionModel
            ? `提取会把任务文字、结果和纠正说明发送给 ${modelDisplayName(extractionModel)}。`
            : "选择可用模型后可以提取，也可直接填写流程。"}</small>
        </div>
      </>}
      {form ? <>
        <label>流程名称 <input className="form-control" value={form.title} maxLength={48}
          onChange={(event) => setForm({ ...form, title: event.target.value })} disabled={Boolean(busy)} /></label>
        <label>适用范围 <input className="form-control" value={form.description} maxLength={220}
          onChange={(event) => setForm({ ...form, description: event.target.value })} disabled={Boolean(busy)} /></label>
        <label>执行流程 <textarea className="form-control" value={form.instructions} rows={8} maxLength={8000}
          onChange={(event) => setForm({ ...form, instructions: event.target.value })} disabled={Boolean(busy)} /></label>
        {workflow && <label>改动说明 <input className="form-control" value={form.change} maxLength={500}
          onChange={(event) => setForm({ ...form, change: event.target.value })} disabled={Boolean(busy)} /></label>}
        <section className="workflow-cases" aria-label="验收用例">
          <h3>验收用例</h3>
          <p>用与本次来源不同的任务输入。验证会比较模型输出，可检查文字或 JSON，不会运行工具、连接器或写入文件。</p>
          {cases.map((item, index) => {
            const update = (patch: Partial<WorkflowCase>) => setCases(cases.map((entry, position) => position === index ? { ...entry, ...patch } : entry));
            return <fieldset key={index} disabled={Boolean(busy)}>
              <legend>用例 {index + 1}</legend>
              <label>任务输入 <textarea className="form-control" value={item.input} rows={3} maxLength={4500} onChange={(event) => update({ input: event.target.value })} /></label>
              <label>判断方式 <select className="form-control" value={item.kind} onChange={(event) => update({ kind: event.target.value as WorkflowCase["kind"] })}>
                <option value="contains">包含文字</option><option value="exact">精确文本</option><option value="json">JSON 内容一致</option>
              </select></label>
              <label>预期输出 <textarea className="form-control" value={item.expected} rows={2} maxLength={2600} onChange={(event) => update({ expected: event.target.value })} /></label>
              <button className="capability-button" type="button" onClick={() => setCases(cases.filter((_, position) => position !== index))}>移除此用例</button>
            </fieldset>;
          })}
          {cases.length < 6 && <button className="capability-button" type="button" disabled={Boolean(busy)}
            onClick={() => setCases([...cases, { input: "", expected: "", kind: "contains" }])}><Plus size={15} />添加用例</button>}
        </section>
      </> : version && workflow && <>
        <div className="workflow-detail__meta">
          <label>查看版本 <select className="form-control" value={version.number} disabled={Boolean(busy)}
            onChange={(event) => { setSelected(Number(event.target.value)); setError(""); setNotice(""); }}>
            {revisions.map((entry) => <option value={entry.number} key={entry.number}>
              版本 {entry.number}{entry.number === workflow.activeVersion ? " · 已启用" : ""}{workflowPassed(entry) ? " · 模型输出已验证" : ""}
            </option>)}
          </select></label>
          <p>{version.description}</p>
          <p>{version.source.runId ? "来源于定时任务" : "来源于对话任务"} · {new Date(version.createdAt).toLocaleString()}</p>
          {onOpenSource && <button className="capability-button" type="button" onClick={() => {
            onClose(); onOpenSource(version.source.sessionId, version.source.cwd ?? cwd);
          }}>查看来源会话</button>}
        </div>
        {version.change && <p className="workflow-change">{version.change}</p>}
        {workflow.activeVersion === version.number && <small>从技能库停用后，已绑定此版本的定时任务仍会按原版本运行。</small>}
        <details open={!base}>
          <summary>{base ? "查看新旧流程" : "查看执行流程"}</summary>
          <div className={base ? "workflow-compare" : undefined}>
            {base && <div><strong>版本 {base.number}</strong><pre>{base.instructions}</pre></div>}
            <div>{base && <strong>版本 {version.number}</strong>}<pre>{version.instructions}</pre></div>
          </div>
        </details>
        {modelId && <small>{task ? "任务模型" : "验证模型"} · {modelDisplayName(modelId)}</small>}
        {workflowPassed(version) && modelMismatch && <p className="workflow-error">
          当前版本使用 {modelDisplayName(version.evaluation!.modelId)} 验证。可编辑为新版本，再用任务模型验证。
        </p>}
        <EvalResults version={version} />
        {version.evaluation && !workflowPassed(version) && <small>生成改进时，模型会分析首个失败用例的输入、预期和实际输出。</small>}
        {!version.cases.length && <small>添加验收用例并通过验证后，可以启用版本。</small>}
      </>}
      {busy && <p role="status">{busy}…</p>}
      {notice && <p role="status">{notice}</p>}
      {error && <p className="workflow-error" role="alert">{error}</p>}
    </div>
  </Modal>;
}

function WorkflowDialog(props: CommonProps & { capture: WorkflowCapture; correction?: string; onClose: () => void }) {
  const [context, setContext] = useState<WorkflowCaptureContext>();
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const captureRef = useRef(props.capture);
  useEffect(() => {
    let current = true;
    setError("");
    void workflowCaptureContext(captureRef.current).then((result) => {
      if (current) setContext(result);
    }).catch((cause) => {
      if (current) setError(errorText(cause));
    });
    return () => { current = false; };
  }, [attempt]);
  if (!context) return <Modal title="保存为可复用流程" onClose={props.onClose}>
    <div className="workflow-dialog__body">
      {error ? <><p role="alert">{error}</p><button type="button" className="capability-button" onClick={() => setAttempt(attempt + 1)}>重试</button></>
        : <p role="status">正在读取任务来源…</p>}
    </div>
  </Modal>;
  return <CaptureEditor {...props} capture={captureRef.current} cwd={context.cwd} context={context} />;
}

function CaptureEditor(props: CommonProps & { capture: WorkflowCapture; correction?: string; onClose: () => void; context: WorkflowCaptureContext }) {
  const { snapshot, error, retry } = useLibrary(props.cwd);
  if (!snapshot) return <Modal title="保存为可复用流程" onClose={props.onClose}>
    <div className="workflow-dialog__body">
      {error ? <><p role="alert">{error}</p><button type="button" className="capability-button" onClick={retry}>重试</button></>
        : <p role="status">正在读取工作区流程…</p>}
    </div>
  </Modal>;
  const existing = snapshot.workflows.find((workflow) => workflowVersions(workflow).some((version) => fromWorkflowSource(version.source, props.capture)))
    ?? snapshot.workflows.find((workflow) => workflow.id === props.context.runWorkflow?.workflowId);
  return <WorkflowEditor {...props} modelId={props.modelId ?? (props.task ? undefined : props.context.modelId ?? undefined)}
    sourceModelId={props.context.modelId} sourceBinding={props.context.runWorkflow}
    initialCorrection={props.correction} library={snapshot} workflow={existing} />;
}

export function WorkflowCaptureAction(props: CommonProps & {
  capture: WorkflowCapture; correction?: string; accepted?: number;
}) {
  const { capture, accepted, cwd } = props;
  const [open, setOpen] = useState(false);
  const [saved, setSaved] = useState<WorkflowSourceMatch | null>(null);
  const attempted = useRef<number>();
  const [automaticError, setAutomaticError] = useState("");
  const latest = useRef(props);
  latest.current = props;
  const runId = capture.kind === "scheduled" ? capture.runId : undefined;
  const sessionId = capture.kind === "chat" ? capture.sessionId : undefined;
  const promptId = capture.kind === "chat" ? capture.promptId : undefined;
  useEffect(() => {
    let live = true;
    let dispose: (() => void) | undefined;
    let generation = 0;
    const load = () => {
      const request = ++generation;
      void workflowFindSource(cwd, runId, sessionId, promptId).then((result) => {
        if (live && request === generation) setSaved(result);
      }).catch(() => {});
    };
    load();
    void listenWorkflowLibrary(load).then((stop) => {
      if (live) dispose = stop;
      else stop();
    }).catch(() => {});
    return () => { live = false; dispose?.(); };
  }, [cwd, runId, sessionId, promptId]);
  useEffect(() => {
    if (!accepted || attempted.current === accepted || open) return;
    attempted.current = accepted;
    const input = latest.current;
    setAutomaticError("");
    void workflowAutoDraft(input.capture, input.cwd, input.correction ?? "").catch((cause) => {
      setAutomaticError(errorText(cause));
    });
  }, [accepted, open]);
  return <div className="workflow-capture">
    {!saved && automaticError && <span role="status">自动提炼未完成。{automaticError}</span>}
    {saved && <span role="status">{sourceStatus[saved.status]}「{saved.title}」 · 版本 {saved.version}</span>}
    <button type="button" className="capability-button" onClick={() => setOpen(true)}><BookmarkPlus size={15} />
      {saved ? "查看流程" : "保存为可复用流程"}
    </button>
    {open && <WorkflowDialog {...props} onClose={() => setOpen(false)} />}
  </div>;
}

export function WorkflowCollection({ cwd, onToast, onOpenSource }: CommonProps) {
  const [directory, setDirectory] = useState(cwd);
  const [directories, setDirectories] = useState([cwd]);
  useEffect(() => {
    let current = true;
    void Promise.allSettled([workflowWorkspaces(), agentListWorkspaces()]).then(([workflow, recent]) => {
      if (!current) return;
      const libraries = workflow.status === "fulfilled" ? workflow.value : [];
      const workspaces = recent.status === "fulfilled" ? recent.value.map((item) => item.cwd) : [];
      setDirectories([...new Set([cwd, ...libraries, ...workspaces])]);
    });
    return () => { current = false; };
  }, [cwd]);
  return <>
    {directories.length > 1 && <label className="workflow-workspace">工作区
      <select className="form-control" value={directory} onChange={(event) => setDirectory(event.target.value)}>
        {directories.map((value) => <option value={value} key={value}>{displayPath(value)}</option>)}
      </select>
    </label>}
    <WorkspaceWorkflows key={directory} cwd={directory} onToast={onToast} onOpenSource={onOpenSource} />
  </>;
}

function WorkspaceWorkflows({ cwd, onToast, onOpenSource }: CommonProps) {
  const { snapshot, error, retry } = useLibrary(cwd);
  const [selected, setSelected] = useState<string>();
  const [changing, setChanging] = useState(false);
  const [settingError, setSettingError] = useState("");
  const [defaultModel, setDefaultModel] = useState<string>();
  useEffect(() => {
    let active = true;
    void agentAuthStatus().then((status) => { if (active) setDefaultModel(status.defaultModelId ?? undefined); }).catch(() => {});
    return () => { active = false; };
  }, []);
  const visible = snapshot?.workflows.filter((workflow) => workflowVersions(workflow).length) ?? [];
  const workflow = snapshot?.workflows.find((entry) => entry.id === selected);
  return <section className="workflow-collection" aria-label="可复用流程">
    {error && <p role="alert">{error} <button className="capability-button" type="button" onClick={retry}>重新读取</button></p>}
    {snapshot ? <>
      <label className="workflow-setting">
        <input type="checkbox" checked={snapshot.automaticDrafts} disabled={changing} onChange={async (event) => {
          const enabled = event.target.checked;
          setChanging(true);
          setSettingError("");
          try { await workflowSetAutomatic(cwd, enabled); retry(); }
          catch (cause) { setSettingError(errorText(cause)); }
          finally { setChanging(false); }
        }} /><span>根据任务反馈和定时失败自动提炼草稿
          <small>会把任务文字、结果和相关反馈发送给来源任务的模型。已有验收用例时，会自动验证候选版本。</small></span>
      </label>
      {settingError && <p role="alert">{settingError}</p>}
      {visible.length ? <ul className="workflow-list">{visible.map((entry) => {
        const latest = workflowVersions(entry)[0];
        return <li key={entry.id}>
          <div><strong>{latest.title}</strong><p>{latest.description}</p>
            <small>{entry.activeVersion ? `已启用版本 ${entry.activeVersion} · ` : ""}{workflowPassed(latest) ? "最新版模型输出已验证" : "有待验证草稿"}</small></div>
          <button type="button" className="capability-button" onClick={() => setSelected(entry.id)}>查看流程</button>
        </li>;
      })}</ul> : <p className="workflow-empty">还没有可复用流程。在已完成的任务结果里，可以保存做法并为它添加验收用例。</p>}
      {workflow && <WorkflowEditor key={workflow.id} cwd={cwd} workflow={workflow} library={snapshot} modelId={defaultModel}
        onToast={onToast} onOpenSource={onOpenSource} onClose={() => { setSelected(undefined); retry(); }} />}
    </> : !error && <p role="status">正在读取工作区流程…</p>}
  </section>;
}

export function WorkflowTaskBinding(props: CommonProps & { task: Automation }) {
  const { task } = props;
  const [open, setOpen] = useState(false);
  return <div className="workflow-task-binding">
    <span>{task.workflowSkill ? `${task.workflowSkill.title} · 版本 ${task.workflowSkill.version}` : "未绑定可复用流程"}</span>
    <button className="capability-button" type="button" onClick={() => setOpen(true)}>选择流程版本</button>
    {open && <BindingDialog {...props} onClose={() => setOpen(false)} />}
  </div>;
}

function BindingDialog({ task, cwd, modelId = task.modelId, onClose, onBindingChanged, onToast }: CommonProps & { task: Automation; onClose: () => void }) {
  const { snapshot, error, retry } = useLibrary(cwd);
  const [selected, setSelected] = useState("");
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState(false);
  const [editorOpen, setEditorOpen] = useState(false);
  const currentVersion = snapshot?.workflows
    .find((workflow) => workflow.id === task.workflowSkill?.workflowId)?.versions
    .find((version) => version.number === task.workflowSkill?.version);
  const currentModel = currentVersion?.evaluation?.modelId;
  const choices = snapshot?.workflows.flatMap((workflow) => workflowVersions(workflow)
    .filter((version) => !(workflow.id === task.workflowSkill?.workflowId
      && version.number === task.workflowSkill.version && modelId === currentModel))
    .map((version) => ({ id: workflow.id, version: version.number, title: version.title,
      verifiedModel: workflowPassed(version) ? version.evaluation?.modelId : undefined }))) ?? [];
  const choice = choices.find((item) => selected === `${item.id}:${item.version}`);
  const compatible = Boolean(modelId && choice?.verifiedModel === modelId);

  if (editorOpen && snapshot && choice) return <WorkflowEditor cwd={cwd} library={snapshot}
    workflow={snapshot.workflows.find((workflow) => workflow.id === choice.id)}
    initialVersionNumber={choice.version} task={task} modelId={modelId} onToast={onToast}
    onBindingChanged={onBindingChanged} onClose={() => setEditorOpen(false)} />;

  async function change(target?: typeof choice) {
    setBusy(true);
    setFailure("");
    try {
      await workflowBind(task, target);
      onBindingChanged?.();
      onToast?.(target ? "已切换定时任务的流程版本" : "已解除定时任务的流程绑定");
      onClose();
    } catch (cause) { setFailure(errorText(cause)); }
    finally { setBusy(false); }
  }
  return <Modal title="选择定时任务的流程版本" busy={busy} onClose={onClose}>
    <div className="workflow-dialog__body">
      <p>任务模型 · {modelId ? modelDisplayName(modelId) : "尚未就绪"}</p>
      {task.workflowSkill && <p>当前绑定 {task.workflowSkill.title} · 版本 {task.workflowSkill.version}
        {currentModel && ` · 验收模型 ${modelDisplayName(currentModel)}`}</p>}
      {currentModel && modelId && currentModel !== modelId &&
        <p role="status">当前绑定需要用任务模型重新验证后再运行。</p>}
      {error ? <p role="alert">{error} <button type="button" className="capability-button" onClick={retry}>重试</button></p>
        : !snapshot ? <p role="status">正在读取已验证版本…</p>
        : choices.length ? <label>流程版本 <select className="form-control" value={selected}
          onChange={(event) => setSelected(event.target.value)} disabled={busy}>
          <option value="">请选择版本</option>
          {choices.map((item) => <option value={`${item.id}:${item.version}`} key={`${item.id}:${item.version}`}>{item.title} · 版本 {item.version} · {item.verifiedModel ? modelDisplayName(item.verifiedModel) : "待验证"}</option>)}
        </select></label> : <p>当前工作区没有其他流程版本。可以从任务结果保存流程。</p>}
      {choice && !compatible && <p>{choice.verifiedModel
        ? `此版本使用 ${modelDisplayName(choice.verifiedModel)} 验证。请用任务模型验证新版本。`
        : "请先使用任务模型完成验证。"} </p>}
      <div className="workflow-actions">
        {task.workflowSkill && <button type="button" className="capability-button" disabled={busy} onClick={() => void change()}>解除绑定</button>}
        {choice && !compatible && <button type="button" className="capability-button" onClick={() => setEditorOpen(true)}>查看并验证</button>}
        <button type="button" className="capability-button capability-button--primary" disabled={!compatible || busy} onClick={() => void change(choice)}>确认切换</button>
      </div>
      {failure && <p role="alert" className="workflow-error">{failure}</p>}
    </div>
  </Modal>;
}
