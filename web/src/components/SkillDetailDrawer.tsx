import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { X, Download } from "lucide-react";
import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { runMutation } from "../stores/toast";

interface SkillDetail {
  name: string;
  content: string;
  files: string[];
}

interface SkillDeps {
  name: string;
  requires: string[];
  missing: string[];
  satisfied: boolean;
}

/**
 * Right-hand drawer with a skill's SKILL.md, file list and pip dependency state.
 *
 * GET /skills/{name} and /skills/{name}/deps have always existed but had no
 * surface at all: the page only listed name/description/enabled. A skill whose
 * declared pip deps were missing would just fail at call time inside the Agent,
 * with the install endpoint reachable only by hand-crafting a request.
 */
export function SkillDetailDrawer({
  name,
  canAdmin,
  onClose,
}: {
  name: string;
  canAdmin: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation(["skills", "common"]);
  const { data, loading, error } = useApi<SkillDetail>(`/skills/${encodeURIComponent(name)}`);
  const {
    data: deps,
    refetch: refetchDeps,
  } = useApi<SkillDeps>(`/skills/${encodeURIComponent(name)}/deps`);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const install = async () => {
    const ok = await runMutation(
      () => apiFetch(`/skills/${encodeURIComponent(name)}/deps/install`, { method: "POST" }),
      { success: t("installSuccess"), error: t("installFailed") },
    );
    if (ok) refetchDeps();
  };

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/20" onClick={onClose}>
      <aside
        role="dialog"
        aria-modal="true"
        aria-labelledby="skill-detail-title"
        className="bg-white w-full max-w-lg h-full overflow-y-auto shadow-xl p-5 space-y-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-3">
          <h2 id="skill-detail-title" className="font-semibold text-base flex-1 break-all">{name}</h2>
          <button
            onClick={onClose}
            aria-label={t("common:close")}
            className="p-1 rounded hover:bg-gray-100 shrink-0"
          >
            <X size={18} />
          </button>
        </div>

        {deps && (
          <section className="space-y-2">
            <h3 className="text-xs font-medium text-gray-500">{t("deps")}</h3>
            {deps.requires.length === 0 ? (
              <p className="text-xs text-gray-400">{t("depsNone")}</p>
            ) : (
              <>
                <ul className="text-xs font-mono space-y-0.5">
                  {deps.requires.map((spec) => (
                    <li
                      key={spec}
                      className={deps.missing.includes(spec) ? "text-amber-700" : "text-gray-600"}
                    >
                      {spec}
                    </li>
                  ))}
                </ul>
                {deps.satisfied ? (
                  <p className="text-xs text-green-600">{t("depsSatisfied")}</p>
                ) : (
                  <div className="flex items-center gap-2">
                    <span className="text-xs text-amber-700">
                      {t("depsMissing", { count: deps.missing.length })}
                    </span>
                    <button
                      onClick={install}
                      disabled={!canAdmin}
                      title={canAdmin ? undefined : t("common:adminOnly")}
                      className="flex items-center gap-1 bg-blue-600 text-white px-2 py-1 rounded text-xs disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <Download size={12} /> {t("installDeps")}
                    </button>
                  </div>
                )}
              </>
            )}
          </section>
        )}

        {error && (
          <div className="text-xs text-red-600 bg-red-50 border border-red-200 rounded p-2">
            {t("loadDetailFailed", { error })}
          </div>
        )}

        {data && data.files.length > 0 && (
          <section>
            <h3 className="text-xs font-medium text-gray-500 mb-1">
              {t("files", { count: data.files.length })}
            </h3>
            <ul className="text-xs font-mono text-gray-600 space-y-0.5">
              {data.files.map((f) => <li key={f} className="break-all">{f}</li>)}
            </ul>
          </section>
        )}

        <section>
          <h3 className="text-xs font-medium text-gray-500 mb-1">{t("content")}</h3>
          <pre className="bg-gray-50 text-gray-800 rounded p-3 text-xs whitespace-pre-wrap break-words max-h-[55vh] overflow-auto">
            {loading ? t("common:loading") : data?.content ?? ""}
          </pre>
        </section>
      </aside>
    </div>
  );
}
