import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, Check, Folder, FolderOpen } from "lucide-react";
import { filesystemPickDirectory, type WorkspaceInfo } from "@/lib/agent-client";
import { displayPath, shortWorkspacePath } from "@/lib/display-path";
import { attachmentBasename } from "@/lib/user-message";
import {
  useAnchoredFloating,
  type FloatingAlignment,
  type FloatingPlacement,
} from "@/lib/use-anchored-floating";

/**
 * Working-directory picker for the Composer.
 * Lists every cwd EchoAgent has seen (from list_workspaces). Selecting one
 * calls onSelectWorkspace — switching cwd means the next new session will
 * be bound to it (ACP locks cwd per-session, so we don't migrate the
 * current session).
 */
export function WorkspacePicker({
  cwd,
  workspaces,
  onSelectWorkspace,
  disabled = false,
  automaticWorkspace = false,
  defaultWorkspaceCwd,
  projectDefault,
  workspaceErrorId,
  onBusyChange,
  menuPlacement = "top",
  menuAlign = "start",
}: {
  /** Currently active cwd (highlighted in the list). */
  cwd?: string;
  workspaces: WorkspaceInfo[];
  onSelectWorkspace: (cwd: string, trigger?: HTMLButtonElement | null) => void;
  disabled?: boolean;
  automaticWorkspace?: boolean;
  defaultWorkspaceCwd?: string;
  projectDefault?: { cwd?: string };
  workspaceErrorId?: string;
  onBusyChange?: (busy: boolean) => void;
  /** Prefer below the trigger in top utility bars and above it in composers. */
  menuPlacement?: FloatingPlacement;
  menuAlign?: FloatingAlignment;
}) {
  const [open, setOpen] = useState(false);
  const [pickError, setPickError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  const pickingRef = useRef(false);
  const reactId = useId();
  const menuId = `workspace-picker-${reactId.replace(/:/g, "")}`;
  const ref = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLUListElement>(null);
  const { style: menuStyle, placement } = useAnchoredFloating(
    triggerRef,
    menuRef,
    open,
    {
      preferredPlacement: menuPlacement,
      align: menuAlign,
      width: "content",
      maxWidth: 320,
      estimatedHeight: Math.min(320, 52 + workspaces.length * 40),
      offset: 6,
    },
  );

  const close = useCallback((restoreFocus = true) => {
    setOpen(false);
    if (restoreFocus) triggerRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!open) return;
    // A pointer click can refocus the trigger after React commits the menu.
    // Move focus once the browser has finished that default action.
    const frame = window.requestAnimationFrame(() => {
      const selected = menuRef.current?.querySelector<HTMLElement>('[role="menuitemradio"][aria-checked="true"]');
      (selected ?? menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]'))?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node | null;
      if (!target) return;
      if (ref.current?.contains(target) || menuRef.current?.contains(target)) return;
      close(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [close, open]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [close, open]);

  useEffect(() => { if (disabled) close(false); }, [disabled, close]);

  const handleMenuKeyDown = (event: React.KeyboardEvent<HTMLUListElement>) => {
    const items = Array.from(
      menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not(:disabled), [role="menuitemradio"]:not(:disabled)') ?? [],
    );
    const index = items.indexOf(document.activeElement as HTMLElement);
    const focusAt = (nextIndex: number) => {
      if (items.length === 0) return;
      items[(nextIndex + items.length) % items.length]?.focus();
    };
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusAt(index + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusAt(index - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusAt(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusAt(items.length - 1);
    }
  };

  // Shorten a cwd for display: show last 2 path segments.
  const shortName = shortWorkspacePath;

  const currentProjectDirectory = projectDefault && !automaticWorkspace && cwd !== projectDefault.cwd
    ? cwd : undefined;
  const recentWorkspaces = projectDefault
    ? workspaces.filter(workspace => workspace.cwd !== projectDefault.cwd && workspace.cwd !== currentProjectDirectory)
    : defaultWorkspaceCwd
    ? workspaces.filter(w => !isAutomaticTaskWorkspace(defaultWorkspaceCwd, w.cwd))
    : workspaces;
  const triggerLabel = automaticWorkspace ? "默认任务目录"
    : cwd ? (projectDefault ? attachmentBasename(displayPath(cwd)) : shortName(cwd)) : "选择工作目录";
  const usingDefault = projectDefault?.cwd ? cwd === projectDefault.cwd : automaticWorkspace;
  const defaultLabel = projectDefault?.cwd ? "项目默认目录" : "默认任务目录";
  const select = (selected: string, restoreFocus = true) => {
    close(restoreFocus);
    if (projectDefault) onSelectWorkspace(selected, triggerRef.current);
    else onSelectWorkspace(selected);
  };

  // 打开系统目录选择框，切换到任意文件夹（不限于历史目录）。
  const pickFolder = async () => {
    if (pickingRef.current || disabled) return;
    pickingRef.current = true;
    onBusyChange?.(true);
    setPicking(true);
    setPickError(null);
    try {
      const selected = await filesystemPickDirectory(cwd || projectDefault?.cwd || defaultWorkspaceCwd);
      if (!selected || !triggerRef.current) return;
      select(selected, false);
    } catch (error) {
      setPickError(`无法选择文件夹：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      pickingRef.current = false;
      setPicking(false);
      onBusyChange?.(false);
      window.requestAnimationFrame(() => {
        // Keep focus with a confirmation opened by the selection callback.
        const active = document.activeElement;
        const menu = menuRef.current;
        if (active === document.body || active === triggerRef.current || menu?.contains(active)) {
          (menu?.querySelector<HTMLButtonElement>("button:not(:disabled)") ?? triggerRef.current)?.focus();
        }
      });
    }
  };

  return (
    <div className="workspace-picker" ref={ref}>
      <button
        className="workspace-picker__trigger"
        onClick={(event) => {
          event.stopPropagation();
          setOpen((value) => !value);
        }}
        type="button"
        disabled={disabled || picking}
        aria-label={automaticWorkspace ? "工作目录：默认任务目录。创建时自动分配独立文件夹" : cwd ? `工作目录：${displayPath(cwd)}。点击切换` : "选择工作目录"}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-describedby={workspaceErrorId}
        aria-invalid={workspaceErrorId ? true : undefined}
        data-tip={automaticWorkspace ? "每个新任务使用独立文件夹；也可在创建前选择现有目录" : cwd ? `当前工作目录：${displayPath(cwd)}` : "选择工作目录"}
        ref={triggerRef}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <Folder size={14} strokeWidth={1.75} style={{ marginRight: 6 }} />
        <span className="workspace-picker__label">{triggerLabel}</span>
        <ChevronDown size={14} strokeWidth={1.75} className="workspace-picker__arrow" />
      </button>
      {open && !disabled && typeof document !== "undefined" && createPortal(
        <ul
          className="workspace-picker__menu"
          id={menuId}
          role="menu"
          aria-label="选择工作目录"
          ref={menuRef}
          style={menuStyle}
          data-placement={placement ?? undefined}
          data-global-tooltip-skip-focus=""
          onClick={(event) => event.stopPropagation()}
          onKeyDown={handleMenuKeyDown}
        >
          <li role="none">
            <button
              type="button"
              className="workspace-picker__item workspace-picker__item--browse"
              onClick={pickFolder}
              role="menuitem"
              disabled={picking}
            >
              <FolderOpen size={14} />
              <span className="workspace-picker__item-name">{picking ? "正在选择…" : pickError ? "重试选择文件夹…" : "选择文件夹…"}</span>
            </button>
          </li>
          {pickError && <li role="none" className="workspace-picker__error"><span role="alert">{pickError}</span></li>}
          {(projectDefault || defaultWorkspaceCwd) && <li role="none">
            <button type="button" role="menuitemradio" aria-checked={usingDefault}
              aria-describedby={`${menuId}-default-path`}
              className={`workspace-picker__item${usingDefault ? " workspace-picker__item--active" : ""}`}
              disabled={picking} onClick={() => select("")}>
              <Folder size={14} /><span className="workspace-picker__item-name">{defaultLabel}</span>
              {usingDefault && <Check size={14} />}
            </button>
            <div className="workspace-picker__hint" id={`${menuId}-default-path`}>
              {projectDefault?.cwd ? displayPath(projectDefault.cwd)
                : projectDefault ? "自动创建独立文件夹" : "自动创建独立文件夹，已有任务的目录保持固定"}
            </div>
          </li>}
          {currentProjectDirectory && <li role="none">
            <button type="button" role="menuitemradio" aria-checked="true"
              aria-describedby={`${menuId}-current-path`}
              className="workspace-picker__item workspace-picker__item--active"
              disabled={picking} onClick={() => select(currentProjectDirectory)}>
              <Folder size={14} /><span className="workspace-picker__item-name">本次任务目录</span>
              <Check size={14} />
            </button>
            <div className="workspace-picker__hint" id={`${menuId}-current-path`}>{displayPath(currentProjectDirectory)}</div>
          </li>}
          {recentWorkspaces.length === 0 && (
            <li className="workspace-picker__empty">{currentProjectDirectory ? "暂无其他历史工作目录" : "暂无历史工作目录"}</li>
          )}
          {recentWorkspaces.map((w) => (
            <li key={w.cwd} role="none">
              <button
                type="button"
                className={
                  "workspace-picker__item" +
                  (w.cwd === cwd ? " workspace-picker__item--active" : "")
                }
                onClick={() => select(w.cwd)}
                role="menuitemradio"
                disabled={picking}
                aria-checked={!automaticWorkspace && w.cwd === cwd}
                title={displayPath(w.cwd)}
              >
                <span className="workspace-picker__item-name">{shortName(w.cwd)}</span>
                <span className="workspace-picker__item-meta">
                  {w.sessionCount} 个会话
                </span>
                {w.cwd === cwd && <Check size={14} className="workspace-picker__check" />}
              </button>
            </li>
          ))}
        </ul>,
        document.body,
      )}
    </div>
  );
}

/** Hide implementation directories from the recent-folder chooser. */
export function isAutomaticTaskWorkspace(base: string, cwd: string): boolean {
  const normalize = (path: string) => path.replace(/\\/g, "/").replace(/\/$/, "");
  const prefix = `${normalize(base)}/Tasks/`;
  const value = normalize(cwd);
  return value.startsWith(prefix) && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value.slice(prefix.length));
}
