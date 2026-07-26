import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { runMutation } from "../stores/toast";
import { Loadable } from "../components/Loadable";
import { Zap } from "lucide-react";
import { useTranslation } from "react-i18next";

interface Skill {
  name: string;
  description: string;
  enabled: boolean;
}

export function Skills() {
  const { t } = useTranslation("skills");
  const { data, loading, error, refetch } = useApi<{ skills: Skill[] }>("/skills");

  const toggle = async (name: string) => {
    const ok = await runMutation(() => apiFetch(`/skills/${name}/toggle`, { method: "POST" }), {
      error: t("toggleFailed"),
    });
    if (ok) refetch();
  };

  return (
    <Loadable
      loading={loading}
      error={error}
      data={data}
      isEmpty={(d) => d.skills.length === 0}
      emptyText={t("empty")}
    >
      {(d) => (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {d.skills.map((skill) => (
            <div key={skill.name} className="bg-white border rounded-lg p-4">
              <div className="flex items-center justify-between mb-2">
                <div className="flex items-center gap-2">
                  <Zap size={16} className="text-yellow-500" />
                  <span className="font-medium text-sm">{skill.name}</span>
                </div>
                <button
                  role="switch"
                  aria-checked={skill.enabled}
                  aria-label={t(skill.enabled ? "disableAria" : "enableAria", { name: skill.name })}
                  onClick={() => toggle(skill.name)}
                  className={`w-10 h-5 rounded-full transition-colors shrink-0 ${skill.enabled ? "bg-blue-600" : "bg-gray-300"}`}
                >
                  <div className={`w-4 h-4 bg-white rounded-full shadow transition-transform ${skill.enabled ? "translate-x-5" : "translate-x-0.5"}`} />
                </button>
              </div>
              <p className="text-xs text-gray-500">{skill.description || t("noDescription")}</p>
            </div>
          ))}
        </div>
      )}
    </Loadable>
  );
}
