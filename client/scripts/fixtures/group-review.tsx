import "../../src/styles/index.css";
// Isolated browser fixture; only production group components are rendered.
import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { GroupWorkspace } from "../../src/features/groups/GroupWorkspace";
import { CapabilitySection } from "../../src/components/CapabilitySection";
import { PlaceholderPage } from "../../src/components/PlaceholderPage";
import { Sidebar } from "../../src/components/Sidebar";
import { useGroupEvents, useGroupStore } from "../../src/features/groups/group-store";
import { usePermissionStore } from "../../src/stores/permission-store";
import type { Group, TeamTemplate } from "../../src/features/groups/group-client";
import { groupRequests, groupPublicSeq } from "../../src/features/groups/group-presentation";
import { latestGroupRequestId } from "../../src/features/groups/group-client";

const theme = new URLSearchParams(location.search).get("theme") ?? "light";
localStorage.setItem("echoagent.theme", theme);
document.documentElement.dataset.theme = theme;
import { GroupActionsHost } from "../../src/features/groups/GroupActions";

const callbacks = new Map<number, (event: unknown) => void>(), listeners = new Map<number, string>();
let counter = 0;
const groups: Group[] = JSON.parse(sessionStorage.getItem("group-review") ?? "[]");
const templates: TeamTemplate[] = JSON.parse(sessionStorage.getItem("group-review-templates") ?? "[]");
let failNextSend = false, failNextDelete = false, failNextTemplates = false;
const cleanupReceipts = new Map<string, {groupId:string;sessionIds:string[]}>();
const publicBodies = new Map<string, string>();
for (const group of groups) for (const message of group.messages) publicBodies.set(message.id, JSON.stringify([message.text,message.attachments]));
const presentation = (group: Group, preview = false): Group => {
  const projected = {...structuredClone(group),requests:groupRequests(group),publicSeq:groupPublicSeq(group),latestRequestId:latestGroupRequestId(group)};
  if (preview) projected.messages = projected.messages.filter(message => ["user","assistant"].includes(message.kind) && (message.text.trim() || message.attachments.length)).slice(-1);
  return projected;
};
const changed = (group: Group) => {
  let revision = groupPublicSeq(group);
  for (const message of group.messages) {
    const body = JSON.stringify([message.text,message.attachments]);
    if (["user","assistant"].includes(message.kind) && (message.text.trim() || message.attachments.length) && publicBodies.get(message.id) !== body) message.publicRevision = ++revision;
    publicBodies.set(message.id,body);
  }
  group.revision++; group.updatedAt = Date.now();
  sessionStorage.setItem("group-review", JSON.stringify(groups));
  for (const [id, event] of listeners) if (event === "group://changed") callbacks.get(id)?.({ payload: { groupId: group.id } });
};
const experts = ["业务分析师", "代码专家", "文档助手"].map(name => ({ name, scope: "user", path: `/review/agents/${name}.md`, raw: "", description: `${name}负责专业分析和交付`, modelTags: [] }));
Object.assign(window, { __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: () => {} }, __TAURI_INTERNALS__: {
  metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
  transformCallback: (callback: (event: unknown) => void) => { callbacks.set(++counter, callback); return counter; },
  unregisterCallback: (id: number) => callbacks.delete(id),
  invoke: async (command: string, args: any = {}) => {
    if (command === "plugin:event|listen") { listeners.set(args.handler, args.event); return args.handler; }
    if (command === "plugin:event|unlisten") { listeners.delete(args.eventId); return; }
    if (command === "agents_list") return experts;
    if (command === "group_list") return groups.map(group => presentation(group,true));
    if (command === "group_cleanup_pending") return [...cleanupReceipts.values()];
    if (command === "group_cleanup_ack") { cleanupReceipts.delete(args.groupId); return; }
    if (command === "group_team_templates") {
      if (failNextTemplates) { failNextTemplates = false; throw new Error("隔离验收：模板暂不可读"); }
      return structuredClone(templates);
    }
    if (command === "group_team_save") {
      const group = groups.find(group => group.id === args.groupId)!;
      const previous = templates.find(template => template.name === args.name);
      const template: TeamTemplate = { id: previous?.id ?? crypto.randomUUID(), name: args.name, description: group.goal, source: "常用团队", members: group.members.filter(member => member.expert && !member.removed).map(member => ({name: member.name, role: member.role, modelId: member.modelId})), coordinatorName: null, coordinatorRole: "组织协作、汇总交付", defaultModelId: "local/mock" };
      if (previous) templates.splice(templates.indexOf(previous), 1, template); else templates.push(template); sessionStorage.setItem("group-review-templates", JSON.stringify(templates)); return template;
    }
    if (command === "group_team_delete") { const index = templates.findIndex(team => team.id === args.templateId); if (index >= 0) templates.splice(index, 1); sessionStorage.setItem("group-review-templates", JSON.stringify(templates)); return; }
    if (command === "group_member_change") {
      const group = groups.find(group => group.id === args.groupId)!;
      if (args.action === "add") group.members.push({id:crypto.randomUUID(), name:args.expert, expert:args.expert, role:`${args.expert}负责专业分析和交付`, modelId:"local/mock", sessionId:null, discussionSessionId:null, generation:0});
      const member = group.members.find(member => member.id === args.memberId);
      if (member && args.action === "remove") member.removed = true;
      if (member && ["refresh", "clear_memory"].includes(args.action)) { member.sessionId = null; member.discussionSessionId = null; if (args.action === "clear_memory") member.generation++; }
      changed(group); return;
    }
    if (command === "group_decision") {
      const group = groups.find(group => group.id === args.groupId)!;
      const decision = group.decisions.find(decision => decision.id === args.decisionId);
      if (decision) decision.confirmed = true;
      else group.decisions.push({id:crypto.randomUUID(), text:args.text, messageId:args.messageId, confirmed:true});
      changed(group); return;
    }
    if (command === "group_metadata") {
      const group = groups.find(group => group.id === args.groupId)!;
      for (const key of ["name", "pinned", "archived"] as const) if (args[key] !== null && args[key] !== undefined) (group as any)[key] = args[key];
      changed(group); return;
    }
    if (command === "group_delete") {
      if (failNextDelete) { failNextDelete = false; throw new Error("隔离验收：尚未确认停止，专家团记录已保留"); }
      const index = groups.findIndex(group => group.id === args.groupId);
      cleanupReceipts.set(args.groupId,{groupId:args.groupId,sessionIds:[]});
      if (index >= 0) groups.splice(index, 1);
      sessionStorage.setItem("group-review", JSON.stringify(groups));
      return { sessionIds: [] };
    }
    if (command === "group_decision_change") {
      const group = groups.find(group => group.id === args.groupId)!;
      const decision = group.decisions.find(decision => decision.id === args.decisionId)!;
      if (args.action === "edit") { decision.history ??= []; decision.history.push({text:decision.text,changedAt:Date.now()}); decision.text=args.text; }
      if (args.action === "void") decision.voided=true;
      if (args.action === "restore") decision.voided=false;
      changed(group); return;
    }
    if (command === "group_snapshot") return presentation(groups.find(group => group.id === args.groupId)!);
    if (command === "group_create") {
      const input = args.input;
      const members = ["协调员", ...input.members].map((name: string, index: number) => ({ id: crypto.randomUUID(), name, expert: index ? name : null, role: index ? `${name}负责专业分析和交付` : "组织协作、汇总交付", modelId: input.modelId, sessionId: `native-${index}`, discussionSessionId: null, generation: 0 }));
      const group: Group = { id: crypto.randomUUID(), name: input.name, goal: input.goal, cwd: input.cwd, sourceSessionId: null, sourceTeamId: null, coordinatorId: members[0].id, members, messages: [], works: [], decisions: [], archived: false, pinned: false, paused: false, revision: 1, updatedAt: Date.now() };
      groups.push(group); changed(group); return structuredClone(group);
    }
    if (command === "group_send") {
      if (failNextSend) { failNextSend = false; throw new Error("隔离验收：暂时无法保存，请重试"); }
      const group = groups.find(group => group.id === args.groupId)!;
      if (group.messages.some(message => message.id === args.input.id)) return;
      const targets=args.input.memberIds.length ? args.input.memberIds : [group.coordinatorId];
      const roots=[...new Set(group.works.filter(work => !["completed","failed","cancelled","stopping","uncertain"].includes(work.status) && work.discussion === args.input.discussion && targets.includes(work.memberId)).map(work => work.rootId))];
      const quote=group.messages.find(message => message.id === args.input.replyTo);
      const quoteRoot=quote?.requestId ?? group.works.find(work => work.id === quote?.workId)?.rootId ?? (group.works.some(work => work.rootId === quote?.id) ? quote?.id : undefined);
      const requestId=args.input.newRequest ? undefined : args.input.requestId ?? (targets.length === 1 && roots.length === 1 && (!args.input.replyTo || quoteRoot === roots[0]) ? roots[0] : undefined);
      group.messages.push({ id: args.input.id, seq: group.messages.length + 1, kind: "user", text: args.input.text, createdAt: Date.now(), memberId: null, workId: null, replyTo: args.input.replyTo, attachments: args.input.attachments, toolCalls: [],requestId,memberIds:targets,requestOwnerId:requestId ? null : targets.length === 1 ? targets[0] : group.coordinatorId });
      if (!requestId) for (const memberId of targets) group.works.push({ id: crypto.randomUUID(), rootId: args.input.id, parentId: null, continuationOf: null, memberId, prompt: args.input.text, status: "running", discussion: args.input.discussion, sessionId: group.members.find(member => member.id === memberId)?.sessionId ?? null, output: "", error: null, createdAt: Date.now(), tokens: 0 });
      changed(group); return;
    }
    if (command === "group_control") {
      const group = groups.find(group => group.id === args.groupId)!;
      if (args.action === "stop_request") { for(const work of group.works) if(work.rootId === args.workId && !["completed","failed","cancelled"].includes(work.status)) work.status="cancelled"; }
      if (args.action === "stop") { group.paused = !args.workId; for (const work of group.works) if ((!args.workId || work.id === args.workId) && !["completed", "failed", "cancelled"].includes(work.status)) work.status = "cancelled"; }
      if (args.action === "retry") {
        const original = group.works.find(work => work.id === args.workId)!;
        const root = crypto.randomUUID();
        group.messages.push({id:root,seq:group.messages.length+1,kind:"user",text:`检查后继续：${original.prompt}`,createdAt:Date.now(),memberId:null,workId:null,replyTo:null,attachments:[],toolCalls:[]});
        group.works.push({...original,id:crypto.randomUUID(),rootId:root,status:"running",error:null,createdAt:Date.now()});
      }
      if (args.action === "resume") { group.paused = false; group.archived = false; }
      if (args.action === "pause") group.paused = true;
      if (args.action === "pin") group.pinned = !group.pinned;
      if (args.action === "archive") group.archived = true;
      changed(group); return;
    }
    if (command === "group_settings") { const group = groups.find(group => group.id === args.groupId)!; Object.assign(group, args.input, { members: group.members.map(member => ({ ...member, ...args.input.members.find((item: any) => item.id === member.id) })) }); changed(group); return; }
    if (command === "agent_resolve_permission") return true;
    if (command === "agent_list_pending_interactions") return { permissions: [], questions: [], planApprovals: [], folderTrustRequests: [] };
    if (command === "automation_capability_status") return { supported: true, permissionMode: "ask", locked: false };
    if (command === "permission_mode_get") return { permissionMode: "ask", locked: false };
    if (command === "plugin:dialog|open") return ["/review/需求说明.md"];
    if (command === "subagents_config_get") return { maxDepth: 1 };
    if (command === "path_stat") return { exists: true, kind: "file", absolute: args.path, path: args.path };
    if (command === "reveal_in_folder" || command === "open_path") return;
    if (command === "coding_fs_read_dir" || command === "coding_list_dir") return [];
    if (command.includes("list") || command.includes("commands")) return [];
    return {};
  },
}, groupReview: {
  concurrent: () => {
    const group = groups.find(group => group.id === useGroupStore.getState().currentId)!;
    const base = group.messages.find(message => message.kind === "user")!;
    const original = group.works[0];
    for (const [index,member] of group.members.slice(0,2).entries()) {
      const root=`routing-${index}`;
      group.messages.push({...base,id:root,seq:group.messages.length+1,text:`独立任务 ${index+1}`,requestId:null,requestOwnerId:member.id,workId:null});
      const work={...original,id:`routing-work-${index}`,rootId:root,memberId:member.id,status:"running",output:"",discussion:false,error:null};
      group.works.push(work);
      if(index === 1) group.messages.push({...base,id:"routing-reply",seq:group.messages.length+1,kind:"assistant",memberId:member.id,text:"第二项任务的进度",requestId:null,workId:work.id});
    }
    changed(group);
  },
  appendReply: () => {
    const group = groups.find(group => group.messages.some(message => message.id === "routing-reply"))!;
    group.messages.find(message => message.id === "routing-reply")!.text += "；这是离开页面之后生成的最终结论";
    changed(group);
  },
  failTemplates: () => { failNextTemplates = true; },
  failSend: () => { failNextSend = true; },
  failDelete: () => { failNextDelete = true; },
  seedMany: (count: number) => {
    const base: Group = groups[0] ?? {id:"seed",name:"seed",goal:"",cwd:"/review/团队工作区",sourceSessionId:null,sourceTeamId:null,coordinatorId:"c",members:[],messages:[],works:[],decisions:[],archived:false,pinned:false,paused:false,revision:1,updatedAt:1};
    for (let index=0; index<count; index++) groups.push({...structuredClone(base),id:`seed-${index}`,name:`目录专家团 ${index}`,goal:index===count-1?"唯一目标检索关键词":"验证历史目录",messages:[],works:[],decisions:[],archived:false,pinned:false});
    changed(groups.at(-1)!);
  },
  history: () => {
    const group = groups.find(group => group.id === useGroupStore.getState().currentId)!;
    for (let i=0;i<40;i++) group.messages.push({id:crypto.randomUUID(),seq:group.messages.length+1,kind:"reference",text:`历史资料 ${i}`,createdAt:Date.now(),memberId:null,workId:null,replyTo:null,attachments:[],toolCalls:[]});
    changed(group);
  },
  failExecution: () => {
    const group = groups.find(group => group.id === useGroupStore.getState().currentId)!;
    for (const work of group.works.filter(work => work.status === "running")) {work.status="failed";work.error="隔离验收：模型暂时不可用，已有记录保留";}
    changed(group);
  },
  complete: () => {
    const group = groups.find(group => group.id === useGroupStore.getState().currentId)!;
    for (const work of group.works.filter(work => work.status === "running")) {
      work.status = "completed"; work.output = "已完成需求分析与交付检查。\n\n- 审批通过前允许撤回\n- 需要补充边界验收\n- 交付物已整理"; work.tokens = 180;
      group.messages.push({ id: crypto.randomUUID(), seq: group.messages.length + 1, kind: "assistant", text:work.output, memberId:work.memberId, workId:work.id, replyTo:null, attachments:[], createdAt:Date.now(), toolCalls:[{toolCallId:"read-requirements",title:"读取需求说明",kind:"read",status:"completed",rawInput:{file_path:"/review/需求说明.md"},content:[{type:"content",content:{type:"text",text:"读取需求说明成功"}}]}] });
    }
    changed(group);
  },
  permission: () => usePermissionStore.getState().request({ requestId: "group-permission", sessionId: "native-1", toolCallId: "tool-write", toolKind: "edit", title: "是否允许写入需求文档？", rawInput: { path: "/review/EchoAgent/需求文档.md" }, options: [{ optionId: "allow-once", kind: "allow", title: "Allow once" }, { optionId: "reject-once", kind: "deny", title: "Reject" }] } as any),
} });
function Review() {
  const [label, setLabel] = useState("专家团");
  useGroupEvents(true);
  useEffect(() => { useGroupStore.setState({ current: null, currentId: null }); }, []);
  const content = label === "专家团" ? (
    <CapabilitySection active={label} onNavigate={setLabel}>
      <GroupWorkspace cwd="/review/团队工作区" modelId="local/mock" models={[{ id: "local/mock", label: "本地验收模型" }]} workspaces={[{ cwd: "/review/团队工作区", sessionCount: 1 }]} onToast={() => {}} onOpenSettings={() => {}} />
    </CapabilitySection>
  ) : <PlaceholderPage label={label} onNavigate={setLabel} />;
  if (new URLSearchParams(location.search).get("groupShell") !== "1") return content;
  return <div className="app"><div className="app__body">
    <Sidebar activeNav={label} onNewSession={() => {}} onNavigate={setLabel} onOpenSettings={() => {}} onOpenSearch={() => {}} onSelect={() => {}} />
    <main className="app__main">{content}</main>
  </div></div>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Review /><GroupActionsHost /></ThemeProvider>);
