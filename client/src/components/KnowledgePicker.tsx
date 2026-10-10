import { useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";
import { BookOpen, Building2, Check, ChevronDown, FolderOpen } from "lucide-react";
import { agentSetKnowledgeSources } from "@/lib/agent-client";
import { listKbProviders } from "@/lib/knowledge-base";
import { useKnowledgeStore, type KnowledgeSource } from "@/stores/knowledge-store";
import {
  organizationKnowledgeAvailability,
  useOrgSessionStore,
} from "@/stores/org-session-store";

interface InFlightKnowledgeSync {
  latestToken: symbol;
  latestFailed: boolean;
  requestSequence: number;
  confirmedSequence: number;
  confirmedSources: KnowledgeSource[];
  confirmedOrganizationScopeIds: string[];
  pendingRequests: number;
}

// Ownership and the confirmed rollback baseline outlive picker instances.
// Native transactions run in order, but an optimistic selection is not a
// successful transaction. Keep one constant-size record per in-flight session
// and remove it after all its requests settle; no completed history accumulates.
const inFlightSessionSyncs = new Map<string, InFlightKnowledgeSync>();

function restoreConfirmedSelection(sessionId: string, sync: InFlightKnowledgeSync) {
  const state = useKnowledgeStore.getState();
  state.setSessionSources(sessionId, [...sync.confirmedSources]);
  state.setSessionOrganizationScopeIds(sessionId, [...sync.confirmedOrganizationScopeIds]);
}

function sourceLabel(sources: KnowledgeSource[]): string {
  if (sources.length === 0) return "知识来源";
  if (sources.length === 2) return "知识来源 2";
  return sources[0] === "personal" ? "个人知识" : "组织知识";
}

export function KnowledgePicker({
  sessionId,
  disabled = false,
  onManage,
  onOpenOrganization,
  onToast,
}: {
  sessionId?: string;
  disabled?: boolean;
  onManage?: () => void;
  onOpenOrganization?: () => void;
  onToast?: (message: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const syncingRef = useRef(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const syncGenerationRef = useRef(0);
  const floating = useAnchoredFloating(triggerRef, menuRef, open, { width: 332, estimatedHeight: 340 });
  const sourceCount = useKnowledgeStore((state) => state.sourceCount);
  const defaultSources = useKnowledgeStore((state) => state.defaultSources);
  const sessionSources = useKnowledgeStore((state) => sessionId ? state.sessionSources[sessionId] : undefined);
  const defaultOrganizationScopeIds = useKnowledgeStore((state) => state.defaultOrganizationScopeIds);
  const sessionOrganizationScopeIds = useKnowledgeStore((state) => sessionId
    ? state.sessionOrganizationScopeIds[sessionId]
    : undefined);
  const selected = sessionId ? sessionSources ?? [] : defaultSources;
  const organizationScopeIds = sessionId
    ? sessionOrganizationScopeIds ?? []
    : defaultOrganizationScopeIds;
  const providers = useMemo(() => listKbProviders(), [sourceCount]);
  const orgSession = useOrgSessionStore((state) => state.session);
  const orgHydrated = useOrgSessionStore((state) => state.hydrated);
  const organizationAvailability = organizationKnowledgeAvailability({
    session: orgSession,
    hydrated: orgHydrated,
  });
  const organizationAvailable = organizationAvailability.available;
  const organizationReason = organizationAvailability.reason;

  useEffect(() => {
    syncGenerationRef.current += 1;
    syncingRef.current = false;
    setSyncing(false);
    setOpen(false);
    return () => { syncGenerationRef.current += 1; };
  }, [sessionId]);

  useEffect(() => {
    if (!open) return;
    const closeOnOutside = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node) && !menuRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.isComposing && event.keyCode !== 229) {
        event.stopPropagation();
        setOpen(false);
        triggerRef.current?.focus();
      }
    };
    const frame = requestAnimationFrame(() => menuRef.current?.querySelector<HTMLButtonElement>("button:not([disabled])")?.focus());
    document.addEventListener("pointerdown", closeOnOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("pointerdown", closeOnOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  const applySelection = (next: KnowledgeSource[], nextOrganizationScopeIds = organizationScopeIds) => {
    if (syncingRef.current) return;
    const appliedOrganizationScopeIds = next.includes("organization")
      ? nextOrganizationScopeIds
      : [];
    const state = useKnowledgeStore.getState();
    if (sessionId) {
      const previous = [...selected];
      state.setSessionSources(sessionId, next);
      syncingRef.current = true;
      setSyncing(true);
      const generation = ++syncGenerationRef.current;
      const sessionToken = Symbol(sessionId);
      const previousOrganizationScopeIds = [...organizationScopeIds];
      const sync = inFlightSessionSyncs.get(sessionId) ?? {
        latestToken: sessionToken,
        latestFailed: false,
        requestSequence: 0,
        confirmedSequence: 0,
        confirmedSources: previous,
        confirmedOrganizationScopeIds: previousOrganizationScopeIds,
        pendingRequests: 0,
      };
      sync.latestToken = sessionToken;
      sync.latestFailed = false;
      const sequence = ++sync.requestSequence;
      sync.pendingRequests += 1;
      inFlightSessionSyncs.set(sessionId, sync);
      state.setSessionOrganizationScopeIds(sessionId, appliedOrganizationScopeIds);
      void agentSetKnowledgeSources(sessionId, next, appliedOrganizationScopeIds)
        .then(() => {
          if (sequence > sync.confirmedSequence) {
            sync.confirmedSequence = sequence;
            sync.confirmedSources = [...next];
            sync.confirmedOrganizationScopeIds = [...appliedOrganizationScopeIds];
          }
          // A delayed earlier ACK can establish the valid rollback baseline
          // after the latest failure was delivered. Reflect that confirmation.
          if (sync.latestFailed) restoreConfirmedSelection(sessionId, sync);
        })
        .catch((error) => {
          // A task may be revisited and edited while its earlier request is
          // still pending. Only the latest request for that task owns rollback.
          if (sync.latestToken !== sessionToken) return;
          // Native reconciliation is transactional; mirror that behaviour in
          // the picker so the checkmarks always describe the capabilities the
          // Runtime actually owns.
          sync.latestFailed = true;
          restoreConfirmedSelection(sessionId, sync);
          onToast?.(`知识来源同步失败，已恢复上一选择：${String(error).replace(/^Error:\s*/, "")}`);
        })
        .finally(() => {
          sync.pendingRequests -= 1;
          if (sync.pendingRequests === 0 && inFlightSessionSyncs.get(sessionId) === sync) {
            inFlightSessionSyncs.delete(sessionId);
          }
          if (syncGenerationRef.current !== generation) return;
          syncingRef.current = false;
          setSyncing(false);
        });
    } else {
      state.setDefaultSources(next);
      state.setDefaultOrganizationScopeIds(appliedOrganizationScopeIds);
    }
  };

  const toggle = (source: KnowledgeSource) => {
    const active = selected.includes(source);
    if (!active && source === "personal" && sourceCount === 0) {
      setOpen(false);
      onManage?.();
      return;
    }
    if (!active && source === "organization" && !organizationAvailable) {
      return;
    }
    applySelection(active
      ? selected.filter((item) => item !== source)
      : [...selected, source]);
  };

  const personalSelected = selected.includes("personal");
  const organizationSelected = selected.includes("organization");
  const selectedOrganizationScope = organizationScopeIds.length === 1
    ? orgSession?.bootstrap?.scopes.find((scope) => scope.id === organizationScopeIds[0])
    : undefined;
  const organizationScopeInvalid = organizationSelected
    && organizationAvailable
    && organizationScopeIds.length > 0
    && !selectedOrganizationScope;
  const hasUnavailableSelection = (personalSelected && sourceCount === 0)
    || (organizationSelected && !organizationAvailable)
    || organizationScopeInvalid;
  const label = sourceLabel(selected);
  const detail = selected.length === 0
    ? "未选择知识来源，本次任务不会读取个人或组织知识库"
    : `已选择：${[
        personalSelected ? "个人知识库" : null,
        organizationSelected ? "组织知识库" : null,
      ].filter(Boolean).join("、")}`;

  return (
    <div className="knowledge-picker" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className={`knowledge-picker__trigger${selected.length > 0 ? " is-active" : ""}${hasUnavailableSelection ? " is-error" : ""}`}
        onClick={(event) => {
          event.stopPropagation();
          setOpen((value) => !value);
        }}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); setOpen(true); }
        }}
        disabled={disabled}
        aria-busy={syncing}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={`${label}${selected.length === 0 ? "，未选择" : ""}`}
        title={detail}
      >
        <BookOpen size={15} />
        <span>{label}</span>
        <ChevronDown size={13} />
      </button>

      {open && createPortal(
        <div id={menuId} ref={menuRef} className="knowledge-picker__menu" style={floating.style} role="menu" aria-label="选择知识来源" aria-busy={syncing}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing || event.target instanceof HTMLSelectElement) return;
            if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
            const items = [...(menuRef.current?.querySelectorAll<HTMLElement>("button:not([disabled]),select:not([disabled])") ?? [])];
            if (!items.length) return;
            event.preventDefault();
            const current = items.indexOf(document.activeElement as HTMLElement);
            const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : (current + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
            items[next].focus();
          }}>
          <div className="knowledge-picker__heading">知识来源</div>
          <div className="knowledge-picker__hint">
            可多选。未选择时不会读取任何知识库；选择只作用于当前任务。
          </div>

          <button
            type="button"
            role="menuitemcheckbox"
            aria-checked={personalSelected}
            aria-disabled={!personalSelected && sourceCount === 0}
            disabled={syncing}
            className={`knowledge-picker__source-option${personalSelected ? " is-selected" : ""}${sourceCount === 0 ? " is-unavailable" : ""}`}
            onClick={() => toggle("personal")}
          >
            <span className="knowledge-picker__source-icon"><FolderOpen size={16} /></span>
            <span className="knowledge-picker__source-copy">
              <span className="knowledge-picker__source-title">
                <strong>个人知识库</strong>
                <span className="knowledge-picker__availability">
                  {sourceCount > 0 ? `${sourceCount} 个来源` : "未配置"}
                </span>
              </span>
              <small>{sourceCount > 0 ? "来自「更多 → 个人知识库」中的本地文件夹" : "先添加本地文件夹后即可选择"}</small>
            </span>
            <span className="knowledge-picker__checkbox" aria-hidden="true">
              {personalSelected && <Check size={13} />}
            </span>
          </button>

          <button
            type="button"
            role="menuitemcheckbox"
            aria-checked={organizationSelected}
            aria-disabled={!organizationSelected && !organizationAvailable}
            disabled={syncing}
            className={`knowledge-picker__source-option${organizationSelected ? " is-selected" : ""}${!organizationAvailable ? " is-unavailable" : ""}`}
            onClick={() => toggle("organization")}
          >
            <span className="knowledge-picker__source-icon"><Building2 size={16} /></span>
            <span className="knowledge-picker__source-copy">
              <span className="knowledge-picker__source-title">
                <strong>组织知识库</strong>
                <span className={`knowledge-picker__availability${organizationAvailable ? " is-ready" : ""}`}>
                  {organizationAvailable ? "可用" : "不可用"}
                </span>
              </span>
              <small>{organizationReason}</small>
            </span>
            <span className="knowledge-picker__checkbox" aria-hidden="true">
              {organizationSelected && <Check size={13} />}
            </span>
          </button>

          {organizationSelected && organizationAvailable && (
            <label className="knowledge-picker__scope-field">
              <span>组织知识范围</span>
              <select
                aria-label="组织知识范围"
                value={organizationScopeInvalid ? "__invalid__" : organizationScopeIds[0] ?? ""}
                disabled={syncing}
                onChange={(event) => applySelection(
                  selected,
                  event.target.value ? [event.target.value] : [],
                )}
              >
                {organizationScopeInvalid && <option value="__invalid__" disabled>原选范围已失效</option>}
                <option value="">全部有权限的范围</option>
                {orgSession?.bootstrap?.scopes.map((scope) => (
                  <option key={scope.id} value={scope.id}>
                    {scope.kind === "personal" ? "仅自己" : scope.kind === "team" ? "团队" : "全组织"} · {scope.name}
                  </option>
                ))}
              </select>
              <small>
                {selectedOrganizationScope
                  ? `本任务只检索“${selectedOrganizationScope.name}”`
                  : organizationScopeInvalid
                    ? "为避免越界检索，请明确选择新范围"
                    : "默认检索当前账号有权访问的全部组织范围"}
              </small>
            </label>
          )}

          {organizationScopeInvalid && (
            <div className="knowledge-picker__source-warning" role="alert">
              <strong>原组织范围已不可用</strong>
              <span>当前任务不会自动扩大检索范围，请在上方重新选择。</span>
            </div>
          )}

          {organizationSelected && !organizationAvailable && (
            <div className="knowledge-picker__source-warning" role="alert">
              <strong>当前任务仍依赖组织知识</strong>
              <span>{organizationReason}。重新登录，或明确移除组织知识后继续。</span>
              <div>
                {onOpenOrganization && (
                  <button type="button" disabled={syncing} onClick={() => { setOpen(false); onOpenOrganization(); }}>
                    重新登录
                  </button>
                )}
                <button type="button" disabled={syncing} onClick={() => toggle("organization")}>
                  移除组织知识
                </button>
              </div>
            </div>
          )}

          {syncing && (
            <div className="knowledge-picker__connected" role="status" aria-live="polite">
              正在为当前任务同步知识来源…
            </div>
          )}

          {providers.length > 0 && (
            <div className="knowledge-picker__connected" title={providers.map((source) => source.label).join("\n")}>
              个人知识已连接：{providers.map((source) => source.label).join("、")}
            </div>
          )}

          <div className="knowledge-picker__actions">
            {onManage && (
              <button type="button" disabled={syncing} onClick={() => { setOpen(false); onManage(); }}>
                管理个人知识库
              </button>
            )}
            {!organizationAvailable && !organizationSelected && onOpenOrganization && (
              <button type="button" disabled={syncing} onClick={() => { setOpen(false); onOpenOrganization(); }}>
                {orgSession?.loggedIn ? "查看组织连接" : "登录组织"}
              </button>
            )}
          </div>
        </div>
      , document.body)}
    </div>
  );
}
