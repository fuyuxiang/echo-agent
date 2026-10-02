/** Local memory browser/editor backed by the embedded Runtime's canonical storage. */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  BookIcon,
  SearchIcon,
  AddIcon,
  EditToolIcon,
  DeleteIcon,
  RefreshCwIcon,
  SparklesIcon,
} from "@/foundation/components/Icon/icons";
import {
  memoryAppend,
  memoryClearSessionSummaries,
  memoryDelete,
  memoryDream,
  memoryFlush,
  memoryList,
  memoryRewrite,
  memorySave,
} from "@/lib/agent-client";
import type { MemoryEntry } from "@/lib/types";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useAppDialog } from "./AppDialog";

interface ResourcesPanelProps {
  cwd?: string;
  sessionId?: string;
  onToast?: (msg: string) => void;
}

interface EditorState {
  scope: MemoryEntry["scope"];
  path: string;
  content: string;
  revision: string;
  isNew: boolean;
  readOnly: boolean;
}

function errorText(error: unknown): string {
  return String(error).replace(/^Error:\s*/, "");
}

function scopeLabel(scope: MemoryEntry["scope"]): string {
  if (scope === "global") return "全局";
  if (scope === "workspace") return "工作区";
  return "会话摘要";
}

function visibleMemoryContent(entry: MemoryEntry): string {
  if (entry.scope !== "session") return entry.content;
  let content = entry.content.trim();
  // Older runtimes sometimes saved the model's reasoning instead of a summary.
  // Keep those files available for deletion without showing private process text.
  while (/^<(think|thinking|reasoning|analysis)(?:\s[^>]*)?>/i.test(content)) {
    const opening = content.match(/^<(think|thinking|reasoning|analysis)(?:\s[^>]*)?>/i);
    if (!opening) break;
    const closing = new RegExp(`</${opening[1]}\\s*>`, "i").exec(content.slice(opening[0].length));
    if (!closing) return "";
    content = content.slice(opening[0].length + closing.index + closing[0].length).trimStart();
  }
  return content;
}

type MemoryTab = "longTerm" | "summaries";

export function ResourcesPanel({ cwd, sessionId, onToast }: ResourcesPanelProps) {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [activeTab, setActiveTab] = useState<MemoryTab>("longTerm");
  const [editing, setEditing] = useState<EditorState | null>(null);
  const [busy, setBusy] = useState(false);
  const { requestConfirmation, dialog } = useAppDialog(JSON.stringify([cwd ?? null, sessionId ?? null]));
  const reloadGeneration = useRef(0);

  const reload = useCallback(async () => {
    const generation = ++reloadGeneration.current;
    setLoading(true);
    setLoadError(null);
    try {
      const next = await memoryList(cwd);
      if (reloadGeneration.current === generation) setEntries(next);
    } catch (error) {
      if (reloadGeneration.current !== generation) return;
      const message = errorText(error);
      setLoadError(message);
      onToast?.(`加载个人记忆失败：${message}`);
    } finally {
      if (reloadGeneration.current === generation) setLoading(false);
    }
  }, [cwd, onToast]);

  useEffect(() => {
    void reload();
    return () => {
      reloadGeneration.current += 1;
    };
  }, [reload]);

  useEffect(() => {
    if (!cwd) setActiveTab("longTerm");
  }, [cwd]);

  const handleSave = useCallback(async (draft: EditorState) => {
    if (!draft.content.trim()) {
      onToast?.("记忆内容不能为空");
      return;
    }
    if (draft.scope === "session") {
      onToast?.("会话摘要由 Agent 自动生成，只能查看或删除");
      return;
    }
    setBusy(true);
    try {
      if (draft.isNew) {
        await memoryAppend(draft.scope, draft.content, cwd);
        onToast?.("已追加记忆");
      } else {
        await memorySave(
          draft.scope,
          draft.path,
          draft.content,
          cwd,
          draft.revision,
        );
        onToast?.("已保存记忆");
      }
      setEditing(null);
      await reload();
    } catch (error) {
      const message = errorText(error);
      if (message.startsWith("记忆文件已保存，但检索索引更新失败")) {
        setEditing(null);
        await reload();
        onToast?.(message);
      } else {
        onToast?.(`保存失败：${message}`);
      }
    } finally {
      setBusy(false);
    }
  }, [cwd, onToast, reload]);

  const handleDelete = useCallback((entry: MemoryEntry) => {
    const isSummary = entry.scope === "session";
    requestConfirmation({
      title: isSummary ? "删除这条会话摘要？" : `删除${scopeLabel(entry.scope)}记忆文件？`,
      description: isSummary
        ? `将永久删除“${entry.path}”及其检索索引。原会话和已整理进长期记忆的内容不会随之删除。`
        : `将永久删除“${entry.path}”中的全部长期记忆，此操作无法撤销。`,
      confirmLabel: isSummary ? "删除摘要" : "删除记忆",
      danger: true,
      action: async () => {
        setBusy(true);
        try {
          await memoryDelete(entry.scope, entry.path, cwd, entry.revision);
          setEntries((current) => current.filter((candidate) => !(
            candidate.scope === entry.scope && candidate.path === entry.path
          )));
          onToast?.(isSummary ? "已删除会话摘要及检索索引" : "已删除记忆文件");
          await reload();
        } finally {
          setBusy(false);
        }
      },
      onError: (error) => onToast?.(`删除失败：${errorText(error)}`),
    });
  }, [cwd, onToast, reload, requestConfirmation]);

  const handleClearSummaries = useCallback(() => {
    if (!cwd) {
      onToast?.("清空会话摘要需要当前工作区");
      return;
    }
    requestConfirmation({
      title: "清空当前工作区的会话摘要？",
      description: "将删除所有自动摘要、已整理归档和对应检索索引。原会话和长期记忆不受影响。",
      confirmLabel: "清空摘要",
      danger: true,
      action: async () => {
        setBusy(true);
        try {
          const removed = await memoryClearSessionSummaries(cwd);
          setEntries((current) => current.filter((entry) => entry.scope !== "session"));
          onToast?.(removed > 0 ? `已清除 ${removed} 个会话摘要文件` : "没有需要清除的会话摘要");
          await reload();
        } finally {
          setBusy(false);
        }
      },
      onError: (error) => onToast?.(`清空失败：${errorText(error)}`),
    });
  }, [cwd, onToast, reload, requestConfirmation]);

  const handleFlush = useCallback(async () => {
    if (!sessionId) {
      onToast?.("立即落盘需要一个已打开的会话");
      return;
    }
    setBusy(true);
    try {
      const completed = await memoryFlush(sessionId);
      onToast?.(completed ? "提取已执行；若有可复用内容，可在会话摘要中查看" : "本次提取未完成，请稍后重试");
      await reload();
    } catch (error) {
      onToast?.(`落盘失败：${errorText(error)}`);
    } finally {
      setBusy(false);
    }
  }, [onToast, reload, sessionId]);

  const handleDream = useCallback(() => {
    if (!sessionId) {
      onToast?.("整理记忆需要一个已打开的会话");
      return;
    }
    requestConfirmation({
      title: "整理历史会话摘要？",
      description: "Agent 会把历史会话摘要归纳到长期记忆中，可能会修改现有记忆内容。",
      confirmLabel: "开始整理",
      action: async () => {
        setBusy(true);
        try {
          const changed = await memoryDream(sessionId);
          onToast?.(changed ? "长期记忆已更新" : "本次没有更新长期记忆；可能没有待整理的摘要");
        } finally {
          await reload();
          setBusy(false);
        }
      },
      onError: (error) => onToast?.(`整理失败：${errorText(error)}`),
    });
  }, [onToast, reload, requestConfirmation, sessionId]);

  const handleRewriteDraft = useCallback(async (draft: EditorState) => {
    if (!sessionId) {
      onToast?.("AI 整理需要一个已打开的会话");
      return;
    }
    setBusy(true);
    try {
      const rewritten = await memoryRewrite(
        sessionId,
        draft.content,
        `${scopeLabel(draft.scope)}记忆 ${draft.path}`,
      );
      setEditing((current) => current ? { ...current, content: rewritten } : current);
      onToast?.("已生成整理结果，请检查后保存");
    } catch (error) {
      onToast?.(`AI 整理失败：${errorText(error)}`);
    } finally {
      setBusy(false);
    }
  }, [onToast, sessionId]);

  const normalizedQuery = query.trim().toLowerCase();
  const visibleEntries = entries.filter((entry) => activeTab === "summaries"
    ? entry.scope === "session"
    : entry.scope !== "session");
  const filtered = visibleEntries.filter((entry) => (
    !normalizedQuery
    || entry.path.toLowerCase().includes(normalizedQuery)
    || visibleMemoryContent(entry).toLowerCase().includes(normalizedQuery)
  ));
  const globalCount = entries.filter((entry) => entry.scope === "global").length;
  const workspaceCount = entries.filter((entry) => entry.scope === "workspace").length;
  const sessionCount = entries.filter((entry) => entry.scope === "session").length;

  return (
    <div className="resources-panel">
      <div className="resources-panel__header">
        <div>
          <h2 className="resources-panel__title">个人记忆</h2>
          <p className="resources-panel__subtitle">
            {activeTab === "longTerm"
              ? "管理跨会话复用的全局与工作区记忆文档"
              : "管理 Agent 从会话中自动提取的待整理摘要"}
          </p>
          <div className="resources-panel__context" aria-label="当前记忆上下文">
            <span className="resources-panel__context-label">{cwd ? "当前工作区" : "全局记忆"}</span>
            {cwd && <code className="resources-panel__context-path" title={cwd}>{cwd}</code>}
            <span className={sessionId
              ? "resources-panel__session-state resources-panel__session-state--connected"
              : "resources-panel__session-state"}
            >
              {sessionId ? "已连接当前会话" : "未连接会话，仅支持查看和手动编辑"}
            </span>
          </div>
        </div>
      </div>

      <div className="resources-panel__toolbar">
        <div className="resources-panel__tabs" role="tablist" aria-label="记忆类型">
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === "longTerm"}
            className={`resources-panel__tab${activeTab === "longTerm" ? " resources-panel__tab--active" : ""}`}
            onClick={() => {
              setActiveTab("longTerm");
              setQuery("");
            }}
          >
            长期记忆文档 <span>{globalCount + workspaceCount}</span>
          </button>
          {cwd && (
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === "summaries"}
              className={`resources-panel__tab${activeTab === "summaries" ? " resources-panel__tab--active" : ""}`}
              onClick={() => {
                setActiveTab("summaries");
                setQuery("");
              }}
            >
              会话摘要 <span>{sessionCount}</span>
            </button>
          )}
        </div>
        <div className="resources-panel__header-actions">
          {activeTab === "summaries" && sessionId && (
            <>
              <button className="resources-panel__action-btn" onClick={handleFlush} disabled={busy} title="提取当前会话中的可复用信息并写入会话摘要">
                立即提取
              </button>
              <button className="resources-panel__action-btn" onClick={handleDream} disabled={busy} title="把历史会话摘要归纳为长期记忆">
                <SparklesIcon size="sm" /> 整理到长期记忆
              </button>
            </>
          )}
          {activeTab === "summaries" && sessionCount > 0 && (
            <button className="resources-panel__action-btn resources-panel__action-btn--danger" onClick={handleClearSummaries} disabled={busy}>
              清空摘要
            </button>
          )}
          <button className="resources-panel__action-btn" onClick={() => void reload()} disabled={loading} title="刷新">
            <RefreshCwIcon size="sm" /> 刷新
          </button>
        </div>
      </div>

      {activeTab === "summaries" && (
        <div className="resources-panel__notice" role="note">
          <strong>这里不是完整聊天记录。</strong>
          <span>Agent 会自动提取可复用信息，用于后续检索和整理；可单独删除，删除原会话时也会自动清理。</span>
        </div>
      )}

      <div className="resources-panel__list-controls">
        <div className="resources-panel__search">
          <SearchIcon size="md" className="resources-panel__search-icon" />
          <input className="resources-panel__search-input" aria-label="搜索记忆" placeholder={activeTab === "summaries" ? "搜索会话摘要…" : "搜索长期记忆…"} value={query} onChange={(event) => setQuery(event.target.value)} />
        </div>
        <div className="resources-panel__stats">
          {activeTab === "summaries" ? (
            <span>{sessionCount} 条自动摘要</span>
          ) : (
            <>
              <span>全局文档 {globalCount}</span>
              {cwd && <span>· 工作区文档 {workspaceCount}</span>}
            </>
          )}
        </div>
      </div>

      {loadError && (
        <div className="panel-inline-error" role="alert">
          <span>个人记忆加载失败：{loadError}</span>
          <button type="button" onClick={() => void reload()} disabled={loading}>
            {loading ? "重试中…" : "重试"}
          </button>
        </div>
      )}

      {activeTab === "longTerm" && (
        <button
          className="resources-panel__create-btn"
          onClick={() => setEditing({
            scope: cwd ? "workspace" : "global",
            path: "MEMORY.md",
            content: "",
            revision: "",
            isNew: true,
            readOnly: false,
          })}
        >
          <AddIcon size="sm" /> 追加记忆到文档
        </button>
      )}

      <div className="resources-panel__list">
        {!loading && !loadError && filtered.length === 0 && (
          <div className="resources-panel__empty" role="status">
            <BookIcon size="xl" color="var(--echo-text-tertiary)" />
            {normalizedQuery ? (
              <>
                <p>没有找到匹配的记忆，请尝试其他关键词。</p>
                <button type="button" className="btn btn--ghost" onClick={() => setQuery("")}>清除搜索</button>
              </>
            ) : activeTab === "summaries" ? (
              <p>暂无会话摘要。Agent 只会在会话中出现值得跨会话复用的信息时生成摘要。</p>
            ) : (
              <p>
                暂无长期记忆文档。可手动添加，或在对话中使用 <code>/remember</code> 保存。
              </p>
            )}
          </div>
        )}
        {filtered.map((entry) => {
          const visibleContent = visibleMemoryContent(entry);
          return <div key={`${entry.scope}/${entry.path}`} className="resources-panel__item">
            <div className="resources-panel__item-icon"><BookIcon size="md" /></div>
            <div className="resources-panel__item-content">
              <div className="resources-panel__item-name">
                <span className="resources-panel__item-path" title={entry.path}>{entry.path}</span>
                <span className="resources-panel__item-scope">{scopeLabel(entry.scope)}</span>
              </div>
              <pre className="resources-panel__item-preview">
                {visibleContent
                  ? `${visibleContent.slice(0, 200)}${visibleContent.length > 200 ? "…" : ""}`
                  : "旧版摘要没有可展示的内容，可删除这条无效摘要。"}
              </pre>
            </div>
            <div className="resources-panel__item-actions">
              <button
                className="resources-panel__icon-btn"
                onClick={() => setEditing({ ...entry, content: visibleContent || "旧版摘要没有可展示的内容。", isNew: false })}
                title={entry.readOnly ? "查看" : "编辑"}
              >
                <EditToolIcon size="sm" />
              </button>
              <button className="resources-panel__icon-btn resources-panel__icon-btn--danger" onClick={() => void handleDelete(entry)} title={entry.scope === "session" ? "删除摘要" : "删除"}>
                <DeleteIcon size="sm" />
              </button>
            </div>
          </div>;
        })}
        {loading && <div className="resources-panel__empty">加载中…</div>}
      </div>

      {editing && (
        <MemoryEditor
          draft={editing}
          cwd={cwd}
          canRewrite={Boolean(sessionId) && !editing.readOnly}
          busy={busy}
          onChange={setEditing}
          onCancel={() => setEditing(null)}
          onRewrite={() => void handleRewriteDraft(editing)}
          onSave={() => void handleSave(editing)}
        />
      )}
      {dialog}
    </div>
  );
}

function MemoryEditor({
  draft,
  cwd,
  canRewrite,
  busy,
  onChange,
  onCancel,
  onRewrite,
  onSave,
}: {
  draft: EditorState;
  cwd?: string;
  canRewrite: boolean;
  busy: boolean;
  onChange: (draft: EditorState) => void;
  onCancel: () => void;
  onRewrite: () => void;
  onSave: () => void;
}) {
  const dialogRef = useModalFocus<HTMLDivElement>(true, onCancel);
  const dialogLabel = draft.isNew ? "追加记忆到文档" : `${draft.readOnly ? "查看" : "编辑"} ${draft.path}`;

  return (
    <div className="modal-overlay memory-editor__overlay" onClick={onCancel}>
      <div ref={dialogRef} className="memory-editor" onClick={(event) => event.stopPropagation()} role="dialog" aria-modal="true" aria-label={dialogLabel} tabIndex={-1}>
        <div className="memory-editor__header">
          <h3>{draft.isNew ? "追加记忆到文档" : `${draft.readOnly ? "查看" : "编辑"} ${draft.path}`}</h3>
          <button className="memory-editor__close" onClick={onCancel} aria-label="关闭">✕</button>
        </div>
        <div className="memory-editor__meta">
          <label>
            范围
            <select
              value={draft.scope}
              disabled={!draft.isNew}
              onChange={(event) => onChange({ ...draft, scope: event.target.value as EditorState["scope"] })}
            >
              <option value="global">全局记忆</option>
              {cwd && <option value="workspace">当前工作区</option>}
              {draft.scope === "session" && <option value="session">会话摘要（只读）</option>}
            </select>
          </label>
          <label>
            文件
            <input type="text" value={draft.path} disabled />
          </label>
        </div>
        <textarea
          className="memory-editor__content"
          aria-label={draft.isNew ? "记忆内容" : `记忆内容 ${draft.path}`}
          value={draft.content}
          readOnly={draft.readOnly}
          onChange={(event) => onChange({ ...draft, content: event.target.value })}
          placeholder={draft.isNew ? "例如：代码示例优先使用 TypeScript，并说明关键设计取舍。" : undefined}
          spellCheck={false}
          data-modal-initial-focus
        />
        <div className="memory-editor__footer">
          <button className="btn btn--ghost" onClick={onCancel}>{draft.readOnly ? "关闭" : "取消"}</button>
          {!draft.readOnly && (
            <>
              <button className="btn btn--ghost" disabled={busy || !canRewrite || !draft.content.trim()} onClick={onRewrite} title={canRewrite ? "使用当前会话模型整理这份文本" : "需要一个已打开的会话"}>
                {busy ? "处理中…" : "AI 整理"}
              </button>
              <button className="btn btn--primary" disabled={busy || !draft.content.trim()} onClick={onSave}>
                {busy ? "保存中…" : draft.isNew ? "添加" : "保存"}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
