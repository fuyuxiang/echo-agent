import { useEffect, useMemo, useRef, useState } from "react";
import { Shield } from "lucide-react";
import { automationResolveApproval } from "@/lib/automation-client";
import { friendlyError } from "@/lib/error-format";
import type { AutomationSessionState } from "@/lib/use-session-automation";

function SessionApprovalCard({
  automation,
  onToast,
}: { automation: AutomationSessionState; onToast?: (message: string) => void }) {
  const request = automation.approvals[0];
  const [resolving, setResolving] = useState(false);
  const pendingRef = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const rejectRef = useRef<HTMLButtonElement>(null);
  const remaining = automation.approvals.length - 1;
  const detailRows = useMemo(() => {
    if (!request?.details || typeof request.details !== "object") return [];
    const details = request.details as Record<string, unknown>;
    const target = details.target && typeof details.target === "object"
      ? details.target as Record<string, unknown>
      : null;
    const safeUrl = typeof details.url === "string" ? (() => {
      try {
        const parsed = new URL(details.url as string);
        return `${parsed.origin}${parsed.pathname}`.slice(0, 300);
      } catch {
        return "";
      }
    })() : "";
    return [
      ["网站", safeUrl],
      ["页面", typeof details.pageTitle === "string" ? details.pageTitle : ""],
      ["目标控件", target && typeof target.name === "string" ? target.name : typeof details.name === "string" ? details.name : ""],
      ["目标应用", typeof details.application === "string" ? details.application : typeof details.targetName === "string" ? details.targetName : ""],
      ["目标窗口", typeof details.windowName === "string" ? details.windowName : ""],
      ["控件角色", typeof details.role === "string" ? details.role : ""],
      ["控件标识", typeof details.identifier === "string" ? details.identifier : ""],
      ["原生动作", typeof details.nativeAction === "string" ? details.nativeAction : typeof details.action === "string" ? details.action : ""],
      ["应用路径", typeof details.appPath === "string" ? details.appPath : ""],
      ["文件", Array.isArray(details.files) ? details.files.join("、") : ""],
      ["文本长度", typeof details.characters === "number" ? `${details.characters} 个字符` : ""],
      ["按键", typeof details.key === "string" ? details.key : ""],
      ["组合键", Array.isArray(details.modifiers) ? details.modifiers.filter((m): m is string => typeof m === "string").join(" + ") : ""],
      ["显示器", typeof details.displayId === "string" ? details.displayId : ""],
    ].filter((row): row is [string, string] => Boolean(row[1]));
  }, [request]);
  const decisionLabels = new Set(["网站", "目标控件", "目标应用", "目标窗口", "文件"]);
  const decisionRows = detailRows.filter(([label]) => decisionLabels.has(label));
  const technicalRows = detailRows.filter(([label]) => !decisionLabels.has(label));

  useEffect(() => {
    if (request) rejectRef.current?.focus();
  }, [request]);

  if (!request) return null;
  const resolve = async (approved: boolean, allowForTask = false) => {
    if (pendingRef.current) return;
    pendingRef.current = true;
    setResolving(true);
    setError(null);
    try {
      const found = allowForTask
        ? await automationResolveApproval(request.requestId, approved, true)
        : await automationResolveApproval(request.requestId, approved);
      if (!automation.isActive()) return;
      automation.removeApproval(request.requestId);
      if (!found) onToast?.("该确认已超时或被处理");
    } catch (error) {
      if (automation.isActive()) setError(`无法回复操作授权：${friendlyError(error)}`);
    } finally {
      pendingRef.current = false;
      if (automation.isActive()) setResolving(false);
    }
  };

  return (
    <section className="perm-inline" data-approval-source="automation" aria-label="操作授权" aria-busy={resolving}>
      <div className="perm-inline__head"><Shield size={15} aria-hidden="true" /><span>操作授权</span>
        {remaining > 0 && <span className="perm-inline__count">还有 {remaining} 项待审批</span>}
      </div>
      <div className="perm-inline__body">
        <p className="perm-inline__title">{request.title}</p>
        <p className="perm-inline__preview">{request.description}</p>
        {decisionRows.length > 0 && <dl className="perm-inline__rows" aria-label="操作对象">
          {decisionRows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
        </dl>}
        {technicalRows.length > 0 && <details className="perm-inline__details">
          <summary>操作详情</summary>
          <dl className="perm-inline__rows">
            {technicalRows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
          </dl>
        </details>}
        {error && <p className="perm-inline__error" role="alert">{error}</p>}
      </div>
      <footer className="perm-inline__footer" role="group" aria-label="授权选择">
        <button ref={rejectRef} type="button" className="perm-inline__action" disabled={resolving} onClick={() => void resolve(false)}>拒绝</button>
        <button type="button" className="perm-inline__action perm-inline__action--allow" disabled={resolving} onClick={() => void resolve(true)}>
          {resolving ? "处理中…" : "允许一次"}
        </button>
      </footer>
    </section>
  );
}

/** Specific requests share the ordinary chat approval presentation. A new
 * request remounts its decision state; no persistent automation UI is rendered. */
export function AutomationApprovalCard(props: { automation: AutomationSessionState; onToast?: (message: string) => void }) {
  const request = props.automation.approvals[0];
  return request ? <SessionApprovalCard key={`${request.sessionId}:${request.requestId}`} {...props} /> : null;
}
