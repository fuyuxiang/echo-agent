import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
import { apiFetch } from "../lib/api";
import { dateTime } from "../lib/datetime";
import { runMutation } from "../stores/toast";
import { Loadable } from "../components/Loadable";
import { useConfirm } from "../components/ConfirmDialog";
import { CronRunsDrawer } from "../components/CronRunsDrawer";
import { Play, Trash2, Plus, Pencil, History } from "lucide-react";

export interface CronJob {
  id: string;
  name: string;
  cron_expr: string;
  enabled: boolean;
  status: string;
  last_status: string;
  next_run_ms: number | null;
  config_valid?: boolean;
  // Needed by the edit form: PUT /cron/{id} replaces payload wholesale, so the
  // form has to be seeded with the current one rather than sending a fresh
  // object that silently drops deliver_channel / source_session_key.
  payload?: Record<string, unknown>;
}

export function Cron() {
  const { t } = useTranslation(["cron", "common"]);
  const { data, loading, error, refetch } = useApi<{ jobs: CronJob[] }>("/cron");
  const confirm = useConfirm();
  const [showForm, setShowForm] = useState(false);
  // null = the form is creating; a job id = editing that job. Both share one set
  // of fields because PUT accepts exactly the same shape POST does.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [runsFor, setRunsFor] = useState<CronJob | null>(null);
  const [name, setName] = useState("");
  const [expr, setExpr] = useState("");
  const [command, setCommand] = useState("");
  const [deliverChannel, setDeliverChannel] = useState("");
  const [deliverChatId, setDeliverChatId] = useState("");
  const [sourceSessionKey, setSourceSessionKey] = useState("");

  const resetForm = () => {
    setName(""); setExpr(""); setCommand("");
    setDeliverChannel(""); setDeliverChatId(""); setSourceSessionKey("");
    setEditingId(null);
    setShowForm(false);
  };

  const str = (v: unknown) => (typeof v === "string" ? v : "");

  // Job runs happen with no user action at all, so a purely on-demand list
  // could not distinguish "never ran" from "fired and failed overnight". The
  // scheduler now emits cron_run into the `cron` channel (app.py wires the
  // sink), so refetch on it to keep last_status / next_run_ms honest.
  useWsSubscribe(["cron"], () => { refetch(); }, ["cron_run"]);

  const startEdit = (job: CronJob) => {
    const p = job.payload ?? {};
    setName(job.name);
    setExpr(job.cron_expr);
    // The backend accepts either key as the instruction; show whichever this
    // job actually uses so a save does not rewrite `message` into `command`.
    setCommand(str(p.command) || str(p.message));
    setDeliverChannel(str(p.deliver_channel));
    setDeliverChatId(str(p.deliver_chat_id));
    setSourceSessionKey(str(p.source_session_key));
    setEditingId(job.id);
    setShowForm(true);
  };

  const trigger = async (id: string) => {
    const ok = await runMutation(() => apiFetch(`/cron/${id}/trigger`, { method: "POST" }), {
      success: t("triggerSuccess"), error: t("triggerFailed"),
    });
    if (ok) refetch();
  };

  // 启用/停用走 PUT /cron/{id}。此前界面只读地显示状态,用户想临时停一个任务只能
  // 删掉重建——而重建会丢掉运行历史,等于被逼做破坏性操作。
  const toggleEnabled = async (job: CronJob) => {
    const ok = await runMutation(
      () => apiFetch(`/cron/${job.id}`, {
        method: "PUT",
        body: JSON.stringify({ enabled: !job.enabled }),
      }),
      {
        success: job.enabled ? t("pauseSuccess") : t("resumeSuccess"),
        error: t("toggleFailed"),
      },
    );
    if (ok) refetch();
  };

  const remove = async (job: CronJob) => {
    const confirmed = await confirm({
      title: t("deleteConfirmTitle"),
      message: t("deleteConfirmMessage", { name: job.name || job.id }),
      confirmLabel: t("common:delete"),
      destructive: true,
    });
    if (!confirmed) return;
    const ok = await runMutation(() => apiFetch(`/cron/${job.id}`, { method: "DELETE" }), {
      success: t("deleteSuccess"), error: t("deleteFailed"),
    });
    if (ok) {
      if (editingId === job.id) resetForm();
      if (runsFor?.id === job.id) setRunsFor(null);
      refetch();
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!command.trim()) {
      return; // 任务内容必填:与后端 400 校验双保险
    }
    const payload: Record<string, string> = { command: command.trim() };
    if (deliverChannel.trim()) payload.deliver_channel = deliverChannel.trim();
    if (deliverChatId.trim()) payload.deliver_chat_id = deliverChatId.trim();
    if (sourceSessionKey.trim()) payload.source_session_key = sourceSessionKey.trim();
    const body = JSON.stringify({ name, cron_expr: expr, payload });
    const ok = editingId
      ? await runMutation(
          () => apiFetch(`/cron/${editingId}`, { method: "PUT", body }),
          { success: t("updateSuccess"), error: t("updateFailed") },
        )
      : await runMutation(
          () => apiFetch("/cron", { method: "POST", body }),
          { success: t("createSuccess"), error: t("createFailed") },
        );
    if (ok) {
      resetForm();
      refetch();
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <h1 className="text-lg font-bold">{t("title")}</h1>
        <button
          onClick={() => (showForm ? resetForm() : setShowForm(true))}
          className="flex items-center gap-1 bg-blue-600 text-white px-3 py-1.5 rounded text-sm"
        >
          <Plus size={16} /> {t("new")}
        </button>
      </div>

      {showForm && (
        <form onSubmit={submit} className="bg-white border rounded p-4 flex flex-col gap-3">
          {editingId && (
            <h2 className="text-sm font-medium">{t("editTitle")}</h2>
          )}
          <div className="flex flex-wrap gap-3">
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("form.name")} aria-label={t("form.name")} className="border rounded px-3 py-1.5 flex-1 min-w-48" />
            <input value={expr} onChange={(e) => setExpr(e.target.value)} placeholder={t("form.expr")} aria-label={t("form.expr")} className="border rounded px-3 py-1.5 w-40" />
          </div>
          <textarea value={command} onChange={(e) => setCommand(e.target.value)} placeholder={t("form.command")} aria-label={t("form.command")} className="border rounded px-3 py-1.5" rows={2} />
          <div className="flex flex-wrap gap-3">
            <input value={deliverChannel} onChange={(e) => setDeliverChannel(e.target.value)} placeholder={t("form.deliverChannel")} aria-label={t("form.deliverChannel")} className="border rounded px-3 py-1.5 flex-1 min-w-48" />
            <input value={deliverChatId} onChange={(e) => setDeliverChatId(e.target.value)} placeholder={t("form.deliverChatId")} aria-label={t("form.deliverChatId")} className="border rounded px-3 py-1.5 flex-1 min-w-48" />
          </div>
          <input value={sourceSessionKey} onChange={(e) => setSourceSessionKey(e.target.value)} placeholder={t("form.sourceSessionKey")} aria-label={t("form.sourceSessionKey")} className="border rounded px-3 py-1.5" />
          <div className="flex gap-2 self-start">
            <button type="submit" disabled={!command.trim()} className="bg-green-600 text-white px-3 py-1.5 rounded text-sm disabled:opacity-50">
              {editingId ? t("save") : t("create")}
            </button>
            <button type="button" onClick={resetForm} className="bg-gray-100 px-3 py-1.5 rounded text-sm hover:bg-gray-200">
              {t("common:cancel")}
            </button>
          </div>
        </form>
      )}

      {/* 三态统一交给 Loadable:此前表头在 error/loading 时会先孤零零渲染出来,
          下面才跟一行错误提示。 */}
      <Loadable
        loading={loading}
        error={error}
        data={data}
        isEmpty={(d) => d.jobs.length === 0}
        emptyText={t("empty")}
      >
        {(d) => (
          <div className="overflow-x-auto">
            <table className="w-full text-sm min-w-[720px]">
              <thead>
                <tr className="border-b text-left text-gray-500">
                  <th className="py-2">{t("col.name")}</th>
                  <th>{t("col.cron")}</th>
                  <th>{t("col.status")}</th>
                  <th>{t("col.lastResult")}</th>
                  <th>{t("col.nextRun")}</th>
                  <th>{t("col.actions")}</th>
                </tr>
              </thead>
              <tbody>
                {d.jobs.map((job) => (
                  <tr key={job.id} className="border-b">
                    <td className="py-2 font-medium">
                      {job.name || job.id}
                      {job.config_valid === false && (
                        <span className="ml-2 text-xs px-1.5 rounded bg-red-100 text-red-700">{t("invalidConfig")}</span>
                      )}
                    </td>
                    <td className="font-mono text-xs">{job.cron_expr}</td>
                    <td>
                      <button
                        role="switch"
                        aria-checked={job.enabled}
                        aria-label={t(job.enabled ? "pauseAria" : "resumeAria", { name: job.name || job.id })}
                        onClick={() => toggleEnabled(job)}
                        className={`text-xs px-1.5 py-0.5 rounded hover:ring-1 hover:ring-blue-300 ${
                          job.enabled ? "bg-green-100 text-green-700" : "bg-gray-100 text-gray-600"
                        }`}
                      >
                        {job.enabled ? t("active") : t("paused")}
                      </button>
                    </td>
                    <td className="text-xs">{job.last_status || "-"}</td>
                    <td className="text-xs">{dateTime(job.next_run_ms)}</td>
                    <td className="flex gap-1 py-2">
                      <button onClick={() => trigger(job.id)} aria-label={t("triggerNow")} className="p-1 hover:bg-gray-100 rounded" title={t("triggerNow")}><Play size={14} /></button>
                      <button onClick={() => startEdit(job)} aria-label={t("editAria", { name: job.name || job.id })} className="p-1 hover:bg-gray-100 rounded" title={t("edit")}><Pencil size={14} /></button>
                      <button onClick={() => setRunsFor(job)} aria-label={t("runsAria", { name: job.name || job.id })} className="p-1 hover:bg-gray-100 rounded" title={t("runs")}><History size={14} /></button>
                      <button onClick={() => remove(job)} aria-label={t("common:delete")} className="p-1 hover:bg-red-50 rounded text-red-500" title={t("common:delete")}><Trash2 size={14} /></button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Loadable>

      {runsFor && (
        <CronRunsDrawer job={runsFor} onClose={() => setRunsFor(null)} />
      )}
    </div>
  );
}
