import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, Users, Settings, Search, Pause, Square, Reply, X, AtSign, ChevronDown } from "lucide-react";
import { agentResolvePlanApproval } from "@/lib/agent-client";
import type { ToolCallView } from "@/stores/session-store";
import { useSessionStore } from "@/stores/session-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { useAppDialog } from "@/components/AppDialog";
import { Composer } from "@/components/Composer";
import { MessageItem } from "@/components/MessageItem";
import { PermissionPicker } from "@/components/PermissionPicker";
import { PermissionInlineCard } from "@/components/PermissionDialog";
import { QuestionInlineCard } from "@/components/QuestionInlineCard";
import { WorkspacePicker } from "@/components/WorkspacePicker";
import { ToolCallDetailBody } from "@/components/ToolCallCard";
import type { ModelOption } from "@/components/ModelSelector";
import { modelChoiceLabel, findModelDisplayName, findModelChoiceLabel } from "@/lib/model-presentation";
import { createMarkdownHostConfig, openLocalPath } from "@/lib/markdown-host";
import { useWorkspaceMentions } from "@/lib/use-workspace-mentions";
import { useElementSize } from "@/lib/use-element-size";
import { attachmentBasename } from "@/lib/user-message";
import { preparePromptWithPersonalKnowledge } from "@/lib/knowledge-context";
import { knowledgeSourcesForSession, organizationScopeIdsForSession } from "@/stores/knowledge-store";
import { SaveTeamButton, MemberManager, DecisionList } from "./GroupManagement";
import { GroupReply } from "./GroupReply";
import { GroupWorkList } from "./GroupWorkList";
import { GroupTaskNotice } from "./GroupTaskNotice";
import { GroupResults } from "./GroupResults";
import { activeRequest, followupReceipt, groupActivity, groupPublicSeq, groupRequests, groupRequestTarget, messageRequestId, publicGroupMessages, requestLabels, requestTitle } from "./group-presentation";
import { groupMessageView } from "./group-message-view";
import { useGroupStore } from "./group-store";
import { GroupActionButton } from "./GroupActions";
import { groupControl, groupSend, groupSettings, groupDecision, groupStatus, groupRecipients, groupDiscussionOnly, groupSourceAttachments, mentionedMembers, hasAllMemberMention, groupWorkAutoBlocked, latestGroupRequestId, isGroupStopRequest, terminalWork, workLabels, type Group } from "./group-client";

const errorText = (error: unknown) => String(error).replace(/^Error:\s*/, "");
type DetailTab = "work" | "results" | "settings";

export function GroupChat({ group, models, onToast, onBack, onOpenSettings, onOpenKnowledgeBase, onOpenOrganization, loadError }: {
  group: Group; models: ModelOption[]; onToast: (text: string) => void; loadError?: string | null;
  onBack: () => void; onOpenSettings: () => void; onOpenKnowledgeBase?: () => void; onOpenOrganization?: () => void;
}) {
  const dialog = useAppDialog(group.id);
  const [initial] = useState(() => useGroupStore.getState().draftOptions(group.id));
  const [targets, setTargets] = useState(initial.targets);
  const [discussion, setDiscussion] = useState(initial.discussion);
  const [reply, setReply] = useState<string | null>(initial.replyTo);
  const [newTopic, setNewTopic] = useState(initial.newTopic ?? false);
  const [newRound, setNewRound] = useState(initial.newRound ?? false);
  const [attachmentSelection, setAttachmentSelection] = useState(initial.sourceAttachments);
  const [selectedRequest, setSelectedRequest] = useState<string | null>(initial.requestId ?? latestGroupRequestId(group) ?? null);
  const [tab, setTab] = useState<DetailTab>("settings");
  const [detailOpen, setDetailOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tool, setTool] = useState<ToolCallView | null>(null);
  const [newMessages, setNewMessages] = useState(false);
  const [visibleCount, setVisibleCount] = useState(150);
  const draft = useGroupStore(state => state.drafts[group.id] ?? "");
  const members = group.members.filter(member => !member.removed);
  const mentions = useWorkspaceMentions(group.cwd);
  const mentionSelections = useRef(new Set<string>());
  const scroll = useRef<HTMLDivElement>(null), content = useRef<HTMLDivElement>(null);
  const composeOptions = useRef<HTMLDivElement>(null);
  const chatElement = useRef<HTMLDivElement>(null), detailElement = useRef<HTMLElement>(null);
  const chatWidth = useElementSize(chatElement, "width");
  const compactDetail = chatWidth !== null && chatWidth <= 820;
  const following = useRef(true), previousRevision = useRef("");
  const mounted = useRef(true), controlPending = useRef(false);
  const worksById = useMemo(() => new Map(group.works.map(work => [work.id, work])), [group.works]);
  const requests = useMemo(() => groupRequests(group), [group]);
  const effectiveDiscussion = discussion || groupDiscussionOnly(draft);
  const requestTarget = newRound ? null : groupRequestTarget(group, requests, selectedRequest, newTopic, reply, effectiveDiscussion);
  const quoted = group.messages.find(message => message.id === reply);
  const quotedRequest = quoted ? messageRequestId(group, quoted) : null;
  const focused = requests.find(request => request.id === (quotedRequest ?? selectedRequest ?? requestTarget)) ?? requests[requests.length - 1];
  const focusedId = useRef(focused?.id);
  focusedId.current = focused?.id;
  const selectionIntent = useRef("");
  selectionIntent.current = JSON.stringify([selectedRequest, newTopic, newRound, reply, targets, discussion, attachmentSelection]);
  const sourceRequestId = !newTopic && !requestTarget ? (reply ? quotedRequest : focused?.id ?? null) : null;
  const compositionRoot = newTopic ? null : requestTarget ?? sourceRequestId;
  const availableAttachments = useMemo(() => sourceRequestId ? groupSourceAttachments(group, sourceRequestId) : [], [group.messages, sourceRequestId]);
  const sourceAttachments = availableAttachments.filter(path => attachmentSelection?.requestId !== sourceRequestId || attachmentSelection.paths.includes(path));
  const implicitOwner = (requestTarget || sourceRequestId) && !targets.length && !reply && !mentionedMembers(draft, members).length ? requests.find(request => request.id === (requestTarget ?? sourceRequestId))?.ownerId : null;
  const recipients = groupRecipients(group, draft, implicitOwner ? [implicitOwner] : targets, reply);
  const messages = useMemo(() => publicGroupMessages(group), [group.messages, group.works]);
  const filtered = messages.filter(message => !query || message.text.toLowerCase().includes(query.trim().toLowerCase()));
  const conclusions = useMemo(() => new Map(requests.flatMap(request => request.finalMessageId ? [[request.finalMessageId, request] as const] : [])), [requests]);
  const active = group.works.filter(work => !terminalWork(work.status));
  const unchecked = group.works.find(work => work.status === "uncertain" && work.rootId !== focused?.id);
  const activity = groupActivity(focused ? { ...group, works: group.works.filter(work => work.rootId === focused.id) } : group);
  const config = useMemo(() => createMarkdownHostConfig({ cwd: group.cwd, onToast }), [group.cwd, onToast]);
  const toolViews = useMemo(() => tool ? group.messages.map(message => groupMessageView(message, worksById.get(message.workId ?? ""))) : [], [group.messages, worksById, tool]);
  const sessions = members.flatMap(member => [member.sessionId, member.discussionSessionId].filter((id): id is string => !!id));
  const permissions = usePermissionStore(state => state.queues), questions = useQuestionStore(state => state.queues);
  // Unrelated token streams must not repaint this group's entire history.
  const planSessionIds = useSessionStore(state => sessions.filter(id => state.transcripts[id]?.planApprovals?.length).join("\n"));
  const planSessions = useMemo(() => new Set(planSessionIds ? planSessionIds.split("\n") : []), [planSessionIds]);
  const pending = sessions.filter(id => permissions[id]?.length || questions[id]?.length || planSessions.has(id));

  useEffect(() => {
    useGroupStore.getState().setDraftOptions(group.id, { targets, discussion, replyTo: reply, newTopic, newRound, requestId: selectedRequest, sourceAttachments: attachmentSelection });
  }, [group.id, targets, discussion, reply, newTopic, newRound, selectedRequest, attachmentSelection]);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, [group.id]);
  useEffect(() => { if ((error || loadError) && composeOptions.current) composeOptions.current.scrollTop = 0; }, [error, loadError]);
  const isCurrent = () => mounted.current && useGroupStore.getState().currentId === group.id;
  const refresh = async () => { if (isCurrent()) await useGroupStore.getState().select(group.id); };
  useEffect(() => {
    if (following.current && !query) useGroupStore.getState().markRead(group.id, groupPublicSeq(group));
    const last = messages[messages.length - 1];
    const revision = `${groupPublicSeq(group)}:${last?.id}:${last?.text.length}`;
    if (following.current && !query && scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight;
    else if (previousRevision.current !== revision) setNewMessages(true);
    previousRevision.current = revision;
  }, [group.revision, messages, query]);
  useEffect(() => {
    if (!content.current || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => { if (following.current && scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; });
    observer.observe(content.current);
    return () => observer.disconnect();
  }, [group.id]);
  useEffect(() => {
    if (!detailOpen) return;
    const previousFocus = document.activeElement as HTMLElement | null;
    detailElement.current?.querySelector<HTMLButtonElement>(".group-detail-close")?.focus();
    const escape = (event: KeyboardEvent) => {
      const otherDialog = [...document.querySelectorAll('[role="dialog"],[role="alertdialog"]')].some(element => element !== detailElement.current);
      if (otherDialog) return;
      if (event.key === "Escape") { event.preventDefault(); setDetailOpen(false); }
      if (event.key === "Tab" && compactDetail && detailElement.current) {
        const focusable = [...detailElement.current.querySelectorAll<HTMLElement>('button:not(:disabled),a[href],input:not(:disabled),select:not(:disabled),textarea:not(:disabled),summary')].filter(element => element.getClientRects().length > 0);
        const first = focusable[0], last = focusable[focusable.length - 1];
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }
    };
    document.addEventListener("keydown", escape);
    return () => { document.removeEventListener("keydown", escape); if (previousFocus?.isConnected) previousFocus.focus(); };
  }, [detailOpen, compactDetail]);

  const showDetail = (next: DetailTab) => { setTool(null); setTab(next); setDetailOpen(true); };
  const selectTask = (id: string) => { setSelectedRequest(id); setReply(null); setNewTopic(false); setNewRound(false); };
  const selectAttachments = (paths: string[]) => {
    if (!sourceRequestId) return;
    setAttachmentSelection({ requestId: sourceRequestId, paths });
    setError(current => current?.includes("附件") ? null : current);
  };
  const showConclusion = (id: string) => {
    setDetailOpen(false);
    setQuery(""); setVisibleCount(messages.length); following.current = false;
    requestAnimationFrame(() => document.getElementById(`group-message-${id}`)?.scrollIntoView?.({ block: "center", behavior: "smooth" }));
  };
  const control = async (action: Parameters<typeof groupControl>[1], workId?: string) => {
    if (controlPending.current) return;
    controlPending.current = true; setBusy(true); setError(null);
    const previousRoots = new Set(requests.map(request => request.id));
    const previousFocus = focused?.id;
    const previousIntent = selectionIntent.current;
    try {
      await groupControl(group.id, action, workId);
      await refresh();
      if (isCurrent() && focusedId.current === previousFocus && selectionIntent.current === previousIntent && ["retry", "summarize"].includes(action)) {
        const current = useGroupStore.getState().current;
        const next = current && [...groupRequests(current)].reverse().find(request => !previousRoots.has(request.id));
        if (next) { setSelectedRequest(next.id); setReply(null); setNewTopic(false); setNewRound(false); }
      }
      if (isCurrent() && action === "stop_request") {
        const stopping = useGroupStore.getState().current?.works.some(work => work.rootId === workId && work.status === "stopping");
        onToast(stopping ? "正在停止本次任务，已有结果保留" : "此项任务已停止，已有结果保留");
      }
      await useGroupStore.getState().refresh();
    } catch (cause) { if (isCurrent()) setError(errorText(cause)); }
    finally { controlPending.current = false; if (isCurrent()) setBusy(false); }
  };
  const retry = (id: string) => {
    const work = worksById.get(id);
    const blocked = work?.status === "completed" && groupWorkAutoBlocked(group, id);
    if (blocked || work?.status === "uncertain") dialog.requestConfirmation({
      title: blocked ? "检查后继续未完成工作？" : "检查后继续工作",
      description: "成员会先检查已有文件与执行结果，再继续未完成部分。后续操作使用当前专家团权限，需要授权时会显示审批卡片。",
      confirmLabel: "检查后继续", action: () => control("retry", id),
    });
    else void control("retry", id);
  };
  const send = async (text: string, attachments: string[] = []) => {
    let submitted = false;
    const submissionIntent = selectionIntent.current;
    try {
      const stopIntent = isGroupStopRequest({ id: "control", text, attachments, memberIds: recipients, discussion: effectiveDiscussion, replyTo: reply });
      const previousAttachments = requestTarget ? groupSourceAttachments(group, requestTarget) : sourceAttachments;
      if (!stopIntent && new Set([...attachments, ...previousAttachments]).size > 20) throw new Error("新附件与沿用的历史附件合计最多 20 个，请调整附件选择后发送。输入内容已保留。");
      if (!targets.length && !hasAllMemberMention(text) && mentionedMembers(text, members).some(id => members.filter(member => member.name === members.find(item => item.id === id)?.name).length > 1)) throw new Error("成员名称重复，请通过 @菜单选择具体成员后发送");
      const scope = `group:${group.id}`;
      const sources = knowledgeSourcesForSession(scope), scopes = organizationScopeIdsForSession(scope);
      const id = useGroupStore.getState().requestId(group.id, JSON.stringify({ text, attachments, recipients, discussion: effectiveDiscussion, reply, sources, scopes, newTopic, selectedRequest, sourceRequestId, ...(sourceRequestId ? { sourceAttachmentPaths: sourceAttachments } : {}) }));
      const stop = isGroupStopRequest({ id, text, attachments, memberIds: recipients, discussion: effectiveDiscussion, replyTo: reply });
      const prepared = stop ? { promptText: text } : await preparePromptWithPersonalKnowledge(scope, text, text, id);
      submitted = true;
      await groupSend(group.id, { id, text, memberIds: recipients, discussion: effectiveDiscussion, attachments, replyTo: reply,
        newRequest: !requestTarget, requestId: requestTarget, sourceRequestId: stop ? null : sourceRequestId,
        ...(!stop && sourceRequestId ? { sourceAttachmentPaths: sourceAttachments } : {}),
        knowledge: { personal: sources.includes("personal"), organization: sources.includes("organization"), organizationScopeIds: scopes,
          context: prepared.promptText === text ? "" : prepared.promptText.slice(0, -text.length).trim() } });
      await refresh();
      useGroupStore.getState().acknowledgeSend(group.id);
      if (isCurrent()) {
        if (selectionIntent.current === submissionIntent) {
          setReply(null); setTargets([]); mentionSelections.current.clear(); setSelectedRequest(requestTarget ?? id); setNewTopic(false); setNewRound(false); setAttachmentSelection(undefined);
        }
        setError(null);
        if (group.paused) onToast("补充已保存，未处理的内容会在恢复派发后继续");
      }
      return true;
    } catch (cause) { if (isCurrent()) { if (submitted) setTargets(recipients); setError(errorText(cause)); } return false; }
  };
  const removeRecipient = (id: string) => {
    const next = recipients.filter(target => target !== id);
    setTargets(next.length ? next : [group.coordinatorId]);
    mentionSelections.current.delete(id);
    const member = members.find(item => item.id === id);
    if (member) useGroupStore.getState().setDraft(group.id, draft.replace(`@${member.name}`, ""));
  };
  const openConvention = (message: Group["messages"][number]) => dialog.requestInput({
    title: "设为群约定", description: "群约定会用于本群以后的任务。本次任务的临时要求无需保存为群约定。",
    fields: [{ name: "decision", label: "约定内容", defaultValue: message.text.slice(0, 2000), multiline: true, required: true, maxLength: 2000 }],
    confirmLabel: "确认群约定", action: async values => { await groupDecision(group.id, values.decision, message.id); await refresh(); if (isCurrent()) showDetail("settings"); },
  });

  return <div className="group-chat" ref={chatElement}>
    <header className="group-chat__header">
      <button type="button" className="groups-icon" aria-label="返回专家团列表" onClick={onBack}><ArrowLeft size={18} /></button>
      <span className="group-card__icon"><Users size={22} /></span>
      <div className="group-chat__heading"><h1>{group.name}</h1><p title={group.goal || group.cwd}>{group.goal || members.map(member => member.name).join(" · ")}</p></div>
      <span className="group-status" data-status={groupStatus(group)}>{pending.length ? "需要你处理" : groupStatus(group)}</span>
      <button type="button" className="groups-icon" aria-label="搜索群消息" aria-expanded={searchOpen} onClick={() => setSearchOpen(open => !open)}><Search size={17} /></button>
      <GroupActionButton group={group} />
      <button type="button" className="groups-icon" aria-label="专家团详情" aria-expanded={detailOpen} onClick={() => { if (detailOpen) setDetailOpen(false); else showDetail("settings"); }}><Settings size={18} /></button>
    </header>
    {focused && <section className="group-task-bar" aria-label="当前任务">
      <div className="group-task-bar__heading">{requests.length > 1 ? <label><span>当前任务</span><select aria-label="当前任务" value={focused.id} onChange={event => selectTask(event.target.value)}>{[...requests].reverse().map(request => <option key={request.id} value={request.id}>{requestTitle(group, request.id)}</option>)}</select></label> : <strong title={requestTitle(group, focused.id)}>当前任务：{requestTitle(group, focused.id)}</strong>}<small>{requestLabels[focused.state]}{activeRequest(focused) && activity ? ` · ${activity}` : ""}</small></div>
      <button type="button" className="groups-text-button" onClick={() => showDetail("work")}>查看工作</button>
      {activeRequest(focused) ? <button type="button" className="groups-text-button" disabled={busy} onClick={() => void control("stop_request", focused.id)}><Square size={12} />{focused.state === "stopping" ? "再次停止" : "停止本次任务"}</button>
        : focused.finalMessageId && <button type="button" className="groups-text-button" onClick={() => showDetail("results")}>查看成果</button>}
    </section>}
    <div className="group-chat__body"><div className="group-chat__main">
      {searchOpen && <div className="group-chat__tools"><label className="groups-search"><Search size={14} /><input autoFocus value={query} onChange={event => setQuery(event.target.value)} aria-label="搜索讨论消息" placeholder="搜索讨论消息" /></label><span className="groups-muted">{query ? `${filtered.length} 条匹配消息` : `${members.length} 位成员`}</span><button type="button" className="groups-icon" aria-label="关闭搜索" onClick={() => { setSearchOpen(false); setQuery(""); }}><X size={14} /></button></div>}
      <div className="group-chat__messages" ref={scroll} onScroll={() => { const el = scroll.current!; following.current = el.scrollHeight - el.scrollTop - el.clientHeight < 90; if (following.current && !query) { setNewMessages(false); useGroupStore.getState().markRead(group.id, groupPublicSeq(group)); } }}>
        <div ref={content}>
          {!messages.length && <div className="groups-empty"><Users size={30} /><h2>成员已就位</h2><p>直接说明目标，协调员会按需要安排专家。也可以 @指定成员。</p><div className="group-suggestions"><button type="button" onClick={() => { setDiscussion(true); useGroupStore.getState().setDraft(group.id, "请先梳理目标，提出建议和需要确认的问题。"); }}>先讨论方案</button></div></div>}
          {filtered.length > visibleCount && <button type="button" className="groups-back" onClick={() => { const viewport = scroll.current; const height = viewport?.scrollHeight ?? 0; following.current = false; setVisibleCount(count => count + 150); requestAnimationFrame(() => { if (viewport) viewport.scrollTop += viewport.scrollHeight - height; }); }}>加载更早消息（{filtered.length - visibleCount} 条）</button>}
          {query && !filtered.length && <p className="groups-empty">没有找到匹配消息，试试其他关键词。</p>}
          {filtered.slice(-visibleCount).map(message => {
            const receipt = followupReceipt(group, message);
            const request = conclusions.get(message.id);
            return <article key={message.id} className={`group-message group-message--${message.kind}`} id={`group-message-${message.id}`}>
              {message.kind === "reference" ? <details className="group-reference"><summary>共享的来源资料</summary><p>{message.text}</p></details> : <>
                {message.kind === "user" ? <MessageItem message={groupMessageView(message)} streaming={false} markdownConfig={config} cwd={group.cwd} onToast={onToast} /> : <GroupReply message={message} member={members.find(member => member.id === message.memberId)} coordinator={message.memberId === group.coordinatorId} conclusion={conclusions.has(message.id)} complete={!worksById.get(message.workId ?? "") || terminalWork(worksById.get(message.workId ?? "")!.status) || ["waiting_children", "uncertain"].includes(worksById.get(message.workId ?? "")!.status)} config={config} onToast={onToast} />}
                {message.sourceRequestId && <small className="groups-muted">关联任务：{requestTitle(group, message.sourceRequestId)}</small>}
                {message.requestId && <small className="groups-muted">补充原请求：{requestTitle(group, message.requestId)}</small>}
                {receipt && <small className="group-followup-receipt" role="status">{receipt}</small>}
                {message.knowledge?.context && <details className="group-reference"><summary>已向成员共享知识摘录</summary><pre>{message.knowledge.context}</pre></details>}
                <div className="group-message__actions"><time>{new Date(message.createdAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time>
                  <button type="button" onClick={() => { setReply(message.id); setSelectedRequest(messageRequestId(group, message)); setNewTopic(false); setNewRound(false); setTargets([]); }} aria-label={`回复${members.find(member => member.id === message.memberId)?.name ?? "用户"}`}><Reply size={13} />回复</button>
                  {request && <button type="button" onClick={() => { setSelectedRequest(request.id); showDetail("results"); }}>查看本次成果</button>}
                  {message.text && <details className="group-message-more"><summary aria-label="更多消息操作">更多</summary><button type="button" className="groups-text-button" onClick={() => openConvention(message)}>设为群约定</button></details>}
                </div>
              </>}
            </article>;
          })}
          {focused && <GroupTaskNotice group={group} request={focused} busy={busy} onDetails={() => showDetail("work")} onControl={(action, id) => { if (action === "retry") retry(id); else void control(action, id); }} />}
          {activity && !pending.length && <div className="group-activity" role="status" aria-live="polite"><span className="group-activity__dot" aria-hidden="true" />{activity}</div>}
          {members.map(member => [member.sessionId, member.discussionSessionId].filter((id): id is string => !!id).map(session => <GroupInteractions key={session} member={member.name} session={session} onError={setError} />))}
        </div>
      </div>
      {newMessages && <button type="button" className="group-new-messages" onClick={() => { setQuery(""); following.current = true; setNewMessages(false); if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; useGroupStore.getState().markRead(group.id, groupPublicSeq(group)); }}>查看新消息 ↓</button>}
      <div className="group-chat__composer">
        <div className="group-chat__compose-options" ref={composeOptions}>
        {!!pending.length && <div className="group-pending-notice" role="status"><span>{pending.length} 位成员需要你处理</span><button type="button" className="groups-text-button" onClick={() => { following.current = true; document.getElementById(`group-interaction-${pending[0]}`)?.scrollIntoView?.({ block: "nearest", behavior: "smooth" }); }}>查看并处理</button></div>}
        {(error || loadError) && <div><p className="groups-error" role="alert">{error || loadError}</p>{error?.includes("附件") && requestTarget && <button type="button" className="groups-text-button" onClick={() => { setNewRound(true); setError(null); }}>改为关联新一轮，选择需要沿用的附件</button>}</div>}
        {unchecked && <div className="group-pending-notice" role="status"><span>有未确认的执行需要核对。{!unchecked.discussion && !unchecked.summary && !unchecked.resultRepair ? "相关工作区的写入正在等待。" : "已有记录保留。"}</span><button type="button" className="groups-text-button" onClick={() => selectTask(unchecked.rootId)}>查看待检查任务</button></div>}
        {(group.paused || group.archived) && <div className="group-pause-notice"><span>{group.archived ? "专家团已归档，恢复后可继续交流。" : "后续派发已暂停，已有消息与结果保留。"}</span><button type="button" className="btn btn--primary" disabled={busy} onClick={() => void control("resume")}>恢复派发</button></div>}
        <div className="group-compose-context">
          <span title={compositionRoot ? requestTitle(group, compositionRoot) : undefined}>{compositionRoot ? `${requestTarget ? "补充" : "继续"}：${requestTitle(group, compositionRoot)}` : "开始新任务"}</span>
          {focused && <button type="button" className="groups-text-button" onClick={() => { setNewTopic(!newTopic); setNewRound(false); setReply(null); setTargets([]); }}>{newTopic ? "返回当前任务" : "另起任务"}</button>}
          {requests.length > 1 && <details className="group-task-picker"><summary>切换任务<ChevronDown size={12} /></summary><label>消息归属<select aria-label="消息归属" value={focused?.id ?? ""} onChange={event => selectTask(event.target.value)}>{[...requests].reverse().map(request => <option value={request.id} key={request.id}>{requestTitle(group, request.id)}</option>)}</select></label></details>}
        </div>
        {sourceRequestId && availableAttachments.length > 0 && <details className="group-source-attachments">
          <summary>沿用此前任务附件（已选 {sourceAttachments.length}/{availableAttachments.length}）</summary>
          <p>勾选本轮需要的资料，与新附件合计最多 20 个、64 MB。未勾选的文件仍保留在原任务记录中。</p>
          <div className="group-source-attachments__actions"><button type="button" className="groups-text-button" onClick={() => selectAttachments(availableAttachments.slice(0, 20))}>{availableAttachments.length <= 20 ? "全部沿用" : "选择前 20 个"}</button><button type="button" className="groups-text-button" onClick={() => selectAttachments([])}>不沿用附件</button></div>
          <div className="group-source-attachments__list">{availableAttachments.map(path => <label key={path} title={path}><input type="checkbox" checked={sourceAttachments.includes(path)} onChange={event => selectAttachments(event.target.checked ? [...sourceAttachments, path] : sourceAttachments.filter(item => item !== path))} /><span>{attachmentBasename(path)}</span></label>)}</div>
        </details>}
        <div className="group-recipient-row">
          {(targets.length > 0 || reply || mentionedMembers(draft, members).length > 0) && <><span>发送给</span>{recipients.map(id => <button type="button" className="group-recipient" key={id} onClick={() => removeRecipient(id)} aria-label={`发送给${members.find(member => member.id === id)?.name}，点击移除`}>{members.find(member => member.id === id)?.name}{id !== group.coordinatorId && <X size={11} />}</button>)}</>}
          <details className="group-recipient-picker"><summary><AtSign size={14} />选择成员</summary><div className="group-recipient-options"><button type="button" onClick={() => setTargets([group.coordinatorId])}>交给协调员</button>{members.filter(member => member.id !== group.coordinatorId).map(member => <button type="button" key={member.id} onClick={() => setTargets([member.id])}>{member.name}</button>)}<button type="button" onClick={() => setTargets(members.filter(member => member.expert !== null).map(member => member.id))}>@全体</button></div></details>
          <label className="group-mode-select"><span className="sr-only">处理方式</span><select aria-label="处理方式" value={discussion ? "discussion" : "execute"} onChange={event => setDiscussion(event.target.value === "discussion")}><option value="execute">执行</option><option value="discussion">只讨论</option></select></label>
          {!discussion && effectiveDiscussion && <small>已识别只读要求</small>}
        </div>
        {active.length > 0 && effectiveDiscussion && <p className="group-mode-notice">下一条消息只讨论，正在执行的工作保持原状态。</p>}
        {reply && <div className="group-reply">回复：{quoted?.text.slice(0, 120)}<button type="button" aria-label="取消回复" onClick={() => { setReply(null); setTargets([]); }}><X size={13} /></button></div>}
        </div>
        <Composer slashCommandsEnabled={false} permissionInline permissionControl={<PermissionPicker groupId={group.id} groupPermissionMode={group.permissionMode} compact={chatWidth !== null && chatWidth < 520} disabled={group.archived} onToast={onToast} />} modelSummary="成员模型" onOpenKnowledgeBase={onOpenKnowledgeBase} onOpenOrganization={onOpenOrganization} streaming={false} disabled={group.archived} apiReady models={models} cwd={group.cwd} filePaths={mentions.filePaths} workspaceSymbols={mentions.workspaceSymbols} mentionMembers={members.map(member => ({ id: member.id, name: member.name, role: member.role }))}
          onMentionMember={id => { mentionSelections.current.add(id); setTargets(previous => [...new Set([...previous.filter(target => target !== group.coordinatorId), id])]); }}
          draft={draft} draftKey={`group:${group.id}`} knowledgeSessionId={`group:${group.id}`} knowledgeDeferred onToast={onToast}
          onDraftChange={value => { setTargets(previous => previous.filter(id => !mentionSelections.current.has(id) || value.includes(`@${members.find(member => member.id === id)?.name}`))); useGroupStore.getState().setDraft(group.id, value); }}
          placeholder="说明目标，或输入 @选择成员；工作中也能继续发送" onOpenSettings={() => { if (!models.length) onOpenSettings(); else showDetail("settings"); }} onCancel={() => focused ? control("stop_request", focused.id) : control("stop")} onSend={send} />
      </div>
    </div>
    {detailOpen && <><button type="button" className="group-detail-backdrop" aria-label="关闭专家团详情" onClick={() => setDetailOpen(false)} /><aside ref={detailElement} className="group-chat__detail" aria-label="专家团详情" role={compactDetail ? "dialog" : undefined} aria-modal={compactDetail || undefined}>
      <div className="group-tabs">{([['work', '工作'], ['results', '成果'], ['settings', '群设置']] as const).map(([key, label]) => <button type="button" key={key} aria-pressed={tab === key} onClick={() => { setTool(null); setTab(key); }}>{label}</button>)}<button type="button" className="groups-icon group-detail-close" aria-label="收起专家团详情" onClick={() => setDetailOpen(false)}><X size={15} /></button></div>
      {tool ? <div className="group-tool-detail"><button type="button" className="groups-back" onClick={() => setTool(null)}>返回专家团详情</button><ToolCallDetailBody tc={tool} executionEnded={Boolean([...toolViews].reverse().find(view => view.parts.some(part => part.kind === "tool_call" && part.toolCall.toolCallId === tool.toolCallId))?.complete)} onOpenPath={path => void openLocalPath(path, { cwd: group.cwd, onToast })} /></div>
        : tab === "work" ? <><GroupWorkList group={group} requestId={focused?.id} onSelect={selectTask} busy={busy} onOpenTool={value => setTool(value)} onControl={(action, id) => { if (action === "retry") retry(id); else void control(action, id); }} /><details className="group-operation-options"><summary>派发控制</summary><p>暂停后续派发时，正在执行的工作会继续完成。</p><button type="button" className="groups-text-button" disabled={busy || group.archived} onClick={() => void control(group.paused ? "resume" : "pause")}><Pause size={12} />{group.paused ? "继续派发" : "暂停后续派发"}</button>{active.length > 0 && <button type="button" className="groups-text-button" disabled={busy} onClick={() => void control("stop")}><Square size={12} />停止全部</button>}</details></>
        : tab === "results" ? <GroupResults group={group} requests={requests} requestId={focused?.id ?? null} onSelect={selectTask} onConclusion={showConclusion} onError={setError} />
        : <div className="group-members">{members.map(member => { const work = [...group.works].reverse().find(work => work.memberId === member.id && !terminalWork(work.status)); return <section key={member.id}><div className="group-member__head"><span className="group-member__avatar">{Array.from(member.name)[0]}</span><strong>{member.name}</strong>{member.id === group.coordinatorId && <small>协调员</small>}</div><p>{member.role}</p><small>{work ? workLabels[work.status] : "空闲"} · {findModelChoiceLabel(models, member.modelId)}</small><button type="button" className="groups-text-button" onClick={() => { setTargets([member.id]); setDetailOpen(false); }}>@请教此成员</button></section>; })}<DecisionList group={group} onChanged={() => void refresh()} onError={setError} /><SaveTeamButton group={group} onError={setError} /><MemberManager group={group} onChanged={() => void refresh()} onError={setError} /><details className="group-settings-options"><summary>名称、目标、模型与工作区</summary><GroupSettings group={group} models={models} onSaved={refresh} onError={setError} /></details></div>}
      <div className="group-detail-footer"><small title={group.cwd}>{group.cwd}</small></div>
    </aside></>}
    </div>{dialog.dialog}
  </div>;
}

function GroupInteractions({ member, session, onError }: { member: string; session: string; onError: (error: string) => void }) {
  const permission = usePermissionStore(state => state.queues[session]?.[0]);
  const question = useQuestionStore(state => state.queues[session]?.[0]);
  const plan = useSessionStore(state => state.transcripts[session]?.planApprovals?.[0]);
  const [busy, setBusy] = useState(false);
  if (!permission && !question && !plan) return null;
  return <section className="group-interaction" id={`group-interaction-${session}`}><strong>{member} 需要你处理</strong><PermissionInlineCard sessionId={session} groupScope /><QuestionInlineCard sessionId={session} />{plan && <div><h3>执行方案</h3><pre>{plan.planContent}</pre>{([['approved', '批准方案'], ['cancelled', '退回方案']] as const).map(([outcome, label]) => <button type="button" className="btn btn--ghost" key={outcome} disabled={busy} onClick={async () => { setBusy(true); try { await agentResolvePlanApproval(plan.requestId, outcome); useSessionStore.getState().dismissPlanApproval(plan.requestId, session); } catch (error) { onError(errorText(error)); } finally { setBusy(false); } }}>{label}</button>)}</div>}</section>;
}
export function GroupSettings({ group, models, onSaved, onError }: { group: Group; models: ModelOption[]; onSaved: () => void | Promise<void>; onError: (error: string) => void }) {
  const [name, setName] = useState(group.name), [goal, setGoal] = useState(group.goal), [coordinatorId, setCoordinator] = useState(group.coordinatorId);
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
  return <form className="group-form group-form--settings" onSubmit={async event => { event.preventDefault(); if (busy || active || conflicts.length || members.some(member => !models.some(model => model.id === member.modelId))) return; setBusy(true); try { await groupSettings(group.id, { expectedRevision: group.revision, name, goal, coordinatorId, members, cwd: directory }); await onSaved(); } catch (error) { onError(errorText(error)); } finally { setBusy(false); } }}><>{conflicts.length > 0 && <div className="groups-error" role="alert">{conflicts.join("、")}已有其他修改，请检查后选择。<button type="button" className="groups-text-button" onClick={() => { setName(group.name); setGoal(group.goal); setCoordinator(group.coordinatorId); setDirectory(group.cwd); setMembers(group.members.filter(member => !member.removed).map(({id,role,modelId}) => ({id,role,modelId}))); setConflicts([]); }}>使用最新设置</button><button type="button" className="groups-text-button" onClick={() => setConflicts([])}>保留我的修改</button></div>}</><label>专家团名称<input required maxLength={80} value={name} onChange={event => setName(event.target.value)} /></label><label>协作目标<textarea maxLength={4096} rows={3} value={goal} onChange={event => setGoal(event.target.value)} /></label><label>协调员<select value={coordinatorId} onChange={event => setCoordinator(event.target.value)}>{group.members.filter(member => !member.removed).map(member => <option key={member.id} value={member.id}>{member.name}</option>)}</select></label>{members.map((member, index) => <fieldset key={member.id}><legend>{group.members.find(item => item.id === member.id)?.name}</legend><label>成员职责<textarea rows={2} value={member.role} maxLength={1000} onChange={event => setMembers(items => items.map((item, i) => i === index ? { ...item, role: event.target.value } : item))} /></label><label>模型<select value={member.modelId} onChange={event => setMembers(items => items.map((item, i) => i === index ? { ...item, modelId: event.target.value } : item))}>{!models.some(model => model.id === member.modelId) && <option value={member.modelId} disabled>不可用：{findModelDisplayName(models, member.modelId)}</option>}{models.map(model => <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>)}</select></label></fieldset>)}<div><span className="group-form__label">工作区</span><WorkspacePicker cwd={directory} workspaces={workspaces} onSelectWorkspace={setDirectory} menuPlacement="bottom" /><p className="groups-muted">更换后会建立新的执行上下文，历史记录保留。</p></div>{active && <p className="groups-muted">请结束或停止当前工作后保存设置。</p>}<button className="btn btn--primary" disabled={busy || active || conflicts.length > 0 || members.some(member => !models.some(model => model.id === member.modelId))}>{busy ? "正在保存…" : "保存专家团设置"}</button></form>;
}
