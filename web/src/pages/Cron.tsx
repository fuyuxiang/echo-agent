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
  // Needed by the edit form to seed the visible fields and to detect which key
  // (`command` or `message`) this job stores its instruction under. PUT merges
  // server-side, so unknown keys and authorization flags no longer have to make
  // the round trip through the browser.
  payload?: Record<string, unknown>;
}

// 表单管的三个投递字段,以及后端 delivery 认的键名(主键在前,别名在后)。
type DeliverySlot = "channel" | "chatId" | "sessionKey";

const DELIVERY_KEYS: Record<DeliverySlot, readonly string[]> = {
  channel: ["deliver_channel", "channel"],
  chatId: ["deliver_chat_id", "chat_id"],
  sessionKey: ["source_session_key", "session_key"],
};

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
  // 该任务用 command 还是 message 作为指令键。后端两者都接受,编辑时必须沿用原键,
  // 否则一次改名就把 message 改写成 command。
  const [commandKey, setCommandKey] = useState<"command" | "message">("command");
  // 投递字段在已存 payload 里实际用的键名(null = 该任务没有这个字段)。scheduler 的
  // delivery 同时接受别名 channel / chat_id / session_key,所以既要能读出别名写的
  // 投递目标,也不能在保存时把别名键旁边再塞一份 deliver_* 空串。
  const [payloadKeys, setPayloadKeys] = useState<Record<DeliverySlot, string | null>>({
    channel: null, chatId: null, sessionKey: null,
  });

  const resetForm = () => {
    setName(""); setExpr(""); setCommand("");
    setDeliverChannel(""); setDeliverChatId(""); setSourceSessionKey("");
    setCommandKey("command");
    setPayloadKeys({ channel: null, chatId: null, sessionKey: null });
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
    // 找出该 slot 在这份 payload 里实际用的键(可能是别名),连同它的值。
    const slotOf = (slot: DeliverySlot): [string | null, string] => {
      for (const key of DELIVERY_KEYS[slot]) {
        if (key in p) return [key, str(p[key])];
      }
      return [null, ""];
    };
    const [channelKey, channelValue] = slotOf("channel");
    const [chatIdKey, chatIdValue] = slotOf("chatId");
    const [sessionKeyKey, sessionKeyValue] = slotOf("sessionKey");
    setName(job.name);
    setExpr(job.cron_expr);
    // The backend accepts either key as the instruction; remember which one this
    // job actually uses so a save does not rewrite `message` into `command`.
    setCommandKey("message" in p && !("command" in p) ? "message" : "command");
    setCommand(str(p.command) || str(p.message));
    setDeliverChannel(channelValue);
    setDeliverChatId(chatIdValue);
    setSourceSessionKey(sessionKeyValue);
    setPayloadKeys({ channel: channelKey, chatId: chatIdKey, sessionKey: sessionKeyKey });
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
    // payload 只带本表单管的字段,由后端与已存 payload 合并(PUT 是合并语义)。
    // 此前这里重建整个 payload,把表单没有的字段全部丢掉——其中 unattended_authorized
    // 缺失会被 delivery 当成 true,于是一个显式禁止无人值守授权的任务,改一次名字就
    // 变成允许执行。这里只声明真正改了什么,未知字段与授权标记留在服务端。
    const payload: Record<string, string> = { [commandKey]: command.trim() };
    const optional: [DeliverySlot, string][] = [
      ["channel", deliverChannel.trim()],
      ["chatId", deliverChatId.trim()],
      ["sessionKey", sourceSessionKey.trim()],
    ];
    for (const [slot, value] of optional) {
      const existing = payloadKeys[slot];
      // 有值就写回它原来的键(别名任务不会被平白多出一个 deliver_* 主键)。
      if (value) {
        payload[existing ?? DELIVERY_KEYS[slot][0]] = value;
        continue;
      }
      // 清空只对"这份 payload 里确实有这个键"才需要发:合并语义下省略等于沿用旧值,
      // 不发就无法清除一个投递目标;而对本来没有的字段发空串,只会给每个任务凭空
      // 存下三个空串键。
      if (editingId && existing) payload[existing] = "";
    }
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
