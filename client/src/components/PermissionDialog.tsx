import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, FileText, Globe, Terminal, Wrench } from "lucide-react";
import { usePermissionStore, selectPermissionForSession } from "@/stores/permission-store";
import { agentResolvePermission, permissionModeSet } from "@/lib/agent-client";
import { permissionModeStatusFromEvent, usePermissionModeStore } from "@/stores/permission-mode-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";
import type { PermissionOption } from "@/lib/types";
import { permissionPresentation } from "@/lib/permission-presentation";

const ENABLE_ALWAYS_APPROVE_OPTION_ID = "enable-always-approve";

function optionPresentation(option: PermissionOption): { label: string; description?: string } {
  if (option.optionId === ENABLE_ALWAYS_APPROVE_OPTION_ID) return { label: "本任务免审批", description: "仅限当前任务，需再次确认" };
  if (option.kind === "allow") return { label: "允许一次" };
  if (option.kind === "deny") return { label: "拒绝本次" };
  const title = option.title.trim();
  const scoped = title.match(/^(?:always\s+(?:allow|reject)|never\s+allow|始终允许|始终拒绝)\s*[:：]\s*([\s\S]+)$/i)?.[1]?.trim();
  const projectScope = title.match(/^(?:Yes,\s*always allow|No,\s*never allow)\s+(.+?)\s+for this project$/i)?.[1]?.trim();
  if (option.kind === "allow_always" || option.kind === "deny_always") {
    const allow = option.kind === "allow_always";
    return {
      label: !scoped && /[\u3400-\u9fff]/u.test(title) ? title : allow ? "始终允许此类操作" : "始终拒绝此类操作",
      description: projectScope ? `项目范围：${projectScope}（跨任务生效）`
        : scoped ? `范围：${scoped}` : allow ? "后续同类请求自动允许" : "后续同类请求自动拒绝",
    };
  }
  // Preserve custom choices from newer runtimes without guessing their scope.
  return { label: title || "其他选项" };
}

/** A task-owned decision at the composer edge; persistent grants stay secondary. */
export function PermissionInlineCard({ sessionId }: { sessionId: string | null }) {
  const head = usePermissionStore(selectPermissionForSession(sessionId));
  const count = usePermissionStore(s => sessionId ? s.queues[sessionId]?.length ?? 0 : 0);
  const dismiss = usePermissionStore(s => s.dismiss);
  const priorityRequestId = usePermissionStore(s => sessionId ? s.priorityRequestBySession[sessionId] : undefined);
  const consumePriority = usePermissionStore(s => s.consumePriority);
  const [resolvingId, setResolvingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmingAlways, setConfirmingAlways] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [previewExpanded, setPreviewExpanded] = useState(false);
  const [previewLong, setPreviewLong] = useState(false);
  const previewRef = useRef<HTMLPreElement>(null);
  const previewId = useId();
  const content = useMemo(() => head ? permissionPresentation(head) : null, [head]);
  const confirmCancelRef = useRef<HTMLButtonElement>(null);
  const menuTriggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const activeRequestRef = useRef(head?.requestId);
  activeRequestRef.current = head?.requestId;
  const pendingRef = useRef(new Set<string>());
  const { style: menuStyle } = useAnchoredFloating(menuTriggerRef, menuRef, menuOpen, {
    preferredPlacement: "top", align: "end", width: "content", maxWidth: 360, estimatedHeight: 180, maxHeight: 260,
  });

  useEffect(() => {
    setResolvingId(null);
    setError(null);
    setConfirmingAlways(false);
    setMenuOpen(false);
    setPreviewExpanded(false);
  }, [head?.requestId]);

  useLayoutEffect(() => {
    const element = previewRef.current;
    if (!element) { setPreviewLong(false); return; }
    const measure = () => {
      const style = getComputedStyle(element);
      const limit = (parseFloat(style.lineHeight) || 18) * 3
        + (parseFloat(style.paddingTop) || 0) + (parseFloat(style.paddingBottom) || 0);
      setPreviewLong(element.scrollHeight > limit + 1);
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(element);
    return () => observer?.disconnect();
  }, [head?.requestId, content?.preview]);

  useEffect(() => {
    if (confirmingAlways) confirmCancelRef.current?.focus();
  }, [confirmingAlways]);

  useEffect(() => {
    if (!menuOpen) return;
    const frame = requestAnimationFrame(() => menuRef.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus());
    const close = (event: MouseEvent) => {
      if (!menuRef.current?.contains(event.target as Node) && !menuTriggerRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => { cancelAnimationFrame(frame); document.removeEventListener("mousedown", close); };
  }, [menuOpen]);

  useEffect(() => {
    if (!sessionId || !head || priorityRequestId !== head.requestId) return;
    const frame = requestAnimationFrame(() => {
      document.getElementById(`permission-${head.requestId}`)?.focus();
      consumePriority(head.requestId, sessionId);
    });
    return () => cancelAnimationFrame(frame);
  }, [sessionId, head?.requestId, priorityRequestId, consumePriority]);

  if (!head || !content) return null;
  const { category, title, purpose, preview, previewKind, details } = content;
  const Icon = category === "terminal" ? Terminal : category === "file" ? FileText : category === "web" ? Globe : Wrench;
  const allowOnce = head.options.find(o => o.optionId === "allow-once" && o.kind === "allow")
    ?? head.options.find(o => o.kind === "allow" && o.optionId !== ENABLE_ALWAYS_APPROVE_OPTION_ID);
  const denyOnce = head.options.find(o => o.optionId === "reject-once" && o.kind === "deny")
    ?? head.options.find(o => o.kind === "deny");
  const enableAlways = head.options.find(o => o.optionId === ENABLE_ALWAYS_APPROVE_OPTION_ID);
  const moreOptions = head.options.filter(o => o !== allowOnce && o !== denyOnce);

  const resolve = async (option?: PermissionOption, cancelled = false) => {
    const request = head;
    if (pendingRef.current.has(request.requestId)) return;
    pendingRef.current.add(request.requestId);
    const isCurrent = () => activeRequestRef.current === request.requestId;
    setResolvingId(option?.optionId ?? "deny");
    setMenuOpen(false);
    setConfirmingAlways(false);
    setError(null);
    let modeChanged = false;
    try {
      if (option?.optionId === ENABLE_ALWAYS_APPROVE_OPTION_ID) {
        const result = await permissionModeSet(request.sessionId, "always-approve");
        modeChanged = true;
        const status = permissionModeStatusFromEvent(result);
        if (status) usePermissionModeStore.getState().setStatus(status);
        useSessionsStore.getState().upsert({ sessionId: request.sessionId, permissionMode: result.permissionMode ?? "always-approve" });
      }
      const acknowledged = await agentResolvePermission(request.requestId, { optionId: option?.optionId, cancelled });
      if (!acknowledged) throw new Error("该请求已失效，请刷新任务后重试");
      dismiss(request.requestId, request.sessionId);
    } catch (e) {
      const message = String(e).replace(/^Error:\s*/, "");
      if (isCurrent()) setError(modeChanged ? `已开启本任务免审批，但当前操作尚未批准：${message}` : message);
    } finally {
      pendingRef.current.delete(request.requestId);
      if (isCurrent()) setResolvingId(null);
    }
  };

  const cancelConfirmation = () => {
    setConfirmingAlways(false);
    requestAnimationFrame(() => menuTriggerRef.current?.focus());
  };

  return (
    <section id={`permission-${head.requestId}`} className="perm-inline perm-inline--tool" aria-label="操作授权" aria-busy={resolvingId != null} tabIndex={-1}>
      <div className="perm-inline__head">
        <Icon size={15} aria-hidden="true" />
        <p className="perm-inline__title">{title}</p>
        {count > 1 && <span className="perm-inline__count">还有 {count - 1} 项待审批</span>}
      </div>
      <div className="perm-inline__body" key={head.requestId}>
        {purpose && <p className="perm-inline__purpose">操作说明：{purpose}</p>}
        {preview && <>
          <pre ref={previewRef} id={previewId} className={`perm-inline__preview perm-inline__operation${previewExpanded ? " is-expanded" : ""}`}
            tabIndex={0} aria-label="操作内容">{preview}</pre>
          {previewLong && <button type="button" className="perm-inline__expand" aria-controls={previewId}
            aria-expanded={previewExpanded} onClick={() => setPreviewExpanded(value => !value)}>
            {previewExpanded ? `收起${previewKind}` : `展开完整${previewKind}`}
          </button>}
        </>}
        {details != null && <details className="perm-inline__details">
          <summary>操作详情</summary>
          <pre className="perm-inline__raw">{JSON.stringify(details, null, 2)}</pre>
        </details>}
        {error && <p className="perm-inline__error" role="alert">{error}</p>}
      </div>
      {confirmingAlways && enableAlways ? <div className="perm-inline__confirm" role="alertdialog" aria-label="确认本任务免审批"
        onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); cancelConfirmation(); } }}>
        <strong>允许本任务后续免审批？</strong>
        <span>将允许当前操作，并自动批准本任务后续的命令和文件修改。网页与电脑操作仍遵守各自的授权规则。</span>
        <div className="perm-inline__confirm-actions">
          <button ref={confirmCancelRef} type="button" className="perm-inline__action" onClick={cancelConfirmation}>取消</button>
          <button type="button" className="perm-inline__action perm-inline__action--allow" onClick={() => void resolve(enableAlways)}>确认并允许</button>
        </div>
      </div> : <footer className="perm-inline__footer" role="group" aria-label="授权选择">
        <button type="button" className="perm-inline__action" onClick={() => void resolve(denyOnce, !denyOnce)} disabled={resolvingId != null}>
          {resolvingId === (denyOnce?.optionId ?? "deny") ? "处理中…" : "拒绝本次"}
        </button>
        <div className="perm-inline__allow-group">
          {allowOnce && <button type="button" className="perm-inline__action perm-inline__action--allow"
            onClick={() => void resolve(allowOnce)} disabled={resolvingId != null}>
            {resolvingId === allowOnce.optionId ? "处理中…" : "允许一次"}
          </button>}
          {moreOptions.length > 0 && <button ref={menuTriggerRef} type="button"
            className={`perm-inline__action perm-inline__menu-trigger${allowOnce ? " perm-inline__action--allow" : ""}`}
            aria-label="更多授权选项" aria-haspopup="menu" aria-expanded={menuOpen}
            aria-controls={menuOpen ? `permission-options-${head.requestId}` : undefined}
            disabled={resolvingId != null} onClick={() => setMenuOpen(!menuOpen)}
            onKeyDown={event => { if (["ArrowDown", "ArrowUp"].includes(event.key)) { event.preventDefault(); setMenuOpen(true); } }}>
            {!allowOnce && <span>授权选项</span>}<ChevronDown size={14} aria-hidden="true" />
          </button>}
        </div>
      </footer>}
      {menuOpen && createPortal(<div ref={menuRef} id={`permission-options-${head.requestId}`}
        className="permission-options" role="menu" aria-label="更多授权选项" style={menuStyle}
        onKeyDown={event => {
          if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); setMenuOpen(false); menuTriggerRef.current?.focus(); }
          if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
            event.preventDefault();
            const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
            const index = items.indexOf(document.activeElement as HTMLButtonElement);
            const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
              : (index + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
            items[next]?.focus();
          }
        }}>
        {moreOptions.map(option => {
          const copy = optionPresentation(option);
          return (
            <button key={option.optionId} type="button" role="menuitem"
              className={option.optionId === ENABLE_ALWAYS_APPROVE_OPTION_ID ? "permission-options__elevated" : undefined}
              onClick={() => {
                if (option.optionId === ENABLE_ALWAYS_APPROVE_OPTION_ID) { setMenuOpen(false); setConfirmingAlways(true); }
                else void resolve(option);
              }}>
              <span>{copy.label}</span>
              {copy.description && <small>{copy.description}</small>}
            </button>
          );
        })}
      </div>, document.body)}
    </section>
  );
}
