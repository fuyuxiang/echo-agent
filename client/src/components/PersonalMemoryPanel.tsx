import { useCallback, useContext, useEffect, useRef, useState } from "react";
import { ArrowLeft, Brain, Check, ChevronDown, ChevronRight, Download, LockKeyhole, Pencil, Search, Trash2 } from "lucide-react";
import { ActionMenu } from "./ActionMenu";
import { Button, IconButton } from "./Button";
import { LocalRetrievalModels } from "./LocalRetrievalModels";
import { PageHeading } from "./PageHeading";
import { ResourcesPanel } from "./ResourcesPanel";
import { MemorySettingsPanel } from "./SettingsSections";
import { useAppDialog } from "./AppDialog";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { SettingsDraftContext, type SettingsDraftState } from "@/lib/settings-drafts";
import { parseMemoryImport } from "@/lib/memory-import";
import { memoryFlushMessage } from "@/lib/memory-feedback";
import { memoryRecordsGet, memoryRecordSave, memoryRecordRetire, memoryFlush, type MemoryConfig, type PersonalMemoryInput, type PersonalMemoryRecord, type PersonalMemorySnapshot } from "@/lib/agent-client";
import "@/styles/personal-memory.css";

const kinds = { user: "个人信息与偏好", feedback: "回复与执行约定", project: "项目记忆", reference: "常用资料", task: "待跟进事项" };
const date = (seconds?: number | null) => seconds ? new Date(seconds * 1000).toLocaleString("zh-CN") : "尚无记录";
const active = (record: PersonalMemoryRecord) => record.status === "active" && (!record.validUntil || record.validUntil * 1000 > Date.now());
const message = (error: unknown) => String(error).replace(/^Error:\s*/, "");
type View = "overview" | "manage" | "history" | "support" | "documents";
type Props = { cwd?: string; sessionId?: string; initialView?: "overview" | "manage"; onToast?: (text: string) => void; onOpenSession?: (id: string, cwd?: string) => void | Promise<void>; onUnsavedChange?: (dirty: boolean) => void; onBusyChange?: (busy: boolean) => void };
type Draft = { input: PersonalMemoryInput; cwd?: string; baseline: string };

export function PersonalMemoryPanel({ cwd, sessionId, initialView = "overview", onToast, onOpenSession, onUnsavedChange, onBusyChange }: Props) {
  const [view, setView] = useState<View>(initialView);
  const [snapshot, setSnapshot] = useState<PersonalMemorySnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState<Draft | null>(null);
  const [importing, setImporting] = useState(false);
  const [importText, setImportText] = useState("");
  const [previews, setPreviews] = useState<PersonalMemoryInput[]>([]);
  const [importCwd, setImportCwd] = useState(cwd);
  const [exportText, setExportText] = useState("");
  const [visibleLimit, setVisibleLimit] = useState(30);
  const [legacyState, setLegacyState] = useState({ dirty: false, busy: false });
  const legacyDrafts = useRef(new Map<string, SettingsDraftState>());
  const outerDraftReport = useContext(SettingsDraftContext);
  const reportLegacyDraft = useCallback((id: string, state: SettingsDraftState | null) => {
    if (state) legacyDrafts.current.set(id, state); else legacyDrafts.current.delete(id);
    setLegacyState({ dirty: [...legacyDrafts.current.values()].some(value => value.dirty), busy: [...legacyDrafts.current.values()].some(value => value.busy) });
    outerDraftReport(id, state);
  }, [outerDraftReport]);
  const locked = busy || settingsBusy || legacyState.busy;
  const draftDirty = draft !== null && JSON.stringify(draft.input) !== draft.baseline;
  const dirty = draftDirty || legacyState.dirty || (importing && (Boolean(importText.trim()) || previews.length > 0));
  const beginEdit = (input: PersonalMemoryInput) => setDraft({ cwd, input, baseline: JSON.stringify(input) });
  useEffect(() => onUnsavedChange?.(dirty), [dirty, onUnsavedChange]);
  useEffect(() => onBusyChange?.(locked), [locked, onBusyChange]);
  useEffect(() => () => { onUnsavedChange?.(false); onBusyChange?.(false); }, [onUnsavedChange, onBusyChange]);
  useEffect(() => setVisibleLimit(30), [view, query, cwd]);
  const generation = useRef(0);
  const panelRef = useRef<HTMLElement>(null);
  const editorRef = useRef<HTMLFormElement>(null);
  const importRef = useRef<HTMLTextAreaElement>(null);
  const previousView = useRef(view);
  useEffect(() => {
    if (previousView.current === view) return;
    previousView.current = view;
    const container = panelRef.current?.closest(".settings-modal__panel");
    if (container) container.scrollTop = 0;
    const heading = panelRef.current?.querySelector("h2");
    heading?.setAttribute("tabindex", "-1");
    heading?.focus({ preventScroll: true });
  }, [view]);
  const editing = draft !== null;
  const draftId = draft?.input.id;
  useEffect(() => {
    if (!editing) return;
    editorRef.current?.scrollIntoView?.({ block: "nearest", behavior: "smooth" });
    editorRef.current?.querySelector<HTMLTextAreaElement>("textarea")?.focus({ preventScroll: true });
  }, [editing, draftId]);
  useEffect(() => { if (importing) importRef.current?.focus(); }, [importing]);
  const currentCwd = useRef(cwd); currentCwd.current = cwd;
  const { requestConfirmation, dialog } = useAppDialog(cwd);
  const destination = useRef<View>("overview");
  const { requestClose, closeDialog } = useUnsavedClose({ dirty, busy: locked, onClose: () => {
    setDraft(null); setImporting(false); setImportText(""); setPreviews([]); setExportText("");
    setView(destination.current);
  } });
  const navigate = (next: View) => { destination.current = next; requestClose(); };
  const reload = useCallback(async (quiet = false) => {
    const version = ++generation.current;
    if (!quiet) setLoading(true);
    try {
      const value = await memoryRecordsGet(cwd);
      if (generation.current === version && currentCwd.current === cwd) { setSnapshot(value); setError(""); }
    } catch (e) { if (generation.current === version) setError(message(e)); }
    finally { if (generation.current === version) setLoading(false); }
  }, [cwd]);
  useEffect(() => {
    setSnapshot(null); void reload();
    const timer = window.setInterval(() => { if (!document.hidden) void reload(true); }, 5000);
    return () => { generation.current += 1; window.clearInterval(timer); };
  }, [reload]);
  const onConfigChange = useCallback((config: MemoryConfig) => {
    setSnapshot(value => value ? { ...value, enabled: config.enabled, autoProcessingEnabled: config.autoFlushEnabled } : value);
  }, []);
  const save = async () => {
    if (!draft || !draft.input.content.trim()) return;
    setBusy(true);
    try { await memoryRecordSave({ ...draft.input, key: draft.input.key.trim() || draft.input.content.trim().slice(0, 60) }, draft.cwd); setDraft(null); onToast?.("记忆已保存"); await reload(true); }
    catch (e) { setError(message(e)); } finally { setBusy(false); }
  };
  const retire = (record: PersonalMemoryRecord, completed = false) => {
    const sourceCwd = cwd;
    requestConfirmation({ title: completed ? "将这件事标记为已完成？" : `忘记“${record.key}”？`,
      description: completed ? "停止把它作为待办提醒，原聊天记录保留。" : "停止自动引用这条记忆，并排除已知来源会话的自动提取和助手历史检索，防止重新记住。原聊天仍可在历史记录中查看。",
      confirmLabel: completed ? "标记完成" : "忘记", danger: !completed,
      action: async () => { setBusy(true); try { await memoryRecordRetire(record, sourceCwd, completed); await reload(true); onToast?.(completed ? "已标记完成" : "已忘记这条记忆"); } finally { setBusy(false); } },
      onError: (e) => setError(message(e)),
    });
  };
  const records = snapshot?.records ?? [];
  const filtered = records.filter(record => (view === "history" ? !active(record) : active(record))
    && (!query.trim() || `${record.key} ${record.content} ${kinds[record.kind]}`.toLowerCase().includes(query.toLowerCase().trim())));
  const copyExport = async () => {
    const text = JSON.stringify(records.filter(active), null, 2);
    try { await navigator.clipboard.writeText(text); onToast?.("记忆已复制"); } catch { setExportText(text); }
  };
  const importRecords = async () => {
    setBusy(true); let saved = 0;
    try {
      for (const input of previews) { await memoryRecordSave(input, importCwd); saved += 1; }
      setImporting(false); setImportText(""); setPreviews([]); onToast?.(`已导入 ${saved} 条记忆`); await reload(true);
    } catch (e) { setPreviews(values => values.slice(saved)); setError(`已导入 ${saved} 条，剩余内容未保存：${message(e)}`); }
    finally { setBusy(false); }
  };
  const extraction = snapshot?.extraction;
  const autoProcessing = !!snapshot?.enabled && snapshot.autoProcessingEnabled !== false;
  const extractionSummary = extraction?.outcome === "queued" ? (!snapshot?.enabled ? (snapshot?.autoProcessingEnabled === false ? "待整理内容已保存在本机，开启记忆并恢复自动整理后继续处理" : "待整理内容已保存在本机，开启记忆后继续处理")
    : !autoProcessing ? "待整理内容已保存在本机，自动整理已暂停，可恢复推荐策略后继续处理"
    : "待整理内容已保存在本机，将自动继续处理")
    : extraction?.outcome === "running" ? "正在提取记忆…"
    : extraction?.outcome === "failed" ? "最近一次提取未完成"
    : extraction?.outcome === "unchanged" ? "最近一次提取没有新增内容，现有记忆保持不变"
    : extraction?.outcome === "saved" ? "最近一次提取已保存记忆"
    : extraction?.succeededAt ? "最近一次记忆提取已完成"
    : "开始使用后，会自动提取有价值的信息";
  const retry = async () => {
    if (!sessionId) return;
    setBusy(true);
    try { const done = await memoryFlush(sessionId); onToast?.(memoryFlushMessage(done)); await reload(true); }
    catch (e) { setError(message(e)); } finally { setBusy(false); }
  };
  const isDraftRecord = (record: PersonalMemoryRecord) => !!draft?.input.id && draft.input.id === record.id && draft.input.scope === record.scope;
  const editor = draft && <form ref={editorRef} className="personal-memory__editor" aria-label="编辑记忆" onSubmit={e => { e.preventDefault(); void save(); }}>
    <label>记忆内容<textarea disabled={locked} className="form-control" required maxLength={4000} rows={4} placeholder="例如：汇报先给结论，再说明依据。" value={draft.input.content} onChange={e => setDraft({ ...draft, input: { ...draft.input, content: e.target.value } })} /></label>
    {draft.cwd !== cwd && <p role="note">此草稿仍保存到原工作区：{draft.cwd ?? "个人记忆"}</p>}
    {!draft.input.id && draft.cwd && <label>适用范围<select disabled={locked} className="form-control" value={draft.input.scope} onChange={e => setDraft({ ...draft, input: { ...draft.input, scope: e.target.value as PersonalMemoryInput["scope"], kind: e.target.value === "global" ? "user" : "project" } })}><option value="global">个人</option><option value="workspace">当前项目</option></select></label>}
    <div className="personal-memory__editor-actions"><Button size="compact" disabled={locked} onClick={() => navigate(view)}>取消</Button><Button type="submit" size="compact" variant="primary" loading={busy} disabled={locked || !draft.input.content.trim()}>保存记忆</Button></div>
  </form>;
  const title = { overview: "记忆", manage: "管理记忆", history: "历史记录", support: "记忆状态与排查", documents: "原有记忆文档" }[view];

  return <section ref={panelRef} className={`personal-memory personal-memory--${view}`} aria-label="个人记忆管理">
    {view !== "overview" && <Button size="compact" variant="ghost" className="personal-memory__back" disabled={locked} onClick={() => navigate(view === "manage" ? "overview" : "manage")}><ArrowLeft size={14} />{view === "manage" ? "返回记忆设置" : "返回管理记忆"}</Button>}
    <header className="personal-memory__header" data-setting-id={view === "manage" ? "memory-management" : undefined}>
      <PageHeading title={title} level={2} description={view === "manage" ? "你可以随时更正，或让 Echo 忘记。" : view === "history" ? "已完成、已过期和已忘记的内容不会再自动参考。" : undefined} />
      {view === "manage" && <ActionMenu label="更多" menuLabel="记忆操作" resetKey={cwd} busy={locked || !!draft || importing} menuZIndex="var(--echo-layer-dialog-popover)" triggerClassName="settings-btn personal-memory__more" trigger={<>更多<ChevronDown size={14} /></>}>
        {({ close }) => <>
          <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(); setExportText(""); setImporting(true); setImportCwd(cwd); setPreviews([]); }}>导入记忆</button>
          <button type="button" role="menuitem" className="topbar-actions__item" disabled={!records.some(active)} onClick={() => { close(true); void copyExport(); }}>复制导出</button>
          <hr className="personal-memory__menu-separator" />
          <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(); beginEdit({ scope: "global", kind: "user", key: "", content: "", pinned: true, validUntil: null }); }}>添加记忆</button>
          {records.some(record => !active(record)) && <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(); navigate("history"); }}>历史记录</button>}
          <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(); navigate("documents"); }}>原有记忆文档</button>
          <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(); navigate("support"); }}>记忆状态与排查</button>
        </>}
      </ActionMenu>}
    </header>

    {view === "overview" ? <>
      <div className="personal-memory__preferences">
        <MemorySettingsPanel compact onBusyChange={setSettingsBusy} onConfigChange={onConfigChange} />
        <div className="personal-memory__manage-entry"><div><p className="personal-memory__entry-title">管理已保存的记忆</p><p className="personal-memory__entry-description">查看、更正或忘记 Echo 保存的内容。</p></div><Button size="compact" disabled={locked} onClick={() => navigate("manage")}>管理<ChevronRight size={14} /></Button></div>
      </div>
      <p className="personal-memory__privacy"><LockKeyhole size={14} aria-hidden="true" />记忆保存在本机，自动整理会使用所配置的模型服务。</p>
    </> : <>
      {error && <div className="panel-inline-error" role="alert"><span>{error}</span><Button size="compact" disabled={locked} onClick={() => void reload()}>重新加载</Button></div>}
      {(view === "manage" || view === "history") && <>
        {importing && <section className="personal-memory__import personal-memory__editor" aria-label="导入记忆">
          <h3>导入记忆</h3><p>粘贴导出的 JSON，或每行一条偏好与事实。检查预览后再保存。</p>
          {importCwd !== cwd && <p role="note">项目记忆仍会导入原工作区：{importCwd ?? "个人记忆"}</p>}
          <textarea ref={importRef} disabled={locked} className="form-control" aria-label="待导入的记忆" rows={5} value={importText} onChange={e => { setImportText(e.target.value); setPreviews([]); }} />
          <Button size="compact" disabled={locked || !importText.trim()} onClick={() => { try { const items = parseMemoryImport(importText); if (!items.length) throw new Error("未识别到有效记忆，请检查内容"); if (!importCwd && items.some(value => value.scope === "workspace")) throw new Error("这些记忆需要工作区，请先打开对应项目"); setPreviews(items); setError(""); } catch (e) { setError(message(e)); } }}>预览导入</Button>
          {previews.map((item, index) => <div className="personal-memory__import-row" key={index}><label>第 {index + 1} 条 · {item.scope === "global" ? "个人" : "原工作区"}<textarea disabled={locked} className="form-control" maxLength={4000} value={item.content} onChange={e => setPreviews(values => values.map((value, at) => at === index ? { ...value, content: e.target.value } : value))} /></label><IconButton disabled={locked} label={`移除导入项 ${index + 1}`} onClick={() => setPreviews(values => values.filter((_, at) => at !== index))}><Trash2 size={14} /></IconButton></div>)}
          <div className="personal-memory__editor-actions"><Button size="compact" disabled={locked} onClick={() => navigate(view)}>取消导入</Button>{previews.length > 0 && <Button size="compact" variant="primary" loading={busy} disabled={locked || previews.some(item => !item.content.trim())} onClick={() => void importRecords()}><Download size={14} />添加 {previews.length} 条记忆</Button>}</div>
        </section>}
        {exportText && <section className="personal-memory__editor" aria-label="导出记忆"><label>导出内容（可复制保存）<textarea className="form-control" rows={6} readOnly value={exportText} onFocus={e => e.target.select()} /></label><Button size="compact" onClick={() => setExportText("")}>关闭导出</Button></section>}
        {(records.length > 8 || query.trim()) && <label className="personal-memory__search"><Search size={16} /><input className="form-control" aria-label="查找个人记忆" placeholder="查找记忆" value={query} disabled={!!draft || importing || locked} onChange={e => setQuery(e.target.value)} /></label>}
        {loading && <p role="status" className="personal-memory__status">正在加载记忆…</p>}
        {!loading && !error && filtered.length === 0 && !draft && <div className="personal-memory__empty"><Brain size={24} aria-hidden="true" /><p>{query ? "没有找到匹配的记忆" : view === "history" ? "暂无历史记录" : "还没有保存的记忆"}</p><span>{query.trim() ? "试试其他关键词，或清空搜索。" : view === "history" ? "已完成、过期或忘记的内容会显示在这里。" : snapshot?.enabled === false ? "开启记忆后，Echo 会自动记住有用的偏好和工作背景。" : "正常使用时，Echo 会自动记住有用的偏好和工作背景。你也可以在对话中说“记住我的这个习惯”。"}</span></div>}
        <div className="personal-memory__list">
          {draft && !filtered.slice(0, visibleLimit).some(isDraftRecord) && editor}
          {filtered.slice(0, visibleLimit).map(record => isDraftRecord(record) ? <div className="personal-memory__card" key={`${record.scope}:${record.id}`}>{editor}</div> : <article className="personal-memory__card" key={`${record.scope}:${record.id}`} aria-label={record.key}>
            <div className="personal-memory__record-main"><p className="personal-memory__content">{record.content}</p>{active(record) && <div className="personal-memory__actions">
              {record.kind === "task" && <IconButton label={`完成 ${record.key}`} title="标记完成" disabled={locked || !!draft || importing} onClick={() => retire(record, true)}><Check size={15} /></IconButton>}
              <IconButton label={`编辑 ${record.key}`} title="编辑" disabled={locked || !!draft || importing} onClick={() => beginEdit({ id: record.id, scope: record.scope, kind: record.kind, key: record.key, content: record.content, pinned: record.pinned, validUntil: record.validUntil, evidence: record.evidence, expectedRevision: record.revision })}><Pencil size={15} /></IconButton>
              <IconButton label={`忘记 ${record.key}`} title="忘记" disabled={locked || !!draft || importing} onClick={() => retire(record)}><Trash2 size={15} /></IconButton>
            </div>}</div>
            <div className="personal-memory__record-meta"><span>{record.scope === "global" ? "个人" : "当前项目"}</span>{active(record) && record.kind === "task" && <span className="personal-memory__task">待跟进</span>}{!active(record) && <span>{record.status === "completed" ? "已完成" : record.status === "forgotten" ? "已忘记" : "已过期"}</span>}</div>
            <details className="personal-memory__record-details"><summary>查看来源</summary>
              <div className="personal-memory__record-facts"><span>{record.origin === "manual" ? "手动保存" : "从对话提取"} · {date(record.updatedAt)}</span><span>{kinds[record.kind]}{record.validUntil ? ` · 有效至 ${date(record.validUntil)}` : ""}</span>
                {record.evidence && <blockquote>{record.evidence}</blockquote>}{record.sources.map((id, index) => onOpenSession ? <Button key={id} size="compact" variant="ghost" aria-label={`打开来源对话 ${id.slice(0, 8)}`} disabled={locked || dirty} onClick={async () => { try { await onOpenSession(id); } catch (e) { setError(message(e)); } }}>来源对话 {index + 1}</Button> : <span key={id}>来源对话 {id.slice(0, 8)}</span>)}
              </div>
            </details>
          </article>)}
        </div>
        {filtered.length > visibleLimit && <Button disabled={!!draft || importing} onClick={() => setVisibleLimit(value => value + 30)}>查看更多记忆（还有 {filtered.length - visibleLimit} 条）</Button>}
      </>}
      {view === "support" && <>
        <div className="personal-memory__health"><div aria-live="polite"><span>{extractionSummary}</span>
          {!!extraction?.succeededAt && <span>上次完成时间：{date(extraction.succeededAt)}</span>}
          {!!snapshot?.pendingCount && <span>{snapshot.pendingCount} 项内容待整理，{autoProcessing ? "打开工作区后自动继续" : "已暂停自动整理"}；不影响已保存的聊天记录</span>}
          {!!snapshot?.quarantinedCount && <p role="note">已隔离 {snapshot.quarantinedCount} 项损坏的待整理内容，原件已保留在本机，不影响其他内容整理和应用退出。损坏部分未完成提取，可打开原对话重新提取；已保存的聊天记录不受影响。</p>}
          {sessionId && <Button size="compact" disabled={locked || !snapshot?.enabled} onClick={() => void retry()}>立即提取</Button>}
          {extraction?.outcome === "failed" && <details><summary>查看原因</summary><p>{extraction.detail}</p></details>}
        </div></div>
        <LocalRetrievalModels />
        {!!snapshot?.retrieval.at && <details className="personal-memory__health"><summary>最近一次检索参考了 {snapshot.retrieval.count} 处记忆 · {date(snapshot.retrieval.at)}</summary><ul>{snapshot.retrieval.sources.map(source => <li key={source}><code>{source}</code></li>)}</ul><span>{snapshot.retrieval.mode === "Hybrid" ? "语义与关键词检索" : "关键词检索（语义检索暂不可用，已有记忆仍可参考）"}</span></details>}
        <Button size="compact" disabled={loading || locked} onClick={() => void reload()}>刷新状态</Button>
      </>}
      {view === "documents" && <div className="personal-memory__legacy"><SettingsDraftContext.Provider value={reportLegacyDraft}><ResourcesPanel cwd={cwd} sessionId={sessionId} onToast={onToast} /></SettingsDraftContext.Provider></div>}
    </>}
    {dialog}{closeDialog}
  </section>;
}
