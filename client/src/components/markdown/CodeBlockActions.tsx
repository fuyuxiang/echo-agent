import { memo, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Check, Copy, AlertCircle } from "lucide-react";
import { copyShareText } from "@/lib/share";
import type { CodeBlockAction } from "./types";

type Props = {
  code: string;
  language: string;
  actions?: CodeBlockAction[];
  applyButton?: ReactNode;
  requestId?: string;
  onAction?: (
    action: string,
    code: string,
    language: string,
    requestId?: string,
  ) => void;
  copyIconOnly?: boolean;
};

export const CodeBlockActions = memo(function CodeBlockActions({
  code,
  language,
  actions = [],
  applyButton,
  requestId,
  onAction,
  copyIconOnly = true,
}: Props) {
  const [status, setStatus] = useState<"idle" | "copying" | "copied" | "failed">("idle");
  const attempt = useRef(0);
  const pending = useRef(false);

  useEffect(() => {
    if (status !== "copied" && status !== "failed") return;
    const t = window.setTimeout(() => setStatus("idle"), 2500);
    return () => window.clearTimeout(t);
  }, [status]);

  useEffect(() => {
    attempt.current += 1;
    pending.current = false;
    setStatus("idle");
    return () => { attempt.current += 1; };
  }, [code, requestId]);

  const handleCopy = useCallback(async () => {
    if (!code || pending.current) return;
    pending.current = true;
    const current = ++attempt.current;
    setStatus("copying");
    try {
      let copied: boolean;
      try { copied = await copyShareText(code); }
      catch { copied = await copyShareText(code, { clipboard: null }); }
      if (!copied) throw new Error("clipboard unavailable");
      if (attempt.current === current) {
        setStatus("copied");
        onAction?.("copy", code, language, requestId);
      }
    } catch {
      if (attempt.current === current) setStatus("failed");
    } finally {
      if (attempt.current === current) pending.current = false;
    }
  }, [code, language, onAction, requestId]);

  const copyLabel = status === "copied" ? "已复制" : status === "failed" ? "复制失败，点击重试" : status === "copying" ? "复制中…" : "复制";

  const visibleActions = actions.filter((action) => {
    if (!action.condition) return true;
    return action.condition(code, language);
  });

  return (
    <div className="md-code-actions">
      <button
        type="button"
        className="md-code-action"
        data-chat-copy="true"
        onClick={() => void handleCopy()}
        aria-label={copyLabel}
        title={copyLabel}
        disabled={!code || status === "copying"}
        aria-busy={status === "copying"}
      >
        {status === "copied" ? (
          <Check size={14} className="md-code-action-icon md-code-action-icon--ok" />
        ) : status === "failed" ? (
          <AlertCircle size={14} className="md-code-action-icon md-code-action-icon--error" />
        ) : (
          <Copy size={14} className="md-code-action-icon" />
        )}
        {(!copyIconOnly || status === "failed") && (
          <span className="md-code-action-label" role={status === "failed" ? "alert" : undefined}>{status === "failed" ? "复制失败" : copyLabel}</span>
        )}
      </button>
      {visibleActions.map((action) => (
        <button
          key={action.id}
          type="button"
          className="md-code-action"
          title={action.description || action.label}
          aria-label={action.label}
          onClick={() => {
            action.onClick(code, language);
            onAction?.(action.id, code, language, requestId);
          }}
        >
          {action.icon}
          <span className="md-code-action-label">{action.label}</span>
        </button>
      ))}
      {applyButton ? (
        <>
          <div className="md-code-divider" />
          {applyButton}
        </>
      ) : null}
    </div>
  );
});
