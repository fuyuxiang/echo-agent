import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { PublishTeamProjectButton } from "../PublishTeamProjectButton";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useProjectsStore, type ProjectMeta } from "@/stores/projects-store";
import { projectSharingAccount, publicationKey } from "@/lib/project-sharing";
const api = vi.hoisted(() => ({ teamProjectRequest: vi.fn(), projectCollaborationCapabilities: vi.fn(), teamProjectUpload: vi.fn(), projectAssetsUse: vi.fn() }));
vi.mock("@/lib/team-project-client", async original => ({ ...await original<object>(), ...api }));
vi.mock("@/lib/agent-client", () => ({ projectAssetsUse: api.projectAssetsUse, projectsSave: vi.fn().mockResolvedValue(undefined) }));
vi.mock("@/lib/org-client", () => ({ orgSession: vi.fn() }));
const project: ProjectMeta = { id: "local", name: "接口项目", instructions: "完成联调", createdAt: "2026-11-16", members: ["私人备注"], conversations: [{ sessionId: "private", title: "私人探索", createdAt: "2026-11-16" }], plans: [], tasks: [{ id: "t", title: "设计工作", status: "pending", scope: "personal", source: "manual" }], assets: [{ id: "file", name: "规范.md", path: "/local/规范.md", kind: "file" }], experts: [], skills: [], connectors: [] };
beforeEach(() => {
 vi.resetAllMocks(); localStorage.clear();
 useProjectsStore.setState({projects:[]});
 api.projectCollaborationCapabilities.mockResolvedValue({ version: 2, idempotentCreate: true, memberSearch: true, memberRoles: true });
 useOrgSessionStore.setState({ session: { loggedIn: true, serverUrl: "https://org.example", user: { id: "a", username: "a", displayName: "A", role: "member", clearance: 0 } }, boundaryRevision: 1 });
 api.teamProjectRequest.mockResolvedValue({ id: "shared" }); api.projectAssetsUse.mockResolvedValue(["/authorized/规范.md"]);
});

it("marks workspace-shared content as published and does not replace its remote identity", async () => {
 const account=projectSharingAccount(useOrgSessionStore.getState().session);
 const p:ProjectMeta={...project,sharedProjects:{[account]:"shared"},tasks:[{...project.tasks[0],sharedTasks:{[account]:{projectId:"shared",taskId:"original-task"}}}],assets:[{...project.assets[0],sharedAssets:{[account]:{projectId:"shared",documentId:"original-doc",familyId:"original-family"}}}]};
 useProjectsStore.setState({projects:[p]}); const published=vi.fn();
 render(<PublishTeamProjectButton project={p} collaborationMode onPublished={published}/>);
 fireEvent.click(screen.getByRole("button",{name:"共享更多内容"}));
 expect(screen.getByRole("checkbox",{name:/任务：设计工作/})).toBeDisabled();
 expect(screen.getByRole("checkbox",{name:/规范.md/})).toBeChecked();
 expect(screen.getByRole("checkbox",{name:/规范.md/})).toBeDisabled();
 fireEvent.click(screen.getByRole("button",{name:"确认共享"})); await waitFor(()=>expect(published).toHaveBeenCalledWith("shared"));
 expect(api.teamProjectRequest).not.toHaveBeenCalled(); expect(api.teamProjectUpload).not.toHaveBeenCalled();
 expect(useProjectsStore.getState().projects[0].tasks[0].sharedTasks?.[account].taskId).toBe("original-task");
});

it("rechecks durable links when content is shared elsewhere while the selection dialog is open",async()=>{
 const account=projectSharingAccount(useOrgSessionStore.getState().session),p={...project,sharedProjects:{[account]:"shared"}};
 useProjectsStore.setState({projects:[p]}); const published=vi.fn(); render(<PublishTeamProjectButton project={p} collaborationMode onPublished={published}/>);
 fireEvent.click(screen.getByRole("button",{name:"共享更多内容"}));
 fireEvent.click(screen.getByRole("checkbox",{name:"任务：设计工作"})); fireEvent.click(screen.getByRole("checkbox",{name:"规范.md"}));
 await act(async()=>{
  await useProjectsStore.getState().linkSharedTask(p.id, "t", account, {projectId:"shared",taskId:"existing"});
  await useProjectsStore.getState().linkSharedAsset(p.id, "file", account, {projectId:"shared",documentId:"existing-doc"});
 });
 fireEvent.click(screen.getByRole("button",{name:"确认共享"})); await waitFor(()=>expect(published).toHaveBeenCalled());
 expect(api.teamProjectRequest).not.toHaveBeenCalled(); expect(api.projectAssetsUse).not.toHaveBeenCalled(); expect(api.teamProjectUpload).not.toHaveBeenCalled();
});

it("reuses a workspace task's pending creation request rather than allocating another identity",async()=>{
 const account=projectSharingAccount(useOrgSessionStore.getState().session);
 const p={...project,sharedProjects:{[account]:"shared"},tasks:[{...project.tasks[0],sharedTasks:{[account]:{projectId:"shared",requestId:"existing-request"}}}]};
 useProjectsStore.setState({projects:[p]});const published=vi.fn();render(<PublishTeamProjectButton project={p} collaborationMode onPublished={published}/>);
 fireEvent.click(screen.getByRole("button",{name:"共享更多内容"}));fireEvent.click(screen.getByRole("checkbox",{name:"任务：设计工作"}));fireEvent.click(screen.getByRole("button",{name:"确认共享"}));
 await waitFor(()=>expect(published).toHaveBeenCalled());expect(api.teamProjectRequest).toHaveBeenCalledWith("createTask","shared",expect.objectContaining({clientRequestId:"existing-request"}),undefined,expect.any(Object));
});

it("restores a successful upload from recovery data without uploading a second document",async()=>{
 const account=projectSharingAccount(useOrgSessionStore.getState().session),p={...project,sharedProjects:{[account]:"shared"}};
 useProjectsStore.setState({projects:[p]});
 localStorage.setItem(publicationKey(p.id,account),JSON.stringify({projectId:"shared",files:[],tasks:[],createdTasks:{},createdAssets:{file:"uploaded-doc"},createdAssetFamilies:{file:"uploaded-family"},selectedFiles:["file"],selectedTasks:[]}));
 const published=vi.fn();render(<PublishTeamProjectButton project={p} collaborationMode onPublished={published}/>);
 fireEvent.click(screen.getByRole("button",{name:"共享更多内容"}));fireEvent.click(screen.getByRole("button",{name:"确认共享"}));await waitFor(()=>expect(published).toHaveBeenCalled());
 expect(api.teamProjectUpload).not.toHaveBeenCalled(); expect(useProjectsStore.getState().projects[0].assets[0].sharedAssets?.[account]).toMatchObject({documentId:"uploaded-doc",familyId:"uploaded-family"});
});
describe("personal project publication", () => {
 it("only publishes explicitly selected content and never uploads private chats or participant notes", async () => {
  const published = vi.fn(); render(<PublishTeamProjectButton project={project} onPublished={published} />);
  fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
  expect(api.teamProjectRequest).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
  await waitFor(() => expect(published).toHaveBeenCalledWith("shared"));
  expect(api.teamProjectRequest).toHaveBeenCalledWith("create", undefined, { name: "接口项目", goal: "完成联调" }, undefined, expect.objectContaining({ assertCurrent: expect.any(Function) }));
  expect(api.teamProjectUpload).not.toHaveBeenCalled();
  expect(api.projectAssetsUse).not.toHaveBeenCalled();
  expect(api.teamProjectRequest).toHaveBeenCalledTimes(1);
 });
 it.each(["对外共享的项目目标", ""])("publishes the explicit project description without copying local instructions: %s", async description => {
  const published = vi.fn(); render(<PublishTeamProjectButton project={{...project, description, instructions:"仅本机的执行规范"}} onPublished={published} />);
  fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
  fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
  await waitFor(() => expect(published).toHaveBeenCalledWith("shared"));
  expect(api.teamProjectRequest).toHaveBeenCalledWith("create", undefined, {name: project.name, goal: description}, undefined, expect.any(Object));
 });
 it("keeps the shared project after a partial upload failure and retries the remaining selected file", async () => {
  api.teamProjectUpload.mockRejectedValueOnce(new Error("连接中断")).mockResolvedValue({ docId: "doc" });
  const published = vi.fn(); render(<PublishTeamProjectButton project={project} onPublished={published} />);
  fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "规范.md" }));
  fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("团队项目已创建");
  fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
  await waitFor(() => expect(published).toHaveBeenCalledWith("shared"));
  expect(api.teamProjectRequest).toHaveBeenCalledTimes(1);
  expect(api.teamProjectUpload).toHaveBeenCalledTimes(2);
 });
 it("reuses a created work item when its status update needs a retry", async () => {
  let attempts = 0;
  api.teamProjectRequest.mockImplementation(async (action: string) => {
   if (action === "snapshot") return { tasks: [{ id: "remote-task", revision: 1, status: "pending" }] };
   if (action === "updateTask" && attempts++ === 0) throw new Error("连接中断");
   return { id: action === "createTask" ? "remote-task" : "shared" };
  });
  const published = vi.fn();
  render(<PublishTeamProjectButton project={{ ...project, tasks: [{ ...project.tasks[0], status: "completed" }] }} onPublished={published} />);
  fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "任务：设计工作" }));
  fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("部分内容尚未发布");
  fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
  await waitFor(() => expect(published).toHaveBeenCalledWith("shared"));
  expect(api.teamProjectRequest.mock.calls.filter(([action]) => action === "createTask")).toHaveLength(1);
 });
});

it("resumes the same publication after leaving the project and coming back", async () => {
 api.teamProjectUpload.mockRejectedValueOnce(new Error("连接中断")).mockResolvedValue({ docId: "doc" });
 const first = render(<PublishTeamProjectButton project={project} onPublished={vi.fn()} />);
 fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
 fireEvent.click(screen.getByRole("checkbox", { name: "规范.md" }));
 fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
 await screen.findByRole("alert"); first.unmount();
 const published = vi.fn(); render(<PublishTeamProjectButton project={project} onPublished={published} />);
 fireEvent.click(screen.getByRole("button", { name: "继续发布" }));
 expect(screen.getByRole("checkbox", { name: "规范.md" })).toBeChecked();
 fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
 await waitFor(() => expect(published).toHaveBeenCalledWith("shared"));
 expect(api.teamProjectRequest.mock.calls.filter(([action]) => action === "create")).toHaveLength(1);
});
it("does not upload a local file after an account switch during authorization", async () => {
 let complete!: (paths: string[]) => void;
 api.projectAssetsUse.mockImplementation(() => new Promise<string[]>(resolve => { complete = resolve; }));
 render(<PublishTeamProjectButton project={project} onPublished={vi.fn()} />);
 fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
 fireEvent.click(screen.getByRole("checkbox", { name: "规范.md" }));
 fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
 await waitFor(() => expect(api.projectAssetsUse).toHaveBeenCalled());
 act(() => useOrgSessionStore.setState({ boundaryRevision: 2 }));
 await act(async () => { complete(["/authorized/规范.md"]); });
 expect(api.teamProjectUpload).not.toHaveBeenCalled();
});
it("does not automatically create another project after an uncertain create response", async () => {
 api.teamProjectRequest.mockRejectedValueOnce(new Error("连接中断"));
 const first = render(<PublishTeamProjectButton project={project} onPublished={vi.fn()} />);
 fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
 fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
 expect(await screen.findByRole("alert")).toHaveTextContent("创建结果尚未确认");
 first.unmount();
 render(<PublishTeamProjectButton project={project} onPublished={vi.fn()} />);
 expect(screen.getByRole("button", { name: "确认创建结果" })).toBeInTheDocument();
 expect(screen.queryByRole("button", { name: "发布为团队项目" })).toBeNull();
 expect(api.teamProjectRequest.mock.calls.filter(([action]) => action === "create")).toHaveLength(1);
});
it("finishes an authorized publication in the background without taking over later navigation", async () => {
 let finish!: (result: { docId: string }) => void;
 api.teamProjectUpload.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
 const published = vi.fn();
 const first = render(<PublishTeamProjectButton project={project} onPublished={published} />);
 fireEvent.click(screen.getByRole("button", { name: "发布为团队项目" }));
 fireEvent.click(screen.getByRole("checkbox", { name: "规范.md" }));
 fireEvent.click(screen.getByRole("button", { name: "发布并管理成员" }));
 await waitFor(() => expect(api.teamProjectUpload).toHaveBeenCalled());
 first.unmount();
 render(<PublishTeamProjectButton project={project} onPublished={published} />);
 expect(screen.getByRole("button", { name: "继续发布" })).toBeDisabled();
 await act(async () => { finish({ docId: "doc" }); });
 expect(published).not.toHaveBeenCalled();
 expect(api.teamProjectUpload).toHaveBeenCalledTimes(1);
 expect(screen.getByRole("button", { name: "打开团队项目" })).toBeInTheDocument();
 expect(screen.getByRole("button", { name: "补充发布内容" })).toBeInTheDocument();
});

it("retries a lost first-sharing response with the same server idempotency key and records the association before uploads", async () => {
 const linked = vi.fn().mockResolvedValue(undefined), published = vi.fn();
 api.teamProjectRequest.mockImplementation(async (action: string) => {
  if (action === "capabilities") return { version: 2, idempotentCreate: true };
  if (action === "create" && api.teamProjectRequest.mock.calls.filter(([name]) => name === "create").length === 1) throw new Error("连接中断");
  return { id: "same-shared-project" };
 });
 render(<PublishTeamProjectButton project={project} collaborationMode onLinked={linked} onPublished={published} />);
 fireEvent.click(screen.getByRole("button", { name: "开启协作并选择共享内容" }));
 fireEvent.click(screen.getByRole("button", { name: "确认共享" }));
 await screen.findByRole("alert");
 fireEvent.click(screen.getByRole("button", { name: "确认共享" }));
 await waitFor(() => expect(published).toHaveBeenCalledWith("same-shared-project"));
 const creates = api.teamProjectRequest.mock.calls.filter(([name]) => name === "create");
 expect(creates).toHaveLength(2); expect(creates[0][2].clientRequestId).toBeTruthy(); expect(creates[1][2].clientRequestId).toBe(creates[0][2].clientRequestId);
 expect(linked).toHaveBeenCalledWith("same-shared-project");
});
