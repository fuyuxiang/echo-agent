import { modelChoiceLabel, findModelDisplayName, findModelChoiceLabel } from "@/lib/model-presentation";
import { useElementSize } from "@/lib/use-element-size";
import { Button } from "@/components/Button";
import { CapabilityPage, CapabilityEmptyState, CapabilityError } from "@/components/experts-panel/shared/CapabilityPage";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, Plus, Users, Settings, Search, Pause, Square, Reply, X } from "lucide-react";
import { agentsList, agentResolvePlanApproval, type WorkspaceInfo } from "@/lib/agent-client";
import type { AgentEntry } from "@/lib/types";
import type { ToolCallView } from "@/stores/session-store";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { useAppDialog } from "@/components/AppDialog";
import { TemplatePicker, GroupTeams, SaveTeamButton, MemberManager, DecisionList } from "./GroupManagement";
import { Composer } from "@/components/Composer";
import { MessageItem } from "@/components/MessageItem";
import { PermissionInlineCard } from "@/components/PermissionDialog";
import { QuestionInlineCard } from "@/components/QuestionInlineCard";
import { WorkspacePicker } from "@/components/WorkspacePicker";
import { ToolCallDetailBody } from "@/components/ToolCallCard";
import type { ModelOption } from "@/components/ModelSelector";
import { createMarkdownHostConfig, openLocalPath } from "@/lib/markdown-host";
import { useWorkspaceMentions } from "@/lib/use-workspace-mentions";
import { groupMessageView } from "./group-message-view";
import { preparePromptWithPersonalKnowledge } from "@/lib/knowledge-context";
import { knowledgeSourcesForSession, organizationScopeIdsForSession } from "@/stores/knowledge-store";
import { collectSessionArtifacts } from "@/lib/session-artifacts";
import { useGroupStore } from "./group-store";
import { GroupActionButton, openGroupMenu } from "./GroupActions";
import { groupCreate, groupControl, groupSend, groupSettings, groupDecision, groupStatus, groupRecipients, failedGroupWorks, isGroupStopRequest, terminalWork, workLabels, type Group } from "./group-client";

const errorText = (error: unknown) => String(error).replace(/^Error:\s*/, "");

export function GroupWorkspace({ cwd, modelId, models, workspaces, onToast, onOpenSettings, onOpenKnowledgeBase, onOpenOrganization }: { cwd: string; modelId?: string; models: ModelOption[]; workspaces: WorkspaceInfo[]; onToast: (text: string) => void; onOpenSettings: () => void; onOpenKnowledgeBase?: () => void; onOpenOrganization?: () => void }) {
  const { groups, current, loading, error, createFrom } = useGroupStore();
  const [creating, setCreating] = useState(false);
  const [teamsOpen, setTeamsOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [visibleCount, setVisibleCount] = useState(24);
  useEffect(() => setVisibleCount(24), [query, archived]);
  useEffect(() => { if (createFrom) setCreating(true); }, [createFrom]);
  if (teamsOpen) return <GroupTeams onCreate={() => { setTeamsOpen(false); setCreating(true); }} onBack={() => setTeamsOpen(false)} onUse={team => { useGroupStore.setState({ createTemplate: team }); setTeamsOpen(false); setCreating(true); }} />;
  if (current) return <GroupChat key={current.id} group={current} models={models} onToast={onToast} onBack={() => { void useGroupStore.getState().select(null); }} onOpenSettings={onOpenSettings} onOpenKnowledgeBase={onOpenKnowledgeBase} onOpenOrganization={onOpenOrganization} loadError={error} />;
  if (creating) return <GroupCreate cwd={cwd} modelId={modelId} models={models} workspaces={workspaces} onClose={() => { setCreating(false); useGroupStore.setState({ createFrom: null, createTemplate: null }); }} onCreated={id => { setCreating(false); void useGroupStore.getState().refresh(); void useGroupStore.getState().select(id); }} />;
  const visibleGroups = groups.filter(group => (group.archived === archived) && `${group.name} ${group.goal}`.toLowerCase().includes(query.trim().toLowerCase())).sort((a,b) => Number(b.pinned) - Number(a.pinned) || b.updatedAt - a.updatedAt);
  return <CapabilityPage title="专家团" className="groups-page groups-page--list"
    description="把熟悉的专家聚在一起，围绕一个目标持续交流与完成工作。"
    search={{ value: query, onChange: setQuery, label: "搜索专家团", placeholder: "搜索名称或协作目标" }}
    actions={<><Button onClick={() => void useGroupStore.getState().refresh()} disabled={loading} aria-busy={loading}>刷新</Button><Button variant="primary" onClick={() => setCreating(true)}><Plus size={16} />创建专家团</Button></>}>
    <div className="groups-toolbar"><Button variant="ghost" onClick={() => setTeamsOpen(true)}>专家团模板</Button><div className="groups-status-filter" role="group" aria-label="专家团状态"><button type="button" aria-pressed={!archived} onClick={() => setArchived(false)}>未归档</button><button type="button" aria-pressed={archived} onClick={() => setArchived(true)}>已归档</button></div></div>
    {error && <CapabilityError onRetry={() => void useGroupStore.getState().refresh()} retrying={loading}>{error}</CapabilityError>}
    {loading && <p className="capability-loading" role="status">正在读取专家团…</p>}
    <div className="groups-grid">{visibleGroups.slice(0, visibleCount).map(group => <article className="group-card group-card--managed" key={group.id} onContextMenu={event => openGroupMenu(group.id, event)}>
      <button type="button" className="group-card__open" onClick={() => void useGroupStore.getState().select(group.id)} aria-label={`打开专家团 ${group.name}`}>
        <span className="group-card__icon"><Users size={24} /></span><span className="group-card__head"><strong>{group.name}</strong><small>{groupStatus(group)}</small></span><p>{group.goal || "在专家团里说明目标，协调员会和专家一起推进。"}</p><span className="group-card__members">{group.members.filter(member => !member.removed).map(member => member.name).join(" · ")}</span><small className="group-card__cwd" title={group.cwd}>{group.cwd}</small>
      </button>
      <GroupActionButton group={group} />
    </article>)}</div>
    {visibleGroups.length > visibleCount && <div className="groups-load-more"><button type="button" className="btn btn--ghost" onClick={() => setVisibleCount(count => count + 24)}>加载更多专家团（已显示 {Math.min(visibleCount, visibleGroups.length)} / {visibleGroups.length}）</button></div>}
    {!loading && groups.length > 0 && !visibleGroups.length && <div className="groups-empty"><Search size={30} /><h2>{query ? "没有找到专家团" : archived ? "没有已归档的专家团" : "没有未归档的专家团"}</h2><p>{query ? "试试其他名称或协作目标。" : archived ? "归档后会保留聊天和工作记录，可随时恢复。" : "可以创建专家团，或从已归档列表恢复。"}</p>{query && <button className="btn btn--ghost" onClick={() => setQuery("")}>清除搜索</button>}</div>}

    {!loading && !groups.length && !error && <CapabilityEmptyState icon={<Users size={24} />} title="让一组专家成为你的协作伙伴" description={<>选择本地专家建立专家团。你可以直接 @成员，也可以由协调员安排工作。<div className="groups-empty__action"><Button variant="primary" onClick={() => setCreating(true)}>创建第一个专家团</Button></div></>} />}
  </CapabilityPage>;
}

function GroupCreate({ cwd, modelId, models, workspaces, onClose, onCreated }: { cwd: string; modelId?: string; models: ModelOption[]; workspaces: WorkspaceInfo[]; onClose: () => void; onCreated: (id: string) => void }) {
  const source = useGroupStore(state => state.createFrom);
  const template = useGroupStore(state => state.createTemplate);
  const [coordinatorName, setCoordinatorName] = useState(template?.coordinatorName ?? undefined);
  const [coordinatorRole, setCoordinatorRole] = useState(template?.coordinatorRole ?? undefined);
  const [roles, setRoles] = useState<Record<string, string>>(source?.team.roles ?? Object.fromEntries(template?.members.map(member => [member.name, member.role]) ?? []));
  const [memberModels, setMemberModels] = useState<Record<string, string>>(Object.fromEntries(template?.members.filter(member => member.modelId).map(member => [member.name, member.modelId!]) ?? []));
  const sourceSession = useSessionsStore(state => state.independent.find(entry => entry.sessionId === source?.sessionId));
  const [name, setName] = useState(source?.team.teamId ?? template?.name ?? "");
  const [goal, setGoal] = useState(source?.team.description ?? template?.description ?? "");
  const [directory, setDirectory] = useState(sourceSession?.cwd ?? cwd);
  const [model, setModel] = useState(sourceSession?.currentModelId ?? template?.defaultModelId ?? modelId ?? "");
  const [selected, setSelected] = useState<string[]>(source?.team.members ?? template?.members.map(member => member.name) ?? []);
  const [experts, setExperts] = useState<AgentEntry[]>([]);
  const [reference, setReference] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const creation = useRef({ active: true, busy: false, key: "", id: "" });
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
  const missingExpertNames = loading || expertLoadError ? [] : selected.filter(name => !experts.some(expert => expert.name === name));
  const missingExperts = missingExpertNames.length > 0;
  const missingModels = selected.some(name => memberModels[name] && !models.some(model => model.id === memberModels[name]));
  return <div className="groups-page"><button className="groups-back" onClick={onClose}><ArrowLeft size={16} />返回专家团</button><header className="groups-page__header"><div><h1>创建专家团</h1><p>选好成员和工作区，之后可以随时在专家团里提出目标。</p></div></header>
    <form className="group-form" onSubmit={async event => { event.preventDefault(); if (creation.current.busy || busy || loading || expertLoadError || missingExperts || missingModels || !selected.length || selected.length > 8 || !models.some(option => option.id === model)) return; creation.current.busy = true; setBusy(true); setError(null); try { const input = { name, goal, cwd: directory, modelId: model, members: selected, roles, models: memberModels, coordinatorName: selected.includes(coordinatorName ?? "") ? coordinatorName : undefined, coordinatorRole: coordinatorName && !selected.includes(coordinatorName) ? undefined : coordinatorRole, ...(source ? { sourceSessionId: source.sessionId, sourceTeamId: source.team.teamId } : {}), ...(reference ? { referenceText: source?.referenceText } : {}) }; const key = JSON.stringify(input); if (creation.current.key !== key) { creation.current.key = key; creation.current.id = crypto.randomUUID(); } const group = await groupCreate({ ...input, requestId: creation.current.id }); if (creation.current.active) onCreated(group.id); else void useGroupStore.getState().refresh(); } catch (cause) { if (creation.current.active) setError(errorText(cause)); } finally { creation.current.busy = false; if (creation.current.active) setBusy(false); } }}>
      {!source && <TemplatePicker initialTemplateId={template?.id} onPick={team => {
        setCoordinatorName(team?.coordinatorName ?? undefined); setCoordinatorRole(team?.coordinatorRole ?? undefined);
        setModel(team?.defaultModelId ?? modelId ?? ""); setName(team?.name ?? ""); setGoal(team?.description ?? "");
        setSelected(team?.members.map(member => member.name) ?? []);
        setRoles(Object.fromEntries(team?.members.map(member => [member.name, member.role]) ?? []));
        setMemberModels(Object.fromEntries(team?.members.filter(member => member.modelId).map(member => [member.name, member.modelId!]) ?? []));
        setError(null);
      }} />}
      <label>专家团名称<input autoFocus required maxLength={80} value={name} onChange={event => setName(event.target.value)} placeholder="例如：订单审批交付组" /></label>
      <label>协作目标<textarea rows={3} maxLength={4096} value={goal} onChange={event => setGoal(event.target.value)} placeholder="这组专家需要共同解决什么问题？" /></label>
      <fieldset><legend>选择专家 <small>{selected.length}/8</small></legend><p className="groups-muted">协调员会自动加入，负责组织协作。成员使用你已有的本地专家配置。</p>{loading ? <p role="status">正在读取本地专家…</p> : expertLoadError ? <p className="groups-error" role="alert">本地专家读取失败：{expertLoadError}。请重新读取专家后继续。</p> : experts.length ? <div className="group-expert-grid">{experts.map(expert => <label className={`group-expert${selected.includes(expert.name) ? " is-selected" : ""}`} key={expert.name}><input type="checkbox" checked={selected.includes(expert.name)} disabled={!selected.includes(expert.name) && selected.length >= 8} onChange={event => setSelected(previous => event.target.checked ? [...previous, expert.name] : previous.filter(name => name !== expert.name))} /><span><strong>{expert.name}</strong><small>{expert.description || "本地专家"}</small></span></label>)}</div> : <p>还没有本地专家，请先在“专家·技能·连接器 → 专家”创建专家。</p>}<button type="button" className="btn btn--ghost" disabled={loading} onClick={load}>重新读取专家</button></fieldset>
      {missingExperts && <p className="groups-error" role="alert">缺少本地专家：{missingExpertNames.join("、")}。请恢复专家配置，或移除后重新选择。<button type="button" className="groups-text-button" onClick={() => setSelected(names => names.filter(name => experts.some(expert => expert.name === name)))}>移除失效选择</button></p>}
      {selected.length > 8 && <p className="groups-error" role="alert">每个专家团最多选择 8 名专家，请取消多余成员。</p>}
      {selected.length > 0 && <details className="group-reference"><summary>成员职责与模型（{selected.length}）</summary><p className="groups-muted">可为成员单独指定模型。未指定时沿用专家配置，再使用下方执行模型。</p>{selected.map(name => <fieldset key={name}><legend>{name}</legend><label>成员职责<textarea rows={2} maxLength={1000} value={roles[name] ?? experts.find(expert => expert.name === name)?.description ?? ""} onChange={event => setRoles(current => ({...current, [name]:event.target.value}))} /></label><label>成员模型<select value={memberModels[name] ?? ""} onChange={event => setMemberModels(current => { const next = {...current}; if (event.target.value) next[name] = event.target.value; else delete next[name]; return next; })}><option value="">沿用专家配置或执行模型</option>{memberModels[name] && !models.some(model => model.id === memberModels[name]) && <option value={memberModels[name]} disabled>不可用：{findModelDisplayName(models, memberModels[name])}</option>}{models.map(model => <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>)}</select></label></fieldset>)}</details>}
      {missingModels && <p className="groups-error" role="alert">部分成员模型不可用，请在“成员职责与模型”中重新选择。</p>}
      <div className="group-form__row"><label>执行模型<select required value={model} onChange={event => setModel(event.target.value)}><option value="">请选择模型</option>{model && !models.some(option => option.id === model) && <option value={model} disabled>不可用：{findModelDisplayName(models, model)}，请重新选择</option>}{models.map(option => <option key={option.id} value={option.id}>{modelChoiceLabel(option, models)}</option>)}</select></label><div><span className="group-form__label">工作区</span><WorkspacePicker cwd={directory} workspaces={workspaces} onSelectWorkspace={setDirectory} menuPlacement="bottom" /><p className="groups-muted">成员在此目录工作。所有操作沿用现有授权机制。</p></div></div>
      {source?.referenceText && <details className="group-reference"><summary>引用来源对话</summary><label><input type="checkbox" checked={reference} onChange={event => setReference(event.target.checked)} />把下列内容共享给成员</label><pre>{source.referenceText}</pre></details>}
      {error && <p role="alert" className="groups-error">{error}</p>}
      <div className="group-form__actions"><button type="button" className="btn btn--ghost" disabled={busy} onClick={onClose}>取消</button><button type="submit" className="btn btn--primary" disabled={busy || loading || !!expertLoadError || missingExperts || missingModels || selected.length > 8 || !name.trim() || !selected.length || !models.some(option => option.id === model) || !directory}>{busy ? "正在创建…" : "创建并进入专家团"}</button></div>
    </form>
  </div>;
}

function GroupChat({ group, models, onToast, onBack, onOpenSettings, onOpenKnowledgeBase, onOpenOrganization, loadError }: { group: Group; models: ModelOption[]; onToast: (text: string) => void; loadError?: string | null; onBack: () => void; onOpenSettings: () => void; onOpenKnowledgeBase?: () => void; onOpenOrganization?: () => void }) {
  const dialog = useAppDialog(group.id);
  const activeMembers = group.members.filter(member => !member.removed);
  useEffect(() => { useGroupStore.getState().markRead(group.id, group.revision); }, [group.id, group.revision]);
  const [initialOptions] = useState(() => useGroupStore.getState().draftOptions(group.id));
  const [targets, setTargets] = useState<string[]>(initialOptions.targets), [discussion, setDiscussion] = useState(initialOptions.discussion), [reply, setReply] = useState<string | null>(initialOptions.replyTo);
  useEffect(() => { useGroupStore.getState().setDraftOptions(group.id, {targets, discussion, replyTo:reply}); }, [group.id, targets, discussion, reply]);
  const [tab, setTab] = useState<"members" | "work" | "files" | "settings">("members");
  const [detailOpen, setDetailOpen] = useState(() => typeof window === "undefined" || window.innerWidth >= 1000), [error, setError] = useState<string | null>(null), [busy, setBusy] = useState(false), [query, setQuery] = useState("");
  const chatRef = useRef<HTMLDivElement>(null);
  const chatWidth = useElementSize(chatRef, "width");
  const detailInitialized = useRef(false);
  useEffect(() => {
    if (chatWidth === null || detailInitialized.current) return;
    detailInitialized.current = true;
    setDetailOpen(chatWidth > 820);
  }, [chatWidth]);
  const [tool, setTool] = useState<ToolCallView | null>(null), [newMessages, setNewMessages] = useState(false);
  const draft = useGroupStore(state => state.drafts[group.id] ?? "");
  const recipients = groupRecipients(group, draft, targets, reply);
  const removeRecipient = (id: string) => {
    const next = recipients.filter(target => target !== id);
    setTargets(next.length ? next : [group.coordinatorId]);
    const member = group.members.find(item => item.id === id);
    if (member) useGroupStore.getState().setDraft(group.id, draft.replace(`@${member.name}`, ""));
  };
  const scroll = useRef<HTMLDivElement>(null), following = useRef(true), previousRevision = useRef(group.revision);
  const content = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!content.current || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (following.current && scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight;
    });
    observer.observe(content.current);
    return () => observer.disconnect();
  }, [group.id]);
  const mentions = useWorkspaceMentions(group.cwd);
  const worksById = useMemo(() => new Map(group.works.map(work => [work.id, work])), [group.works]);
  const views = useMemo(() => group.messages.map(message => groupMessageView(message, worksById.get(message.workId ?? ""))), [group.messages, worksById]);
  const viewsById = useMemo(() => new Map(views.map(view => [view.id, view])), [views]);
  const [visibleCount, setVisibleCount] = useState(150);
  const filteredMessages = group.messages.filter(message => !query || message.text.toLowerCase().includes(query.toLowerCase()));
  const visibleMessages = filteredMessages.slice(-visibleCount);
  const showTool = useCallback((value: ToolCallView) => { setTool(value); setDetailOpen(true); }, []);
  const artifacts = useMemo(() => collectSessionArtifacts(views), [views]);
  const config = useMemo(() => createMarkdownHostConfig({ cwd: group.cwd, onToast }), [group.cwd, onToast]);
  const active = group.works.filter(work => !terminalWork(work.status));
  const failures = failedGroupWorks(group);
  const control = async (action: Parameters<typeof groupControl>[1], workId?: string) => { if (busy) return; setBusy(true); setError(null); try { await groupControl(group.id, action, workId); await useGroupStore.getState().select(group.id); await useGroupStore.getState().refresh(); } catch (cause) { setError(errorText(cause)); } finally { setBusy(false); } };
  const sendMessage = async (text: string, attachments: string[] = []) => {
    try {
      const scope = `group:${group.id}`;
      const sources = knowledgeSourcesForSession(scope);
      const scopes = organizationScopeIdsForSession(scope);
      const messageId = useGroupStore.getState().requestId(group.id, JSON.stringify({text,attachments,recipients,discussion,reply,sources,scopes}));
      const stop = isGroupStopRequest({id:messageId,text,attachments,memberIds:recipients,discussion,replyTo:reply});
      // Stopping must never wait for knowledge retrieval or model availability.
      const prepared = stop ? {promptText:text} : await preparePromptWithPersonalKnowledge(scope,text,text,messageId);
      await groupSend(group.id, {id:messageId,text,memberIds:recipients,discussion,attachments,replyTo:reply,
        knowledge:{personal:sources.includes("personal"),organization:sources.includes("organization"),organizationScopeIds:scopes,
          context:prepared.promptText === text ? "" : prepared.promptText.slice(0,-text.length).trim()}});
      useGroupStore.getState().acknowledgeSend(group.id);
      setReply(null); setTargets([]); following.current=true; setError(null);
      return true;
    } catch(cause) {setError(errorText(cause));return false;}
  };
  useEffect(() => { const viewport = scroll.current; if (!viewport) return; if (following.current) viewport.scrollTop = viewport.scrollHeight; else if (previousRevision.current !== group.revision) setNewMessages(true); previousRevision.current = group.revision; }, [group.revision]);
  const sessions = group.members.flatMap(member => [member.sessionId, member.discussionSessionId].filter((id): id is string => !!id));
  const permissions = usePermissionStore(state => state.queues), questions = useQuestionStore(state => state.queues);
  const pendingPlan = useSessionStore(state => sessions.some(id => state.transcripts[id]?.planApprovals?.length));
  const pending = sessions.filter(id => permissions[id]?.length || questions[id]?.length);
  return <div className="group-chat" ref={chatRef}>
    <header className="group-chat__header"><button className="groups-icon" aria-label="返回专家团列表" onClick={onBack}><ArrowLeft size={18} /></button><span className="group-card__icon"><Users size={22} /></span><div className="group-chat__heading"><h1>{group.name}</h1><p title={group.cwd}>{group.goal || group.cwd}</p></div><span className="group-status" data-status={groupStatus(group)}>{pending.length || pendingPlan ? "需要你处理" : groupStatus(group)}</span><GroupActionButton group={group} /><button className="groups-icon" aria-label="专家团详情" aria-expanded={detailOpen} onClick={() => setDetailOpen(open => !open)}><Settings size={18} /></button></header>
    <div className="group-chat__body"><div className="group-chat__main">
      <div className="group-chat__tools"><label className="groups-search"><Search size={14} /><input value={query} onChange={event => setQuery(event.target.value)} aria-label="搜索讨论消息" placeholder="搜索讨论消息" /></label><span className="groups-muted">{activeMembers.length} 位成员 · {group.works.reduce((sum, work) => sum + work.tokens, 0).toLocaleString()} Token</span></div>
      <div className="group-chat__messages" ref={scroll} onScroll={() => { const el = scroll.current!; following.current = el.scrollHeight - el.scrollTop - el.clientHeight < 90; if (following.current) setNewMessages(false); }}>
        <div ref={content}>
        {!group.messages.length && <div className="groups-empty"><Users size={32} /><h2>成员已就位</h2><p>直接说明你希望完成的工作，协调员会组织成员；也可以输入 @选择专家。</p><div className="group-suggestions"><button onClick={() => useGroupStore.getState().setDraft(group.id, "请结合专家团目标，分析需要完成的工作并提出协作计划。")}>一起梳理目标</button><button onClick={() => { setDiscussion(true); setTargets(activeMembers.filter(member => member.expert !== null).map(member => member.id)); useGroupStore.getState().setDraft(group.id, "请从各自专业角度讨论这个目标，指出风险和需要确认的问题。"); }}>请成员讨论一轮</button></div></div>}
        {filteredMessages.length > visibleCount && <button className="groups-back" onClick={() => { const viewport = scroll.current; const height = viewport?.scrollHeight ?? 0; following.current = false; setVisibleCount(count => count + 150); requestAnimationFrame(() => { if (viewport) viewport.scrollTop += viewport.scrollHeight - height; }); }}>加载更早消息（{filteredMessages.length - visibleCount} 条）</button>}
        {visibleMessages.map(message => <article key={message.id} className={`group-message group-message--${message.kind}`} id={`group-message-${message.id}`}>
          {message.kind === "reference" || message.kind === "handoff" ? <details className="group-reference"><summary>{message.kind === "reference" ? "共享的来源资料" : `${group.members.find(member => member.id === message.memberId)?.name ?? "成员"} 派发了工作`}</summary><p>{message.text}</p></details> : <><MessageItem message={viewsById.get(message.id)!} streaming={!viewsById.get(message.id)!.complete} autoExpandProcess={false} authorName={group.members.find(member => member.id === message.memberId)?.name ?? "协调员"} markdownConfig={config} cwd={group.cwd} onOpenTool={showTool} onToast={onToast} /><>{message.requestId && <small className="groups-muted" title={group.messages.find(item => item.id === message.requestId)?.text}>补充原请求：{group.messages.find(item => item.id === message.requestId)?.text.slice(0, 80) ?? "当前任务"}</small>}{message.knowledge?.context && <details className="group-reference"><summary>已向成员共享知识摘录</summary><pre>{message.knowledge.context}</pre></details>}</>{message.kind !== "activity" && <div className="group-message__actions"><time>{new Date(message.createdAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time>{message.workId && <span role="status">{workLabels[group.works.find(work => work.id === message.workId)?.status ?? ""]}</span>}<button onClick={() => { setReply(message.id); setTargets(message.memberId && activeMembers.some(member => member.id === message.memberId) ? [message.memberId] : []); }} aria-label={`回复${group.members.find(member => member.id === message.memberId)?.name ?? "用户"}`}><Reply size={13} />回复</button>{message.text && <button onClick={() => dialog.requestInput({ title: "记为专家团决定", fields: [{ name: "decision", label: "决定内容", defaultValue: message.text.slice(0, 2000), multiline: true, required: true, maxLength: 2000 }], confirmLabel: "确认决定", action: async values => { await groupDecision(group.id, values.decision, message.id); await useGroupStore.getState().select(group.id); } })}>记为决定</button>}</div>}</>}
        </article>)}
        {failures.map(work => <section className="group-interaction groups-error" role="alert" key={`failed-${work.id}`}><strong>{group.members.find(member => member.id === work.memberId)?.name ?? "成员"} 执行失败</strong><p>{work.error || "本轮未能完成，请检查已有输出后重试。"}</p><button className="btn btn--ghost" disabled={busy} onClick={() => void control("retry", work.id)}>检查后重试</button><button className="groups-text-button" onClick={() => {setDetailOpen(true);setTab("work");}}>查看工作详情</button></section>)}
        {group.members.map(member => [member.sessionId, member.discussionSessionId].filter((id): id is string => !!id).map(session => <GroupInteractions key={session} member={member.name} session={session} onError={setError} />))}
        </div>
      </div>
      {newMessages && <button className="group-new-messages" onClick={() => { following.current = true; setNewMessages(false); if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; }}>查看新消息 ↓</button>}
      <div className="group-chat__composer">
        {(error || loadError) && <p className="groups-error" role="alert">{error || loadError}</p>}
        {(group.paused || group.archived) && <div className="group-pause-notice"><span>{group.archived ? "专家团已归档，恢复后可继续交流。" : "协作已暂停，已有消息与结果已保存。"}</span><button className="btn btn--primary" disabled={busy} onClick={() => void control("resume")}>恢复协作</button></div>}
        <div className="group-recipient-row"><span>发送给</span>{recipients.length ? recipients.map(id => <button className="group-recipient" key={id} onClick={() => { removeRecipient(id); }}>{group.members.find(member => member.id === id)?.name}{id !== group.coordinatorId && <X size={11} />}</button>) : <span className="group-recipient">{group.members.find(member => member.id === group.coordinatorId)?.name ?? "协调员"}</span>}<button className="groups-text-button" onClick={() => setTargets([group.coordinatorId])} disabled={recipients.length === 1 && recipients[0] === group.coordinatorId}>交给协调员</button><button className="groups-text-button" onClick={() => setTargets(activeMembers.filter(member => member.expert !== null).map(member => member.id))}>@全体</button><label><input type="checkbox" checked={discussion} onChange={event => setDiscussion(event.target.checked)} />只读讨论</label><span className="group-recipient-row__spacer" />{active.length > 0 && <><button className="groups-text-button" disabled={busy} onClick={() => void control(group.paused ? "resume" : "pause")}><Pause size={13} />{group.paused ? "继续" : "暂停派发"}</button><button className="groups-text-button" disabled={busy} onClick={() => void control("stop")}><Square size={12} />停止全部</button></>}</div>
        {reply && <div className="group-reply">回复：{group.messages.find(message => message.id === reply)?.text.slice(0, 120)}<button aria-label="取消回复" onClick={() => setReply(null)}><X size={13} /></button></div>}
        <Composer slashCommandsEnabled={false} modelSummary="成员模型" onOpenKnowledgeBase={onOpenKnowledgeBase} onOpenOrganization={onOpenOrganization} streaming={false} disabled={group.paused || group.archived} apiReady models={models} cwd={group.cwd} filePaths={mentions.filePaths} workspaceSymbols={mentions.workspaceSymbols} mentionMembers={activeMembers.map(member => ({ id: member.id, name: member.name, role: member.role }))} onMentionMember={id => setTargets(previous => [...new Set([...previous, id])])} draft={draft} draftKey={`group:${group.id}`} knowledgeSessionId={`group:${group.id}`} knowledgeDeferred onToast={onToast} onDraftChange={value => useGroupStore.getState().setDraft(group.id, value)} placeholder="说明目标，或输入 @选择成员；工作中也能继续发送" onOpenSettings={() => { if (!models.length) onOpenSettings(); else { setDetailOpen(true); setTab("settings"); } }} onCancel={() => control("stop")} onSend={sendMessage} />
      </div>
    </div>
    {detailOpen && <aside className="group-chat__detail" aria-label="专家团详情"><div className="group-tabs">{([['members', '成员'], ['work', '工作'], ['files', '交付物'], ['settings', '设置']] as const).map(([key, label]) => <button key={key} aria-pressed={tab === key} onClick={() => setTab(key)}>{label}{key === "work" && active.length > 0 ? ` ${active.length}` : ""}</button>)}</div>
      {tool ? <div className="group-tool-detail"><button className="groups-back" onClick={() => setTool(null)}><ArrowLeft size={14} />返回专家团详情</button><ToolCallDetailBody tc={tool} executionEnded={Boolean([...views].reverse().find(view => view.parts.some(part => part.kind === "tool_call" && part.toolCall.toolCallId === tool.toolCallId))?.complete)} onOpenPath={path => void openLocalPath(path, { cwd: group.cwd, onToast })} /></div> : tab === "members" ? <div className="group-members">{activeMembers.map(member => { const work = [...group.works].reverse().find(work => work.memberId === member.id && !terminalWork(work.status)); return <section key={member.id}><div className="group-member__head"><span className="group-member__avatar">{member.name.slice(0, 1)}</span><strong>{member.name}</strong>{member.id === group.coordinatorId && <small>协调员</small>}</div><p>{member.role}</p><small>{work ? workLabels[work.status] : "空闲"} · {findModelChoiceLabel(models, member.modelId)}</small><button className="groups-text-button" onClick={() => setTargets([member.id])}>@请教此成员</button></section>})}<DecisionList group={group} onChanged={() => void useGroupStore.getState().select(group.id)} onError={setError} /><SaveTeamButton group={group} onError={setError} /><MemberManager group={group} onChanged={() => void useGroupStore.getState().select(group.id)} onError={setError} /></div> : tab === "work" ? <div className="group-work-list">{!group.works.length && <p className="groups-muted">提出目标后，这里会显示成员的工作与进度。</p>}{[...group.works].reverse().map(work => <section key={work.id}><div><strong>{group.members.find(member => member.id === work.memberId)?.name}</strong><span className="group-status">{workLabels[work.status] ?? work.status}</span></div><p>{work.prompt.slice(0, 180)}</p>{work.error && <p className="groups-error">{work.error}</p>}<small>{work.discussion ? "只读讨论" : "协作执行"}{work.tokens ? ` · ${work.tokens.toLocaleString()} Token` : ""}</small>{["failed", "cancelled", "uncertain"].includes(work.status) ? <button className="groups-text-button" disabled={busy} onClick={() => { if (work.status === "uncertain") { dialog.requestConfirmation({ title: "检查后继续工作", description: "请先检查已有文件与操作。专家会先核对现有结果，再继续未完成工作。", confirmLabel: "检查后重试", action: () => control("retry", work.id) }); } else void control("retry", work.id); }}>检查后重试</button> : !terminalWork(work.status) && <button className="groups-text-button" disabled={busy || work.status === "stopping"} onClick={() => void control("stop", work.id)}>停止此项工作</button>}</section>)}</div> : tab === "files" ? <div className="group-artifacts">{!artifacts.length && <p className="groups-muted">成员通过工具成功生成的文件会显示在这里。</p>}{artifacts.map(file => <button key={file.id} onClick={() => void openLocalPath(file.path, { cwd: group.cwd, revealFile: false, onToast })}><strong>{file.path.split(/[\\/]/).pop()}</strong><small>{file.path}</small></button>)}</div> : <GroupSettings group={group} models={models} onSaved={() => useGroupStore.getState().select(group.id)} onError={setError} />}
      <div className="group-detail-footer"><small title={group.cwd}>{group.cwd}</small></div>
    </aside>}
    </div>
    {dialog.dialog}
  </div>;
}

function GroupInteractions({ member, session, onError }: { member: string; session: string; onError: (error: string) => void }) {
  const permission = usePermissionStore(state => state.queues[session]?.[0]);
  const question = useQuestionStore(state => state.queues[session]?.[0]);
  const plan = useSessionStore(state => state.transcripts[session]?.planApprovals?.[0]);
  const [busy, setBusy] = useState(false);
  if (!permission && !question && !plan) return null;
  return <section className="group-interaction"><strong>{member} 需要你处理</strong><PermissionInlineCard sessionId={session} /><QuestionInlineCard sessionId={session} />{plan && <div><h3>执行方案</h3><pre>{plan.planContent}</pre>{([['approved', '批准方案'], ['cancelled', '退回方案']] as const).map(([outcome, label]) => <button className="btn btn--ghost" key={outcome} disabled={busy} onClick={async () => { setBusy(true); try { await agentResolvePlanApproval(plan.requestId, outcome); useSessionStore.getState().dismissPlanApproval(plan.requestId, session); } catch (error) { onError(errorText(error)); } finally { setBusy(false); } }}>{label}</button>)}</div>}</section>;
}

export function GroupSettings({ group, models, onSaved, onError }: { group: Group; models: ModelOption[]; onSaved: () => void | Promise<void>; onError: (error: string) => void }) {
  const [name, setName] = useState(group.name), [goal, setGoal] = useState(group.goal), [coordinatorId, setCoordinator] = useState(group.coordinatorId);
  const [maxCalls, setMaxCalls] = useState(group.maxCalls), [budget, setBudget] = useState(group.tokenBudget);
  const [members, setMembers] = useState(group.members.filter(member => !member.removed).map(({ id, role, modelId }) => ({ id, role, modelId })));
  const [busy, setBusy] = useState(false);
  const [directory, setDirectory] = useState(group.cwd);
  const baseline = useRef(group);
  const [conflicts, setConflicts] = useState<string[]>([]);
  useEffect(() => {
    const previous = baseline.current;
    const changed: string[] = [];
    const merge = <T,>(label: string, old: T, latest: T, current: T): T => {
      if (current === old) return latest;
      if (latest !== old && current !== latest) changed.push(label);
      return current;
    };
    setName(merge("名称",previous.name,group.name,name));
    setGoal(merge("目标",previous.goal,group.goal,goal));
    setCoordinator(merge("协调员",previous.coordinatorId,group.coordinatorId,coordinatorId));
    setMaxCalls(merge("调用上限",previous.maxCalls,group.maxCalls,maxCalls));
    setBudget(merge("Token 预算",previous.tokenBudget,group.tokenBudget,budget));
    setDirectory(merge("工作区",previous.cwd,group.cwd,directory));
    setMembers(group.members.filter(member => !member.removed).map(member => {
      const edited = members.find(item => item.id === member.id);
      const old = previous.members.find(item => item.id === member.id);
      if (!edited || !old) return { id:member.id,role:member.role,modelId:member.modelId };
      if ((edited.role !== old.role && member.role !== old.role && edited.role !== member.role) || (edited.modelId !== old.modelId && member.modelId !== old.modelId && edited.modelId !== member.modelId)) {
        changed.push("成员配置");
      }
      return {id:member.id,role:edited.role === old.role ? member.role : edited.role,modelId:edited.modelId === old.modelId ? member.modelId : edited.modelId};
    }));
    if (changed.length) setConflicts(values => [...new Set([...values,...changed])]);
    baseline.current = group;
  }, [group]);
  const workspaces = useSessionsStore(state => state.workspaces);
  const active = group.works.some(work => !terminalWork(work.status));
  return <form className="group-form group-form--settings" onSubmit={async event => { event.preventDefault(); if (busy || active || conflicts.length || members.some(member => !models.some(model => model.id === member.modelId))) return; setBusy(true); try { await groupSettings(group.id, { expectedRevision: group.revision, name, goal, coordinatorId, maxCalls, tokenBudget: budget, members, cwd: directory }); await onSaved(); } catch (error) { onError(errorText(error)); } finally { setBusy(false); } }}><>{conflicts.length > 0 && <div className="groups-error" role="alert">{conflicts.join("、")}已有其他修改，请检查后选择。<button type="button" className="groups-text-button" onClick={() => { setName(group.name); setGoal(group.goal); setCoordinator(group.coordinatorId); setMaxCalls(group.maxCalls); setBudget(group.tokenBudget); setDirectory(group.cwd); setMembers(group.members.filter(member => !member.removed).map(({id,role,modelId}) => ({id,role,modelId}))); setConflicts([]); }}>使用最新设置</button><button type="button" className="groups-text-button" onClick={() => setConflicts([])}>保留我的修改</button></div>}</><label>专家团名称<input required maxLength={80} value={name} onChange={event => setName(event.target.value)} /></label><label>协作目标<textarea maxLength={4096} rows={3} value={goal} onChange={event => setGoal(event.target.value)} /></label><label>协调员<select value={coordinatorId} onChange={event => setCoordinator(event.target.value)}>{group.members.filter(member => !member.removed).map(member => <option key={member.id} value={member.id}>{member.name}</option>)}</select></label>{members.map((member, index) => <fieldset key={member.id}><legend>{group.members.find(item => item.id === member.id)?.name}</legend><label>成员职责<textarea rows={2} value={member.role} maxLength={1000} onChange={event => setMembers(items => items.map((item, i) => i === index ? { ...item, role: event.target.value } : item))} /></label><label>模型<select value={member.modelId} onChange={event => setMembers(items => items.map((item, i) => i === index ? { ...item, modelId: event.target.value } : item))}>{!models.some(model => model.id === member.modelId) && <option value={member.modelId} disabled>不可用：{findModelDisplayName(models, member.modelId)}</option>}{models.map(model => <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>)}</select></label></fieldset>)}<div><span className="group-form__label">工作区</span><WorkspacePicker cwd={directory} workspaces={workspaces} onSelectWorkspace={setDirectory} menuPlacement="bottom" /><p className="groups-muted">更换后会建立新的执行上下文，历史记录保留。</p></div><label>每次请求最多自动调用<input type="number" min={2} max={50} value={maxCalls} onChange={event => setMaxCalls(Number(event.target.value))} /></label><label>后续派发 Token 预算<input type="number" min={1000} max={2000000} value={budget} onChange={event => setBudget(Number(event.target.value))} /></label>{active && <p className="groups-muted">请结束或停止当前工作后保存设置。</p>}<button className="btn btn--primary" disabled={busy || active || conflicts.length > 0 || members.some(member => !models.some(model => model.id === member.modelId))}>{busy ? "正在保存…" : "保存专家团设置"}</button></form>;
}
