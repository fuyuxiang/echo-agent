import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowLeft, Plus, Users, Search } from "lucide-react";
import { ResourceIcon, ResourceList, ResourceRow } from "@/components/ResourceList";
import { Button } from "@/components/Button";
import { CapabilityPage, CapabilityEmptyState, CapabilityError } from "@/components/experts-panel/shared/CapabilityPage";
import { WorkspacePicker } from "@/components/WorkspacePicker";
import { modelChoiceLabel, findModelDisplayName } from "@/lib/model-presentation";
import { agentsList, type WorkspaceInfo } from "@/lib/agent-client";
import type { AgentEntry } from "@/lib/types";
import type { ModelOption } from "@/components/ModelSelector";
import { useSessionsStore } from "@/stores/sessions-store";
import { TemplatePicker, GroupTeams } from "./GroupManagement";
import { GroupChat } from "./GroupChat";
import { GroupActionButton, openGroupMenu } from "./GroupActions";
import { groupCreate, groupStatus } from "./group-client";
import { useGroupStore } from "./group-store";
import { readGroupCreationDraft } from "./group-creation-draft";
export { GroupSettings } from "./GroupChat";

const errorText = (error: unknown) => String(error).replace(/^Error:\s*/, "");

export function GroupWorkspace({ cwd, modelId, models, workspaces, onToast, onOpenSettings, onOpenKnowledgeBase, onOpenOrganization, onOpenExperts }: { cwd: string; modelId?: string; models: ModelOption[]; workspaces: WorkspaceInfo[]; onToast: (text: string) => void; onOpenSettings: () => void; onOpenKnowledgeBase?: () => void; onOpenOrganization?: () => void; onOpenExperts?: () => void }) {
  const { groups, current, loading, error, createFrom } = useGroupStore();
  const [creating, setCreating] = useState(() => !!useGroupStore.getState().drafts["create:pending"]);
  const [teamsOpen, setTeamsOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [visibleCount, setVisibleCount] = useState(24);
  useEffect(() => setVisibleCount(24), [query, archived]);
  useEffect(() => { if (createFrom) setCreating(true); }, [createFrom]);
  if (teamsOpen) return <GroupTeams onCreate={() => { setTeamsOpen(false); setCreating(true); }} onBack={() => setTeamsOpen(false)} onUse={team => { useGroupStore.setState({ createTemplate: team }); setTeamsOpen(false); setCreating(true); }} />;
  if (current) return <GroupChat key={current.id} group={current} models={models} onToast={onToast} onBack={() => { void useGroupStore.getState().select(null); }} onOpenSettings={onOpenSettings} onOpenKnowledgeBase={onOpenKnowledgeBase} onOpenOrganization={onOpenOrganization} loadError={error} />;
  if (creating) return <GroupCreate onOpenSettings={onOpenSettings} onOpenExperts={onOpenExperts} cwd={cwd} modelId={modelId} models={models} workspaces={workspaces} onClose={() => { useGroupStore.getState().clearCreationDraft(); setCreating(false); useGroupStore.setState({ createFrom: null, createTemplate: null }); }} onCreated={id => { useGroupStore.getState().clearCreationDraft(); useGroupStore.setState({ createFrom: null, createTemplate: null }); setCreating(false); void useGroupStore.getState().refresh(); void useGroupStore.getState().select(id); }} />;
  const visibleGroups = groups.filter(group => (group.archived === archived) && `${group.name} ${group.goal}`.toLowerCase().includes(query.trim().toLowerCase())).sort((a,b) => Number(b.pinned) - Number(a.pinned) || b.updatedAt - a.updatedAt);
  return <CapabilityPage title="专家团" className="groups-page groups-page--list"
    description="把熟悉的专家聚在一起，围绕一个目标持续交流与完成工作。"
    search={{ value: query, onChange: setQuery, label: "搜索专家团", placeholder: "搜索名称或协作目标" }}
    actions={<><Button onClick={() => void useGroupStore.getState().refresh()} disabled={loading} aria-busy={loading}>刷新</Button><Button variant="primary" onClick={() => setCreating(true)}><Plus size={16} />创建专家团</Button></>}>
    <div className="groups-toolbar"><Button variant="ghost" onClick={() => setTeamsOpen(true)}>专家团模板</Button><div className="groups-status-filter" role="group" aria-label="专家团状态"><button type="button" aria-pressed={!archived} onClick={() => setArchived(false)}>未归档</button><button type="button" aria-pressed={archived} onClick={() => setArchived(true)}>已归档</button></div></div>
    {error && <CapabilityError onRetry={() => void useGroupStore.getState().refresh()} retrying={loading}>{error}</CapabilityError>}
    {loading && <p className="capability-loading" role="status">正在读取专家团…</p>}
    {visibleGroups.length > 0 && <ResourceList collectionKey="groups" className="groups-grid" aria-label="专家团列表">{visibleGroups.slice(0, visibleCount).map(group => <ResourceRow className="group-card--managed" key={group.id} onContextMenu={event => openGroupMenu(group.id, event)}>
      <button type="button" className="resource-row__main" onClick={() => void useGroupStore.getState().select(group.id)} aria-label={`打开专家团 ${group.name}`}>
        <ResourceIcon><Users size={18} /></ResourceIcon>
        <span className="resource-row__body">
          <span className="resource-row__title">{group.name}<span className="resource-tag">{groupStatus(group)}</span></span>
          <span className="resource-row__description resource-clamp">{group.goal || "在专家团里说明目标，协调员会和专家一起推进。"}</span>
          <span className="resource-row__meta"><span>{group.members.filter(member => !member.removed).map(member => member.name).join(" · ")}</span><span className="resource-path" title={group.cwd}>{group.cwd}</span></span>
        </span>
      </button>
      <div className="resource-row__actions">
        <button type="button" className="capability-button" onClick={() => void useGroupStore.getState().select(group.id)} aria-label={`进入专家团 ${group.name}`}>进入专家团</button>
        <GroupActionButton group={group} />
      </div>
    </ResourceRow>)}</ResourceList>}

    {visibleGroups.length > visibleCount && <div className="groups-load-more"><button type="button" className="btn btn--ghost" onClick={() => setVisibleCount(count => count + 24)}>加载更多专家团（已显示 {Math.min(visibleCount, visibleGroups.length)} / {visibleGroups.length}）</button></div>}
    {!loading && groups.length > 0 && !visibleGroups.length && <div className="groups-empty"><Search size={30} /><h2>{query ? "没有找到专家团" : archived ? "没有已归档的专家团" : "没有未归档的专家团"}</h2><p>{query ? "试试其他名称或协作目标。" : archived ? "归档后会保留聊天和工作记录，可随时恢复。" : "可以创建专家团，或从已归档列表恢复。"}</p>{query && <button className="btn btn--ghost" onClick={() => setQuery("")}>清除搜索</button>}</div>}

    {!loading && !groups.length && !error && <CapabilityEmptyState icon={<Users size={24} />} title="让一组专家成为你的协作伙伴" description={<>选择本地专家建立专家团。你可以直接 @成员，也可以由协调员安排工作。<div className="groups-empty__action"><Button variant="primary" onClick={() => setCreating(true)}>创建第一个专家团</Button></div></>} />}
  </CapabilityPage>;
}

function GroupCreate({ cwd, modelId, models, workspaces, onClose, onCreated, onOpenSettings, onOpenExperts }: { cwd: string; modelId?: string; models: ModelOption[]; workspaces: WorkspaceInfo[]; onClose: () => void; onCreated: (id: string) => void; onOpenSettings: () => void; onOpenExperts?: () => void }) {
  const liveSource = useGroupStore(state => state.createFrom);
  const template = useGroupStore(state => state.createTemplate);
  const [saved] = useState(() => readGroupCreationDraft(useGroupStore.getState().drafts["create:pending"],
    liveSource ? `source:${liveSource.sessionId}:${liveSource.team.teamId}` : template ? `template:${template.id}` : undefined));
  const source = liveSource ?? saved?.source;
  const creationScope = source ? `source:${source.sessionId}:${source.team.teamId}` : template ? `template:${template.id}` : saved?.scope ?? "manual";
  const [selectedTemplateId, setSelectedTemplateId] = useState(saved?.templateId ?? template?.id ?? "");
  const [coordinatorName, setCoordinatorName] = useState(saved?.coordinatorName ?? template?.coordinatorName ?? undefined);
  const [coordinatorRole, setCoordinatorRole] = useState(saved?.coordinatorRole ?? template?.coordinatorRole ?? undefined);
  const [roles, setRoles] = useState<Record<string, string>>(saved?.roles ?? source?.team.roles ?? Object.fromEntries(template?.members.map(member => [member.name, member.role]) ?? []));
  const [memberModels, setMemberModels] = useState<Record<string, string>>(saved?.memberModels ?? Object.fromEntries(template?.members.filter(member => member.modelId).map(member => [member.name, member.modelId!]) ?? []));
  const sourceSession = useSessionsStore(state => state.independent.find(entry => entry.sessionId === source?.sessionId));
  const [name, setName] = useState(saved?.name ?? source?.team.teamId ?? template?.name ?? "");
  const [goal, setGoal] = useState(saved?.goal ?? source?.team.description ?? template?.description ?? "");
  const [directory, setDirectory] = useState(saved?.directory ?? sourceSession?.cwd ?? cwd);
  const [model, setModel] = useState(saved?.model || sourceSession?.currentModelId || template?.defaultModelId || modelId || models[0]?.id || "");
  const [selected, setSelected] = useState<string[]>(saved?.selected ?? source?.team.members ?? template?.members.map(member => member.name) ?? []);
  const [experts, setExperts] = useState<AgentEntry[]>([]);
  const [reference, setReference] = useState(saved?.reference === true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const creation = useRef({ active: true, busy: false, key: saved?.requestKey ?? "", id: saved?.requestId && /^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(saved.requestId) ? saved.requestId : "" });
  useEffect(() => { creation.current.active = true; return () => { creation.current.active = false; }; }, []);
  const [loading, setLoading] = useState(true);
  const [expertLoadError, setExpertLoadError] = useState<string | null>(null);
  const expertGeneration = useRef(0);
  const load = useCallback(() => {
    const request = ++expertGeneration.current;
    setLoading(true);
    void agentsList().then(items => {
      if (request === expertGeneration.current) { setExperts(items.filter(item => item.scope === "user")); setExpertLoadError(null); }
    }).catch(cause => {
      if (request === expertGeneration.current) setExpertLoadError(errorText(cause));
    }).finally(() => { if (request === expertGeneration.current) setLoading(false); });
  }, []);
  useEffect(() => { load(); return () => { expertGeneration.current++; }; }, [load]);
  const persistCreation = () => {
    const value = JSON.stringify({ scope: creationScope, source, name, goal, directory, model, selected, roles, memberModels, coordinatorName, coordinatorRole, templateId: selectedTemplateId, reference, requestKey: creation.current.key, requestId: creation.current.id });
    useGroupStore.getState().setDraft("create:pending", value);
    return useGroupStore.getState().drafts["create:pending"] === value;
  };
  useEffect(() => { persistCreation(); }, [name, goal, directory, model, selected, roles, memberModels, coordinatorName, coordinatorRole, selectedTemplateId, reference]);
  const suggestedName = selected.length ? `${selected.slice(0, 2).join("与")}协作组`.slice(0, 80) : "新的专家团";
  const missingExpertNames = loading || expertLoadError ? [] : selected.filter(name => !experts.some(expert => expert.name === name));
  const missingExperts = missingExpertNames.length > 0;
  const missingModels = selected.some(name => memberModels[name] && !models.some(model => model.id === memberModels[name]));
  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (creation.current.busy || busy || loading || expertLoadError || missingExperts || missingModels || !selected.length || selected.length > 8 || !directory || !models.some(option => option.id === model)) return;
    if (reference && !source?.referenceText) { setError("此前草稿未保存引用正文，请返回来源会话重新引用，或取消共享来源对话后创建。"); return; }
    creation.current.busy = true;
    setBusy(true);
    setError(null);
    try {
      const input = {
        name: name.trim() || suggestedName, goal, cwd: directory, modelId: model,
        members: selected, roles, models: memberModels,
        coordinatorName: selected.includes(coordinatorName ?? "") ? coordinatorName : undefined,
        coordinatorRole: coordinatorName && !selected.includes(coordinatorName) ? undefined : coordinatorRole,
        ...(source ? { sourceSessionId: source.sessionId, sourceTeamId: source.team.teamId } : {}),
        ...(reference && source?.referenceText ? { referenceText: source.referenceText } : {}),
      };
      const key = JSON.stringify(input);
      if (creation.current.key !== key || !creation.current.id) {
        creation.current.key = key;
        creation.current.id = crypto.randomUUID();
      }
      if (!persistCreation()) throw new Error("暂时无法保存创建请求，请释放存储空间后重试");
      const group = await groupCreate({ ...input, requestId: creation.current.id });
      if (creation.current.active) onCreated(group.id);
      else void useGroupStore.getState().refresh();
    } catch (cause) {
      if (creation.current.active) setError(errorText(cause));
    } finally {
      creation.current.busy = false;
      if (creation.current.active) setBusy(false);
    }
  };
  return <div className="groups-page"><button className="groups-back" onClick={onClose}><ArrowLeft size={16} />返回专家团</button><header className="groups-page__header"><div><h1>创建专家团</h1><p>选好成员和工作区，之后可以随时在专家团里提出目标。</p></div></header>
    <form className="group-form group-form--create" onSubmit={submit}>
      {!source && <TemplatePicker initialTemplateId={selectedTemplateId} onPick={team => {
        setSelectedTemplateId(team?.id ?? "");
        setCoordinatorName(team?.coordinatorName ?? undefined); setCoordinatorRole(team?.coordinatorRole ?? undefined);
        setModel(team?.defaultModelId ?? modelId ?? models[0]?.id ?? ""); setName(team?.name ?? ""); setGoal(team?.description ?? "");
        setSelected(team?.members.map(member => member.name) ?? []);
        setRoles(Object.fromEntries(team?.members.map(member => [member.name, member.role]) ?? []));
        setMemberModels(Object.fromEntries(team?.members.filter(member => member.modelId).map(member => [member.name, member.modelId!]) ?? []));
        setError(null);
      }} />}
      <label>专家团名称<input autoFocus maxLength={80} value={name} onChange={event => setName(event.target.value)} placeholder={suggestedName} /></label>
      <label>协作目标<textarea rows={3} maxLength={4096} value={goal} onChange={event => setGoal(event.target.value)} placeholder="可选，也可以进入群聊后直接说明目标" /></label>
      <fieldset><legend>选择专家 <small>{selected.length}/8</small></legend><p className="groups-muted">协调员会自动加入，负责组织协作。成员使用你已有的本地专家配置。</p>{loading ? <p role="status">正在读取本地专家…</p> : expertLoadError ? <p className="groups-error" role="alert">本地专家读取失败：{expertLoadError}。请重新读取专家后继续。</p> : experts.length ? <div className="group-expert-grid">{experts.map(expert => <label className={`group-expert${selected.includes(expert.name) ? " is-selected" : ""}`} key={expert.name}><input type="checkbox" checked={selected.includes(expert.name)} disabled={!selected.includes(expert.name) && selected.length >= 8} onChange={event => setSelected(previous => event.target.checked ? [...previous, expert.name] : previous.filter(name => name !== expert.name))} /><span><strong>{expert.name}</strong><small>{expert.description || "本地专家"}</small></span></label>)}</div> : <p>还没有本地专家，请先创建专家。{onOpenExperts && <button type="button" className="groups-text-button" onClick={onOpenExperts}>去创建专家</button>}</p>}<button type="button" className="btn btn--ghost" disabled={loading} onClick={load}>重新读取专家</button></fieldset>
      {missingExperts && <p className="groups-error" role="alert">缺少本地专家：{missingExpertNames.join("、")}。请恢复专家配置，或移除后重新选择。<button type="button" className="groups-text-button" onClick={() => setSelected(names => names.filter(name => experts.some(expert => expert.name === name)))}>移除失效选择</button></p>}
      {selected.length > 8 && <p className="groups-error" role="alert">每个专家团最多选择 8 名专家，请取消多余成员。</p>}
      {selected.length > 0 && <details className="group-reference"><summary>成员职责与模型（{selected.length}）</summary><p className="groups-muted">可为成员单独指定模型。未指定时沿用专家配置，再使用下方执行模型。</p>{selected.map(name => <fieldset key={name}><legend>{name}</legend><label>成员职责<textarea rows={2} maxLength={1000} value={roles[name] ?? experts.find(expert => expert.name === name)?.description ?? ""} onChange={event => setRoles(current => ({...current, [name]:event.target.value}))} /></label><label>成员模型<select value={memberModels[name] ?? ""} onChange={event => setMemberModels(current => { const next = {...current}; if (event.target.value) next[name] = event.target.value; else delete next[name]; return next; })}><option value="">沿用专家配置或执行模型</option>{memberModels[name] && !models.some(model => model.id === memberModels[name]) && <option value={memberModels[name]} disabled>不可用：{findModelDisplayName(models, memberModels[name])}</option>}{models.map(model => <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>)}</select></label></fieldset>)}</details>}
      {missingModels && <p className="groups-error" role="alert">部分成员模型不可用，请在“成员职责与模型”中重新选择。</p>}
      <div className="group-form__row"><details className="group-model-options group-reference" open={!models.some(option => option.id === model) || undefined}><summary>模型设置 · {findModelDisplayName(models, model) || "请选择模型"}</summary><label>执行模型<select required value={model} onChange={event => setModel(event.target.value)}><option value="">请选择模型</option>{model && !models.some(option => option.id === model) && <option value={model} disabled>不可用：{findModelDisplayName(models, model)}，请重新选择</option>}{models.map(option => <option key={option.id} value={option.id}>{modelChoiceLabel(option, models)}</option>)}</select></label>{!models.length && <button type="button" className="groups-text-button" onClick={onOpenSettings}>配置可用模型</button>}</details><div><span className="group-form__label">工作区</span><WorkspacePicker cwd={directory} workspaces={workspaces} onSelectWorkspace={setDirectory} menuPlacement="bottom" /><p className="groups-muted">成员在此目录工作。进入专家团后，可在输入框中统一调整协调员与全部成员的权限。</p></div></div>
      {source && <details className="group-reference"><summary>引用来源对话</summary><label><input type="checkbox" checked={reference} onChange={event => setReference(event.target.checked)} />把下列内容共享给成员</label>{source.referenceText ? <pre>{source.referenceText}</pre> : <p className="groups-muted">此前草稿没有保存引用正文。可返回来源会话重新引用，或取消共享后创建。</p>}</details>}
      {error && <p role="alert" className="groups-error">{error}</p>}
      <div className="group-form__actions"><button type="button" className="btn btn--ghost" disabled={busy} onClick={onClose}>取消</button><button type="submit" className="btn btn--primary" disabled={busy || loading || !!expertLoadError || missingExperts || missingModels || selected.length > 8 || !selected.length || !models.some(option => option.id === model) || !directory}>{busy ? "正在创建…" : "创建并进入专家团"}</button></div>
    </form>
  </div>;
}
