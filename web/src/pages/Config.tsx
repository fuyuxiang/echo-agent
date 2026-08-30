import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Save, RotateCw } from "lucide-react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { useIsAdmin } from "../stores/capabilities";
import { runMutation } from "../stores/toast";

interface ConfigResponse {
  ui?: { locale?: string };
  observability?: { log_level?: string; trace_enabled?: boolean };
  memory?: { enabled?: boolean };
  knowledge?: { enabled?: boolean; auto_index?: boolean };
  cost?: { enabled?: boolean; daily_budget_usd?: number; soft_threshold_ratio?: number };
  _meta?: { config_path?: string; editable_roots?: string[] };
  [key: string]: unknown;
}

interface Draft {
  locale: string; logLevel: string; traceEnabled: boolean; memoryEnabled: boolean;
  knowledgeEnabled: boolean; autoIndex: boolean; costEnabled: boolean;
  dailyBudget: string; softThreshold: string;
}

const EMPTY: Draft = {
  locale: "auto", logLevel: "INFO", traceEnabled: true, memoryEnabled: true,
  knowledgeEnabled: true, autoIndex: true, costEnabled: false,
  dailyBudget: "0", softThreshold: "0.8",
};

export function Config() {
  const { t } = useTranslation(["config", "common"]);
  const isAdmin = useIsAdmin();
  const { data, loading, error, refetch } = useApi<ConfigResponse>(isAdmin === true ? "/config" : null);
  const [draft, setDraft] = useState<Draft>(EMPTY);
  const [restartRequired, setRestartRequired] = useState(false);

  useEffect(() => {
    if (!data) return;
    setDraft({
      locale: data.ui?.locale ?? "auto",
      logLevel: data.observability?.log_level ?? "INFO",
      traceEnabled: data.observability?.trace_enabled ?? true,
      memoryEnabled: data.memory?.enabled ?? true,
      knowledgeEnabled: data.knowledge?.enabled ?? true,
      autoIndex: data.knowledge?.auto_index ?? true,
      costEnabled: data.cost?.enabled ?? false,
      dailyBudget: String(data.cost?.daily_budget_usd ?? 0),
      softThreshold: String(data.cost?.soft_threshold_ratio ?? 0.8),
    });
  }, [data]);

  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((current) => ({ ...current, [key]: value }));

  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    const ok = await runMutation(
      () => apiFetch("/config", {
        method: "PATCH",
        body: JSON.stringify({ changes: {
          "ui.locale": draft.locale,
          "observability.log_level": draft.logLevel,
          "observability.trace_enabled": draft.traceEnabled,
          "memory.enabled": draft.memoryEnabled,
          "knowledge.enabled": draft.knowledgeEnabled,
          "knowledge.auto_index": draft.autoIndex,
          "cost.enabled": draft.costEnabled,
          "cost.daily_budget_usd": Number(draft.dailyBudget),
          "cost.soft_threshold_ratio": Number(draft.softThreshold),
        } }),
      }),
      { success: t("saveSuccess"), error: t("saveFailed") },
    );
    if (ok) { setRestartRequired(true); refetch(); }
  };

  if (isAdmin === false) return <div className="bg-amber-50 text-amber-700 rounded-lg p-4 text-sm">{t("common:adminOnly")}</div>;
  if (error) return <div className="bg-red-50 text-red-600 rounded-lg p-4 text-sm">{t("common:loadFailed", { error })}</div>;
  if (isAdmin === null || loading || !data) return <div className="text-gray-400 text-sm">{t("common:loading")}</div>;

  return <div className="space-y-4 max-w-4xl">
    <div className="flex items-start justify-between gap-3">
      <div><h1 className="text-lg font-bold">{t("title")}</h1>
        <p className="text-xs text-gray-500 font-mono break-all">{data._meta?.config_path}</p></div>
    </div>
    {restartRequired && <div className="flex items-start gap-2 bg-amber-50 border border-amber-200 text-amber-800 rounded-lg p-3 text-sm" role="status">
      <RotateCw size={16} className="mt-0.5 shrink-0" /><span>{t("restartRequired")}</span>
    </div>}
    <form onSubmit={save} className="space-y-4">
      <Section title={t("sections.interface")}>
        <Select label={t("fields.locale")} value={draft.locale} onChange={(value) => set("locale", value)} options={["auto", "zh", "en"]} />
        <Select label={t("fields.logLevel")} value={draft.logLevel} onChange={(value) => set("logLevel", value)} options={["DEBUG", "INFO", "WARNING", "ERROR"]} />
        <Toggle label={t("fields.traceEnabled")} checked={draft.traceEnabled} onChange={(value) => set("traceEnabled", value)} />
      </Section>
      <Section title={t("sections.capabilities")}>
        <Toggle label={t("fields.memoryEnabled")} checked={draft.memoryEnabled} onChange={(value) => set("memoryEnabled", value)} />
        <Toggle label={t("fields.knowledgeEnabled")} checked={draft.knowledgeEnabled} onChange={(value) => set("knowledgeEnabled", value)} />
        <Toggle label={t("fields.autoIndex")} checked={draft.autoIndex} onChange={(value) => set("autoIndex", value)} />
      </Section>
      <Section title={t("sections.cost")}>
        <Toggle label={t("fields.costEnabled")} checked={draft.costEnabled} onChange={(value) => set("costEnabled", value)} />
        <NumberField label={t("fields.dailyBudget")} value={draft.dailyBudget} min="0" step="0.01" onChange={(value) => set("dailyBudget", value)} />
        <NumberField label={t("fields.softThreshold")} value={draft.softThreshold} min="0" max="1" step="0.05" onChange={(value) => set("softThreshold", value)} />
      </Section>
      <button type="submit" className="flex items-center gap-1.5 bg-blue-600 text-white rounded px-4 py-2 text-sm hover:bg-blue-700">
        <Save size={15} /> {t("save")}
      </button>
    </form>
    <details className="border rounded-lg bg-white">
      <summary className="cursor-pointer p-3 text-sm text-gray-600">{t("advancedReadonly")}</summary>
      <pre className="border-t bg-gray-950 text-green-300 p-4 text-xs overflow-auto max-h-96">{JSON.stringify(data, null, 2)}</pre>
    </details>
  </div>;
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return <fieldset className="bg-white border rounded-lg p-4 grid grid-cols-1 sm:grid-cols-2 gap-4">
    <legend className="font-medium text-sm px-1">{title}</legend>{children}
  </fieldset>;
}
function Toggle({ label, checked, onChange }: { label: string; checked: boolean; onChange: (value: boolean) => void }) {
  return <label className="flex items-center justify-between gap-3 text-sm"><span>{label}</span>
    <button type="button" role="switch" aria-checked={checked} onClick={() => onChange(!checked)}
      className={`w-10 h-5 rounded-full ${checked ? "bg-blue-600" : "bg-gray-300"}`}>
      <span className={`block w-4 h-4 bg-white rounded-full shadow transition-transform ${checked ? "translate-x-5" : "translate-x-0.5"}`} />
    </button></label>;
}
function Select({ label, value, options, onChange }: { label: string; value: string; options: string[]; onChange: (value: string) => void }) {
  return <label className="text-sm"><span className="block text-gray-600 mb-1">{label}</span>
    <select value={value} onChange={(event) => onChange(event.target.value)} className="border rounded px-2 py-1.5 w-full">{options.map((option) => <option key={option}>{option}</option>)}</select>
  </label>;
}
function NumberField({ label, value, onChange, ...props }: { label: string; value: string; onChange: (value: string) => void; min?: string; max?: string; step?: string }) {
  return <label className="text-sm"><span className="block text-gray-600 mb-1">{label}</span>
    <input type="number" value={value} onChange={(event) => onChange(event.target.value)} {...props} className="border rounded px-2 py-1.5 w-full" />
  </label>;
}
