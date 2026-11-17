// Isolated UI acceptance data; never imported by the production application.
import React from "react";
import { createRoot } from "react-dom/client";
import "../../src/styles/index.css";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { AssistantsPanel } from "../../src/components/AssistantsPanel";
import { ProjectsPanel } from "../../src/components/ProjectsPanel";
import { ModelCatalogContext } from "../../src/lib/model-management";
import { useProjectsStore, type ProjectMeta } from "../../src/stores/projects-store";
import { useOrgSessionStore } from "../../src/stores/org-session-store";
import { useSessionsStore } from "../../src/stores/sessions-store";
import type { TeamProjectSnapshot } from "../../src/lib/team-project-client";

const params = new URLSearchParams(location.search);
const theme = params.get("theme") ?? "light";
localStorage.setItem("echoagent.theme", theme); document.documentElement.dataset.theme = theme;
const models = [{ id:"personal-model",label:"通用模型",source:"personal" as const,providerLabel:"个人连接" },{ id:"organization/model",label:"组织推理模型",source:"organization" as const,providerLabel:"组织连接" }];
let settings = { model:"personal-model",skills:["writing"],tools:["read_file"],disallowedTools:[],discoverSkills:true,inheritSkills:true,mcpServers:["docs"],mcpInheritance:"all" };
const expert = {name:"业务分析师",description:"整理需求、分析数据并形成可评审的交付文档",scope:"user",path:"/review/agents/analyst.md",raw:"---\nname: 业务分析师\n---\n根据项目背景分析问题，给出有依据的建议。",modelTags:["reasoning"]};
const project:ProjectMeta = {id:"review-local-project",name:"产品需求梳理",instructions:"梳理用户需求，完成可评审的方案。",createdAt:new Date().toISOString(),cwd:"/review/workspace",defaultModelId:models[0].id,connectors:[],experts:[],skills:[],plans:[],tasks:[{id:"local-task",title:"梳理需求",status:"pending",scope:"personal",source:"manual"}],assets:[{id:"local-file",name:"需求说明.md",kind:"file",path:"/review/reference.md"}],members:["保留原有本机备注"],conversations:[]};
const shared:TeamProjectSnapshot = {id:"00000000-0000-4000-8000-000000000001",name:project.name,goal:project.instructions!,scopeId:"scope",ownerId:"a",revision:1,updatedAt:Date.now(),role:"admin",members:[{userId:"a",username:"owner",displayName:"项目负责人",role:"admin",status:"active"}],tasks:[],decisions:[],assets:[],runs:[],events:[]};
const remote:TeamProjectSnapshot = {...structuredClone(shared),id:"00000000-0000-4000-8000-000000000002",name:"客户交付",role:"viewer"};
shared.comments=[]; remote.comments=[];
const workspaceReview=params.get("surface")==="workspace";
if(workspaceReview){
 project.name="客户洞察与产品规划";project.description="汇总客户反馈，形成有依据、可落地的产品规划与交付文档。";
 project.skills=[{id:"/review/skills/analysis",name:"数据分析"}];
 project.tasks=[{id:"analysis",title:"竞品分析与产品机会点",description:"对比三款同类产品，梳理客户需求与可落地的改进方向。",acceptance:"结论可追溯，提供三项可执行建议。",sessionId:"analysis-session",status:"in_progress",scope:"personal",source:"manual"},{id:"roadmap",title:"形成下一季度产品路线图",sessionId:"roadmap-session",status:"in_progress",scope:"personal",source:"manual"},{id:"todo",title:"准备产品评审会汇报材料",status:"pending",scope:"personal",source:"manual"}];
 project.decisions=[{id:"decision",content:"需求排序统一采用用户影响、业务价值和实现成本三个维度。",rationale:"评审标准保持一致",status:"active",createdAt:new Date().toISOString(),updatedAt:new Date().toISOString()}];
 useSessionsStore.setState({independent:[{sessionId:"analysis-session",title:"竞品分析与产品机会点",cwd:"/review/workspace",status:"completed",updatedAt:new Date().toISOString()},{sessionId:"roadmap-session",title:"形成下一季度产品路线图",cwd:"/review/workspace",status:"working",updatedAt:new Date().toISOString()}],drafts:{}});
 localStorage.setItem("echoagent.task-artifacts.v1",JSON.stringify([{id:"output",path:"/review/workspace/竞品分析报告.md",kind:"edit",title:"Write report",toolCallId:"write-report",status:"completed",verifiedOutput:true,sessionId:"analysis-session",sessionTitle:"竞品分析与产品机会点",cwd:"/review/workspace",updatedAt:Date.now()}]));
}
const scheduled:any[]=[];const records:any[]=[];
let created=false; const calls:unknown[]=[];
const callbacks = new Map<number, unknown>();
Object.assign(window,{reviewCalls:calls,__TAURI_INTERNALS__:{metadata:{currentWindow:{label:"main"},currentWebview:{label:"main"}},transformCallback:(callback:unknown)=>{const id=callbacks.size+1;callbacks.set(id,callback);return id;},unregisterCallback:(id:number)=>callbacks.delete(id),invoke:async(command:string,args:any={})=>{
 calls.push({command,args});
 switch(command){
  case "agents_list":return [expert];
  case "agents_definition_settings":return structuredClone(settings);
  case "agents_configure_definition":settings=args.settings;return args.raw;
  case "agents_merge_definition":return args.originalRaw;
  case "agents_template":return expert.raw;
  case "agents_update":return expert;
  case "skills_list":return [{name:"writing",displayName:"文档撰写",description:"形成结构化报告与交付文档",enabled:true,scope:"user"},{name:"analysis",path:"/review/skills/analysis",displayName:"数据分析",description:"整理数据、识别趋势并形成建议",enabled:true,scope:"server",capability:{declared:false,state:"instruction_only",ready:true,capabilities:[],checks:[]}},{name:"retired",displayName:"已停用技能",enabled:false,scope:"user"}];
  case "mcp_list":return [{name:"docs",displayName:"组织文档",enabled:true,live:true,status:"ready",tools:[],source:"user"},{name:"repo",displayName:"代码仓库",enabled:true,live:true,status:"ready",tools:[],source:"user"},{name:"mail",displayName:"企业邮箱",enabled:true,authRequired:true,tools:[],source:"user"}];
  case "org_project_request":{
   if(params.get("service")==="unsupported")throw new Error("接口不存在");
   if(args.action==="list")return structuredClone(created?[shared,remote]:[remote]);
   if(args.action==="capabilities")return {version:4,memberSearch:true,memberRoles:true,idempotentCreate:true,taskIdentity:true,projectComments:true,decisionRevisions:true};
   if(args.action==="create"){created=true;return structuredClone(shared);}
   if(args.action==="memberCandidates")return [{userId:"b",username:"liming",displayName:"李明"}];
   if(args.action==="addMember"){shared.members.push({userId:"b",username:args.body.username,displayName:"李明",role:args.body.role,status:"active"});shared.revision++;return structuredClone(shared);}
   if(args.action==="createTask"){const id=`remote-task-${shared.tasks.length}`;shared.tasks.push({id,title:args.body.title,description:args.body.description??"",acceptance:args.body.acceptance??"",assigneeId:null,assigneeName:null,status:"pending",revision:1,updatedAt:Date.now()});return {id};}
   if(args.action==="ask"){const p=args.projectId===remote.id?remote:shared;return {answer:"当前任务待验收，建议先核对交付成果。[来源1]",verification:"extractive_fallback",insufficient:true,gaps:["部分资料仍在处理中"],projectRevision:p.revision,updatedAt:p.updatedAt,citations:[{id:"来源1",title:"项目规范",resourceId:"shared-document",kind:"md",quote:"旧摘录",openUrl:"/docs/shared-document"}]};}
   if(args.action==="createComment"){shared.comments!.unshift({id:args.body.clientRequestId,content:args.body.content,createdAt:Date.now(),actorName:"项目负责人"});return {id:args.body.clientRequestId};}
   return structuredClone(args.projectId===remote.id?remote:shared);
  }
  case "org_project_upload":shared.assets=[{id:"shared-document",familyId:"shared-family",title:"需求说明.md",version:1,status:"ready",ftsStatus:"ready",byteSize:100,currentDocumentId:"shared-document",updatedAt:Date.now()}];return {docId:"shared-document",familyId:"shared-family",status:"ready"};
  case "org_fetch_document":return {text:"项目规范原文：结论需保留来源，任务完成后由负责人验收。"};
  case "projects_save":return;
  case "projects_load":return [project];
  case "project_assets_use":return args.paths??["/review/reference.md"];
  case "project_assets_import":return args.sources.map((path:string,i:number)=>({name:path.split('/').pop(),path:`/review/project-files/${Date.now()}-${i}.md`,kind:"file",ext:"md",sizeBytes:120,updatedAt:new Date().toISOString()}));
  case "filesystem_pick_files":return workspaceReview?["/review/new-reference.md"]:[];
  case "filesystem_pick_directory":return "/review/workspace";
  case "filesystem_attachment_stats":return {files:(args.paths??[]).map((path:string)=>({inputPath:path,path,sizeBytes:120})),rejected:[]};
  case "path_stat":return {absolute:args.path,exists:true,kind:"file"};
  case "read_text_file":return "# 竞品分析报告\n\n## 主要发现\n\n项目上下文复用、进度呈现和成果管理应形成完整流程。\n\n## 建议行动\n\n优先验证三项改进方向，并保留原始反馈来源。";
  case "session_exports_catalog":return [];
  case "automations_snapshot":return {automations:scheduled,records};
  case "automations_save":{const saved={...args.automation,id:args.automation.id||`automation-${scheduled.length}`,createdAt:new Date().toISOString()};const i=scheduled.findIndex(a=>a.id===saved.id);if(i<0)scheduled.push(saved);else scheduled[i]=saved;return saved;}
  case "automations_set_status":{const a=scheduled.find(a=>a.id===args.id);if(a)a.status=args.status;return;}
  case "automations_run":{records.unshift({id:"scheduled-run",automationId:args.id,automationName:"项目周报",status:"running",startedAt:new Date().toISOString(),sessionId:"scheduled-session",cwd:"/review/workspace",archived:false});return "scheduled-run";}
  case "agent_cancel":return;
  case "permission_mode_get":return {permissionMode:"ask",configuredPermissionMode:"ask",locked:false};
  case "plugin:event|listen":return 1;
  case "plugin:event|unlisten":return;
  case "group_team_templates":return [];
  default:return [];
 }
}},__TAURI_EVENT_PLUGIN_INTERNALS__:{unregisterListener:()=>{}}});
useProjectsStore.setState({projects:[project],persistError:null,persisting:false});
useOrgSessionStore.setState({hydrated:true,boundaryRevision:1,session:{loggedIn:true,accountId:"review-account",accountGeneration:1,serverUrl:"https://isolated.example",user:{id:"a",username:"owner",displayName:"项目负责人",role:"member",clearance:0}}});
createRoot(document.getElementById("root")!).render(<ThemeProvider><ModelCatalogContext.Provider value={models}>{params.get("surface")==="expert"?<AssistantsPanel/>:<ProjectsPanel models={models} defaultModelId={models[0].id} onOpenProjectWorkItemSession={(projectId,sessionId,cwd)=>{calls.push({command:"review_open_project_session",args:{projectId,sessionId,cwd}});}} onStartProjectConversation={async(projectId,text,modelId,options)=>{const attachments=await options.prepareAttachments?.();calls.push({command:"review_start_project_task",args:{projectId,text,modelId,attachments,cwd:options.cwd}});const sessionId=`new-session-${Date.now()}`;useProjectsStore.getState().addConversation(projectId,{sessionId,title:text,createdAt:new Date().toISOString(),modelId});useSessionsStore.getState().upsert({sessionId,title:text,cwd:options.cwd??"/review/task",status:"working"});return sessionId;}}/>}</ModelCatalogContext.Provider></ThemeProvider>);
