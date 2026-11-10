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

function fmtPercent(pct: number): string {
  if (pct > 0 && pct < 0.1) return "<0.1%";
  return `${Number(pct.toFixed(1))}%`;
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
  // 分类估算可能重叠。超出已用量时按比例缩放，保留小数以免改变色段的总宽度。
  // 分类合计不足已用量时，差额归入「其他」。
  const known = rows.reduce((s, r) => s + r.tokens, 0);
  if (known > ctx.used) {
    const scale = ctx.used / known;
    for (const row of rows) row.tokens *= scale;
  }
  rows.push({ key: "other", label: "其他", tokens: Math.max(0, ctx.used - known) });
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
  const rows = ctx ? buildRows(ctx).map((row) => ({
    ...row,
    percent: (row.tokens / ctx.total) * 100,
  })) : [];
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
              {fmtToken(ctx.used, useWan)}/{fmtToken(ctx.total, useWan)}（{fmtPercent((ctx.used / ctx.total) * 100)}）
            </span>
          </div>
          <div className="context-usage__bar" aria-hidden="true">
            {rows.filter((row) => row.percent > 0).map((row) => (
              <span
                key={row.key}
                className={`context-usage__bar-segment context-usage__color--${row.key}`}
                style={{ width: `${row.percent}%` }}
              />
            ))}
          </div>
          <p className="context-usage__estimate">
            {typeof ctx.autoCompactThresholdPercent === "number" && <>自动整理阈值 {ctx.autoCompactThresholdPercent}%。</>}
            单次请求超出模型输入预算时也可能提前整理。各项占比为估算，按模型总容量计算。
          </p>
          <ul className="context-usage__rows">
            {rows.map((r) => (
              <li key={r.key} className="context-usage__row">
                <span className={`context-usage__swatch context-usage__color--${r.key}`} />
                <span className="context-usage__row-label">
                  {r.label}
                  {r.detail ? <span className="context-usage__row-detail">{r.detail}</span> : null}
                </span>
                <span className="context-usage__row-value">
                  {fmtPercent(r.percent)}
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
