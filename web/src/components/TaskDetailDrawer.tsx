import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Pencil, Ban } from "lucide-react";
import { useApi } from "../hooks/use-api";
import { relativeTime, fullTimestamp } from "../lib/datetime";
import { statusMeta, useKanbanStore, canTransition, type TaskCard } from "../stores/kanban";
import { useIsAdmin } from "../stores/capabilities";
import { useConfirm } from "../components/ConfirmDialog";
import { toast } from "../stores/toast";

/**
 * Right-hand drawer with a task's full record, plus the edit and cancel paths.
 *
 * The board card can only show a truncated two-line detail, and only for
 * failed/blocked/review. A task's `description` and — more importantly — its
 * `result` (what the Agent actually did) had no surface anywhere in the
 * dashboard, even though GET /tasks/{id} has always returned them.
 *
 * Editing is here rather than on the card because PUT /tasks/{id} covers five
 * fields (title/description/priority/assignee/labels) that do not fit a hover
 * strip. Before this, a task with a typo'd title could only be cancelled and
 * recreated, which loses its result, retry count and workflow link.
 *
 * Seeded with the board's copy so the panel paints immediately, then replaced
 * by the authoritative fetch.
 */
export function TaskDetailDrawer({ task, onClose }: { task: TaskCard; onClose: () => void }) {
  const { t } = useTranslation(["kanban", "common"]);
  const { data, error, refetch } = useApi<{ task: TaskCard }>(`/tasks/${task.id}`);
  const { editTask, transitionTask, updateLocal } = useKanbanStore();
  const confirm = useConfirm();
  const canWrite = useIsAdmin() !== false;
  const detail = data?.task ?? task;
  const meta = statusMeta(detail.status);

  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [form, setForm] = useState({
    title: detail.title,
    description: detail.description,
    priority: String(detail.priority),
    assignee: detail.assignee,
    labels: (detail.labels ?? []).join(", "),
  });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const startEdit = () => {
    // Seed from the authoritative record at the moment the user opens the form,
    // not at mount: the fetch (or a WS push) may have landed since.
    setForm({
      title: detail.title,
      description: detail.description,
      priority: String(detail.priority),
      assignee: detail.assignee,
      labels: (detail.labels ?? []).join(", "),
    });
    setEditing(true);
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!form.title.trim()) return; // title is required server-side too
    // Clamp rather than reject: the backend takes any int, but the board's
    // sort and the 0-9 label only make sense inside that range.
    const parsed = Number.parseInt(form.priority, 10);
    const priority = Number.isNaN(parsed) ? detail.priority : Math.min(9, Math.max(0, parsed));
    setSaving(true);
    const ok = await editTask(detail.id, {
      title: form.title.trim(),
      description: form.description,
      priority,
      assignee: form.assignee.trim(),
      labels: form.labels.split(",").map((l) => l.trim()).filter(Boolean),
    });
    setSaving(false);
    if (ok) {
      setEditing(false);
      toast.success(t("toast.updated"));
      refetch();
    }
  };

  // DELETE /tasks/{id} is an alias for cancel (manager.cancel → CANCELLED), not
  // a hard delete, so this is worded and gated as a cancel: same state-machine
  // rule the board's drag uses, and it goes through transitionTask so a running
  // task's turn is interrupted the same way.
  const canCancel = canTransition(detail.status, "cancelled");

  const doCancel = async () => {
    const confirmed = await confirm({
      title: t("detail.cancelConfirmTitle"),
      message: t("detail.cancelConfirmMessage", { title: detail.title }),
      confirmLabel: t("action.cancel"),
      destructive: true,
    });
    if (!confirmed) return;
    const prev = detail.status;
    updateLocal(detail.id, { status: "cancelled" });
    try {
      await transitionTask(detail.id, "cancelled");
      toast.success(t("toast.cancelled"));
      onClose();
    } catch {
      updateLocal(detail.id, { status: prev }); // error toast already raised
    }
  };

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
          <div className="flex items-center gap-1 shrink-0">
            {!editing && canWrite && (
              <button
                onClick={startEdit}
                aria-label={t("detail.editAria", { title: detail.title })}
                title={t("detail.edit")}
                className="p-1 rounded hover:bg-gray-100"
              >
                <Pencil size={16} />
              </button>
            )}
            {canCancel && canWrite && (
              <button
                onClick={doCancel}
                aria-label={t("detail.cancelTask")}
                title={t("detail.cancelTask")}
                className="p-1 rounded text-red-500 hover:bg-red-50"
              >
                <Ban size={16} />
              </button>
            )}
            <button
              onClick={onClose}
              aria-label={t("common:close")}
              className="p-1 rounded hover:bg-gray-100"
            >
              <X size={18} />
            </button>
          </div>
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

        {editing ? (
          <form onSubmit={save} className="space-y-2 border rounded p-3">
            <label className="block text-xs text-gray-500">
              {t("detail.titleField")}
              <input
                value={form.title}
                onChange={(e) => setForm({ ...form, title: e.target.value })}
                className="mt-1 border rounded px-2 py-1 w-full text-sm text-gray-800"
              />
            </label>
            <label className="block text-xs text-gray-500">
              {t("detail.description")}
              <textarea
                value={form.description}
                onChange={(e) => setForm({ ...form, description: e.target.value })}
                rows={4}
                className="mt-1 border rounded px-2 py-1 w-full text-sm text-gray-800"
              />
            </label>
            <div className="flex gap-2">
              <label className="block text-xs text-gray-500 w-28">
                {t("detail.priorityField")}
                <input
                  type="number"
                  min={0}
                  max={9}
                  value={form.priority}
                  onChange={(e) => setForm({ ...form, priority: e.target.value })}
                  className="mt-1 border rounded px-2 py-1 w-full text-sm text-gray-800"
                />
              </label>
              <label className="block text-xs text-gray-500 flex-1">
                {t("detail.assigneeField")}
                <input
                  value={form.assignee}
                  onChange={(e) => setForm({ ...form, assignee: e.target.value })}
                  className="mt-1 border rounded px-2 py-1 w-full text-sm text-gray-800"
                />
              </label>
            </div>
            <label className="block text-xs text-gray-500">
              {t("detail.labelsField")}
              <input
                value={form.labels}
                onChange={(e) => setForm({ ...form, labels: e.target.value })}
                className="mt-1 border rounded px-2 py-1 w-full text-sm text-gray-800"
              />
            </label>
            <div className="flex gap-2 pt-1">
              <button
                type="submit"
                disabled={saving || !form.title.trim()}
                className="bg-blue-600 text-white px-3 py-1 rounded text-sm disabled:opacity-50"
              >
                {t("detail.save")}
              </button>
              <button
                type="button"
                onClick={() => setEditing(false)}
                className="bg-gray-100 px-3 py-1 rounded text-sm hover:bg-gray-200"
              >
                {t("common:cancel")}
              </button>
            </div>
          </form>
        ) : (
          <Field label={t("detail.description")} value={detail.description} />
        )}

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
