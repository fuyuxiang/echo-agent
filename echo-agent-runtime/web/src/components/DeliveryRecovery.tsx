import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useWsSubscribe } from "../hooks/use-ws";
import { apiDownload, apiFetch } from "../lib/api";
import { runMutation } from "../stores/toast";
import { useConfirm } from "./ConfirmDialog";
import { Loadable } from "./Loadable";

interface Delivery {
  event_id: string; channel: string; chat_id: string; status: string; preview: string;
  attachments: number; confirmed_parts: number; unknown_parts: number;
  error?: string; recovery_action?: string; can_retry?: boolean; auto_retry?: boolean;
  created_at?: number;
}

function DeliveryContent({ id }: { id: string }) {
  const { t } = useTranslation("channels");
  const [open, setOpen] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const { data, loading, error } = useApi<{ text: string; files: string[]; downloadable_files?: number[] }>(open ? `/deliveries/${encodeURIComponent(id)}` : null);
  const download = async (file: string, index: number) => {
    setDownloading(true);
    try {
      await runMutation(async () => {
        const blob = await apiDownload(`/deliveries/${encodeURIComponent(id)}/files/${index}`);
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url; link.download = file;
        document.body.appendChild(link); link.click(); link.remove();
        window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      }, { error: t("delivery.downloadFailed") });
    } finally { setDownloading(false); }
  };
  return <details onToggle={event => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer text-sm text-blue-700">{t("delivery.viewFull")}</summary>
    <Loadable loading={loading} error={error} data={data}>{message => <div className="space-y-2 py-2">
      <pre className="whitespace-pre-wrap break-words text-sm max-h-64 overflow-auto">{message.text}</pre>
      {message.files.map((file, index) => <p className="text-sm break-all" key={index}>
        {file}{message.downloadable_files?.includes(index) && <button className="ml-2 text-blue-700 underline disabled:opacity-40"
          disabled={downloading} onClick={() => void download(file, index)}>{t("delivery.download")}</button>}
      </p>)}
    </div>}</Loadable>
  </details>;
}

export function DeliveryRecovery({ scheduledOnly = false }: { scheduledOnly?: boolean }) {
  const { t } = useTranslation("channels");
  const { data, loading, error, refetch } = useApi<{ messages: Delivery[]; durable: boolean }>(scheduledOnly ? "/deliveries?scheduled=true" : "/deliveries");
  const [inflight, setInflight] = useState<string | null>(null);
  const confirm = useConfirm();
  useWsSubscribe(["cron", "channels"], () => { void refetch(); }, ["cron_run", "channel_updated"]);
  useEffect(() => {
    if (!data?.messages.some(message => message.auto_retry || ["pending", "retrying"].includes(message.status))) return;
    const interval = window.setInterval(() => { void refetch(); }, 15000);
    return () => window.clearInterval(interval);
  }, [data, refetch]);
  const recover = async (message: Delivery, action: "retry" | "confirm_delivery") => {
    if (inflight !== null) return;
    const unknown = message.unknown_parts > 0;
    if ((unknown || action === "confirm_delivery") && !await confirm({
      title: t(action === "retry" ? "delivery.retryTitle" : "delivery.confirmTitle"),
      message: t(action === "retry" ? "delivery.retryWarning" : "delivery.confirmWarning"),
      confirmLabel: t(action === "retry" ? "delivery.retryChecked" : "delivery.confirmChecked"),
      destructive: true,
    })) return;
    setInflight(message.event_id);
    await runMutation(() => apiFetch(`/deliveries/${encodeURIComponent(message.event_id)}/recover`, {
      method: "POST", body: JSON.stringify({ action, confirmed_not_delivered: unknown,
        confirmed_delivered: action === "confirm_delivery" }),
    }), { success: t("delivery.success"), error: t("delivery.failed") });
    setInflight(null);
    refetch();
  };
  return <section className="space-y-2 border rounded-lg bg-white p-4" aria-label={t("delivery.title")}>
    <div className="flex justify-between items-center">
      <h2 className="font-semibold">{t("delivery.title")}</h2>
      <button className="text-sm border rounded px-2 py-1" onClick={() => refetch()} disabled={loading || inflight !== null}>
        {t("delivery.refresh")}
      </button>
    </div>
    <p className="text-sm text-gray-600">{t("delivery.description")}</p>
    <Loadable loading={loading} error={error} data={data} isEmpty={d => d.messages.length === 0}
      emptyText={t("delivery.empty")}>
      {d => <div className="space-y-3">{d.messages.map(message => <article key={message.event_id} className="border rounded p-3 space-y-2">
        <p className="text-sm font-medium break-all">{message.channel} · {message.chat_id}</p>
        <p className="whitespace-pre-wrap break-words text-sm max-h-32 overflow-auto">{message.preview}</p>
        <DeliveryContent id={message.event_id} />
        <p className="text-sm text-gray-600">{t("delivery.parts", { count: message.confirmed_parts, files: message.attachments })}</p>
        <p className="text-sm">{t(message.recovery_action ? "delivery.waitingContext" : message.status === "pending" || message.status === "retrying" ? "delivery.sending" :
          message.unknown_parts ? "delivery.unknown" : "delivery.unsent")}</p>
        {message.recovery_action && <div role="status" className="rounded border border-amber-200 bg-amber-50 p-3 text-sm text-amber-900">
          <p>{t(message.recovery_action === "relogin" ? "delivery.relogin" : "delivery.sendMessage")}</p>
          {message.auto_retry && <p className="mt-1">{t("delivery.autoRetry")}</p>}
        </div>}
        {message.error && <details className="text-xs text-gray-500">
          <summary className="cursor-pointer">{t("delivery.errorDetails")}</summary>
          <p className="whitespace-pre-wrap break-words mt-1">{message.error}</p>
        </details>}
        <div className="flex flex-wrap gap-2">
          <button disabled={inflight !== null || message.can_retry === false || ["pending", "retrying"].includes(message.status)} className="border rounded px-3 py-1 disabled:opacity-40"
            onClick={() => recover(message, "retry")}>{t(inflight === message.event_id ? "delivery.sending" : "delivery.retry")}</button>
          {message.unknown_parts > 0 && <button disabled={inflight !== null || ["pending", "retrying"].includes(message.status)} className="border rounded px-3 py-1 disabled:opacity-40"
            onClick={() => recover(message, "confirm_delivery")}>{t("delivery.confirm")}</button>}
        </div>
      </article>)}</div>}
    </Loadable>
  </section>;
}
