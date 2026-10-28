import { useCallback, useEffect, useRef, useState } from "react";
import { Brain, Plus, RefreshCw, Search, Pin, Pencil, Trash2, Check, Copy, Download } from "lucide-react";
import { Button, IconButton } from "./Button";
import { ResourcesPanel } from "./ResourcesPanel";
import { MemorySettingsPanel } from "./SettingsSections";
import { useAppDialog } from "./AppDialog";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { parseMemoryImport } from "@/lib/memory-import";
import { memoryFlushMessage } from "@/lib/memory-feedback";
import { memoryRecordsGet, memoryRecordSave, memoryRecordRetire, memoryFlush, type PersonalMemoryInput, type PersonalMemoryRecord, type PersonalMemorySnapshot } from "@/lib/agent-client";
import "@/styles/personal-memory.css";

const kinds = { user: "个人信息与偏好", feedback: "行为约定", project: "项目事实与决策", reference: "常用资料", task: "近期跟进" };
const date = (seconds?: number | null) => seconds ? new Date(seconds * 1000).toLocaleString("zh-CN") : "尚无记录";
const active = (record: PersonalMemoryRecord) => record.status === "active" && (!record.validUntil || record.validUntil * 1000 > Date.now());
const message = (error: unknown) => String(error).replace(/^Error:\s*/, "");
type Props = { cwd?: string; sessionId?: string; onToast?: (text: string) => void; onOpenSession?: (id: string, cwd?: string) => void | Promise<void>; onUnsavedChange?: (dirty: boolean) => void; onBusyChange?: (busy: boolean) => void };
type Draft = { input: PersonalMemoryInput; cwd?: string };

export function PersonalMemoryPanel({ cwd, sessionId, onToast, onOpenSession, onUnsavedChange, onBusyChange }: Props) {
  const [snapshot, setSnapshot] = useState<PersonalMemorySnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<"active" | "tasks" | "inactive">("active");
  const [draft, setDraft] = useState<Draft | null>(null);
  const [importing, setImporting] = useState(false);
  const [importText, setImportText] = useState("");
  const [previews, setPreviews] = useState<PersonalMemoryInput[]>([]);
  const [importCwd, setImportCwd] = useState(cwd);
  const [exportText, setExportText] = useState("");
  const [legacyOpen, setLegacyOpen] = useState(false);
  const [visibleLimit, setVisibleLimit] = useState(30);
  const dirty = !!draft?.input.content.trim() || (importing && !!importText.trim());
  useEffect(() => onUnsavedChange?.(dirty), [dirty, onUnsavedChange]);
  useEffect(() => onBusyChange?.(busy), [busy, onBusyChange]);
  useEffect(() => () => { onUnsavedChange?.(false); onBusyChange?.(false); }, [onUnsavedChange, onBusyChange]);
  useEffect(() => setVisibleLimit(30), [tab, query, cwd]);
  const generation = useRef(0);
  const editorRef = useRef<HTMLFormElement>(null);
  const editing = draft !== null;
  const draftId = draft?.input.id;
  useEffect(() => {
    if (!editing) return;
    editorRef.current?.scrollIntoView?.({ block: "nearest", behavior: "smooth" });
    editorRef.current?.querySelector<HTMLTextAreaElement>("textarea")?.focus({ preventScroll: true });
  }, [editing, draftId]);
  const currentCwd = useRef(cwd); currentCwd.current = cwd;
  const { requestConfirmation, dialog } = useAppDialog(cwd);
  const { requestClose, closeDialog } = useUnsavedClose({ dirty: !!draft?.input.content.trim(), busy, onClose: () => setDraft(null) });
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
  const save = async () => {
    if (!draft) return;
    setBusy(true);
    try { await memoryRecordSave({ ...draft.input, key: draft.input.key.trim() || draft.input.content.trim().slice(0, 60) }, draft.cwd); setDraft(null); await reload(true); }
    catch (e) { setError(message(e)); } finally { setBusy(false); }
  };
  const retire = (record: PersonalMemoryRecord, completed = false) => {
    const sourceCwd = cwd;
    requestConfirmation({ title: completed ? "将这件事标记为已完成？" : `忘记“${record.key}”？`,
      description: completed ? "停止把它作为待办提醒，原聊天记录保留。" : "停止自动引用这条记忆，并排除已知来源会话的自动提取和助手历史检索，防止重新记住。原聊天仍可在历史记录中查看。",
      confirmLabel: completed ? "标记完成" : "忘记", danger: !completed,
      action: async () => { setBusy(true); try { await memoryRecordRetire(record, sourceCwd, completed); await reload(true);  } finally { setBusy(false); } },
      onError: (e) => setError(message(e)),
    });
  };
  const records = snapshot?.records ?? [];
  const filtered = records.filter((record) => (tab === "inactive" ? !active(record) : active(record) && (tab !== "tasks" || record.kind === "task"))
    && (!query.trim() || `${record.key} ${record.content} ${kinds[record.kind]}`.toLowerCase().includes(query.toLowerCase().trim())));
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
  return <section className="personal-memory" aria-label="个人记忆管理">
    <header className="personal-memory__header"><div><h2><Brain size={20} /> 记忆</h2><p>记住你的习惯、项目约定和近期跟进事项。你可以随时更正或让 Echo 忘记。</p></div>
      <IconButton label="刷新个人记忆" disabled={loading || busy} onClick={() => void reload()}><RefreshCw size={16} /></IconButton></header>
    <MemorySettingsPanel compact />
    <div className="personal-memory__context">{cwd ? <>当前工作区 <code title={cwd}>{cwd}</code></> : "当前仅查看跨项目的全局记忆"}</div>
    {snapshot && !snapshot.enabled && <p className="personal-memory__notice" role="status">记忆已关闭，后续对话不再提取或使用记忆。已有内容仍可查看和管理。</p>}
    {error && <div className="panel-inline-error" role="alert"><span>{error}</span><Button size="compact" onClick={() => void reload()}>重新加载</Button></div>}
    <div className="personal-memory__toolbar"><div role="tablist" aria-label="记忆分类">{([["active", "有效记忆"], ["tasks", "近期跟进"], ["inactive", "已停用"]] as const).map(([value, label]) => <button key={value} role="tab" aria-selected={tab === value} onClick={() => setTab(value)}>{label}</button>)}</div>
      <Button size="compact" disabled={busy || !!draft || importing} onClick={() => setDraft({ cwd, input: { scope: "global", kind: "user", key: "", content: "", pinned: true, validUntil: null } })}><Plus size={15} /> 添加记忆</Button>
      <Button size="compact" disabled={busy || !!draft || importing} onClick={() => { setImporting(true); setImportCwd(cwd); setPreviews([]); }}>导入记忆</Button>
      <Button size="compact" disabled={!records.some(active)} onClick={async () => { const text = JSON.stringify(records.filter(active), null, 2); try { await navigator.clipboard.writeText(text); onToast?.("记忆已复制"); } catch { setExportText(text); } }}><Copy size={14} /> 复制导出</Button>
    </div>
    <label className="personal-memory__search"><Search size={16} /><input className="form-control" aria-label="查找个人记忆" placeholder="查找偏好、项目或事项" value={query} onChange={(e) => setQuery(e.target.value)} /></label>
    {loading && <p role="status">正在加载记忆…</p>}
    {!loading && !error && filtered.length === 0 && <div className="personal-memory__empty"><Brain size={28} /><p>{query ? "没有找到匹配的记忆" : tab === "tasks" ? "暂无待跟进事项" : tab === "inactive" ? "暂无已完成、过期或忘记的记忆" : "还没有整理好的记忆条目"}</p><span>也可以在对话中说“记住我的这个习惯”，或使用 /remember 保存。旧版文档可在下方高级区域查看。</span></div>}
    <div className="personal-memory__list">{filtered.slice(0, visibleLimit).map((record) => <article className="personal-memory__card" key={`${record.scope}:${record.id}`}>
      <div className="personal-memory__card-head"><span>{kinds[record.kind]}</span><span>{record.scope === "global" ? "所有项目" : "当前工作区"}</span>{record.pinned && <span><Pin size={12} /> 已固定</span>}{!active(record) && <span>{record.status === "completed" ? "已完成" : record.status === "forgotten" ? "已忘记" : "已过期"}</span>}</div>
      <h3>{record.key}</h3><p className="personal-memory__content">{record.content}</p>
      <footer><span>{record.origin === "manual" ? "手动保存" : "从对话提取"} · {date(record.updatedAt)}{record.validUntil ? ` · 有效至 ${date(record.validUntil)}` : ""}</span>
        {active(record) && <div className="personal-memory__actions">{record.kind === "task" && <IconButton label={`完成 ${record.key}`} disabled={busy} onClick={() => retire(record, true)}><Check size={15} /></IconButton>}
          <IconButton label={`更正 ${record.key}`} disabled={busy || !!draft || importing} onClick={() => setDraft({ cwd, input: { id: record.id, scope: record.scope, kind: record.kind, key: record.key, content: record.content, pinned: record.pinned, validUntil: record.validUntil, evidence: record.evidence, expectedRevision: record.revision } })}><Pencil size={15} /></IconButton>
          <IconButton label={`忘记 ${record.key}`} disabled={busy} onClick={() => retire(record)}><Trash2 size={15} /></IconButton></div>}
      </footer>
      {(record.sources.length > 0 || record.evidence) && <details><summary>来源与依据</summary>{record.evidence && <blockquote>{record.evidence}</blockquote>}{record.sources.map((id) => onOpenSession ? <Button key={id} size="compact" variant="ghost" disabled={busy || dirty} onClick={async () => { try { await onOpenSession(id); } catch (e) { setError(message(e)); } }}>打开来源对话 {id.slice(0, 8)}</Button> : <span key={id}>来源对话 {id.slice(0, 8)}</span>)}</details>}
    </article>)}</div>
    {filtered.length > visibleLimit && <Button onClick={() => setVisibleLimit((value) => value + 30)}>查看更多记忆（还有 {filtered.length - visibleLimit} 条）</Button>}
    {draft && <form ref={editorRef} className="personal-memory__editor" aria-label="编辑记忆" onSubmit={(e) => { e.preventDefault(); void save(); }}>
      <h3>{draft.input.id ? "更正记忆" : "添加记忆"}</h3>{draft.cwd !== cwd && <p role="note">此草稿仍保存到原工作区：{draft.cwd ?? "全局"}</p>}
      <label>记忆内容<textarea className="form-control" required maxLength={4000} rows={5} placeholder="例如：汇报先给结论，再说明依据。" value={draft.input.content} onChange={(e) => setDraft({ ...draft, input: { ...draft.input, content: e.target.value } })} /></label>
      <label>适用范围<select className="form-control" value={draft.input.scope} disabled={!!draft.input.id} onChange={(e) => setDraft({ ...draft, input: { ...draft.input, scope: e.target.value as PersonalMemoryInput["scope"], kind: e.target.value === "global" ? "user" : "project" } })}><option value="global">所有项目</option>{draft.cwd && <option value="workspace">当前工作区</option>}</select></label>
      <details><summary>更多选项</summary>
        <label>主题（可选）<input className="form-control" maxLength={160} value={draft.input.key} onChange={(e) => setDraft({ ...draft, input: { ...draft.input, key: e.target.value } })} /></label>
        <label>类型<select className="form-control" value={draft.input.kind} onChange={(e) => setDraft({ ...draft, input: { ...draft.input, kind: e.target.value as PersonalMemoryInput["kind"] } })}>{Object.entries(kinds).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
        <label>有效期（可选）<input type="date" className="form-control" value={draft.input.validUntil ? new Date(draft.input.validUntil * 1000).toLocaleDateString("sv-SE") : ""} onChange={(e) => setDraft({ ...draft, input: { ...draft.input, validUntil: e.target.value ? Math.floor(new Date(`${e.target.value}T23:59:59`).getTime() / 1000) : null } })} /></label>
        <label className="personal-memory__checkbox"><input type="checkbox" checked={draft.input.pinned} onChange={(e) => setDraft({ ...draft, input: { ...draft.input, pinned: e.target.checked } })} /> 优先参考这条记忆</label>
      </details>
      <p className="settings-hint">你保存的内容会优先保留，不会被自动整理覆盖。</p>
      <div className="personal-memory__actions"><Button type="submit" variant="primary" loading={busy}>保存记忆</Button><Button disabled={busy} onClick={requestClose}>取消</Button></div>
    </form>}
    {importing && <section className="personal-memory__editor" aria-label="导入记忆"><h3>从其他 AI 或备份导入</h3>{importCwd !== cwd && <p role="note">项目记忆仍会导入原工作区：{importCwd ?? "全局"}</p>}<p>粘贴导出的 JSON，或每行一条偏好与事实。先检查预览，再添加到记忆。</p>
      <textarea className="form-control" aria-label="待导入的记忆" rows={6} value={importText} onChange={(e) => { setImportText(e.target.value); setPreviews([]); }} />
      <Button disabled={busy || !importText.trim()} onClick={() => { try { const items = parseMemoryImport(importText); if (!items.length) throw new Error("未识别到有效记忆，请检查内容"); if (!importCwd && items.some((v) => v.scope === "workspace")) throw new Error("这些记忆需要工作区，请先打开对应项目"); setPreviews(items); setError(""); } catch (e) { setError(message(e)); } }}>预览导入</Button>
      {previews.map((item, i) => <div className="personal-memory__import-row" key={i}><label>第 {i + 1} 条 · {item.scope === "global" ? "所有项目" : "原工作区"}<textarea className="form-control" value={item.content} maxLength={4000} onChange={(e) => setPreviews((values) => values.map((v, n) => n === i ? { ...v, content: e.target.value } : v))} /></label><IconButton label={`移除导入项 ${i + 1}`} onClick={() => setPreviews((values) => values.filter((_, n) => n !== i))}><Trash2 size={14} /></IconButton></div>)}
      <div className="personal-memory__actions">{previews.length > 0 && <Button variant="primary" loading={busy} onClick={async () => { setBusy(true); let saved = 0; try { for (const input of previews) { await memoryRecordSave(input, importCwd); saved += 1; } setImporting(false); setImportText(""); setPreviews([]); onToast?.(`已导入 ${saved} 条记忆`); await reload(true); } catch (e) { setPreviews((values) => values.slice(saved)); setError(`已导入 ${saved} 条，剩余内容未保存：${message(e)}`); } finally { setBusy(false); } }}><Download size={14} /> 添加 {previews.length} 条记忆</Button>}<Button disabled={busy} onClick={() => { if (!importText.trim()) { setImporting(false); return; } requestConfirmation({ title: "舍弃未导入的记忆？", description: "已保存的条目会保留，剩余草稿将被丢弃。", confirmLabel: "舍弃草稿", danger: true, action: () => { setImporting(false); setImportText(""); setPreviews([]); } }); }}>关闭导入</Button></div>
    </section>}
    {exportText && <label>导出内容（可复制保存）<textarea className="form-control" rows={8} readOnly value={exportText} onFocus={(e) => e.target.select()} /><Button onClick={() => setExportText("")}>关闭导出</Button></label>}
    <details className="personal-memory__health"><summary>记忆状态与排查</summary><div aria-live="polite"><span>{extractionSummary}</span>
      {!!extraction?.succeededAt && <span>上次完成时间：{date(extraction.succeededAt)}</span>}
      {!!snapshot?.pendingCount && <span>{snapshot.pendingCount} 项内容待整理，{autoProcessing ? "打开工作区后自动继续" : "已暂停自动整理"}；不影响已保存的聊天记录</span>}
      {!!snapshot?.quarantinedCount && <p role="note">已隔离 {snapshot.quarantinedCount} 项损坏的待整理内容，原件已保留在本机，不影响其他内容整理和应用退出。损坏部分未完成提取，可打开原对话重新提取；已保存的聊天记录不受影响。</p>}
      {sessionId && <Button size="compact" disabled={busy || !snapshot?.enabled} onClick={() => void retry()}>立即提取</Button>}
      {extraction?.outcome === "failed" && <details><summary>查看原因</summary><p>{extraction.detail}</p></details>}
    </div></details>
    {!!snapshot?.retrieval.at && <details className="personal-memory__health"><summary>最近一次检索参考了 {snapshot.retrieval.count} 处记忆 · {date(snapshot.retrieval.at)}</summary><ul>{snapshot.retrieval.sources.map((source) => <li key={source}><code>{source}</code></li>)}</ul><span>{snapshot.retrieval.mode === "Hybrid" ? "语义与关键词检索" : "关键词检索（语义检索暂不可用，已有记忆仍可参考）"}</span></details>}
    <details className="personal-memory__legacy" onToggle={(event) => setLegacyOpen(event.currentTarget.open)}><summary>高级：记忆文档与会话摘要</summary>{legacyOpen && <ResourcesPanel cwd={cwd} sessionId={sessionId} onToast={onToast} />}</details>
    {dialog}{closeDialog}
  </section>;
}
