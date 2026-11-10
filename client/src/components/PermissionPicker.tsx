/**
 * 任务级权限模式选择器 - Composer meta 行的下拉
 *
 * EchoAgent 任务级权限的三档选择:
 *  - ask            请求批准:需要授权的操作交给用户确认
 *  - auto           自动判断:分类器评估风险并批准安全操作
 *  - always-approve 本任务免审批:常规工具自动批准；电脑执行仍需明确任务请求
 *
 * 首页选择只属于即将创建的任务；已有会话通过 sessionId
 * 定向同步，绝不影响其他任务。
 * 专家团通过 groupId 管理整个协作范围，独立于首页和成员单个会话。
 */
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Hand, ShieldCheck, ShieldAlert } from "lucide-react";
import {
  ChevronDownIcon,
  CheckIcon,
} from "@/foundation/components/Icon/icons";
import { permissionModeGet, permissionModeSet } from "@/lib/agent-client";
import type { PermissionMode, PermissionModeStatus } from "@/lib/agent-client";
import { groupPermissionModeGet, groupPermissionModeSet } from "@/features/groups/group-client";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";
import {
  permissionModeStatusFromEvent,
  projectPermissionUnavailableReason,
  usePermissionModeStore,
} from "@/stores/permission-mode-store";
import { useSessionsStore } from "@/stores/sessions-store";

const MODES = [
  {
    id: "ask",
    label: "请求批准",
    desc: "需要授权的操作由你确认",
    icon: Hand,
  },
  {
    id: "auto",
    label: "自动判断",
    desc: "低风险自动批准，必要时询问",
    icon: ShieldCheck,
  },
  {
    id: "always-approve",
    label: "本任务免审批",
    desc: "常规工具操作不再逐次询问",
    icon: ShieldAlert,
  },
] as const;

export function permissionModeLabel(mode: PermissionMode): string {
  return (MODES.find(item => item.id === mode) ?? MODES[0]).label;
}

export interface ProjectDraftPermission {
  mode?: PermissionMode;
  onChange: (mode: PermissionMode) => void;
}

export function PermissionPicker({
  onToast,
  triggerLabel,
  sessionId,
  compact = false,
  groupId,
  groupPermissionMode,
  disabled = false,
  inheritTaskDefault = false,
  projectDraftPermission,
  showLockedAsText = false,
}: {
  onToast?: (msg: string) => void;
  /** 覆盖触发按钮文字（如本地助理页固定显示「默认权限」）；缺省显示当前模式名。 */
  triggerLabel?: string;
  /** Existing task scope. Omit on Home for the one pending new-task draft. */
  sessionId?: string;
  /** Icon trigger for narrow editor composers; the current mode stays in its accessible name and tooltip. */
  compact?: boolean;
  /** A persistent collaboration scope, independent of Home and single tasks. */
  groupId?: string;
  groupPermissionMode?: PermissionMode;
  disabled?: boolean;
  /** Use the Runtime's new-task default without reading or changing the Home draft. */
  inheritTaskDefault?: boolean;
  /** Editable selection for the next task in the current project. */
  projectDraftPermission?: ProjectDraftPermission;
  /** Expose non-editable modes with a reason instead of mode choices. */
  showLockedAsText?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [readOnlyOpen, setReadOnlyOpen] = useState(false);
  const [confirmingAlways, setConfirmingAlways] = useState(false);
  const homeMode = usePermissionModeStore((state) => state.homeMode);
  const taskStatus = usePermissionModeStore((state) => sessionId
    ? state.statuses[sessionId] ?? null
    : state.capabilityStatus);
  const readsTaskDefault = inheritTaskDefault || projectDraftPermission !== undefined;
  const projectPermissionRead = usePermissionModeStore((state) => readsTaskDefault ? state.projectPermissionRead : null);
  const refreshProjectPermission = usePermissionModeStore((state) => state.refreshProjectPermission);
  const [groupStatus, setGroupStatus] = useState<PermissionModeStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const status = groupId ? groupStatus : taskStatus;
  // Managed policy/capability broadcasts also apply to already-open groups.
  const groupCapabilityRefresh = groupId ? taskStatus : null;
  const scope = groupId ? "本专家团" : "本任务";
  const scopeKey = groupId ? `group:${groupId}` : sessionId;
  const setHomeMode = usePermissionModeStore((state) => state.setHomeMode);
  const setStatus = usePermissionModeStore((state) => state.setStatus);
  const catalogMode = useSessionsStore((state) => sessionId
    ? state.independent.find((entry) => entry.sessionId === sessionId)?.permissionMode
    : undefined);
  const [busy, setBusy] = useState(false);
  const reactId = useId();
  const popoverId = `permission-picker-${reactId.replace(/:/g, "")}`;
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const alwaysOptionRef = useRef<HTMLButtonElement>(null);
  const confirmCancelRef = useRef<HTMLButtonElement>(null);
  const readOnlyTriggerRef = useRef<HTMLButtonElement>(null);
  const projectPreviewRef = useRef<HTMLDivElement>(null);
  const restoreProjectFocus = useRef(false);
  const requestRef = useRef(0);
  const activeScopeRef = useRef(scopeKey);
  activeScopeRef.current = scopeKey;
  const mountedRef = useRef(true);
  useEffect(() => { mountedRef.current = true; return () => { mountedRef.current = false; }; }, []);
  const { style: popoverStyle, placement } = useAnchoredFloating(
    triggerRef,
    popoverRef,
    open,
    {
      preferredPlacement: "top",
      align: "start",
      width: "content",
      maxWidth: 320,
      estimatedHeight: confirmingAlways ? 350 : 220,
      maxHeight: confirmingAlways ? 440 : 320,
    },
  );

  const runtimeMode = status &&
      (status.runtimeSyncState === "failed" || status.runtimeSyncState === "syncing") &&
      status.runtimeAppliedMode
    ? status.runtimeAppliedMode
    : status?.permissionMode;
  const mode: PermissionMode = status?.locked
    ? status.permissionMode
    : groupId
      ? runtimeMode ?? groupPermissionMode ?? "ask"
    : sessionId
      ? runtimeMode ?? catalogMode ?? "ask"
      : readsTaskDefault ? projectDraftPermission?.mode ?? status?.permissionMode ?? "ask" : homeMode;

  useEffect(() => {
    if (readsTaskDefault) {
      void refreshProjectPermission();
      return;
    }
    const request = ++requestRef.current;
    if (groupId) { setGroupStatus(null); setError(null); }
    (groupId ? groupPermissionModeGet(groupId) : permissionModeGet(sessionId))
      .then((status) => {
        if (requestRef.current !== request) return;
        if (groupId) {
          const normalized = permissionModeStatusFromEvent(status);
          if (!normalized) throw new Error("专家团权限状态无效，请重试");
          setGroupStatus(normalized); return;
        }
        setStatus(status);
        if (sessionId) {
          useSessionsStore.getState().upsert({
            sessionId,
            permissionMode: status.permissionMode,
          });
        } else {
          const selected = usePermissionModeStore.getState().homeMode;
          if (
            (selected === "auto" && !status.autoModeAvailable) ||
            (selected === "always-approve" && !status.alwaysApproveAvailable)
          ) {
            setHomeMode("ask");
          }
        }
      })
      .catch((error) => {
        if (groupId && requestRef.current === request) setError(`读取权限失败：${String(error).replace(/^Error:\s*/, "")}`);
        /* 读不到就用默认 ask */
      });
    return () => {
      if (requestRef.current === request) requestRef.current += 1;
    };
  }, [groupId, groupPermissionMode, groupCapabilityRefresh, sessionId, readsTaskDefault, setHomeMode, setStatus, refresh, refreshProjectPermission]);

  const closePicker = useCallback((restoreFocus = false) => {
    setOpen(false);
    setConfirmingAlways(false);
    if (restoreFocus) {
      window.requestAnimationFrame(() => triggerRef.current?.focus());
    }
  }, []);

  const cancelAlwaysConfirmation = useCallback(() => {
    setConfirmingAlways(false);
    requestAnimationFrame(() => alwaysOptionRef.current?.focus());
  }, []);

  useEffect(() => {
    closePicker(false);
    setReadOnlyOpen(false);
    setBusy(false);
    setError(null);
  }, [scopeKey, closePicker]);

  // A portalled popover is not a DOM child of the trigger, so both trees
  // participate in outside-click and Escape handling.
  useEffect(() => {
    if (!open) return;
    const onMouseDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (rootRef.current?.contains(target) || popoverRef.current?.contains(target)) return;
      closePicker(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      if (confirmingAlways) cancelAlwaysConfirmation();
      else closePicker(true);
    };
    document.addEventListener("mousedown", onMouseDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [cancelAlwaysConfirmation, closePicker, confirmingAlways, open]);

  useEffect(() => {
    if (confirmingAlways) confirmCancelRef.current?.focus();
  }, [confirmingAlways]);

  useEffect(() => {
    if (!open) return;
    const frame = requestAnimationFrame(() => {
      // The confirmation can open before this frame runs. Its Cancel button
      // already owns focus, so the initial menu focus must not override it.
      if (confirmCancelRef.current) return;
      const menu = popoverRef.current;
      (menu?.querySelector<HTMLButtonElement>('[aria-checked="true"]:not(:disabled)')
        ?? menu?.querySelector<HTMLButtonElement>('[role="menuitemradio"]:not(:disabled)'))?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [open, groupStatus]);

  const applySelection = useCallback(
    async (next: PermissionMode) => {
      const desiredMode = sessionId || groupId
        ? status?.configuredPermissionMode ?? mode
        : mode;
      if (
        next === desiredMode &&
        status?.runtimeSyncState !== "failed" &&
        status?.runtimeSyncState !== "syncing"
      ) {
        closePicker(true);
        return;
      }
      // An earlier capability read must not overwrite the result of the
      // user's newer write when it eventually resolves.
      if (sessionId || groupId) requestRef.current += 1;
      setBusy(true);
      setError(null);
      try {
        if (!sessionId && !groupId) {
          if (projectDraftPermission) projectDraftPermission.onChange(next);
          else setHomeMode(next);
          closePicker(true);
          return;
        }
        const result = groupId ? await groupPermissionModeSet(groupId, next) : await permissionModeSet(sessionId!, next);
        const normalizedStatus = permissionModeStatusFromEvent(result);
        if (normalizedStatus && !groupId) setStatus(normalizedStatus);
        if (!groupId) useSessionsStore.getState().upsert({
          sessionId: sessionId!,
          permissionMode: result.permissionMode ?? next,
        });
        if (!mountedRef.current || activeScopeRef.current !== scopeKey) return;
        if (normalizedStatus && groupId) setGroupStatus(normalizedStatus);
        closePicker(true);
        const remaining = result?.remainingPending ?? 0;
        if (remaining > 0) onToast?.(`仍有 ${remaining} 项操作等待你确认`);
        else if (groupId) onToast?.("已更新本专家团权限，协调员与全部成员共用此设置");
      } catch (e) {
        if (mountedRef.current && activeScopeRef.current === scopeKey) {
          const message = `权限模式切换失败：${String(e).replace(/^Error:\s*/, "")}`;
          setError(message);
          onToast?.(message);
        }
      } finally {
        if (mountedRef.current && activeScopeRef.current === scopeKey) setBusy(false);
      }
    },
    [closePicker, mode, onToast, sessionId, groupId, scopeKey, projectDraftPermission, setHomeMode, setStatus, status],
  );

  const select = useCallback((next: PermissionMode) => {
    if (next === "always-approve" && mode !== "always-approve") {
      setConfirmingAlways(true);
      return;
    }
    void applySelection(next);
  }, [applySelection, mode]);

  const current = MODES.find((m) => m.id === mode) ?? MODES[0];
  const modeLabel = (item: (typeof MODES)[number]) => item.id === "always-approve" ? `${scope}免审批` : item.label;
  const autoUnavailable = status?.autoModeAvailable === false;
  const alwaysUnavailable = status?.alwaysApproveAvailable === false;
  const selectedModeUnavailable = projectDraftPermission &&
    ((mode === "auto" && autoUnavailable) || (mode === "always-approve" && alwaysUnavailable));
  const currentLabel = modeLabel(current) + (selectedModeUnavailable ? "（不可用）" : "");
  const runtimeUnavailable = Boolean(sessionId) && status?.runtimeSyncState === "offline";
  const syncWarning = status?.runtimeSyncError ??
    (status?.runtimeSyncState === "syncing"
      ? "权限模式正在同步"
      : runtimeUnavailable
        ? "当前任务正在加载，权限模式暂不可切换"
        : null);
  const readOnlyReason = showLockedAsText && status?.locked
    ? status.lockedReason || "权限模式由组织策略锁定"
    : showLockedAsText && runtimeUnavailable ? syncWarning : null;
  useEffect(() => {
    if (readOnlyReason && open) closePicker(false);
  }, [closePicker, open, readOnlyReason]);
  useEffect(() => {
    if (!restoreProjectFocus.current || !projectPermissionRead || projectPermissionRead.phase === "loading") return;
    restoreProjectFocus.current = false;
    if (projectPermissionRead.phase !== "ready") return;
    const active = document.activeElement;
    if (active === projectPreviewRef.current || (projectDraftPermission && active === document.body)) {
      (readOnlyTriggerRef.current ?? triggerRef.current)?.focus({ preventScroll: true });
    }
  }, [projectPermissionRead, projectDraftPermission]);

  const modeValue = <>
    <current.icon className="permission-picker__trigger-icon" size={14} aria-hidden="true" />
    <span>{modeLabel(current)}</span>
  </>;

  const readOnlyValue = readOnlyReason && (
    <div className="permission-picker__read-only">
      <button type="button" className="echo-composer-meta__btn permission-picker__value" data-mode-id={current.id}
        ref={readOnlyTriggerRef} aria-expanded={readOnlyOpen} aria-controls={`${popoverId}-reason`}
        onClick={() => setReadOnlyOpen(value => !value)}>
        {modeValue}
        <span className="permission-picker__readonly-label">{status?.locked ? "组织锁定" : "暂不可切换"}</span>
        <ChevronDownIcon size="sm" aria-hidden="true" />
      </button>
      <p id={`${popoverId}-reason`} hidden={!readOnlyOpen}>{readOnlyReason}{status?.locked && runtimeUnavailable && syncWarning ? `。${syncWarning}` : ""}</p>
    </div>
  );

  const failed = projectPermissionRead?.phase === "failed";
  const waiting = !projectPermissionRead || projectPermissionRead.phase === "loading";
  const unavailableReason = readsTaskDefault && status && projectPermissionUnavailableReason(status);
  if (readsTaskDefault && (inheritTaskDefault || waiting || failed || unavailableReason || !status)) {
    return (
      <div className="permission-picker__preview" ref={projectPreviewRef} tabIndex={-1} role="group" aria-label="任务默认权限">
        {waiting || failed || unavailableReason || !status ? <>
          <span className="echo-composer-meta__value" role="status">
            {failed ? `未能读取任务默认权限：${projectPermissionRead.message}`
              : waiting ? "正在读取任务默认权限…"
                : unavailableReason ?? "未能读取任务默认权限"}
          </span>
          {!waiting && <button type="button" className="permission-picker__retry echo-composer-meta__btn" onClick={() => {
            restoreProjectFocus.current = true;
            projectPreviewRef.current?.focus({ preventScroll: true });
            void refreshProjectPermission();
          }}>重试读取权限</button>}
        </> : readOnlyValue || (
          <span className="echo-composer-meta__value permission-picker__value" data-mode-id={current.id} role="status">
            {modeValue}
            <span className="permission-picker__readonly-label">任务默认权限</span>
          </span>
        )}
      </div>
    );
  }

  if (readOnlyValue) return readOnlyValue;

  const modeDescription = (item: (typeof MODES)[number]) => {
    if (item.id === "auto" && autoUnavailable) {
      return status?.autoModeUnavailableReason ?? "自动判断当前不可用";
    }
    if (item.id === "always-approve" && alwaysUnavailable) {
      return status?.alwaysApproveUnavailableReason ?? `${scope}免审批当前不可用`;
    }
    return item.id === "auto" && groupId ? "低风险自动批准，风险操作交给你确认" : item.desc;
  };
  const modeDisabled = (item: (typeof MODES)[number]) =>
    busy || disabled || Boolean(groupId && !groupStatus) ||
    runtimeUnavailable ||
    (item.id === "auto" && autoUnavailable) ||
    (item.id === "always-approve" && alwaysUnavailable) ||
    Boolean(status?.locked);

  return (
    <div className={`permission-picker${compact ? " permission-picker--compact" : ""}`} ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        disabled={disabled}
        className="echo-composer-meta__btn permission-picker__trigger"
        data-mode-id={current.id}
        onClick={() => {
          if (open) closePicker(false);
          else { if (groupId && groupStatus) setRefresh(value => value + 1); setOpen(true); }
        }}
        onKeyDown={(event) => {
          if (["ArrowDown", "ArrowUp"].includes(event.key)) { event.preventDefault(); if (!open && groupId && groupStatus) setRefresh(value => value + 1); setOpen(true); }
        }}
        aria-label={triggerLabel ?? (groupId ? `专家团权限：${currentLabel}` : currentLabel)}
        title={`权限模式 · ${modeLabel(current)} · ${modeDescription(current)}${error || syncWarning ? ` · ${error ?? syncWarning}` : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? popoverId : undefined}
      >
        <current.icon className="permission-picker__trigger-icon" size={14} aria-hidden="true" />
        {!compact && <span className="permission-picker__label">{triggerLabel ?? currentLabel}</span>}
        {!compact && <ChevronDownIcon size="sm" />}
      </button>
      {open && typeof document !== "undefined" && createPortal(
        <div
          ref={popoverRef}
          id={popoverId}
          className="permission-picker__popover permission-picker__popover--modes"
          role="menu"
          aria-busy={busy}
          aria-label={`${scope}权限`}
          style={popoverStyle}
          data-placement={placement ?? undefined}
          onClick={(event) => event.stopPropagation()}
          onKeyDown={(event) => {
            if (confirmingAlways || !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
            event.preventDefault();
            const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]:not(:disabled)'));
            const index = items.indexOf(document.activeElement as HTMLButtonElement);
            const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
              : (index + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
            items[next]?.focus();
          }}
        >
          {error && <div className="permission-picker__status" role="alert">{error}{groupId && !groupStatus && <button type="button" className="groups-text-button" onClick={() => setRefresh(value => value + 1)}>重试读取权限</button>}</div>}
          {groupId && !groupStatus && !error && <div className="permission-picker__status" role="status">正在读取专家团权限…</div>}
          {groupId && busy && <div className="permission-picker__status" role="status">正在同步协调员与成员权限…</div>}
          {(syncWarning || status?.lockedReason) && (
            <div className="permission-picker__status" role="status">
              {syncWarning ?? status?.lockedReason}
              {status?.runtimeSyncState === "failed" ? "，选择目标模式可重试" : ""}
            </div>
          )}
          <div className="permission-picker__modes">
            {MODES.map((m) => (
              <button
                key={m.id}
                ref={m.id === "always-approve" ? alwaysOptionRef : undefined}
                type="button"
                data-mode-id={m.id}
                className={
                  "permission-picker__mode" +
                  (m.id === mode ? " permission-picker__mode--active" : "")
                }
                onClick={() => select(m.id)}
                disabled={modeDisabled(m)}
                role="menuitemradio"
                aria-checked={m.id === mode}
              >
                <m.icon className="permission-picker__mode-icon" size={19} aria-hidden="true" />
                <span className="permission-picker__mode-copy">
                <span className="permission-picker__mode-label">
                  {modeLabel(m)}
                  {((m.id === "auto" && autoUnavailable) ||
                    (m.id === "always-approve" && alwaysUnavailable)) ? "（不可用）" : ""}
                </span>
                <span className="permission-picker__mode-desc">{modeDescription(m)}</span>
                </span>
                {m.id === mode && <CheckIcon size="sm" className="permission-picker__mode-check" />}
              </button>
            ))}
          </div>
          <div className="permission-picker__scope">{groupId ? "仅限当前专家团，协调员与全部成员共用。只读讨论始终保持只读。" : projectDraftPermission ? "仅用于从此输入框发送的下一条任务。工作项使用任务默认权限。" : "仅限当前任务"}</div>
          {confirmingAlways && (
            <div
              className="permission-picker__confirm"
              role="alertdialog"
              aria-label={`确认${scope}免审批`}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  cancelAlwaysConfirmation();
                }
              }}
            >
              <strong>{projectDraftPermission ? "允许此草稿发送后的任务免审批？" : `允许${scope}免审批？`}</strong>
              <span>{groupId ? "协调员与全部成员后续的命令和文件修改将自动批准，设置会保留到你再次修改。只读讨论、组织策略及网页与电脑操作的授权限制仍生效。现有请求仍需单独确认。" : "后续命令和文件修改将自动批准。网页与电脑操作仍遵守各自的授权规则，现有请求仍需单独确认。"}</span>
              <div className="permission-picker__confirm-actions">
                <button ref={confirmCancelRef} type="button" className="btn btn--ghost" disabled={busy} onClick={cancelAlwaysConfirmation}>取消</button>
                <button type="button" className="btn btn--danger" disabled={modeDisabled(MODES[2])} onClick={() => void applySelection("always-approve")}>
                  {busy ? "正在切换…" : "确认开启"}
                </button>
              </div>
            </div>
          )}
        </div>,
        document.body,
      )}
    </div>
  );
}
