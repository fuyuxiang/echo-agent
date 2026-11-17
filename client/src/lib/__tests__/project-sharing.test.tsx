import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useProjectsStore, flushProjectPersistence, type ProjectMeta } from "@/stores/projects-store";
import { projectSharingAccount, publicationKey, sharedProjectId, useSharedProjectCatalog } from "../project-sharing";
const api = vi.hoisted(() => ({ teamProjectRequest: vi.fn(), projectsSave: vi.fn() }));
vi.mock("../team-project-client", async original => ({ ...await original<object>(), teamProjectRequest: api.teamProjectRequest }));
vi.mock("../agent-client", () => ({ projectsSave: api.projectsSave, projectsLoad: vi.fn() }));
vi.mock("../org-client", () => ({ orgSession: vi.fn() }));
const project: ProjectMeta = { id:"local",name:"原项目",createdAt:"2026-11-17",tasks:[],plans:[],conversations:[{sessionId:"private",title:"个人对话",createdAt:"2026-11-17"}],members:["旧备注"],assets:[],experts:[],skills:[{id:"review",name:"审查"}],connectors:[] };
beforeEach(async () => {
 await flushProjectPersistence().catch(() => {}); vi.clearAllMocks(); localStorage.clear();
 api.projectsSave.mockResolvedValue(undefined);
 useProjectsStore.setState({ projects:[project],persistError:null,persisting:false });
 useOrgSessionStore.setState({session:{loggedIn:true,accountId:"account-A",accountGeneration:1,user:{id:"a",username:"a",displayName:"A",role:"member",clearance:0}},boundaryRevision:1});
});
it("reuses legacy sharing records and never associates another account's remote project", () => {
 const account=projectSharingAccount(useOrgSessionStore.getState().session);
 localStorage.setItem(publicationKey(project.id,account),JSON.stringify({projectId:"legacy-shared"}));
 expect(sharedProjectId(project,account)).toBe("legacy-shared"); expect(sharedProjectId(project,"account-B")).toBeUndefined();
});
it("persists per-account associations without changing local chats, notes, skills or prior account links",async()=>{
 await useProjectsStore.getState().linkSharedProject(project.id,"account-A","shared-A");
 await useProjectsStore.getState().linkSharedProject(project.id,"account-B","shared-B");
 const saved=useProjectsStore.getState().projects[0];
 expect(saved).toMatchObject({...project,tasks:[{sessionId:"private",title:"个人对话",source:"conversation",scope:"personal"}],sharedProjects:{"account-A":"shared-A","account-B":"shared-B"}});
 expect(saved.conversations).toEqual(project.conversations);
 expect(saved.tasks[0].sharedTasks).toBeUndefined();
 expect(api.projectsSave).toHaveBeenLastCalledWith([saved]);
});
it("discards delayed shared catalogs when an account changes",async()=>{
 let finish!:(value:unknown)=>void; api.teamProjectRequest.mockImplementation(()=>new Promise(resolve=>{finish=resolve}));
 function Probe(){const catalog=useSharedProjectCatalog();return <span>{catalog.projects.map(p=>p.name).join(",") || "empty"}</span>}
 render(<Probe/>); await waitFor(()=>expect(api.teamProjectRequest).toHaveBeenCalled());
 act(()=>useOrgSessionStore.setState({session:null,boundaryRevision:2}));
 await act(async()=>finish([{id:"private-shared",name:"旧账号项目"}]));
 expect(screen.getByText("empty")).toBeInTheDocument();expect(screen.queryByText("旧账号项目")).toBeNull();
});
