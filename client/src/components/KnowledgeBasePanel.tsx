/**
 * 知识库面板 —— 对齐 EchoAgent `knowledge-base-panel`(可插拔知识源)。
 *
 * EchoAgent 的知识源 provider-agnostic(本地文件夹/网盘/文档库均可注册,见
 * lib/knowledge-base)。本面板提供搜索框 + 跨源结果列表。无 provider 时显示空态。
 */
import { useEffect, useState } from "react";
import { PageToolbar } from "./PageToolbar";
import { FolderOpen, Info, Search } from "lucide-react";
import {
  searchKbWithDiagnostics,
  listKbProvidersWithStats,
  rebuildAllKbProvidersWithDiagnostics,
  type KbEntry,
  type KbIndexStats,
} from "@/lib/knowledge-base";
import { isTauriAvailable } from "@/lib/tauri-kb-reader";
import { addLocalKnowledgeSource, hydrateKnowledgeSources, removeKnowledgeSource } from "@/lib/kb-source-storage";
import { filesystemPickDirectory } from "@/lib/agent-client";
import {
  cancelPersonalKnowledgeSearch,
  createPersonalKnowledgeSearchRequestId,
  getPersonalKnowledgeIndexStatus,
  rebuildPersonalKnowledgeIndex,
  searchPersonalKnowledge,
  type PersonalKnowledgeIndexStatus,
} from "@/lib/personal-knowledge";

interface KnowledgeBasePanelProps {
  /** 打开条目回调(可选)。 */
  onOpen?: (entryId: string, url?: string) => void;
  /** 临时反馈(可选)。 */
  onToast?: (msg: string) => void;
}

export function KnowledgeBasePanel({ onOpen, onToast }: KnowledgeBasePanelProps) {
  const [query, setQuery] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [results, setResults] = useState<KbEntry[]>([]);
  const [searching, setSearching] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);
  const [sources, setSources] = useState<Array<{ id: string; label: string; stats: KbIndexStats }>>([]);
  const [sourcesHydrated, setSourcesHydrated] = useState(false);
  const [sourcesLoading, setSourcesLoading] = useState(true);
  const [sourcesError, setSourcesError] = useState<string | null>(null);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [searchNotice, setSearchNotice] = useState<string | null>(null);
  const [semanticStatus, setSemanticStatus] = useState<PersonalKnowledgeIndexStatus | null>(null);
  const [searchRetryKey, setSearchRetryKey] = useState(0);
  const [sourceChangeKey, setSourceChangeKey] = useState(0);
  const [rebuildError, setRebuildError] = useState<string | null>(null);
  const q = debouncedQuery.trim();
  const currentQuery = query.trim();
  useEffect(() => {
    let cancelled = false;
    void hydrateKnowledgeSources()
      .then(() => { if (!cancelled) setSourcesHydrated(true); })
      .catch((error) => {
        if (cancelled) return;
        const message = String(error).replace(/^Error:\s*/, "");
        setSourcesError(message);
        setSourcesLoading(false);
        onToast?.(`读取知识源失败：${message}`);
      });
    return () => { cancelled = true; };
  }, [onToast]);

  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedQuery(query), 300);
    return () => window.clearTimeout(timer);
  }, [query]);
  // 每当 refreshKey 变化(添加/移除/重建/搜索)重新拉取含索引状态的源列表。
  useEffect(() => {
    if (!sourcesHydrated) return;
    let cancelled = false;
    setSourcesLoading(true);
    void listKbProvidersWithStats()
      .then((s) => {
        if (!cancelled) {
          setSources(s);
          setSourcesError(null);
        }
      })
      .catch((error) => {
        if (!cancelled) {
          const message = String(error).replace(/^Error:\s*/, "");
          setSourcesError(message);
          onToast?.(`读取知识源失败：${message}`);
        }
      })
      .finally(() => {
        if (!cancelled) setSourcesLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [refreshKey, onToast, sourcesHydrated]);

  useEffect(() => {
    let cancelled = false;
    const refreshStatus = () => void getPersonalKnowledgeIndexStatus()
      .then((status) => {
        if (!cancelled && status) setSemanticStatus(status);
      })
      .catch(() => {});
    refreshStatus();
    const timer = semanticStatus?.state === "indexing"
      ? window.setInterval(refreshStatus, 1_000)
      : undefined;
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearInterval(timer);
    };
  }, [refreshKey, semanticStatus?.state]);

  /** 添加本地文件夹知识源:弹出原生目录选择 → 注册 local KbProvider。 */
  const addLocalFolder = async () => {
    if (!isTauriAvailable()) {
      onToast?.("请在 EchoAgent 桌面应用中添加本地文件夹");
      return;
    }
    try {
      const dir = await filesystemPickDirectory();
      if (!dir) return;
      const { added } = await addLocalKnowledgeSource(dir);
      setSourceChangeKey((key) => key + 1);
      setRefreshKey((k) => k + 1);
      if (!added) onToast?.("这个文件夹已经添加过了");
    } catch (e) {
      onToast?.(`添加失败：${String(e).replace(/^Error:\s*/, "")}`);
    }
  };

  /** 移除一个已注册知识源(by id)。 */
  const removeSource = async (id: string) => {
    try {
      if (await removeKnowledgeSource(id)) {
        if (sources.length === 1) setQuery("");
        setSources((current) => current.filter((source) => source.id !== id));
        setResults([]);
        setSourceChangeKey((key) => key + 1);
        setRefreshKey((k) => k + 1);
      }
    } catch (error) {
      onToast?.(`移除失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };

  /** 重建索引:刷新所有知识源缓存(本地文件夹内容变化后手动重新扫描)。 */
  const [rebuilding, setRebuilding] = useState(false);
  const rebuildIndex = async () => {
    setRebuilding(true);
    setRebuildError(null);
    try {
      const { rebuilt: res, failures } = await rebuildAllKbProvidersWithDiagnostics();
      const semantic = await rebuildPersonalKnowledgeIndex();
      if (semantic) setSemanticStatus(semantic);
      setRefreshKey((k) => k + 1);
      setSourceChangeKey((key) => key + 1);
      if (failures.length > 0) {
        const message = `部分知识源索引刷新失败：${failures.map((failure) => `${failure.label}：${failure.message}`).join("；")}。请重试。`;
        setRebuildError(message);
        onToast?.(message);
        return;
      }
      onToast?.(semantic || res.length > 0 ? "知识库已更新，可以搜索最新内容" : "暂无可更新的知识源");
    } catch (e) {
      const message = `重建失败：${String(e).replace(/^Error:\s*/, "")}`;
      setRebuildError(message);
      onToast?.(message);
    } finally {
      setRebuilding(false);
    }
  };

  useEffect(() => {
    // Cancel and hide old results on the keystroke, including the debounce
    // interval; an older document must not remain actionable under new text.
    if (!q || q !== currentQuery) {
      setResults([]);
      setSearchError(null);
      setSearchNotice(null);
      setSearching(Boolean(currentQuery));
      return;
    }
    let cancelled = false;
    let settled = false;
    const requestId = createPersonalKnowledgeSearchRequestId();
    setSearching(true);
    setSearchError(null);
    setSearchNotice(null);
    void (async () => {
      const semantic = await searchPersonalKnowledge(q, 20, requestId);
      if (semantic) {
        return {
          entries: semantic.items,
          failures: [],
          successfulProviders: 1,
          notice: semantic.degradedReason
            ? "部分搜索能力暂时不可用，结果可能不完整；请稍后重试。"
            : null,
          semanticStatus: semantic.index,
        };
      }
      return { ...(await searchKbWithDiagnostics(q)), notice: null, semanticStatus: null };
    })()
      .then(({ entries, failures, successfulProviders, notice, semanticStatus: nextStatus }) => {
        if (!cancelled) {
          setResults(entries);
          setSearchNotice(notice);
          if (nextStatus) setSemanticStatus(nextStatus);
          // The first search lazily builds local indexes. Refresh the displayed
          // coverage so “waiting for scan” immediately becomes a real count.
          setRefreshKey((key) => key + 1);
          if (failures.length === 0) {
            setSearchError(null);
          } else {
            const details = failures
              .map((failure) => `${failure.label}：${failure.message}`)
              .join("；");
            setSearchError(
              successfulProviders > 0
                ? entries.length > 0
                  ? `部分知识源搜索失败（${details}），已显示其他知识源的结果。`
                  : `部分知识源搜索失败（${details}），其他知识源未找到匹配结果。`
                : `所有知识源搜索失败（${details}）。`,
            );
          }
        }
      })
      .catch((error) => {
        if (!cancelled) {
          setResults([]);
          setSearchError(String(error).replace(/^Error:\s*/, ""));
        }
      })
      .finally(() => {
        settled = true;
        if (!cancelled) setSearching(false);
      });
    return () => {
      cancelled = true;
      if (!settled) {
        void cancelPersonalKnowledgeSearch(requestId).catch(() => {});
      }
    };
  }, [q, currentQuery, searchRetryKey, sourceChangeKey]);

  const retrySources = () => {
    setSourcesError(null);
    setSourcesLoading(true);
    if (sourcesHydrated) {
      setRefreshKey((key) => key + 1);
      return;
    }
    void hydrateKnowledgeSources()
      .then(() => setSourcesHydrated(true))
      .catch((error) => {
        setSourcesError(String(error).replace(/^Error:\s*/, ""));
        setSourcesLoading(false);
      });
  };

  return (
    <div className="kb-panel" role="region" aria-label="知识库">
      <PageToolbar className="kb-panel__head">
        <div className="kb-panel__heading">
          <h1 className="kb-panel__title">知识库</h1>
          <p>管理知识源并搜索其中的资料</p>
        </div>
        {sources.length > 0 && <div className="kb-panel__head-right">
          <span className="kb-panel__sources">{sources.length} 个知识源</span>
          {isTauriAvailable() && <button
            type="button"
            className="kb-panel__add-btn"
            onClick={() => void addLocalFolder()}
            title="添加本地文件夹作为知识源"
          >
            + 添加本地文件夹
          </button>}
          <button
            type="button"
            className="kb-panel__refresh-btn"
            onClick={() => void rebuildIndex()}
            disabled={rebuilding}
            title="重新扫描所有知识源(文件夹内容变化后刷新)"
          >
            {rebuilding ? "重建中…" : "↻ 刷新索引"}
          </button>
        </div>}
      </PageToolbar>
      {sourcesError && (
        <div className="kb-panel__index-status kb-panel__load-error" role="alert">
          <span>知识源读取失败：{sourcesError}</span>
          <button type="button" className="kb-panel__refresh-btn" onClick={retrySources}>
            重试
          </button>
        </div>
      )}
      {rebuildError && <div className="kb-panel__index-status kb-panel__load-error" role="alert">
        <span>{rebuildError}</span>
        <button type="button" className="kb-panel__refresh-btn" onClick={() => void rebuildIndex()} disabled={rebuilding}>重试刷新索引</button>
      </div>}
      {!sourcesError && sourcesLoading && sources.length === 0 && (
        <div className="kb-panel__loading" role="status">正在读取知识源…</div>
      )}
      {!sourcesError && !sourcesLoading && sources.length === 0 && (
        <div className="kb-panel__empty-state">
          <div className="kb-panel__empty-icon"><FolderOpen size={30} strokeWidth={1.7} /></div>
          <h2>还没有添加知识源</h2>
          <p>选择一个存放资料的本地文件夹，添加后即可在这里搜索内容，并在任务中引用来源。</p>
          {isTauriAvailable() ? (
            <button type="button" className="kb-panel__add-btn" onClick={() => void addLocalFolder()}>
              + 添加本地文件夹
            </button>
          ) : (
            <span className="kb-panel__desktop-hint">请在桌面应用中添加本地文件夹</span>
          )}
          <span className="kb-panel__privacy-hint">文件保留在原目录；建立语义索引时，文本片段会发送到模型服务。</span>
        </div>
      )}
      {sources.length > 0 && <div className="kb-panel__privacy" role="note">
        <Info size={16} strokeWidth={1.8} aria-hidden="true" />
        <span>原文件保留在所选目录；文本片段会发送到语义模型服务生成向量并排序，索引保存在本机。任务回答会标注来源。</span>
      </div>}
      {/* 已注册知识源 chip 列表(每个可移除)。 */}
      {sources.length > 0 && (
        <div className="kb-panel__sources-row">
          {sources.map((s) => (
            <span key={s.id} className="kb-panel__source-chip" title={s.label}>
              <span className="kb-panel__source-chip-label">{s.label}</span>
              <button
                type="button"
                className="kb-panel__source-chip-remove"
                onClick={() => void removeSource(s.id)}
                aria-label={`移除知识源 ${s.label}`}
                title="移除此知识源"
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
      {/* 索引状态指示:已索引文件数 + 最近重建时间(让用户直观感知覆盖范围)。 */}
      {sources.length > 0 && (
        <div className="kb-panel__index-status" aria-live="polite">
          <span className="kb-panel__index-count">
            {sources.some((source) => typeof source.stats.fileCount === "number")
              ? `已索引 ${sources.reduce((sum, source) => sum + (source.stats.fileCount ?? 0), 0)} 个文件`
              : "等待首次扫描，可直接搜索或刷新索引"}
          </span>
          {(() => {
            const ts = sources
              .map((s) => s.stats.lastRebuiltAt)
              .filter((t): t is number => typeof t === "number")
              .sort((a, b) => b - a)[0];
            return ts ? (
              <span className="kb-panel__index-time">最近更新 {formatRelativeTime(ts)}</span>
            ) : null;
          })()}
        </div>
      )}
      {sources.length > 0 && semanticStatus && !["ready", "idle"].includes(semanticStatus.state) && (
        <div
          className="kb-panel__index-status"
          role={semanticStatus.state === "error" ? "alert" : "status"}
        >
          <span>
            {semanticStatus.state === "indexing"
              ? "正在更新知识库索引…"
              : semanticStatus.state === "error"
                ? "知识库索引暂时不可用，请刷新索引后重试。"
                : "知识库搜索部分可用，结果可能不完整。"}
          </span>
        </div>
      )}
      {sources.length > 0 && <div className="kb-panel__search">
        <Search size={18} strokeWidth={1.8} aria-hidden="true" />
        <input
          className="kb-panel__input"
          type="text"
          value={query}
          placeholder="搜索知识库…"
          onChange={(e) => setQuery(e.target.value)}
          aria-label="搜索知识库"
        />
      </div>}
      {sources.length > 0 && currentQuery && (
        <>
          {searchError && !searching && (
            <div className="kb-panel__index-status" role="alert">
              <span>{searchError}</span>
              <button
                type="button"
                className="kb-panel__refresh-btn"
                onClick={() => setSearchRetryKey((key) => key + 1)}
              >
                重试搜索
              </button>
            </div>
          )}
          {searchNotice && !searching && (
            <div className="kb-panel__index-status" role="status">
              <span>{searchNotice}</span>
            </div>
          )}
          <ul className="kb-panel__list" aria-label="知识库搜索结果" aria-busy={searching}>
            {searching ? (
              <li className="kb-panel__empty" role="status">搜索中…</li>
            ) : results.length === 0 && !searchError ? (
              <li className="kb-panel__empty">未找到相关内容，请尝试其他关键词</li>
            ) : (
              results.map((e) => (
                <li key={`${e.source}:${e.id}`}>
                  <button
                    type="button"
                    className="kb-panel__row"
                    onClick={() => onOpen?.(e.id, e.url)}
                    title={e.url ?? e.title}
                    aria-label={`打开知识条目：${e.title}`}
                  >
                    <span className="kb-panel__row-source">{e.source}</span>
                    <span className="kb-panel__row-title">{e.title}</span>
                    {e.snippet && <span className="kb-panel__row-snippet">{e.snippet}</span>}
                  </button>
                </li>
              ))
            )}
          </ul>
        </>
      )}
    </div>
  );
}

/** 把时间戳(ms)格式化为相对时间(如「刚刚 / 3 分钟前 / 2 小时前 / 昨天」)。 */
function formatRelativeTime(ts: number): string {
  const diff = Date.now() - ts;
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return "刚刚";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} 分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} 小时前`;
  const day = Math.floor(hr / 24);
  if (day === 1) return "昨天";
  if (day < 30) return `${day} 天前`;
  return new Date(ts).toLocaleDateString();
}
