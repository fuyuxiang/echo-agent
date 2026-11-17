import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ProjectParticipantsButton } from "../ProjectParticipantsButton";
import { useOrgSessionStore } from "@/stores/org-session-store";
import type { ProjectMeta } from "@/stores/projects-store";
const api = vi.hoisted(() => ({ teamProjectRequest: vi.fn(), projectCollaborationCapabilities: vi.fn() }));
vi.mock("@/lib/team-project-client", async original => ({ ...await original<object>(), ...api }));
vi.mock("@/lib/org-client", () => ({ orgSession: vi.fn() }));
vi.mock("../PublishTeamProjectButton", () => ({ PublishTeamProjectButton: () => <button>开启协作并选择共享内容</button> }));
const snapshot = { id: "shared", role: "admin", ownerId: "a", revision: 4, members: [{ userId: "a", username: "owner", displayName: "负责人", role: "admin", status: "active" }, { userId: "b", username: "editor", displayName: "编辑成员", role: "collaborator", status: "active" }] };
const project: ProjectMeta = { id: "local", name: "本机项目", createdAt: "2026-11-17", members: ["原备注"], conversations: [], plans: [], tasks: [], assets: [], experts: [], skills: [], connectors: [] };
beforeEach(() => {
 vi.resetAllMocks();
 useOrgSessionStore.setState({ session: { loggedIn: true, accountGeneration: 7, serverUrl: "https://org.example", user: { id: "a", username: "owner", displayName: "负责人", role: "member", clearance: 0 } }, boundaryRevision: 1, hydrated: true });
 api.projectCollaborationCapabilities.mockResolvedValue({ version: 2, memberSearch: true, memberRoles: true, idempotentCreate: true });
 api.teamProjectRequest.mockImplementation(async action => action === "memberCandidates" ? [{ userId: "c", username: "new-user", displayName: "李明" }] : snapshot);
});
async function open() { fireEvent.click(screen.getByRole("button", { name: /参与者/ })); return screen.findByRole("dialog", { name: "项目参与者" }); }
describe("project participants", () => {
 it("searches organization members and invites the selected identity with an explicit role", async () => {
  render(<ProjectParticipantsButton sharedId="shared" />); const dialog = await open();
  fireEvent.change(await within(dialog).findByRole("searchbox", { name: "搜索组织成员" }), { target: { value: "李明" } });
  fireEvent.click(await screen.findByRole("button", { name: /李明.*@new-user/ }));
  fireEvent.change(screen.getByRole("combobox", { name: "邀请成员权限" }), { target: { value: "viewer" } });
  fireEvent.click(screen.getByRole("button", { name: "邀请加入项目" }));
  await waitFor(() => expect(api.teamProjectRequest).toHaveBeenCalledWith("addMember", "shared", { username: "new-user", role: "viewer" }, undefined, expect.objectContaining({ expectedGeneration: 7 })));
 });
 it("changes permissions with the current revision and never offers owner removal", async () => {
  render(<ProjectParticipantsButton sharedId="shared" />); await open();
  fireEvent.change(await screen.findByRole("combobox", { name: "编辑成员的项目权限" }), { target: { value: "viewer" } });
  await waitFor(() => expect(api.teamProjectRequest).toHaveBeenCalledWith("updateMember", "shared", { role: "viewer", expectedRevision: 4 }, "b", expect.any(Object)));
  expect(screen.queryByRole("combobox", { name: "负责人的项目权限" })).toBeNull();
  expect(screen.getAllByRole("button", { name: "移除" })).toHaveLength(1);
 });
 it("lets viewers inspect membership without exposing invite or role controls", async () => {
  api.teamProjectRequest.mockResolvedValue({ ...snapshot, role: "viewer" });
  render(<ProjectParticipantsButton sharedId="shared" />); await open();
  await screen.findByText("编辑成员");
  expect(screen.queryByRole("searchbox")).toBeNull(); expect(screen.queryByRole("button", { name: "移除" })).toBeNull();
 });
 it("keeps local projects usable and explains an unavailable collaboration service", async () => {
  api.projectCollaborationCapabilities.mockRejectedValue(new Error("接口不存在"));
  render(<ProjectParticipantsButton project={project} />); await open();
  expect(await screen.findByRole("alert")).toHaveTextContent("个人项目仍可正常使用");
  expect(screen.queryByRole("button", { name: "开启协作并选择共享内容" })).toBeNull();
  expect(api.teamProjectRequest).not.toHaveBeenCalled();
 });
 it("offers login for a local project and does not request or modify remote data", async () => {
  useOrgSessionStore.setState({ session: null }); const login = vi.fn();
  render(<ProjectParticipantsButton project={project} onOpenOrganization={login} />); await open();
  fireEvent.click(screen.getByRole("button", { name: "登录组织" })); expect(login).toHaveBeenCalledOnce();
  expect(api.projectCollaborationCapabilities).not.toHaveBeenCalled();
 });
 it("discards membership and a delayed search when the organization account changes", async () => {
  let finish!: (value: unknown) => void;
  api.teamProjectRequest.mockImplementation(async action => action === "memberCandidates" ? new Promise(resolve => { finish = resolve; }) : snapshot);
  render(<ProjectParticipantsButton sharedId="shared" />); await open();
  fireEvent.change(await screen.findByRole("searchbox", { name: "搜索组织成员" }), { target: { value: "李明" } });
  await waitFor(() => expect(api.teamProjectRequest).toHaveBeenCalledWith("memberCandidates", "shared", { query: "李明" }, undefined, expect.any(Object)));
  act(() => useOrgSessionStore.setState({ boundaryRevision: 2, session: null }));
  await act(async () => finish([{ userId: "c", username: "private", displayName: "旧账号成员" }]));
  expect(screen.queryByRole("dialog")).toBeNull(); expect(screen.queryByText("旧账号成员")).toBeNull();
 });
});
