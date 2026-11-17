import { Select } from "@/components/Select";
import { createPortal } from "react-dom";
import { useCallback, useEffect, useRef, useState } from "react";
import { Users } from "lucide-react";
import { useOrgSessionStore } from "@/stores/org-session-store";
import type { ProjectMeta } from "@/stores/projects-store";
import { useModalFocus } from "@/lib/use-modal-focus";
import { captureTeamProjectOperation, projectCollaborationCapabilities, projectCollaborationError, teamProjectRequest, type ProjectCollaborationCapabilities, type ProjectMemberCandidate, type ProjectRole, type TeamProjectSnapshot } from "@/lib/team-project-client";
import { useAppDialog } from "./AppDialog";
import { PublishTeamProjectButton } from "./PublishTeamProjectButton";
import "@/styles/project-collaboration.css";

const roles: Record<ProjectRole, string> = { admin: "管理员", collaborator: "可编辑", viewer: "仅查看" };
interface ParticipantProps {
  label?: string;
  sharedId?: string; project?: ProjectMeta; onLinked?: (id: string) => Promise<void> | void; onOpenOrganization?: () => void;
}
export function ProjectParticipantsButton(props: ParticipantProps) {
  const boundary = useOrgSessionStore(state => state.boundaryRevision);
  return <ProjectParticipantsControl key={`${boundary}:${props.project?.id ?? props.sharedId ?? "local"}`} {...props} />;
}
function ProjectParticipantsControl({ sharedId, project, onLinked, onOpenOrganization, label = "参与者" }: ParticipantProps) {
  const session = useOrgSessionStore(state => state.session), boundary = useOrgSessionStore(state => state.boundaryRevision);
  const [open, setOpen] = useState(false), [snapshot, setSnapshot] = useState<TeamProjectSnapshot | null>(null);
  const [capabilities, setCapabilities] = useState<ProjectCollaborationCapabilities | null>(null);
  const [loading, setLoading] = useState(false), [error, setError] = useState(""), [busy, setBusy] = useState(false);
  const [query, setQuery] = useState(""), [candidate, setCandidate] = useState<ProjectMemberCandidate | null>(null), [candidates, setCandidates] = useState<ProjectMemberCandidate[]>([]);
  const [searching, setSearching] = useState(false), [searchError, setSearchError] = useState("");
  const [role, setRole] = useState<ProjectRole>("collaborator"), [notice, setNotice] = useState("");
  const generation = useRef(0), operation = useRef(false);
  const { requestConfirmation, dialog } = useAppDialog(`${boundary}:${sharedId ?? project?.id}`);
  const dialogRef = useModalFocus<HTMLDivElement>(open, () => setOpen(false));
  useEffect(() => {
    generation.current += 1; setOpen(false); setSnapshot(null); setCapabilities(null); setError(""); setQuery(""); setCandidate(null); setCandidates([]); setNotice(""); setBusy(false); operation.current = false;
    return () => { generation.current += 1; };
  }, [boundary, project?.id]);
  useEffect(() => {
    generation.current += 1; setSnapshot(null); setCandidate(null); setCandidates([]); setQuery(""); setSearchError("");
    return () => { generation.current += 1; };
  }, [sharedId]);
  const reload = useCallback(async () => {
    const current = generation.current;
    if (!session?.loggedIn) return;
    setLoading(true); setError("");
    try {
      const identity = captureTeamProjectOperation();
      const capability = await projectCollaborationCapabilities(identity);
      const value = sharedId ? await teamProjectRequest<TeamProjectSnapshot>("snapshot", sharedId, undefined, undefined, identity) : null;
      identity.assertCurrent();
      if (current === generation.current) { setCapabilities(capability); setSnapshot(value); }
    } catch (failure) { if (current === generation.current) { setError(projectCollaborationError(failure)); setCapabilities(null); setSnapshot(null); } }
    finally { if (current === generation.current) setLoading(false); }
  }, [session?.loggedIn, sharedId]);
  useEffect(() => { if (open) void reload(); }, [open, reload]);
  useEffect(() => {
    let active = true; setCandidates([]); setSearchError(""); setSearching(false);
    if (!open || !sharedId || !capabilities?.memberSearch || snapshot?.role !== "admin" || query.trim().length < 2 || candidate) return;
    setSearching(true);
    const timer = window.setTimeout(() => {
      const identity = captureTeamProjectOperation();
      void teamProjectRequest<ProjectMemberCandidate[]>("memberCandidates", sharedId, { query: query.trim() }, undefined, identity).then(value => {
        identity.assertCurrent(); if (active) setCandidates(value);
      }).catch(failure => { if (active) setSearchError(projectCollaborationError(failure)); }).finally(() => { if (active) setSearching(false); });
    }, 250);
    return () => { active = false; window.clearTimeout(timer); };
  }, [open, sharedId, capabilities?.memberSearch, snapshot?.role, query, candidate, boundary]);
  const mutate = async (action: "addMember" | "removeMember" | "updateMember", body?: unknown, memberId?: string) => {
    if (operation.current || !sharedId) return;
    const current = generation.current, identity = captureTeamProjectOperation();
    operation.current = true; setBusy(true); setError(""); setNotice("");
    try {
      await teamProjectRequest(action, sharedId, body, memberId, identity);
      const value = await teamProjectRequest<TeamProjectSnapshot>("snapshot", sharedId, undefined, undefined, identity);
      identity.assertCurrent();
      if (current === generation.current) { setSnapshot(value); setQuery(""); setCandidate(null); setCandidates([]); setNotice("项目参与者已更新"); }
    } catch (failure) { if (current === generation.current) { setError(projectCollaborationError(failure)); if (/失去成员|无权|403|404/.test(String(failure))) setSnapshot(null); } }
    finally { if (current === generation.current) { operation.current = false; setBusy(false); } }
  };
  const canManage = snapshot?.role === "admin";
  const username = capabilities?.memberSearch ? candidate?.username : query.trim();
  return <>
    <button type="button" className="capability-button project-participants-trigger" aria-label={label === "参与者" ? undefined : "邀请与项目参与者"} onClick={() => setOpen(true)}><Users size={16} aria-hidden="true" />{label}{label === "参与者" ? snapshot ? ` · ${snapshot.members.length}` : !sharedId ? " · 1" : "" : ""}</button>
    {open && createPortal(<div className="modal-overlay" onClick={() => setOpen(false)}>
      <div ref={dialogRef} className="create-colleague-dialog project-participants-dialog" role="dialog" aria-modal="true" aria-label="项目参与者" onClick={event => event.stopPropagation()}>
        <div className="create-colleague-header"><h3>项目参与者</h3><button type="button" className="create-colleague-close" aria-label="关闭参与者" onClick={() => setOpen(false)}>×</button></div>
        <div className="create-colleague-body project-participants-body">
          {!session?.loggedIn ? <><p>当前项目保存在本机，仅自己可见。登录组织后可共享项目并邀请成员协作。</p><button type="button" className="capability-button capability-button--primary" onClick={() => { setOpen(false); onOpenOrganization?.(); }}>登录组织</button></> : <>
            {loading && <p role="status">正在检查协作服务与项目权限…</p>}
            {error && <div className="project-collaboration-error" role="alert"><p>{error}</p><button type="button" className="capability-button" disabled={loading} onClick={() => void reload()}>重试连接</button></div>}
            {!sharedId && <><p>当前仅自己可见。首次协作前，请选择要共享的任务和文件；个人会话、参与者备注与账号凭证保留在本机。</p><div className="project-participant-row"><span>{session.user?.displayName ?? session.bootstrap?.user.displayName ?? "我"}</span><span>我 · 项目负责人</span></div></>}
            {snapshot && <>
              <p className="project-collaboration-hint">仅项目成员可访问已共享的资料、任务与成果。成员使用各自的模型和连接器授权。</p>
              <div aria-label="现有项目参与者">{snapshot.members.map(member => <div className="project-participant-row" key={member.userId}>
                <div><strong>{member.displayName}</strong><small>@{member.username}{member.userId === snapshot.ownerId ? " · 项目负责人" : ""}{member.status !== "active" ? " · 已停用" : ""}</small></div>
                {canManage && capabilities?.memberRoles && member.userId !== snapshot.ownerId && member.status === "active" ? <Select aria-label={`${member.displayName}的项目权限`} disabled={busy} value={member.role} onChange={event => { void mutate("updateMember", { role: event.target.value, expectedRevision: snapshot.revision }, member.userId); }}>{Object.entries(roles).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</Select> : <span>{roles[member.role]}</span>}
                {canManage && member.userId !== snapshot.ownerId && <button type="button" className="project-participant-remove" disabled={busy} onClick={() => requestConfirmation({ title: `移除 ${member.displayName}？`, description: "移除后，该成员将失去项目访问权限。", confirmLabel: "移除成员", danger: true, action: () => mutate("removeMember", undefined, member.userId) })}>移除</button>}
              </div>)}</div>
              {canManage && <fieldset className="project-participant-invite" disabled={busy}><legend>邀请组织成员</legend>
                <label>成员<input type="search" aria-label="搜索组织成员" value={query} maxLength={120} placeholder={capabilities?.memberSearch ? "输入至少 2 个字搜索姓名或用户名" : "输入组织用户名"} onChange={event => { setQuery(event.target.value); setCandidate(null); }} /></label>
                {searching && <p role="status">正在搜索成员…</p>}{searchError && <p role="alert">{searchError}</p>}
                {!!candidates.length && <div className="project-member-candidates" aria-label="可邀请成员">{candidates.map(person => <button type="button" key={person.userId} onClick={() => { setCandidate(person); setQuery(person.displayName); setCandidates([]); }}><strong>{person.displayName}</strong><small>@{person.username}</small></button>)}</div>}
                {capabilities?.memberSearch && query.trim().length >= 2 && !candidate && !searching && !searchError && !candidates.length && <p>没有匹配的可邀请成员；仅显示尚未加入项目的有效组织成员。</p>}
                {candidate && <p className="project-collaboration-hint">已选择：{candidate.displayName}（@{candidate.username}）</p>}
                <label>项目权限<Select aria-label="邀请成员权限" value={role} onChange={event => setRole(event.target.value as ProjectRole)}>{Object.entries(roles).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</Select></label>
                <p className="project-collaboration-hint">邀请后将直接加入项目，可访问已共享的历史内容。</p>
                <button type="button" className="capability-button capability-button--primary" disabled={!username || loading} onClick={() => void mutate("addMember", { username, role })}>{busy ? "正在更新…" : "邀请加入项目"}</button>
              </fieldset>}
            </>}
            {project && <div hidden={!capabilities || (!!sharedId && !canManage)}><PublishTeamProjectButton collaborationMode project={project} onLinked={onLinked} onPublished={() => { setNotice("所选内容已共享，可继续邀请成员"); void reload(); }} /></div>}
            {notice && <p role="status">{notice}</p>}
          </>}
        </div>
      </div>
    </div>, document.body)}
    {dialog}
  </>;
}
