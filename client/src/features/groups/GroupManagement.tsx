import { CapabilityPage } from "@/components/experts-panel/shared/CapabilityPage";
import { ResourceIcon } from "@/components/ResourceList";
import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeft, Plus, Users } from "lucide-react";
import { agentsList } from "@/lib/agent-client";
import type { AgentEntry } from "@/lib/types";
import { useAppDialog } from "@/components/AppDialog";
import { groupTemplates, groupDeleteTeam, groupSaveTeam, groupMemberChange, groupDecision, groupDecisionChange, terminalWork, type TeamTemplate, type Group } from "./group-client";

const readableError = (error: unknown) => String(error).replace(/^Error:\s*/, "");

function useTemplates() {
  const [teams, setTeams] = useState<TeamTemplate[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const generation = useRef(0);
  const refresh = useCallback(async () => {
    const request = ++generation.current;
    setLoading(true);
    try {
      const teams = await groupTemplates();
      if (request === generation.current) { setTeams(teams); setError(null); }
    } catch (error) {
      if (request === generation.current) setError(readableError(error));
    } finally {
      if (request === generation.current) setLoading(false);
    }
  }, []);
  useEffect(() => { void refresh(); return () => { generation.current++; }; }, [refresh]);
  return { teams, setTeams, error, loading, refresh };
}

export function TemplatePicker({ onPick, initialTemplateId = "" }: { onPick: (team: TeamTemplate | null) => void; initialTemplateId?: string }) {
  const { teams, error, loading, refresh } = useTemplates();
  const [selectedId, setSelectedId] = useState(initialTemplateId);
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    if (!loading && !error && selectedId && !teams.some(team => team.id === selectedId)) {
      setSelectedId(""); setNotice("所选模板已删除，已保留当前填写的配置，可继续调整并创建专家团。");
    }
  }, [teams, loading, error, selectedId]);
  return <div className="group-template-picker">
    <div className="group-template-picker__head"><span className="group-form__label">专家团模板（可选）</span><button type="button" className="groups-text-button" disabled={loading} onClick={() => void refresh()}>{loading ? "读取中…" : "刷新模板"}</button></div>
    {loading && <p role="status" className="groups-muted">正在读取专家团模板…</p>}
    {error && <p role="alert" className="groups-error">模板读取失败：{error}。{teams.length > 0 ? "当前显示上次读取的模板。" : ""}可刷新重试，或直接选择下方专家创建。</p>}
    {teams.length > 0 && <label>使用专家团模板<select value={selectedId} disabled={loading} onChange={event => {
      const team = teams.find(team => team.id === event.target.value) ?? null;
      setSelectedId(team?.id ?? ""); setNotice(null); onPick(team);
    }}><option value="">自行选择专家</option>{teams.map(team => <option key={team.id} value={team.id}>{team.name} · {team.members.length} 位专家</option>)}</select></label>}
    {notice && <p role="status" className="groups-muted">{notice}</p>}
    {!loading && !error && !teams.length && <p className="groups-muted">还没有保存的专家团模板，可直接选择下方专家创建。</p>}
    {!loading && !error && teams.length > 0 && <p className="groups-muted">模板只包含已保存的成员、分工与模型。切回“自行选择专家”会清空模板带入的配置。</p>}
  </div>;
}

export function GroupTeams({ onBack, onUse, onCreate }: { onBack: () => void; onUse: (team: TeamTemplate) => void; onCreate: () => void }) {
  const { teams, setTeams, error, loading, refresh } = useTemplates();
  const [experts, setExperts] = useState<string[] | null>(null);
  const [expertError, setExpertError] = useState(false);
  const expertGeneration = useRef(0);
  const loadExperts = useCallback(async () => {
    const request = ++expertGeneration.current;
    try {
      const entries = await agentsList();
      if (request === expertGeneration.current) { setExperts(entries.filter(entry => entry.scope === "user").map(entry => entry.name)); setExpertError(false); }
    } catch { if (request === expertGeneration.current) { setExperts(null); setExpertError(true); } }
  }, []);
  useEffect(() => { void loadExperts(); return () => { expertGeneration.current++; }; }, [loadExperts]);
  const dialog = useAppDialog("group-templates");
  return <CapabilityPage title="专家团模板" headingLevel={1} className="groups-page groups-page--list"
    description="只展示你明确保存的协作组合，用于创建新的专家团。模板不包含聊天记录。"
    actions={<><button type="button" className="capability-button" onClick={onBack}><ArrowLeft size={16} />返回专家团</button><button type="button" className="capability-button" disabled={loading} onClick={() => { void refresh(); void loadExperts(); }}>{loading ? "读取中…" : "刷新模板"}</button></>}>
    {error && <p role="alert" className="groups-error">模板读取失败：{error}。请刷新重试。</p>}
    {loading && <p role="status">正在读取专家团模板…</p>}
    {expertError && <p role="status" className="groups-muted">暂时无法检查本地专家，创建时会重新核对。可刷新重试。</p>}
    <div className="template-grid">{teams.map(team => {
      const missing = experts ? team.members.filter(member => !experts.includes(member.name)).map(member => member.name) : [];
      return <section className="resource-template resource-template--managed group-card--template" key={team.id}>
        <div className="resource-row__main"><ResourceIcon><Users size={18} /></ResourceIcon><div className="resource-template__body">
        <span className="resource-template__heading"><strong className="resource-template__title">{team.name}</strong><span className="resource-tag">{team.members.length} 位专家</span></span>
        <p className="resource-template__description">{team.description || "围绕新目标，复用这组专家。"}</p><span className="resource-template__metadata">{team.members.map(member => member.name).join(" · ")}</span>
        </div></div>
        {!!missing.length && <div className="groups-error">缺少本地专家：{missing.join("、")}。创建时可移除失效成员并重新选择。</div>}
        {!team.members.length && <div className="groups-muted">模板暂无成员，创建时请先选择专家。</div>}
        <div className="group-form__actions">
          <button className="btn btn--ghost" onClick={() => dialog.requestConfirmation({ title: `删除专家团模板“${team.name}”？`, description: "已经建立的专家团和专家配置会保留。", danger: true, confirmLabel: "删除模板", action: async () => {
            await groupDeleteTeam(team.id); setTeams(current => current.filter(item => item.id !== team.id)); await refresh();
          } })}>删除模板</button>
          <button className="btn btn--primary" disabled={loading || !!error} onClick={() => onUse(team)}>用此模板创建专家团</button>
        </div>
      </section>;
    })}</div>
    {!loading && !teams.length && !error && <div className="groups-empty"><Users size={32} /><h2>把合适的协作组合留下来</h2><p>在专家团的操作菜单或群设置中选择“保存为模板”，下次就可以直接复用。会话团队可从对应会话创建专家团。</p><button className="btn btn--primary" onClick={onCreate}><Plus size={16} />创建专家团</button></div>}
    {dialog.dialog}
  </CapabilityPage>;
}

export function SaveTeamButton({ group, onError }: { group: Group; onError: (error: string) => void }) {
  const dialog = useAppDialog(group.id);
  const [saved, setSaved] = useState(false);
  return <><button className="groups-text-button" onClick={() => dialog.requestInput({ title: "保存为专家团模板", description: "保存成员、分工和模型选择。同名专家团模板会更新，已有专家团保持自己的配置。", fields: [{ name: "name", label: "模板名称", defaultValue: group.name, required: true, maxLength: 80 }], confirmLabel: "保存模板", onError: error => onError(String(error)), action: async values => { await groupSaveTeam(group.id, values.name); setSaved(true); } })}>保存为模板</button>{saved && <p role="status" className="groups-muted">模板已保存，可从“专家团模板”复用。</p>}{dialog.dialog}</>;
}

export function MemberManager({ group, onChanged, onError }: { group: Group; onChanged: () => void; onError: (error: string) => void }) {
  const [experts, setExperts] = useState<AgentEntry[]>([]), [expert, setExpert] = useState(""), [busy, setBusy] = useState(false);
  const dialog = useAppDialog(group.id);
  useEffect(() => { void agentsList().then(setExperts).catch(error => onError(String(error))); }, []);
  const active = group.works.some(work => !terminalWork(work.status));
  const change = async (action: "add" | "remove" | "refresh" | "clear_memory", id?: string) => { setBusy(true); try { await groupMemberChange(group.id, action, id, action === "add" ? expert : undefined); setExpert(""); onChanged(); } catch (error) { onError(String(error)); if (action !== "add") throw error; } finally { setBusy(false); } };
  return <details className="group-reference"><summary>管理成员</summary><p className="groups-muted">请在当前工作结束后调整成员，历史发言会保留。</p><label>添加本地专家<select aria-label="添加本地专家" value={expert} disabled={active || busy} onChange={event => setExpert(event.target.value)}><option value="">请选择专家</option>{experts.filter(expert => expert.scope === "user" && !group.members.some(member => !member.removed && member.expert === expert.name)).map(expert => <option key={expert.name} value={expert.name}>{expert.name}</option>)}</select></label><button className="groups-text-button" disabled={active || busy || !expert} onClick={() => void change("add")}><Plus size={12} />添加成员</button>{group.members.filter(member => !member.removed && member.expert).map(member => <div className="group-member-management" key={member.id}><strong>{member.name}</strong><button className="groups-text-button" disabled={active || busy} onClick={() => dialog.requestConfirmation({ title: `更新${member.name}的专家配置？`, description: "采用本地最新专家定义，建立新的执行上下文，并保留原来的成员发言。", confirmLabel: "更新配置", action: () => change("refresh", member.id) })}>更新配置</button><button className="groups-text-button" disabled={active || busy || member.id === group.coordinatorId} onClick={() => dialog.requestConfirmation({ title: `移除${member.name}？`, description: "成员的历史发言和交付物仍会保留。", confirmLabel: "移除成员", action: () => change("remove", member.id) })}>移除</button></div>)}<p className="groups-muted">成员记忆只在本群内复用。更换模型或更新配置会保留记忆；更换工作区会隔离记忆。可直接请成员记住、更正或忘记，也可在此清除。</p>{group.members.filter(member => !member.removed).map(member => <div className="group-member-management" key={`memory-${member.id}`}><strong>{member.name}</strong><button className="groups-text-button" disabled={active || busy} onClick={() => dialog.requestConfirmation({ title: `清除${member.name}的记忆？`, description: "删除该成员在本群的长期记忆，建立新上下文，并停止自动召回清除前的群聊。聊天记录与已确认的群决定保留，可由你主动引用。其他成员的记忆保持独立。", danger: true, confirmLabel: "清除成员记忆", action: () => change("clear_memory", member.id) })}>清除成员记忆</button></div>)}{dialog.dialog}</details>;
}

export function DecisionList({ group, onChanged, onError }: { group: Group; onChanged: () => void; onError: (error: string) => void }) {
  const dialog = useAppDialog(group.id);
  const [count, setCount] = useState(20), [showVoided, setShowVoided] = useState(false);
  const [busy, setBusy] = useState(false);
  const records = [...group.decisions].reverse().filter(decision => showVoided || !decision.voided);
  const change = async (id: string, action: "edit" | "void" | "restore", text?: string) => {
    setBusy(true);
    try { await groupDecisionChange(group.id, id, action, text); onChanged(); }
    finally { setBusy(false); }
  };
  if (!group.decisions.length) return null;
  return <section className="group-decisions"><h3>群约定</h3><label className="groups-muted"><input type="checkbox" checked={showVoided} onChange={event => { setShowVoided(event.target.checked); setCount(20); }} />显示已作废约定</label>
    {!records.length && <p className="groups-muted">暂无有效群约定。</p>}
    {records.slice(0, count).map(decision => <div className={`group-decision${decision.voided ? " is-voided" : ""}`} key={decision.id}><p>{decision.text}</p><small>{decision.voided ? "已作废 · 后续工作不再采用" : decision.confirmed ? "已由你确认" : "待你确认"}</small><div className="group-decision__actions">
      {!decision.confirmed && !decision.voided && <button className="groups-text-button" disabled={busy} onClick={async () => { setBusy(true); try { await groupDecision(group.id, "", "", decision.id); onChanged(); } catch (error) { onError(String(error)); } finally { setBusy(false); } }}>确认为群约定</button>}
      {!decision.voided && <><button className="groups-text-button" disabled={busy} onClick={() => dialog.requestInput({ title: "更正群约定", description: "更正后用于后续工作，原内容会保留在修改记录中。", fields: [{ name: "text", label: "约定内容", defaultValue: decision.text, multiline: true, required: true, maxLength: 2000 }], confirmLabel: "保存更正", action: values => change(decision.id, "edit", values.text.trim()) })}>更正</button><button className="groups-text-button" disabled={busy} onClick={() => dialog.requestConfirmation({ title: "作废这条群约定？", description: "后续工作不再采用，原内容仍保留以便查阅。正在执行的工作不会因此自动停止。", confirmLabel: "作废约定", action: () => change(decision.id, "void") })}>作废</button></>}
      {decision.voided && <button className="groups-text-button" disabled={busy} onClick={() => dialog.requestConfirmation({ title: "恢复这条群约定？", description: decision.text, confirmLabel: "确认恢复", action: () => change(decision.id, "restore") })}>恢复约定</button>}
    </div>{!!decision.history?.length && <details className="group-reference"><summary>修改记录（{decision.history.length}）</summary>{decision.history.map((version, index) => <p key={index}><small>{version.action === "void" ? "作废" : version.action === "restore" ? "恢复" : "更正前"} · {new Date(version.changedAt).toLocaleString()}</small><br />{version.text}</p>)}</details>}</div>)}
    {records.length > count && <button className="groups-text-button" onClick={() => setCount(value => value + 20)}>加载更多约定（{Math.min(count, records.length)} / {records.length}）</button>}{dialog.dialog}
  </section>;
}
