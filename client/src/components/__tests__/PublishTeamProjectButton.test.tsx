import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { PublishTeamProjectButton } from "../PublishTeamProjectButton";
import { useOrgSessionStore } from "@/stores/org-session-store";
import type { ProjectMeta } from "@/stores/projects-store";
const api = vi.hoisted(() => ({ teamProjectRequest: vi.fn(), teamProjectUpload: vi.fn(), projectAssetsUse: vi.fn() }));
vi.mock("@/lib/team-project-client", async original => ({ ...await original<object>(), ...api }));
vi.mock("@/lib/agent-client", () => ({ projectAssetsUse: api.projectAssetsUse }));
vi.mock("@/lib/org-client", () => ({ orgSession: vi.fn() }));
const project: ProjectMeta = { id: "local", name: "接口项目", instructions: "完成联调", createdAt: "2026-11-16", members: ["私人备注"], conversations: [{ sessionId: "private", title: "私人探索", createdAt: "2026-11-16" }], plans: [], tasks: [{ id: "t", title: "设计工作", status: "pending", scope: "personal", source: "manual" }], assets: [{ id: "file", name: "规范.md", path: "/local/规范.md", kind: "file" }], experts: [], skills: [], connectors: [] };
beforeEach(() => {
 vi.resetAllMocks(); localStorage.clear();
 useOrgSessionStore.setState({ session: { loggedIn: true, serverUrl: "https://org.example", user: { id: "a", username: "a", displayName: "A", role: "member", clearance: 0 } }, boundaryRevision: 1 });
 api.teamProjectRequest.mockResolvedValue({ id: "shared" }); api.projectAssetsUse.mockResolvedValue(["/authorized/规范.md"]);
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
  fireEvent.click(screen.getByRole("checkbox", { name: "工作项：设计工作" }));
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
