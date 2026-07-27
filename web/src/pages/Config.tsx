import { useTranslation } from "react-i18next";
import { useApi } from "../hooks/use-api";
import { useIsAdmin } from "../stores/capabilities";

export function Config() {
  const { t } = useTranslation(["config", "common"]);
  const isAdmin = useIsAdmin();
  // /config is admin-guarded end to end, so for an api-token session the fetch
  // is guaranteed to 403. Skip it and say why, instead of showing a generic
  // "load failed: 403" that reads like a server fault.
  //
  // Wait for the probe to settle (isAdmin === null) rather than fetching
  // optimistically: this is the one page whose *entire* content is admin-only,
  // so an optimistic first request would fire the 403 on every non-admin mount
  // — the exact thing the gate exists to avoid. Elsewhere `null` means "assume
  // allowed" because it only controls whether a button looks enabled.
  const { data, loading, error } = useApi<Record<string, unknown>>(
    isAdmin === true ? "/config" : null,
  );

  return (
    <div className="space-y-4">
      <h1 className="text-lg font-bold">{t("title")}</h1>
      {isAdmin === false ? (
        <div className="bg-amber-50 text-amber-700 rounded-lg p-4 text-sm">
          {t("common:adminOnly")}
        </div>
      ) : error ? (
        <div className="bg-red-50 text-red-600 rounded-lg p-4 text-sm">
          {t("common:loadFailed", { error })}
        </div>
      ) : (
        <pre className="bg-gray-900 text-green-300 rounded-lg p-4 text-xs overflow-auto max-h-[75vh]">
          {isAdmin === null || loading ? t("common:loading") : JSON.stringify(data, null, 2)}
        </pre>
      )}
    </div>
  );
}
