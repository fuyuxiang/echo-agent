import { useState, useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { XCloseIcon } from "@/foundation/components/Icon/icons";
const logoMarkUrl = "/app-icon.png";

interface MenuItem {
  label: string;
  action?: "minimize" | "maximize" | "close";
  /** Maps the item to a `document.execCommand` action (editing menu). */
  edit?: "undo" | "redo" | "cut" | "copy" | "paste" | "selectAll";
  /** Opens the About dialog. */
  about?: boolean;
  /** Opens the signed application updater. */
  update?: boolean;
}
const MENUS: Record<string, MenuItem[]> = {
  编辑: [
    { label: "撤销", edit: "undo" },
    { label: "重做", edit: "redo" },
    { label: "剪切", edit: "cut" },
    { label: "复制", edit: "copy" },
    { label: "粘贴", edit: "paste" },
    { label: "全选", edit: "selectAll" },
  ],
  窗口: [
    { label: "最小化", action: "minimize" },
    { label: "最大化", action: "maximize" },
    { label: "关闭", action: "close" },
  ],
  帮助: [
    { label: "检查更新…", update: true },
    { label: "关于 EchoAgent", about: true },
  ],
};

// 最小化图标
const MinimizeIcon = () => (
  <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
    <rect x="1" y="5.5" width="10" height="1" fill="currentColor" />
  </svg>
);

// 最大化/还原图标
const MaximizeIcon = ({ isMaximized }: { isMaximized: boolean }) => {
  if (isMaximized) {
    return (
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
        <rect x="3" y="1" width="8" height="8" stroke="currentColor" strokeWidth="1" fill="none" />
        <rect x="1" y="3" width="8" height="8" stroke="currentColor" strokeWidth="1" fill="var(--echo-bg-primary)" />
      </svg>
    );
  }
  return (
    <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
      <rect x="1" y="1" width="10" height="10" stroke="currentColor" strokeWidth="1" fill="none" />
    </svg>
  );
};

async function win(action: "minimize" | "maximize" | "close") {
  try {
    const w = getCurrentWindow();
    if (action === "minimize") void w.minimize();
    else if (action === "maximize") void w.toggleMaximize();
    else void w.close();
  } catch {
    // 浏览器预览环境下无 Tauri 窗口,静默忽略
  }
}

function handleDropdownKeyDown(
  event: ReactKeyboardEvent<HTMLDivElement>,
  onEscape: () => void,
) {
  if (event.altKey || event.ctrlKey || event.metaKey) return;
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    onEscape();
    return;
  }
  if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;

  const items = Array.from(
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)'),
  );
  if (items.length === 0) return;
  event.preventDefault();
  const current = items.indexOf(document.activeElement as HTMLButtonElement);
  if (event.key === "Home") items[0].focus();
  else if (event.key === "End") items[items.length - 1].focus();
  else if (event.key === "ArrowDown") {
    items[current < 0 ? 0 : (current + 1) % items.length].focus();
  } else {
    items[current < 0 ? items.length - 1 : (current - 1 + items.length) % items.length].focus();
  }
}

/** Insert text at the cursor position of the currently focused editable
 *  element (textarea/input). Used by the Edit → Paste menu item. Falls back
 *  to execCommand('insertText') when available so undo history is preserved. */
function insertTextAtFocus(text: string) {
  const el = document.activeElement as HTMLTextAreaElement | HTMLInputElement | null;
  if (!el || (el.tagName !== "TEXTAREA" && el.tagName !== "INPUT")) {
    // No text target focused — try execCommand as a generic fallback.
    try {
      document.execCommand("insertText", false, text);
    } catch {
      /* ignore */
    }
    return;
  }
  const start = el.selectionStart ?? el.value.length;
  const end = el.selectionEnd ?? el.value.length;
  const next = el.value.slice(0, start) + text + el.value.slice(end);
  el.value = next;
  const caret = start + text.length;
  el.selectionStart = el.selectionEnd = caret;
  // React-controlled inputs need a native input event to update state.
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/**
 * 自定义标题栏 - Tauri 2 无边框窗口
 *
 * 功能:
 * - 拖拽移动窗口 (data-tauri-drag-region)
 * - 品牌标识 + 菜单
 * - 窗口控制按钮 (最小化/最大化/关闭)
 */
export function TitleBar({
  onPlaceholder,
  onShowAbout,
  onCheckForUpdates,
  hideMenus = false,
}: {
  onPlaceholder: (label: string) => void;
  onShowAbout?: () => void;
  onCheckForUpdates?: () => void;
  hideMenus?: boolean;
}) {
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const [isMaximized, setIsMaximized] = useState(false);
  const menuButtonRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const dropdownRefs = useRef<Record<string, HTMLDivElement | null>>({});
  const pendingKeyboardFocus = useRef<{ name: string; edge: "first" | "last" } | null>(null);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const checkMaximized = async () => {
      try {
        const w = getCurrentWindow();
        const maximized = await w.isMaximized();
        if (disposed) return;
        setIsMaximized(maximized);

        // 监听窗口状态变化
        const stop = await w.onResized(async () => {
          if (disposed) return;
          try {
            const nextMaximized = await w.isMaximized();
            if (!disposed) setIsMaximized(nextMaximized);
          } catch {
            // 窗口已关闭或预览环境无原生窗口。
          }
        });
        // Subscription can resolve after the component has already unmounted.
        if (disposed) stop();
        else unlisten = stop;
      } catch {
        // 预览模式
      }
    };
    void checkMaximized();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const pending = pendingKeyboardFocus.current;
    if (!openMenu || !pending || pending.name !== openMenu) return;
    const items = dropdownRefs.current[openMenu]?.querySelectorAll<HTMLButtonElement>(
      '[role="menuitem"]:not(:disabled)',
    );
    items?.[pending.edge === "first" ? 0 : items.length - 1]?.focus();
    pendingKeyboardFocus.current = null;
  }, [openMenu]);

  const openMenuFromKeyboard = (name: string, edge: "first" | "last") => {
    if (openMenu === name) {
      const items = dropdownRefs.current[name]?.querySelectorAll<HTMLButtonElement>(
        '[role="menuitem"]:not(:disabled)',
      );
      items?.[edge === "first" ? 0 : items.length - 1]?.focus();
      return;
    }
    pendingKeyboardFocus.current = { name, edge };
    setOpenMenu(name);
  };

  const closeMenuAndRestoreFocus = (name: string) => {
    setOpenMenu(null);
    menuButtonRefs.current[name]?.focus();
  };

  const runItem = (item: MenuItem) => {
    setOpenMenu(null);
    if (item.action) {
      win(item.action);
      return;
    }
    if (item.edit) {
      // Editing commands operate on the focused editable element (textarea,
      // input). execCommand is deprecated but still the only API that works
      // without a heavy editor framework, and it's well-supported in Tauri's
      // webview. For paste, prefer the async Clipboard API (execCommand paste
      // is blocked in non-secure contexts).
      if (item.edit === "paste") {
        navigator.clipboard
          ?.readText()
          .then((text) => insertTextAtFocus(text))
          .catch(() => {
            /* clipboard permission denied — silent */
          });
      } else {
        try {
          document.execCommand(item.edit);
        } catch {
          /* no editable focus — silent */
        }
      }
      return;
    }
    if (item.about) {
      onShowAbout?.();
      return;
    }
    if (item.update) {
      onCheckForUpdates?.();
      return;
    }
    onPlaceholder(item.label);
  };

  return (
    <div className="titlebar" data-tauri-drag-region>
      {!hideMenus && (
        <div className="titlebar__menus" data-tauri-drag-region>
          <button
            className="titlebar__brand"
            onClick={() => onShowAbout?.()}
            aria-label="EchoAgent"
            title="关于 EchoAgent"
          >
            <img
              src={logoMarkUrl}
              alt=""
              width={18}
              height={18}
              className="titlebar__brand-logo"
            />
            <span>EchoAgent</span>
          </button>
          {Object.keys(MENUS).map((name) => (
            <div key={name} className="titlebar__menu-wrap">
              <button
                ref={(element) => {
                  menuButtonRefs.current[name] = element;
                }}
                type="button"
                className={"titlebar__menu" + (openMenu === name ? " titlebar__menu--open" : "")}
                onClick={() => setOpenMenu(openMenu === name ? null : name)}
                aria-label={name}
                aria-haspopup="menu"
                aria-expanded={openMenu === name}
                aria-controls={openMenu === name ? `titlebar-menu-${name}` : undefined}
                onKeyDown={(event) => {
                  if (["Enter", " ", "ArrowDown", "ArrowUp"].includes(event.key)) {
                    event.preventDefault();
                    openMenuFromKeyboard(name, event.key === "ArrowUp" ? "last" : "first");
                  } else if (event.key === "Escape" && openMenu === name) {
                    event.preventDefault();
                    setOpenMenu(null);
                  }
                }}
              >
                {name}
              </button>
              {openMenu === name && (
                <>
                  <div
                    className="titlebar__backdrop"
                    onClick={() => closeMenuAndRestoreFocus(name)}
                  />
                  <div
                    ref={(element) => {
                      dropdownRefs.current[name] = element;
                    }}
                    id={`titlebar-menu-${name}`}
                    className="titlebar__dropdown"
                    role="menu"
                    aria-label={`${name}菜单`}
                    onKeyDown={(event) => handleDropdownKeyDown(
                      event,
                      () => closeMenuAndRestoreFocus(name),
                    )}
                  >
                    {MENUS[name].map((item) => (
                      <button
                        key={item.label}
                        type="button"
                        role="menuitem"
                        className="titlebar__dropdown-item"
                        onClick={() => runItem(item)}
                      >
                        {item.label}
                      </button>
                    ))}
                  </div>
                </>
              )}
            </div>
          ))}
        </div>
      )}

      {/* 弹性空间 - 用于拖拽 */}
      <div className="titlebar__spacer" data-tauri-drag-region />

      {/* 窗口控制按钮 */}
      <div className="titlebar__controls">
        <button
          className="titlebar__control titlebar__control--minimize"
          onClick={() => win("minimize")}
          title="最小化"
        >
          <MinimizeIcon />
        </button>
        <button
          className="titlebar__control titlebar__control--maximize"
          onClick={() => win("maximize")}
          title={isMaximized ? "还原" : "最大化"}
        >
          <MaximizeIcon isMaximized={isMaximized} />
        </button>
        <button
          className="titlebar__control titlebar__control--close"
          onClick={() => win("close")}
          title="关闭窗口"
        >
          <XCloseIcon size="sm" />
        </button>
      </div>
    </div>
  );
}
