import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { ProjectDetailView } from "../ProjectDetailView";
import { ProjectMaterials } from "../ProjectMaterials";
import { agentCancel, agentListPendingInteractions, projectAssetsUse } from "@/lib/agent-client";
import { ProjectTasks } from "../ProjectTasks";
import { ProjectSettingsDialog } from "../ProjectSettingsDialog";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { usePermissionModeStore } from "@/stores/permission-mode-store";
import type { TeamProjectSnapshot } from "@/lib/team-project-client";
import type { ProjectWorkspace } from "@/lib/use-project-workspace";
const api=vi.hoisted(()=>({request:vi.fn(),caps:vi.fn(),upload:vi.fn()}));
vi.mock("@/lib/team-project-client",async original=>({...await original<object>(),teamProjectRequest:api.request,projectCollaborationCapabilities:api.caps,teamProjectUpload:api.upload}));
vi.mock("@/lib/agent-client",()=>({agentCancel:vi.fn(),agentListPendingInteractions:vi.fn(),projectsSave:vi.fn().mockResolvedValue(undefined),projectAssetsUse:vi.fn().mockResolvedValue(["/private/result.md"]),filesystemPickFiles:vi.fn().mockResolvedValue([]),permissionModeGet:vi.fn().mockResolvedValue({permissionMode:"ask",configuredPermissionMode:"ask",locked:false})}));
const project:ProjectMeta={id:"p",name:"统一项目",createdAt:"2026-01-01",instructions:"原指令",defaultModelId:"m",tasks:[],conversations:[],assets:[],plans:[],skills:[{id:"old-skill",name:"保留技能"}],experts:[],connectors:[],members:["原备注"],sharedProjects:{account:"shared"}};
const snapshot:TeamProjectSnapshot={id:"shared",name:project.name,goal:"共享说明",scopeId:"scope",ownerId:"a",role:"admin",revision:1,updatedAt:100,members:[],tasks:[],assets:[],runs:[],events:[],decisions:[]};
const picker={options:{skills:[],experts:[],connectors:[]},loading:false,error:null};
const models=[{id:"m",label:"默认模型"}];
const workspace=()=>({snapshot:structuredClone(snapshot),loading:false,error:"",refresh:vi.fn().mockResolvedValue(undefined),mutate:vi.fn().mockResolvedValue({id:"remote"})}) as unknown as ProjectWorkspace;
beforeEach(()=>{
 localStorage.clear();vi.clearAllMocks();api.caps.mockResolvedValue({version:3,taskIdentity:true});
 useProjectsStore.setState({projects:[structuredClone(project)],persistError:null,persisting:false,draftPermissions:{}});
 useSessionsStore.setState({independent:[],drafts:{}});usePermissionModeStore.setState(usePermissionModeStore.getInitialState());
 useOrgSessionStore.setState({boundaryRevision:1,session:{loggedIn:true,accountId:"account",accountGeneration:1,serverUrl:"https://example.test",user:{id:"a",username:"a",displayName:"A",role:"member",clearance:0}}});
 api.request.mockResolvedValue(structuredClone(snapshot));
});
it("keeps old private conversations inside tasks and can restore and continue the exact history",async()=>{
 const p={...project,conversations:[{sessionId:"old",title:"历史讨论",createdAt:"2026-01-01",archived:true}]};useProjectsStore.setState({projects:[p]});
 const restore=vi.fn().mockResolvedValue(undefined),open=vi.fn();render(<ProjectTasks project={p} account="account" sharedId="shared" workspace={workspace()} models={models} onOpenSession={open} onArchiveSession={restore}/>);
 expect(screen.queryByRole("button",{name:/历史讨论/})).toBeNull();fireEvent.change(screen.getByRole("combobox",{name:"任务范围"}),{target:{value:"all"}});
 fireEvent.click(screen.getByRole("button",{name:/历史讨论/}));fireEvent.click(screen.getByRole("button",{name:"恢复并继续任务"}));
 await waitFor(()=>expect(restore).toHaveBeenCalledWith("old",false));expect(open).toHaveBeenCalledWith("old");expect(p.conversations[0].sessionId).toBe("old");
});

it("checks pending interactions in the project detail after a pause fails", async () => {
 const sessionId = "running-project-task";
 const p = {...project, conversations:[{sessionId,title:"运行中的任务",createdAt:"2026-01-01"}]};
 useSessionsStore.getState().upsert({sessionId,title:"运行中的任务",cwd:"/workspace",status:"working"});
 useSessionStore.getState().startStreaming(sessionId, "running-prompt");
 vi.mocked(agentCancel).mockRejectedValueOnce(new Error("暂停回执丢失"));
 let respond!: (value: Awaited<ReturnType<typeof agentListPendingInteractions>>) => void;
 vi.mocked(agentListPendingInteractions).mockImplementationOnce(() => new Promise(resolve => { respond = resolve; }));
 render(<ProjectTasks project={p} account="account" workspace={workspace()} models={models} />);
 fireEvent.click(screen.getByRole("button", {name:/运行中的任务/}));
 const task = within(screen.getByRole("dialog", {name:"任务：运行中的任务"}));
 fireEvent.click(task.getByRole("button", {name:"暂停任务"}));
 expect(await task.findByText("正在检查待处理事项…")).toBeInTheDocument();
 await act(async () => respond({permissions:[],questions:[],planApprovals:[],folderTrustRequests:[]}));
 expect(task.queryByText("正在检查待处理事项…")).toBeNull();
 expect(useSessionsStore.getState().independent.find(item => item.sessionId === sessionId)?.status).toBe("working");
});
it("does not turn a linked task into an editable private copy when the server is unavailable",()=>{
 const p={...project,tasks:[{id:"t",title:"共享任务",source:"manual",scope:"personal" as const,status:"pending" as const,sharedTasks:{account:{projectId:"shared",taskId:"r"}}}]};
 render(<ProjectTasks project={p} account="account" sharedId="shared" workspace={{...workspace(),snapshot:null,error:"网络中断"}} models={models}/>);
 fireEvent.click(screen.getByRole("button",{name:/共享任务/}));expect(screen.queryByRole("button",{name:"编辑任务"})).toBeNull();expect(screen.queryByRole("combobox",{name:"任务状态"})).toBeNull();
});
it("keeps an archived private source reachable after its task is shared, even for a project viewer",async()=>{
 const p:ProjectMeta={...project,tasks:[{id:"t",title:"已发布的任务",source:"manual",scope:"personal",status:"pending",sessionId:"private-source",sessionArchived:true,sharedTasks:{account:{projectId:"shared",taskId:"r"}}}],conversations:[{sessionId:"private-source",title:"原私人讨论",createdAt:project.createdAt,archived:true}]};
 useProjectsStore.setState({projects:[p]});const ws=workspace();ws.snapshot={...snapshot,role:"viewer",tasks:[{id:"r",title:"已发布的任务",description:"",acceptance:"",status:"pending",revision:1,assigneeId:null,assigneeName:null,updatedAt:100}]};
 const restore=vi.fn().mockResolvedValue(undefined),open=vi.fn(),start=vi.fn();
 render(<ProjectTasks project={p} account="account" sharedId="shared" workspace={ws} models={models} onOpenSession={open} onArchiveSession={restore} onStartTeamWork={start}/>);
 fireEvent.click(screen.getByRole("button",{name:/已发布的任务/}));fireEvent.click(screen.getByRole("button",{name:"恢复并打开本机原会话"}));
 await waitFor(()=>expect(open).toHaveBeenCalledWith("private-source"));expect(restore).toHaveBeenCalledWith("private-source",false);expect(start).not.toHaveBeenCalled();
});
it("does not describe a native-bound shared execution as a former private conversation",()=>{
 const p:ProjectMeta={...project,tasks:[{id:"t",title:"共享执行",source:"manual",scope:"personal",status:"pending",sessionId:"shared-session",sharedTasks:{account:{projectId:"shared",taskId:"r"}}}]};
 const ws=workspace();ws.snapshot={...snapshot,tasks:[{id:"r",title:"共享执行",description:"",acceptance:"",status:"pending",revision:1,assigneeId:null,assigneeName:null,updatedAt:100}],runs:[{id:"run",taskId:"r",actorId:"a",sessionId:"shared-session",status:"completed",summary:"报告",modelId:"m",expertName:null,expertVersion:null,updatedAt:100}]};
 render(<ProjectTasks project={p} account="account" sharedId="shared" workspace={ws} models={models} onOpenSession={vi.fn()} onOpenTeamExecution={vi.fn()}/>);
 fireEvent.click(screen.getByRole("button",{name:/共享执行/}));expect(screen.queryByRole("button",{name:"打开本机原会话"})).toBeNull();expect(screen.getByRole("button",{name:"打开并继续任务"})).toBeInTheDocument();
});
it("offers read-only project Q&A from the actual unified workspace without starting a task",async()=>{
 api.request.mockImplementation(async(action:string)=>action==="ask"?{answer:"任务待验收",projectRevision:1,updatedAt:100,verification:"supported",insufficient:false,gaps:[],citations:[]}:{...snapshot,role:"viewer"});
 const start=vi.fn();render(<ProjectDetailView project={project} onBack={vi.fn()} models={[]} picker={picker} onStartTeamWork={start}/>);
 const ask=screen.getByRole("button",{name:"问项目"});
 await waitFor(()=>expect(ask).toBeEnabled());
 fireEvent.click(ask);
 const dialog=screen.getByRole("dialog",{name:"问项目"});fireEvent.change(within(dialog).getByRole("textbox",{name:"项目问题"}),{target:{value:"进展如何？"}});
 await waitFor(()=>expect(within(dialog).getByRole("button",{name:"查询项目"})).toBeEnabled());fireEvent.click(within(dialog).getByRole("button",{name:"查询项目"}));
 expect(await screen.findByLabelText("项目问答结果")).toHaveTextContent("任务待验收");expect(start).not.toHaveBeenCalled();expect(api.request.mock.calls.some(([action])=>action==="createTask")).toBe(false);
});
it("creates one shared task from the composer and forwards file grants, directory and permission options",async()=>{
 let state=structuredClone(snapshot);api.request.mockImplementation(async(action:string,_id:string,body:any)=>{
  if(action==="createTask") {state.tasks=[{id:"remote",title:body.title,description:body.description,acceptance:"",status:"pending",revision:1,assigneeId:null,assigneeName:null,updatedAt:100}];return {id:"remote"};}
  return structuredClone(state);
 });
 const start=vi.fn().mockResolvedValue("run-session");render(<ProjectDetailView project={project} onBack={vi.fn()} models={models} picker={picker} onStartTeamWork={start}/>);
 const input=screen.getByRole("textbox",{name:"输入问题或任务"});fireEvent.change(input,{target:{value:"生成交付报告"}});
 const send=screen.getByRole("button",{name:"发送"});await waitFor(()=>expect(send).toBeEnabled());fireEvent.click(send);
 await waitFor(()=>expect(start).toHaveBeenCalledOnce());
 expect(start.mock.calls[0][3]).toMatchObject({localProjectId:"p",options:{cwd:null,expectedPermissionMode:"ask",sourceTab:"task"}});
 expect(await start.mock.calls[0][3].options.prepareAttachments()).toEqual([]);
 await waitFor(()=>expect(useProjectsStore.getState().projects[0].tasks).toMatchObject([{title:"生成交付报告",sessionId:"run-session",sharedTasks:{account:{projectId:"shared",taskId:"remote"}}}]));
 expect(api.request.mock.calls.filter(([a])=>a==="createTask")).toHaveLength(1);expect(input).toHaveValue("");
});
it("retains the draft and retries the same shared task identity after a lost create response",async()=>{
 let attempts=0,state=structuredClone(snapshot);api.request.mockImplementation(async(action:string,_id:string,body:any)=>{
  if(action==="createTask") {if(attempts++===0) throw new Error("响应丢失");state.tasks=[{id:"remote",title:body.title,description:body.description,acceptance:"",status:"pending",revision:1,assigneeId:null,assigneeName:null,updatedAt:100}];return {id:"remote"};}
  return structuredClone(state);
 });
 render(<ProjectDetailView project={project} onBack={vi.fn()} models={models} picker={picker} onStartTeamWork={vi.fn().mockResolvedValue("session")}/>);
 const input=screen.getByRole("textbox",{name:"输入问题或任务"});fireEvent.change(input,{target:{value:"保留同一任务"}});const send=screen.getByRole("button",{name:"发送"});await waitFor(()=>expect(send).toBeEnabled());fireEvent.click(send);
 await screen.findByText(/响应丢失/);expect(input).toHaveValue("保留同一任务");expect(useProjectsStore.getState().projects[0].tasks).toHaveLength(1);
 await waitFor(()=>expect(send).toBeEnabled());fireEvent.click(send);await waitFor(()=>expect(input).toHaveValue(""));
 const calls=api.request.mock.calls.filter(([a])=>a==="createTask");expect(calls).toHaveLength(2);expect(calls[0][2].clientRequestId).toBe(calls[1][2].clientRequestId);expect(useProjectsStore.getState().projects[0].tasks).toHaveLength(1);
});
it("only saves explicit settings and preserves legacy instructions, unavailable capabilities and member notes",async()=>{
 const p=structuredClone(project),close=vi.fn(),ws=workspace();render(<ProjectSettingsDialog project={p} sharedId="shared" workspace={ws} models={models} picker={picker} onClose={close}/>);
 fireEvent.change(screen.getByRole("textbox",{name:"项目说明"}),{target:{value:"新的共享说明"}});
 expect(useProjectsStore.getState().projects[0].instructions).toBe("原指令");fireEvent.click(screen.getByRole("button",{name:"保存设置"}));await waitFor(()=>expect(close).toHaveBeenCalledOnce());
 expect(ws.mutate).toHaveBeenCalledWith("update",{name:project.name,goal:"新的共享说明",expectedRevision:1});
 expect(useProjectsStore.getState().projects[0]).toMatchObject({instructions:"原指令",skills:project.skills,members:project.members});
});
it("discards a cancelled settings draft without changing the project",async()=>{
 render(<ProjectSettingsDialog project={project} workspace={workspace()} models={models} picker={picker} onClose={vi.fn()}/>);
 fireEvent.change(screen.getByRole("textbox",{name:"项目说明"}),{target:{value:"不保存"}});fireEvent.click(screen.getByRole("button",{name:"取消"}));
 const confirmation=await screen.findByRole("alertdialog",{name:"舍弃未保存的修改？"});await act(async()=>fireEvent.click(within(confirmation).getByRole("button",{name:"舍弃修改"})));
 expect(useProjectsStore.getState().projects[0].instructions).toBe("原指令");
});
it("reuses a saved draft task after reopening the project instead of publishing a duplicate",async()=>{
 const p={...project,tasks:[{id:"pending",title:"待恢复",description:"恢复同一草稿",source:"project-draft",scope:"personal" as const,status:"pending" as const,modelId:"m",sharedTasks:{account:{projectId:"shared",requestId:"00000000-0000-4000-8000-000000000003"}}}]};useProjectsStore.setState({projects:[p]});useSessionsStore.getState().setDraft("project:p","恢复同一草稿");
 let state=structuredClone(snapshot);api.request.mockImplementation(async(action:string,_id:string,body:any)=>{
  if(action==="createTask") {state.tasks=[{id:"remote",title:body.title,description:body.description,acceptance:"",status:"pending",revision:1,assigneeId:null,assigneeName:null,updatedAt:100}];return {id:"remote"};}return structuredClone(state);
 });
 render(<ProjectDetailView project={p} onBack={vi.fn()} models={models} picker={picker} onStartTeamWork={vi.fn().mockResolvedValue("session")}/>);
 const send=screen.getByRole("button",{name:"发送"});await waitFor(()=>expect(send).toBeEnabled());fireEvent.click(send);await waitFor(()=>expect(useProjectsStore.getState().projects[0].tasks[0].sessionId).toBe("session"));
 expect(useProjectsStore.getState().projects[0].tasks).toHaveLength(1);expect(api.request.mock.calls.find(([a])=>a==="createTask")?.[2].clientRequestId).toBe(p.tasks[0].sharedTasks.account.requestId);
});

it("shares an existing local output only after confirmation and restores its exact file grant",async()=>{
 const p={...project,assets:[{id:"out",name:"交付.md",kind:"file" as const,path:"/private/result.md",purpose:"output" as const,sourceSessionId:"source"}]};
 useProjectsStore.setState({projects:[p]});api.upload.mockResolvedValue({docId:"doc",familyId:"family"});
 const ws=workspace();render(<ProjectMaterials project={p} account="account" sharedId="shared" workspace={ws} selectedAssetIds={[]} onUseAsset={vi.fn()}/>);
 fireEvent.click(screen.getByRole("button",{name:"共享到项目"}));
 expect(api.upload).not.toHaveBeenCalled();
 fireEvent.click(within(screen.getByRole("dialog",{name:"共享「交付.md」到项目？"})).getByRole("button",{name:"共享文件"}));
 await waitFor(()=>expect(ws.refresh).toHaveBeenCalled());
 expect(projectAssetsUse).toHaveBeenCalledWith("p",["/private/result.md"],"share");
 expect(api.upload).toHaveBeenCalledWith("shared","/private/result.md",undefined,expect.objectContaining({expectedGeneration:1}));
 expect(useProjectsStore.getState().projects[0].assets[0].sharedAssets?.account).toEqual({projectId:"shared",documentId:"doc",familyId:"family"});
});
