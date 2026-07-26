import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import { useApi } from "../hooks/use-api";
import { relativeTime, fullTimestamp } from "../lib/datetime";
import { statusMeta, type TaskCard } from "../stores/kanban";

/**
 * Right-hand drawer with a task's full record.
 *
 * The board card can only show a truncated two-line detail, and only for
 * failed/blocked/review. A task's `description` and — more importantly — its
 * `result` (what the Agent actually did) had no surface anywhere in the
 * dashboard, even though GET /tasks/{id} has always returned them.
 *
 * Seeded with the board's copy so the panel paints immediately, then replaced
 * by the authoritative fetch.
 */
export function TaskDetailDrawer({ task, onClose }: { task: TaskCard; onClose: () => void }) {
  const { t } = useTranslation(["kanban", "common"]);
  const { data, error } = useApi<{ task: TaskCard }>(`/tasks/${task.id}`);
  const detail = data?.task ?? task;
  const meta = statusMeta(detail.status);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/20" onClick={onClose}>
      <aside
        role="dialog"
        aria-modal="true"
        aria-labelledby="task-detail-title"
        className="bg-white w-full max-w-md h-full overflow-y-auto shadow-xl p-5 space-y-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-3">
          <h2 id="task-detail-title" className="font-semibold text-base flex-1">{detail.title}</h2>
          <button
            onClick={onClose}
            aria-label={t("common:close")}
            className="p-1 rounded hover:bg-gray-100 shrink-0"
          >
            <X size={18} />
          </button>
        </div>

        <div className="flex items-center gap-2 flex-wrap text-xs">
          <span className={`px-2 py-0.5 rounded-full ${meta.chip}`}>{meta.label}</span>
          <span className="text-gray-500">P{detail.priority}</span>
          {detail.assignee && <span className="text-gray-500">@{detail.assignee}</span>}
          {detail.labels?.map((l) => (
            <span key={l} className="bg-blue-100 text-blue-700 px-1.5 rounded">{l}</span>
          ))}
        </div>

        {error && (
          <div className="text-xs text-amber-600 bg-amber-50 border border-amber-200 rounded p-2">
            {t("detail.staleWarning")}
          </div>
        )}

        <Field label={t("detail.description")} value={detail.description} />
        <Field label={t("detail.result")} value={detail.result} mono />
        <Field label={t("detail.error")} value={detail.error} mono tone="error" />
        <Field label={t("detail.blockedReason")} value={detail.blocked_reason} />
        <Field label={t("detail.reviewSummary")} value={detail.review_summary} />

        <dl className="text-xs text-gray-500 space-y-1 border-t pt-3">
          <Row label={t("detail.source")} value={detail.source || "-"} />
          <Row label={t("detail.sessionId")} value={detail.session_id || "-"} />
          <Row label={t("detail.retries")} value={`${detail.retry_count}/${detail.max_retries}`} />
          <Row
            label={t("detail.createdAt")}
            value={relativeTime(detail.created_at)}
            title={fullTimestamp(detail.created_at)}
          />
          <Row
            label={t("detail.updatedAt")}
            value={relativeTime(detail.updated_at)}
            title={fullTimestamp(detail.updated_at)}
          />
          <Row label={t("detail.id")} value={detail.id} />
        </dl>
      </aside>
    </div>
  );
}

function Field({
  label,
  value,
  mono,
  tone,
}: {
  label: string;
  value: string;
  mono?: boolean;
  tone?: "error";
}) {
  if (!value) return null;
  return (
    <section>
      <h3 className="text-xs font-medium text-gray-500 mb-1">{label}</h3>
      <div
        className={`text-sm whitespace-pre-wrap break-words rounded p-2 ${
          tone === "error" ? "bg-red-50 text-red-700" : "bg-gray-50 text-gray-800"
        } ${mono ? "font-mono text-xs" : ""}`}
      >
        {value}
      </div>
    </section>
  );
}

function Row({ label, value, title }: { label: string; value: string; title?: string }) {
  return (
    <div className="flex gap-2">
      <dt className="shrink-0 w-20">{label}</dt>
      <dd className="flex-1 break-all font-mono" title={title}>{value}</dd>
    </div>
  );
}
