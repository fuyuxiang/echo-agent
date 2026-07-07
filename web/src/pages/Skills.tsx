import { useApi } from "../hooks/use-api";
import { apiFetch } from "../lib/api";
import { Zap } from "lucide-react";

interface Skill {
  name: string;
  description: string;
  enabled: boolean;
}

export function Skills() {
  const { data, refetch } = useApi<{ skills: Skill[] }>("/skills");

  const toggle = async (name: string) => {
    await apiFetch(`/skills/${name}/toggle`, { method: "POST" });
    refetch();
  };

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
      {data?.skills.map((skill) => (
        <div key={skill.name} className="bg-white border rounded-lg p-4">
          <div className="flex items-center justify-between mb-2">
            <div className="flex items-center gap-2">
              <Zap size={16} className="text-yellow-500" />
              <span className="font-medium text-sm">{skill.name}</span>
            </div>
            <button
              onClick={() => toggle(skill.name)}
              className={`w-10 h-5 rounded-full transition-colors ${skill.enabled ? "bg-blue-600" : "bg-gray-300"}`}
            >
              <div className={`w-4 h-4 bg-white rounded-full shadow transition-transform ${skill.enabled ? "translate-x-5" : "translate-x-0.5"}`} />
            </button>
          </div>
          <p className="text-xs text-gray-500">{skill.description || "无描述"}</p>
        </div>
      ))}
    </div>
  );
}
