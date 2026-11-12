import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import { useApi } from "../hooks/use-api";
import { dateTime } from "../lib/datetime";
import { apiDownload } from "../lib/api";
import { runMutation } from "../stores/toast";
import { useWsSubscribe } from "../hooks/use-ws";
import { useIsAdmin } from "../stores/capabilities";
import type { CronJob } from "../pages/Cron";

interface CronRun {
  run_id?: string;
  ts: number;
  status: string;
  error: string | null;
  run_count: number;
  delivery_status?: string;
  recovered_ts?: number;
  generation_status?: string;
  report_file?: string;
  report_title?: string;
  archive_error?: string;
  deliveries?: { recovery_action?: string; retry_at_ms?: number }[];
}

/**
 * Run history for one scheduled job, from GET /cron/{id}/runs.
 *
 * The endpoint existed with no caller: the table's "last result" cell showed a
 * status string but never the error text behind a failure, so a job that had
 * been failing all week looked identical to one that had never run.
 *
 * History is persisted with the job and bounded to the latest 100 outcomes.
 */
export function CronRunsDrawer({ job, onClose }: { job: CronJob; onClose: () => void }) {
  const { t } = useTranslation(["cron", "common"]);
  const canDownload = useIsAdmin() !== false;
  const [downloading, setDownloading] = useState(false);
  const { data, loading, error, refetch } = useApi<{ runs: CronRun[] }>(`/cron/${job.id}/runs?limit=100`);
  useWsSubscribe(["cron"], () => { void refetch(); }, ["cron_run"]);

  const download = async (run: CronRun) => {
    if (downloading || !canDownload) return;
    setDownloading(true);
    try { await runMutation(async () => {
    const blob = await apiDownload(`/cron/${encodeURIComponent(job.id)}/runs/${encodeURIComponent(run.run_id!)}/report`);
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url; link.download = run.report_file!.split("/").pop()!;
    document.body.appendChild(link); link.click(); link.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    }, { error: t("reportDownloadFailed") }); }
    finally { setDownloading(false); }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const runs = data?.runs ?? [];

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/20" onClick={onClose}>
      <aside
        role="dialog"
        aria-modal="true"
        aria-labelledby="cron-runs-title"
        className="bg-white w-full max-w-md h-full overflow-y-auto shadow-xl p-5 space-y-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-3">
          <div className="flex-1 min-w-0">
            <h2 id="cron-runs-title" className="font-semibold text-base">{t("runs")}</h2>
            <p className="text-xs text-gray-500 break-all">{job.name || job.id}</p>
          </div>
          <button
            onClick={onClose}
            aria-label={t("common:close")}
            className="p-1 rounded hover:bg-gray-100 shrink-0"
          >
            <X size={18} />
          </button>
        </div>

        {error && (
          <div className="text-xs text-red-600 bg-red-50 border border-red-200 rounded p-2">
            {t("common:loadFailed", { error })}
          </div>
        )}

        {loading ? (
          <p className="text-sm text-gray-400">{t("common:loading")}</p>
        ) : runs.length === 0 ? (
          <p className="text-sm text-gray-400">{t("runsEmpty")}</p>
        ) : (
          <ul className="space-y-3">
            {runs.map((run) => (
              <li key={`${run.ts}-${run.status}`} className="border rounded p-3 space-y-1">
                <div className="flex justify-between gap-2 text-xs">
                  <span className="text-gray-500">{t("runTime")}</span>
                  <span className="font-mono">{dateTime(run.ts)}</span>
                </div>
                <div className="flex justify-between gap-2 text-xs">
                  <span className="text-gray-500">{t("runStatus")}</span>
                  <span>{t(`outcome.${run.status}`, { defaultValue: run.status })}</span>
                </div>
                <div className="text-xs text-gray-500">
                  {t("runCount", { count: run.run_count })}
                </div>
                {run.generation_status && <p className="text-xs text-gray-600">
                  {t("generationStatus")}: {t(`generationOutcome.${run.generation_status}`, { defaultValue: run.generation_status })}
                </p>}
                {run.status === "delivery_pending" && <div role="status" className="text-xs rounded bg-amber-50 p-2 text-amber-900">
                  {t(run.deliveries?.some(d => d.recovery_action === "retry_later") ? "automaticRecoveryHint" :
                    run.deliveries?.some(d => d.recovery_action === "relogin") ? "restoreLoginHint" : "restoreMessageHint")}
                  {run.deliveries?.find(d => d.retry_at_ms)?.retry_at_ms ? <p className="mt-1">
                    {t("retryAt", { time: dateTime(run.deliveries.find(d => d.retry_at_ms)!.retry_at_ms!) })}
                  </p> : null}
                </div>}
                {run.generation_status === "incomplete" && <p className="text-xs text-amber-800">{t("draftSavedHint")}</p>}
                {run.error && <details className="text-xs whitespace-pre-wrap break-words text-gray-600">
                  <summary className="cursor-pointer">{t("runError")}</summary>{run.error}
                </details>}
                {run.archive_error && <p className="text-xs text-amber-800">{t("archiveFailedHint")}</p>}
                {run.report_file && run.run_id && <button className="text-xs text-blue-700 underline"
                  disabled={downloading || !canDownload} title={canDownload ? undefined : t("common:adminOnly")}
                  onClick={() => void download(run)}>{t("downloadReport")}</button>}
                {run.delivery_status && <p className="text-xs text-gray-600">
                  {t("deliveryStatus")}: {t(`deliveryOutcome.${run.delivery_status}`, { defaultValue: run.delivery_status })}
                </p>}
                {run.recovered_ts && <p className="text-xs text-green-700">{t("recoveredAt", { time: dateTime(run.recovered_ts) })}</p>}
              </li>
            ))}
          </ul>
        )}

        <p className="text-xs text-gray-400 border-t pt-3">{t("runsRetention")}</p>
      </aside>
    </div>
  );
}
