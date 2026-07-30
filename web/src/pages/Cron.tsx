import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
import { apiFetch } from "../lib/api";
import { dateTime } from "../lib/datetime";
import { runMutation, toast } from "../stores/toast";
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
  // server-side, so unknown keys no longer have to make the round trip through
  // the browser. Authorization is not in here at all: it is a first-class field
  // on ScheduledJob, and a same-named key inside payload carries no weight.
  payload?: Record<string, unknown>;
  /** Audit trail of who granted unattended execution, or null if never granted. */
  authorization?: {
    operator: string;
    source: string;
    granted_at_ms: number;
    summary: string;
  } | null;
  /** Whether that grant still applies to the job's current content. Separate
   *  from `authorization` on purpose: having one without the other is the
   *  "edited after authorizing" state. */
  authorization_valid?: boolean;
}

// 表单管的三个投递字段,以及后端 delivery 认的键名(主键在前,别名在后)。
type DeliverySlot = "channel" | "chatId" | "sessionKey";

const DELIVERY_KEYS: Record<DeliverySlot, readonly string[]> = {
  channel: ["deliver_channel", "channel"],
  chatId: ["deliver_chat_id", "chat_id"],
  sessionKey: ["source_session_key", "session_key"],
};

type AuthState = "granted" | "stale" | "none";

// "Has a grant" and "the grant still applies" are two separate facts, so a job
// carrying an authorization whose fingerprint no longer matches is neither
// authorized nor untouched — it needs a human to look again.
function authStateOf(job: CronJob): AuthState {
  if (job.authorization_valid) return "granted";
  return job.authorization ? "stale" : "none";
}

const AUTH_BADGE: Record<AuthState, string> = {
  granted: "bg-green-100 text-green-800",
  stale: "bg-amber-100 text-amber-900",
  none: "bg-gray-100 text-gray-600",
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
  // Unattended authorization is opt-in per submit and never sticky: reopening
  // the form for an edit starts unchecked, so a previously authorized job is
  // not silently re-authorized by an unrelated rename.
  const [authorizeUnattended, setAuthorizeUnattended] = useState(false);
  // Exactly what the user was shown when they confirmed. The backend fingerprints
  // whatever the request carries, so without this a user could confirm "echo hi"
  // and then edit the instruction into anything before saving — walking away with
  // a valid grant for content nobody ever read. Consent covers what was on screen,
  // so a mismatch at submit time revokes it rather than travelling with the edit.
  const [authorizedSnapshot, setAuthorizedSnapshot] = useState("");

  const resetForm = () => {
    setName(""); setExpr(""); setCommand("");
    setDeliverChannel(""); setDeliverChatId(""); setSourceSessionKey("");
    setCommandKey("command");
    setPayloadKeys({ channel: null, chatId: null, sessionKey: null });
    setAuthorizeUnattended(false);
    setAuthorizedSnapshot("");
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
    // Deliberately not seeded from job.authorization: an existing grant is the
    // server's business, and pre-checking the box would turn every edit into a
    // fresh authorization the user never asked for. The snapshot goes with it,
    // so consent given for one job cannot linger into the next.
    setAuthorizeUnattended(false);
    setAuthorizedSnapshot("");
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

  // The delivery target as shown in the confirm dialog.
  const deliveryTarget = () =>
    [deliverChannel.trim(), deliverChatId.trim()].filter(Boolean).join(":") ||
    sourceSessionKey.trim();

  // Everything the confirm dialog puts in front of the user, as one comparable
  // string. Built in one place so the dialog and the submit-time check can never
  // disagree about which fields consent covers.
  const consentDigest = () =>
    JSON.stringify([command.trim(), expr.trim(), deliveryTarget()]);

  // 勾选授权前先让人看清自己在授权什么:指令全文、频率、投递目标。拒绝确认就保持
  // 关闭,不存在"点了一下就授权了"这条路径。
  const toggleAuthorize = async (checked: boolean) => {
    if (!checked) {
      setAuthorizeUnattended(false);
      setAuthorizedSnapshot("");
      return;
    }
    const shown = consentDigest();
    const ok = await confirm({
      title: t("authorizeConfirmTitle"),
      message: t("authorizeConfirmMessage", {
        command: command.trim() || t("authorizeUnset"),
        expr: expr.trim() || t("authorizeUnset"),
        target: deliveryTarget() || t("authorizeNoTarget"),
      }),
      confirmLabel: t("authorizeConfirmLabel"),
      destructive: true,
    });
    setAuthorizeUnattended(ok);
    // `shown` is captured before the await, from the same reads that built the
    // dialog message — so it is exactly the text the user was asked to approve.
    setAuthorizedSnapshot(ok ? shown : "");
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!command.trim()) {
      return; // 任务内容必填:与后端 400 校验双保险
    }
    // payload 只带本表单管的字段,由后端与已存 payload 合并(PUT 是合并语义)。
    // 此前这里重建整个 payload,把表单没有的字段全部丢掉,一次改名就会顺带改掉任务
    // 的行为。这里只声明真正改了什么,未知字段留在服务端。
    // 授权不再走 payload:它是 ScheduledJob 的一等字段,只能通过 authorize_unattended
    // 这个显式标记签发,payload 里的同名键不再有任何作用。
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
    // 授权只对用户确认时看到的那份内容有效。之后改了指令/频率/投递目标,就不能顺着
    // 这次授权带出去——后端按请求里的新内容签指纹,那样签出来的是一份用户从没看过
    // 的有效授权。不一致就照常保存但不授权,同时打回开关并说明,由用户重新核对再勾。
    const consentStale = authorizeUnattended && consentDigest() !== authorizedSnapshot;
    if (consentStale) {
      setAuthorizeUnattended(false);
      setAuthorizedSnapshot("");
      toast.error(t("authorizeStaleForm"));
    }
    // Only send the flag when it is on. Sending `false` explicitly would be
    // equivalent, but omitting it keeps "absence is not consent" visible in the
    // wire format itself.
    const body = JSON.stringify(
      authorizeUnattended && !consentStale
        ? { name, cron_expr: expr, payload, authorize_unattended: true }
        : { name, cron_expr: expr, payload },
    );
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
          {/* htmlFor + aria-describedby rather than a wrapping label: the hint is
              a description, not part of the control's name. */}
          <div className="flex items-start gap-2 text-sm">
            <input
              id="cron-authorize-unattended"
              type="checkbox"
              checked={authorizeUnattended}
              onChange={(e) => void toggleAuthorize(e.target.checked)}
              aria-describedby="cron-authorize-hint"
              className="mt-0.5"
            />
            <div>
              <label htmlFor="cron-authorize-unattended">{t("authorizeUnattended")}</label>
              <span id="cron-authorize-hint" className="block text-xs text-gray-500">
                {t("authorizeHint")}
              </span>
            </div>
          </div>
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
                      <span
                        className={`ml-2 px-1.5 py-0.5 rounded text-xs ${AUTH_BADGE[authStateOf(job)]}`}
                        title={
                          authStateOf(job) === "stale"
                            ? t("authStaleHint")
                            : job.authorization
                              ? t("authGrantedBy", {
                                  operator: job.authorization.operator,
                                  time: dateTime(job.authorization.granted_at_ms),
                                })
                              : t("authorizeHint")
                        }
                      >
                        {t(`authState.${authStateOf(job)}`)}
                      </span>
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
