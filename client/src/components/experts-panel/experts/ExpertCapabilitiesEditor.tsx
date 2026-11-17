import "@/styles/expert-capabilities.css";
import { useContext, useEffect, useId, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { mcpList, skillsList } from "@/lib/agent-client";
import type { McpServerEntry, SkillInfo } from "@/lib/types";
import type { ExpertSettings } from "@/lib/expert-definition";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { ModelCatalogContext, ModelManagementContext } from "@/lib/model-management";
import { groupModelsBySource, modelChoiceLabel } from "@/lib/model-presentation";

const defaults: ExpertSettings = { model: "inherit", skills: [], tools: [], disallowedTools: [], discoverSkills: true, inheritSkills: true, mcpServers: [], mcpInheritance: "all" };
const lines = (value: string) => [...new Set(value.split(/\n|,/).map(v => v.trim()).filter(Boolean))];
interface Choice { id: string; name: string; description: string; unavailable?: string }
const sourceLabel = (scope?: string) => ({ user: "个人", local: "本机", repo: "工作区", project: "工作区", server: "组织", bundled: "内置", plugin: "插件" })[scope ?? ""] ?? scope ?? "本机";

function CapabilityChoices({ title, choices, selected, loading, error, onChange, onReload, locked = false, extra }: {
  title: string; choices: Choice[]; selected: string[]; loading: boolean; error: string; onChange: (ids: string[]) => void; onReload: () => void; locked?: boolean; extra?: ReactNode;
}) {
  const [open, setOpen] = useState(false), [query, setQuery] = useState("");
  const listId = useId();
  const known = new Set(choices.map(choice => choice.id));
  const all = [...choices, ...selected.filter(id => !known.has(id)).map(id => ({ id, name: id, description: "原配置已保留", unavailable: loading ? "正在检查" : error ? "暂时无法校验" : "未找到或已停用" }))];
  const toggle = (id: string) => onChange(selected.includes(id) ? selected.filter(value => value !== id) : [...selected, id]);
  return <div className="expert-capability-section">
    <div className="expert-capability-heading"><div><h4>{title}</h4><p>{title === "指定技能" ? "从已安装的技能中选择，按需使用对应的工作方法。" : "选择可使用的外部服务，账号授权由当前使用者管理。"}</p></div>
      <button type="button" className="capability-button" disabled={locked} aria-expanded={open} aria-controls={listId} onClick={() => setOpen(!open)}>选择{title === "指定技能" ? "技能" : "连接器"}</button></div>
    {extra}
    <div className="expert-capability-chips" aria-label={`已选${title}`}>
      {selected.length === 0 ? <span className="expert-capability-hint">{locked ? "包含高级连接配置，已完整保留" : "未单独指定"}</span> : selected.map(id => {
        const choice = all.find(item => item.id === id)!;
        return <span key={id} className={`expert-capability-chip${choice.unavailable ? " expert-capability-chip--unavailable" : ""}`}><span>{choice.name}{choice.unavailable && <small> · {choice.unavailable}</small>}</span><button type="button" aria-label={`移除${title === "指定技能" ? "技能" : "连接器"} ${choice.name}`} disabled={locked} onClick={() => toggle(id)}>×</button></span>;
      })}
    </div>
    {open && <div id={listId} className="expert-capability-picker">
      <div className="expert-capability-search"><input type="search" aria-label={`搜索${title}`} placeholder={`搜索${title === "指定技能" ? "技能名称或用途" : "连接器"}`} value={query} onChange={event => setQuery(event.target.value)} /><button type="button" className="capability-button" disabled={loading} onClick={onReload}>刷新</button></div>
      {loading && <p role="status">正在读取可用列表…</p>}{error && <p role="alert">{error}；原有选择不会被清空。</p>}
      {!loading && !error && !all.length && <p>暂无可选{title === "指定技能" ? "技能，请先在技能管理中安装或启用。" : "连接器，请先在连接器管理中添加并授权。"}</p>}
      {all.filter(choice => `${choice.name} ${choice.description}`.toLowerCase().includes(query.toLowerCase())).map(choice => <label className="expert-capability-choice" key={choice.id}>
        <input type="checkbox" checked={selected.includes(choice.id)} disabled={locked || (!!choice.unavailable && !selected.includes(choice.id))} onChange={() => toggle(choice.id)} />
        <span><strong>{choice.name}</strong><small>{choice.description}{choice.unavailable && ` · ${choice.unavailable}`}</small></span>
      </label>)}
      {query && all.length > 0 && !all.some(choice => `${choice.name} ${choice.description}`.toLowerCase().includes(query.toLowerCase())) && <p>没有匹配的结果。</p>}
    </div>}
  </div>;
}

export function ExpertCapabilitiesEditor({ raw, disabled, onChange, cwd }: { raw?: string; disabled: boolean; onChange: (value: ExpertSettings) => void; cwd?: string }) {
  const boundary = useOrgSessionStore(state => state.boundaryRevision);
  const [catalogBoundary, setCatalogBoundary] = useState(boundary);
  const models = useContext(ModelCatalogContext), manageModels = useContext(ModelManagementContext);
  const [settings, setSettings] = useState<ExpertSettings | null>(raw ? null : defaults);
  const [error, setError] = useState("");
  const [skills, setSkills] = useState<SkillInfo[]>([]), [connectors, setConnectors] = useState<McpServerEntry[]>([]);
  const [loading, setLoading] = useState(true), [skillError, setSkillError] = useState(""), [connectorError, setConnectorError] = useState("");
  const [revision, setRevision] = useState(0);
  const modelField = useId();
  useEffect(() => {
    let active = true;
    setError(""); setSettings(raw ? null : defaults);
    if (raw) void invoke<ExpertSettings>("agents_definition_settings", { raw }).then(value => { if (active) setSettings(value); }).catch(e => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, [raw]);
  useEffect(() => {
    let active = true; setCatalogBoundary(boundary); setLoading(true); setSkills([]); setConnectors([]); setSkillError(""); setConnectorError("");
    void Promise.allSettled([Promise.resolve().then(() => skillsList(cwd)), Promise.resolve().then(() => mcpList())]).then(([skillResult, connectorResult]) => {
      if (!active) return;
      if (skillResult.status === "fulfilled" && Array.isArray(skillResult.value)) setSkills(skillResult.value); else setSkillError("技能列表读取失败，请刷新重试");
      if (connectorResult.status === "fulfilled" && Array.isArray(connectorResult.value)) setConnectors(connectorResult.value); else setConnectorError("连接器列表读取失败，请刷新重试");
      setLoading(false);
    });
    return () => { active = false; };
  }, [cwd, revision, boundary]);
  if (error) return <p role="alert">能力配置读取失败：{error}。原配置将保留，请重新打开后重试。</p>;
  if (!settings) return <p role="status">正在读取专家能力…</p>;
  const change = <K extends keyof ExpertSettings>(key: K, value: ExpertSettings[K]) => { const next = { ...settings, [key]: value }; setSettings(next); onChange(next); };
  // Old definitions may use a path. Preserve its exact value rather than
  // rewriting references when a user merely edits another field.
  const selectedSkill = (skill: SkillInfo) => settings.skills.includes(skill.path ?? "") ? skill.path! : skill.name;
  const skillChoices: Choice[] = (catalogBoundary === boundary ? skills : []).map(skill => ({ id: selectedSkill(skill), name: skill.displayName ?? skill.name, description: [skill.description || skill.whenToUse || "技能工作方法", sourceLabel(skill.scope)].join(" · "), unavailable: !skill.enabled ? "已停用" : skill.capability && !skill.capability.ready ? "依赖或配置未就绪" : undefined }));
  const connectorChoices: Choice[] = (catalogBoundary === boundary ? connectors : []).map(connector => ({ id: connector.name, name: connector.displayName ?? connector.name, description: [connector.vendor, sourceLabel(connector.source), connector.authRequired ? "待授权" : connector.setupRequired ? "待配置" : connector.status === "ready" || connector.live ? "已连接" : "已配置"].filter(Boolean).join(" · "), unavailable: !connector.enabled ? "已停用" : connector.authRequired ? "待授权" : connector.setupRequired ? "待配置" : undefined }));
  const unavailableModel = settings.model !== "inherit" && !models.some(model => model.id === settings.model);
  const inheritance = typeof settings.mcpInheritance === "string" ? settings.mcpInheritance : "custom";
  return <fieldset className="expert-capabilities" disabled={disabled}>
    <legend>模型、技能与连接器</legend>
    <div className="expert-capability-section"><div className="expert-capability-heading"><div><label htmlFor={modelField}>默认模型</label><p>为此专家指定默认模型，本次任务也可明确选择覆盖。</p></div>{manageModels && <button type="button" className="expert-capability-link" onClick={() => manageModels()}>管理模型</button>}</div>
      <select id={modelField} aria-label="专家默认模型" value={settings.model} onChange={event => change("model", event.target.value)}>
        <option value="inherit">跟随当前任务模型（推荐）</option>
        {unavailableModel && <option value={settings.model}>{settings.model} · 原配置已保留，当前不可用</option>}
        {groupModelsBySource(models).map(group => <optgroup label={group.label} key={group.label}>{group.models.map(model => <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>)}</optgroup>)}
      </select>
      {unavailableModel && <p className="expert-capability-warning" role="status">原模型当前不在可用列表中，请更换模型或检查连接；保存其他设置不会移除它。</p>}
      {!models.length && <p className="expert-capability-hint">暂无已配置模型，可先跟随任务模型，或前往模型设置添加。</p>}
    </div>
    <CapabilityChoices title="指定技能" choices={[...new Map(skillChoices.map(choice => [choice.id, choice])).values()]} selected={settings.skills} loading={loading} error={skillError} onReload={() => setRevision(value => value + 1)} onChange={value => change("skills", value)} />
    <div className="expert-capability-section">
      <CapabilityChoices title="外部连接器" choices={connectorChoices} selected={settings.mcpServers ?? []} locked={settings.mcpServers === null} loading={loading} error={connectorError} onReload={() => setRevision(value => value + 1)} onChange={value => change("mcpServers", value)} extra={<label className="expert-capability-field">使用范围<select aria-label="连接器继承" value={inheritance} onChange={event => { if (event.target.value === "all" || event.target.value === "none") change("mcpInheritance", event.target.value); }}>
        <option value="all">跟随任务授权，并包含指定项</option><option value="none">仅使用指定连接器</option>{inheritance === "custom" && <option value="custom">保留原有自定义范围</option>}
      </select></label>} />
      {settings.mcpServers === null && <p className="expert-capability-hint">此专家包含内联连接配置，原配置完整保留；当前选择器只管理命名连接器。</p>}
      {inheritance === "none" && settings.mcpServers?.length === 0 && <p className="expert-capability-hint">不使用外部连接器。项目知识、文档等应用内置服务仍按任务权限提供。</p>}
    </div>
    <details className="expert-capability-advanced"><summary>高级设置 · 技能来源与工具限制</summary><div>
      <label className="expert-capability-choice"><input type="checkbox" checked={settings.discoverSkills} onChange={event => change("discoverSkills", event.target.checked)} /><span>发现当前工作区的技能<small>运行时还可以读取任务目录中的技能。</small></span></label>
      <label className="expert-capability-choice"><input type="checkbox" checked={settings.inheritSkills} onChange={event => change("inheritSkills", event.target.checked)} /><span>加入专家团时，使用上级提供的技能</span></label>
      <label className="expert-capability-field">允许工具<textarea aria-label="专家允许工具" defaultValue={settings.tools.join("\n")} onChange={event => change("tools", lines(event.target.value))} /><small>每行一个工具名称；留空表示沿用任务可用工具。</small></label>
      <label className="expert-capability-field">禁止工具<textarea aria-label="专家禁止工具" defaultValue={settings.disallowedTools.join("\n")} onChange={event => change("disallowedTools", lines(event.target.value))} /><small>禁止规则优先，所有工具仍受本次任务权限约束。</small></label>
    </div></details>
    <p className="expert-capability-summary">指定 {settings.skills.length} 项技能{settings.discoverSkills && " · 允许工作区发现"}{settings.inheritSkills && " · 允许上级继承"}；外部连接器：{inheritance === "all" ? "跟随任务授权范围" : inheritance === "custom" ? "保留自定义范围" : settings.mcpServers === null ? "保留高级配置" : `${settings.mcpServers.length} 个指定项`}。账号凭证不会保存在专家中。</p>
  </fieldset>;
}
