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
 */
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Hand, ShieldCheck, ShieldAlert } from "lucide-react";
import {
  ChevronDownIcon,
  CheckIcon,
} from "@/foundation/components/Icon/icons";
import { permissionModeGet, permissionModeSet } from "@/lib/agent-client";
import type { PermissionMode } from "@/lib/agent-client";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";
import {
  permissionModeStatusFromEvent,
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

export function PermissionPicker({
  onToast,
  triggerLabel,
  sessionId,
}: {
  onToast?: (msg: string) => void;
  /** 覆盖触发按钮文字（如本地助理页固定显示「默认权限」）；缺省显示当前模式名。 */
  triggerLabel?: string;
  /** Existing task scope. Omit on Home for the one pending new-task draft. */
  sessionId?: string;
}) {
  const [open, setOpen] = useState(false);
  const [confirmingAlways, setConfirmingAlways] = useState(false);
  const homeMode = usePermissionModeStore((state) => state.homeMode);
  const status = usePermissionModeStore((state) => sessionId
    ? state.statuses[sessionId] ?? null
    : state.capabilityStatus);
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
  const requestRef = useRef(0);
  const activeSessionRef = useRef(sessionId);
  activeSessionRef.current = sessionId;
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
    : sessionId
      ? runtimeMode ?? catalogMode ?? "ask"
      : homeMode;

  useEffect(() => {
    const request = ++requestRef.current;
    permissionModeGet(sessionId)
      .then((status) => {
        if (requestRef.current !== request) return;
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
      .catch(() => {
        /* 读不到就用默认 ask */
      });
    return () => {
      if (requestRef.current === request) requestRef.current += 1;
    };
  }, [sessionId, setHomeMode, setStatus]);

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
    setBusy(false);
  }, [sessionId, closePicker]);

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
  }, [open]);

  const applySelection = useCallback(
    async (next: PermissionMode) => {
      const desiredMode = sessionId
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
      if (sessionId) requestRef.current += 1;
      setBusy(true);
      try {
        if (!sessionId) {
          setHomeMode(next);
          closePicker(true);
          return;
        }
        const result = await permissionModeSet(sessionId, next);
        const normalizedStatus = permissionModeStatusFromEvent(result);
        if (normalizedStatus) setStatus(normalizedStatus);
        useSessionsStore.getState().upsert({
          sessionId,
          permissionMode: result.permissionMode ?? next,
        });
        if (activeSessionRef.current !== sessionId) return;
        closePicker(true);
        const remaining = result?.remainingPending ?? 0;
        if (remaining > 0) onToast?.(`仍有 ${remaining} 项操作等待你确认`);
      } catch (e) {
        if (activeSessionRef.current === sessionId) {
          onToast?.(`权限模式切换失败：${String(e).replace(/^Error:\s*/, "")}`);
        }
      } finally {
        if (activeSessionRef.current === sessionId) setBusy(false);
      }
    },
    [closePicker, mode, onToast, sessionId, setHomeMode, setStatus, status],
  );

  const select = useCallback((next: PermissionMode) => {
    if (next === "always-approve" && mode !== "always-approve") {
      setConfirmingAlways(true);
      return;
    }
    void applySelection(next);
  }, [applySelection, mode]);

  const current = MODES.find((m) => m.id === mode) ?? MODES[0];
  const autoUnavailable = status?.autoModeAvailable === false;
  const alwaysUnavailable = status?.alwaysApproveAvailable === false;
  const runtimeUnavailable = Boolean(sessionId) && status?.runtimeSyncState === "offline";
  const syncWarning = status?.runtimeSyncError ??
    (status?.runtimeSyncState === "syncing"
      ? "权限模式正在同步"
      : runtimeUnavailable
        ? "当前任务正在加载，权限模式暂不可切换"
        : null);
  const modeDescription = (item: (typeof MODES)[number]) => {
    if (item.id === "auto" && autoUnavailable) {
      return status?.autoModeUnavailableReason ?? "自动判断当前不可用";
    }
    if (item.id === "always-approve" && alwaysUnavailable) {
      return status?.alwaysApproveUnavailableReason ?? "本任务免审批当前不可用";
    }
    return item.desc;
  };
  const modeDisabled = (item: (typeof MODES)[number]) =>
    busy ||
    runtimeUnavailable ||
    (item.id === "auto" && autoUnavailable) ||
    (item.id === "always-approve" && alwaysUnavailable) ||
    Boolean(status?.locked);

  return (
    <div className="permission-picker" ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className="echo-composer-meta__btn permission-picker__trigger"
        data-mode-id={current.id}
        onClick={() => {
          if (open) closePicker(false);
          else setOpen(true);
        }}
        onKeyDown={(event) => {
          if (["ArrowDown", "ArrowUp"].includes(event.key)) { event.preventDefault(); setOpen(true); }
        }}
        aria-label={triggerLabel ?? current.label}
        title={`权限模式 · ${current.label} · ${modeDescription(current)}${syncWarning ? ` · ${syncWarning}` : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? popoverId : undefined}
      >
        <current.icon className="permission-picker__trigger-icon" size={14} aria-hidden="true" />
        <span className="permission-picker__label">{triggerLabel ?? current.label}</span>
        <ChevronDownIcon size="sm" />
      </button>
      {open && typeof document !== "undefined" && createPortal(
        <div
          ref={popoverRef}
          id={popoverId}
          className="permission-picker__popover permission-picker__popover--modes"
          role="menu"
          aria-label="本任务权限"
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
                  {m.label}
                  {((m.id === "auto" && autoUnavailable) ||
                    (m.id === "always-approve" && alwaysUnavailable)) ? "（不可用）" : ""}
                </span>
                <span className="permission-picker__mode-desc">{modeDescription(m)}</span>
                </span>
                {m.id === mode && <CheckIcon size="sm" className="permission-picker__mode-check" />}
              </button>
            ))}
          </div>
          <div className="permission-picker__scope">仅限当前任务</div>
          {confirmingAlways && (
            <div
              className="permission-picker__confirm"
              role="alertdialog"
              aria-label="确认本任务免审批"
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  cancelAlwaysConfirmation();
                }
              }}
            >
              <strong>允许本任务免审批？</strong>
              <span>后续命令和文件修改将自动批准。网页与电脑操作仍遵守各自的授权规则，现有请求仍需单独确认。</span>
              <div className="permission-picker__confirm-actions">
                <button ref={confirmCancelRef} type="button" className="btn btn--ghost" disabled={busy} onClick={cancelAlwaysConfirmation}>取消</button>
                <button type="button" className="btn btn--danger" disabled={busy} onClick={() => void applySelection("always-approve")}>
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
