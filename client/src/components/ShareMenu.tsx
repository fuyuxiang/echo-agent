/**
 * 分享 / 导出菜单 —— 对齐 EchoAgent `share:*`(导出对话 / 分享链接)。
 *
 * EchoAgent 本地导出(markdown/html/text 下载 + mailto 分享意图),不上传云端。
 * 由 ChatView 顶栏「分享」按钮触发。
 */
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  buildSharePayload,
  buildMailtoUrl,
  copyShareText,
  systemShare,
  triggerDownload,
  type ShareFormat,
} from "@/lib/share";
import type { ChatMessage } from "@/stores/session-store";
import { useModalFocus } from "@/lib/use-modal-focus";
import { openExternalUrl, exportTextFile } from "@/lib/agent-client";

interface ShareMenuProps {
  controlledOpen?: boolean;
  onClose?: () => void;
  returnFocus?: HTMLElement | null;
  saveMarkdown?: boolean;
  /** Use the native file picker consistently for all exported formats. */
  saveToFile?: boolean;
  messages: ChatMessage[];
  title?: string;
  /** 宿主回调(打开系统邮件客户端)。依赖注入便于测试;缺省用 window.open。 */
  openUrl?: (url: string) => void;
  onDone?: (msg: string) => void;
}

export function ShareMenu({ messages, title, openUrl, onDone, controlledOpen, onClose, returnFocus, saveMarkdown, saveToFile }: ShareMenuProps) {
  const [localOpen, setLocalOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const open = controlledOpen ?? localOpen;
  const openRef = useRef(open);
  openRef.current = open;
  const operationRef = useRef(0);
  const busyRef = useRef(false);
  const setOpen = (next: boolean | ((prev: boolean) => boolean)) => {
    const value = typeof next === "function" ? next(open) : next;
    openRef.current = value;
    if (!value) {
      operationRef.current++;
      busyRef.current = false;
      setBusy(false);
    }
    if (controlledOpen === undefined) setLocalOpen(value);
    if (!value) onClose?.();
  };
  const dialogRef = useModalFocus<HTMLDivElement>(controlledOpen === true, () => setOpen(false), returnFocus);
  const [includeProcess, setIncludeProcess] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const popoverId = useId();
  const [position, setPosition] = useState({ top: 0, left: 0 });
  useEffect(() => () => { operationRef.current++; }, []);
  useEffect(() => {
    if (!open) {
      operationRef.current++;
      busyRef.current = false;
      setBusy(false);
    }
  }, [open]);

  useLayoutEffect(() => {
    if (!open || controlledOpen !== undefined) return;
    const place = () => {
      const trigger = triggerRef.current?.getBoundingClientRect();
      const popover = popoverRef.current;
      if (!trigger || !popover) return;
      const width = popover.offsetWidth;
      const height = popover.offsetHeight;
      const left = Math.max(8, Math.min(trigger.right - width, window.innerWidth - width - 8));
      const below = trigger.bottom + 6;
      const above = trigger.top - height - 6;
      const top = below + height <= window.innerHeight - 8
        ? below
        : above >= 8 ? above : Math.max(8, window.innerHeight - height - 8);
      setPosition({ top, left });
    };
    place();
    popoverRef.current?.querySelector<HTMLElement>('input, button')?.focus();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open, controlledOpen]);

  useEffect(() => {
    if (!open || controlledOpen !== undefined) return;
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node) && !popoverRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
      triggerRef.current?.focus();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open]);

  const exportAs = async (format: ShareFormat) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    const operation = ++operationRef.current;
    const isCurrent = () => operation === operationRef.current && openRef.current;
    try {
      const payload = await buildSharePayload(messages, format, title, { includeProcess });
      if (!isCurrent()) return;
      if (saveToFile ?? saveMarkdown) {
        const extension = { markdown: "md", html: "html", text: "txt" }[format];
        const path = await exportTextFile(payload.filename, payload.content, extension);
        if (isCurrent() && path) { onDone?.(`已导出到 ${path}`); setOpen(false); }
      } else {
        triggerDownload(payload);
        setOpen(false);
        onDone?.(`已导出 ${payload.filename}`);
      }
    } catch (error) {
      if (isCurrent()) onDone?.(`导出失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      if (operation === operationRef.current) {
        busyRef.current = false;
        setBusy(false);
      }
    }
  };

  const shareMail = () => {
    const payload = buildSharePayload(messages, "text", title, { includeProcess });
    const url = buildMailtoUrl(title || "对话分享", payload.content);
    if (openUrl) {
      openUrl(url);
    } else {
      void openExternalUrl(url).catch((error) => {
        onDone?.(`打开邮件客户端失败：${String(error).replace(/^Error:\s*/, "")}`);
      });
    }
    setOpen(false);
    onDone?.("已打开邮件分享");
  };

  const copyMarkdown = async () => {
    const payload = buildSharePayload(messages, "markdown", title, { includeProcess });
    try {
      if (!await copyShareText(payload.content)) throw new Error("当前环境不支持剪贴板");
      setOpen(false);
      onDone?.("已复制 Markdown");
    } catch (error) {
      onDone?.(`复制失败：${String(error).replace(/^Error:\s*/, "")}`);
    }
  };

  const shareSystem = async () => {
    const payload = buildSharePayload(messages, "text", title, { includeProcess });
    try {
      if (!await systemShare(payload, title)) throw new Error("当前系统不支持原生分享");
      setOpen(false);
      onDone?.("已打开系统分享");
    } catch (error) {
      // Cancelling the native share sheet is expected and needs no error toast.
      if ((error as { name?: string })?.name !== "AbortError") {
        onDone?.(`分享失败：${String(error).replace(/^Error:\s*/, "")}`);
      }
    }
  };

  return (
    <div className="share-menu" ref={ref}>
      {controlledOpen === undefined && <button
        ref={triggerRef}
        type="button"
        className={
          "share-menu__trigger" +
          (open ? " share-menu__trigger--active" : "")
        }
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        title="导出 / 分享本会话"
        aria-expanded={open}
        aria-controls={open ? popoverId : undefined}
      >
        分享
      </button>}
      {open && createPortal(
        <div className={controlledOpen !== undefined ? "workbench-dialog-overlay" : undefined} onMouseDown={event => { if (event.target === event.currentTarget) setOpen(false); }}>
        <div id={popoverId} ref={controlledOpen !== undefined ? dialogRef : popoverRef} className={"share-menu__popover" + (controlledOpen !== undefined ? " share-menu__popover--dialog" : "")} role={controlledOpen !== undefined ? "dialog" : "group"} aria-modal={controlledOpen !== undefined ? true : undefined} aria-label="分享选项" style={controlledOpen === undefined ? position : undefined} onClick={(e) => e.stopPropagation()}>
          {controlledOpen !== undefined && <div className="workbench-dialog__header"><strong>导出与本地分享</strong><button type="button" className="main-topbar__btn" aria-label="关闭分享选项" onClick={() => setOpen(false)}>×</button></div>}
          <div className="share-menu__note">本地分享，不会自动上传会话</div>
          <label className="share-menu__option">
            <input
              type="checkbox"
              disabled={busy}
              checked={includeProcess}
              onChange={(event) => setIncludeProcess(event.target.checked)}
            />
            <span>
              包含执行过程
              <small>阶段说明、分析详情与操作记录</small>
            </span>
          </label>
          <div className="share-menu__divider" />
          <button type="button" className="share-menu__item" disabled={busy} onClick={() => void copyMarkdown()}>
            复制 Markdown
          </button>
          {typeof navigator !== "undefined" && typeof navigator.share === "function" && (
            <button type="button" className="share-menu__item" disabled={busy} onClick={() => void shareSystem()}>
              系统分享…
            </button>
          )}
          <div className="share-menu__divider" />
          <button type="button" className="share-menu__item" aria-label="导出 Markdown" aria-describedby={`${popoverId}-markdown`} disabled={busy} onClick={() => void exportAs("markdown")}>
            <span>导出 Markdown</span><small id={`${popoverId}-markdown`}>保留 Markdown 源码，便于编辑</small>
          </button>
          <button type="button" className="share-menu__item" aria-label="导出 HTML" aria-describedby={`${popoverId}-html`} disabled={busy} onClick={() => void exportAs("html")}>
            <span>导出 HTML</span><small id={`${popoverId}-html`}>打开即可阅读排版，包含表格、代码和公式</small>
          </button>
          <button type="button" className="share-menu__item" disabled={busy} onClick={() => void exportAs("text")}>
            导出纯文本
          </button>
          <div className="share-menu__divider" />
          {busy && <div className="share-menu__note" role="status">正在准备导出文件…</div>}
          <button type="button" className="share-menu__item" disabled={busy} onClick={shareMail}>
            通过邮件分享
          </button>
        </div>
        </div>,
        document.body,
      )}
    </div>
  );
}
