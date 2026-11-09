import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { Search, X, Clock, FileText } from "lucide-react";
import { useSessionsStore } from "@/stores/sessions-store";
import { useGroupStore } from "@/features/groups/group-store";
import { agentListSessions, sessionSearchPage } from "@/lib/agent-client";
import type { SearchHit, SessionSummary } from "@/lib/types";
import { useModalFocus } from "@/lib/use-modal-focus";
import { TrayActionCancelled } from "@/lib/desktop-tray-connection";

const FULL_TEXT_PAGE_SIZE = 30;
const MAX_AUTO_SEARCH_PAGES = 5;

/**
 * Session search overlay — now powered by EchoAgent's FTS5 full-text index.
 *
 * Two modes:
 *  - Empty / short query: filter the historical session catalog by
 *    title (instant, local).
 *  - Query ≥ 2 chars: fire `echo.agent/session/search` against EchoAgent's full-text
 *    index (cross-workspace, matches message content + titles). Results show
 *    a snippet of the matched content.
 *
 * Selecting an uncached FTS hit hydrates its workspace summary first, so the
 * parent always receives an authoritative cwd/title/model tuple.
 */
export function SearchOverlay({
  open,
  onClose,
  onSelect,
  onSelectGroup,
}: {
  open: boolean;
  onClose: () => void;
  onSelect: (sessionId: string, cwd?: string) => void | Promise<void>;
  onSelectGroup?: (groupId: string) => void | Promise<void>;
}) {
  // The local catalog already spans every historical working directory.
  const independent = useSessionsStore((s) => s.independent);
  const deletedSessionIds = useSessionsStore((s) => s.deletedSessionIds);
  const groups = useGroupStore(state => state.groups);
  // Archive means leaving active work surfaces. Archived content is searched
  // and managed exclusively in Settings > Archived until it is restored.
  const sessions = independent.filter((session) => !session.archived && !session.hidden);
  const [query, setQuery] = useState("");
  const matchingGroups = onSelectGroup ? groups.filter(group => !group.archived
    && (!query.trim() || group.name.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())))
    .sort((a, b) => b.updatedAt - a.updatedAt) : [];
  const [remoteHits, setRemoteHits] = useState<SearchHit[]>([]);
  const [nextRemoteOffset, setNextRemoteOffset] = useState<number | null>(null);
  const [indexBootstrapping, setIndexBootstrapping] = useState(false);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<{ message: string; offset: number } | null>(null);
  const [openingId, setOpeningId] = useState<string | null>(null);
  const [localLimit, setLocalLimit] = useState(30);
  const [groupLimit, setGroupLimit] = useState(30);
  const [failedOpen, setFailedOpen] = useState<{
    message: string;
    target: { kind: "session"; sessionId: string; cwd?: string } | { kind: "group"; groupId: string };
  } | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchGenerationRef = useRef(0);
  const lifecycleGenerationRef = useRef(0);
  const openingRef = useRef(false);
  const focusedResultRef = useRef<string | null>(null);
  const modalRef = useModalFocus<HTMLDivElement>(open, onClose);

  useEffect(() => {
    if (open) {
      const lifecycleGeneration = ++lifecycleGenerationRef.current;
      setQuery("");
      setRemoteHits([]);
      setNextRemoteOffset(null);
      setIndexBootstrapping(false);
      setSearchError(null);
      setSearching(false);
      setOpeningId(null);
      setFailedOpen(null);
      focusedResultRef.current = null;
      return () => {
        searchGenerationRef.current += 1;
        if (lifecycleGenerationRef.current === lifecycleGeneration) {
          lifecycleGenerationRef.current += 1;
        }
        openingRef.current = false;
      };
    }
  }, [open]);

  useLayoutEffect(() => {
    // FTS replaces duplicate title-only rows with snippets. Keep the same
    // task focused when that async merge removes its original DOM button.
    if (!open || openingId !== null || !focusedResultRef.current) return;
    if (document.activeElement !== document.body && document.activeElement?.isConnected) return;
    const replacement = Array.from(modalRef.current?.querySelectorAll<HTMLButtonElement>(".conversation-search-modal__item:not(:disabled)") ?? [])
      .find(button => button.dataset.resultKey === focusedResultRef.current);
    if (replacement) {
      replacement.focus({ preventScroll: true });
      replacement.scrollIntoView?.({ block: "nearest" });
    } else inputRef.current?.focus({ preventScroll: true });
  });

  // Debounced remote search. Only kicks in for queries ≥ 2 chars.
  const runRemoteSearch = useCallback(async (q: string, offset = 0) => {
    const generation = ++searchGenerationRef.current;
    if (q.trim().length < 2) {
      setRemoteHits([]);
      setNextRemoteOffset(null);
      setIndexBootstrapping(false);
      setSearchError(null);
      setSearching(false);
      return;
    }
    setSearching(true);
    setSearchError(null);
    try {
      const hits = new Map<string, SearchHit>();
      let nextOffset: number | null = offset;
      let bootstrapping = false;
      for (let pageIndex = 0; pageIndex < MAX_AUTO_SEARCH_PAGES && nextOffset !== null; pageIndex++) {
        const requestedOffset: number = nextOffset;
        const page = await sessionSearchPage(q.trim(), requestedOffset, FULL_TEXT_PAGE_SIZE);
        if (searchGenerationRef.current !== generation) return;
        for (const hit of page.results) hits.set(hit.sessionId, hit);
        nextOffset = page.nextOffset !== null && page.nextOffset > requestedOffset ? page.nextOffset : null;
        bootstrapping = page.bootstrapping;
        const { independent, deletedSessionIds } = useSessionsStore.getState();
        const unavailable = new Set([
          ...deletedSessionIds,
          ...independent.filter(session => session.archived || session.hidden).map(session => session.sessionId),
        ]);
        if (bootstrapping || page.results.some(hit => !unavailable.has(hit.sessionId))) break;
      }
      setRemoteHits((previous) => {
        if (offset === 0) return [...hits.values()];
        const seen = new Set(previous.map(hit => hit.sessionId));
        return [...previous, ...[...hits.values()].filter(hit => !seen.has(hit.sessionId))];
      });
      setNextRemoteOffset(nextOffset);
      setIndexBootstrapping(bootstrapping);
    } catch (error) {
      if (searchGenerationRef.current === generation) {
        setSearchError({ message: String(error).replace(/^Error:\s*/, ""), offset });
      }
    } finally {
      if (searchGenerationRef.current === generation) setSearching(false);
    }
  }, []);

  useEffect(() => {
    // Invalidate immediately when the query changes, including the debounce
    // interval. A response for the previous text must never become clickable.
    searchGenerationRef.current += 1;
    if (!open) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    setRemoteHits([]);
    setNextRemoteOffset(null);
    setIndexBootstrapping(false);
    setSearchError(null);
    setFailedOpen(null);
    setLocalLimit(30);
    setGroupLimit(30);
    if (query.trim().length < 2) {
      setSearching(false);
      return;
    }
    setSearching(true);
    debounceRef.current = setTimeout(() => runRemoteSearch(query), 250);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      searchGenerationRef.current += 1;
    };
  }, [query, open, runRemoteSearch]);

  // Local title matches (always computed, instant).
  const localMatches: SessionSummary[] = (() => {
    const q = query.trim().toLowerCase();
    return sessions.filter((s) => !q || s.title.toLowerCase().includes(q))
      .sort((a, b) => (Date.parse(b.updatedAt ?? "") || 0) - (Date.parse(a.updatedAt ?? "") || 0));
  })();

  // Prefer the FTS result for duplicate ids because it carries the matching
  // message snippet; keep title-only local rows for ids FTS did not return.
  const unavailableIds = new Set([...deletedSessionIds, ...independent
    .filter((session) => session.archived || session.hidden)
    .map((session) => session.sessionId)]);
  const visibleRemoteHits = remoteHits.filter((hit) => !unavailableIds.has(hit.sessionId));
  const remoteIds = new Set(visibleRemoteHits.map((hit) => hit.sessionId));
  const localOnly = localMatches.filter((session) => !remoteIds.has(session.sessionId));

  const openSession = async (sessionId: string, cwd?: string) => {
    if (openingRef.current) return;
    openingRef.current = true;
    const lifecycleGeneration = lifecycleGenerationRef.current;
    setOpeningId(sessionId);
    setFailedOpen(null);
    try {
      let summary = sessions.find((item) => item.sessionId === sessionId);
      if (!summary) {
        if (!cwd) throw new Error("检索结果缺少工作目录信息，无法打开");
        const hydrated = await agentListSessions(cwd, true);
        if (lifecycleGenerationRef.current !== lifecycleGeneration) return;
        useSessionsStore.getState().mergeSessions(hydrated);
        summary = hydrated.find((item) => item.sessionId === sessionId);
      }
      if (!summary) {
        throw new Error("会话不存在、已归档，或当前无权访问");
      }
      if (summary.archived) {
        throw new Error("该会话已归档，请前往“设置 → 数据管理 → 已归档”恢复");
      }
      if (summary.hidden) {
        throw new Error("该记录属于子代理，请从所属主任务的子代理面板打开");
      }
      await onSelect(summary.sessionId, summary.cwd);
      if (lifecycleGenerationRef.current === lifecycleGeneration) onClose();
    } catch (error) {
      if (lifecycleGenerationRef.current === lifecycleGeneration && !(error instanceof TrayActionCancelled)) {
        setFailedOpen({ message: String(error).replace(/^Error:\s*/, ""), target: { kind: "session", sessionId, cwd } });
      }
    } finally {
      if (lifecycleGenerationRef.current === lifecycleGeneration) {
        openingRef.current = false;
        setOpeningId(null);
      }
    }
  };

  const openGroup = async (groupId: string) => {
    if (openingRef.current || !onSelectGroup) return;
    openingRef.current = true;
    const generation = lifecycleGenerationRef.current;
    setOpeningId(`group:${groupId}`);
    setFailedOpen(null);
    try {
      await onSelectGroup(groupId);
      if (generation === lifecycleGenerationRef.current) onClose();
    } catch (error) {
      if (generation === lifecycleGenerationRef.current && !(error instanceof TrayActionCancelled)) {
        setFailedOpen({ message: String(error).replace(/^Error:\s*/, ""), target: { kind: "group", groupId } });
      }
    } finally {
      if (generation === lifecycleGenerationRef.current) { openingRef.current = false; setOpeningId(null); }
    }
  };

  // Keep native buttons and Tab navigation. Arrow keys make the task switcher
  // usable directly from its input without requiring dozens of Tab presses.
  const navigateResults = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
    const fromInput = event.target === inputRef.current;
    const fromResult = event.target instanceof Element && event.target.matches(".conversation-search-modal__item");
    if (!fromInput && !fromResult) return;
    const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>(".conversation-search-modal__item:not(:disabled)"));
    if (!buttons.length) return;
    if (event.key === "Enter" && fromInput) { event.preventDefault(); buttons[0].click(); }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const index = buttons.indexOf(event.target as HTMLButtonElement);
    const next = fromInput ? (event.key === "ArrowDown" ? 0 : buttons.length - 1)
      : index + (event.key === "ArrowDown" ? 1 : -1);
    if (next < 0 || next >= buttons.length) inputRef.current?.focus();
    else { buttons[next].focus(); buttons[next].scrollIntoView?.({ block: "nearest" }); }
  };

  if (!open) return null;

  return (
    <div
      ref={modalRef}
      className="conversation-search-modal__overlay"
      role="dialog"
      aria-modal="true"
      aria-label="搜索会话"
      tabIndex={-1}
      onKeyDown={navigateResults}
      onFocusCapture={event => {
        const target = event.target;
        focusedResultRef.current = target instanceof HTMLButtonElement && target.classList.contains("conversation-search-modal__item")
          ? target.dataset.resultKey ?? null : null;
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="conversation-search-modal">
        <div className="conversation-search-modal__heading">
          <strong>查找任务</strong><span>跨工作区 · {onSelectGroup ? "会话与专家团" : "会话标题与内容"}</span>
        </div>
        <div className="conversation-search-modal__input-wrapper">
          <Search size={16} strokeWidth={1.75} className="conversation-search-modal__icon" />
          <input
            ref={inputRef}
            data-modal-initial-focus
            className="conversation-search-modal__input"
            placeholder="搜索会话标题或内容…"
            aria-label="搜索会话标题或内容"
            value={query}
            readOnly={openingId !== null}
            onChange={(e) => {
              searchGenerationRef.current += 1;
              setRemoteHits([]);
              setNextRemoteOffset(null);
              setIndexBootstrapping(false);
              setSearchError(null);
              setFailedOpen(null);
              setSearching(e.target.value.trim().length >= 2);
              setQuery(e.target.value);
            }}
          />
          {searching && (
            <span className="conversation-search-modal__spinner" role="status" aria-live="polite">搜索中…</span>
          )}
          <button
            className="conversation-search-modal__close"
            onClick={onClose}
            aria-label="关闭"
            type="button"
          >
            <X size={16} strokeWidth={1.75} />
          </button>
        </div>

        <div className="conversation-search-modal__body">
          {openingId && <div className="conversation-search-modal__count" role="status">正在打开，请稍候…</div>}
          {matchingGroups.length > 0 && <>
            <div className="conversation-search-modal__count">专家团 ({matchingGroups.length})</div>
            <ul className="conversation-search-modal__list">
              {matchingGroups.slice(0, groupLimit).map(group => <li key={group.id}>
                <button type="button" className="conversation-search-modal__item" title={group.name} disabled={openingId !== null}
                  data-result-key={`group:${group.id}`}
                  onClick={() => void openGroup(group.id)}>
                  <Clock size={14} strokeWidth={1.75} className="conversation-search-modal__item-icon" />
                  <span className="conversation-search-modal__item-body"><span className="conversation-search-modal__item-title">{group.name}</span>{" "}
                    <span className="conversation-search-modal__item-cwd" title={group.cwd}>{group.cwd || "默认工作区"}</span></span>
                </button>
              </li>)}
            </ul>
            {matchingGroups.length > groupLimit && <button type="button" className="conversation-search-modal__more" disabled={openingId !== null}
              onClick={() => setGroupLimit(value => value + 30)}>显示更多专家团（还有 {matchingGroups.length - groupLimit} 项）</button>}
          </>}
          {failedOpen && (
            <div className="conversation-search-modal__error" role="alert">
              <div>打开失败：{failedOpen.message}</div>
              <button type="button" className="conversation-search-modal__retry" disabled={openingId !== null} onClick={() => void (failedOpen.target.kind === "group"
                ? openGroup(failedOpen.target.groupId) : openSession(failedOpen.target.sessionId, failedOpen.target.cwd))}>重试打开</button>
            </div>
          )}
          {searchError && (
            <div className="conversation-search-modal__error" role="alert">
              <div>搜索失败：{searchError.message}</div>
              {query.trim().length >= 2 && (
                <button type="button" className="conversation-search-modal__retry" disabled={searching} onClick={() => void runRemoteSearch(query, searchError.offset)}>重试搜索</button>
              )}
            </div>
          )}
          {localOnly.length > 0 && (
            <>
              <div className="conversation-search-modal__count">
                本地会话 ({localOnly.length})
              </div>
              <ul className="conversation-search-modal__list">
                {localOnly.slice(0, localLimit).map((s) => (
                  <li key={s.sessionId}>
                    <button
                      type="button"
                      className="conversation-search-modal__item"
                      data-result-key={`session:${s.sessionId}`}
                      onClick={() => void openSession(s.sessionId, s.cwd)}
                      disabled={openingId !== null}
                      title={s.title}
                    >
                      <Clock
                        size={14}
                        strokeWidth={1.75}
                        className="conversation-search-modal__item-icon"
                      />
                      <span className="conversation-search-modal__item-body">
                        <span className="conversation-search-modal__item-title">{s.title || "未命名任务"}</span>{" "}
                        <span className="conversation-search-modal__item-cwd" title={s.cwd}>{s.cwd || "默认工作区"}</span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
              {localOnly.length > localLimit && <button type="button" className="conversation-search-modal__more" disabled={openingId !== null}
                onClick={() => setLocalLimit(value => value + 30)}>显示更多本地会话（还有 {localOnly.length - localLimit} 项）</button>}
            </>
          )}

          {visibleRemoteHits.length > 0 && (
            <>
              <div className="conversation-search-modal__count">
                全文检索结果 ({visibleRemoteHits.length})
              </div>
              <ul className="conversation-search-modal__list">
                {visibleRemoteHits.map((h) => (
                  <li key={h.sessionId}>
                    <button
                      type="button"
                      className="conversation-search-modal__item conversation-search-modal__item--remote"
                      data-result-key={`session:${h.sessionId}`}
                      onClick={() => void openSession(h.sessionId, h.cwd)}
                      disabled={openingId !== null}
                      title={h.cwd ?? h.sessionId}
                    >
                      <FileText
                        size={14}
                        strokeWidth={1.75}
                        className="conversation-search-modal__item-icon"
                      />
                      <div className="conversation-search-modal__item-body">
                        <div className="conversation-search-modal__item-title">
                          {h.title || h.sessionId.slice(0, 8)}
                        </div>
                        {h.snippet && (
                          <div className="conversation-search-modal__item-snippet">
                            {h.snippet}
                          </div>
                        )}
                        {h.cwd && (
                          <div className="conversation-search-modal__item-cwd">
                            {h.cwd}
                          </div>
                        )}
                      </div>
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}

          {indexBootstrapping && (
            <div className="conversation-search-modal__count" role="status">
              正在建立历史索引，全文结果可能尚未完整。
              <button type="button" className="conversation-search-modal__more" disabled={searching || openingId !== null}
                onClick={() => void runRemoteSearch(query)}>刷新全文结果</button>
            </div>
          )}
          {!indexBootstrapping && nextRemoteOffset !== null && !searchError && (
            <button type="button" className="conversation-search-modal__more" disabled={searching || openingId !== null}
              onClick={() => void runRemoteSearch(query, nextRemoteOffset)}>继续检索历史正文</button>
          )}

          {!searching &&
            !searchError &&
            !failedOpen &&
            !indexBootstrapping &&
            nextRemoteOffset === null &&
            localOnly.length === 0 &&
            matchingGroups.length === 0 &&
            visibleRemoteHits.length === 0 &&
            query.trim().length > 0 && (
              <div className="conversation-search-modal__empty">
                没有匹配的会话
              </div>
            )}
          {!searching &&
            !searchError &&
            !failedOpen &&
            localOnly.length === 0 &&
            matchingGroups.length === 0 &&
            remoteHits.length === 0 &&
            query.trim().length === 0 && (
              <div className="conversation-search-modal__count">
                输入关键词，搜索历史任务的标题和内容。
              </div>
            )}
        </div>
        <div className="conversation-search-modal__footer">↑ ↓ 选择 · Enter 打开 · Esc 关闭<span>已归档记录可在设置中恢复</span></div>
      </div>
    </div>
  );
}
