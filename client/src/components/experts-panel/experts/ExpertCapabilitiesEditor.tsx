import "@/styles/team-projects.css";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ExpertSettings } from "@/lib/expert-definition";

const defaults: ExpertSettings = { model: "inherit", skills: [], tools: [], disallowedTools: [], discoverSkills: true, inheritSkills: true, mcpServers: [], mcpInheritance: "all" };
const lines = (value: string) => [...new Set(value.split(/\n|,/).map(v => v.trim()).filter(Boolean))];

export function ExpertCapabilitiesEditor({ raw, disabled, onChange }: { raw?: string; disabled: boolean; onChange: (value: ExpertSettings) => void }) {
  const [settings, setSettings] = useState<ExpertSettings | null>(raw ? null : defaults);
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    setError("");
    setSettings(raw ? null : defaults);
    if (raw) void invoke<ExpertSettings>("agents_definition_settings", { raw }).then(value => { if (active) setSettings(value); }).catch(e => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, [raw]);
  if (error) return <p role="alert">能力配置读取失败：{error}</p>;
  if (!settings) return <p role="status">正在读取专家能力…</p>;
  const change = <K extends keyof ExpertSettings>(key: K, value: ExpertSettings[K]) => {
    const next = { ...settings, [key]: value }; setSettings(next); onChange(next);
  };
  return <fieldset className="expert-capabilities" disabled={disabled}>
    <legend>运行能力</legend>
    <p>单独使用与加入专家团使用同一能力定义。账号授权由个人或组织管理。</p>
    <label>默认模型<input aria-label="专家默认模型" value={settings.model === "inherit" ? "" : settings.model} placeholder="跟随项目或本次默认模型；也可填写模型 ID" onChange={e => change("model", e.target.value.trim() || "inherit")} /></label>
    <label>技能名称（每行一项）<textarea aria-label="专家技能" defaultValue={settings.skills.join("\n")} onChange={e => change("skills", lines(e.target.value))} /></label>
    <label><input type="checkbox" checked={settings.discoverSkills} onChange={e => change("discoverSkills", e.target.checked)} />自动发现工作目录中的技能</label>
    <label><input type="checkbox" checked={settings.inheritSkills} onChange={e => change("inheritSkills", e.target.checked)} />作为子专家时继承上级技能</label>
    <label>连接器引用（每行一项）<textarea aria-label="专家连接器" disabled={settings.mcpServers === null} defaultValue={settings.mcpServers?.join("\n") ?? "包含内联连接配置，原配置将保留"} onChange={e => change("mcpServers", lines(e.target.value))} /></label>
    <label>连接器继承<select aria-label="连接器继承" value={typeof settings.mcpInheritance === "string" ? settings.mcpInheritance : "custom"} onChange={e => change("mcpInheritance", e.target.value as "all" | "none")}>
      <option value="all">继承已授权连接器</option><option value="none">仅使用专家声明的连接器</option>{typeof settings.mcpInheritance !== "string" && <option value="custom">保留原有范围规则</option>}
    </select></label>
    <label>允许工具（留空表示继承可用工具）<textarea aria-label="专家允许工具" defaultValue={settings.tools.join("\n")} onChange={e => change("tools", lines(e.target.value))} /></label>
    <label>禁止工具<textarea aria-label="专家禁止工具" defaultValue={settings.disallowedTools.join("\n")} onChange={e => change("disallowedTools", lines(e.target.value))} /></label>
    <p>连接器范围适用于单独使用和专家团。项目知识、协作和文档等应用内置服务由宿主管理；工具仍受本次权限和组织政策限制。这里不保存密钥。</p>
  </fieldset>;
}
