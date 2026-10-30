import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Info } from "lucide-react";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";
import type { estimateSendCost } from "@/lib/token-estimate";
import { useContextUsage } from "@/lib/use-context-usage";
import type { ContextInfo } from "@/lib/types";

/**
 * Context-usage ring + popover for the Composer footer.
 * Reads runtime snapshots independently of the popover's visibility. Polls
 * during execution and refreshes on turn/model changes and window activation.
 */

/** Adaptive token formatting: ≥100万 → 万, else K. */
function fmtToken(t: number, useWan: boolean): string {
  if (useWan) {
    const wan = t / 10000;
    return `${Number.isInteger(wan) ? wan : wan.toFixed(1)}万`;
  }
  if (t >= 1000) return `${(t / 1000).toFixed(1)}K`;
  return String(t);
}

interface CategoryRow {
  key: string;
  label: string;
  tokens: number;
  detail?: string;
}

function buildRows(ctx: ContextInfo): CategoryRow[] {
  const cats = ctx.usageCategories ?? [];
  const skills = cats.find((c) => c.label === "Skills");
  const mcp = cats.find((c) => c.label === "MCP servers");
  const rows: CategoryRow[] = [
    { key: "messages", label: "消息", tokens: ctx.messageTokens },
    { key: "tools", label: "系统工具", tokens: ctx.toolDefinitionsTokens },
    { key: "skills", label: "技能", tokens: skills?.tokens ?? 0, detail: skills?.detail },
    { key: "system", label: "系统提示词", tokens: ctx.systemPromptTokens },
  ];
  if (mcp && mcp.tokens > 0) {
    rows.push({ key: "mcp", label: "MCP", tokens: mcp.tokens, detail: mcp.detail });
  }
  // 守恒收敛(参照 EchoAgent distributeWithConservation):EchoAgent 的各项是独立
  // 估算,口径与 used 不完全一致(可能重叠),总和可能超过 used。超过时按比例
  // 缩放到 used,保证各项占比之和恒为 100%;不足时差额归入「其他」。
  const known = rows.reduce((s, r) => s + r.tokens, 0);
  if (known > ctx.used && known > 0) {
    const scale = ctx.used / known;
    for (const r of rows) r.tokens = Math.round(r.tokens * scale);
    rows.push({ key: "other", label: "其他", tokens: Math.max(0, ctx.used - rows.reduce((s, r) => s + r.tokens, 0)) });
  } else {
    rows.push({ key: "other", label: "其他", tokens: Math.max(0, ctx.used - known) });
  }
  return rows;
}

export function ContextUsagePill({ sessionId, active = false, onRefreshSignal, estimate }: { sessionId?: string; active?: boolean; onRefreshSignal?: unknown; estimate?: ReturnType<typeof estimateSendCost> }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const { ctx, usage } = useContextUsage(sessionId, active, open, onRefreshSignal);
  const { style } = useAnchoredFloating(triggerRef, popoverRef, open, { align: "end", width: 320 });

  useEffect(() => {
    setOpen(false);
  }, [sessionId]);

  useEffect(() => {
    if (!open) return;
    popoverRef.current?.focus();
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node) && !popoverRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") { setOpen(false); triggerRef.current?.focus(); } };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  if (!ctx?.total && !estimate) return null;

  const pct = ctx?.usagePct ?? 0;
  const useWan = (ctx?.total ?? 0) >= 1_000_000;
  const rows = ctx ? buildRows(ctx) : [];
  const cacheHit =
    usage && usage.inputTokens > 0
      ? `${((usage.cachedReadTokens / usage.inputTokens) * 100).toFixed(1)}%`
      : "—";

  // Ring SVG params
  const r = 7;
  const c = 2 * Math.PI * r;
  const offset = c - (Math.min(pct, 100) / 100) * c;

  return (
    <div className="context-usage" ref={ref}>
      <button
        type="button"
        ref={triggerRef}
        aria-label={ctx ? `上下文已使用 ${pct}%` : "上下文与用量详情"}
        aria-haspopup="dialog"
        aria-expanded={open}
        className={"context-usage__pill" + (open ? " context-usage__pill--open" : "") + (pct >= 60 || estimate?.severity !== "ok" && estimate ? " context-usage__pill--attention" : "")}
        onClick={(e) => { e.stopPropagation(); setOpen((o) => !o); }}
        title={ctx ? `${pct}% · ${fmtToken(ctx.used, useWan)} / ${fmtToken(ctx.total, useWan)} 上下文已使用` : `本地估算：约 ${estimate?.newTokens} token，非计费结果`}
      >
        {ctx ? <svg width="18" height="18" viewBox="0 0 18 18" className="context-usage__ring" aria-hidden="true">
          <circle cx="9" cy="9" r={r} fill="none" className="context-usage__ring-track" />
          <circle
            cx="9" cy="9" r={r} fill="none"
            className="context-usage__ring-progress"
            strokeDasharray={c}
            strokeDashoffset={offset}
          />
        </svg> : <Info size={15} />}
        {pct >= 60 && <span className="context-usage__pill-text">{pct}%</span>}
      </button>
      {open && createPortal(
        <div style={style} ref={popoverRef} role="dialog" aria-label="上下文详情" tabIndex={-1} className="context-usage__popover" onClick={(e) => e.stopPropagation()}>
          {estimate && <p className="context-usage__estimate">本次输入约 {estimate.newTokens.toLocaleString()} token。仅为本地估算，非计费结果。</p>}
          {ctx && <><div className="context-usage__popover-header">
            <span className="context-usage__popover-title">上下文容量</span>
            <span className="context-usage__popover-summary">
              {fmtToken(ctx.used, useWan)}/{fmtToken(ctx.total, useWan)}（{pct}%）
            </span>
          </div>
          <div className="context-usage__bar">
            <div
              className="context-usage__bar-fill"
              style={{ width: `${Math.min(100, Math.max(0, (ctx.used / ctx.total) * 100))}%` }}
            />
          </div>
          <ul className="context-usage__rows">
            {rows.map((r) => (
              <li key={r.key} className="context-usage__row">
                <span className={`context-usage__swatch context-usage__swatch--${r.key}`} />
                <span className="context-usage__row-label">
                  {r.label}
                  {r.detail ? <span className="context-usage__row-detail">{r.detail}</span> : null}
                </span>
                <span className="context-usage__row-value">
                  {ctx.used > 0 ? `${((r.tokens / ctx.used) * 100).toFixed(1)}%` : "0%"}
                </span>
              </li>
            ))}
          </ul>
          <div className="context-usage__footer">
            <span>平均缓存命中率</span>
            <span className="context-usage__footer-value">{cacheHit}</span>
          </div>
          </>}
        </div>, document.body
      )}
    </div>
  );
}
