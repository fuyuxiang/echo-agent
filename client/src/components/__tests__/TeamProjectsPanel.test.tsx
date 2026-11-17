import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TeamProjectsPanel } from "../TeamProjectsPanel";
import { useOrgSessionStore } from "@/stores/org-session-store";
import type { ProjectAnswer, TeamProjectSnapshot } from "@/lib/team-project-client";
const api = vi.hoisted(() => ({ teamProjectRequest: vi.fn(), projectCollaborationCapabilities: vi.fn(), teamProjectUpload: vi.fn(), agentsList: vi.fn(), filesystemPickFiles: vi.fn(), orgFetchDocument: vi.fn(), orgDownloadDocument: vi.fn(), groupTemplates: vi.fn() }));
vi.mock("@/lib/team-project-client", async original => ({ ...await original<object>(), teamProjectRequest: api.teamProjectRequest, teamProjectUpload: api.teamProjectUpload, projectCollaborationCapabilities: api.projectCollaborationCapabilities }));
vi.mock("@/lib/agent-client", () => ({ agentsList: api.agentsList, filesystemPickFiles: api.filesystemPickFiles }));
vi.mock("@/lib/org-client", () => ({ orgSession: vi.fn(), orgFetchDocument: api.orgFetchDocument, orgDownloadDocument: api.orgDownloadDocument }));
vi.mock("@/features/groups/group-client", () => ({ groupTemplates: api.groupTemplates }));
const snapshot: TeamProjectSnapshot = { id: "p", name: "接口项目", goal: "完成接口建设", scopeId: "scope", ownerId: "a", revision: 3, updatedAt: 1700000000000, role: "admin",
 members: [{ userId: "a", username: "a", displayName: "A", role: "admin", status: "active" }, { userId: "c", username: "c", displayName: "C", role: "collaborator", status: "active" }],
 tasks: [{ id: "t", title: "接口联调", description: "接续接口方案", acceptance: "联调报告通过", assigneeId: "b", assigneeName: "B", status: "review", revision: 2, updatedAt: 1700000000000 }],
 decisions: [{ id: "d", content: "采用 REST 接口", rationale: "兼容现有系统", status: "active", sourceTaskId: "t", sourceDocumentId: null, supersedesId: null, revision: 2 }],
 assets: [{ id: "file", familyId: "family", title: "接口规范.md", version: 2, status: "parsing", ftsStatus: "pending", byteSize: 200, currentDocumentId: "v1", updatedAt: 1700000000000 }], events: [] };
const answer: ProjectAnswer = { answer: "接口联调待验收 [cit-1]", verification: "extractive_fallback", insufficient: true, projectRevision: 3, updatedAt: 1700000000000, gaps: ["资料还在处理"], citations: [{ id: "cit-1", title: "接口规范", quote: "联调待验收", resourceId: "file", kind: "md", openUrl: "/api/v1/docs/file" }] };
beforeEach(() => {
 vi.resetAllMocks();
 api.projectCollaborationCapabilities.mockResolvedValue({ version: 1, memberSearch: false, memberRoles: false, idempotentCreate: false });
 useOrgSessionStore.setState({ session: { loggedIn: true, accountGeneration: 7, user: { id: "c", username: "c", displayName: "C", role: "member", clearance: 0 } }, hydrated: true, boundaryRevision: 1 });
 api.agentsList.mockResolvedValue([]); api.groupTemplates.mockResolvedValue([]); api.filesystemPickFiles.mockResolvedValue(["/input/report.md"]);
 api.teamProjectRequest.mockImplementation(async action => action === "ask" ? answer : action === "list" ? [snapshot] : snapshot);
});
describe("team project collaboration", () => {
 it("invites organization members with an explicit role and preserves shared history", async () => {
  render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /^参与者/ }));
  const dialog = screen.getByRole("dialog", { name: "项目参与者" });
  fireEvent.change(await within(dialog).findByRole("searchbox", { name: "搜索组织成员" }), { target: { value: "new-member" } });
  fireEvent.change(within(dialog).getByRole("combobox", { name: "邀请成员权限" }), { target: { value: "viewer" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "邀请加入项目" }));
  await waitFor(() => expect(api.teamProjectRequest).toHaveBeenCalledWith("addMember", "p", { username: "new-member", role: "viewer" }, undefined, expect.objectContaining({ assertCurrent: expect.any(Function) })));
 });
 it("offers a sourced newcomer briefing and reauthorizes opening original evidence", async () => {
  api.orgFetchDocument.mockResolvedValue({ text: "接口规范原文" });
  render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "新成员快速了解项目" }));
  expect(await screen.findByLabelText("项目问答结果")).toHaveTextContent("待验收");
  fireEvent.click(screen.getByRole("button", { name: /cit-1 · 接口规范/ }));
  expect(await screen.findByLabelText("项目来源原文")).toHaveTextContent("接口规范原文");
  expect(api.orgFetchDocument).toHaveBeenCalledWith("file", undefined, undefined, 7);
 });
 it("shows processing and effective versions separately; publishes files through the native organization bridge", async () => {
  render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "资料与成果" }));
  expect(screen.getByText(/上一有效版本仍可用于问答/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "发布新版本" }));
  await waitFor(() => expect(api.teamProjectUpload).toHaveBeenCalledWith("p", "/input/report.md", snapshot.assets[0], expect.objectContaining({ assertCurrent: expect.any(Function) })));
 });
 it("requires a fresh question when the source changes after the answer was generated", async () => {
  render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "新成员快速了解项目" }));
  await screen.findByLabelText("项目问答结果");
  api.teamProjectRequest.mockResolvedValue({ ...snapshot, revision: 4 });
  fireEvent.click(screen.getByRole("button", { name: /cit-1 · 接口规范/ }));
  expect(await screen.findByRole("alert")).toHaveTextContent("项目依据已更新");
  expect(api.orgFetchDocument).not.toHaveBeenCalled();
  expect(screen.queryByLabelText("项目来源原文")).toBeNull();
  expect(screen.queryByLabelText("项目问答结果")).toBeNull();
 });
 it("confirms a handoff and delegates preflight and claim atomically to the startup callback", async () => {
  const onStart = vi.fn().mockResolvedValue("session-c");
  render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} onStartWork={onStart} models={[{ id: "m" }]} defaultModelId="m" />);
  fireEvent.click(await screen.findByRole("button", { name: "工作项" }));
  fireEvent.click(screen.getByRole("button", { name: "接手并执行" }));
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "接手并执行" }));
  await waitFor(() => expect(onStart).toHaveBeenCalledWith(snapshot, snapshot.tasks[0], "m", { expert: undefined, team: undefined }));
  expect(api.teamProjectRequest.mock.calls.some(([action]) => action === "claimTask")).toBe(false);
 });
 it("hides write controls for viewers and discards stale answers at an account boundary", async () => {
  api.teamProjectRequest.mockImplementation(async action => action === "ask" ? answer : { ...snapshot, role: "viewer" });
  render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "新成员快速了解项目" }));
  await screen.findByLabelText("项目问答结果");
  expect(screen.queryByRole("button", { name: "邀请成员" })).toBeNull();
  act(() => useOrgSessionStore.setState({ session: null, boundaryRevision: 2 }));
  expect(screen.queryByLabelText("项目问答结果")).toBeNull();
  expect(screen.getByRole("button", { name: "登录组织" })).toBeInTheDocument();
 });
 it("a question completing after logout cannot restore the previous organization's content", async () => {
  let complete!: (a: ProjectAnswer) => void;
  api.teamProjectRequest.mockImplementation(async action => action === "ask" ? new Promise<ProjectAnswer>(resolve => { complete = resolve; }) : snapshot);
  render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "新成员快速了解项目" }));
  act(() => useOrgSessionStore.setState({ session: null, boundaryRevision: 2 }));
  await act(async () => { complete(answer); });
  expect(screen.queryByLabelText("项目问答结果")).toBeNull();
 });
});

it("cancels an upload when the account changes while choosing files", async () => {
 let choose!: (paths: string[]) => void;
 api.filesystemPickFiles.mockImplementation(() => new Promise<string[]>(resolve => { choose = resolve; }));
 render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
 fireEvent.click(await screen.findByRole("button", { name: "资料与成果" }));
 fireEvent.click(screen.getByRole("button", { name: "发布资料或成果" }));
 act(() => useOrgSessionStore.setState({ boundaryRevision: 2 }));
 await act(async () => { choose(["/private/old-account.md"]); });
 expect(api.teamProjectUpload).not.toHaveBeenCalled();
});
it("uses a single-file picker for a new version and refreshes after a partial batch failure", async () => {
 api.filesystemPickFiles.mockResolvedValue(["/one.md", "/two.md"]);
 api.teamProjectUpload.mockResolvedValueOnce({ docId: "one" }).mockRejectedValueOnce(new Error("连接中断"));
 render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
 fireEvent.click(await screen.findByRole("button", { name: "资料与成果" }));
 fireEvent.click(screen.getByRole("button", { name: "发布新版本" }));
 expect(await screen.findByRole("alert")).toHaveTextContent("一次只能发布一个新版本");
 expect(api.filesystemPickFiles).toHaveBeenCalledWith(expect.objectContaining({ multiple: false, maxFiles: 1 }));
 expect(api.teamProjectUpload).not.toHaveBeenCalled();
 fireEvent.click(screen.getByRole("button", { name: "发布资料或成果" }));
 expect(await screen.findByRole("alert")).toHaveTextContent("已发布 1 份文件");
 expect(api.teamProjectRequest.mock.calls.filter(([action]) => action === "snapshot").length).toBeGreaterThan(1);
});
it("shows shared reports and opens existing executions without starting another one", async () => {
 const execution = { id: "run", taskId: "t", actorId: "c", sessionId: "local", status: "completed", modelId: "m", expertName: "接口专家", expertVersion: null, summary: "完成接口验证，报告已发布。", updatedAt: 1700000000000 };
 api.teamProjectRequest.mockResolvedValue({ ...snapshot, runs: [execution] });
 const open = vi.fn().mockResolvedValue(undefined), start = vi.fn();
 render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} onOpenExecution={open} onStartWork={start} />);
 fireEvent.click(await screen.findByRole("button", { name: "执行记录" }));
 expect(screen.getByLabelText("项目执行记录")).toHaveTextContent("完成接口验证");
 fireEvent.click(screen.getByRole("button", { name: "打开本机执行" }));
 await waitFor(() => expect(open).toHaveBeenCalledWith(expect.objectContaining({ id: "p" }), execution));
 expect(start).not.toHaveBeenCalled();
});
it("surfaces a failed preflight without claiming the work item", async () => {
 const start = vi.fn().mockRejectedValue(new Error("当前配额不足，工作项尚未接手"));
 render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} onStartWork={start} models={[{ id: "m" }]} />);
 fireEvent.click(await screen.findByRole("button", { name: "工作项" }));
 fireEvent.click(screen.getByRole("button", { name: "接手并执行" }));
 fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "接手并执行" }));
 expect(await screen.findByRole("alert")).toHaveTextContent("配额不足");
 expect(api.teamProjectRequest.mock.calls.some(([action]) => action === "claimTask")).toBe(false);
});
it("reopens acceptance when the requirements of an accepted task change", async () => {
 api.teamProjectRequest.mockResolvedValue({ ...snapshot, tasks: [{ ...snapshot.tasks[0], status: "accepted" }] });
 render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
 fireEvent.click(await screen.findByRole("button", { name: "工作项" }));
 fireEvent.click(screen.getByRole("button", { name: "编辑工作项" }));
 const dialog = screen.getByRole("dialog");
 fireEvent.change(within(dialog).getByLabelText("验收要求"), { target: { value: "通过更新后的验收条件" } });
 fireEvent.click(within(dialog).getByRole("button", { name: "保存工作项" }));
 await waitFor(() => expect(api.teamProjectRequest).toHaveBeenCalledWith("updateTask", "p", expect.objectContaining({ expectedRevision: 2, status: "review", acceptance: "通过更新后的验收条件" }), "t", expect.any(Object)));
});
it("does not open a late-created project after the user has moved to another one", async () => {
 let finish!: (project: TeamProjectSnapshot) => void;
 api.teamProjectRequest.mockImplementation(async action => action === "create" ? new Promise<TeamProjectSnapshot>(resolve => { finish = resolve; }) : action === "list" ? [] : snapshot);
 const select = vi.fn();
 const view = render(<TeamProjectsPanel selectedId={null} onSelect={select} />);
 fireEvent.click(await screen.findByRole("button", { name: "新建团队项目" }));
 const dialog = screen.getByRole("dialog");
 fireEvent.change(within(dialog).getByLabelText(/项目名称/), { target: { value: "新项目" } });
 fireEvent.click(within(dialog).getByRole("button", { name: "创建团队项目" }));
 await waitFor(() => expect(api.teamProjectRequest.mock.calls.some(([action]) => action === "create")).toBe(true));
 view.rerender(<TeamProjectsPanel selectedId="another" onSelect={select} />);
 await act(async () => { finish({ ...snapshot, id: "created" }); });
 expect(select).not.toHaveBeenCalled();
});

it("keeps the original account generation when opening the native document save dialog", async () => {
 render(<TeamProjectsPanel selectedId="p" onSelect={vi.fn()} />);
 fireEvent.click(await screen.findByRole("button", { name: "资料与成果" }));
 fireEvent.click(screen.getByRole("button", { name: "下载原件" }));
 await waitFor(() => expect(api.orgDownloadDocument).toHaveBeenCalledWith("file", false, "接口规范.md", 7));
});
